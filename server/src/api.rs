use std::{convert::Infallible, sync::Arc, time::Duration};

use axum::{
    extract::{Path, State},
    response::{sse::Event, Sse},
    Json,
};
use futures_util::Stream;
use serde::Deserialize;
use serde_json::json;
use tokio_stream::{wrappers::BroadcastStream, StreamExt};
use uuid::Uuid;

use crate::{
    app::AppState,
    error::Result,
    pty::{SessionSpec, SessionSummary},
};

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
) -> Result<Json<serde_json::Value>> {
    let s = state.sessions.get(id)?;
    let (snap, pos) = s.snapshot();
    let body = json!({
        "summary": s.summary(),
        "scrollback_pos": pos,
        "scrollback_base64": base64_encode(&snap),
    });
    Ok(Json(body))
}

#[derive(Debug, Deserialize)]
pub struct RenameReq {
    pub name: String,
}

pub async fn rename_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Json(req): Json<RenameReq>,
) -> Result<Json<serde_json::Value>> {
    state.sessions.rename(id, req.name)?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn delete_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    state.sessions.remove(id)?;
    Ok(Json(json!({ "ok": true })))
}

pub async fn kill_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    state.sessions.kill(id)?;
    Ok(Json(json!({ "ok": true })))
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

pub async fn healthz() -> Json<serde_json::Value> {
    Json(json!({ "ok": true }))
}

fn base64_encode(b: &[u8]) -> String {
    use std::fmt::Write as _;
    // Hand-roll to avoid pulling in another crate at this stage.
    const TBL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(b.len().div_ceil(3) * 4);
    let mut i = 0;
    while i + 3 <= b.len() {
        let n = ((b[i] as u32) << 16) | ((b[i + 1] as u32) << 8) | (b[i + 2] as u32);
        out.push(TBL[((n >> 18) & 0x3f) as usize] as char);
        out.push(TBL[((n >> 12) & 0x3f) as usize] as char);
        out.push(TBL[((n >> 6) & 0x3f) as usize] as char);
        out.push(TBL[(n & 0x3f) as usize] as char);
        i += 3;
    }
    let rem = b.len() - i;
    if rem == 1 {
        let n = (b[i] as u32) << 16;
        let _ = write!(
            out,
            "{}{}==",
            TBL[((n >> 18) & 0x3f) as usize] as char,
            TBL[((n >> 12) & 0x3f) as usize] as char
        );
    } else if rem == 2 {
        let n = ((b[i] as u32) << 16) | ((b[i + 1] as u32) << 8);
        let _ = write!(
            out,
            "{}{}{}=",
            TBL[((n >> 18) & 0x3f) as usize] as char,
            TBL[((n >> 12) & 0x3f) as usize] as char,
            TBL[((n >> 6) & 0x3f) as usize] as char
        );
    }
    out
}
