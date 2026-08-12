//! ContextKeeper client: continuity state for terminals, and a proxy for the
//! recovery workflow.
//!
//! ContextKeeper is a sidecar that captures agent transcripts, classifies
//! failures, and compiles deterministic recovery bundles. It owns a control
//! token that the browser must never hold, so every call is made here and the
//! PWA talks only to same-origin MyDevEnv2 routes.
//!
//! Two rules shape this module:
//!
//! * **A ContextKeeper outage degrades to "unprotected", never to an error.**
//!   Listing, creating, attaching, and killing terminals must not depend on a
//!   sidecar being up. Roster enrichment therefore reads a cache refreshed in
//!   the background, so a slow or dead ContextKeeper costs a stale badge rather
//!   than a hung request.
//! * **The sidecar is reached by a pinned host entry, not container DNS.**
//!   `mydevenv2-dev` runs Tailscale, which overwrites `/etc/resolv.conf` and
//!   breaks Docker service-name resolution; `extra_hosts` pins
//!   `contextkeeper` in the Compose file. Nothing here may assume DNS works.

use std::{collections::HashMap, sync::Arc, time::Duration};

use mydevenv2_contract::{ProtectionState, SessionContinuity};
use parking_lot::RwLock;
use serde::Serialize;
use serde_json::Value;

use crate::{
    config::Config,
    error::{ApiError, Result},
};

/// Every call is bounded well under a browser's patience: a sidecar that is
/// merely slow must not become MyDevEnv2's latency.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// Bundle compilation is real work over a whole transcript, so the read path
/// for a preview gets more room than a roster refresh.
const PREVIEW_TIMEOUT: Duration = Duration::from_secs(20);
const REFRESH_INTERVAL: Duration = Duration::from_secs(10);
/// The roster only needs sessions bound to a live PTY; this bounds the answer
/// on a ledger with thousands of historical rows.
const ROSTER_LIMIT: usize = 200;

#[derive(Debug, Default)]
struct Snapshot {
    /// Keyed by MyDevEnv2 PTY id — the join between the two systems.
    by_pty: HashMap<String, SessionContinuity>,
    reachable: bool,
    capture_lag_seconds: Option<f64>,
    capture_status: Option<String>,
}

pub struct ContextKeeperRuntime {
    client: reqwest::Client,
    base: String,
    token: String,
    snapshot: RwLock<Snapshot>,
}

impl ContextKeeperRuntime {
    /// Construct only when both URL and token are configured. ContextKeeper is
    /// optional: with either missing, MyDevEnv2 behaves exactly as it did
    /// before, and the PWA shows every terminal as unprotected.
    pub fn from_config(cfg: &Config) -> Option<Arc<Self>> {
        let base = cfg.contextkeeper_url.clone()?;
        let token = cfg.contextkeeper_token.clone()?;
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .ok()?;
        Some(Arc::new(Self {
            client,
            base: base.trim_end_matches('/').to_string(),
            token,
            snapshot: RwLock::new(Snapshot::default()),
        }))
    }

    /// Poll the sidecar on an interval and keep the roster cache warm.
    ///
    /// The refresher, not the request path, is what absorbs an outage: a failed
    /// refresh leaves the previous answer in place, marks the sidecar
    /// unreachable, and the next roster read reports unprotected.
    pub fn spawn_refresher(self: &Arc<Self>) {
        let runtime = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                runtime.refresh().await;
                tokio::time::sleep(REFRESH_INTERVAL).await;
            }
        });
    }

    async fn refresh(&self) {
        let health = self.get_json("/healthz", &[]).await.ok();
        let capture = health
            .as_ref()
            .and_then(|value| value.get("capture").cloned());
        let capture_lag_seconds = capture
            .as_ref()
            .and_then(|c| c.get("scan_lag_seconds"))
            .and_then(Value::as_f64);
        let capture_status = capture.as_ref().map(|c| {
            // A first pass over a large backlog is minutes long: "catching up"
            // has to be distinguishable from "stopped" or the badge reads as a
            // fault during normal startup.
            if c.get("first_scan_complete").and_then(Value::as_bool) == Some(false) {
                "catching-up".to_string()
            } else if c.get("watcher_running").and_then(Value::as_bool) == Some(true) {
                "running".to_string()
            } else {
                "stopped".to_string()
            }
        });

        let sessions = self
            .get_json("/api/sessions", &[("limit", ROSTER_LIMIT.to_string())])
            .await;
        let Ok(sessions) = sessions else {
            let mut snapshot = self.snapshot.write();
            snapshot.reachable = false;
            snapshot.capture_status = None;
            return;
        };

        let mut by_pty = HashMap::new();
        for row in sessions.as_array().cloned().unwrap_or_default() {
            let Some(pty) = row
                .get("mydevenv2_session_id")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            by_pty.insert(
                pty.to_string(),
                continuity_from_row(&row, capture_lag_seconds, capture_status.clone()),
            );
        }

        let mut snapshot = self.snapshot.write();
        snapshot.by_pty = by_pty;
        snapshot.reachable = true;
        snapshot.capture_lag_seconds = capture_lag_seconds;
        snapshot.capture_status = capture_status;
    }

    /// Continuity for one PTY, or None for "unprotected".
    ///
    /// Reads the cache, so it is synchronous and cannot block a roster
    /// response no matter what the sidecar is doing.
    pub fn continuity_for(&self, pty_id: &str) -> Option<SessionContinuity> {
        let snapshot = self.snapshot.read();
        if !snapshot.reachable {
            return None;
        }
        snapshot.by_pty.get(pty_id).cloned()
    }

    pub fn health_snapshot(&self) -> Value {
        let snapshot = self.snapshot.read();
        serde_json::json!({
            "configured": true,
            "reachable": snapshot.reachable,
            "capture_status": snapshot.capture_status,
            "capture_lag_seconds": snapshot.capture_lag_seconds,
            "protected_sessions": snapshot.by_pty.len(),
        })
    }

    // -- proxied operations -------------------------------------------------

    pub async fn continuation(&self, session_id: &str) -> Result<Value> {
        self.get_json(&format!("/api/sessions/{session_id}/continuation"), &[])
            .await
    }

    pub async fn work_session(&self, work_id: &str) -> Result<Value> {
        self.get_json(&format!("/api/work/{work_id}"), &[]).await
    }

    pub async fn session(&self, session_id: &str) -> Result<Value> {
        self.get_json(&format!("/api/sessions/{session_id}"), &[])
            .await
    }

    pub async fn preview(&self, session_id: &str) -> Result<Value> {
        self.request(
            reqwest::Method::GET,
            &format!("/api/sessions/{session_id}/recovery"),
            &[],
            None::<&Value>,
            PREVIEW_TIMEOUT,
        )
        .await
    }

    pub async fn approve(&self, session_id: &str, body: &impl Serialize) -> Result<Value> {
        self.request(
            reqwest::Method::POST,
            &format!("/api/sessions/{session_id}/recovery/approve"),
            &[],
            Some(body),
            REQUEST_TIMEOUT,
        )
        .await
    }

    pub async fn launch(&self, session_id: &str, body: &impl Serialize) -> Result<Value> {
        self.request(
            reqwest::Method::POST,
            &format!("/api/sessions/{session_id}/recovery/launch"),
            &[],
            Some(body),
            PREVIEW_TIMEOUT,
        )
        .await
    }

    // -- transport ----------------------------------------------------------

    async fn get_json(&self, path: &str, query: &[(&str, String)]) -> Result<Value> {
        self.request(
            reqwest::Method::GET,
            path,
            query,
            None::<&Value>,
            REQUEST_TIMEOUT,
        )
        .await
    }

    async fn request(
        &self,
        method: reqwest::Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<&impl Serialize>,
        timeout: Duration,
    ) -> Result<Value> {
        let mut request = self
            .client
            .request(method, format!("{}{path}", self.base))
            .bearer_auth(&self.token)
            .timeout(timeout);
        if !query.is_empty() {
            request = request.query(query);
        }
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request.send().await.map_err(|e| {
            // Never include the error's full chain: it can carry the URL with
            // credentials if a future config ever embeds them.
            ApiError::BadGateway(format!(
                "contextkeeper unreachable ({})",
                if e.is_timeout() {
                    "timeout"
                } else {
                    "transport"
                }
            ))
        })?;
        let status = response.status();
        let payload: Value = response.json().await.unwrap_or(Value::Null);
        if status.is_success() {
            return Ok(payload);
        }
        // ContextKeeper's own `detail` is the useful part — an open launch
        // circuit reports its retry time there, and the PWA renders it.
        Err(ApiError::Upstream {
            status: status.as_u16(),
            detail: payload
                .get("detail")
                .cloned()
                .unwrap_or_else(|| payload.clone()),
        })
    }
}

fn continuity_from_row(
    row: &Value,
    capture_lag_seconds: Option<f64>,
    capture_status: Option<String>,
) -> SessionContinuity {
    let lifecycle = row
        .get("lifecycle")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    // `recovery_pending` means a bundle is waiting for a human decision, which
    // is a different thing to show than a healthy protected session.
    let state = match lifecycle.as_str() {
        "recovery_pending" | "failed" => ProtectionState::Recovering,
        "closed" => ProtectionState::Unprotected,
        _ => ProtectionState::Protected,
    };
    SessionContinuity {
        state,
        session_id: row
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        provider: row
            .get("provider")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        native_session_id: row
            .get("native_session_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        work_id: row
            .get("work_id")
            .and_then(Value::as_str)
            .map(str::to_string),
        lifecycle,
        event_count: row
            .get("event_count")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        failure_count: row
            .get("failure_count")
            .and_then(Value::as_u64)
            .unwrap_or_default(),
        capture_lag_seconds,
        capture_status,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(lifecycle: &str) -> Value {
        serde_json::json!({
            "id": "registry-1",
            "provider": "claude",
            "native_session_id": "native-1",
            "work_id": "work-1",
            "lifecycle": lifecycle,
            "event_count": 12,
            "failure_count": 1,
        })
    }

    #[test]
    fn a_pending_recovery_reads_as_recovering() {
        let continuity = continuity_from_row(&row("recovery_pending"), Some(1.5), None);
        assert_eq!(continuity.state, ProtectionState::Recovering);
        assert_eq!(continuity.work_id.as_deref(), Some("work-1"));
        assert_eq!(continuity.capture_lag_seconds, Some(1.5));
    }

    #[test]
    fn an_active_session_reads_as_protected() {
        assert_eq!(
            continuity_from_row(&row("active"), None, None).state,
            ProtectionState::Protected
        );
    }

    #[test]
    fn a_closed_session_is_not_protected() {
        assert_eq!(
            continuity_from_row(&row("closed"), None, None).state,
            ProtectionState::Unprotected
        );
    }
}
