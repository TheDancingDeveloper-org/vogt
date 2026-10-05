//! How long a session took to become usable, and why (WI-927).
//!
//! On 2026-10-05 a new session on prod took two minutes to give its person a
//! prompt, and nothing recorded that, let alone where the time went: the
//! launch wrapper (`vogt-agent-auth`) was making fifteen vendor-CLI calls in
//! series, each waiting on that CLI's telemetry. Finding it took process start
//! times read out of `/proc`. This module makes a launch explain itself:
//!
//! - **Every session start is audited** (`event=session.start`): origin,
//!   launcher, how long the spawn took, and the outcome.
//! - **First output** (`event=launch.first_output`): the engine's own
//!   measurement, spawn to the first byte the session printed, for every
//!   session whatever runs in it. Logged at `warn` when slow.
//! - **The wrapper's launch report** (`event=launch.report` and one
//!   `event=launch.stage` line per stage): login, each secret project read
//!   (how, how long, which names, found or not; never a value), the client
//!   bootstrap, and the total to handover. The secret names are an audit
//!   record, so they go to `vogt::audit` like the broker's fetches.
//!
//! The same numbers feed `crate::metrics`, so the p95 can be alerted on.
//!
//! The report arrives on `POST /api/agent-auth/launch-report`, beside the
//! secret broker and authenticated the same way: by the per-session broker
//! token, which only the session's own launch holds. It is accepted once per
//! session, so a session cannot inflate the metrics by repeating it.

use std::sync::Arc;
use std::time::Duration;

use axum::{
    body::Body,
    extract::State,
    http::{header, Method, Request, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::json;

use crate::app::AppState;
use crate::auth;
use crate::error::ApiError;
use crate::metrics::metrics;
use crate::observability::RequestId;
use crate::pty::Session;

/// The report route. `POST` only, outside the engine bearer gate (see
/// `app.rs`), authenticated with the session's own broker token.
pub const REPORT_ROUTE: &str = "/api/agent-auth/launch-report";

/// A launch, or first output, slower than this is logged at `warn`.
pub const SLOW_LAUNCH: Duration = Duration::from_secs(10);

/// Largest report accepted. A launch with every secret named is a few KiB.
const MAX_REPORT_BYTES: usize = 32 * 1024;
/// Stages a report may carry: login, bootstrap, and one per secret project.
const MAX_STAGES: usize = 32;
/// Secrets one project stage may name.
const MAX_SECRETS: usize = 128;

/// Where a session came from, for the start audit and its counter.
#[derive(Clone, Copy, Debug)]
pub enum Origin {
    Api,
    AgentTask,
    Wake,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Origin::Api => "api",
            Origin::AgentTask => "agent-task",
            Origin::Wake => "wake",
        }
    }
}

/// Record a session start: the audit line and the counter. `spawn` is how
/// long the engine itself took to set the session up and fork it.
pub fn record_start(
    origin: Origin,
    result: std::result::Result<&Session, &ApiError>,
    name: &str,
    template: Option<&str>,
    spawn: Duration,
) {
    match result {
        Ok(session) => {
            metrics()
                .session_starts
                .inc(&[("origin", origin.as_str()), ("outcome", "ok")]);
            tracing::info!(
                target: "vogt::audit",
                event = "session.start",
                session_id = %session.id,
                name = %name,
                template = template.unwrap_or(""),
                origin = origin.as_str(),
                launcher = session.launcher(),
                spawn_ms = spawn.as_millis() as u64,
                outcome = "ok",
                "session started"
            );
        }
        Err(error) => {
            metrics()
                .session_starts
                .inc(&[("origin", origin.as_str()), ("outcome", "failed")]);
            tracing::warn!(
                target: "vogt::audit",
                event = "session.start",
                name = %name,
                template = template.unwrap_or(""),
                origin = origin.as_str(),
                spawn_ms = spawn.as_millis() as u64,
                outcome = "failed",
                error = %error,
                "session start failed"
            );
        }
    }
}

/// The session printed its first byte. Called once, from the PTY reader.
pub fn record_first_output(session: &Session, after: Duration) {
    let launcher = session.launcher();
    metrics()
        .first_output
        .observe(&[("launcher", launcher)], after);
    let ms = after.as_millis() as u64;
    if after >= SLOW_LAUNCH {
        tracing::warn!(
            target: "vogt::launch",
            event = "launch.first_output",
            session_id = %session.id,
            name = %session.name(),
            launcher,
            first_output_ms = ms,
            "session slow to first output"
        );
    } else {
        tracing::info!(
            target: "vogt::launch",
            event = "launch.first_output",
            session_id = %session.id,
            name = %session.name(),
            launcher,
            first_output_ms = ms,
            "session first output"
        );
    }
}

/// What the launch wrapper reports at handover (or at its failure).
#[derive(Debug, Deserialize)]
pub struct LaunchReport {
    pub outcome: String,
    #[serde(default)]
    pub command: String,
    pub total_ms: u64,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub stages: Vec<LaunchStage>,
}

#[derive(Debug, Deserialize)]
pub struct LaunchStage {
    pub stage: String,
    pub ms: u64,
    #[serde(default)]
    pub outcome: String,
    /// Secret stages: the project read.
    #[serde(default)]
    pub project: Option<String>,
    /// Secret stages: `bulk` (one request) or `cli` (one CLI run per secret).
    #[serde(default)]
    pub mode: Option<String>,
    /// Secret stages: each name read, never a value.
    #[serde(default)]
    pub secrets: Vec<LaunchSecret>,
}

#[derive(Debug, Deserialize)]
pub struct LaunchSecret {
    pub var: String,
    pub name: String,
    pub found: bool,
}

/// A label value or log field from the report: kept only if it is the kind of
/// token it claims to be (identifier characters, bounded), so a report can
/// neither inject log lines nor mint unbounded metric series.
fn token(raw: &str, max: usize) -> Option<&str> {
    let ok = !raw.is_empty()
        && raw.len() <= max
        && raw
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'));
    ok.then_some(raw)
}

/// One of a closed set, or `other`: metric labels never take free text.
fn one_of(raw: &str, allowed: &[&'static str]) -> &'static str {
    allowed
        .iter()
        .find(|a| **a == raw)
        .copied()
        .unwrap_or("other")
}

/// `POST /api/agent-auth/launch-report` — the launch wrapper says how its
/// launch went. See the module docs.
pub async fn report(State(state): State<Arc<AppState>>, request: Request<Body>) -> Response {
    let path = request.uri().path().to_string();
    let request_id = request
        .extensions()
        .get::<RequestId>()
        .map(|id| id.0.clone())
        .unwrap_or_default();
    let presented = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string);
    let Some(presented) = presented else {
        auth::record_auth_failure(&Method::POST, &path, &request_id, "missing broker token").await;
        return ApiError::Unauthorized.into_response();
    };
    let broker = state.sessions.secret_broker();
    let Some(session) = broker
        .authenticate(&presented)
        .and_then(|id| state.sessions.get(id).ok())
    else {
        auth::record_auth_failure(&Method::POST, &path, &request_id, "unknown broker token").await;
        return ApiError::Unauthorized.into_response();
    };
    let body = match axum::body::to_bytes(request.into_body(), MAX_REPORT_BYTES).await {
        Ok(body) => body,
        Err(_) => {
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                Json(json!({ "error": "launch report too large" })),
            )
                .into_response()
        }
    };
    let report: LaunchReport = match serde_json::from_slice(&body) {
        Ok(report) => report,
        Err(e) => {
            return ApiError::BadRequest(format!("launch report: {e}")).into_response();
        }
    };
    if report.stages.len() > MAX_STAGES
        || report.stages.iter().any(|s| s.secrets.len() > MAX_SECRETS)
    {
        return ApiError::BadRequest("launch report has too many stages or secrets".into())
            .into_response();
    }
    if !session.mark_launch_reported() {
        return (
            StatusCode::CONFLICT,
            Json(json!({ "error": "this session's launch was already reported" })),
        )
            .into_response();
    }
    record_report(&session, &report, &request_id);
    StatusCode::NO_CONTENT.into_response()
}

/// Log, audit and measure one accepted report.
pub fn record_report(session: &Session, report: &LaunchReport, request_id: &str) {
    let outcome = one_of(&report.outcome, &["ok", "failed"]);
    let command = one_of(&report.command, &["run", "shell"]);
    let total = Duration::from_millis(report.total_ms);
    metrics()
        .launch
        .observe(&[("command", command), ("outcome", outcome)], total);

    for stage in &report.stages {
        let name = one_of(&stage.stage, &["login", "secrets", "bootstrap"]);
        let stage_outcome = token(&stage.outcome, 32).unwrap_or("unknown");
        let took = Duration::from_millis(stage.ms);
        metrics().launch_stage.observe(&[("stage", name)], took);
        let project = stage
            .project
            .as_deref()
            .and_then(|p| token(p, 64))
            .unwrap_or("");
        let mode = stage
            .mode
            .as_deref()
            .map_or("", |m| one_of(m, &["bulk", "cli"]));
        tracing::info!(
            target: "vogt::launch",
            event = "launch.stage",
            request_id = %request_id,
            session_id = %session.id,
            stage = name,
            ms = stage.ms,
            outcome = stage_outcome,
            project,
            mode,
            "launch stage"
        );
        if name == "secrets" {
            metrics().launch_secret_reads.inc(&[
                ("mode", if mode.is_empty() { "other" } else { mode }),
                ("outcome", one_of(&stage.outcome, &["ok", "bulk-failed"])),
            ]);
            // Which secrets this session was handed at launch, by name — the
            // launch half of the broker's per-fetch audit. Never a value.
            let read: Vec<String> = stage
                .secrets
                .iter()
                .filter_map(|s| {
                    Some(format!(
                        "{}={}{}",
                        token(&s.var, 128)?,
                        token(&s.name, 128)?,
                        if s.found { "" } else { "(missing)" }
                    ))
                })
                .collect();
            let missing = stage.secrets.iter().filter(|s| !s.found).count();
            tracing::info!(
                target: "vogt::audit",
                event = "launch.secrets",
                request_id = %request_id,
                session_id = %session.id,
                project,
                mode,
                ms = stage.ms,
                count = read.len(),
                missing,
                secrets = %read.join(","),
                "launch secrets read"
            );
        }
    }

    let error = report
        .error
        .as_deref()
        .map(|e| {
            e.chars()
                .filter(|c| !c.is_control())
                .take(300)
                .collect::<String>()
        })
        .unwrap_or_default();
    if outcome != "ok" || total >= SLOW_LAUNCH {
        tracing::warn!(
            target: "vogt::launch",
            event = "launch.report",
            request_id = %request_id,
            session_id = %session.id,
            name = %session.name(),
            command,
            outcome,
            total_ms = report.total_ms,
            error = %error,
            "session launch slow or failed"
        );
    } else {
        tracing::info!(
            target: "vogt::launch",
            event = "launch.report",
            request_id = %request_id,
            session_id = %session.id,
            name = %session.name(),
            command,
            outcome,
            total_ms = report.total_ms,
            "session launch"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_fields_are_tokens_or_dropped() {
        assert_eq!(token("GIT_AUTH_TOKEN", 128), Some("GIT_AUTH_TOKEN"));
        assert_eq!(token("76b1ebe1-3656-4cef", 64), Some("76b1ebe1-3656-4cef"));
        assert_eq!(token("a b", 64), None);
        assert_eq!(token("x\nlevel=error", 64), None);
        assert_eq!(token("", 64), None);
        assert_eq!(one_of("bulk", &["bulk", "cli"]), "bulk");
        assert_eq!(one_of("anything else", &["bulk", "cli"]), "other");
    }
}
