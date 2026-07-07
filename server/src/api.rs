use std::{convert::Infallible, sync::Arc, time::Duration};

use axum::{
    extract::{Path, State},
    response::{sse::Event, Sse},
    Json,
};
use base64::Engine as _;
use futures_util::Stream;
use mydevenv2_contract::{OkResponse, SessionDetail, SessionSummary};
use serde::Deserialize;
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
