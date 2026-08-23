use std::{sync::Arc, time::Duration};

use axum::{
    extract::{
        ws::{CloseCode, CloseFrame, Message, WebSocket, WebSocketUpgrade},
        Path, Query, State,
    },
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::broadcast::error::RecvError;
use uuid::Uuid;
use vogt_engine_contract::{ClientControl, ServerControl};

use crate::{app::AppState, auth};

#[derive(Debug, Deserialize)]
pub struct AttachQuery {
    /// Legacy: token in query string. Deprecated — left in only so an existing
    /// client that hasn't been redeployed still works. Real auth is via the
    /// first text frame `{"type":"auth","token":"..."}`. Tokens passed here
    /// land in proxy/access logs and shouldn't be relied on for new clients.
    pub token: Option<String>,
}

/// Chunk size for streaming the scrollback snapshot back to the client.
/// xterm.js handles big binary frames fine, but keeping each under 64 KiB
/// avoids spikes in browser memory while the buffer parses.
const SNAPSHOT_CHUNK: usize = 64 * 1024;

/// Maximum input accepted in one frame. This prevents an accidental clipboard
/// dump from becoming a multi-megabyte editable command line inside the PTY.
const MAX_INPUT_BYTES: usize = 64 * 1024;

/// How long a freshly-upgraded socket has to send `{"type":"auth",...}` before
/// we drop it. Keeps unauth clients from hanging on to a socket indefinitely.
const AUTH_DEADLINE: Duration = Duration::from_secs(5);

pub async fn attach(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Query(q): Query<AttachQuery>,
) -> impl IntoResponse {
    // Do not check auth before upgrading — the bearer is supplied by the
    // first client text frame so it doesn't end up in access/proxy logs.
    // The legacy ?token= path still works for older clients.
    let legacy = q.token.clone();
    ws.on_upgrade(move |socket| handle_socket(socket, state, id, legacy))
}

async fn close_with(socket: &mut WebSocket, code: CloseCode, reason: &'static str) {
    let _ = socket
        .send(Message::Close(Some(CloseFrame {
            code,
            reason: reason.into(),
        })))
        .await;
}

fn token_ok(state: &AppState, candidate: &str) -> bool {
    auth::ws_token_allows_session_access(state, candidate)
}

/// Read the first frame after upgrade. Must be an `auth` control frame OR the
/// legacy `?token=` query param must have been correct. Returns Some(()) on
/// success, None on failure (after sending a close frame).
async fn authenticate(
    socket: &mut WebSocket,
    state: &Arc<AppState>,
    legacy_token: Option<&str>,
) -> Option<Option<u64>> {
    if let Some(tok) = legacy_token {
        if token_ok(state, tok) {
            return Some(None);
        }
    }

    let first = match tokio::time::timeout(AUTH_DEADLINE, socket.recv()).await {
        Ok(Some(Ok(msg))) => msg,
        Ok(Some(Err(_))) | Ok(None) => {
            return None;
        }
        Err(_) => {
            close_with(socket, 4408, "auth timeout").await;
            return None;
        }
    };
    let text = match first {
        Message::Text(s) => s,
        _ => {
            close_with(socket, 4401, "auth frame required").await;
            return None;
        }
    };
    let parsed: ClientControl = match serde_json::from_str(&text) {
        Ok(c) => c,
        Err(_) => {
            close_with(socket, 4401, "auth frame required").await;
            return None;
        }
    };
    match parsed {
        ClientControl::Auth { token, resume_from } if token_ok(state, &token) => Some(resume_from),
        _ => {
            close_with(socket, 4401, "unauthorized").await;
            None
        }
    }
}

async fn handle_socket(
    mut socket: WebSocket,
    state: Arc<AppState>,
    id: Uuid,
    legacy_token: Option<String>,
) {
    let Some(resume_from) = authenticate(&mut socket, &state, legacy_token.as_deref()).await else {
        return;
    };

    let session = match state.sessions.get(id) {
        Ok(s) => s,
        Err(_) => {
            close_with(&mut socket, 4404, "no such session").await;
            return;
        }
    };

    let (mut sink, mut stream) = socket.split();

    // Subscribe BEFORE snapshotting so no broadcast chunks are missed in the gap.
    let mut rx = session.subscribe();
    let (snapshot, snap_pos, reset) = session.snapshot_for_attach(resume_from);

    // Send a meta JSON header so clients know the session ID and current pos.
    let meta = ServerControl::SnapshotStart {
        session_id: Some(session.id),
        scrollback_bytes: snapshot.len() as u64,
        scrollback_pos: snap_pos,
        reset,
    };
    if sink
        .send(Message::Text(serde_json::to_string(&meta).unwrap().into()))
        .await
        .is_err()
    {
        return;
    }

    // Stream the scrollback in chunks.
    for chunk in snapshot.chunks(SNAPSHOT_CHUNK) {
        if sink
            .send(Message::Binary(chunk.to_vec().into()))
            .await
            .is_err()
        {
            return;
        }
    }
    if sink
        .send(Message::Text(
            serde_json::to_string(&ServerControl::SnapshotDone)
                .unwrap()
                .into(),
        ))
        .await
        .is_err()
    {
        return;
    }

    let writer_session = Arc::clone(&session);
    // Inbound: client → PTY stdin + control frames.
    let inbound = tokio::spawn(async move {
        while let Some(msg) = stream.next().await {
            let Ok(msg) = msg else { break };
            match msg {
                Message::Binary(data) => {
                    if data.len() > MAX_INPUT_BYTES {
                        continue;
                    }
                    if writer_session.write_input(&data).is_err() {
                        break;
                    }
                }
                Message::Text(s) => {
                    // Try to decode a control frame; fall back to raw text input.
                    match serde_json::from_str::<ClientControl>(&s) {
                        Ok(ClientControl::Resize { cols, rows }) => {
                            let _ = writer_session.resize(cols, rows);
                        }
                        Ok(ClientControl::Ping) => {}
                        Ok(ClientControl::Auth { .. }) => {
                            // Already authenticated; ignore further auth frames.
                        }
                        Err(_) => {
                            // Plain-text input (some tools send text frames).
                            if s.len() > MAX_INPUT_BYTES {
                                continue;
                            }
                            if writer_session.write_input(s.as_bytes()).is_err() {
                                break;
                            }
                        }
                    }
                }
                Message::Ping(_) | Message::Pong(_) => {}
                Message::Close(_) => break,
            }
        }
    });

    // Outbound: broadcast chunks → client, skipping anything already in the snapshot.
    let outbound = tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(chunk) => {
                    // Skip wholly-replayed chunks (chunk.pos < snap_pos and chunk
                    // ends before snap_pos). If chunk straddles snap_pos, send
                    // only the tail.
                    let chunk_end = chunk.pos + chunk.data.len() as u64;
                    if chunk_end <= snap_pos {
                        continue;
                    }
                    let send_buf = if chunk.pos >= snap_pos {
                        chunk.data
                    } else {
                        let skip = (snap_pos - chunk.pos) as usize;
                        chunk.data.slice(skip..)
                    };
                    if sink
                        .send(Message::Binary(send_buf.to_vec().into()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                Err(RecvError::Lagged(_n)) => {
                    let lag = ServerControl::Lag {
                        note: "client too slow; reattach".into(),
                    };
                    let _ = sink
                        .send(Message::Text(serde_json::to_string(&lag).unwrap().into()))
                        .await;
                    break;
                }
                Err(RecvError::Closed) => break,
            }
        }
    });

    // First one to finish terminates the other.
    tokio::select! {
        _ = inbound => {}
        _ = outbound => {}
    }
}
