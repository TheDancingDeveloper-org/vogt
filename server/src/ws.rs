use std::{borrow::Cow, sync::Arc, time::Duration};

use axum::{
    extract::{
        ws::{CloseCode, CloseFrame, Message, WebSocket, WebSocketUpgrade},
        Path, Query, State,
    },
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use subtle::ConstantTimeEq;
use tokio::sync::broadcast::error::RecvError;
use uuid::Uuid;

use crate::app::AppState;

#[derive(Debug, Deserialize)]
pub struct AttachQuery {
    /// Legacy: token in query string. Deprecated — left in only so an existing
    /// client that hasn't been redeployed still works. Real auth is via the
    /// first text frame `{"type":"auth","token":"..."}`. Tokens passed here
    /// land in proxy/access logs and shouldn't be relied on for new clients.
    pub token: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
enum ClientControl {
    Resize {
        cols: u16,
        rows: u16,
    },
    Ping,
    /// First-frame auth. Anything else before this is rejected.
    Auth {
        token: String,
    },
}

/// Chunk size for streaming the scrollback snapshot back to the client.
/// xterm.js handles big binary frames fine, but keeping each under 64 KiB
/// avoids spikes in browser memory while the buffer parses.
const SNAPSHOT_CHUNK: usize = 64 * 1024;

/// How long a freshly-upgraded socket has to send `{"type":"auth",...}` before
/// we drop it. Keeps unauth clients from hanging on to a socket indefinitely.
const AUTH_DEADLINE: Duration = Duration::from_secs(5);

const BRACKETED_PASTE_START: &[u8] = b"\x1b[200~";
const BRACKETED_PASTE_END: &[u8] = b"\x1b[201~";

fn strip_bracketed_paste_markers(data: &[u8]) -> Cow<'_, [u8]> {
    let has_marker = data
        .windows(BRACKETED_PASTE_START.len())
        .any(|w| w == BRACKETED_PASTE_START)
        || data
            .windows(BRACKETED_PASTE_END.len())
            .any(|w| w == BRACKETED_PASTE_END);
    if !has_marker {
        return Cow::Borrowed(data);
    }

    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        if data[i..].starts_with(BRACKETED_PASTE_START) {
            i += BRACKETED_PASTE_START.len();
        } else if data[i..].starts_with(BRACKETED_PASTE_END) {
            i += BRACKETED_PASTE_END.len();
        } else {
            out.push(data[i]);
            i += 1;
        }
    }
    Cow::Owned(out)
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

/// Returns true if `candidate` matches the configured server token (constant time).
fn token_ok(state: &AppState, candidate: &str) -> bool {
    bool::from(candidate.as_bytes().ct_eq(state.config.token.as_bytes()))
}

/// Read the first frame after upgrade. Must be an `auth` control frame OR the
/// legacy `?token=` query param must have been correct. Returns Some(()) on
/// success, None on failure (after sending a close frame).
async fn authenticate(
    socket: &mut WebSocket,
    state: &Arc<AppState>,
    legacy_token: Option<&str>,
) -> Option<()> {
    if let Some(tok) = legacy_token {
        if token_ok(state, tok) {
            return Some(());
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
        ClientControl::Auth { token } if token_ok(state, &token) => Some(()),
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
    if authenticate(&mut socket, &state, legacy_token.as_deref())
        .await
        .is_none()
    {
        return;
    }

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
                    let data = strip_bracketed_paste_markers(&data);
                    if writer_session.write_input(data.as_ref()).is_err() {
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
                            let data = strip_bracketed_paste_markers(s.as_bytes());
                            if writer_session.write_input(data.as_ref()).is_err() {
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

#[cfg(test)]
mod tests {
    use super::strip_bracketed_paste_markers;
    use std::borrow::Cow;

    #[test]
    fn leaves_input_without_bracketed_paste_markers_borrowed() {
        let input = b"plain-token-text";
        let stripped = strip_bracketed_paste_markers(input);
        assert!(matches!(stripped, Cow::Borrowed(_)));
        assert_eq!(stripped.as_ref(), input);
    }

    #[test]
    fn strips_wrapped_bracketed_paste_token() {
        let stripped = strip_bracketed_paste_markers(b"\x1b[200~abc123+/=\x1b[201~");
        assert_eq!(stripped.as_ref(), b"abc123+/=");
    }

    #[test]
    fn strips_multiple_bracketed_paste_markers() {
        let stripped = strip_bracketed_paste_markers(b"one\x1b[200~two\x1b[201~three");
        assert_eq!(stripped.as_ref(), b"onetwothree");
    }
}
