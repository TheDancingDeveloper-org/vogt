//! HTTP surface for the conversational assistant. All routes 404 when the
//! assistant is disabled (no `assistant_api_key` configured) so the feature
//! is invisible unless provisioned.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    Json,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    app::AppState,
    assistant::{AssistantReply, AssistantRuntime, PendingActionView, TranscriptEntry},
    error::{ApiError, Result},
};

fn runtime(state: &AppState) -> Result<Arc<AssistantRuntime>> {
    state.assistant.clone().ok_or(ApiError::NotFound)
}

#[derive(Debug, Deserialize)]
pub struct MessageReq {
    pub text: String,
}

pub async fn message(
    State(state): State<Arc<AppState>>,
    Json(req): Json<MessageReq>,
) -> Result<Json<AssistantReply>> {
    let rt = runtime(&state)?;
    Ok(Json(rt.handle_message(req.text).await?))
}

#[derive(Debug, Deserialize)]
pub struct ActionReq {
    pub approve: bool,
}

pub async fn resolve_action(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Json(req): Json<ActionReq>,
) -> Result<Json<AssistantReply>> {
    let rt = runtime(&state)?;
    Ok(Json(rt.resolve_action(id, req.approve).await?))
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
