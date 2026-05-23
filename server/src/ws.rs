use std::sync::Arc;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, Query, State,
    },
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::broadcast::error::RecvError;
use uuid::Uuid;

use crate::{app::AppState, auth::ws_token_ok};

#[derive(Debug, Deserialize)]
pub struct AttachQuery {
    /// Bearer token for browser clients that can't set Authorization on WS.
    pub token: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
enum ClientControl {
    Resize { cols: u16, rows: u16 },
    Ping,
}

/// Chunk size for streaming the scrollback snapshot back to the client.
/// xterm.js handles big binary frames fine, but keeping each under 64 KiB
/// avoids spikes in browser memory while the buffer parses.
const SNAPSHOT_CHUNK: usize = 64 * 1024;

pub async fn attach(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
    Path(id): Path<Uuid>,
    Query(q): Query<AttachQuery>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !ws_token_ok(&state, &headers, q.token.as_deref()) {
        return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    }
    let session = match state.sessions.get(id) {
        Ok(s) => s,
        Err(_) => return (StatusCode::NOT_FOUND, "no such session").into_response(),
    };
    ws.on_upgrade(move |socket| handle_socket(socket, session))
}

async fn handle_socket(socket: WebSocket, session: Arc<crate::pty::Session>) {
    let (mut sink, mut stream) = socket.split();

    // Subscribe BEFORE snapshotting so no broadcast chunks are missed in the gap.
    let mut rx = session.subscribe();
    let (snapshot, snap_pos) = session.snapshot();

    // Send a meta JSON header so clients know the session ID and current pos.
    let meta = json!({
        "type": "snapshot-start",
        "session_id": session.id,
        "scrollback_bytes": snapshot.len(),
        "scrollback_pos": snap_pos,
    });
    if sink
        .send(Message::Text(meta.to_string().into()))
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
            json!({ "type": "snapshot-done" }).to_string().into(),
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
                        Err(_) => {
                            // Plain-text input (some tools send text frames).
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
                    let _ = sink
                        .send(Message::Text(
                            json!({"type":"lag","note":"client too slow; reattach"})
                                .to_string()
                                .into(),
                        ))
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
