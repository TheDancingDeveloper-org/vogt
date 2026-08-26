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
use tokio::sync::{broadcast::error::RecvError, mpsc};
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

/// De-duplicate a live broadcast chunk against the already-replayed snapshot.
///
/// `chunk_pos` is the absolute byte offset where the chunk begins and
/// `snap_pos` is the exclusive end of the snapshot (`total_written` at the
/// moment the snapshot was taken). Returns:
/// - `None` if the whole chunk is `< snap_pos` (fully in the snapshot: drop it),
/// - `Some(0)` if the chunk begins at or after `snap_pos` (all new: send it all),
/// - `Some(skip)` for a chunk straddling `snap_pos` (send `data[skip..]`).
///
/// The boundary is exclusive on both sides — a chunk ending exactly at
/// `snap_pos` is fully covered, and a chunk beginning exactly at `snap_pos` is
/// fully new — so replay and live stream meet with no gap and no duplicate.
fn live_skip(chunk_pos: u64, chunk_len: usize, snap_pos: u64) -> Option<usize> {
    let chunk_end = chunk_pos + chunk_len as u64;
    if chunk_end <= snap_pos {
        return None;
    }
    if chunk_pos >= snap_pos {
        Some(0)
    } else {
        Some((snap_pos - chunk_pos) as usize)
    }
}

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
    let (control_tx, mut control_rx) = mpsc::unbounded_channel::<ServerControl>();

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
                        Ok(ClientControl::Ping { id }) => {
                            let _ = control_tx.send(ServerControl::Pong {
                                id,
                                pos: writer_session.scrollback_position(),
                            });
                        }
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
            tokio::select! {
                Some(control) = control_rx.recv() => {
                    if sink
                        .send(Message::Text(serde_json::to_string(&control).unwrap().into()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                result = rx.recv() => match result {
                Ok(chunk) => {
                    // Skip anything already delivered in the replayed snapshot;
                    // for a chunk straddling the snapshot boundary, send only
                    // the not-yet-seen tail.
                    let Some(skip) = live_skip(chunk.pos, chunk.data.len(), snap_pos) else {
                        continue;
                    };
                    let send_buf = if skip == 0 {
                        chunk.data
                    } else {
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
                },
            }
        }
    });

    // First one to finish terminates the other.
    tokio::select! {
        _ = inbound => {}
        _ = outbound => {}
    }
}

#[cfg(test)]
mod tests {
    use super::live_skip;

    #[test]
    fn chunk_fully_in_snapshot_is_dropped() {
        // Chunk [0,5) with a snapshot ending at 5: fully covered.
        assert_eq!(live_skip(0, 5, 5), None);
        assert_eq!(live_skip(0, 3, 5), None);
    }

    #[test]
    fn chunk_fully_after_snapshot_is_sent_whole() {
        // Chunk beginning exactly at the boundary is entirely new.
        assert_eq!(live_skip(5, 4, 5), Some(0));
        assert_eq!(live_skip(10, 4, 5), Some(0));
    }

    #[test]
    fn chunk_straddling_snapshot_boundary_skips_the_replayed_head() {
        // Chunk [3,7) with snapshot ending at 5: send only bytes [5,7).
        assert_eq!(live_skip(3, 4, 5), Some(2));
        // One-byte overlap: send all but the first byte.
        assert_eq!(live_skip(4, 3, 5), Some(1));
    }

    #[test]
    fn boundary_is_gapless_and_duplicate_free() {
        // Two adjacent chunks around the boundary reconstruct [snap_pos, end)
        // exactly once: the first is dropped, the second sent whole.
        let snap_pos = 8;
        assert_eq!(live_skip(4, 4, snap_pos), None); // [4,8) fully replayed
        assert_eq!(live_skip(8, 4, snap_pos), Some(0)); // [8,12) all new
    }
}
