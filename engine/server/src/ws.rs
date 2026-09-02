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
use tokio::sync::{
    broadcast::error::{RecvError, TryRecvError},
    mpsc,
};
use uuid::Uuid;
use vogt_engine_contract::{ClientControl, ServerControl};

use crate::{app::AppState, auth, pty::OutputChunk, pty::Session};

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

/// Upper bound on the bytes coalesced into a single outbound frame. Bursty
/// output (agent token streams, TUI redraws, a build) arrives as many small
/// broadcast chunks; batching the ones already queued into one WebSocket frame
/// means a chatty session wakes the client's single message-draining thread far
/// fewer times, which is what starves the other panes' sockets under load
/// (#466). Bounded so one burst cannot build an unbounded frame.
const OUTBOUND_COALESCE_CAP: usize = 256 * 1024;

/// How many in-band lag resyncs to attempt, with no normal live send in
/// between, before giving up and asking the client for a clean reattach. A
/// client that keeps falling behind every resync would otherwise drive
/// unbounded resnapshotting; the counter resets whenever live output flows
/// again (#466).
const MAX_CONSECUTIVE_RESYNCS: u32 = 5;

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

/// Coalesce a run of consecutive broadcast chunks into the bytes to forward.
///
/// `snap_pos` is the current dedup boundary: any part of a chunk at or before
/// it was already delivered (in the initial snapshot or a resync) and is
/// dropped via [`live_skip`]. Returns the concatenated not-yet-seen bytes and
/// the absolute position after the last chunk that contributed output — the
/// caller's new "sent up to here" cursor. When nothing new is forwarded the
/// returned position is `snap_pos`, so the caller can keep its existing cursor.
fn coalesce(snap_pos: u64, chunks: &[OutputChunk]) -> (Vec<u8>, u64) {
    let mut out = Vec::new();
    let mut end = snap_pos;
    for chunk in chunks {
        if let Some(skip) = live_skip(chunk.pos, chunk.data.len(), snap_pos) {
            out.extend_from_slice(&chunk.data[skip..]);
            end = chunk.pos + chunk.data.len() as u64;
        }
    }
    (out, end)
}

/// Re-synchronise a lagging client in-band, on the same socket, instead of
/// dropping it and forcing a reconnect + replay (the cascade in #466).
///
/// Snapshots from `from_pos` (the client's last delivered position) and streams
/// it as the same `SnapshotStart` → payload → `SnapshotDone` sequence a fresh
/// attach uses. When the cursor is still inside the scrollback window the server
/// sends a `reset: false` delta (the client appends it); when it has aged out it
/// sends a `reset: true` full snapshot (the client clears and reloads). Either
/// way the socket stays open. Returns the new absolute position the client is
/// synchronised to, or `Err(())` if the socket died mid-send.
async fn send_resync<S>(sink: &mut S, session: &Session, from_pos: u64) -> Result<u64, ()>
where
    S: SinkExt<Message> + Unpin,
{
    // A resync always carries the client's cursor, so this is the warm/delta
    // path; the cold-attach tail cap never applies here.
    let (payload, pos, reset) = session.snapshot_for_attach(Some(from_pos), None);
    let meta = ServerControl::SnapshotStart {
        session_id: Some(session.id),
        scrollback_bytes: payload.len() as u64,
        scrollback_pos: pos,
        reset,
    };
    sink.send(Message::Text(serde_json::to_string(&meta).unwrap().into()))
        .await
        .map_err(|_| ())?;
    // Zero-copy frames: `payload` is `Bytes`, so `slice` shares the buffer
    // rather than allocating and copying each 64 KiB frame (#533.3).
    let mut offset = 0;
    while offset < payload.len() {
        let end = (offset + SNAPSHOT_CHUNK).min(payload.len());
        sink.send(Message::Binary(payload.slice(offset..end)))
            .await
            .map_err(|_| ())?;
        offset = end;
    }
    sink.send(Message::Text(
        serde_json::to_string(&ServerControl::SnapshotDone)
            .unwrap()
            .into(),
    ))
    .await
    .map_err(|_| ())?;
    Ok(pos)
}

/// Outcome of an in-band lag recovery attempt.
enum Recovery {
    /// Client re-synchronised to this absolute position on the same socket.
    Resynced(u64),
    /// Too many resyncs without progress — fall back to a clean reattach.
    GiveUp,
}

/// Attempt an in-band resync, tripping the circuit breaker after too many in a
/// row. Increments `resyncs`; the caller resets it whenever live output flows.
async fn recover_from_lag<S>(
    sink: &mut S,
    session: &Session,
    from_pos: u64,
    resyncs: &mut u32,
) -> Recovery
where
    S: SinkExt<Message> + Unpin,
{
    *resyncs += 1;
    if *resyncs > MAX_CONSECUTIVE_RESYNCS {
        let lag = ServerControl::Lag {
            note: "client too slow; reattach".into(),
        };
        let _ = sink
            .send(Message::Text(serde_json::to_string(&lag).unwrap().into()))
            .await;
        return Recovery::GiveUp;
    }
    match send_resync(sink, session, from_pos).await {
        Ok(pos) => Recovery::Resynced(pos),
        Err(()) => Recovery::GiveUp,
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

/// What a successful attach handshake tells the snapshot: where to resume from
/// (a warm reattach), and the cold-attach tail cap (#474).
struct AttachAuth {
    /// Absolute position the client has already rendered, if any. Present ->
    /// warm reattach (delta or full-reset resume); absent -> cold attach.
    resume_from: Option<u64>,
    /// Cold-attach only: bound the full snapshot to at most this many trailing
    /// bytes. Ignored when `resume_from` is present.
    snapshot_tail_bytes: Option<u64>,
}

/// Read the first frame after upgrade. Must be an `auth` control frame OR the
/// legacy `?token=` query param must have been correct. Returns `Some(_)` on
/// success, `None` on failure (after sending a close frame).
async fn authenticate(
    socket: &mut WebSocket,
    state: &Arc<AppState>,
    legacy_token: Option<&str>,
) -> Option<AttachAuth> {
    if let Some(tok) = legacy_token {
        if token_ok(state, tok) {
            return Some(AttachAuth {
                resume_from: None,
                snapshot_tail_bytes: None,
            });
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
        ClientControl::Auth {
            token,
            resume_from,
            snapshot_tail_bytes,
        } if token_ok(state, &token) => Some(AttachAuth {
            resume_from,
            snapshot_tail_bytes,
        }),
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
    let Some(auth) = authenticate(&mut socket, &state, legacy_token.as_deref()).await else {
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
    let (snapshot, snap_pos, reset) = session.snapshot_for_attach(
        auth.resume_from,
        auth.snapshot_tail_bytes.map(|b| b as usize),
    );

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

    // Stream the scrollback in zero-copy `Bytes` slices (#533.3): `snapshot`
    // is `Bytes`, so each frame shares the buffer instead of copying 64 KiB.
    let mut offset = 0;
    while offset < snapshot.len() {
        let end = (offset + SNAPSHOT_CHUNK).min(snapshot.len());
        if sink
            .send(Message::Binary(snapshot.slice(offset..end)))
            .await
            .is_err()
        {
            return;
        }
        offset = end;
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

    // Outbound: broadcast chunks → client. Bursts are coalesced into fewer
    // frames, and a lagging client is recovered in-band rather than dropped
    // (#466).
    let outbound_session = Arc::clone(&session);
    let outbound = tokio::spawn(async move {
        // `snap_pos` is the dedup boundary handed to `live_skip`; `sent_pos` is
        // the absolute offset the client has been streamed up to. Both jump
        // forward after an in-band resync.
        let mut snap_pos = snap_pos;
        let mut sent_pos = snap_pos;
        // Consecutive resyncs with no normal live send in between. Reset on any
        // live output; a client that trips the ceiling is handed back to a
        // clean reattach instead of driving unbounded resnapshotting.
        let mut resyncs: u32 = 0;

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
                result = rx.recv() => {
                    let first = match result {
                        Ok(chunk) => chunk,
                        // Lagged straight from the blocking recv: recover in-band.
                        Err(RecvError::Lagged(_)) => {
                            match recover_from_lag(
                                &mut sink,
                                &outbound_session,
                                sent_pos,
                                &mut resyncs,
                            )
                            .await
                            {
                                Recovery::Resynced(pos) => {
                                    snap_pos = pos;
                                    sent_pos = pos;
                                    continue;
                                }
                                Recovery::GiveUp => break,
                            }
                        }
                        Err(RecvError::Closed) => break,
                    };

                    // Drain everything already queued so a burst becomes one
                    // frame, not dozens — each frame is a client main-thread
                    // wakeup. Bounded so one session can't build a huge frame.
                    let mut drained = vec![first];
                    let mut queued = drained[0].data.len();
                    let mut lagged = false;
                    let mut closed = false;
                    while queued < OUTBOUND_COALESCE_CAP {
                        match rx.try_recv() {
                            Ok(next) => {
                                queued += next.data.len();
                                drained.push(next);
                            }
                            Err(TryRecvError::Empty) => break,
                            Err(TryRecvError::Lagged(_)) => {
                                lagged = true;
                                break;
                            }
                            Err(TryRecvError::Closed) => {
                                closed = true;
                                break;
                            }
                        }
                    }

                    let (frame, end) = coalesce(snap_pos, &drained);
                    if end > sent_pos {
                        sent_pos = end;
                    }
                    if !frame.is_empty() {
                        if sink.send(Message::Binary(frame.into())).await.is_err() {
                            break;
                        }
                        // Live output flowed: the client is keeping up again.
                        resyncs = 0;
                    }

                    if closed {
                        break;
                    }
                    if lagged {
                        match recover_from_lag(
                            &mut sink,
                            &outbound_session,
                            sent_pos,
                            &mut resyncs,
                        )
                        .await
                        {
                            Recovery::Resynced(pos) => {
                                snap_pos = pos;
                                sent_pos = pos;
                            }
                            Recovery::GiveUp => break,
                        }
                    }
                }
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
    use super::{coalesce, live_skip};
    use crate::pty::OutputChunk;
    use bytes::Bytes;

    fn chunk(pos: u64, data: &'static [u8]) -> OutputChunk {
        OutputChunk {
            pos,
            data: Bytes::from_static(data),
        }
    }

    #[test]
    fn coalesce_concatenates_consecutive_new_chunks() {
        // A burst of small chunks all past the boundary becomes one buffer, and
        // the returned cursor is the end of the last chunk.
        let chunks = [chunk(10, b"abc"), chunk(13, b"de"), chunk(15, b"f")];
        let (out, end) = coalesce(10, &chunks);
        assert_eq!(&out, b"abcdef");
        assert_eq!(end, 16);
    }

    #[test]
    fn coalesce_drops_chunks_already_below_the_boundary() {
        // The first chunk is fully within an earlier snapshot (pos+len <= 10);
        // only the not-yet-seen chunks survive, and the cursor tracks them.
        let chunks = [chunk(4, b"OLD"), chunk(10, b"new")];
        let (out, end) = coalesce(10, &chunks);
        assert_eq!(&out, b"new");
        assert_eq!(end, 13);
    }

    #[test]
    fn coalesce_trims_a_chunk_straddling_the_boundary() {
        // Chunk [8,13) with boundary 10: only bytes [10,13) are new.
        let chunks = [chunk(8, b"XXabc")];
        let (out, end) = coalesce(10, &chunks);
        assert_eq!(&out, b"abc");
        assert_eq!(end, 13);
    }

    #[test]
    fn coalesce_forwards_nothing_and_keeps_the_cursor_when_all_seen() {
        // Everything is at or before the boundary: no bytes, cursor unchanged so
        // the caller keeps its existing sent position.
        let chunks = [chunk(0, b"seen"), chunk(4, b"more")];
        let (out, end) = coalesce(8, &chunks);
        assert!(out.is_empty());
        assert_eq!(end, 8);
    }

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
