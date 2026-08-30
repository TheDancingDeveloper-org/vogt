//! HTTP handlers for the push API + the background task that dispatches
//! notifications when sessions enter `waiting-for-input`.

use std::sync::Arc;

use axum::{extract::State, Json};
use serde::Deserialize;
use serde_json::json;
use tracing::info;

use crate::{
    activity::ActivityState,
    app::AppState,
    error::Result,
    events::ServerEvent,
    push::{DispatchCounts, NotificationKind, PushPreferences, Subscription},
};

#[derive(Debug, Deserialize)]
pub struct SubscribeReq {
    #[serde(flatten)]
    pub sub: Subscription,
    pub label: Option<String>,
}

pub async fn public_key(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    Json(json!({
        "vapid_public_key": state.push.vapid_public_key(),
        "fcm_enabled": state.config.fcm_service_account_json.is_some(),
    }))
}

pub async fn subscribe(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SubscribeReq>,
) -> Result<Json<serde_json::Value>> {
    let stored = state.push.add(req.sub, req.label)?;
    Ok(Json(
        json!({ "ok": true, "id": stored.id, "prefs": stored.prefs }),
    ))
}

#[derive(Debug, Deserialize)]
pub struct UpdateReq {
    pub id: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub clear_label: bool,
    #[serde(default)]
    pub prefs: Option<PushPreferences>,
}

pub async fn update(
    State(state): State<Arc<AppState>>,
    Json(req): Json<UpdateReq>,
) -> Result<Json<serde_json::Value>> {
    let label = if req.clear_label {
        Some(None)
    } else if req.label.is_some() {
        Some(req.label)
    } else {
        None
    };
    let updated = state.push.update(&req.id, label, req.prefs)?;
    Ok(Json(json!({
        "ok": true,
        "id": updated.id,
        "label": updated.label,
        "prefs": updated.prefs,
    })))
}

#[derive(Debug, Deserialize)]
pub struct UnsubscribeReq {
    pub id: String,
}

pub async fn unsubscribe(
    State(state): State<Arc<AppState>>,
    Json(req): Json<UnsubscribeReq>,
) -> Result<Json<serde_json::Value>> {
    let removed = state.push.remove(&req.id)?;
    Ok(Json(json!({ "ok": removed })))
}

pub async fn list(State(state): State<Arc<AppState>>) -> Json<Vec<serde_json::Value>> {
    let xs = state
        .push
        .list()
        .into_iter()
        .map(|s| {
            // Strip the secret bits before exposing — endpoint URL alone is enough.
            let kind = match &s.sub {
                Subscription::WebPush { endpoint, .. } => json!({
                    "kind": "web-push",
                    "endpoint_host": endpoint
                        .split("//").nth(1).and_then(|x| x.split('/').next()),
                }),
                Subscription::Fcm { .. } => json!({ "kind": "fcm" }),
            };
            json!({
                "id": s.id,
                "label": s.label,
                "created_at": s.created_at.to_string(),
                "kind": kind,
                "prefs": s.prefs,
                "pending_digest_count": s.pending_digest.as_ref().map(|digest| digest.total_count).unwrap_or(0),
                "pending_digest_since": s.pending_digest.as_ref().map(|digest| digest.queued_at.to_string()),
            })
        })
        .collect();
    Json(xs)
}

#[derive(Debug, Deserialize)]
pub struct TestReq {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
}

pub async fn test_dispatch(
    State(state): State<Arc<AppState>>,
    Json(req): Json<TestReq>,
) -> Json<serde_json::Value> {
    let title = req.title.unwrap_or_else(|| "Vogt test".into());
    let body = req
        .body
        .unwrap_or_else(|| "Push notifications are working.".into());
    let counts = state
        .push
        .notify(
            NotificationKind::Test,
            &title,
            &body,
            json!({ "kind": "test" }),
        )
        .await;
    Json(json!({ "ok": counts.ok, "fail": counts.fail, "queued": counts.queued }))
}

pub async fn flush_digests(State(state): State<Arc<AppState>>) -> Json<DispatchCounts> {
    Json(state.push.flush_ready_digests().await)
}

/// Spawn the background task that watches the event bus and pushes a
/// notification when any session transitions to `waiting-for-input` or
/// `errored`.
pub fn spawn_activity_watcher(state: Arc<AppState>) {
    let mut rx = state.bus.subscribe();
    tokio::spawn(async move {
        while let Ok(ev) = rx.recv().await {
            if let ServerEvent::Activity { id, state: act, .. } = ev {
                let (kind, verb, data_kind) = match act {
                    ActivityState::WaitingForInput => (
                        NotificationKind::WaitingForInput,
                        "is waiting for input",
                        "waiting-for-input",
                    ),
                    ActivityState::Errored => (NotificationKind::Errored, "errored", "errored"),
                    _ => continue,
                };
                let session = state.sessions.get(id).ok();
                let name = session
                    .as_ref()
                    .map(|s| s.name())
                    .unwrap_or_else(|| id.to_string());
                let title = format!("{name} {verb}");
                let body = "Tap to open the session in Vogt.";
                let data = json!({
                    "kind": data_kind,
                    "session_id": id.to_string(),
                    "url": format!("/#/t/{id}"),
                });
                let counts = state.push.notify(kind, &title, body, data).await;
                info!(session = %id, ok = counts.ok, fail = counts.fail, queued = counts.queued, "push dispatched");
            }
        }
    });
}

/// The core event kinds worth a phone interruption (FR-M2).
///
/// A named set, never a `starts_with("drift.")` prefix. The requirement says
/// "and for nothing else by default", and a prefix silently opts in to every
/// kind the core adds later — which is exactly how a notification surface
/// grows without anyone deciding that it should. `drift.resolved` is
/// deliberately absent: somebody resolving drift is somebody already looking
/// at it.
///
/// Spelled as the core spells it: `DRIFT_RAISED_EVENT` in
/// `services/drift_service.py`. `vogt-engine-contract` gave `drift.opened` as
/// its example of a kind — a string the core has never emitted — and a filter
/// written from that comment would match nothing while looking correct. That
/// comment is fixed now; this note stays, because the next person adding a
/// kind here will reach for the same place. `tests/test_drift.py` asserts the
/// two spellings still agree.
const DRIFT_NOTIFY_KINDS: [&str; 1] = ["drift.raised"];

/// How long to keep collecting drift before sending one notification about
/// all of it. See `spawn_vogt_drift_watcher`.
const DRIFT_COALESCE_WINDOW: std::time::Duration = std::time::Duration::from_secs(10);

/// Spawn the background task that turns newly raised drift into a push
/// (FR-M2).
///
/// This is the fan-out half only. The polling half is
/// `vogt_core::spawn_event_follower`, which follows the core's `events.list`
/// cursor and republishes each change onto this bus as `VogtChanged` — so
/// this watcher subscribes to the bus exactly as `spawn_activity_watcher`
/// above does, and inherits the follower's properties instead of restating
/// them: silent when no core or no core token is configured, quiet through an
/// outage, and starting from the core's current head so a boot replays
/// nothing.
///
/// **What reading the bus costs, stated rather than discovered.** The
/// follower's cursor is in memory, so drift raised while this process was
/// down is never republished and never notified. A redeploy is therefore a
/// hole in the notification stream. That is accepted, on two grounds. A drift
/// proposal is not an event that expires — it stays open in the inbox on the
/// projects surface until somebody rules on it, so the *work* is never lost,
/// only the interruption. And the alternative costs more than it looks:
/// a second cursor, persisted, is a second thing to seed, to migrate and to
/// get wrong, and its characteristic failure is a phone replaying an estate's
/// history after a restart — which is the failure that makes someone turn the
/// channel off, taking `waiting-for-input` with it. A missed buzz is
/// recoverable by opening the app; a channel someone disabled is not.
///
/// **It coalesces, and its sibling above does not.** That is the one
/// deliberate difference, and it is about how the two events arrive. Two
/// sessions entering `waiting-for-input` in the same instant is a
/// coincidence; drift arriving in a burst is the *normal* case, because
/// `drift.detect` sweeps and raises everything it finds in one pass — and the
/// follower then delivers that whole batch from a single poll. One
/// notification per proposal would be thirty buzzes from one sweep.
///
/// Latency is the follower's interval plus this window: up to five seconds
/// before the event reaches the bus, then up to ten more spent collecting.
/// For "come and look at this when you can" that is well inside useful, and
/// it is the price of not buzzing thirty times.
///
/// Quiet-hours digesting is not reimplemented here: `PushManager::notify`
/// already queues rather than sends during quiet hours, per subscription,
/// which is the right layer for it.
pub fn spawn_vogt_drift_watcher(state: Arc<AppState>) {
    use tokio::sync::broadcast::error::RecvError;

    let mut rx = state.bus.subscribe();
    tokio::spawn(async move {
        loop {
            // Block until there is drift at all. Everything else on this bus
            // — every activity change, every session lifecycle event — falls
            // through here costing one comparison.
            match rx.recv().await {
                Ok(event) if is_notifiable_drift(&event) => {}
                Ok(_) => continue,
                // Lagged means this task fell behind a burst and lost events.
                // Not a reason to stop watching, and what was lost is drift
                // proposals that stay open in the inbox regardless.
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => return,
            }

            // The window. Anything further inside it is counted, not
            // announced.
            let mut count: u32 = 1;
            let deadline = tokio::time::Instant::now() + DRIFT_COALESCE_WINDOW;
            loop {
                match tokio::time::timeout_at(deadline, rx.recv()).await {
                    Ok(Ok(event)) => {
                        if is_notifiable_drift(&event) {
                            count = count.saturating_add(1);
                        }
                    }
                    Ok(Err(RecvError::Lagged(_))) => continue,
                    Ok(Err(RecvError::Closed)) => break,
                    Err(_elapsed) => break,
                }
            }

            let title = if count == 1 {
                "New drift".to_string()
            } else {
                format!("{count} new drift proposals")
            };
            // The payload says what happened and where to act, and cannot say
            // more: FR-U10's event carries kind, entity kind, id and seq by
            // design — a client that wants the change reads it from Vogt — so
            // there is no summary here to quote. The inbox is on the projects
            // surface, which is where accepting or rejecting is possible; a
            // notification that opens somewhere you cannot act wasted the tap.
            let body = "The estate disagrees with what was declared about it. \
                        Tap to review the drift inbox.";
            let data = json!({
                "kind": "drift",
                "count": count,
                "url": "/#/projects",
            });
            let counts = state
                .push
                .notify(NotificationKind::Drift, &title, body, data)
                .await;
            info!(
                drift = count,
                ok = counts.ok,
                fail = counts.fail,
                queued = counts.queued,
                "drift push dispatched"
            );
        }
    });
}

/// Is this bus event drift that FR-M2 says is worth waking someone for?
fn is_notifiable_drift(event: &ServerEvent) -> bool {
    matches!(event, ServerEvent::VogtChanged { kind, .. }
        if DRIFT_NOTIFY_KINDS.contains(&kind.as_str()))
}

/// Spawn the background task that periodically scans live sessions and
/// pushes a one-shot notification when a session has sat continuously
/// `Idle` (not exited) for longer than `idle_stall_after_ms` — the case
/// where output just stops without printing a recognizable prompt, so the
/// `waiting-for-input` heuristic in `activity.rs` never fires.
pub fn spawn_idle_stall_watcher(state: Arc<AppState>) {
    use std::collections::HashSet;
    use uuid::Uuid;

    tokio::spawn(async move {
        let mut notified: HashSet<Uuid> = HashSet::new();
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
            let threshold = std::time::Duration::from_millis(state.config.idle_stall_after_ms);
            let mut still_idle: HashSet<Uuid> = HashSet::new();
            for session in state.sessions.live_sessions() {
                if session.exit_code().is_some() {
                    continue;
                }
                if session.activity() != ActivityState::Idle {
                    continue;
                }
                if session.activity_duration() < threshold {
                    continue;
                }
                still_idle.insert(session.id);
                if notified.contains(&session.id) {
                    continue;
                }
                let title = format!("{} has been idle a while", session.name());
                let body = "No output for a long time. Check whether it's stuck.";
                let data = json!({
                    "kind": "idle-stall",
                    "session_id": session.id.to_string(),
                    "url": format!("/#/t/{}", session.id),
                });
                let counts = state
                    .push
                    .notify(NotificationKind::IdleStall, &title, body, data)
                    .await;
                info!(session = %session.id, ok = counts.ok, fail = counts.fail, queued = counts.queued, "idle-stall push dispatched");
                notified.insert(session.id);
            }
            // Forget sessions that are no longer stalled (state changed, or
            // gone) so a future stall on the same id notifies again.
            notified.retain(|id| still_idle.contains(id));
        }
    });
}

pub fn spawn_digest_flusher(state: Arc<AppState>) {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            let counts = state.push.flush_ready_digests().await;
            if counts.ok > 0 || counts.fail > 0 {
                info!(ok = counts.ok, fail = counts.fail, "push digests flushed");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vogt_event(kind: &str) -> ServerEvent {
        ServerEvent::VogtChanged {
            kind: kind.to_string(),
            entity_kind: "drift_proposal".into(),
            entity_id: "d-1".into(),
            seq: 1,
            summary: serde_json::Value::Null,
        }
    }

    /// FR-M2's "and for nothing else by default", at the filter that keeps it.
    #[test]
    fn only_newly_raised_drift_notifies() {
        assert!(is_notifiable_drift(&vogt_event("drift.raised")));

        // Already being looked at by somebody.
        assert!(!is_notifiable_drift(&vogt_event("drift.resolved")));
        // Ordinary estate traffic, and the reason this is a named set rather
        // than a prefix: every one of these reaches the same bus.
        for kind in [
            "work.created",
            "work.transitioned",
            "project.imported",
            "observation.recorded",
            // The string the contract crate used to give as its example. If
            // this ever starts notifying, someone believed the comment over
            // the core.
            "drift.opened",
        ] {
            assert!(
                !is_notifiable_drift(&vogt_event(kind)),
                "{kind} must not notify"
            );
        }
    }

    /// A kind the core grows later must not opt itself in. This is the whole
    /// argument for a named set over `kind.starts_with("drift.")`.
    #[test]
    fn a_new_core_event_kind_does_not_opt_itself_in() {
        assert!(!is_notifiable_drift(&vogt_event("drift.escalated")));
        assert!(!is_notifiable_drift(&vogt_event("contract.violated")));
    }

    /// Nothing else on the bus is drift, whatever shape it arrives in.
    #[test]
    fn session_events_are_not_drift() {
        assert!(!is_notifiable_drift(&ServerEvent::Activity {
            id: uuid::Uuid::nil(),
            state: ActivityState::WaitingForInput,
            activity_changed_at: String::new(),
        }));
    }
}
