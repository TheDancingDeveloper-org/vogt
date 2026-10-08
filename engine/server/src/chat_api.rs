//! HTTP surface for quick chats (WI-1097). Every route 404s when chats are
//! not available (`ENGINE_CHAT_ENABLED=0`, or no Klaudia launch configured),
//! so the feature is invisible unless it can work.
//!
//! All of them except the gate sit behind the bearer gate and need the
//! `sessions` capability (`auth::required_capability`): a chat starts an
//! agent and its transcript is a shared record, like a session's. The gate is
//! outside it and authenticates the per-process token the engine gave the
//! chat's own agent, as the secret broker does for a session.

use std::{convert::Infallible, sync::Arc, time::Duration};

use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    response::sse::{Event, KeepAlive, Sse},
    Extension, Json,
};
use futures_util::Stream;
use serde::Deserialize;
use serde_json::Value;
use tokio_stream::{wrappers::BroadcastStream, StreamExt};
use uuid::Uuid;
use vogt_engine_contract::{
    ChatApproval, ChatArchiveRequest, ChatCreateRequest, ChatDecisionRequest, ChatDetail,
    ChatEvent, ChatModelRequest, ChatPromoteRequest, ChatPromoteResult, ChatSendRequest,
    ChatSendResult, ChatSummary,
};

use crate::{
    app::AppState,
    auth::AuthorizedIdentity,
    chat_store::ChatQuery,
    chats::ChatRuntime,
    error::{ApiError, Result},
};

fn runtime(state: &AppState) -> Result<Arc<ChatRuntime>> {
    state.chats.clone().ok_or(ApiError::NotFound)
}

/// Who is acting: the caller, or — only from vogt-core's own credential,
/// which authenticated them — the actor the core relays.
fn actor(identity: Option<&AuthorizedIdentity>, relayed: Option<&str>) -> String {
    match identity {
        Some(i) if i.stack_secret => relayed
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| i.name.clone()),
        Some(i) => i.name.clone(),
        None => "unidentified".into(),
    }
}

#[derive(Debug, Deserialize)]
pub struct ListParams {
    pub q: Option<String>,
    /// `true` archived only, `false` open only (the default), `all` both.
    pub archived: Option<String>,
    pub limit: Option<usize>,
}

pub async fn list(
    State(state): State<Arc<AppState>>,
    Query(params): Query<ListParams>,
) -> Result<Json<Vec<ChatSummary>>> {
    let archived = match params.archived.as_deref().map(str::trim) {
        None | Some("") | Some("false") => Some(false),
        Some("true") => Some(true),
        Some("all") => None,
        Some(other) => {
            return Err(ApiError::BadRequest(format!(
                "archived={other:?}: use true, false or all"
            )))
        }
    };
    Ok(Json(
        runtime(&state)?
            .list(ChatQuery {
                q: params.q.filter(|q| !q.trim().is_empty()),
                archived,
                limit: params.limit.unwrap_or(100),
            })
            .await?,
    ))
}

pub async fn create(
    State(state): State<Arc<AppState>>,
    identity: Option<Extension<AuthorizedIdentity>>,
    Json(req): Json<ChatCreateRequest>,
) -> Result<Json<ChatSendResult>> {
    let creator = actor(identity.as_deref(), req.creator.as_deref());
    Ok(Json(runtime(&state)?.create(req, creator).await?))
}

#[derive(Debug, Deserialize)]
pub struct GetParams {
    /// The newest this many entries (default 500).
    pub tail: Option<usize>,
}

pub async fn get(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Query(params): Query<GetParams>,
) -> Result<Json<ChatDetail>> {
    Ok(Json(
        runtime(&state)?.get(id, params.tail.unwrap_or(500)).await?,
    ))
}

pub async fn send(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    identity: Option<Extension<AuthorizedIdentity>>,
    Json(req): Json<ChatSendRequest>,
) -> Result<Json<ChatSendResult>> {
    let by = actor(identity.as_deref(), req.by.as_deref());
    let wait = Duration::from_secs(u64::from(req.wait_secs.unwrap_or(0).min(300)));
    Ok(Json(runtime(&state)?.send(id, &req.text, by, wait).await?))
}

pub async fn decide(
    State(state): State<Arc<AppState>>,
    Path((id, approval_id)): Path<(Uuid, String)>,
    identity: Option<Extension<AuthorizedIdentity>>,
    Json(req): Json<ChatDecisionRequest>,
) -> Result<Json<ChatApproval>> {
    let identity = identity.as_deref();
    // The WI-983 rule: an approval is a person's to give. An agent — the
    // chat's own, or one driving it over MCP — is refused.
    if !crate::person_gate::is_person(identity, req.person) {
        let who = identity.map_or("unidentified", |i| i.name.as_str());
        tracing::warn!(
            target: "vogt::audit",
            event = "chat.approval",
            outcome = "refused",
            chat = %id,
            approval = %approval_id,
            principal = %who,
            "refused an agent's answer to a chat approval; only a person answers one"
        );
        return Err(ApiError::Forbidden(format!(
            "person required: only a person answers a chat's approval, and {who} is not one"
        )));
    }
    let who = identity.map_or_else(|| "unidentified".to_string(), |i| i.name.clone());
    Ok(Json(
        runtime(&state)?
            .decide(id, &approval_id, req.allow, req.message, who)
            .await?,
    ))
}

pub async fn set_model(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Json(req): Json<ChatModelRequest>,
) -> Result<Json<ChatSummary>> {
    Ok(Json(runtime(&state)?.set_model(id, &req.model).await?))
}

pub async fn interrupt(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<ChatSummary>> {
    Ok(Json(runtime(&state)?.interrupt(id).await?))
}

pub async fn archive(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Json(req): Json<ChatArchiveRequest>,
) -> Result<Json<ChatSummary>> {
    Ok(Json(runtime(&state)?.archive(id, req.archived).await?))
}

pub async fn promote(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Json(req): Json<ChatPromoteRequest>,
) -> Result<Json<ChatPromoteResult>> {
    Ok(Json(runtime(&state)?.promote(id, req).await?))
}

/// The chat's live events. A client that reconnects re-reads the chat; a
/// lag is said in band, as on `/api/events`.
pub async fn events(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Sse<impl Stream<Item = std::result::Result<Event, Infallible>>>> {
    let runtime = runtime(&state)?;
    // A chat that does not exist is a 404 rather than a stream that never
    // says anything.
    runtime.get(id, 1).await?;
    let stream = BroadcastStream::new(runtime.subscribe(id)).filter_map(|res| {
        let event = match res {
            Ok(event) => event,
            Err(tokio_stream::wrappers::errors::BroadcastStreamRecvError::Lagged(skipped)) => {
                ChatEvent::Lagged { skipped }
            }
        };
        serde_json::to_string(&event)
            .ok()
            .map(|json| Ok(Event::default().data(json)))
    });
    Ok(Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("ka"),
    ))
}

/// Path of the gate, which the chat's own hook calls.
pub const GATE_ROUTE: &str = "/api/chats/{id}/gate";

/// The `PreToolUse` hook's question. Answers the hook's JSON: `{}` lets the
/// call run, `{"decision":"block","reason":…}` refuses it. Anything else the
/// hook gets — a 401, a 404, a dropped connection — it turns into a block.
pub async fn gate(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Result<Json<Value>> {
    let runtime = runtime(&state)?;
    let token = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    Ok(Json(runtime.gate(id, token, body).await?))
}
