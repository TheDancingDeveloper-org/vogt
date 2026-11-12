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
use vogt_engine_contract::source::SourceDescriptor;
use vogt_session_agent::Frame;
const BOOTSTRAP_TTL: Duration = Duration::from_secs(300);
const AGENT_OUTPUT_CHANNEL_CAPACITY: usize = 16;
const AGENT_BUFFER_BYTES: usize = vogt_session_agent::DEFAULT_BUFFER_BYTES;
/// The largest agent WS message: an `Output` frame carrying
/// `AGENT_BUFFER_BYTES` of data, JSON-encoded as a byte array (up to four
/// characters a byte, "255,"), plus room for the envelope. A legitimate replay
/// always fits, so a big replay never loops through refuse-and-reconnect.
const AGENT_WS_MAX_MESSAGE_SIZE: usize = AGENT_BUFFER_BYTES * 4 + 64 * 1024;
/// The most bytes one `Gap` may report lost. A gap carries no data, and a
/// long disconnect legitimately loses far more than the replay buffer holds,
/// so this only keeps the cursor finite and far from overflow (256 TiB).
const AGENT_MAX_GAP_BYTES: u64 = 1 << 48;
static AUTH: OnceLock<Arc<AgentAuth>> = OnceLock::new();
#[derive(Debug, Clone)]
struct Pending {
    token: Vec<u8>,
    expires: Instant,
}
/// What a session spec asked the pod for that only the bootstrap response can
/// carry (WI-3883): the caller's extra environment, which the entrypoint
/// applies only at the agent's final exec, and the work directory inside the
/// clone. Held from pod create until the pod's one bootstrap request, like the
/// credential itself, and never written into the pod spec or into the
/// entrypoint's environment before the clone.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PodLaunch {
    pub agent_env: Vec<(String, String)>,
    /// The pod-side working directory (`VOGT_WORK_DIR`), when the caller's
    /// cwd is not the clone root.
    pub work_dir: Option<String>,
}
#[derive(Debug, Clone)]
struct SessionCredential {
    token: Vec<u8>,
}
#[derive(Debug, Clone)]
struct PendingCredential {
    handle: crate::source_provider::CredentialHandle,
    expires: Instant,
}

type AgentLinks = HashMap<Uuid, (mpsc::UnboundedSender<Frame>, Arc<PodLink>)>;

#[derive(Debug, Default)]
pub struct AgentAuth {
    pending: Mutex<HashMap<Uuid, Pending>>,
    /// The last launch's carried fields, until its bootstrap request takes
    /// them. Withdrawn with the credential, so a pod that never boots leaves
    /// nothing behind.
    launches: Mutex<HashMap<Uuid, PodLaunch>>,
    /// Per-session source credentials minted at pod create, held until the
    /// pod's bootstrap request takes them (or the pod is withdrawn or
    /// forgotten). Handles only: the value stays in the provider's file.
    source_credentials: Mutex<HashMap<Uuid, PendingCredential>>,
    sources: Mutex<HashMap<Uuid, SourceDescriptor>>,
    /// The core session id a minted graft token is bound to, held until the
    /// pod's bootstrap request writes the token under that name.
    graft_sessions: Mutex<HashMap<Uuid, String>>,
    sessions: Mutex<HashMap<Uuid, SessionCredential>>,
    /// The attached agent's command channel, beside the link it belongs to so
    /// attach/detach can update the connection state under one lock.
    links: std::sync::Mutex<AgentLinks>,
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
    out: std::sync::Mutex<Option<std::sync::mpsc::SyncSender<Vec<u8>>>>,
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
    /// An agent's socket is attached right now.
    connected: AtomicBool,
    /// An agent has completed its hello on this link at least once.
    ever_connected: AtomicBool,
    last_heartbeat: std::sync::Mutex<Instant>,
    /// When the current agent's socket was lost, while it is not connected.
    disconnected_at: std::sync::Mutex<Option<Instant>>,
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
    fn new(out: std::sync::mpsc::SyncSender<Vec<u8>>) -> Self {
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
            ever_connected: AtomicBool::new(false),
            last_heartbeat: std::sync::Mutex::new(Instant::now()),
            disconnected_at: std::sync::Mutex::new(None),
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
        self.ever_connected.load(Ordering::Acquire)
    }

    /// An agent's socket is attached right now.
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Acquire)
    }

    pub fn heartbeat_age(&self) -> Duration {
        self.last_heartbeat
            .lock()
            .expect("pod link lock poisoned")
            .elapsed()
    }

    fn note_heartbeat(&self) {
        *self.last_heartbeat.lock().expect("pod link lock poisoned") = Instant::now();
    }

    /// A new agent completed its hello: it is attached and its heartbeat
    /// clock starts now, not when the previous socket went away.
    pub(crate) fn note_connected(&self) {
        *self.disconnected_at.lock().expect("pod link lock poisoned") = None;
        self.ever_connected.store(true, Ordering::Release);
        self.connected.store(true, Ordering::Release);
        self.note_heartbeat();
    }

    /// The attached agent's socket went away. The link stays registered so a
    /// redial resumes it, but the heartbeat deadline no longer applies: the
    /// reconnect timeout owns a disconnect.
    pub(crate) fn note_disconnected(&self) {
        self.connected.store(false, Ordering::Release);
        *self.disconnected_at.lock().expect("pod link lock poisoned") = Some(Instant::now());
    }

    /// How long the link has had no attached agent, if it has none now.
    pub fn disconnected_age(&self) -> Option<Duration> {
        self.disconnected_at
            .lock()
            .expect("pod link lock poisoned")
            .map(|at| at.elapsed())
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
        if data.len() > AGENT_BUFFER_BYTES {
            return Err(OutputGap {
                expected: self.next_offset(),
                got: offset,
            });
        }
        let mut next = self.next_offset.lock().expect("pod link lock poisoned");
        if offset > *next {
            return Err(OutputGap {
                expected: *next,
                got: offset,
            });
        }
        let skip = (*next - offset).min(data.len() as u64) as usize;
        let fresh = &data[skip..];
        if !fresh.is_empty() {
            let end = next.checked_add(fresh.len() as u64).ok_or(OutputGap {
                expected: *next,
                got: offset,
            })?;
            self.enqueue(fresh.to_vec(), *next, offset)?;
            *next = end;
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
        if lost > AGENT_MAX_GAP_BYTES {
            return Err(OutputGap {
                expected: *next,
                got: offset,
            });
        }
        let Some(end) = offset.checked_add(lost) else {
            return Err(OutputGap {
                expected: *next,
                got: offset,
            });
        };
        if end > *next {
            let missing = end - *next;
            self.enqueue(
                format!("\r\n[{missing} bytes lost]\r\n").into_bytes(),
                *next,
                offset,
            )?;
            *next = end;
        }
        Ok(*next)
    }

    fn enqueue(&self, data: Vec<u8>, expected: u64, got: u64) -> Result<(), OutputGap> {
        let out = self.out.lock().expect("pod link lock poisoned");
        let Some(out) = out.as_ref() else {
            return Err(OutputGap { expected, got });
        };
        out.try_send(data).map_err(|_| OutputGap { expected, got })
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

    /// The output stream has ended (see [`Self::finish`]): nothing more can
    /// be taken, however often it is resent.
    fn output_closed(&self) -> bool {
        self.out.lock().expect("pod link lock poisoned").is_none()
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
    #[serde(default)]
    pub source: Option<SourceDescriptor>,
    pub cli: String,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub extra_ca_file: Option<String>,
}
#[derive(Debug, Serialize)]
pub struct BootstrapResponse {
    pub session_token: String,
    pub bundle: serde_json::Value,
    pub env: serde_json::Value,
    /// The caller's extra environment (WI-3883), kept apart from `env` on
    /// purpose: the entrypoint applies it only at the final `exec` of the
    /// agent, after the clone and every step that uses a credential. Absent
    /// when the caller sent none.
    #[serde(skip_serializing_if = "serde_json::Map::is_empty")]
    pub agent_env: serde_json::Map<String, serde_json::Value>,
    /// The project-owned source selected for this session.
    pub source: vogt_engine_contract::source::SourceDescriptor,
    pub brief: String,
    /// Credential for the session's source, from its provider, delivered to
    /// the entrypoint as a 0600 file rather than an environment value. Absent
    /// when the provider has none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_token: Option<String>,
    /// The core session id the source token is bound to, when the core
    /// supplied one: the entrypoint writes the token under that name and
    /// exports `GRAFT_SESSION_ID` so commits attribute to the same session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub graft_session: Option<String>,
    /// The `--mcp-config` file a pod-backed Claude Code session is started
    /// with (WI-3243): the engine's MCP endpoints as Claude Code reads them.
    /// The bearer is an environment reference to `VOGT_HTTP_TOKEN`, which the
    /// response's `env` carries; no credential is in the file. Absent for a
    /// CLI that takes no such file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp_config: Option<serde_json::Value>,
    /// The Codex counterpart (WI-3161): a `config.toml` fragment with the
    /// engine's MCP servers, for the entrypoint to merge into `$CODEX_HOME`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codex_config: Option<String>,
    /// The opencode counterpart (WI-3161): an `opencode.json` body.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opencode_config: Option<serde_json::Value>,
}

/// The names-only agent-auth breadcrumbs a pod's bootstrap env carries
/// (WI-3884): what the deployment declared for sessions and what this session
/// holds. The values are names, never secrets, so the entrypoint can export
/// them into the session's environment before the agent starts.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct AuthBreadcrumbs {
    /// The variables this session holds an approved grant for.
    pub granted: Vec<String>,
    /// Manifest entries declared `ondemand`, plus the Codex provider keys a
    /// pod fetches at launch (WI-3886): fetchable through the broker by name.
    pub ondemand: Vec<String>,
    /// Manifest entries flagged `writable`: storable through the broker.
    pub writable: Vec<String>,
}

impl AuthBreadcrumbs {
    /// Read the live manifest and this session's grants from the broker.
    pub(crate) fn snapshot(broker: &crate::secret_broker::SecretBroker, session: Uuid) -> Self {
        let manifest = broker.manifest();
        Self {
            granted: broker
                .grants_for(session, time::OffsetDateTime::now_utc())
                .into_iter()
                .map(|held| held.grant.var)
                .collect(),
            ondemand: manifest
                .iter()
                .filter(|entry| entry.policy == crate::secret_broker::ManifestPolicy::OnDemand)
                .map(|entry| entry.var.clone())
                .collect(),
            writable: manifest
                .iter()
                .filter(|entry| entry.writable)
                .map(|entry| entry.var.clone())
                .collect(),
        }
    }
}

/// The contract env the bootstrap response carries (the same fields as the
/// pod's spec.env), as strings.
fn bootstrap_env(
    engine: String,
    id: Uuid,
    cli: &str,
    model: Option<&str>,
    source_env: &[(String, String)],
    extra_ca_file: Option<&str>,
    breadcrumbs: &AuthBreadcrumbs,
) -> serde_json::Map<String, serde_json::Value> {
    let mut env = serde_json::Map::new();
    for (name, value) in [
        ("VOGT_ENGINE_URL", engine),
        ("VOGT_SESSION_ID", id.to_string()),
        ("VOGT_CLI", cli.to_owned()),
    ] {
        env.insert(name.into(), serde_json::Value::String(value));
    }
    for (name, value) in source_env {
        env.insert(name.clone(), serde_json::Value::String(value.clone()));
    }
    // The four names-only agent-auth breadcrumbs the host path exports
    // (WI-3884): a pod is brokered too, and the session's tooling can tell
    // what the deployment declared and what this session holds without a
    // value ever entering the bootstrap response.
    for (name, value) in [
        ("AGENT_AUTH_MODE", "brokered".to_owned()),
        ("AGENT_AUTH_GRANTED", breadcrumbs.granted.join(" ")),
        ("AGENT_AUTH_ONDEMAND", breadcrumbs.ondemand.join(" ")),
        ("AGENT_AUTH_WRITABLE", breadcrumbs.writable.join(" ")),
    ] {
        env.insert(name.into(), serde_json::Value::String(value));
    }
    // The session agent builds the pod's Claude line itself (WI-3881), so the
    // model the caller asked for travels through the same contract as the one
    // the pod spec carries; a pod whose spec is missing it still launches on
    // the requested model.
    if let Some(model) = model.map(str::trim).filter(|model| !model.is_empty()) {
        env.insert(
            "VOGT_MODEL".into(),
            serde_json::Value::String(model.to_owned()),
        );
    }
    if let Some(path) = extra_ca_file {
        env.insert(
            "VOGT_EXTRA_CA_FILE".into(),
            serde_json::Value::String(path.to_owned()),
        );
    }
    env
}

/// The credential a pod's bootstrap response carries: the one its provider
/// minted for it at create, else (a provider that mints none) the
/// deployment's shared one. With minting on the shared credential is never
/// handed out, even if a mint went missing: the pod then checks out with its
/// session token and fails loudly, rather than quietly widening its access.
fn resolve_source_token(
    minted: Option<crate::source_provider::CredentialHandle>,
    provider_mints: bool,
    shared: Option<crate::source_provider::CredentialHandle>,
) -> std::io::Result<Option<String>> {
    let handle = match minted {
        Some(handle) => handle,
        None if provider_mints => return Ok(None),
        None => match shared {
            Some(handle) => handle,
            None => return Ok(None),
        },
    };
    crate::source_provider::read_credential(&handle)
}

pub fn global() -> Arc<AgentAuth> {
    Arc::clone(AUTH.get_or_init(|| Arc::new(AgentAuth::default())))
}

impl AgentAuth {
    pub async fn issue_source(&self, id: Uuid, source: SourceDescriptor) {
        self.sources.lock().await.insert(id, source);
    }

    pub async fn take_source(&self, id: Uuid) -> Option<SourceDescriptor> {
        self.sources.lock().await.remove(&id)
    }

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
        self.launches.lock().await.remove(&id);
        self.source_credentials.lock().await.remove(&id);
        self.graft_sessions.lock().await.remove(&id);
    }
    /// Hold the core session id the session's graft token is bound to, for
    /// the bootstrap response that writes the token into the pod.
    pub async fn issue_graft_session(&self, id: Uuid, session_ref: String) {
        self.graft_sessions.lock().await.insert(id, session_ref);
    }
    pub async fn take_graft_session(&self, id: Uuid) -> Option<String> {
        self.graft_sessions.lock().await.remove(&id)
    }
    /// Hold what the session spec asked the pod to carry, for the bootstrap
    /// response to take once the pod presents its credential.
    pub async fn issue_launch(&self, id: Uuid, launch: PodLaunch) {
        self.launches.lock().await.insert(id, launch);
    }
    pub async fn take_launch(&self, id: Uuid) -> Option<PodLaunch> {
        self.launches.lock().await.remove(&id)
    }
    /// Whether a bootstrap credential is held for `id`: issued, and neither
    /// consumed nor withdrawn.
    pub async fn bootstrap_pending(&self, id: Uuid) -> bool {
        self.pending.lock().await.contains_key(&id)
    }
    /// Hold the credential minted for `id`'s pod until its bootstrap request
    /// takes it. It lives as long as the bootstrap credential does; a pod that
    /// never boots leaves nothing behind past `BOOTSTRAP_TTL`.
    pub async fn stash_source_credential(
        &self,
        id: Uuid,
        handle: crate::source_provider::CredentialHandle,
    ) {
        let now = Instant::now();
        let mut held = self.source_credentials.lock().await;
        held.retain(|_, g| g.expires > now);
        held.insert(
            id,
            PendingCredential {
                handle,
                expires: now + BOOTSTRAP_TTL,
            },
        );
    }
    /// Take (once) the credential minted for `id`, if one is held and fresh.
    pub(crate) async fn take_source_credential(
        &self,
        id: Uuid,
    ) -> Option<crate::source_provider::CredentialHandle> {
        let g = self.source_credentials.lock().await.remove(&id)?;
        (g.expires > Instant::now()).then_some(g.handle)
    }
    /// Whether `candidate` is the unspent bootstrap credential for `id` —
    /// the read-only counterpart of [`Self::consume`]. The workspace
    /// snapshot's init container presents the same credential to fetch its
    /// tarball before the session container's one bootstrap request takes it
    /// (WI-4127), so that fetch must not spend it.
    pub async fn bootstrap_ok(&self, id: Uuid, candidate: &[u8]) -> bool {
        let pending = self.pending.lock().await;
        pending.get(&id).is_some_and(|entry| {
            entry.expires > Instant::now() && bool::from(entry.token.as_slice().ct_eq(candidate))
        })
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
    async fn attach(&self, id: Uuid, tx: mpsc::UnboundedSender<Frame>, link: Arc<PodLink>) {
        self.links
            .lock()
            .expect("agent links lock poisoned")
            .insert(id, (tx, Arc::clone(&link)));
        link.note_connected();
    }
    /// Drop the link, unless a newer connection has already replaced it. A
    /// dropped socket marks the link disconnected, so a redial is judged by
    /// the reconnect timeout rather than the heartbeat deadline.
    async fn detach(&self, id: Uuid, tx: &mpsc::UnboundedSender<Frame>) {
        let mut links = self.links.lock().expect("agent links lock poisoned");
        if let Some((_, link)) = links.get(&id).filter(|(cur, _)| cur.same_channel(tx)) {
            let link = Arc::clone(link);
            links.remove(&id);
            link.note_disconnected();
        }
    }

    /// Create the engine's half of a pod session's stream before the pod
    /// exists, so output the agent sends the moment it dials in has a home.
    pub fn register_pod(&self, id: Uuid) -> (Arc<PodLink>, PodReader) {
        let (tx, rx) = std::sync::mpsc::sync_channel(AGENT_OUTPUT_CHANNEL_CAPACITY);
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
        if let Ok(mut held) = self.source_credentials.try_lock() {
            held.remove(&id);
        }
        if let Ok(mut launches) = self.launches.try_lock() {
            launches.remove(&id);
        }
    }
    pub fn send(&self, id: Uuid, frame: Frame) -> std::io::Result<()> {
        let result = self
            .links
            .lock()
            .expect("agent links lock poisoned")
            .get(&id)
            .map(|(tx, _)| tx.send(frame))
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
    /// Which pod session holds this stream credential, if any. Constant-time
    /// per entry, like the other credential checks here.
    pub async fn session_for_token(&self, candidate: &[u8]) -> Option<Uuid> {
        self.sessions
            .lock()
            .await
            .iter()
            .find(|(_, c)| bool::from(c.token.as_slice().ct_eq(candidate)))
            .map(|(id, _)| *id)
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
/// Mark the bundle's `.claude.json` onboarded and the pod's clone root
/// trusted (the workspace's `session/` directory when the pod has a workspace
/// tree, WI-3374). Without
/// `hasCompletedOnboarding` Claude Code runs its first-run flow (theme picker, then "Select login
/// method") and never reads the login it was handed (WI-3334). The account's
/// stored config (the engine's own trust entries, say) often lacks the flag,
/// and a fresh account has no config at all. Only a bundle with a login is
/// touched, and a config that is not a JSON object is passed through as is.
fn with_onboarding_config(
    mut files: Vec<(String, Vec<u8>)>,
    work_dir: &str,
) -> Vec<(String, Vec<u8>)> {
    if !files.iter().any(|(path, _)| path == ".credentials.json") {
        return files;
    }
    let config = match files.iter().position(|(path, _)| path == ".claude.json") {
        Some(at) => &mut files[at].1,
        None => {
            files.push((".claude.json".into(), b"{}".to_vec()));
            &mut files.last_mut().expect("just pushed").1
        }
    };
    let Ok(serde_json::Value::Object(mut map)) = serde_json::from_slice(config) else {
        return files;
    };
    let mut changed = false;
    if map.get("hasCompletedOnboarding") != Some(&serde_json::Value::Bool(true)) {
        map.insert(
            "hasCompletedOnboarding".into(),
            serde_json::Value::Bool(true),
        );
        map.entry("theme")
            .or_insert_with(|| serde_json::Value::String("dark".into()));
        changed = true;
    }
    // The pod's checkout is the one directory the session runs in, and the
    // engine put it there: without this Claude Code opens at "Is this a
    // project you trust?", and autopilot's nudge answers its default, "No,
    // exit" (WI-3355).
    let mut value = serde_json::Value::Object(map);
    changed |= crate::claude_config::accept_in_config(&mut value, work_dir);
    if !changed {
        return files;
    }
    if let Ok(bytes) = serde_json::to_vec(&value) {
        *config = bytes;
    }
    files
}

/// Whether a restored bundle path is `conversation`'s transcript, in either
/// agent's layout. The predicate is the session agent's, so the engine and the
/// pod that writes the rollout agree on the name (WI-3873).
fn resumes_conversation(path: &str, conversation: &str) -> bool {
    vogt_session_agent::is_conversation_transcript(path, conversation)
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
                    "bootstrap needs a JSON body {{session_id, source, cli}} \
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
    let engine_endpoint = cfg.engine_endpoint.clone();
    // The pod's clone root, for the Claude project-trust entry (WI-3355): it
    // moves under the workspace tree when that is on (WI-3374).
    let pod_work_dir = cfg.work_dir();
    let Some(source) = state.agent_auth.take_source(id).await else {
        return (StatusCode::BAD_REQUEST, "session source is unavailable").into_response();
    };
    let provider = match state.sessions.source_registry().and_then(|r| {
        r.for_source(&source)
            .map_err(|e| crate::error::ApiError::Config(e.to_string()))
    }) {
        Ok(p) => p,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    };
    let minted = state.agent_auth.take_source_credential(id).await;
    let graft_session = state.agent_auth.take_graft_session(id).await;
    let source_token = match resolve_source_token(
        minted,
        provider.mints_credentials(),
        provider.shared_credential(),
    ) {
        Ok(token) => token,
        Err(_) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "the source credential is configured but unreadable",
            )
                .into_response()
        }
    };
    let broker = state.sessions.secret_broker();
    let mut breadcrumbs = AuthBreadcrumbs::snapshot(broker, id);
    // WI-3886: a Codex pod fetches its provider keys at launch, around the CLI
    // only. The bootstrap names them among the ondemand entries and carries no
    // value: `vogt-agent-auth fetch` resolves each one into the Codex child
    // (the session agent's launch line), so a key never joins the pod's
    // long-lived environment.
    let providers = if req.cli == "codex" {
        broker.providers()
    } else {
        Vec::new()
    };
    for entry in crate::llm_providers::manifest_entries(&providers) {
        if !breadcrumbs.ondemand.contains(&entry.var) {
            breadcrumbs.ondemand.push(entry.var);
        }
    }
    // WI-4121: a pod's cadastre entry authenticates with the same bearer the
    // host registration names, and only when this deployment declares that
    // variable (any policy, `ondemand` included) or this session holds a
    // grant for it. Without one the entry is left out rather than registered
    // to answer 401. A Codex session fetches the value like a provider key:
    // its name joins the ondemand breadcrumbs and the session agent resolves
    // it into the child, never into the pod's environment or the config.
    let cadastre_bearer = broker
        .manifest()
        .iter()
        .any(|entry| entry.var == crate::session_endpoints::CADASTRE_TOKEN_ENV)
        || breadcrumbs
            .granted
            .iter()
            .any(|var| var == crate::session_endpoints::CADASTRE_TOKEN_ENV);
    let cadastre_named = breadcrumbs
        .ondemand
        .iter()
        .any(|var| var == crate::session_endpoints::CADASTRE_TOKEN_ENV);
    if cadastre_bearer && req.cli == "codex" && !cadastre_named {
        breadcrumbs
            .ondemand
            .push(crate::session_endpoints::CADASTRE_TOKEN_ENV.to_string());
    }
    let mut env = bootstrap_env(
        cfg.engine_endpoint,
        id,
        &req.cli,
        req.model.as_deref(),
        &provider.pod_env(&source),
        req.extra_ca_file.as_deref(),
        &breadcrumbs,
    );
    // The caller's extra environment and working directory (WI-3883) travel
    // here rather than in the pod spec, so a value never lands in the
    // Kubernetes object. The environment stays apart from `env` so the
    // entrypoint cannot export it into its own shell: it is handed to the
    // agent at the final `exec`, after the clone that spends the source
    // token, or a caller's `GRAFT_ENDPOINT` would receive that token.
    let launch = state.agent_auth.take_launch(id).await.unwrap_or_default();
    let mut agent_env = serde_json::Map::new();
    for (name, value) in launch.agent_env {
        agent_env.insert(name, serde_json::Value::String(value));
    }
    if let Some(dir) = launch.work_dir {
        env.insert("VOGT_WORK_DIR".into(), serde_json::Value::String(dir));
    }
    env.insert(
        "VOGT_SOURCE_JSON".into(),
        serde_json::to_string(&source)
            .expect("source serializes")
            .into(),
    );
    // A Claude session gets what a host-launched one does: its own core
    // credential and the engine's MCP endpoints (WI-3243). The credential
    // travels in the environment, never in the config file.
    let (mut mcp_config, mut codex_config, mut opencode_config) = (None, None, None);
    if matches!(req.cli.as_str(), "claude" | "codex" | "opencode") {
        let token = state.sessions.mint_agent_token(&req.cli, id).await;
        let endpoints = state.sessions.pod_mcp_endpoints(&engine_endpoint);
        let has = token.is_some();
        match req.cli.as_str() {
            "claude" => {
                mcp_config = Some(crate::session_endpoints::claude_mcp_config(
                    &endpoints,
                    has,
                    cadastre_bearer,
                ))
            }
            "codex" => {
                codex_config = Some(crate::session_endpoints::codex_mcp_config(
                    &endpoints,
                    has,
                    cadastre_bearer,
                ))
            }
            _ => {
                opencode_config = Some(crate::session_endpoints::opencode_mcp_config(
                    &endpoints,
                    has,
                    cadastre_bearer,
                ))
            }
        }
        if let Some(token) = token {
            for (name, value) in [
                ("VOGT_HTTP_TOKEN", token),
                ("VOGT_ENGINE_SESSION_ID", id.to_string()),
            ] {
                env.insert(name.into(), serde_json::Value::String(value));
            }
        }
    }
    // WI-3491/WI-3886: a Codex pod gets the registered LLM providers as it
    // launches, as a host session does: their `model_providers` tables in its
    // config, and their `env_key` names among the ondemand breadcrumbs above.
    // The session agent fetches each key around the Codex child; resolving it
    // here would put the value in the whole pod's environment.
    if req.cli == "codex" && !providers.is_empty() {
        codex_config = Some(
            crate::llm_providers::codex_config(req.model.as_deref(), &providers)
                + "\n"
                + &codex_config.unwrap_or_default(),
        );
    }
    // A pod resumes from the account bundle the engine holds: the login, the
    // global config and, for a woken session, its conversation's transcript.
    let crate::sessions::PodAccountConversation {
        account,
        conversation,
    } = state.sessions.pod_account_conversation(id);
    let mut bundle = serde_json::Map::new();
    let mut resume = false;
    // The login travels whenever the session holds an account: an agent-task
    // session has no conversation pinned yet, but still needs to be signed in
    // (WI-3334). A Codex session never holds a Claude account (the engine
    // refuses one for a non-Claude CLI), but its rollout belongs to the
    // conversation, not an account: snapshot it with an empty account or a
    // wake would seed nothing and start fresh (WI-3873).
    let snapshot_account = account
        .as_deref()
        .or_else(|| (req.cli == "codex").then_some(""));
    if let Some(account) = snapshot_account {
        let files = state
            .sessions
            .secret_broker()
            .bundle_snapshot(account, conversation.as_deref().unwrap_or(""));
        for (path, bytes) in with_onboarding_config(files, &pod_work_dir) {
            // Claude keeps `<conversation>.jsonl`; a Codex rollout is
            // `rollout-<stamp>-<conversation>.jsonl`. Both must count, or a
            // woken Codex pod starts a fresh conversation and loses its
            // transcript (WI-3873).
            resume |= conversation
                .as_deref()
                .is_some_and(|c| resumes_conversation(&path, c));
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
        agent_env,
        source,
        source_token,
        graft_session,
        mcp_config,
        codex_config,
        opencode_config,
        brief: state.sessions.session_brief(id),
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
    ws.max_message_size(AGENT_WS_MAX_MESSAGE_SIZE)
        .on_upgrade(move |socket| relay(socket, Arc::clone(&state.agent_auth), id))
        .into_response()
}
/// Decides when a refused frame earns a `Resync` (WI-3282). While the reader
/// is slow every frame behind the first refused one is refused too; one
/// Resync per expected offset is enough, since the agent resends everything
/// from there. A frame that starts at the expected offset is the head of
/// that resend, so a refusal of it may ask again. Once the output stream is
/// closed (the session finished) nothing can be taken, so frames are dropped
/// without a Resync rather than looping resend after resend.
#[derive(Debug, Default)]
struct ResyncGate {
    sent_for: Option<u64>,
}

impl ResyncGate {
    fn accepted(&mut self) {
        self.sent_for = None;
    }

    /// The offset to send a `Resync` for, or `None` to drop quietly.
    fn refused(&mut self, gap: &OutputGap, closed: bool) -> Option<u64> {
        if closed {
            return None;
        }
        if self.sent_for == Some(gap.expected) && gap.got != gap.expected {
            return None;
        }
        self.sent_for = Some(gap.expected);
        Some(gap.expected)
    }
}

async fn send_frame<S>(socket: &mut S, frame: &Frame) -> bool
where
    S: futures_util::Sink<axum::extract::ws::Message> + Unpin,
{
    let text = serde_json::to_string(frame).expect("frames serialize");
    socket
        .send(axum::extract::ws::Message::Text(text.into()))
        .await
        .is_ok()
}
/// Generic over the socket so tests can drive it without an HTTP server.
async fn relay<S, E>(mut socket: S, agent_auth: Arc<AgentAuth>, id: Uuid)
where
    S: futures_util::Stream<Item = Result<axum::extract::ws::Message, E>>
        + futures_util::Sink<axum::extract::ws::Message>
        + Unpin,
{
    let Some(link) = agent_auth.pod(id) else {
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
    agent_auth.attach(id, tx.clone(), Arc::clone(&link)).await;
    // A stop asked for while no agent was attached is delivered on arrival.
    if link.stop_requested() {
        let _ = tx.send(Frame::Signal {
            signal: "kill".into(),
        });
    }
    let mut resync = ResyncGate::default();
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
                        Frame::Output { offset, data } => {
                            let refused = if data.len() > AGENT_BUFFER_BYTES {
                                // Refused but not fatal: the Resync below has
                                // the agent resend from its replay, in chunks.
                                Err((
                                    OutputGap { expected: link.next_offset(), got: offset },
                                    Frame::Error {
                                        code: "output_too_large".into(),
                                        message: format!("output frame exceeds {} bytes", AGENT_BUFFER_BYTES),
                                    },
                                ))
                            } else {
                                link.ingest(offset, &data).map_err(|gap| {
                                    let error = Frame::Error {
                                        code: "output_gap".into(),
                                        message: format!("expected offset {}, got {}", gap.expected, gap.got),
                                    };
                                    (gap, error)
                                })
                            };
                            match refused {
                            Ok(next) => {
                                resync.accepted();
                                if !send_frame(&mut socket, &Frame::Ack { offset: next }).await { break; }
                            }
                            Err((gap, error)) => {
                                // Say where the stream ends, so the agent can
                                // resend from there or mark what it cannot.
                                if let Some(offset) = resync.refused(&gap, link.output_closed()) {
                                    let _ = send_frame(&mut socket, &error).await;
                                    if !send_frame(&mut socket, &Frame::Resync { offset }).await { break; }
                                }
                            }
                            }
                        }
                        Frame::Gap { offset, lost } => {
                            if lost > AGENT_MAX_GAP_BYTES || offset.checked_add(lost).is_none() {
                                let _ = send_frame(&mut socket, &Frame::Error {
                                    code: "invalid_gap".into(),
                                    message: "gap exceeds the replay limit or overflows the stream offset".into(),
                                }).await;
                                break;
                            }
                            match link.ingest_gap(offset, lost) {
                            Ok(next) => {
                                resync.accepted();
                                if !send_frame(&mut socket, &Frame::Ack { offset: next }).await { break; }
                            }
                            Err(gap) => {
                                if let Some(offset) = resync.refused(&gap, link.output_closed()) {
                                    if !send_frame(&mut socket, &Frame::Resync { offset }).await { break; }
                                }
                            }
                            }
                        }
                        Frame::Flushed { ok } => {
                            let counter = if ok { &link.flushes } else { &link.flush_failures };
                            counter.fetch_add(1, Ordering::AcqRel);
                            link.flush_notify.notify_waiters();
                        }
                        Frame::Heartbeat { .. } => {
                            link.note_heartbeat();
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
    agent_auth.detach(id, &tx).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(name: &str, models: &[&str]) -> crate::llm_providers::Provider {
        crate::llm_providers::Provider {
            name: name.into(),
            base_url: "http://litellm:4000/v1".into(),
            wire_api: "chat".into(),
            env_key: "PROVIDER_KEY".into(),
            secret_project: "project".into(),
            secret_name: "PROVIDER_KEY".into(),
            context_window: None,
            auto_compact_token_limit: None,
            models: models.iter().map(|model| (*model).into()).collect(),
        }
    }

    #[test]
    fn codex_model_selection_prefers_request_then_provider_then_default() {
        let providers = [
            provider("deepseek", &["deepseek-chat"]),
            provider("other", &[]),
        ];
        assert_eq!(
            crate::llm_providers::codex_selection(Some("requested"), &providers)
                .map(|(model, provider)| (model, provider.name.as_str())),
            Some(("requested".into(), "deepseek"))
        );
        assert_eq!(
            crate::llm_providers::codex_selection(None, &providers)
                .map(|(model, provider)| (model, provider.name.as_str())),
            Some(("deepseek-chat".into(), "deepseek"))
        );
        let providers = [provider("other", &[])];
        assert_eq!(
            crate::llm_providers::codex_selection(None, &providers)
                .map(|(model, provider)| (model, provider.name.as_str())),
            Some(("grok".into(), "other"))
        );
    }

    #[test]
    fn a_slow_reader_gets_one_resync_per_expected_offset() {
        let (sender, _receiver) = std::sync::mpsc::sync_channel(1);
        let link = PodLink::new(sender);
        let mut gate = ResyncGate::default();
        assert_eq!(link.ingest(0, b"a"), Ok(1));
        let mut resyncs = 0;
        for offset in 1..17 {
            let gap = link.ingest(offset, b"b").unwrap_err();
            if gate.refused(&gap, link.output_closed()).is_some() {
                resyncs += 1;
            }
        }
        assert_eq!(resyncs, 1);
        // The resend's head, refused again, may ask once more.
        let gap = link.ingest(1, b"b").unwrap_err();
        assert_eq!(gate.refused(&gap, link.output_closed()), Some(1));
        let gap = link.ingest(2, b"c").unwrap_err();
        assert_eq!(gate.refused(&gap, link.output_closed()), None);
    }

    #[test]
    fn output_after_finish_is_dropped_without_a_resync() {
        let (sender, _receiver) = std::sync::mpsc::sync_channel(4);
        let link = PodLink::new(sender);
        let mut gate = ResyncGate::default();
        link.finish(0);
        assert!(link.output_closed());
        for _ in 0..3 {
            let gap = link.ingest(0, b"a").unwrap_err();
            assert_eq!(gate.refused(&gap, link.output_closed()), None);
        }
    }

    #[test]
    fn an_oversized_frame_is_refused_with_a_resync_not_a_disconnect() {
        let (sender, _receiver) = std::sync::mpsc::sync_channel(4);
        let link = PodLink::new(sender);
        let mut gate = ResyncGate::default();
        let gap = link
            .ingest(0, &vec![0; AGENT_BUFFER_BYTES + 1])
            .unwrap_err();
        assert_eq!(gate.refused(&gap, link.output_closed()), Some(0));
    }

    /// An in-memory WebSocket for driving [`relay`] (WI-3282): what the test
    /// sends arrives as agent messages, and what relay sends comes back.
    struct FakeSocket {
        incoming: mpsc::UnboundedReceiver<axum::extract::ws::Message>,
        outgoing: mpsc::UnboundedSender<axum::extract::ws::Message>,
    }

    impl futures_util::Stream for FakeSocket {
        type Item = Result<axum::extract::ws::Message, std::convert::Infallible>;
        fn poll_next(
            mut self: std::pin::Pin<&mut Self>,
            cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Self::Item>> {
            self.incoming.poll_recv(cx).map(|m| m.map(Ok))
        }
    }

    impl futures_util::Sink<axum::extract::ws::Message> for FakeSocket {
        type Error = ();
        fn poll_ready(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), ()>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn start_send(
            self: std::pin::Pin<&mut Self>,
            item: axum::extract::ws::Message,
        ) -> Result<(), ()> {
            self.outgoing.send(item).map_err(|_| ())
        }
        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), ()>> {
            std::task::Poll::Ready(Ok(()))
        }
        fn poll_close(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Result<(), ()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    /// The agent's end of a relay running over a [`FakeSocket`].
    struct Agent {
        to_engine: mpsc::UnboundedSender<axum::extract::ws::Message>,
        from_engine: mpsc::UnboundedReceiver<axum::extract::ws::Message>,
    }

    impl Agent {
        fn send(&self, frame: Frame) {
            let text = serde_json::to_string(&frame).unwrap();
            self.to_engine
                .send(axum::extract::ws::Message::Text(text.into()))
                .unwrap();
        }

        /// Every frame relay sends until it falls quiet, and whether it
        /// closed the socket.
        async fn drain(&mut self) -> (Vec<Frame>, bool) {
            let mut frames = Vec::new();
            loop {
                match tokio::time::timeout(Duration::from_millis(300), self.from_engine.recv())
                    .await
                {
                    Err(_) => return (frames, false),
                    Ok(None) => return (frames, true),
                    Ok(Some(axum::extract::ws::Message::Text(text))) => {
                        frames.push(serde_json::from_str(&text).unwrap())
                    }
                    Ok(Some(_)) => {}
                }
            }
        }
    }

    async fn connect(auth: &Arc<AgentAuth>, id: Uuid) -> Agent {
        let (to_engine, incoming) = mpsc::unbounded_channel();
        let (outgoing, from_engine) = mpsc::unbounded_channel();
        let socket = FakeSocket { incoming, outgoing };
        tokio::spawn(relay(socket, Arc::clone(auth), id));
        let agent = Agent {
            to_engine,
            from_engine,
        };
        agent.send(Frame::Hello {
            session_id: id.to_string(),
        });
        agent
    }

    fn resyncs(frames: &[Frame]) -> usize {
        frames
            .iter()
            .filter(|f| matches!(f, Frame::Resync { .. }))
            .count()
    }

    #[tokio::test]
    async fn relay_sends_one_resync_for_a_burst_of_refused_frames() {
        let auth = Arc::new(AgentAuth::default());
        let id = Uuid::new_v4();
        let (_link, _reader) = auth.register_pod(id);
        let mut agent = connect(&auth, id).await;
        let capacity = AGENT_OUTPUT_CHANNEL_CAPACITY as u64;
        // Fill the queue; the reader never drains it.
        for offset in 0..capacity {
            agent.send(Frame::Output {
                offset,
                data: b"a".to_vec(),
            });
        }
        // A burst behind the full queue: every frame is refused.
        for offset in capacity..capacity + 10 {
            agent.send(Frame::Output {
                offset,
                data: b"b".to_vec(),
            });
        }
        let (frames, closed) = agent.drain().await;
        assert!(!closed);
        assert_eq!(resyncs(&frames), 1, "{frames:?}");
        assert!(frames.contains(&Frame::Resync { offset: capacity }));
    }

    #[tokio::test]
    async fn relay_sends_no_resync_for_output_after_finish() {
        let auth = Arc::new(AgentAuth::default());
        let id = Uuid::new_v4();
        let (link, _reader) = auth.register_pod(id);
        let mut agent = connect(&auth, id).await;
        link.finish(0);
        for _ in 0..3 {
            agent.send(Frame::Output {
                offset: 0,
                data: b"a".to_vec(),
            });
        }
        let (frames, closed) = agent.drain().await;
        assert!(!closed);
        assert_eq!(frames, vec![]);
    }

    #[tokio::test]
    async fn relay_refuses_an_oversized_output_and_keeps_the_socket_open() {
        let auth = Arc::new(AgentAuth::default());
        let id = Uuid::new_v4();
        let (_link, _reader) = auth.register_pod(id);
        let mut agent = connect(&auth, id).await;
        agent.send(Frame::Output {
            offset: 0,
            data: vec![0; AGENT_BUFFER_BYTES + 1],
        });
        let (frames, closed) = agent.drain().await;
        assert!(
            !closed,
            "socket closed after an oversized frame: {frames:?}"
        );
        assert!(
            matches!(&frames[0], Frame::Error { code, .. } if code == "output_too_large"),
            "{frames:?}"
        );
        assert_eq!(frames[1..], [Frame::Resync { offset: 0 }]);
        agent.send(Frame::Heartbeat { cursor: 0 });
        assert_eq!(agent.drain().await, (vec![Frame::Ack { offset: 0 }], false));
    }

    #[test]
    fn full_output_queue_preserves_cursor_until_retry() {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let link = PodLink::new(sender);
        assert_eq!(link.ingest(0, b"a"), Ok(1));
        assert!(link.ingest(1, b"b").is_err());
        assert!(link.ingest_gap(1, 1).is_err());
        assert_eq!(link.next_offset(), 1);
        assert_eq!(receiver.recv().unwrap(), b"a");
        assert_eq!(link.ingest(1, b"b"), Ok(2));
        assert_eq!(receiver.recv().unwrap(), b"b");
        drop(receiver);
        assert!(link.ingest(2, b"c").is_err());
        assert_eq!(link.next_offset(), 2);
    }

    #[test]
    fn largest_output_frame_fits_the_ws_message_limit() {
        // Worst case for JSON byte-array encoding: every byte is three digits.
        let frame = Frame::Output {
            offset: u64::MAX,
            data: vec![255; AGENT_BUFFER_BYTES],
        };
        let encoded = serde_json::to_string(&frame).unwrap();
        assert!(
            encoded.len() <= AGENT_WS_MAX_MESSAGE_SIZE,
            "{}",
            encoded.len()
        );
    }

    #[test]
    fn gap_larger_than_the_replay_buffer_is_accepted() {
        let (sender, receiver) = std::sync::mpsc::sync_channel(4);
        let link = PodLink::new(sender);
        let lost = 64 * AGENT_BUFFER_BYTES as u64;
        assert_eq!(link.ingest_gap(0, lost), Ok(lost));
        assert_eq!(
            receiver.recv().unwrap(),
            format!("\r\n[{lost} bytes lost]\r\n").into_bytes()
        );
        assert_eq!(link.ingest(lost, b"x"), Ok(lost + 1));
    }

    #[test]
    fn invalid_lengths_and_overflow_leave_output_and_cursor_unchanged() {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let link = PodLink::new(sender);
        assert!(link.ingest(0, &vec![0; AGENT_BUFFER_BYTES + 1]).is_err());
        assert!(link.ingest_gap(0, u64::MAX).is_err());
        assert!(link.ingest_gap(0, AGENT_MAX_GAP_BYTES + 1).is_err());
        assert_eq!(link.next_offset(), 0);
        link.skip_to(u64::MAX - 1);
        assert!(link.ingest_gap(u64::MAX - 1, 2).is_err());
        assert!(link.ingest(u64::MAX - 1, b"ab").is_err());
        assert_eq!(link.next_offset(), u64::MAX - 1);
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn bootstrap_env_carries_the_providers_source_env() {
        let source_env = vec![("SOURCE_ENDPOINT".to_string(), "http://src".to_string())];
        let env = bootstrap_env(
            "http://engine".into(),
            Uuid::nil(),
            "claude",
            Some("claude-opus-4-6"),
            &source_env,
            None,
            &AuthBreadcrumbs::default(),
        );
        assert_eq!(env["SOURCE_ENDPOINT"], "http://src");
        assert_eq!(env["VOGT_CLI"], "claude");
        assert_eq!(env["VOGT_MODEL"], "claude-opus-4-6");
        let bare = bootstrap_env(
            "e".into(),
            Uuid::nil(),
            "claude",
            None,
            &[],
            None,
            &AuthBreadcrumbs::default(),
        );
        assert!(!bare.contains_key("SOURCE_ENDPOINT"));
        assert!(!bare.contains_key("VOGT_MODEL"));
    }

    /// WI-3884: a pod's bootstrap env names the broker model and the names
    /// the deployment declared / the session holds — never a value.
    #[test]
    fn bootstrap_env_carries_the_four_agent_auth_breadcrumbs() {
        let breadcrumbs = AuthBreadcrumbs {
            granted: vec!["GRANTED_ONE".into(), "GRANTED_TWO".into()],
            ondemand: vec!["ONDEMAND_ONE".into()],
            writable: vec!["WRITABLE_ONE".into(), "WRITABLE_TWO".into()],
        };
        let env = bootstrap_env(
            "http://engine".into(),
            Uuid::nil(),
            "codex",
            None,
            &[],
            None,
            &breadcrumbs,
        );
        assert_eq!(env["AGENT_AUTH_MODE"], "brokered");
        assert_eq!(env["AGENT_AUTH_GRANTED"], "GRANTED_ONE GRANTED_TWO");
        assert_eq!(env["AGENT_AUTH_ONDEMAND"], "ONDEMAND_ONE");
        assert_eq!(env["AGENT_AUTH_WRITABLE"], "WRITABLE_ONE WRITABLE_TWO");
        // The empty case is present and empty, not absent: a session can tell
        // "nothing declared" from an older bootstrap that never set it.
        let empty = bootstrap_env(
            "e".into(),
            Uuid::nil(),
            "claude",
            None,
            &[],
            None,
            &AuthBreadcrumbs::default(),
        );
        for name in [
            "AGENT_AUTH_MODE",
            "AGENT_AUTH_GRANTED",
            "AGENT_AUTH_ONDEMAND",
            "AGENT_AUTH_WRITABLE",
        ] {
            assert!(empty.contains_key(name), "bootstrap env lacks {name}");
        }
        assert_eq!(empty["AGENT_AUTH_GRANTED"], "");
        assert_eq!(empty["AGENT_AUTH_ONDEMAND"], "");
        assert_eq!(empty["AGENT_AUTH_WRITABLE"], "");
    }

    #[test]
    fn the_minted_credential_wins_and_the_shared_one_is_only_a_fallback() {
        use crate::source_provider::CredentialHandle;
        let dir = tempfile::tempdir().unwrap();
        let file = |name: &str, value: &str| {
            let path = dir.path().join(name);
            std::fs::write(&path, value).unwrap();
            CredentialHandle { path }
        };
        let minted = file("minted", "minted-token\n");
        let shared = file("shared", "shared-token\n");
        // A minted credential is used whatever else is configured.
        for mints in [true, false] {
            assert_eq!(
                resolve_source_token(Some(minted.clone()), mints, Some(shared.clone()))
                    .unwrap()
                    .as_deref(),
                Some("minted-token")
            );
        }
        // A provider that mints none: the shared credential is the fallback.
        assert_eq!(
            resolve_source_token(None, false, Some(shared.clone()))
                .unwrap()
                .as_deref(),
            Some("shared-token")
        );
        assert_eq!(resolve_source_token(None, false, None).unwrap(), None);
        // The provider mints but nothing was minted for this pod: the shared
        // credential is not handed out in its place.
        assert_eq!(
            resolve_source_token(None, true, Some(shared)).unwrap(),
            None
        );
        // A configured file that is missing is an error, not silence.
        let missing = CredentialHandle {
            path: dir.path().join("missing"),
        };
        assert!(resolve_source_token(None, false, Some(missing)).is_err());
    }

    #[tokio::test]
    async fn a_minted_credential_is_taken_once_and_withdrawn_with_the_bootstrap_token() {
        use crate::source_provider::CredentialHandle;
        let h = |n: &str| CredentialHandle { path: n.into() };
        let auth = AgentAuth::default();
        let id = Uuid::new_v4();
        auth.issue(id, b"boot").await;
        auth.stash_source_credential(id, h("/c1")).await;
        assert!(auth.bootstrap_pending(id).await);
        assert_eq!(auth.take_source_credential(id).await, Some(h("/c1")));
        assert!(
            auth.take_source_credential(id).await.is_none(),
            "taken once"
        );

        auth.stash_source_credential(id, h("/c2")).await;
        auth.withdraw(id).await;
        assert!(!auth.bootstrap_pending(id).await);
        assert!(auth.take_source_credential(id).await.is_none());

        // Forgetting a pod drops a credential that was never collected.
        auth.stash_source_credential(id, h("/c3")).await;
        auth.forget_pod(id);
        assert!(auth.take_source_credential(id).await.is_none());
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
        let (link, _reader) = auth.register_pod(id);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let tx2 = tx.clone();
        auth.attach(id, tx, link).await;
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
        let (link, _reader) = auth.register_pod(id);
        let (old, _old_rx) = mpsc::unbounded_channel();
        let (new, mut new_rx) = mpsc::unbounded_channel();
        auth.attach(id, old.clone(), Arc::clone(&link)).await;
        auth.attach(id, new, link).await;
        auth.detach(id, &old).await;
        auth.send(id, Frame::Ack { offset: 3 }).unwrap();
        assert_eq!(new_rx.recv().await, Some(Frame::Ack { offset: 3 }));
    }

    #[test]
    fn a_gap_marker_records_the_loss_and_continues_after_it() {
        let (tx, rx) = std::sync::mpsc::sync_channel(AGENT_OUTPUT_CHANNEL_CAPACITY);
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
        let (tx, rx) = std::sync::mpsc::sync_channel(AGENT_OUTPUT_CHANNEL_CAPACITY);
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

    /// WI-3873: a restored bundle resumes the agent only when it carries the
    /// conversation's transcript. Claude's layout predates the pod Codex
    /// rollout; both must count.
    #[test]
    fn a_restored_bundle_resumes_either_agent_layout() {
        assert!(resumes_conversation("projects/work/conv.jsonl", "conv"));
        assert!(resumes_conversation("conv.jsonl", "conv"));
        assert!(resumes_conversation(
            "sessions/2026/11/09/rollout-2026-11-09T00-00-00-conv.jsonl",
            "conv"
        ));
        assert!(!resumes_conversation(
            "sessions/2026/11/09/rollout-2026-11-09T00-00-00-other.jsonl",
            "conv"
        ));
        assert!(!resumes_conversation(
            "sessions/2026/11/09/conv.jsonl",
            "conv"
        ));
    }
}
