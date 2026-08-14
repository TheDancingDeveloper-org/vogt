//! Same-origin proxy for the ContextKeeper recovery workflow.
//!
//! The browser never receives ContextKeeper's control token, so every call the
//! PWA makes lands here and is forwarded server-side. Only the operations the
//! UI actually needs are exposed — this is a deliberate allow-list, not a
//! transparent pass-through, because ContextKeeper's API also carries prune and
//! maintenance routes that no browser should be able to reach.
//!
//! With no ContextKeeper configured every route answers `404`, and the PWA
//! reads that as "continuity is unavailable" rather than as an error.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{
    app::AppState,
    error::{ApiError, Result},
};

fn runtime(state: &Arc<AppState>) -> Result<&Arc<crate::contextkeeper::ContextKeeperRuntime>> {
    state.contextkeeper.as_ref().ok_or(ApiError::NotFound)
}

/// Whether continuity is available at all, and how fresh capture is.
///
/// Answers `200` even when the sidecar is unreachable: "configured but not
/// reachable" is exactly what the PWA needs in order to show every terminal as
/// unprotected instead of pretending the feature is absent.
pub async fn health(State(state): State<Arc<AppState>>) -> Json<Value> {
    match state.contextkeeper.as_ref() {
        Some(runtime) => Json(runtime.health_snapshot()),
        None => Json(json!({"configured": false, "reachable": false})),
    }
}

/// Continuity for one terminal, keyed by MyDevEnv2's PTY id.
pub async fn session_continuity(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>> {
    let runtime = runtime(&state)?;
    match runtime.continuity_for(&id.to_string()) {
        Some(continuity) => Ok(Json(
            serde_json::to_value(continuity).unwrap_or(Value::Null),
        )),
        None => Ok(Json(json!({"state": "unprotected"}))),
    }
}

/// The continuation recipe for a ContextKeeper registry session.
///
/// ContextKeeper picks the rung; MyDevEnv2 creates the PTY from the recipe's
/// command, cwd, and env. `kind: "reattach"` means attach the existing terminal
/// and start nothing.
pub async fn continuation(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> Result<Json<Value>> {
    Ok(Json(runtime(&state)?.continuation(&session_id).await?))
}

pub async fn session(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> Result<Json<Value>> {
    Ok(Json(runtime(&state)?.session(&session_id).await?))
}

/// Every attempt in one durable work session, so earlier attempts stay
/// reachable after a recovery replaces the terminal.
pub async fn work_session(
    State(state): State<Arc<AppState>>,
    Path(work_id): Path<String>,
) -> Result<Json<Value>> {
    Ok(Json(runtime(&state)?.work_session(&work_id).await?))
}

/// Compile and return the deterministic recovery bundle.
///
/// Preview is a separate step from approval on purpose: approval is a human
/// deciding about *this* bundle, so the UI must have shown it first.
pub async fn preview(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
) -> Result<Json<Value>> {
    Ok(Json(runtime(&state)?.preview(&session_id).await?))
}

#[derive(Debug, Deserialize)]
pub struct BundleReq {
    pub bundle_id: String,
    /// Supplied by the client so a retried click replays the same operation
    /// instead of performing a second one.
    #[serde(default)]
    pub request_id: Option<String>,
}

impl BundleReq {
    fn body(&self, prefix: &str) -> Value {
        json!({
            "bundle_id": self.bundle_id,
            "request_id": self
                .request_id
                .clone()
                .unwrap_or_else(|| format!("{prefix}-{}", Uuid::new_v4())),
        })
    }
}

pub async fn approve(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
    Json(req): Json<BundleReq>,
) -> Result<Json<Value>> {
    let body = req.body("mydevenv2-approve");
    Ok(Json(runtime(&state)?.approve(&session_id, &body).await?))
}

/// Launch an approved recovery.
///
/// ContextKeeper fails closed here: with no readable approved bundle nothing
/// starts, and with no launcher available it returns a copyable command for the
/// user to run instead. Both are answers the PWA renders rather than errors.
pub async fn launch(
    State(state): State<Arc<AppState>>,
    Path(session_id): Path<String>,
    Json(req): Json<BundleReq>,
) -> Result<Json<Value>> {
    let body = req.body("mydevenv2-launch");
    Ok(Json(runtime(&state)?.launch(&session_id, &body).await?))
}
