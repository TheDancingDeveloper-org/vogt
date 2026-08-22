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
    extract::{Path, Query, State},
    Extension, Json,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    app::AppState,
    assistant::{AssistantReply, AssistantRuntime, PendingActionView, TranscriptEntry},
    assistant_log::{ListQuery, LoggedEntry},
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
    // With profiles (FR-T9) this fires only when *no* configured route can
    // answer; a request naming one that cannot is refused in `message`, where
    // the profile is known and can be named.
    if let Some(reason) = runtime.refusal() {
        return Err(ApiError::Config(reason));
    }
    Ok(runtime)
}

#[derive(Debug, Deserialize)]
pub struct MessageReq {
    pub text: String,
    /// The raw recognised utterance behind a voice turn, before FR-T13's
    /// repair pass. Sent by a voice client so the durable log can carry the raw
    /// form beside the repaired `text` (FR-T14). Absent for a typed turn.
    #[serde(default)]
    pub utterance: Option<String>,
    /// Which configured provider profile to run this turn against (FR-T9).
    /// Absent means the deployment's default.
    #[serde(default)]
    pub profile: Option<String>,
}

/// One conversational turn.
///
/// Cancellation-safe by construction (#242): the composer's Stop button aborts
/// the fetch, which closes the connection, and axum drops this handler's future
/// at its next await point. Dropping it cancels the in-flight request to the
/// model with it, releases the conversation lock, and logs nothing — a client
/// that went away is not an error and needs no cleanup path of its own.
pub async fn message(
    State(state): State<Arc<AppState>>,
    identity: Option<Extension<AuthorizedIdentity>>,
    Json(req): Json<MessageReq>,
) -> Result<Json<AssistantReply>> {
    let rt = runtime(&state)?;
    let caller = Caller::from_identity(identity.map(|Extension(id)| id));
    Ok(Json(
        rt.handle_message(caller, req.text, req.utterance, req.profile)
            .await?,
    ))
}

#[derive(Debug, Deserialize)]
pub struct ActionReq {
    pub approve: bool,
}

#[derive(Debug, Deserialize)]
pub struct ReasonReq {
    pub reason: String,
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

/// Preview an edited reason without approving or delivering the held write.
/// The runtime keeps the original action id and expiry, and returns the exact
/// regenerated card the next approval will review.
pub async fn replace_reason(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Json(req): Json<ReasonReq>,
) -> Result<Json<PendingActionView>> {
    let rt = runtime(&state)?;
    Ok(Json(rt.replace_pending_reason(id, req.reason).await?))
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
) -> Result<Json<vogt_engine_contract::OkResponse>> {
    let rt = runtime(&state)?;
    rt.reset().await;
    Ok(Json(vogt_engine_contract::OkResponse::new(true)))
}

#[derive(Debug, Deserialize)]
pub struct LogQuery {
    #[serde(default = "default_log_limit")]
    pub limit: usize,
    #[serde(default)]
    pub offset: usize,
    /// Restrict to one actor's interactions.
    #[serde(default)]
    pub actor: Option<String>,
}

fn default_log_limit() -> usize {
    100
}

/// The durable interaction log (FR-T14), newest first.
///
/// Scope-gated: unlike `history` — the ephemeral in-memory transcript of the
/// current conversation — this is a durable, cross-conversation record, so the
/// route requires the `assistant` token capability (FR-S3), enforced in
/// `auth::required_capability`. Entries carry external content still delimited
/// as untrusted data (FR-T4), exactly as it was fed to the model.
pub async fn log(
    State(state): State<Arc<AppState>>,
    Query(q): Query<LogQuery>,
) -> Result<Json<Vec<LoggedEntry>>> {
    let rt = runtime(&state)?;
    let entries = rt
        .read_log(ListQuery {
            limit: q.limit,
            offset: q.offset,
            actor: q.actor,
        })
        .await?;
    Ok(Json(entries))
}
