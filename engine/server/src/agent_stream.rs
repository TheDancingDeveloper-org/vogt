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
    io::Read,
    sync::{atomic::AtomicBool, atomic::Ordering, Arc, Condvar},
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;
use tokio::sync::{mpsc, Mutex, Notify};
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
    links: std::sync::Mutex<HashMap<Uuid, mpsc::UnboundedSender<Frame>>>,
    pods: std::sync::Mutex<HashMap<Uuid, Arc<PodLink>>>,
}

/// The engine's half of one pod session's agent stream. It outlives any one
/// WebSocket: the agent may drop and redial, and this keeps the byte cursor
/// (so a replay is deduplicated exactly), the output channel the session's
/// reader thread drains, and the exit status.
#[derive(Debug)]
pub struct PodLink {
    /// Absolute offset of the next output byte the engine has not yet taken.
    next_offset: std::sync::Mutex<u64>,
    out: std::sync::Mutex<Option<std::sync::mpsc::Sender<Vec<u8>>>>,
    exit: std::sync::Mutex<Option<i32>>,
    exited: Condvar,
    exit_notify: Notify,
    stop_requested: AtomicBool,
    created: Instant,
}

/// Output the agent sent that does not continue the stream.
#[derive(Debug, PartialEq, Eq)]
pub struct OutputGap {
    pub expected: u64,
    pub got: u64,
}

impl PodLink {
    fn new(out: std::sync::mpsc::Sender<Vec<u8>>) -> Self {
        Self {
            next_offset: std::sync::Mutex::new(0),
            out: std::sync::Mutex::new(Some(out)),
            exit: std::sync::Mutex::new(None),
            exited: Condvar::new(),
            exit_notify: Notify::new(),
            stop_requested: AtomicBool::new(false),
            created: Instant::now(),
        }
    }

    /// How long ago the engine registered this link (the pod was asked for).
    pub fn age(&self) -> Duration {
        self.created.elapsed()
    }

    pub fn next_offset(&self) -> u64 {
        *self.next_offset.lock().expect("pod link lock poisoned")
    }

    /// Take the part of `data` (which starts at absolute `offset`) the engine
    /// has not already seen, and return the new cursor. A reconnecting agent
    /// replays from its last ack, so the overlap is dropped here; data that
    /// starts beyond the cursor would leave a hole and is refused.
    pub fn ingest(&self, offset: u64, data: &[u8]) -> Result<u64, OutputGap> {
        let mut next = self.next_offset.lock().expect("pod link lock poisoned");
        if offset > *next {
            return Err(OutputGap {
                expected: *next,
                got: offset,
            });
        }
        let skip = ((*next - offset) as usize).min(data.len());
        let fresh = &data[skip..];
        if !fresh.is_empty() {
            if let Some(out) = self.out.lock().expect("pod link lock poisoned").as_ref() {
                let _ = out.send(fresh.to_vec());
            }
            *next += fresh.len() as u64;
        }
        Ok(*next)
    }

    /// Record the exit and end the output stream, so the reader thread sees
    /// EOF and the exit waiter returns.
    pub fn finish(&self, code: i32) {
        let mut exit = self.exit.lock().expect("pod link lock poisoned");
        if exit.is_none() {
            *exit = Some(code);
        }
        self.out.lock().expect("pod link lock poisoned").take();
        self.exited.notify_all();
        self.exit_notify.notify_waiters();
    }

    pub fn is_finished(&self) -> bool {
        self.exit.lock().expect("pod link lock poisoned").is_some()
    }

    /// Block until the agent has reported the child's exit.
    pub fn wait_exit(&self) -> i32 {
        let mut exit = self.exit.lock().expect("pod link lock poisoned");
        loop {
            if let Some(code) = *exit {
                return code;
            }
            exit = self.exited.wait(exit).expect("pod link lock poisoned");
        }
    }

    /// Resolve once the exit has been recorded.
    pub async fn exited(&self) {
        loop {
            let notified = self.exit_notify.notified();
            if self.is_finished() {
                return;
            }
            notified.await;
        }
    }

    pub fn request_stop(&self) {
        self.stop_requested.store(true, Ordering::Release);
    }

    fn stop_requested(&self) -> bool {
        self.stop_requested.load(Ordering::Acquire)
    }
}

/// The session reader thread's view of the agent's output: a blocking `Read`
/// that returns EOF once the link is finished.
pub struct PodReader {
    rx: std::sync::mpsc::Receiver<Vec<u8>>,
    pending: Vec<u8>,
    at: usize,
}

impl Read for PodReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.at >= self.pending.len() {
            match self.rx.recv() {
                Ok(chunk) => {
                    self.pending = chunk;
                    self.at = 0;
                }
                Err(_) => return Ok(0),
            }
        }
        let n = buf.len().min(self.pending.len() - self.at);
        buf[..n].copy_from_slice(&self.pending[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
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
        let Some(entry) = p.get(&id) else {
            return false;
        };
        let valid = bool::from(entry.token.as_slice().ct_eq(candidate));
        if entry.expires < Instant::now() || !valid {
            return false;
        }
        p.remove(&id);
        // Stored in the form it is handed out in (hex), so the stream's
        // bearer comparison sees exactly what the bootstrap response carried.
        let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        self.sessions.lock().await.insert(
            id,
            SessionCredential {
                token: token.into_bytes(),
            },
        );
        true
    }
    async fn attach(&self, id: Uuid, tx: mpsc::UnboundedSender<Frame>) {
        self.links
            .lock()
            .expect("agent links lock poisoned")
            .insert(id, tx);
    }
    /// Drop the link, unless a newer connection has already replaced it.
    async fn detach(&self, id: Uuid, tx: &mpsc::UnboundedSender<Frame>) {
        let mut links = self.links.lock().expect("agent links lock poisoned");
        if links.get(&id).is_some_and(|cur| cur.same_channel(tx)) {
            links.remove(&id);
        }
    }

    /// Create the engine's half of a pod session's stream before the pod
    /// exists, so output the agent sends the moment it dials in has a home.
    pub fn register_pod(&self, id: Uuid) -> (Arc<PodLink>, PodReader) {
        let (tx, rx) = std::sync::mpsc::channel();
        let link = Arc::new(PodLink::new(tx));
        self.pods
            .lock()
            .expect("pod links lock poisoned")
            .insert(id, Arc::clone(&link));
        (
            link,
            PodReader {
                rx,
                pending: Vec::new(),
                at: 0,
            },
        )
    }

    pub fn pod(&self, id: Uuid) -> Option<Arc<PodLink>> {
        self.pods
            .lock()
            .expect("pod links lock poisoned")
            .get(&id)
            .cloned()
    }

    /// Sessions that have a pod link registered, finished or not.
    pub fn pod_ids(&self) -> Vec<Uuid> {
        self.pods
            .lock()
            .expect("pod links lock poisoned")
            .keys()
            .copied()
            .collect()
    }

    pub fn forget_pod(&self, id: Uuid) {
        self.pods
            .lock()
            .expect("pod links lock poisoned")
            .remove(&id);
        self.links
            .lock()
            .expect("agent links lock poisoned")
            .remove(&id);
        if let Ok(mut sessions) = self.sessions.try_lock() {
            sessions.remove(&id);
        }
    }
    pub fn send(&self, id: Uuid, frame: Frame) -> std::io::Result<()> {
        let result = self
            .links
            .lock()
            .expect("agent links lock poisoned")
            .get(&id)
            .map(|tx| tx.send(frame))
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotConnected,
                    "session-agent is not attached",
                )
            })?;
        result
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "agent stream closed"))
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
            .map(|c| String::from_utf8_lossy(&c.token).into_owned())
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
    req: Result<axum::Json<BootstrapRequest>, axum::extract::rejection::JsonRejection>,
) -> impl IntoResponse {
    let axum::Json(req) = match req {
        Ok(req) => req,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                format!(
                    "bootstrap needs a JSON body {{session_id, graft_repo, graft_line, cli}} \
                     with Content-Type: application/json: {}",
                    e.body_text()
                ),
            )
                .into_response()
        }
    };
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
    let engine = match state.sessions.pod_config() {
        Ok(cfg) => cfg.engine_endpoint,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    // The same contract fields the pod's spec.env carries (pod_backend::pod_env),
    // as strings, so the agent can take either source.
    let mut env = serde_json::Map::new();
    for (name, value) in [
        ("VOGT_ENGINE_URL", engine),
        ("VOGT_SESSION_ID", id.to_string()),
        ("VOGT_GRAFT_REPO", req.graft_repo.clone()),
        ("VOGT_GRAFT_LINE", req.graft_line.clone()),
        ("VOGT_CLI", req.cli.clone()),
    ] {
        env.insert(name.into(), serde_json::Value::String(value));
    }
    if let Some(path) = req.extra_ca_file.as_ref() {
        env.insert(
            "VOGT_EXTRA_CA_FILE".into(),
            serde_json::Value::String(path.clone()),
        );
    }
    axum::Json(BootstrapResponse {
        session_token,
        // Account bundle files (path -> base64) belong to the broker store; the
        // entrypoint writes every entry as a file, so a placeholder would be
        // written as a credential.
        bundle: serde_json::json!({}),
        env: serde_json::Value::Object(env),
        brief: state
            .sessions
            .summary_of(id)
            .map(|s| s.name)
            .unwrap_or_default(),
    })
    .into_response()
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
async fn send_frame(socket: &mut axum::extract::ws::WebSocket, frame: &Frame) -> bool {
    let text = serde_json::to_string(frame).expect("frames serialize");
    socket
        .send(axum::extract::ws::Message::Text(text.into()))
        .await
        .is_ok()
}
async fn relay(mut socket: axum::extract::ws::WebSocket, state: Arc<AppState>, id: Uuid) {
    let Some(link) = state.agent_auth.pod(id) else {
        let _ = socket.close().await;
        return;
    };
    let hello = match socket.next().await {
        Some(Ok(axum::extract::ws::Message::Text(t))) => serde_json::from_str::<Frame>(&t).ok(),
        _ => None,
    };
    if !hello
        .as_ref()
        .is_some_and(|f| crate::pod_backend::validate_agent_hello(f, id).is_ok())
    {
        let _ = socket.close().await;
        return;
    }
    let (tx, mut commands) = mpsc::unbounded_channel();
    state.agent_auth.attach(id, tx.clone()).await;
    // A stop asked for while no agent was attached is delivered on arrival.
    if link.stop_requested() {
        let _ = tx.send(Frame::Signal {
            signal: "kill".into(),
        });
    }
    loop {
        tokio::select! {
            Some(frame) = commands.recv() => {
                if !send_frame(&mut socket, &frame).await { break; }
            }
            msg = socket.next() => match msg {
                Some(Ok(axum::extract::ws::Message::Text(t))) => if let Ok(frame) = serde_json::from_str::<Frame>(&t) {
                    match frame {
                        Frame::Resume { cursor } => {
                            // The agent resumes from what it believes the
                            // engine holds; a cursor past the engine's own
                            // would mean bytes the engine never saw.
                            let next = link.next_offset();
                            if crate::pod_backend::validate_resume_cursor(cursor, next).is_err() {
                                let _ = send_frame(&mut socket, &Frame::Error {
                                    code: "resume_beyond_stream".into(),
                                    message: format!("resume cursor {cursor} is beyond engine offset {next}"),
                                }).await;
                                break;
                            }
                            // Tell the agent how much it can discard.
                            if !send_frame(&mut socket, &Frame::Ack { offset: next }).await { break; }
                        }
                        Frame::Output { offset, data } => match link.ingest(offset, &data) {
                            Ok(next) => {
                                if !send_frame(&mut socket, &Frame::Ack { offset: next }).await { break; }
                            }
                            Err(gap) => {
                                let _ = send_frame(&mut socket, &Frame::Error {
                                    code: "output_gap".into(),
                                    message: format!("expected offset {}, got {}", gap.expected, gap.got),
                                }).await;
                            }
                        },
                        Frame::Heartbeat { .. } => {
                            let next = link.next_offset();
                            if !send_frame(&mut socket, &Frame::Ack { offset: next }).await { break; }
                        }
                        Frame::Exited { status } => {
                            link.finish(status.unwrap_or(-1));
                            break;
                        }
                        _ => {}
                    }
                },
                Some(Ok(axum::extract::ws::Message::Close(_))) | Some(Err(_)) | None => break,
                _ => {}
            }
        }
    }
    state.agent_auth.detach(id, &tx).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bootstrap_is_single_use_and_rejects_bad_tokens() {
        let auth = AgentAuth::default();
        let id = Uuid::new_v4();
        auth.issue(id, b"one-time").await;
        assert!(!auth.consume(id, b"wrong").await);
        assert!(auth.consume(id, b"one-time").await);
        assert!(!auth.consume(id, b"one-time").await);
        assert!(auth.session_token(id).await.is_some());
    }

    #[tokio::test]
    async fn commands_reach_the_connected_agent() {
        let auth = AgentAuth::default();
        let id = Uuid::new_v4();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let tx2 = tx.clone();
        auth.attach(id, tx).await;
        auth.send(
            id,
            Frame::Resize {
                cols: 120,
                rows: 40,
            },
        )
        .unwrap();
        assert_eq!(
            rx.recv().await,
            Some(Frame::Resize {
                cols: 120,
                rows: 40
            })
        );
        auth.detach(id, &tx2).await;
        assert!(auth.send(id, Frame::Ack { offset: 1 }).is_err());
    }

    #[tokio::test]
    async fn an_old_connections_cleanup_leaves_the_new_one_attached() {
        let auth = AgentAuth::default();
        let id = Uuid::new_v4();
        let (old, _old_rx) = mpsc::unbounded_channel();
        let (new, mut new_rx) = mpsc::unbounded_channel();
        auth.attach(id, old.clone()).await;
        auth.attach(id, new).await;
        auth.detach(id, &old).await;
        auth.send(id, Frame::Ack { offset: 3 }).unwrap();
        assert_eq!(new_rx.recv().await, Some(Frame::Ack { offset: 3 }));
    }

    #[test]
    fn ingest_drops_replayed_bytes_and_refuses_gaps() {
        let (tx, rx) = std::sync::mpsc::channel();
        let link = PodLink::new(tx);
        assert_eq!(link.ingest(0, b"abcd"), Ok(4));
        assert_eq!(link.ingest(2, b"cdef"), Ok(6));
        assert_eq!(link.ingest(0, b"ab"), Ok(6));
        assert_eq!(
            link.ingest(9, b"x"),
            Err(OutputGap {
                expected: 6,
                got: 9
            })
        );
        link.finish(3);
        let mut reader = PodReader {
            rx,
            pending: Vec::new(),
            at: 0,
        };
        let mut all = Vec::new();
        reader.read_to_end(&mut all).unwrap();
        assert_eq!(all, b"abcdef");
        assert_eq!(link.wait_exit(), 3);
    }
}
