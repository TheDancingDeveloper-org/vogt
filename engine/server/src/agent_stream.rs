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
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Condvar,
    },
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

/// How a requested write-back ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlushOutcome {
    /// The agent stored the transcript.
    Done,
    /// No agent is attached, so there is nothing newer to pull.
    NotAttached,
    /// The agent is attached but did not confirm in time, or could not store.
    Failed,
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
    /// Write-backs the agent has confirmed (`Flushed { ok: true }`).
    flushes: AtomicU64,
    /// Write-backs the agent reported it could not complete.
    flush_failures: AtomicU64,
    flush_notify: Notify,
    /// The pod is gone and the stream forgotten; hibernation waits for it so
    /// a wake never races the old pod's teardown.
    torn_down: AtomicBool,
    torn_notify: Notify,
    /// An agent has completed its hello on this link.
    connected: AtomicBool,
    /// The link was rebuilt after an engine restart rather than registered by
    /// a launch: its cursor came from the output log, and until an agent
    /// dials in the pod's fate is unknown.
    adopted: AtomicBool,
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
            flushes: AtomicU64::new(0),
            flush_failures: AtomicU64::new(0),
            flush_notify: Notify::new(),
            torn_down: AtomicBool::new(false),
            torn_notify: Notify::new(),
            connected: AtomicBool::new(false),
            adopted: AtomicBool::new(false),
        }
    }

    /// The pod behind this link has been deleted and the stream forgotten.
    pub fn mark_torn_down(&self) {
        self.torn_down.store(true, Ordering::Release);
        self.torn_notify.notify_waiters();
    }

    /// Resolve once [`Self::mark_torn_down`] ran, or after `limit`. Returns
    /// whether the teardown finished.
    pub async fn torn_down(&self, limit: Duration) -> bool {
        let wait = async {
            loop {
                let notified = self.torn_notify.notified();
                if self.torn_down.load(Ordering::Acquire) {
                    return;
                }
                notified.await;
            }
        };
        tokio::time::timeout(limit, wait).await.is_ok()
    }

    /// An agent has dialled in on this link at least once.
    pub fn has_connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }

    /// Rebuilt after an engine restart (see `AgentAuth::adopt_pod`).
    pub fn is_adopted(&self) -> bool {
        self.adopted.load(Ordering::Acquire)
    }

    /// Move the cursor forward over output the engine acknowledged before a
    /// crash but never logged. Only an adopted link does this, and only
    /// forward: the bytes are gone, and refusing the agent would strand it.
    fn skip_to(&self, cursor: u64) {
        let mut next = self.next_offset.lock().expect("pod link lock poisoned");
        if cursor > *next {
            *next = cursor;
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

    /// The agent trimmed `lost` bytes from `offset` before the engine had them:
    /// write a marker in their place and continue after them. Returns the new
    /// cursor, or the hole that is still open when the loss starts beyond the
    /// engine's stream. A marker for bytes the engine already holds changes
    /// nothing; one that overlaps them only counts the bytes it really lost.
    pub fn ingest_gap(&self, offset: u64, lost: u64) -> Result<u64, OutputGap> {
        let mut next = self.next_offset.lock().expect("pod link lock poisoned");
        if offset > *next {
            return Err(OutputGap {
                expected: *next,
                got: offset,
            });
        }
        let end = offset.saturating_add(lost);
        if end > *next {
            let missing = end - *next;
            if let Some(out) = self.out.lock().expect("pod link lock poisoned").as_ref() {
                let _ = out.send(format!("\r\n[{missing} bytes lost]\r\n").into_bytes());
            }
            *next = end;
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
    /// Graft credential for the session's repo and line, delivered to the
    /// entrypoint as a 0600 file rather than an environment value. Absent
    /// when the engine has none configured.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub graft_token: Option<String>,
}

/// The contract env the bootstrap response carries (the same fields as the
/// pod's spec.env), as strings. Graft's endpoint is engine config.
fn bootstrap_env(
    engine: String,
    graft_endpoint: Option<String>,
    id: Uuid,
    req: &BootstrapRequest,
) -> serde_json::Map<String, serde_json::Value> {
    let mut env = serde_json::Map::new();
    for (name, value) in [
        ("VOGT_ENGINE_URL", engine),
        ("VOGT_SESSION_ID", id.to_string()),
        ("VOGT_GRAFT_REPO", req.graft_repo.clone()),
        ("VOGT_GRAFT_LINE", req.graft_line.clone()),
        ("GRAFT_LINE", req.graft_line.clone()),
        ("VOGT_CLI", req.cli.clone()),
    ] {
        env.insert(name.into(), serde_json::Value::String(value));
    }
    if let Some(endpoint) = graft_endpoint {
        env.insert("GRAFT_ENDPOINT".into(), serde_json::Value::String(endpoint));
    }
    if let Some(path) = req.extra_ca_file.as_ref() {
        env.insert(
            "VOGT_EXTRA_CA_FILE".into(),
            serde_json::Value::String(path.clone()),
        );
    }
    env
}

/// Read the configured graft token file. The path is engine config (a mounted
/// secret); the token is never logged or stored.
async fn read_graft_token(path: Option<&str>) -> std::io::Result<Option<String>> {
    let Some(path) = path else { return Ok(None) };
    let text = tokio::fs::read_to_string(path).await?;
    let token = text.trim();
    Ok((!token.is_empty()).then(|| token.to_owned()))
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
    /// Withdraw a bootstrap credential no pod will present, because the pod
    /// could not be created. It would expire on its own after
    /// `BOOTSTRAP_TTL`; this closes that window.
    pub async fn withdraw(&self, id: Uuid) {
        self.pending.lock().await.remove(&id);
    }
    pub(crate) async fn consume(&self, id: Uuid, candidate: &[u8]) -> bool {
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

    /// Rebuild the engine's half of a pod session's stream after the engine
    /// restarted. `cursor` is how much output the engine already holds, so the
    /// agent's resume replays exactly the rest; `session_token` is the
    /// credential the pod already holds. The link counts as unconnected until
    /// an agent dials in.
    pub async fn adopt_pod(
        &self,
        id: Uuid,
        cursor: u64,
        session_token: String,
    ) -> (Arc<PodLink>, PodReader) {
        let (link, reader) = self.register_pod(id);
        *link.next_offset.lock().expect("pod link lock poisoned") = cursor;
        link.adopted.store(true, Ordering::Release);
        self.sessions.lock().await.insert(
            id,
            SessionCredential {
                token: session_token.into_bytes(),
            },
        );
        (link, reader)
    }

    /// Ask the agent to write its bundle back and wait for it to say so.
    pub async fn request_flush(&self, id: Uuid, limit: Duration) -> FlushOutcome {
        let Some(link) = self.pod(id) else {
            return FlushOutcome::NotAttached;
        };
        let before = link.flushes.load(Ordering::Acquire);
        let failed_before = link.flush_failures.load(Ordering::Acquire);
        if self.send(id, Frame::Flush).is_err() {
            return FlushOutcome::NotAttached;
        }
        let wait = async {
            loop {
                let notified = link.flush_notify.notified();
                if link.flushes.load(Ordering::Acquire) > before {
                    return FlushOutcome::Done;
                }
                if link.flush_failures.load(Ordering::Acquire) > failed_before {
                    return FlushOutcome::Failed;
                }
                notified.await;
            }
        };
        tokio::time::timeout(limit, wait)
            .await
            .unwrap_or(FlushOutcome::Failed)
    }

    /// Forget the stream, unless a newer registration has replaced `link`.
    pub fn forget_pod_if(&self, id: Uuid, link: &Arc<PodLink>) {
        let current = self.pod(id);
        if current.is_some_and(|c| Arc::ptr_eq(&c, link)) {
            self.forget_pod(id);
        }
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
    pub async fn session_token(&self, id: Uuid) -> Option<String> {
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
    let cfg = match state.sessions.pod_config() {
        Ok(cfg) => cfg,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let graft_token = match read_graft_token(cfg.graft_token_file.as_deref()).await {
        Ok(token) => token,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "graft token file is configured but unreadable",
            )
                .into_response()
        }
    };
    let mut env = bootstrap_env(cfg.engine_endpoint, cfg.graft_endpoint, id, &req);
    // A pod resumes from the account bundle the engine holds: the login, the
    // global config and, for a woken session, its conversation's transcript.
    let summary = state.sessions.summary_of(id);
    let account = summary.as_ref().and_then(|s| s.account.clone());
    let conversation = summary
        .as_ref()
        .and_then(|s| s.conversation.as_ref())
        .map(|c| c.id.clone());
    let mut bundle = serde_json::Map::new();
    let mut resume = false;
    if let (Some(account), Some(conversation)) = (account.as_deref(), conversation.as_deref()) {
        for (path, bytes) in state
            .sessions
            .secret_broker()
            .bundle_snapshot(account, conversation)
        {
            resume |= path.ends_with(&format!("/{conversation}.jsonl"));
            bundle.insert(
                path,
                serde_json::Value::String(base64::Engine::encode(
                    &base64::engine::general_purpose::STANDARD,
                    bytes,
                )),
            );
        }
    }
    if let Some(account) = account {
        env.insert(
            "VOGT_SESSION_ACCOUNT".into(),
            serde_json::Value::String(account),
        );
    }
    if let Some(conversation) = conversation {
        env.insert(
            "VOGT_SESSION_CONVERSATION_ID".into(),
            serde_json::Value::String(conversation),
        );
    }
    if resume {
        env.insert(
            "VOGT_SESSION_RESUME".into(),
            serde_json::Value::String("1".into()),
        );
    }
    // The pod's stream credential outlives this process only if it is kept:
    // a restarted engine adopts the pod by reading it back.
    state.sessions.keep_pod_token(id, &session_token);
    axum::Json(BootstrapResponse {
        session_token,
        bundle: serde_json::Value::Object(bundle),
        env: serde_json::Value::Object(env),
        graft_token,
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
    link.connected.store(true, Ordering::Release);
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
                            let mut next = link.next_offset();
                            if link.is_adopted() && cursor > next {
                                // The engine acknowledged these bytes, then
                                // died before its log held them.
                                tracing::warn!(session = %id, cursor, next, "adopted pod's agent is ahead of the engine's log; skipping the missing output");
                                link.skip_to(cursor);
                                next = link.next_offset();
                            }
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
                                // Say where the stream ends, so the agent can
                                // resend from there or mark what it cannot.
                                if !send_frame(&mut socket, &Frame::Resync { offset: gap.expected }).await { break; }
                            }
                        },
                        Frame::Gap { offset, lost } => match link.ingest_gap(offset, lost) {
                            Ok(next) => {
                                if !send_frame(&mut socket, &Frame::Ack { offset: next }).await { break; }
                            }
                            Err(gap) => {
                                if !send_frame(&mut socket, &Frame::Resync { offset: gap.expected }).await { break; }
                            }
                        },
                        Frame::Flushed { ok } => {
                            let counter = if ok { &link.flushes } else { &link.flush_failures };
                            counter.fetch_add(1, Ordering::AcqRel);
                            link.flush_notify.notify_waiters();
                        }
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

    #[test]
    fn bootstrap_env_carries_the_graft_endpoint_and_line() {
        let req = BootstrapRequest {
            session_id: String::new(),
            graft_repo: "vogt".into(),
            graft_line: "rust-2".into(),
            cli: "claude".into(),
            extra_ca_file: None,
        };
        let env = bootstrap_env(
            "http://engine".into(),
            Some("http://graft-dev.graft-dev.svc:17888".into()),
            Uuid::nil(),
            &req,
        );
        assert_eq!(
            env["GRAFT_ENDPOINT"],
            "http://graft-dev.graft-dev.svc:17888"
        );
        assert_eq!(env["GRAFT_LINE"], "rust-2");
        assert_eq!(env["VOGT_GRAFT_LINE"], "rust-2");
        assert!(!bootstrap_env("e".into(), None, Uuid::nil(), &req).contains_key("GRAFT_ENDPOINT"));
    }

    #[tokio::test]
    async fn graft_token_is_read_from_the_configured_file_only() {
        assert_eq!(read_graft_token(None).await.unwrap(), None);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("graft-token");
        std::fs::write(&path, "tok\n").unwrap();
        assert_eq!(
            read_graft_token(path.to_str()).await.unwrap().as_deref(),
            Some("tok")
        );
        assert!(read_graft_token(dir.path().join("missing").to_str())
            .await
            .is_err());
    }

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
    async fn the_bootstrap_issued_token_authenticates_the_agent_stream() {
        let auth = AgentAuth::default();
        let id = Uuid::new_v4();
        auth.issue(id, b"one-time").await;
        assert!(auth.consume(id, b"one-time").await);
        let issued = auth.session_token(id).await.unwrap();
        assert!(auth.session_ok(id, issued.as_bytes()).await);
        assert!(!auth.session_ok(id, b"one-time").await);
        assert!(!auth.session_ok(Uuid::new_v4(), issued.as_bytes()).await);
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
    fn a_gap_marker_records_the_loss_and_continues_after_it() {
        let (tx, rx) = std::sync::mpsc::channel();
        let link = PodLink::new(tx);
        assert_eq!(link.ingest(0, b"abcd"), Ok(4));
        // Bytes 4..10 are gone; the stream picks up at 10.
        assert_eq!(link.ingest_gap(4, 6), Ok(10));
        assert_eq!(link.ingest(10, b"ef"), Ok(12));
        // A marker for bytes already held changes nothing, and one that
        // overlaps them counts only what was really lost.
        assert_eq!(link.ingest_gap(4, 6), Ok(12));
        assert_eq!(link.ingest_gap(10, 4), Ok(14));
        // A loss starting past the stream leaves a hole still.
        assert_eq!(
            link.ingest_gap(20, 1),
            Err(OutputGap {
                expected: 14,
                got: 20
            })
        );
        link.finish(0);
        let mut reader = PodReader {
            rx,
            pending: Vec::new(),
            at: 0,
        };
        let mut all = Vec::new();
        reader.read_to_end(&mut all).unwrap();
        assert_eq!(
            String::from_utf8(all).unwrap(),
            "abcd\r\n[6 bytes lost]\r\nef\r\n[2 bytes lost]\r\n"
        );
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
