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
    let title = req.title.unwrap_or_else(|| "MyDevEnv2 test".into());
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
/// notification when any session transitions to `waiting-for-input`.
pub fn spawn_activity_watcher(state: Arc<AppState>) {
    let mut rx = state.bus.subscribe();
    tokio::spawn(async move {
        while let Ok(ev) = rx.recv().await {
            if let ServerEvent::Activity { id, state: act } = ev {
                if act == ActivityState::WaitingForInput {
                    let session = state.sessions.get(id).ok();
                    let name = session
                        .as_ref()
                        .map(|s| s.name())
                        .unwrap_or_else(|| id.to_string());
                    let title = format!("{name} is waiting for input");
                    let body = "Tap to open the session in MyDevEnv2.";
                    let data = json!({
                        "kind": "waiting-for-input",
                        "session_id": id.to_string(),
                        "url": format!("/#/t/{id}"),
                    });
                    let counts = state
                        .push
                        .notify(NotificationKind::WaitingForInput, &title, body, data)
                        .await;
                    info!(session = %id, ok = counts.ok, fail = counts.fail, queued = counts.queued, "push dispatched");
                }
            }
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
