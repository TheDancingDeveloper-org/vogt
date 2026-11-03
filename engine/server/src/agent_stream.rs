//! Authenticated relay for outbound pod session agents.
use crate::app::AppState;
use axum::{
    extract::{Path, State, WebSocketUpgrade},
    http::{header, HeaderMap, StatusCode},
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;
use tokio::sync::Mutex;
use uuid::Uuid;
use vogt_session_agent::Frame;
const BOOTSTRAP_TTL: Duration = Duration::from_secs(300);
static AUTH: OnceLock<Arc<AgentAuth>> = OnceLock::new();
#[derive(Debug, Clone)]
struct Pending {
    token: Vec<u8>,
    expires: Instant,
}
#[derive(Debug, Clone)]
struct SessionCredential {
    token: Vec<u8>,
}
#[derive(Debug, Default)]
pub struct AgentAuth {
    pending: Mutex<HashMap<Uuid, Pending>>,
    sessions: Mutex<HashMap<Uuid, SessionCredential>>,
}
#[derive(Debug, Deserialize)]
pub struct BootstrapRequest {
    pub session_id: String,
    pub graft_repo: String,
    pub graft_line: String,
    pub cli: String,
    #[serde(default)]
    pub extra_ca_file: Option<String>,
}
#[derive(Debug, Serialize)]
pub struct BootstrapResponse {
    pub session_token: String,
    pub bundle: serde_json::Value,
    pub env: serde_json::Value,
    pub brief: String,
}
pub fn global() -> Arc<AgentAuth> {
    Arc::clone(AUTH.get_or_init(|| Arc::new(AgentAuth::default())))
}

impl AgentAuth {
    pub async fn issue(&self, id: Uuid, token: &[u8]) {
        self.pending.lock().await.insert(
            id,
            Pending {
                token: token.to_vec(),
                expires: Instant::now() + BOOTSTRAP_TTL,
            },
        );
    }
    async fn consume(&self, id: Uuid, candidate: &[u8]) -> bool {
        let mut p = self.pending.lock().await;
        let Some(entry) = p.remove(&id) else {
            return false;
        };
        if entry.expires < Instant::now() || !bool::from(entry.token.as_slice().ct_eq(candidate)) {
            return false;
        }
        let token = *Uuid::new_v4().as_bytes();
        self.sessions.lock().await.insert(
            id,
            SessionCredential {
                token: token.to_vec(),
            },
        );
        true
    }
    async fn session_ok(&self, id: Uuid, candidate: &[u8]) -> bool {
        self.sessions
            .lock()
            .await
            .get(&id)
            .is_some_and(|c| bool::from(c.token.as_slice().ct_eq(candidate)))
    }
    async fn session_token(&self, id: Uuid) -> Option<String> {
        self.sessions
            .lock()
            .await
            .get(&id)
            .map(|c| c.token.iter().map(|b| format!("{b:02x}")).collect())
    }
}
fn bearer(h: &HeaderMap) -> Option<&[u8]> {
    h.get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::as_bytes)
}
pub async fn bootstrap(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::Json(req): axum::Json<BootstrapRequest>,
) -> impl IntoResponse {
    let Ok(id) = Uuid::parse_str(req.session_id.trim()) else {
        return (StatusCode::BAD_REQUEST, "invalid session_id").into_response();
    };
    let Some(token) = bearer(&headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if !state.agent_auth.consume(id, token).await {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Some(session_token) = state.agent_auth.session_token(id).await else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let engine = state
        .config
        .agent_auth_public_url
        .clone()
        .unwrap_or_default();
    axum::Json(BootstrapResponse{session_token,bundle:serde_json::json!({"repo":req.graft_repo,"line":req.graft_line}),env:serde_json::json!({"VOGT_ENGINE_URL":engine,"VOGT_SESSION_ID":id.to_string(),"VOGT_CLI":req.cli,"VOGT_EXTRA_CA_FILE":req.extra_ca_file}),brief:state.sessions.summary_of(id).map(|s|s.name).unwrap_or_default()}).into_response()
}
pub async fn stream(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let Some(token) = bearer(&headers).map(<[u8]>::to_vec) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if !state.agent_auth.session_ok(id, &token).await {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    ws.on_upgrade(move |socket| relay(socket, state, id))
        .into_response()
}
async fn relay(mut socket: axum::extract::ws::WebSocket, state: Arc<AppState>, id: Uuid) {
    let Ok(session) = state.sessions.get(id) else {
        return;
    };
    let hello = match socket.next().await {
        Some(Ok(axum::extract::ws::Message::Text(t))) => serde_json::from_str::<Frame>(&t).ok(),
        _ => None,
    };
    if !matches!(hello, Some(Frame::Hello { ref session_id }) if session_id == &id.to_string()) {
        let _ = socket.close().await;
        return;
    }
    let mut output = session.subscribe();
    let mut cursor = None;
    if let Ok(Some(Ok(axum::extract::ws::Message::Text(t)))) =
        tokio::time::timeout(Duration::from_millis(50), socket.next()).await
    {
        if let Ok(Frame::Resume { cursor: c }) = serde_json::from_str(&t) {
            cursor = Some(c);
        }
    }
    let (snapshot, pos, _) = session.snapshot_for_attach(cursor, None);
    if !snapshot.is_empty() {
        let frame = Frame::Output {
            offset: pos - snapshot.len() as u64,
            data: snapshot.to_vec(),
        };
        let _ = socket
            .send(axum::extract::ws::Message::Text(
                serde_json::to_string(&frame).unwrap().into(),
            ))
            .await;
    }
    loop {
        tokio::select! {
            msg = output.recv() => match msg {
                Ok(chunk) => {
                    let frame = Frame::Output { offset: chunk.pos, data: chunk.data.to_vec() };
                    if socket.send(axum::extract::ws::Message::Text(serde_json::to_string(&frame).unwrap().into())).await.is_err() { break; }
                }
                Err(_) => break,
            },
            msg = socket.next() => match msg {
                Some(Ok(axum::extract::ws::Message::Text(t))) => if let Ok(frame) = serde_json::from_str::<Frame>(&t) {
                    match frame {
                        Frame::Input { data } => { let _ = session.write_input(&data); }
                        Frame::Resize { cols, rows } => { let _ = session.resize(cols, rows); }
                        Frame::Signal { signal } if signal == "kill" => { let _ = session.kill(); }
                        Frame::Heartbeat { cursor } => {
                            let ack = Frame::Ack { offset: cursor };
                            let _ = socket.send(axum::extract::ws::Message::Text(serde_json::to_string(&ack).unwrap().into())).await;
                        }
                        Frame::Exited { status } => { let _ = session.mark_done(status.map(|s| s.to_string()).unwrap_or_default()); break; }
                        _ => {}
                    }
                },
                Some(Ok(axum::extract::ws::Message::Close(_))) | None => break,
                _ => {}
            }
        }
    }
}
