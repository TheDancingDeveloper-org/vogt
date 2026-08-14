//! HTTP surface for the conversational assistant. All routes 404 when the
//! assistant is disabled (no `assistant_api_key` configured) so the feature
//! is invisible unless provisioned.
//!
//! The two routes that can cause something to happen carry the caller's
//! identity into the runtime. `require_bearer` has already decided who this
//! request is and left an `AuthorizedIdentity` in the extensions; taking it
//! here is what lets a Vogt write be made with the approving user's own core
//! token rather than a shared one (FR-T3). It is `Option` because the type
//! system cannot know these routes sit behind the gate — a request without an
//! identity gets a caller with no pairing, which can read nothing privileged
//! and write nothing at all.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    Extension, Json,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    app::AppState,
    assistant::{AssistantReply, AssistantRuntime, PendingActionView, TranscriptEntry},
    auth::AuthorizedIdentity,
    error::{ApiError, Result},
    vogt_tools::Caller,
};

fn runtime(state: &AppState) -> Result<Arc<AssistantRuntime>> {
    let runtime = state.assistant.clone().ok_or(ApiError::NotFound)?;
    // FR-T7: a configuration that would hang refuses here instead, with the
    // sentence that says which model, which transport, and which setting
    // overrides it. Refused rather than 404: the assistant *is* provisioned,
    // and reporting it absent would send an operator looking for a missing
    // API key.
    if let Some(reason) = runtime.refusal() {
        return Err(ApiError::Config(reason.to_string()));
    }
    Ok(runtime)
}

#[derive(Debug, Deserialize)]
pub struct MessageReq {
    pub text: String,
}

pub async fn message(
    State(state): State<Arc<AppState>>,
    identity: Option<Extension<AuthorizedIdentity>>,
    Json(req): Json<MessageReq>,
) -> Result<Json<AssistantReply>> {
    let rt = runtime(&state)?;
    let caller = Caller::from_identity(identity.map(|Extension(id)| id));
    Ok(Json(rt.handle_message(caller, req.text).await?))
}

#[derive(Debug, Deserialize)]
pub struct ActionReq {
    pub approve: bool,
}

/// Approve or deny. The identity on *this* request is the approving user, and
/// it is theirs a Vogt write is made with — not the one who sent the message
/// that proposed it, if a different token did.
pub async fn resolve_action(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    identity: Option<Extension<AuthorizedIdentity>>,
    Json(req): Json<ActionReq>,
) -> Result<Json<AssistantReply>> {
    let rt = runtime(&state)?;
    let caller = Caller::from_identity(identity.map(|Extension(id)| id));
    Ok(Json(rt.resolve_action(caller, id, req.approve).await?))
}

#[derive(Debug, serde::Serialize)]
pub struct HistoryResponse {
    pub transcript: Vec<TranscriptEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_action: Option<PendingActionView>,
}

pub async fn history(State(state): State<Arc<AppState>>) -> Result<Json<HistoryResponse>> {
    let rt = runtime(&state)?;
    Ok(Json(HistoryResponse {
        transcript: rt.history().await,
        pending_action: rt.pending_action().await,
    }))
}

pub async fn reset(
    State(state): State<Arc<AppState>>,
) -> Result<Json<mydevenv2_contract::OkResponse>> {
    let rt = runtime(&state)?;
    rt.reset().await;
    Ok(Json(mydevenv2_contract::OkResponse::new(true)))
}
