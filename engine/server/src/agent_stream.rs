//! Authenticated relay for outbound pod session agents.
use crate::app::AppState;
use axum::{
    extract::{Path, State, WebSocketUpgrade},
    http::{header, HeaderMap, StatusCode},
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use parking_lot::Condvar;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use std::{
    collections::HashMap,
    io::Read,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};
use subtle::ConstantTimeEq;
use tokio::sync::{mpsc, watch, Notify};
use uuid::Uuid;
use vogt_engine_contract::source::SourceDescriptor;
use vogt_session_agent::Frame;
const BOOTSTRAP_TTL: Duration = Duration::from_secs(300);
const AGENT_OUTPUT_CHANNEL_CAPACITY: usize = 16;
/// Frames the engine may queue for one attached agent. Control frames are
/// never dropped: when the queue is full a send fails loudly instead, and a
/// later resize supersedes a lost one, so only resizes are shed.
const AGENT_COMMAND_QUEUE: usize = 1024;
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
    /// The MCP set the start resolved (WI-4489): the bootstrap hands the pod
    /// only those servers, as a host launch's trim does.
    pub mcp_set: crate::mcp_set::McpSet,
    /// The project's `mcp_extra`, kept on top of the worker set.
    pub mcp_extra: Vec<String>,
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

fn try_queue(tx: &mpsc::Sender<Frame>, frame: Frame) -> std::io::Result<()> {
    use mpsc::error::TrySendError;
    match tx.try_send(frame) {
        Ok(()) => Ok(()),
        Err(TrySendError::Full(Frame::Resize { .. })) => Ok(()),
        Err(TrySendError::Full(_)) => Err(std::io::Error::new(
            std::io::ErrorKind::WouldBlock,
            "agent command queue full",
        )),
        Err(TrySendError::Closed(_)) => Err(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "agent stream closed",
        )),
    }
}

type AgentLinks = HashMap<Uuid, (mpsc::Sender<Frame>, Arc<PodLink>)>;

#[derive(Debug, Default)]
pub struct AgentAuth {
    pending: parking_lot::Mutex<HashMap<Uuid, Pending>>,
    /// The last launch's carried fields, until its bootstrap request takes
    /// them. Withdrawn with the credential, so a pod that never boots leaves
    /// nothing behind.
    launches: parking_lot::Mutex<HashMap<Uuid, PodLaunch>>,
    /// Per-session source credentials minted at pod create, held until the
    /// pod's bootstrap request takes them (or the pod is withdrawn or
    /// forgotten). Handles only: the value stays in the provider's file.
    source_credentials: parking_lot::Mutex<HashMap<Uuid, PendingCredential>>,
    sources: parking_lot::Mutex<HashMap<Uuid, SourceDescriptor>>,
    /// The core session id a minted graft token is bound to, held until the
    /// pod's bootstrap request writes the token under that name.
    graft_sessions: parking_lot::Mutex<HashMap<Uuid, String>>,
    sessions: parking_lot::Mutex<HashMap<Uuid, SessionCredential>>,
    /// The attached agent's command channel, beside the link it belongs to so
    /// attach/detach can update the connection state under one lock.
    links: parking_lot::Mutex<AgentLinks>,
    pods: parking_lot::Mutex<HashMap<Uuid, Arc<PodLink>>>,
}

/// How a requested salvage ended (WI-4132).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SalvageOutcome {
    /// The agent stored the archive (or found nothing to save).
    Done,
    /// No agent is attached, so the working tree cannot be read.
    NotAttached,
    /// The agent could not build or upload the salvage; the string is why.
    Failed(String),
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
    next_offset: parking_lot::Mutex<u64>,
    out: parking_lot::Mutex<Option<std::sync::mpsc::SyncSender<PodChunk>>>,
    exit: parking_lot::Mutex<Option<i32>>,
    exited: Condvar,
    exit_notify: Notify,
    stop_requested: AtomicBool,
    created: Instant,
    /// Write-backs the agent has confirmed (`Flushed { ok: true }`).
    flushes: AtomicU64,
    /// Write-backs the agent reported it could not complete.
    flush_failures: AtomicU64,
    flush_notify: Notify,
    /// Salvage requests the agent answered, and the last failure reason.
    salvages: AtomicU64,
    salvage_notify: Notify,
    salvage_failure: parking_lot::Mutex<Option<String>>,
    /// The pod is gone and the stream forgotten; hibernation waits for it so
    /// a wake never races the old pod's teardown.
    torn_down: AtomicBool,
    torn_notify: Notify,
    /// An agent's socket is attached right now.
    connected: AtomicBool,
    /// An agent has completed its hello on this link at least once.
    ever_connected: AtomicBool,
    last_heartbeat: parking_lot::Mutex<Instant>,
    /// When the current agent's socket was lost, while it is not connected.
    disconnected_at: parking_lot::Mutex<Option<Instant>>,
    /// The link was rebuilt after an engine restart rather than registered by
    /// a launch: its cursor came from the output log, and until an agent
    /// dials in the pod's fate is unknown.
    adopted: AtomicBool,
    /// Stream offset up to which the session reader has written the output to
    /// the log. The engine acknowledges an agent's output only up to here, so
    /// an ack never covers bytes a kill could lose (WI-4442).
    durable: watch::Sender<u64>,
    /// The session reader has stopped: nothing more will be written.
    reader_stopped: AtomicBool,
    /// Set by [`Self::crash`]: the reader writes nothing it takes after this.
    crashed: AtomicBool,
    /// The session's output log. Set by [`Self::keep_log`] before the reader
    /// takes output; each durable offset is checkpointed beside it.
    log: parking_lot::Mutex<Option<std::path::PathBuf>>,
}

/// Output queued for the session reader: the stream offset just past the
/// bytes, and the bytes (a gap marker is text the log holds but the stream does
/// not).
type PodChunk = (u64, Vec<u8>);

/// Output the agent sent that does not continue the stream.
#[derive(Debug, PartialEq, Eq)]
pub struct OutputGap {
    pub expected: u64,
    pub got: u64,
}

impl PodLink {
    fn new(out: std::sync::mpsc::SyncSender<PodChunk>) -> Self {
        Self {
            next_offset: parking_lot::Mutex::new(0),
            out: parking_lot::Mutex::new(Some(out)),
            exit: parking_lot::Mutex::new(None),
            exited: Condvar::new(),
            exit_notify: Notify::new(),
            stop_requested: AtomicBool::new(false),
            created: Instant::now(),
            flushes: AtomicU64::new(0),
            flush_failures: AtomicU64::new(0),
            flush_notify: Notify::new(),
            salvages: AtomicU64::new(0),
            salvage_notify: Notify::new(),
            salvage_failure: parking_lot::Mutex::new(None),
            torn_down: AtomicBool::new(false),
            torn_notify: Notify::new(),
            connected: AtomicBool::new(false),
            ever_connected: AtomicBool::new(false),
            last_heartbeat: parking_lot::Mutex::new(Instant::now()),
            disconnected_at: parking_lot::Mutex::new(None),
            adopted: AtomicBool::new(false),
            durable: watch::Sender::new(0),
            reader_stopped: AtomicBool::new(false),
            crashed: AtomicBool::new(false),
            log: parking_lot::Mutex::new(None),
        }
    }

    /// Keep the session's output log, so every durable offset is checkpointed
    /// beside it (see [`checkpoint_path`]). Call before the reader takes output.
    pub fn keep_log(&self, log: std::path::PathBuf) {
        *self.log.lock() = Some(log);
    }

    /// Checkpoint `offset` (the stream offset the reader has written through)
    /// together with the log's length now. The log holds fewer or more bytes
    /// than the stream offset whenever a gap marker or a stripped OSC 7501
    /// query was written, so a restart needs both numbers. Returns false when
    /// the checkpoint could not be written: the offset must then stay unacked.
    fn checkpoint(&self, offset: u64) -> bool {
        let Some(log) = self.log.lock().clone() else {
            return true;
        };
        let written = std::fs::metadata(&log)
            .map_err(|e| e.to_string())
            .and_then(|m| {
                write_checkpoint(&checkpoint_path(&log), offset, m.len()).map_err(|e| e.to_string())
            });
        match written {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!(error = %e, "could not checkpoint the agent stream offset; output stays unacked");
                false
            }
        }
    }

    /// Stream offset up to which the output is in the log.
    pub fn durable_offset(&self) -> u64 {
        *self.durable.borrow()
    }

    /// The session reader wrote the output up to `offset`.
    fn mark_durable(&self, offset: u64) {
        self.durable.send_if_modified(|v| {
            if *v < offset {
                *v = offset;
                true
            } else {
                false
            }
        });
    }

    /// A receiver that wakes when the durable offset moves or the reader stops.
    fn durable_watch(&self) -> watch::Receiver<u64> {
        self.durable.subscribe()
    }

    /// The session reader is gone, so nothing more will reach the log.
    fn note_reader_stopped(&self) {
        self.reader_stopped.store(true, Ordering::Release);
        self.durable.send_modify(|_| {});
    }

    fn reader_stopped(&self) -> bool {
        self.reader_stopped.load(Ordering::Acquire)
    }

    /// Simulate the engine dying between an ack and its log write: the session
    /// reader writes nothing it takes from here on. Only tests call this.
    #[doc(hidden)]
    pub fn crash(&self) {
        self.crashed.store(true, Ordering::Release);
    }

    fn crashed(&self) -> bool {
        self.crashed.load(Ordering::Acquire)
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
        self.last_heartbeat.lock().elapsed()
    }

    fn note_heartbeat(&self) {
        *self.last_heartbeat.lock() = Instant::now();
    }

    /// A new agent completed its hello: it is attached and its heartbeat
    /// clock starts now, not when the previous socket went away.
    pub(crate) fn note_connected(&self) {
        *self.disconnected_at.lock() = None;
        self.ever_connected.store(true, Ordering::Release);
        self.connected.store(true, Ordering::Release);
        self.note_heartbeat();
    }

    /// The attached agent's socket went away. The link stays registered so a
    /// redial resumes it, but the heartbeat deadline no longer applies: the
    /// reconnect timeout owns a disconnect.
    pub(crate) fn note_disconnected(&self) {
        self.connected.store(false, Ordering::Release);
        *self.disconnected_at.lock() = Some(Instant::now());
    }

    /// How long the link has had no attached agent, if it has none now.
    pub fn disconnected_age(&self) -> Option<Duration> {
        self.disconnected_at.lock().map(|at| at.elapsed())
    }

    /// Rebuilt after an engine restart (see `AgentAuth::adopt_pod`).
    pub fn is_adopted(&self) -> bool {
        self.adopted.load(Ordering::Acquire)
    }

    /// Move the cursor forward past output the engine acknowledged but whose
    /// log no longer holds it. A kill cannot cause that: an ack waits for the
    /// log write (see [`Self::durable_offset`]), so the log always holds every
    /// acknowledged byte. The skip stays for a log that was lost or truncated
    /// after the fact. Only an adopted link does this, and only forward: the
    /// bytes are gone, and refusing the agent would strand it.
    fn skip_to(&self, cursor: u64) {
        let mut next = self.next_offset.lock();
        if cursor > *next {
            *next = cursor;
        }
        drop(next);
        self.mark_durable(cursor);
    }

    /// How long ago the engine registered this link (the pod was asked for).
    pub fn age(&self) -> Duration {
        self.created.elapsed()
    }

    pub fn next_offset(&self) -> u64 {
        *self.next_offset.lock()
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
        let mut next = self.next_offset.lock();
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
            self.enqueue(fresh.to_vec(), end, *next, offset)?;
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
        let mut next = self.next_offset.lock();
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
                end,
                *next,
                offset,
            )?;
            *next = end;
        }
        Ok(*next)
    }

    /// Queue `data`, which ends at stream offset `end`, for the session reader.
    fn enqueue(&self, data: Vec<u8>, end: u64, expected: u64, got: u64) -> Result<(), OutputGap> {
        let out = self.out.lock();
        let Some(out) = out.as_ref() else {
            return Err(OutputGap { expected, got });
        };
        out.try_send((end, data))
            .map_err(|_| OutputGap { expected, got })
    }

    /// Record the exit and end the output stream, so the reader thread sees
    /// EOF and the exit waiter returns.
    pub fn finish(&self, code: i32) {
        let mut exit = self.exit.lock();
        if exit.is_none() {
            *exit = Some(code);
        }
        self.out.lock().take();
        self.exited.notify_all();
        self.exit_notify.notify_waiters();
    }

    /// The output stream has ended (see [`Self::finish`]): nothing more can
    /// be taken, however often it is resent.
    fn output_closed(&self) -> bool {
        self.out.lock().is_none()
    }

    pub fn is_finished(&self) -> bool {
        self.exit.lock().is_some()
    }

    /// Block until the agent has reported the child's exit.
    pub fn wait_exit(&self) -> i32 {
        let mut exit = self.exit.lock();
        loop {
            if let Some(code) = *exit {
                return code;
            }
            self.exited.wait(&mut exit);
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
///
/// Each read is called only after the reader has written the bytes the
/// previous read returned, so a read marks those bytes durable before it
/// blocks. That is what lets the engine acknowledge them (WI-4442).
pub struct PodReader {
    rx: std::sync::mpsc::Receiver<PodChunk>,
    pending: Vec<u8>,
    at: usize,
    /// Stream offset just past `pending`.
    end: u64,
    /// The link this reader serves; `None` for a reader built on its own.
    link: Option<Arc<PodLink>>,
}

impl PodReader {
    fn crashed(&self) -> bool {
        self.link.as_ref().is_some_and(|link| link.crashed())
    }
}

impl Read for PodReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.at >= self.pending.len() {
            if let Some(link) = &self.link {
                // Checkpoint before the mark: an ack follows the mark, so the
                // checkpoint always covers every byte the agent was told is safe.
                if link.checkpoint(self.end) {
                    link.mark_durable(self.end);
                }
            }
            if self.crashed() {
                return Ok(0);
            }
            match self.rx.recv() {
                Ok((end, chunk)) => {
                    // A crashed engine never writes the chunk it took last.
                    if self.crashed() {
                        return Ok(0);
                    }
                    self.end = end;
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

impl Drop for PodReader {
    fn drop(&mut self) {
        if let Some(link) = &self.link {
            link.note_reader_stopped();
        }
    }
}

/// The checkpoint kept beside a session's output log: `<id>.offset`, holding
/// `"<stream offset> <log length>"`.
pub fn checkpoint_path(log: &std::path::Path) -> std::path::PathBuf {
    log.with_extension("offset")
}

/// Write the checkpoint atomically, and durably, so a crash leaves either the
/// old pair or the new one.
fn write_checkpoint(path: &std::path::Path, offset: u64, log_len: u64) -> std::io::Result<()> {
    let tmp = path.with_extension("offset.tmp");
    let mut file = std::fs::File::create(&tmp)?;
    std::io::Write::write_all(&mut file, format!("{offset} {log_len}\n").as_bytes())?;
    file.sync_data()?;
    std::fs::rename(&tmp, path)
}

fn read_checkpoint(path: &std::path::Path) -> Option<(u64, u64)> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut parts = text.split_whitespace();
    let offset = parts.next()?.parse().ok()?;
    let log_len = parts.next()?.parse().ok()?;
    Some((offset, log_len))
}

/// Where a restarted engine resumes a pod session's stream, and what it does
/// to the log to make that true. The checkpoint's offset is the cursor, and
/// the log is cut back to the checkpoint's length: bytes past the checkpoint
/// were never acknowledged, so the agent still holds them and resends them.
/// A session with no checkpoint (one from before checkpoints existed) resumes
/// at the log's end, as it always did.
pub fn restore_resume_point(log: &std::path::Path) -> std::io::Result<u64> {
    let log_len = std::fs::metadata(log).map_or(0, |m| m.len());
    let Some((offset, checkpoint_len)) = read_checkpoint(&checkpoint_path(log)) else {
        return Ok(log_len);
    };
    if log_len > checkpoint_len {
        std::fs::OpenOptions::new()
            .write(true)
            .open(log)?
            .set_len(checkpoint_len)?;
    } else if log_len < checkpoint_len {
        tracing::warn!(
            log = %log.display(),
            log_len,
            checkpoint_len,
            "the session log is shorter than its checkpoint; output between them is lost"
        );
    }
    Ok(offset)
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
    /// The pod runs a person's shell or a raw command, not the agent
    /// (WI-4521): every agent's config comes back, so a hand-typed `claude`
    /// or `codex` gets the engine's MCP servers (and Codex its providers).
    #[serde(default)]
    pub shell: bool,
}
#[derive(Debug, Serialize)]
pub struct BootstrapResponse {
    #[serde(serialize_with = "crate::secret::serialize_exposed")]
    pub session_token: crate::secret::Secret<String>,
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
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "crate::secret::serialize_exposed_opt"
    )]
    pub source_token: Option<crate::secret::Secret<String>>,
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
    /// Launch-time manifest entries (WI-4517): the pod entrypoint fetches
    /// each through the broker before the agent starts, as `vogt-agent-auth
    /// run` resolves them on a host. A required one that fails stops the
    /// launch; an optional one is skipped with a warning.
    pub launch_required: Vec<String>,
    pub launch_optional: Vec<String>,
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
            // The deployment's own lines only: a provider key is fetched
            // around its CLI (WI-3886), never exported at launch.
            launch_required: launch_names(
                &broker.base_manifest(),
                crate::secret_broker::ManifestPolicy::Required,
            ),
            launch_optional: launch_names(
                &broker.base_manifest(),
                crate::secret_broker::ManifestPolicy::Optional,
            ),
        }
    }

    /// Drop `var` from every list: a name the session may not fetch.
    fn forget(&mut self, var: &str) {
        for list in [
            &mut self.granted,
            &mut self.ondemand,
            &mut self.launch_required,
            &mut self.launch_optional,
        ] {
            list.retain(|name| name != var);
        }
    }

    /// A name fetched around one child (`ondemand`) is never also exported
    /// into the pod's whole environment at launch.
    fn keep_ondemand_out_of_launch(&mut self) {
        let ondemand = &self.ondemand;
        self.launch_required.retain(|var| !ondemand.contains(var));
        self.launch_optional.retain(|var| !ondemand.contains(var));
    }
}

fn launch_names(
    manifest: &[crate::secret_broker::ManifestSecret],
    policy: crate::secret_broker::ManifestPolicy,
) -> Vec<String> {
    manifest
        .iter()
        .filter(|entry| entry.policy == policy)
        .map(|entry| entry.var.clone())
        .collect()
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
        (
            "AGENT_AUTH_LAUNCH_REQUIRED",
            breadcrumbs.launch_required.join(" "),
        ),
        (
            "AGENT_AUTH_LAUNCH_OPTIONAL",
            breadcrumbs.launch_optional.join(" "),
        ),
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
        self.sources.lock().insert(id, source);
    }

    pub async fn take_source(&self, id: Uuid) -> Option<SourceDescriptor> {
        self.sources.lock().remove(&id)
    }

    pub async fn issue(&self, id: Uuid, token: &[u8]) {
        self.pending.lock().insert(
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
        self.pending.lock().remove(&id);
        self.launches.lock().remove(&id);
        self.source_credentials.lock().remove(&id);
        self.graft_sessions.lock().remove(&id);
    }
    /// Hold the core session id the session's graft token is bound to, for
    /// the bootstrap response that writes the token into the pod.
    pub async fn issue_graft_session(&self, id: Uuid, session_ref: String) {
        self.graft_sessions.lock().insert(id, session_ref);
    }
    pub async fn take_graft_session(&self, id: Uuid) -> Option<String> {
        self.graft_sessions.lock().remove(&id)
    }
    /// Hold what the session spec asked the pod to carry, for the bootstrap
    /// response to take once the pod presents its credential.
    pub async fn issue_launch(&self, id: Uuid, launch: PodLaunch) {
        self.launches.lock().insert(id, launch);
    }
    pub async fn take_launch(&self, id: Uuid) -> Option<PodLaunch> {
        self.launches.lock().remove(&id)
    }
    /// Whether a bootstrap credential is held for `id`: issued, and neither
    /// consumed nor withdrawn.
    pub async fn bootstrap_pending(&self, id: Uuid) -> bool {
        self.pending.lock().contains_key(&id)
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
        let mut held = self.source_credentials.lock();
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
        let g = self.source_credentials.lock().remove(&id)?;
        (g.expires > Instant::now()).then_some(g.handle)
    }
    /// Whether `candidate` is the unspent bootstrap credential for `id` —
    /// the read-only counterpart of [`Self::consume`]. The workspace
    /// snapshot's init container presents the same credential to fetch its
    /// tarball before the session container's one bootstrap request takes it
    /// (WI-4127), so that fetch must not spend it.
    pub async fn bootstrap_ok(&self, id: Uuid, candidate: &[u8]) -> bool {
        let pending = self.pending.lock();
        pending.get(&id).is_some_and(|entry| {
            entry.expires > Instant::now() && bool::from(entry.token.as_slice().ct_eq(candidate))
        })
    }
    pub(crate) async fn consume(&self, id: Uuid, candidate: &[u8]) -> bool {
        let mut p = self.pending.lock();
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
        self.sessions.lock().insert(
            id,
            SessionCredential {
                token: token.into_bytes(),
            },
        );
        true
    }
    async fn attach(&self, id: Uuid, tx: mpsc::Sender<Frame>, link: Arc<PodLink>) {
        self.links.lock().insert(id, (tx, Arc::clone(&link)));
        link.note_connected();
    }
    /// Drop the link, unless a newer connection has already replaced it. A
    /// dropped socket marks the link disconnected, so a redial is judged by
    /// the reconnect timeout rather than the heartbeat deadline.
    async fn detach(&self, id: Uuid, tx: &mpsc::Sender<Frame>) {
        let mut links = self.links.lock();
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
        self.pods.lock().insert(id, Arc::clone(&link));
        let reader = PodReader {
            rx,
            pending: Vec::new(),
            at: 0,
            end: 0,
            link: Some(Arc::clone(&link)),
        };
        (link, reader)
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
        let (link, mut reader) = self.register_pod(id);
        *link.next_offset.lock() = cursor;
        // The reader starts at the cursor, so its first checkpoint is the
        // stream offset the log already holds, not zero.
        reader.end = cursor;
        link.mark_durable(cursor);
        link.adopted.store(true, Ordering::Release);
        self.sessions.lock().insert(
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

    /// Ask the agent to salvage its checkout and wait for the answer. The
    /// archive itself arrives on the upload route, which records it; this is
    /// only the ordering a stop needs (WI-4132).
    pub async fn request_salvage(&self, id: Uuid, limit: Duration) -> SalvageOutcome {
        let Some(link) = self.pod(id) else {
            return SalvageOutcome::NotAttached;
        };
        let before = link.salvages.load(Ordering::Acquire);
        // Clear a stale failure so this request cannot return the last one.
        *link.salvage_failure.lock() = None;
        if self.send(id, Frame::Salvage).is_err() {
            return SalvageOutcome::NotAttached;
        }
        let wait = async {
            loop {
                let notified = link.salvage_notify.notified();
                if link.salvages.load(Ordering::Acquire) > before {
                    return match link.salvage_failure.lock().clone() {
                        Some(reason) => SalvageOutcome::Failed(reason),
                        None => SalvageOutcome::Done,
                    };
                }
                notified.await;
            }
        };
        tokio::time::timeout(limit, wait).await.unwrap_or_else(|_| {
            SalvageOutcome::Failed(
                "the pod agent did not answer the salvage request in time".into(),
            )
        })
    }

    /// Forget the stream, unless a newer registration has replaced `link`.
    pub fn forget_pod_if(self: &Arc<Self>, id: Uuid, link: &Arc<PodLink>) {
        let current = self.pod(id);
        if current.is_some_and(|c| Arc::ptr_eq(&c, link)) {
            self.forget_pod(id);
        }
    }

    pub fn pod(&self, id: Uuid) -> Option<Arc<PodLink>> {
        self.pods.lock().get(&id).cloned()
    }

    /// Sessions that have a pod link registered, finished or not.
    pub fn pod_ids(&self) -> Vec<Uuid> {
        self.pods.lock().keys().copied().collect()
    }

    /// Forget a pod session's stream and the credentials held for it. Every
    /// map is a `parking_lot` mutex never held across an `.await`, so the
    /// removal is always immediate and the old try_lock-else-spawn path
    /// (WI-4290) is not needed.
    pub fn forget_pod(self: &Arc<Self>, id: Uuid) {
        self.pods.lock().remove(&id);
        self.links.lock().remove(&id);
        self.sessions.lock().remove(&id);
        self.source_credentials.lock().remove(&id);
        self.launches.lock().remove(&id);
    }
    /// Queue a frame for the attached agent without blocking. A full queue
    /// sheds a `Resize` (the next one supersedes it) and fails every other
    /// frame with `WouldBlock`, so a control message is never silently lost.
    pub fn send(&self, id: Uuid, frame: Frame) -> std::io::Result<()> {
        let result = self
            .links
            .lock()
            .get(&id)
            .map(|(tx, _)| try_queue(tx, frame))
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotConnected,
                    "session-agent is not attached",
                )
            })?;
        result
    }
    async fn session_ok(&self, id: Uuid, candidate: &[u8]) -> bool {
        self.sessions
            .lock()
            .get(&id)
            .is_some_and(|c| bool::from(c.token.as_slice().ct_eq(candidate)))
    }
    /// Which pod session holds this stream credential, if any. Constant-time
    /// per entry, like the other credential checks here.
    pub async fn session_for_token(&self, candidate: &[u8]) -> Option<Uuid> {
        self.sessions
            .lock()
            .iter()
            .find(|(_, c)| bool::from(c.token.as_slice().ct_eq(candidate)))
            .map(|(id, _)| *id)
    }
    pub async fn session_token(&self, id: Uuid) -> Option<String> {
        self.sessions
            .lock()
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
    work_dirs: &[&str],
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
    // The workspace root too: a `CLAUDE.md` there imports from outside its
    // own directory, so the approval must not depend on the cwd alone.
    for dir in work_dirs {
        changed |= crate::claude_config::accept_in_config(&mut value, dir);
    }
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

/// The account bundle a pod resumes from: the login, the global config, a
/// woken session's transcript, and project memory when it is on. Returns the
/// bundle and whether it carries the session's conversation.
fn pod_bundle(
    state: &AppState,
    cli: &str,
    account: Option<&str>,
    conversation: Option<&str>,
    source: &vogt_engine_contract::source::SourceDescriptor,
    onboarding_dirs: &[&str],
    pod_work_dir: &str,
) -> (serde_json::Map<String, serde_json::Value>, bool) {
    let mut bundle = serde_json::Map::new();
    let mut resume = false;
    // The login travels whenever the session holds an account: an agent-task
    // session has no conversation pinned yet, but still needs to be signed in
    // (WI-3334). A Codex session never holds a Claude account (the engine
    // refuses one for a non-Claude CLI), but its rollout belongs to the
    // conversation, not an account: snapshot it with an empty account or a
    // wake would seed nothing and start fresh (WI-3873).
    let snapshot_account = account.or_else(|| (cli == "codex").then_some(""));
    if let Some(account) = snapshot_account {
        let files = state
            .sessions
            .secret_broker()
            .bundle_snapshot(account, conversation.unwrap_or(""));
        for (path, bytes) in with_onboarding_config(files, onboarding_dirs) {
            // Claude keeps `<conversation>.jsonl`; a Codex rollout is
            // `rollout-<stamp>-<conversation>.jsonl`. Both must count, or a
            // woken Codex pod starts a fresh conversation and loses its
            // transcript (WI-3873).
            resume |= conversation.is_some_and(|c| resumes_conversation(&path, c));
            bundle.insert(
                path,
                serde_json::Value::String(base64::Engine::encode(
                    &base64::engine::general_purpose::STANDARD,
                    bytes,
                )),
            );
        }
    }
    // Project memory is keyed by the project, not the pod's path or the
    // leased account, and restored only at launch (WI-3371). Off unless the
    // deployment turned it on.
    if crate::pod_salvage::persist_memory_enabled() {
        for (path, bytes) in crate::pod_salvage::memory_bundle(
            state.sessions.state_dir(),
            source.location.repo(),
            pod_work_dir,
        ) {
            bundle.insert(
                path,
                serde_json::Value::String(base64::Engine::encode(
                    &base64::engine::general_purpose::STANDARD,
                    bytes,
                )),
            );
        }
    }
    (bundle, resume)
}

struct PodConfigInputs<'a> {
    engine_endpoint: &'a str,
    vogt_prod_bearer: bool,
    cadastre_bearer: bool,
    providers: &'a [crate::llm_providers::Provider],
}

/// The per-CLI MCP and provider config a pod is handed, plus the credential
/// variables that go with it (inserted into `env`).
async fn pod_agent_configs(
    state: &AppState,
    req: &BootstrapRequest,
    id: Uuid,
    mcp_set: crate::mcp_set::McpSet,
    mcp_extra: &[String],
    inputs: &PodConfigInputs<'_>,
    env: &mut serde_json::Map<String, serde_json::Value>,
) -> (
    Option<serde_json::Value>,
    Option<String>,
    Option<serde_json::Value>,
) {
    let (mut mcp_config, mut codex_config, mut opencode_config) = (None, None, None);
    if matches!(req.cli.as_str(), "claude" | "codex" | "opencode") {
        let token = state.sessions.mint_agent_token(&req.cli, id).await;
        let endpoints = state.sessions.pod_mcp_endpoints(
            inputs.engine_endpoint,
            inputs.vogt_prod_bearer,
            mcp_set,
            mcp_extra,
        );
        let has = token.is_some();
        let wants = |cli: &str| req.cli == cli || (req.shell && cli != "opencode");
        if wants("claude") {
            mcp_config = Some(crate::session_endpoints::claude_mcp_config(
                &endpoints,
                has,
                inputs.cadastre_bearer,
            ));
        }
        if wants("codex") {
            codex_config = Some(crate::session_endpoints::codex_mcp_config(
                &endpoints,
                has,
                inputs.cadastre_bearer,
            ));
        }
        if req.cli == "opencode" {
            opencode_config = Some(crate::session_endpoints::opencode_mcp_config(
                &endpoints,
                has,
                inputs.cadastre_bearer,
            ));
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
    if (req.cli == "codex" || req.shell) && !inputs.providers.is_empty() {
        codex_config = Some(
            crate::llm_providers::codex_config(req.model.as_deref(), inputs.providers)
                + "\n"
                + &codex_config.unwrap_or_default(),
        );
    }
    // WI-4296: an opencode pod gets them as its config's `provider` block,
    // each key named `{env:KEY}`; without one opencode starts signed out on
    // its own free default, whatever model the session asked for.
    if req.cli == "opencode" {
        if let (Some(serde_json::Value::Object(config)), serde_json::Value::Object(extra)) = (
            opencode_config.as_mut(),
            crate::llm_providers::opencode_provider_config(inputs.providers),
        ) {
            config.extend(extra);
        }
    }
    (mcp_config, codex_config, opencode_config)
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
    // The pod's own workspace mount, where its CLAUDE.md imports resolve, not
    // the engine's workspace root, a path the pod never sees (WI-2000).
    let workspace_root = cfg
        .workspace
        .as_ref()
        .map(|workspace| workspace.dir.clone())
        .unwrap_or_else(|| crate::pod_backend::DEFAULT_WORKSPACE_DIR.to_string());
    let Some(source) = state.agent_auth.take_source(id).await else {
        return (StatusCode::BAD_REQUEST, "session source is unavailable").into_response();
    };
    let (registry, provider) = match state.sessions.source_registry().and_then(|r| {
        let p = r
            .for_source(&source)
            .map_err(|e| crate::error::ApiError::Config(e.to_string()))?;
        Ok((r, p))
    }) {
        Ok(rp) => rp,
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
    // WI-4296: an opencode pod reaches the same providers by the same rule.
    let providers = if matches!(req.cli.as_str(), "codex" | "opencode") {
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
    // to answer 401. Every supported agent fetches the value around its final
    // child: the name joins the ondemand breadcrumbs and the session agent
    // resolves it, never into the pod's environment or the config.
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
    if cadastre_bearer && !cadastre_named {
        breadcrumbs
            .ondemand
            .push(crate::session_endpoints::CADASTRE_TOKEN_ENV.to_string());
    }
    // WI-4289: the remote Vogt (`vogt-prod`) follows the cadastre rule, and
    // is offered only when this engine names one. Its token is never in the
    // pod's environment: the name joins the ondemand breadcrumbs and the
    // session agent fetches it around the agent.
    // WI-4489: a bootstrap whose launch details are missing fails closed, on
    // the worker set, rather than open on the full one.
    let launch = state
        .agent_auth
        .take_launch(id)
        .await
        .unwrap_or_else(|| PodLaunch {
            mcp_set: crate::mcp_set::McpSet::Worker,
            ..PodLaunch::default()
        });
    // WI-4489: and only to a full-set session; a worker never gets it, so its
    // token is not fetched around a worker's agent either. A dev lane hands
    // it to nobody.
    if launch.mcp_set != crate::mcp_set::McpSet::Full || state.sessions.is_dev_lane() {
        breadcrumbs.forget(crate::session_endpoints::VOGT_PROD_TOKEN_ENV);
    }
    let vogt_prod_bearer = state.sessions.vogt_prod_configured()
        && launch.mcp_set == crate::mcp_set::McpSet::Full
        && (broker
            .manifest()
            .iter()
            .any(|entry| entry.var == crate::session_endpoints::VOGT_PROD_TOKEN_ENV)
            || breadcrumbs
                .granted
                .iter()
                .any(|var| var == crate::session_endpoints::VOGT_PROD_TOKEN_ENV));
    if vogt_prod_bearer
        && !breadcrumbs
            .ondemand
            .iter()
            .any(|var| var == crate::session_endpoints::VOGT_PROD_TOKEN_ENV)
    {
        breadcrumbs
            .ondemand
            .push(crate::session_endpoints::VOGT_PROD_TOKEN_ENV.to_string());
    }
    breadcrumbs.keep_ondemand_out_of_launch();
    let mut env = bootstrap_env(
        cfg.engine_endpoint,
        id,
        &req.cli,
        req.model.as_deref(),
        &registry.pod_env(provider.as_ref(), &source),
        req.extra_ca_file.as_deref(),
        &breadcrumbs,
    );
    // The caller's extra environment and working directory (WI-3883) travel
    // here rather than in the pod spec, so a value never lands in the
    // Kubernetes object. The environment stays apart from `env` so the
    // entrypoint cannot export it into its own shell: it is handed to the
    // agent at the final `exec`, after the clone that spends the source
    // token, or a caller's `GRAFT_ENDPOINT` would receive that token.
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
    let (mcp_config, codex_config, opencode_config) = pod_agent_configs(
        &state,
        &req,
        id,
        launch.mcp_set,
        &launch.mcp_extra,
        &PodConfigInputs {
            engine_endpoint: &engine_endpoint,
            vogt_prod_bearer,
            cadastre_bearer,
            providers: &providers,
        },
        &mut env,
    )
    .await;
    // A pod resumes from the account bundle the engine holds: the login, the
    // global config and, for a woken session, its conversation's transcript.
    let crate::sessions::PodAccountConversation {
        account,
        conversation,
    } = state.sessions.pod_account_conversation(id);
    let (bundle, resume) = pod_bundle(
        &state,
        &req.cli,
        account.as_deref(),
        conversation.as_deref(),
        &source,
        &[pod_work_dir.as_str(), workspace_root.as_str()],
        &pod_work_dir,
    );
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
        session_token: session_token.into(),
        bundle: serde_json::Value::Object(bundle),
        env: serde_json::Value::Object(env),
        agent_env,
        source,
        source_token: source_token.map(Into::into),
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
    let (tx, mut commands) = mpsc::channel(AGENT_COMMAND_QUEUE);
    agent_auth.attach(id, tx.clone(), Arc::clone(&link)).await;
    // A stop asked for while no agent was attached is delivered on arrival.
    if link.stop_requested() {
        let _ = tx.try_send(Frame::Signal {
            signal: "kill".into(),
        });
    }
    let mut resync = ResyncGate::default();
    // Output is acknowledged only once the log holds it: `owed` is the offset
    // the agent is waiting on, and `durable` wakes the loop as the log catches up.
    let mut durable = link.durable_watch();
    let mut owed: Option<u64> = None;
    loop {
        tokio::select! {
            Some(frame) = commands.recv() => {
                if !send_frame(&mut socket, &frame).await { break; }
            }
            Ok(()) = durable.changed(), if owed.is_some() => {
                let due = owed.unwrap_or_default();
                if link.durable_offset() >= due {
                    owed = None;
                    if !send_frame(&mut socket, &Frame::Ack { offset: due }).await { break; }
                } else if link.reader_stopped() {
                    // Nothing more will be written, so the owed bytes never will be.
                    let _ = send_frame(&mut socket, &Frame::Error {
                        code: "output_not_written".into(),
                        message: format!("the engine stopped writing before offset {due}"),
                    }).await;
                    break;
                }
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
                            // Tell the agent how much it can discard: what the log
                            // holds now, and the rest once it is written.
                            let ack = link.durable_offset().min(next);
                            let due = owed.map_or(next, |o| o.max(next));
                            owed = (due > ack).then_some(due);
                            if !send_frame(&mut socket, &Frame::Ack { offset: ack }).await { break; }
                        }
                        Frame::Output { offset, data } => {
                            // Output is proof the agent is alive, not only its
                            // Heartbeat: a busy agent may never send one.
                            link.note_heartbeat();
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
                                let due = owed.map_or(next, |o| o.max(next));
                                owed = Some(due);
                                if link.durable_offset() >= due {
                                    owed = None;
                                    if !send_frame(&mut socket, &Frame::Ack { offset: due }).await { break; }
                                }
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
                            link.note_heartbeat();
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
                                let due = owed.map_or(next, |o| o.max(next));
                                owed = Some(due);
                                if link.durable_offset() >= due {
                                    owed = None;
                                    if !send_frame(&mut socket, &Frame::Ack { offset: due }).await { break; }
                                }
                            }
                            Err(gap) => {
                                if let Some(offset) = resync.refused(&gap, link.output_closed()) {
                                    if !send_frame(&mut socket, &Frame::Resync { offset }).await { break; }
                                }
                            }
                            }
                        }
                        Frame::Flushed { ok } => {
                            link.note_heartbeat();
                            let counter = if ok { &link.flushes } else { &link.flush_failures };
                            counter.fetch_add(1, Ordering::AcqRel);
                            link.flush_notify.notify_waiters();
                        }
                        Frame::Salvaged { ok, reason, .. } => {
                            link.note_heartbeat();
                            *link.salvage_failure.lock() =
                                (!ok).then(|| {
                                    reason.unwrap_or_else(|| "the pod agent could not save the working tree".into())
                                });
                            link.salvages.fetch_add(1, Ordering::AcqRel);
                            link.salvage_notify.notify_waiters();
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
    #[test]
    fn bootstrap_response_hides_tokens_in_debug_but_serializes_them() {
        let r = BootstrapResponse {
            session_token: "sess-tok-123".to_string().into(),
            bundle: serde_json::json!({}),
            env: serde_json::json!({}),
            agent_env: Default::default(),
            source: vogt_engine_contract::source::SourceDescriptor {
                location: vogt_engine_contract::source::SourceLocation::Graft {
                    endpoint: "http://g".into(),
                    repo: "r".into(),
                },
                reference: "main".into(),
                landing: Default::default(),
            },
            brief: String::new(),
            source_token: Some("src-tok-456".to_string().into()),
            graft_session: None,
            mcp_config: None,
            codex_config: None,
            opencode_config: None,
        };
        let shown = format!("{r:?}");
        assert!(!shown.contains("sess-tok-123") && !shown.contains("src-tok-456"));
        let wire = serde_json::to_value(&r).unwrap();
        assert_eq!(wire["session_token"], "sess-tok-123");
        assert_eq!(wire["source_token"], "src-tok-456");
    }

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

    /// WI-4418: a busy agent streams output and never sends a Heartbeat, and
    /// its output alone keeps the pod alive. An agent that sends nothing still
    /// goes stale.
    #[tokio::test]
    async fn output_alone_keeps_a_busy_agent_alive_and_silence_does_not() {
        let timeout = Duration::from_millis(200);
        let auth = Arc::new(AgentAuth::default());
        let busy = Uuid::new_v4();
        let (busy_link, _busy_reader) = auth.register_pod(busy);
        let agent = connect(&auth, busy).await;
        // Three heartbeat timeouts of output, with no Heartbeat frame.
        let started = tokio::time::Instant::now();
        let mut offset = 0u64;
        while started.elapsed() < timeout * 3 {
            agent.send(Frame::Output {
                offset,
                data: b"a".to_vec(),
            });
            offset += 1;
            tokio::time::sleep(Duration::from_millis(20)).await;
            assert!(
                busy_link.heartbeat_age() < timeout,
                "busy agent judged silent after {:?}",
                busy_link.heartbeat_age()
            );
        }

        let silent = Uuid::new_v4();
        let (silent_link, _silent_reader) = auth.register_pod(silent);
        let _quiet = connect(&auth, silent).await;
        tokio::time::sleep(timeout * 3).await;
        assert!(silent_link.heartbeat_age() > timeout);
    }

    /// One read of a session reader, on the blocking pool, handed back with
    /// the bytes it returned.
    async fn read_once(mut reader: PodReader) -> (PodReader, Vec<u8>) {
        tokio::task::spawn_blocking(move || {
            let mut buf = [0u8; 64];
            let n = reader.read(&mut buf).unwrap();
            (reader, buf[..n].to_vec())
        })
        .await
        .unwrap()
    }

    fn acks(frames: &[Frame]) -> Vec<u64> {
        frames
            .iter()
            .filter_map(|frame| match frame {
                Frame::Ack { offset } => Some(*offset),
                _ => None,
            })
            .collect()
    }

    /// WI-4442: the engine acknowledges output only once the log holds it.
    /// It dies after taking output from the agent but before writing it. The
    /// agent never saw an ack for those bytes, so the next engine's cursor is
    /// the last logged byte, the agent resends from there, and the log has no
    /// gap and no duplicate.
    #[tokio::test]
    async fn an_ack_waits_for_the_log_write_so_a_kill_in_the_window_loses_nothing() {
        let auth = Arc::new(AgentAuth::default());
        let id = Uuid::new_v4();
        let (_link, reader) = auth.register_pod(id);
        let mut agent = connect(&auth, id).await;
        agent.send(Frame::Output {
            offset: 0,
            data: b"hello".to_vec(),
        });
        let (frames, _) = agent.drain().await;
        assert!(
            acks(&frames).is_empty(),
            "output acked before the log has it: {frames:?}"
        );

        // The session reader takes "hello", writes it, and reads again: that
        // read is what marks "hello" durable, and the agent hears the ack.
        let (reader, taken) = read_once(reader).await;
        assert_eq!(taken, b"hello");
        let next = tokio::spawn(read_once(reader));
        let (frames, _) = agent.drain().await;
        assert_eq!(acks(&frames), vec![5]);

        // The agent sends "world". The reader takes it and dies before it
        // writes it.
        agent.send(Frame::Output {
            offset: 5,
            data: b"world".to_vec(),
        });
        let (reader, taken) = next.await.unwrap();
        assert_eq!(taken, b"world");
        drop(reader);
        let (frames, closed) = agent.drain().await;
        assert!(
            acks(&frames).is_empty(),
            "an ack for bytes never written: {frames:?}"
        );
        assert!(
            closed,
            "the relay ends when its output can never be written"
        );

        // The next engine adopts the pod at the log's end, "hello". The agent
        // resumes from its last ack, resends "world", and it lands once.
        let restarted = Arc::new(AgentAuth::default());
        let (_adopted, reader) = restarted.adopt_pod(id, 5, "token".into()).await;
        let mut agent = connect(&restarted, id).await;
        agent.send(Frame::Resume { cursor: 5 });
        agent.send(Frame::Output {
            offset: 5,
            data: b"world".to_vec(),
        });
        let (reader, taken) = read_once(reader).await;
        assert_eq!(taken, b"world", "the resent bytes arrive exactly once");
        let next = tokio::spawn(read_once(reader));
        let (frames, _) = agent.drain().await;
        assert_eq!(acks(&frames), vec![5, 10]);
        // Close the link so the blocked read returns: the runtime waits for it.
        restarted.pod(id).unwrap().finish(0);
        next.await.unwrap();
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
    async fn a_salvage_request_reaches_the_agent_and_ok_is_done() {
        let auth = Arc::new(AgentAuth::default());
        let id = Uuid::new_v4();
        let (_link, _reader) = auth.register_pod(id);
        let mut agent = connect(&auth, id).await;
        let requester = {
            let auth = Arc::clone(&auth);
            tokio::spawn(async move { auth.request_salvage(id, Duration::from_secs(2)).await })
        };
        let (frames, _) = agent.drain().await;
        assert_eq!(frames, vec![Frame::Salvage]);
        agent.send(Frame::Salvaged {
            ok: true,
            files: 2,
            reason: None,
        });
        assert_eq!(requester.await.unwrap(), SalvageOutcome::Done);
    }

    #[tokio::test]
    async fn a_salvage_failure_carries_its_reason() {
        let auth = Arc::new(AgentAuth::default());
        let id = Uuid::new_v4();
        let (_link, _reader) = auth.register_pod(id);
        let mut agent = connect(&auth, id).await;
        let requester = {
            let auth = Arc::clone(&auth);
            tokio::spawn(async move { auth.request_salvage(id, Duration::from_secs(2)).await })
        };
        let (frames, _) = agent.drain().await;
        assert_eq!(frames, vec![Frame::Salvage]);
        agent.send(Frame::Salvaged {
            ok: false,
            files: 0,
            reason: Some("the checkout could not be read".into()),
        });
        assert_eq!(
            requester.await.unwrap(),
            SalvageOutcome::Failed("the checkout could not be read".into())
        );
    }

    #[tokio::test]
    async fn a_salvage_without_an_attached_pod_is_not_attached() {
        let auth = AgentAuth::default();
        assert_eq!(
            auth.request_salvage(Uuid::new_v4(), Duration::from_millis(10))
                .await,
            SalvageOutcome::NotAttached
        );
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
        assert_eq!(receiver.recv().unwrap(), (1, b"a".to_vec()));
        assert_eq!(link.ingest(1, b"b"), Ok(2));
        assert_eq!(receiver.recv().unwrap(), (2, b"b".to_vec()));
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
            (lost, format!("\r\n[{lost} bytes lost]\r\n").into_bytes())
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
            launch_required: vec!["FORGEJO_TOKEN".into()],
            launch_optional: vec!["GH_TOKEN".into(), "GITHUB_MCP_TOKEN".into()],
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
        assert_eq!(env["AGENT_AUTH_LAUNCH_REQUIRED"], "FORGEJO_TOKEN");
        assert_eq!(
            env["AGENT_AUTH_LAUNCH_OPTIONAL"],
            "GH_TOKEN GITHUB_MCP_TOKEN"
        );
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

    /// WI-4517: a pod is told the launch-time names (required and optional
    /// apart) to fetch through the broker; never an ondemand name, which is
    /// fetched around its child only, and never one the session may not have.
    #[test]
    fn launch_names_are_the_manifests_launch_time_entries_only() {
        let manifest = crate::secret_broker::parse_manifest(
            "FORGEJO_TOKEN p forgejo\nGH_TOKEN p gh optional\nDB p db ondemand\nCADASTRE p c optional\nVOGT_PROD_TOKEN p v optional\n",
        )
        .unwrap();
        use crate::secret_broker::ManifestPolicy;
        let mut crumbs = AuthBreadcrumbs {
            granted: vec!["VOGT_PROD_TOKEN".into()],
            ondemand: vec!["DB".into(), "CADASTRE".into()],
            writable: Vec::new(),
            launch_required: launch_names(&manifest, ManifestPolicy::Required),
            launch_optional: launch_names(&manifest, ManifestPolicy::Optional),
        };
        assert_eq!(crumbs.launch_required, ["FORGEJO_TOKEN"]);
        assert_eq!(
            crumbs.launch_optional,
            ["GH_TOKEN", "CADASTRE", "VOGT_PROD_TOKEN"]
        );
        crumbs.forget("VOGT_PROD_TOKEN");
        crumbs.keep_ondemand_out_of_launch();
        assert_eq!(crumbs.launch_required, ["FORGEJO_TOKEN"]);
        assert_eq!(crumbs.launch_optional, ["GH_TOKEN"]);
        assert!(crumbs.granted.is_empty());
        assert_eq!(crumbs.ondemand, ["DB", "CADASTRE"]);
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
        let auth = Arc::new(AgentAuth::default());
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
        let (tx, mut rx) = mpsc::channel(8);
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
        let (old, _old_rx) = mpsc::channel(8);
        let (new, mut new_rx) = mpsc::channel(8);
        auth.attach(id, old.clone(), Arc::clone(&link)).await;
        auth.attach(id, new, link).await;
        auth.detach(id, &old).await;
        auth.send(id, Frame::Ack { offset: 3 }).unwrap();
        assert_eq!(new_rx.recv().await, Some(Frame::Ack { offset: 3 }));
    }

    #[test]
    fn the_agent_command_queue_is_bounded_and_never_drops_a_control_frame() {
        let auth = AgentAuth::default();
        let id = Uuid::new_v4();
        let (link, _reader) = auth.register_pod(id);
        let (tx, mut rx) = mpsc::channel(2);
        futures_util::FutureExt::now_or_never(auth.attach(id, tx, link)).unwrap();
        auth.send(id, Frame::Ack { offset: 1 }).unwrap();
        auth.send(id, Frame::Ack { offset: 2 }).unwrap();
        // Full: a control frame fails rather than queueing without bound.
        let err = auth.send(id, Frame::Ack { offset: 3 }).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::WouldBlock);
        // A resize is shed (superseded by the next), not an error.
        auth.send(id, Frame::Resize { cols: 1, rows: 1 }).unwrap();
        assert_eq!(rx.try_recv().unwrap(), Frame::Ack { offset: 1 });
        assert_eq!(rx.try_recv().unwrap(), Frame::Ack { offset: 2 });
        assert!(rx.try_recv().is_err());
        // Room again: sends succeed.
        auth.send(id, Frame::Ack { offset: 3 }).unwrap();
    }

    #[test]
    fn a_panic_under_one_sessions_lock_does_not_take_down_others() {
        let auth = Arc::new(AgentAuth::default());
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let (link_a, _ra) = auth.register_pod(a);
        let (link_b, _rb) = auth.register_pod(b);
        // Panic while holding the registry, a link and the pod table locks.
        let (auth2, link2) = (Arc::clone(&auth), Arc::clone(&link_a));
        let crashed = std::thread::spawn(move || {
            let _links = auth2.links.lock();
            let _pods = auth2.pods.lock();
            let _exit = link2.exit.lock();
            let _off = link2.next_offset.lock();
            panic!("boom");
        })
        .join();
        assert!(crashed.is_err());
        // Every other caller still works; nothing cascades.
        assert_eq!(link_b.ingest(0, b"ok"), Ok(2));
        assert_eq!(link_a.ingest(0, b"ok"), Ok(2));
        assert!(!link_a.is_finished());
        assert!(auth.pod(b).is_some());
        auth.forget_pod(a);
        assert!(auth.pod(a).is_none());
        assert!(auth.send(b, Frame::Ack { offset: 1 }).is_err());
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
            end: 0,
            link: None,
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
            end: 0,
            link: None,
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

    /// WI-2000: an alternate account's bundle gets trust and the external
    /// import approval for the checkout and for the workspace root.
    #[test]
    fn the_bundle_config_approves_every_given_directory() {
        let files = vec![(".credentials.json".to_string(), b"{}".to_vec())];
        let files = with_onboarding_config(files, &["/work", "/home/a/Working"]);
        let config = files.iter().find(|(p, _)| p == ".claude.json").unwrap();
        let config: serde_json::Value = serde_json::from_slice(&config.1).unwrap();
        for dir in ["/work", "/home/a/Working"] {
            let project = &config["projects"][dir];
            assert_eq!(project["hasTrustDialogAccepted"], true, "{dir}: {config}");
            assert_eq!(
                project["hasClaudeMdExternalIncludesApproved"], true,
                "{dir}: {config}"
            );
        }
    }

    /// Drives a session reader the way `pty.rs` does: take the stream's
    /// output, strip the OSC 7501 queries, append what is left to the log.
    fn drive_session_log(
        mut reader: PodReader,
        log: std::path::PathBuf,
    ) -> tokio::task::JoinHandle<()> {
        tokio::task::spawn_blocking(move || {
            let mut file = std::fs::OpenOptions::new().append(true).open(log).unwrap();
            let mut buf = [0u8; 8192];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                let kept = crate::osc7501::strip_queries(&buf[..n]);
                std::io::Write::write_all(&mut file, &kept).unwrap();
            }
        })
    }

    /// WI-4453: a log with a gap marker and a stripped OSC 7501 query is
    /// shorter than its stream offset, so the log length is the wrong resume
    /// point. Crash with a byte written to the log but never acknowledged, then
    /// readopt: the engine resumes at the stream offset, drops the unacked
    /// byte, and the replayed overlap lands once. The log is exact bytes.
    #[tokio::test]
    async fn a_restart_resumes_at_the_stream_offset_not_the_log_length() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("session.log");
        std::fs::write(&log, b"").unwrap();
        let auth = Arc::new(AgentAuth::default());
        let id = Uuid::new_v4();
        let (link, reader) = auth.register_pod(id);
        link.keep_log(log.clone());
        let session = drive_session_log(reader, log.clone());
        let agent = connect(&auth, id).await;

        // Stream: 18 bytes, then 4 the agent lost, then 20 more (42 in all).
        // The log takes 9 + an 18-byte marker + 10 = 37 of them.
        let first = b"alpha\x1b]7501;?\x07beta".to_vec();
        let second = b"gamma\x1b]7501;?\x1b\\delta".to_vec();
        let third = b"epsilon".to_vec();
        agent.send(Frame::Output {
            offset: 0,
            data: first,
        });
        agent.send(Frame::Gap {
            offset: 18,
            lost: 4,
        });
        agent.send(Frame::Output {
            offset: 22,
            data: second.clone(),
        });
        link.durable_watch().wait_for(|v| *v >= 42).await.unwrap();
        let before_crash = b"alphabeta\r\n[4 bytes lost]\r\ngammadelta";
        assert_eq!(std::fs::read(&log).unwrap(), before_crash);

        // The engine dies after the log took "epsilon" but before any ack.
        link.crash();
        link.finish(0);
        session.await.unwrap();
        let mut log_file = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
        std::io::Write::write_all(&mut log_file, &third).unwrap();
        drop(log_file);

        // The next engine resumes at the checkpoint, 42, and cuts the log back to it.
        let cursor = restore_resume_point(&log).unwrap();
        assert_eq!(cursor, 42);
        assert_eq!(std::fs::read(&log).unwrap(), before_crash);

        let restarted = Arc::new(AgentAuth::default());
        let (restarted_link, reader) = restarted.adopt_pod(id, cursor, "token".into()).await;
        restarted_link.keep_log(log.clone());
        let session = drive_session_log(reader, log.clone());
        let agent = connect(&restarted, id).await;
        agent.send(Frame::Resume { cursor });
        // The agent replays from 22, which overlaps what the log holds, then
        // carries on with "epsilon". The overlap is dropped; "epsilon" lands once.
        let mut replay = second;
        replay.extend_from_slice(&third);
        agent.send(Frame::Output {
            offset: 22,
            data: replay,
        });
        restarted_link
            .durable_watch()
            .wait_for(|v| *v >= 49)
            .await
            .unwrap();
        restarted_link.finish(0);
        session.await.unwrap();

        assert_eq!(
            std::fs::read(&log).unwrap(),
            b"alphabeta\r\n[4 bytes lost]\r\ngammadeltaepsilon"
        );
    }
}
