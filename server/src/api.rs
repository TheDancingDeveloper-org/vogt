use std::{convert::Infallible, sync::Arc, time::Duration};

use axum::{
    extract::{Path, State},
    response::{sse::Event, Sse},
    Json,
};
use base64::Engine as _;
use futures_util::Stream;
use mydevenv2_contract::{OkResponse, SessionDetail, SessionSummary};
use serde::{Deserialize, Serialize};
use tokio_stream::{wrappers::BroadcastStream, StreamExt};
use uuid::Uuid;

use crate::{app::AppState, error::Result, pty::SessionSpec};

pub async fn list_sessions(State(state): State<Arc<AppState>>) -> Json<Vec<SessionSummary>> {
    Json(state.sessions.list())
}

pub async fn create_session(
    State(state): State<Arc<AppState>>,
    Json(spec): Json<SessionSpec>,
) -> Result<Json<SessionSummary>> {
    let s = state.sessions.create(spec)?;
    Ok(Json(s.summary()))
}

pub async fn get_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<SessionDetail>> {
    let s = state.sessions.get(id)?;
    let (snap, pos) = s.snapshot();
    Ok(Json(SessionDetail {
        summary: s.summary(),
        scrollback_pos: pos,
        scrollback_base64: base64::engine::general_purpose::STANDARD.encode(&snap),
    }))
}

#[derive(Debug, Deserialize)]
pub struct RenameReq {
    pub name: String,
}

pub async fn rename_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Json(req): Json<RenameReq>,
) -> Result<Json<OkResponse>> {
    state.sessions.rename(id, req.name)?;
    Ok(Json(OkResponse::new(true)))
}

pub async fn delete_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<OkResponse>> {
    state.sessions.remove(id)?;
    Ok(Json(OkResponse::new(true)))
}

pub async fn kill_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<OkResponse>> {
    state.sessions.kill(id)?;
    Ok(Json(OkResponse::new(true)))
}

pub async fn events_stream(
    State(state): State<Arc<AppState>>,
) -> Sse<impl Stream<Item = std::result::Result<Event, Infallible>>> {
    let rx = state.bus.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|res| match res {
        Ok(ev) => match serde_json::to_string(&ev) {
            Ok(json) => Some(Ok(Event::default().data(json))),
            Err(_) => None,
        },
        Err(_) => None, // lagging receiver — skip
    });
    Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("ka"),
    )
}

pub async fn healthz() -> Json<OkResponse> {
    Json(OkResponse::new(true))
}

#[derive(Debug, Serialize)]
pub struct OperationalStatus {
    pub version: &'static str,
    pub session_count: usize,
    pub push_subscription_count: usize,
    pub gui_process_count: usize,
    pub gui_stream_configured: bool,
    pub fcm_enabled: bool,
    pub history: HistoryStatus,
    pub auth_broker: AuthBrokerStatus,
    pub storage: ServerStorageStatus,
}

#[derive(Debug, Serialize)]
pub struct HistoryStatus {
    pub enabled: bool,
    pub archived_session_count: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct AuthBrokerStatus {
    pub auto_agent_auth: bool,
    pub helper: String,
}

#[derive(Debug, Serialize)]
pub struct ServerStorageStatus {
    pub state_dir: String,
    pub workspace_root: String,
}

pub async fn operational_status(
    State(state): State<Arc<AppState>>,
) -> Result<Json<OperationalStatus>> {
    let archived_session_count = match state.history.as_ref() {
        Some(history) => Some(history.count_sessions().await?),
        None => None,
    };

    Ok(Json(OperationalStatus {
        version: env!("CARGO_PKG_VERSION"),
        session_count: state.sessions.list().len(),
        push_subscription_count: state.push.list().len(),
        gui_process_count: state.gui.count_alive(),
        gui_stream_configured: state.config.gui_stream_url.is_some(),
        fcm_enabled: state.config.fcm_service_account_json.is_some(),
        history: HistoryStatus {
            enabled: state.history.is_some(),
            archived_session_count,
        },
        auth_broker: AuthBrokerStatus {
            auto_agent_auth: state.config.auto_agent_auth,
            helper: state
                .config
                .agent_auth_helper
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| state.config.agent_auth_helper.display().to_string()),
        },
        storage: ServerStorageStatus {
            state_dir: state.config.state_dir.display().to_string(),
            workspace_root: state.config.workspace_root.display().to_string(),
        },
    }))
}
