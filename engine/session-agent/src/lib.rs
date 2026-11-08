//! The pod-side Vogt session agent.
//!
//! The agent owns the PTY and makes the connection to the engine; a session pod
//! never listens for an engine connection. See the crate README for the wire
//! protocol.

pub mod codex_policy;

use std::{
    collections::VecDeque,
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use futures_util::{SinkExt, StreamExt};
use portable_pty::{CommandBuilder, PtySize};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::{
    connect_async_tls_with_config,
    tungstenite::{client::IntoClientRequest, http::HeaderValue, Message},
    Connector,
};

pub const DEFAULT_BUFFER_BYTES: usize = 1024 * 1024;
/// The most data one replayed `Output` frame carries.
pub const RESEND_CHUNK_BYTES: usize = 64 * 1024;
pub const BUNDLE_DEBOUNCE: Duration = Duration::from_secs(1);

/// The only files a Claude account bundle may receive from a pod.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BundleFileKind {
    Credentials,
    Config,
    Transcript,
}

#[derive(Debug, Clone)]
pub struct BundleFile {
    pub kind: BundleFileKind,
    pub path: PathBuf,
    pub bytes: Vec<u8>,
}

/// Select the whole-file bundle files (credentials, global config) without
/// following a path outside the configured Claude directory. Both are
/// account-scoped; the transcript travels separately, see `transcript_path`.
pub fn bundle_files(root: &Path) -> Vec<BundleFile> {
    let mut files = Vec::new();
    for (name, kind) in [
        (".credentials.json", BundleFileKind::Credentials),
        (".claude.json", BundleFileKind::Config),
    ] {
        let path = root.join(name);
        if let Ok(bytes) = std::fs::read(&path) {
            files.push(BundleFile { kind, path, bytes });
        }
    }
    files
}

/// A file's modification time in milliseconds since the epoch: the version a
/// `.claude.json` write carries.
fn file_version(path: &Path) -> Option<u64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let millis = modified
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis();
    u64::try_from(millis).ok().filter(|v| *v > 0)
}

/// The session's own transcript under `<root>/projects`, found without
/// following symlinks. It is sent as append-only deltas, never read whole.
pub fn transcript_path(root: &Path, conversation: &str) -> Option<PathBuf> {
    if conversation.is_empty() || conversation.contains('/') || conversation.contains('\\') {
        return None;
    }
    walkdir::WalkDir::new(root.join("projects"))
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
        .find(|entry| {
            entry.file_type().is_file()
                && entry.file_name() == std::ffi::OsStr::new(&format!("{conversation}.jsonl"))
        })
        .map(walkdir::DirEntry::into_path)
}

/// The largest transcript delta in one request; the engine refuses more.
pub const TRANSCRIPT_DELTA_BYTES: usize = 512 * 1024;
/// Resyncs tolerated in one flush before it gives up until the next debounce.
const MAX_RESYNCS: u32 = 8;

/// Read up to `TRANSCRIPT_DELTA_BYTES` of `path` from `offset`.
pub fn read_delta(path: &Path, offset: u64) -> std::io::Result<Vec<u8>> {
    use std::os::unix::fs::FileExt;
    let file = std::fs::File::open(path)?;
    let mut buf = vec![0u8; TRANSCRIPT_DELTA_BYTES];
    let mut filled = 0;
    while filled < buf.len() {
        let n = file.read_at(&mut buf[filled..], offset + filled as u64)?;
        if n == 0 {
            break;
        }
        filled += n;
    }
    buf.truncate(filled);
    Ok(buf)
}

/// What the engine's answer to a transcript delta means for the next send.
#[derive(Debug, PartialEq, Eq)]
pub enum DeltaReply {
    /// Accepted (or already held): continue from this engine length.
    Advance(u64),
    /// The engine is at a different length: resend from it.
    Resync(u64),
    /// Refused for another reason: stop until the next flush.
    Stop,
}

pub fn delta_reply(status: u16, body: &serde_json::Value) -> DeltaReply {
    let offset = body.get("offset").and_then(serde_json::Value::as_u64);
    match (status, offset) {
        (200..=299, Some(next)) => DeltaReply::Advance(next),
        (409, Some(next))
            if body.get("resync").and_then(serde_json::Value::as_bool) == Some(true) =>
        {
            DeltaReply::Resync(next)
        }
        _ => DeltaReply::Stop,
    }
}

/// Messages exchanged on the authenticated engine stream. Binary PTY data is
/// represented as JSON byte arrays so this module remains usable by non-Rust
/// engine clients without a second codec.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Frame {
    Hello {
        session_id: String,
    },
    Resume {
        cursor: u64,
    },
    Output {
        offset: u64,
        data: Vec<u8>,
    },
    Input {
        data: Vec<u8>,
    },
    Resize {
        cols: u16,
        rows: u16,
    },
    Signal {
        signal: String,
    },
    Ack {
        offset: u64,
    },
    Heartbeat {
        cursor: u64,
    },
    Exited {
        status: Option<i32>,
    },
    Error {
        code: String,
        message: String,
    },
    /// Engine to agent: output at `offset` would leave a hole, and `offset`
    /// is where the engine's stream actually ends. The agent resends from
    /// there if it still holds those bytes, and sends a `Gap` if it does not.
    Resync {
        offset: u64,
    },
    /// Agent to engine: the `lost` bytes starting at `offset` were trimmed
    /// from the replay buffer before the engine acknowledged them. The
    /// stream resumes at `offset + lost`; the engine records the loss.
    Gap {
        offset: u64,
        lost: u64,
    },
    /// Engine to agent: write the account bundle back now and say when done.
    Flush,
    /// Agent to engine: the write-back finished. `ok` is false when the
    /// conversation transcript could not be stored.
    Flushed {
        ok: bool,
    },
}

/// A bounded absolute-offset replay buffer. `next_offset` is the offset after
/// the last byte appended; `replay(cursor)` returns bytes starting at cursor.
#[derive(Debug, Clone)]
pub struct ReplayBuffer {
    capacity: usize,
    start: u64,
    bytes: VecDeque<u8>,
}

impl ReplayBuffer {
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0);
        Self {
            capacity,
            start: 0,
            bytes: VecDeque::new(),
        }
    }
    pub fn start(&self) -> u64 {
        self.start
    }
    pub fn end(&self) -> u64 {
        self.start + self.bytes.len() as u64
    }
    pub fn append(&mut self, data: &[u8]) {
        for &byte in data {
            self.bytes.push_back(byte);
        }
        while self.bytes.len() > self.capacity {
            self.bytes.pop_front();
            self.start += 1;
        }
    }
    pub fn ack(&mut self, offset: u64) {
        let n = offset
            .saturating_sub(self.start)
            .min(self.bytes.len() as u64) as usize;
        self.bytes.drain(..n);
        self.start += n as u64;
    }
    /// The frames that bring an engine whose stream ends at `from` up to the
    /// end of this buffer: the bytes from `from` if they are still held,
    /// otherwise a `Gap` for those trimmed away followed by what remains.
    pub fn resend_frames(&self, from: u64) -> Vec<Frame> {
        let mut frames = Vec::new();
        let mut at = from;
        if from < self.start {
            frames.push(Frame::Gap {
                offset: from,
                lost: self.start - from,
            });
            at = self.start;
        }
        if let Some(bytes) = self.replay(at) {
            // Chunked, so one frame stays well inside the engine's WS message
            // limit whatever VOGT_AGENT_BUFFER_BYTES is set to.
            for chunk in bytes.chunks(RESEND_CHUNK_BYTES) {
                frames.push(Frame::Output {
                    offset: at,
                    data: chunk.to_vec(),
                });
                at += chunk.len() as u64;
            }
        }
        frames
    }

    pub fn replay(&self, cursor: u64) -> Option<Vec<u8>> {
        if cursor < self.start || cursor > self.end() {
            return None;
        }
        Some(
            self.bytes
                .iter()
                .skip((cursor - self.start) as usize)
                .copied()
                .collect(),
        )
    }
}

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("missing environment variable {0}")]
    MissingEnv(&'static str),
    #[error("invalid engine URL: {0}")]
    Url(#[from] url::ParseError),
    #[error("invalid token header")]
    TokenHeader,
    #[error("PTY error: {0}")]
    Pty(String),
    #[error("websocket error: {0}")]
    WebSocket(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("protocol error: {0}")]
    Protocol(#[from] serde_json::Error),
    #[error("TLS configuration error: {0}")]
    TlsConfig(String),
    #[error("HTTP client error: {0}")]
    HttpClient(#[from] reqwest::Error),
}

#[derive(Debug, Clone)]
pub struct Config {
    pub session_id: String,
    pub engine_url: String,
    pub token: String,
    pub cli: String,
    /// Argv to run in the PTY instead of the agent CLI (`VOGT_SESSION_COMMAND`, a JSON array).
    pub command: Option<Vec<String>>,
    pub cwd: Option<String>,
    pub cols: u16,
    pub rows: u16,
    pub buffer_bytes: usize,
    pub claude_config_dir: Option<PathBuf>,
    pub account: Option<String>,
    pub conversation_id: Option<String>,
    /// Resume the conversation from the transcript the engine seeded into the
    /// bundle instead of starting it (a pod woken from hibernation).
    pub resume: bool,
    /// The files Claude Code is started with, as the engine starts one on a
    /// host (WI-3243).
    pub claude: ClaudeLaunch,
}

/// Where the image installs the default driven-session policy; the same path
/// as the merged image, so the engine's documentation of it holds for pods.
pub const IMAGE_SETTINGS_FILE: &str = "/usr/local/share/vogt/driven-session-settings.json";

/// The `--settings` and `--mcp-config` files of a Claude Code launch.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClaudeLaunch {
    /// The driven-session policy, passed as `--settings=<file>`.
    pub settings: Option<PathBuf>,
    /// The engine's MCP endpoints, passed as `--strict-mcp-config
    /// --mcp-config=<file>` so only these servers apply, not the account's
    /// own set. The session entrypoint writes it from the bootstrap response.
    pub mcp_config: Option<PathBuf>,
    /// The brief the entrypoint wrote (`VOGT_SESSION_BRIEF_FILE`), when it
    /// has something in it: the first prompt points Claude Code at it, as the
    /// host launcher's does (WI-3355).
    pub brief: Option<PathBuf>,
}

/// The first prompt of a session started with a brief.
pub fn brief_instruction(path: &Path) -> String {
    format!(
        "Vogt started this session with a brief in {}. Read that file now. \
         If it has a \"Task\" section, carry that task out; otherwise \
         summarise the brief in one line and wait for instructions.",
        path.display()
    )
}

impl ClaudeLaunch {
    /// Read from the environment: `VOGT_CLAUDE_SETTINGS_FILE` names a
    /// deployment's own policy; unset or empty means the image's, when it is
    /// there; `off` turns it off. This is the engine's
    /// `ENGINE_AGENT_CLAUDE_SETTINGS` rule, applied inside the pod.
    /// `VOGT_CLAUDE_MCP_CONFIG` is the file the entrypoint wrote, if any.
    pub fn from_env() -> Self {
        Self::resolve(
            std::env::var("VOGT_CLAUDE_SETTINGS_FILE").ok().as_deref(),
            Path::new(IMAGE_SETTINGS_FILE),
            std::env::var_os("VOGT_CLAUDE_MCP_CONFIG"),
        )
        .with_brief(std::env::var_os("VOGT_SESSION_BRIEF_FILE"))
    }

    /// Keep `brief` only when the file holds text: an empty brief is no
    /// brief, and a prompt naming it would send Claude Code to read nothing.
    fn with_brief(mut self, brief: Option<std::ffi::OsString>) -> Self {
        self.brief = brief
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .filter(|p| std::fs::read_to_string(p).is_ok_and(|t| !t.trim().is_empty()));
        self
    }

    fn resolve(
        settings: Option<&str>,
        image: &Path,
        mcp_config: Option<std::ffi::OsString>,
    ) -> Self {
        let settings = match settings.map(str::trim) {
            Some("off") => None,
            Some(path) if !path.is_empty() => Some(PathBuf::from(path)),
            _ => Some(image.to_path_buf()).filter(|p| p.is_file()),
        };
        Self {
            settings,
            mcp_config: mcp_config.filter(|p| !p.is_empty()).map(PathBuf::from),
            brief: None,
        }
    }
}

/// `value` as one shell word, so a path from the environment cannot add
/// words or run anything on the line `command_line` builds.
fn sh_quote(value: &Path) -> String {
    format!("'{}'", value.to_string_lossy().replace('\'', "'\\''"))
}

/// The argv in `VOGT_SESSION_COMMAND` (a JSON array of strings), if any.
pub fn parse_command(raw: Option<&str>) -> Option<Vec<String>> {
    serde_json::from_str::<Vec<String>>(raw?)
        .ok()
        .filter(|argv| !argv.is_empty())
}

impl Config {
    pub fn from_env() -> Result<Self, AgentError> {
        let get = |key: &'static str| std::env::var(key).map_err(|_| AgentError::MissingEnv(key));
        Ok(Self {
            session_id: get("VOGT_SESSION_ID")?,
            engine_url: get("VOGT_ENGINE_URL")?,
            token: std::fs::read_to_string(get("VOGT_SESSION_TOKEN_FILE")?)
                .map_err(|_| AgentError::MissingEnv("VOGT_SESSION_TOKEN_FILE"))?
                .trim()
                .to_owned(),
            cli: get("VOGT_CLI")?,
            command: parse_command(std::env::var("VOGT_SESSION_COMMAND").ok().as_deref()),
            cwd: std::env::var("VOGT_SESSION_CWD").ok(),
            cols: std::env::var("VOGT_PTY_COLS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(120),
            rows: std::env::var("VOGT_PTY_ROWS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(40),
            buffer_bytes: std::env::var("VOGT_AGENT_BUFFER_BYTES")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(DEFAULT_BUFFER_BYTES),
            claude_config_dir: std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from),
            account: std::env::var("VOGT_SESSION_ACCOUNT").ok(),
            conversation_id: std::env::var("VOGT_SESSION_CONVERSATION_ID")
                .ok()
                .filter(|v| !v.is_empty()),
            resume: std::env::var("VOGT_SESSION_RESUME").is_ok_and(|v| v == "1"),
            claude: ClaudeLaunch::from_env(),
        })
    }
    pub fn ws_url(&self) -> Result<String, AgentError> {
        let mut url = url::Url::parse(&self.engine_url)?;
        let scheme = match url.scheme() {
            "http" => "ws",
            "https" => "wss",
            "ws" => "ws",
            "wss" => "wss",
            _ => return Err(AgentError::Url(url::ParseError::RelativeUrlWithoutBase)),
        };
        url.set_scheme(scheme)
            .map_err(|_| url::ParseError::RelativeUrlWithoutBase)?;
        let path = format!("/api/agent-auth/sessions/{}/agent", self.session_id);
        url.set_path(&path);
        Ok(url.to_string())
    }
}

fn tls_config() -> Result<Arc<rustls::ClientConfig>, AgentError> {
    let native = rustls_native_certs::load_native_certs();
    let mut roots = rustls::RootCertStore::empty();
    let (added, ignored) = roots.add_parsable_certificates(native.certs);
    if !native.errors.is_empty() {
        tracing::warn!(errors = ?native.errors, "some native root certificates could not be loaded");
    }
    tracing::debug!(added, ignored, "loaded native root certificates");

    if let Some(path) = std::env::var_os("VOGT_EXTRA_CA_FILE") {
        let file = File::open(&path).map_err(|error| {
            AgentError::TlsConfig(format!(
                "cannot read VOGT_EXTRA_CA_FILE {}: {error}",
                Path::new(&path).display()
            ))
        })?;
        let mut reader = std::io::BufReader::new(file);
        let certs = rustls_pemfile::certs(&mut reader)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| {
                AgentError::TlsConfig(format!("invalid VOGT_EXTRA_CA_FILE: {error}"))
            })?;
        if certs.is_empty() {
            return Err(AgentError::TlsConfig(
                "VOGT_EXTRA_CA_FILE contains no certificates".into(),
            ));
        }
        let (extra_added, extra_ignored) = roots.add_parsable_certificates(certs);
        if extra_added == 0 {
            return Err(AgentError::TlsConfig(
                "VOGT_EXTRA_CA_FILE contains no usable certificates".into(),
            ));
        }
        tracing::debug!(extra_added, extra_ignored, "loaded extra root certificates");
    }

    Ok(Arc::new(
        rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|error| AgentError::TlsConfig(error.to_string()))?
        .with_root_certificates(roots)
        .with_no_client_auth(),
    ))
}

fn http_client(tls: Arc<rustls::ClientConfig>) -> Result<reqwest::Client, AgentError> {
    reqwest::Client::builder()
        .no_proxy()
        .use_preconfigured_tls((*tls).clone())
        .build()
        .map_err(AgentError::HttpClient)
}

/// Write the bundle files that changed back to the engine. Returns false when
/// the conversation transcript could not be stored, so a caller that must not
/// lose it (hibernation) can tell. A file the engine refuses with a status
/// (credentials no newer than the stored ones, say) is final and not retried;
/// a transport failure is retried on the next flush.
async fn flush_bundle(
    config: &Config,
    tls: &Arc<rustls::ClientConfig>,
    seen: &mut std::collections::HashMap<PathBuf, [u8; 32]>,
    offsets: &mut std::collections::HashMap<PathBuf, u64>,
) -> bool {
    let (Some(root), Some(account)) = (
        config.claude_config_dir.as_deref(),
        config.account.as_deref(),
    ) else {
        return true;
    };
    // The login and global config are the account's and are written back with
    // or without a conversation; only a transcript needs one (WI-3334).
    let conversation = config.conversation_id.as_deref().unwrap_or("");
    // The broker token is a credential and must only ever go to the engine.
    // In particular, do not let ambient HTTP(S)_PROXY redirect this request.
    let Ok(client) = http_client(tls.clone()) else {
        return false;
    };
    let mut transcript_ok = true;
    let mut url = config.engine_url.clone();
    url.push_str("/api/agent-auth/store/bundle/bundle-file");
    for file in bundle_files(root) {
        let marker = bundle_marker(&file.bytes);
        if seen.get(&file.path).copied() == Some(marker) {
            continue;
        }
        let kind = match file.kind {
            BundleFileKind::Credentials => "credentials",
            BundleFileKind::Config => "config",
            BundleFileKind::Transcript => continue,
        };
        let mut body = serde_json::json!({"account": account, "conversation_id": conversation, "path": file.path.strip_prefix(root).unwrap_or(&file.path).to_string_lossy(), "kind": kind, "bytes": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &file.bytes)});
        if file.kind == BundleFileKind::Config {
            // The engine orders `.claude.json` writes by this and refuses one
            // without it: the file's own modification time, in milliseconds.
            match file_version(&file.path) {
                Some(version) => body["version"] = serde_json::json!(version),
                None => continue,
            }
        }
        // A reply the engine will give again for the same bytes is final. A
        // rate limit or a server fault is not: the file is tried again on the
        // next flush.
        if let Ok(reply) = client
            .post(&url)
            .bearer_auth(&config.token)
            .json(&body)
            .send()
            .await
        {
            let status = reply.status();
            if !(status.as_u16() == 429 || status.is_server_error()) {
                seen.insert(file.path, marker);
            }
        }
    }
    if let Some(path) = (!conversation.is_empty())
        .then(|| transcript_path(root, conversation))
        .flatten()
    {
        // Caught up when the delta is empty; any send failure leaves it behind.
        let mut caught_up = false;
        let relative = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned();
        let mut offset = offsets.get(&path).copied().unwrap_or(0);
        let mut resyncs = 0;
        while let Ok(delta) = read_delta(&path, offset) {
            if delta.is_empty() {
                caught_up = true;
                break;
            }
            let body = serde_json::json!({"account": account, "conversation_id": conversation, "path": relative, "kind": "transcript", "offset": offset, "bytes": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &delta)});
            let Ok(reply) = client
                .post(&url)
                .bearer_auth(&config.token)
                .json(&body)
                .send()
                .await
            else {
                break;
            };
            let status = reply.status().as_u16();
            let json = reply.json::<serde_json::Value>().await.unwrap_or_default();
            match delta_reply(status, &json) {
                DeltaReply::Advance(next) => offset = next,
                DeltaReply::Resync(next) => {
                    resyncs += 1;
                    if next == offset || resyncs > MAX_RESYNCS {
                        break;
                    }
                    offset = next;
                }
                DeltaReply::Stop => break,
            }
            offsets.insert(path.clone(), offset);
        }
        offsets.insert(path.clone(), offset);
        // A transcript that cannot be read yet (no turn written) has nothing to lose.
        transcript_ok = caught_up || !path.exists();
    }
    transcript_ok
}

async fn connect_websocket(
    request: http::Request<()>,
    tls: Arc<rustls::ClientConfig>,
) -> Result<
    (
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        tokio_tungstenite::tungstenite::handshake::client::Response,
    ),
    tokio_tungstenite::tungstenite::Error,
> {
    connect_async_tls_with_config(request, None, false, Some(Connector::Rustls(tls))).await
}

const CONNECT_INITIAL_BACKOFF: Duration = Duration::from_millis(250);
const CONNECT_BACKOFF_CAP: Duration = Duration::from_secs(2);
const CONNECT_TOTAL: Duration = Duration::from_secs(15);
const CONNECT_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(5);

/// A new pod's IP is admitted to the engine's allow-set 1-3 s after it starts;
/// until then the engine refuses the connection. Connection failures, timeouts
/// and 5xx are transient; a 4xx (bad token, unknown session) is not.
fn connect_retryable(error: &tokio_tungstenite::tungstenite::Error) -> bool {
    use tokio_tungstenite::tungstenite::Error;
    match error {
        Error::Io(_) | Error::ConnectionClosed | Error::AlreadyClosed => true,
        Error::Http(response) => response.status().is_server_error(),
        _ => false,
    }
}

/// Retry `attempt` with doubling backoff capped at `cap`, until `total` has
/// elapsed. The last error is returned once retrying stops.
async fn retry_with_backoff<T, E, Fut>(
    mut attempt: impl FnMut() -> Fut,
    retryable: impl Fn(&E) -> bool,
    initial: Duration,
    cap: Duration,
    total: Duration,
) -> Result<T, E>
where
    Fut: std::future::Future<Output = Result<T, E>>,
{
    let started = tokio::time::Instant::now();
    let mut backoff = initial;
    loop {
        match attempt().await {
            Ok(value) => return Ok(value),
            Err(error) => {
                let remaining = total.saturating_sub(started.elapsed());
                if !retryable(&error) || remaining.is_zero() {
                    return Err(error);
                }
                tokio::time::sleep(backoff.min(remaining)).await;
                backoff = (backoff * 2).min(cap);
            }
        }
    }
}

async fn connect_with_retry(
    request: &http::Request<()>,
    tls: Arc<rustls::ClientConfig>,
    total: Duration,
) -> Result<
    (
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        tokio_tungstenite::tungstenite::handshake::client::Response,
    ),
    tokio_tungstenite::tungstenite::Error,
> {
    retry_with_backoff(
        || async {
            let attempt = connect_websocket(request.clone(), tls.clone());
            match tokio::time::timeout(CONNECT_ATTEMPT_TIMEOUT, attempt).await {
                Ok(result) => result,
                Err(_) => Err(tokio_tungstenite::tungstenite::Error::Io(
                    std::io::ErrorKind::TimedOut.into(),
                )),
            }
        },
        connect_retryable,
        CONNECT_INITIAL_BACKOFF,
        CONNECT_BACKOFF_CAP,
        total,
    )
    .await
}

fn bundle_marker(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

pub fn authorize(request: &mut http::Request<()>, token: &str) -> Result<(), AgentError> {
    let value =
        HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| AgentError::TokenHeader)?;
    request.headers_mut().insert("authorization", value);
    Ok(())
}

/// Spawn a command in a PTY and return its reader, writer and child handle.
type PtyChild = Box<dyn portable_pty::Child + Send>;
type PtyMaster = Box<dyn portable_pty::MasterPty + Send>;
type PtyIo = (
    Box<dyn Read + Send>,
    Box<dyn Write + Send>,
    PtyChild,
    PtyMaster,
);

/// Apply a viewer's `resize` frame to the PTY.
pub fn resize_pty(master: &PtyMaster, cols: u16, rows: u16) -> Result<(), AgentError> {
    master
        .resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| AgentError::Pty(e.to_string()))
}

/// The shell line that starts the CLI. A Claude conversation is pinned to the
/// engine's conversation id (`--session-id`), or resumed from the transcript
/// the engine seeded (`--resume`), so the transcript this agent writes back is
/// the one a later wake resumes. Claude Code also gets the driven-session
/// policy and the engine's MCP set when the pod has them (WI-3243), so a pod
/// session has the permission posture a host session has.
pub fn command_line(config: &Config) -> Result<String, AgentError> {
    let conversation = config.conversation_id.as_deref().filter(|id| {
        id.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    });
    match config.cli.as_str() {
        "claude" => {
            // The host launcher's order (`agent_cli::launch`): the
            // conversation, the driven-session policy, the MCP set, then
            // `--add-dir`. The `=` forms keep the variadic flags from taking
            // the next word.
            let mut line = String::from("claude");
            match (conversation, config.resume) {
                (Some(id), true) => line.push_str(&format!(" --resume {id}")),
                (Some(id), false) => line.push_str(&format!(" --session-id {id}")),
                (None, _) => {}
            }
            if let Some(file) = &config.claude.settings {
                line.push_str(&format!(" --settings={}", sh_quote(file)));
            }
            if let Some(file) = &config.claude.mcp_config {
                line.push_str(&format!(
                    " --strict-mcp-config --mcp-config={}",
                    sh_quote(file)
                ));
            }
            // The `=` form: `--add-dir` is variadic, and the prompt after it
            // must stay a prompt. A resumed conversation has read its brief.
            line.push_str(" --add-dir=\"$VOGT_SESSION_CWD\"");
            if let (Some(file), false) = (&config.claude.brief, config.resume) {
                line.push(' ');
                line.push_str(&sh_quote(Path::new(&brief_instruction(file))));
            }
            Ok(line)
        }
        // No `--add-dir`: Codex's effective sandbox refuses extra writable
        // roots and exits. The PTY already starts in the session cwd, which
        // is the workspace root Codex uses.
        "codex" => Ok("codex".into()),
        "opencode" => Ok("opencode".into()),
        _ => Err(AgentError::Pty(
            "VOGT_CLI must be claude, codex or opencode".into(),
        )),
    }
}

/// What the session agent adds to the CLI's environment. Claude Code's own
/// updater cannot write to the image and only prints "Auto-update failed"
/// into the session (WI-3355).
pub fn launch_env(config: &Config) -> Vec<(&'static str, &'static str)> {
    match (config.command.as_ref(), config.cli.as_str()) {
        (None, "claude") => vec![("DISABLE_AUTOUPDATER", "1")],
        _ => Vec::new(),
    }
}

pub fn spawn_pty(config: &Config) -> Result<PtyIo, AgentError> {
    let system = portable_pty::native_pty_system();
    let pair = system
        .openpty(PtySize {
            rows: config.rows,
            cols: config.cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| AgentError::Pty(e.to_string()))?;
    let command = match &config.command {
        Some(argv) => {
            let mut command = CommandBuilder::new(&argv[0]);
            command.args(&argv[1..]);
            command
        }
        None => {
            let command_line = command_line(config)?;
            let mut command = CommandBuilder::new("/bin/sh");
            command.arg("-lc");
            command.arg(command_line);
            command
        }
    };
    let mut command = command;
    for (name, value) in launch_env(config) {
        command.env(name, value);
    }
    if let Some(cwd) = &config.cwd {
        command.cwd(cwd);
    }
    let child = pair
        .slave
        .spawn_command(command)
        .map_err(|e| AgentError::Pty(e.to_string()))?;
    let reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| AgentError::Pty(e.to_string()))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|e| AgentError::Pty(e.to_string()))?;
    Ok((reader, writer, child, pair.master))
}

pub async fn run(config: Config) -> Result<(), AgentError> {
    let tls = tls_config()?;
    let mut bundle_seen = std::collections::HashMap::new();
    let mut transcript_offsets = std::collections::HashMap::new();
    let _ = flush_bundle(&config, &tls, &mut bundle_seen, &mut transcript_offsets).await;
    tracing::info!(
        cli = %config.cli,
        program = config.command.as_ref().map(|argv| argv[0].as_str()).unwrap_or("-"),
        cwd = ?config.cwd,
        "session-agent: starting the PTY"
    );
    let (reader, writer, child, master) = spawn_pty(&config)?;
    tracing::info!("session-agent: PTY spawned");
    let writer = Arc::new(Mutex::new(writer));
    let (output_tx, mut output_rx) = mpsc::channel::<Vec<u8>>(32);
    std::thread::spawn(move || {
        let mut reader = reader;
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if output_tx.blocking_send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    let mut child = Some(child);
    let mut replay = ReplayBuffer::new(config.buffer_bytes);
    let mut cursor = 0u64;
    let mut acked = 0u64;
    let mut backoff = Duration::from_millis(100);
    let mut first_output_logged = false;
    let mut stopped = false;
    loop {
        // Request construction is a configuration error, not a transient socket
        // failure.  Return through the cleanup below rather than orphaning PTY.
        let mut req = match config
            .ws_url()
            .and_then(|url| url.into_client_request().map_err(AgentError::WebSocket))
        {
            Ok(req) => req,
            Err(error) => {
                if let Some(c) = child.as_mut() {
                    let _ = c.kill();
                }
                return Err(error);
            }
        };
        if let Err(error) = authorize(&mut req, &config.token) {
            if let Some(c) = child.as_mut() {
                let _ = c.kill();
            }
            return Err(error);
        }
        let connection: Result<(), AgentError> = async {
            tracing::info!(path = %req.uri().path(), "session-agent: connecting to the engine");
            let (socket, _) = connect_with_retry(&req, tls.clone(), CONNECT_TOTAL).await?;
            tracing::info!("session-agent: connected to the engine");
            backoff = Duration::from_millis(100);
            let (mut sink, mut stream) = socket.split();
            send_frame(&mut sink, &Frame::Hello { session_id: config.session_id.clone() }).await?;
            send_frame(&mut sink, &Frame::Resume { cursor: acked }).await?;
            for frame in replay.resend_frames(acked) {
                send_frame(&mut sink, &frame).await?;
            }
            loop {
                tokio::select! {
                    Some(data) = output_rx.recv() => {
                        if !first_output_logged {
                            first_output_logged = true;
                            tracing::info!(bytes = data.len(), "session-agent: first PTY output, sending to the engine");
                        }
                        replay.append(&data);
                        let frame = Frame::Output { offset: cursor, data };
                        cursor += frame_data_len(&frame);
                        send_frame(&mut sink, &frame).await?;
                    }
                    Some(message) = stream.next() => {
                        match message? {
                            Message::Text(text) => {
                                let frame = serde_json::from_str::<Frame>(&text)?;
                                match frame {
                                    Frame::Input { data } => {
                                        tracing::info!(bytes = data.len(), "session-agent: input from the engine");
                                        writer.lock().await.write_all(&data).map_err(|e| AgentError::Pty(e.to_string()))?
                                    },
                                    Frame::Resize { cols, rows } => {
                                        tracing::info!(cols, rows, "session-agent: resize from the engine");
                                        resize_pty(&master, cols, rows)?
                                    },
                                    // A stop ends this agent whether or not the
                                    // signal could be delivered: the agent is the
                                    // container's PID 1, so its exit takes the CLI
                                    // with it. A failed kill used to drop the
                                    // stream; the engine re-sent the stop on every
                                    // reconnect, and the pod was never torn down.
                                    // The flag, not this block's return, ends
                                    // run(): returning here alone used to loop
                                    // back and reconnect.
                                    Frame::Signal { signal } if signal == "kill" => {
                                        let frame = stop_child(&mut child).await;
                                        stopped = true;
                                        // Final flush: the CLI's last credential and
                                        // transcript writes reach the engine before the
                                        // pod goes (hibernate and stop both kill).
                                        let _ = flush_bundle(&config, &tls, &mut bundle_seen, &mut transcript_offsets).await;
                                        let _ = send_frame(&mut sink, &frame).await;
                                        return Ok(());
                                    },
                                    Frame::Ack { offset } if offset <= cursor => { replay.ack(offset); acked = acked.max(offset); },
                                    // The engine's stream ends at `offset`, short of
                                    // what was sent. A stale one (the engine has
                                    // since acked past it) needs nothing.
                                    Frame::Resync { offset } if offset >= acked && offset <= cursor => {
                                        for frame in replay.resend_frames(offset) {
                                            send_frame(&mut sink, &frame).await?;
                                        }
                                    },
                                    Frame::Heartbeat { .. } => {},
                                    Frame::Flush => {
                                        let ok = flush_bundle(&config, &tls, &mut bundle_seen, &mut transcript_offsets).await;
                                        send_frame(&mut sink, &Frame::Flushed { ok }).await?;
                                    },
                                    _ => {},
                                }
                            }
                            Message::Ping(data) => { sink.send(Message::Pong(data)).await?; }
                            Message::Close(_) => return Ok(()),
                            _ => {},
                        }
                    }
                    _ = tokio::time::sleep(BUNDLE_DEBOUNCE) => {
                        let _ = flush_bundle(&config, &tls, &mut bundle_seen, &mut transcript_offsets).await;
                        if let Some(c) = child.as_mut() {
                            if let Ok(Some(status)) = c.try_wait() {
                                tracing::info!(exit_code = status.exit_code(), "session-agent: the CLI exited");
                                // The CLI may have written after the flush above.
                                let _ = flush_bundle(&config, &tls, &mut bundle_seen, &mut transcript_offsets).await;
                                send_frame(&mut sink, &Frame::Exited { status: Some(status.exit_code() as i32) }).await?;
                                return Ok(());
                            }
                        }
                        send_frame(&mut sink, &Frame::Heartbeat { cursor }).await?;
                    }
                }
            }
        }.await;
        if stopped {
            return Ok(());
        }
        if let Err(error) = &connection {
            // The engine refuses a session it no longer knows (it ended and its
            // credential was dropped): retrying cannot succeed.
            if is_auth_refusal(error) {
                tracing::info!(%error, "session-agent: the engine refused the session credential; stopping");
                if let Some(c) = child.as_mut() {
                    let _ = c.kill();
                }
                return Ok(());
            }
            tracing::warn!(%error, retry_in_ms = backoff.as_millis() as u64, "session-agent: engine stream ended");
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(Duration::from_secs(10));
        }
        // A disconnected stream is retried. If the child exited meanwhile,
        // the reconnect sends Hello/Resume and then Exited on that same
        // authenticated socket from the timer branch above.
    }
}

/// How long a stop waits for the killed CLI to be reaped before reporting
/// the exit without a status.
const STOP_REAP_WAIT: Duration = Duration::from_secs(2);

/// Stop the CLI for an engine `kill` and say how it ended. Never fails: a
/// kill the kernel or an LSM refuses (an AppArmor signal rule answers
/// `EACCES`) is logged, then `SIGKILL` is tried by pid, and the caller exits
/// anyway, which ends the container and everything in it. A CLI this agent
/// killed reports 137 (128 + SIGKILL), as a shell would. The signal and the
/// bounded reap run off the async workers.
async fn stop_child(child: &mut Option<PtyChild>) -> Frame {
    let Some(c) = child.take() else {
        return Frame::Exited { status: None };
    };
    let status = tokio::task::spawn_blocking(move || stop_and_reap(c, STOP_REAP_WAIT))
        .await
        .unwrap_or(None);
    tracing::info!(?status, "session-agent: stopped on the engine's request");
    Frame::Exited { status }
}

fn stop_and_reap(mut c: PtyChild, wait: Duration) -> Option<i32> {
    let killed = match c.kill() {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(%error, "session-agent: could not signal the CLI; trying SIGKILL, then exiting");
            sigkill_by_pid(c.as_ref())
        }
    };
    let deadline = std::time::Instant::now() + wait;
    loop {
        match c.try_wait() {
            Ok(Some(status)) if killed && status.signal().is_some() => return Some(137),
            Ok(Some(status)) => return Some(status.exit_code() as i32),
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => return None,
        }
    }
}

#[cfg(unix)]
fn sigkill_by_pid(c: &(dyn portable_pty::Child + Send)) -> bool {
    let Some(pid) = c.process_id() else {
        return false;
    };
    // SAFETY: a plain kill(2) on the child's pid; the child is not reaped
    // yet (try_wait reaps it), so the pid is still ours.
    if unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) } != 0 {
        let error = std::io::Error::last_os_error();
        tracing::warn!(%error, pid, "session-agent: SIGKILL refused too; the container's exit ends the CLI");
        return false;
    }
    true
}

#[cfg(not(unix))]
fn sigkill_by_pid(_: &(dyn portable_pty::Child + Send)) -> bool {
    false
}

fn is_auth_refusal(error: &AgentError) -> bool {
    matches!(
        error,
        AgentError::WebSocket(tokio_tungstenite::tungstenite::Error::Http(response))
            if matches!(response.status().as_u16(), 401 | 403)
    )
}

fn frame_data_len(frame: &Frame) -> u64 {
    match frame {
        Frame::Output { data, .. } => data.len() as u64,
        _ => 0,
    }
}

async fn send_frame<S>(sink: &mut S, frame: &Frame) -> Result<(), AgentError>
where
    S: futures_util::Sink<Message> + Unpin,
    S::Error: Into<tokio_tungstenite::tungstenite::Error>,
{
    let text = serde_json::to_string(frame)?;
    sink.send(Message::Text(text.into()))
        .await
        .map_err(Into::into)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    /// A child whose signals are all refused, as an AppArmor signal rule does.
    #[derive(Debug)]
    struct Unkillable;
    impl portable_pty::ChildKiller for Unkillable {
        fn kill(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::from_raw_os_error(libc::EACCES))
        }
        fn clone_killer(&self) -> Box<dyn portable_pty::ChildKiller + Send + Sync> {
            Box::new(Unkillable)
        }
    }
    impl portable_pty::Child for Unkillable {
        fn try_wait(&mut self) -> std::io::Result<Option<portable_pty::ExitStatus>> {
            Ok(None)
        }
        fn wait(&mut self) -> std::io::Result<portable_pty::ExitStatus> {
            Err(std::io::Error::from_raw_os_error(libc::EACCES))
        }
        fn process_id(&self) -> Option<u32> {
            None
        }
    }

    // WI-3319: a refused kill must still end the session, not fail the stream.
    #[test]
    fn a_refused_kill_still_reports_the_exit() {
        assert_eq!(stop_and_reap(Box::new(Unkillable), Duration::ZERO), None);
        let runtime = tokio::runtime::Runtime::new().unwrap();
        assert_eq!(
            runtime.block_on(stop_child(&mut None)),
            Frame::Exited { status: None }
        );
    }

    #[test]
    fn a_kill_reports_the_cli_exit() {
        let config = Config {
            session_id: "s".into(),
            engine_url: "http://127.0.0.1:1".into(),
            token: "t".into(),
            cli: "claude".into(),
            command: Some(vec!["sleep".into(), "30".into()]),
            cwd: None,
            cols: 80,
            rows: 24,
            buffer_bytes: 1024,
            claude_config_dir: None,
            account: None,
            conversation_id: None,
            resume: false,
            claude: ClaudeLaunch::default(),
        };
        let (_reader, _writer, child, _master) = spawn_pty(&config).unwrap();
        // The reap waits for the exit, so no sleep races the kill.
        assert_eq!(stop_and_reap(child, Duration::from_secs(30)), Some(137));
    }

    fn kill_config(engine_url: String) -> Config {
        Config {
            session_id: "s".into(),
            engine_url,
            token: "t".into(),
            cli: "claude".into(),
            command: Some(vec!["sleep".into(), "30".into()]),
            cwd: None,
            cols: 80,
            rows: 24,
            buffer_bytes: 1024,
            claude_config_dir: None,
            account: None,
            conversation_id: None,
            resume: false,
            claude: ClaudeLaunch::default(),
        }
    }

    // WI-3319 B1: a kill ends run() itself, even when the Exited frame cannot
    // be sent. It used to end only the connection, so the agent reconnected.
    #[tokio::test]
    async fn a_kill_ends_run_even_when_exited_cannot_be_sent() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let accepts = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let counted = accepts.clone();
        tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                if counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst) > 0 {
                    // A reconnect: hold it open, as a live engine would.
                    let socket = tokio_tungstenite::accept_async(stream).await.unwrap();
                    tokio::spawn(async move {
                        let _socket = socket;
                        std::future::pending::<()>().await
                    });
                    continue;
                }
                let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
                // Hello and Resume first, so the kill lands on a live stream.
                for _ in 0..2 {
                    socket.next().await.unwrap().unwrap();
                }
                let kill = serde_json::to_string(&Frame::Signal {
                    signal: "kill".into(),
                })
                .unwrap();
                socket.send(Message::Text(kill.into())).await.unwrap();
                // Drop the socket unread, so the Exited send has nowhere to go.
                let mut tcp = socket.into_inner();
                let _ = tcp.shutdown().await;
            }
        });
        let result = tokio::time::timeout(
            Duration::from_secs(20),
            run(kill_config(format!("http://{address}"))),
        )
        .await;
        assert!(
            matches!(result, Ok(Ok(()))),
            "run() did not end: {result:?}"
        );
        assert_eq!(accepts.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    /// A fake engine on one port: bundle-file POSTs are answered 200 and their
    /// `kind` recorded (the first `fail_first` get a 503 and are not), as is each Exited frame (`exited`), in arrival order; anything else is the session stream, which reads Hello
    /// and Resume and then, when `kill` is set, calls `before_kill` and sends
    /// a kill.
    async fn fake_engine(
        kill: bool,
        fail_first: usize,
        before_kill: impl Fn() + Send + Sync + 'static,
    ) -> (String, Arc<std::sync::Mutex<Vec<String>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let stored = Arc::new(std::sync::Mutex::new(Vec::new()));
        let record = stored.clone();
        let before_kill = Arc::new(before_kill);
        let failures = Arc::new(std::sync::atomic::AtomicUsize::new(fail_first));
        tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let record = record.clone();
                let before_kill = before_kill.clone();
                let failures = failures.clone();
                tokio::spawn(async move {
                    let mut head = [0u8; 32];
                    let n = stream.peek(&mut head).await.unwrap_or(0);
                    if head[..n].starts_with(b"POST /api/agent-auth/store/") {
                        use tokio::io::AsyncReadExt;
                        let mut buf = Vec::new();
                        let mut chunk = [0u8; 8192];
                        let (header_end, length) = loop {
                            let n = stream.read(&mut chunk).await.unwrap();
                            assert!(n > 0, "bundle POST ended early");
                            buf.extend_from_slice(&chunk[..n]);
                            if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                                let head = String::from_utf8_lossy(&buf[..end]).to_lowercase();
                                let length = head
                                    .lines()
                                    .find_map(|l| l.strip_prefix("content-length:"))
                                    .map(|v| v.trim().parse::<usize>().unwrap())
                                    .unwrap_or(0);
                                break (end + 4, length);
                            }
                        };
                        while buf.len() < header_end + length {
                            let n = stream.read(&mut chunk).await.unwrap();
                            assert!(n > 0, "bundle POST body ended early");
                            buf.extend_from_slice(&chunk[..n]);
                        }
                        let body: serde_json::Value =
                            serde_json::from_slice(&buf[header_end..header_end + length]).unwrap();
                        let fail = failures
                            .try_update(
                                std::sync::atomic::Ordering::SeqCst,
                                std::sync::atomic::Ordering::SeqCst,
                                |n| n.checked_sub(1),
                            )
                            .is_ok();
                        if fail {
                            let _ = stream
                                .write_all(b"HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                                .await;
                            return;
                        }
                        record
                            .lock()
                            .unwrap()
                            .push(body["kind"].as_str().unwrap_or("").to_string());
                        let _ = stream
                            .write_all(b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}")
                            .await;
                        return;
                    }
                    let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
                    for _ in 0..2 {
                        socket.next().await.unwrap().unwrap();
                    }
                    if kill {
                        before_kill();
                        let kill = serde_json::to_string(&Frame::Signal {
                            signal: "kill".into(),
                        })
                        .unwrap();
                        socket.send(Message::Text(kill.into())).await.unwrap();
                    }
                    while let Some(Ok(message)) = socket.next().await {
                        if let Message::Text(text) = message {
                            if let Ok(Frame::Exited { .. }) = serde_json::from_str::<Frame>(&text) {
                                record.lock().unwrap().push("exited".into());
                            }
                        }
                    }
                });
            }
        });
        (format!("http://{address}"), stored)
    }

    const VALID_CREDENTIALS: &str =
        r#"{"claudeAiOauth":{"accessToken":"a","refreshToken":"r","expiresAt":1}}"#;

    // WI-3160: a stop or hibernate kills the CLI; its last login write must
    // reach the engine before run() returns, not wait for a debounce that
    // never comes.
    #[tokio::test]
    async fn a_kill_flushes_the_bundle_before_run_returns() {
        let dir = tempfile::tempdir().unwrap();
        let credentials = dir.path().join(".credentials.json");
        let (url, stored) = fake_engine(true, 0, move || {
            std::fs::write(&credentials, VALID_CREDENTIALS).unwrap();
        })
        .await;
        let mut config = kill_config(url);
        config.claude_config_dir = Some(dir.path().to_path_buf());
        config.account = Some("acct".into());
        let result = tokio::time::timeout(Duration::from_secs(20), run(config)).await;
        assert!(
            matches!(result, Ok(Ok(()))),
            "run() did not end: {result:?}"
        );
        // The Exited send may lose the race with the socket closing; the push
        // must not.
        assert_eq!(
            stored.lock().unwrap().first().map(String::as_str),
            Some("credentials")
        );
    }

    // WI-3160: a CLI that writes its login and exits still has that write
    // pushed before the agent reports the exit, even when the debounce flush
    // that saw it was refused (a 503 the agent must retry).
    #[tokio::test]
    async fn a_cli_exit_flushes_its_last_write() {
        let dir = tempfile::tempdir().unwrap();
        let credentials = dir.path().join(".credentials.json");
        let (url, stored) = fake_engine(false, 1, || {}).await;
        let mut config = kill_config(url);
        config.command = Some(vec![
            "sh".into(),
            "-c".into(),
            format!(
                "sleep 1.5; printf '%s' '{VALID_CREDENTIALS}' > '{}'",
                credentials.display()
            ),
        ]);
        config.claude_config_dir = Some(dir.path().to_path_buf());
        config.account = Some("acct".into());
        // After Exited the agent keeps its stream until the pod is reaped, so
        // run() does not return here: wait for the Exited frame instead.
        let exited = async {
            while !stored.lock().unwrap().iter().any(|k| k == "exited") {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        };
        tokio::select! {
            () = exited => {},
            result = run(config) => panic!("run() ended before Exited: {result:?}"),
            () = tokio::time::sleep(Duration::from_secs(20)) => panic!("no Exited frame"),
        }
        let events = stored.lock().unwrap().clone();
        let exited = events.iter().position(|k| k == "exited").unwrap();
        assert!(
            events[..exited].iter().any(|k| k == "credentials"),
            "the last write was not pushed before Exited: {events:?}"
        );
    }

    #[test]
    fn resize_changes_the_pty_size() {
        let config = Config {
            session_id: "s".into(),
            engine_url: "http://127.0.0.1:1".into(),
            token: "t".into(),
            cli: "claude".into(),
            command: None,
            cwd: None,
            cols: 80,
            rows: 24,
            buffer_bytes: 1024,
            claude_config_dir: None,
            account: None,
            conversation_id: None,
            resume: false,
            claude: ClaudeLaunch::default(),
        };
        let (_reader, _writer, mut child, master) = spawn_pty(&config).unwrap();
        let size = master.get_size().unwrap();
        assert_eq!((size.cols, size.rows), (80, 24));
        resize_pty(&master, 132, 43).unwrap();
        let size = master.get_size().unwrap();
        assert_eq!((size.cols, size.rows), (132, 43));
        let _ = child.kill();
    }

    #[test]
    fn claude_is_pinned_to_or_resumes_the_engine_conversation() {
        let mut config = Config {
            session_id: "s".into(),
            engine_url: "http://127.0.0.1:1".into(),
            token: "t".into(),
            cli: "claude".into(),
            command: None,
            cwd: None,
            cols: 80,
            rows: 24,
            buffer_bytes: 1024,
            claude_config_dir: None,
            account: None,
            conversation_id: Some("abc-123".into()),
            resume: false,
            claude: ClaudeLaunch::default(),
        };
        assert!(command_line(&config)
            .unwrap()
            .starts_with("claude --session-id abc-123 "));
        config.resume = true;
        assert!(command_line(&config)
            .unwrap()
            .starts_with("claude --resume abc-123 "));
        // An id that is not plain is never put on a shell line.
        config.conversation_id = Some("x; rm -rf /".into());
        assert_eq!(
            command_line(&config).unwrap(),
            "claude --add-dir=\"$VOGT_SESSION_CWD\""
        );
    }

    /// The argv `claude` sees when the shell runs `line`, one word per line.
    fn argv_of(line: &str) -> Vec<String> {
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("claude() {{ printf '%s\\n' \"$@\"; }}; {line}"))
            .env("VOGT_SESSION_CWD", "/work")
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        String::from_utf8(out.stdout)
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn claude_argv_carries_the_policy_and_the_mcp_config() {
        let config = Config {
            session_id: "s".into(),
            engine_url: "http://127.0.0.1:1".into(),
            token: "t".into(),
            cli: "claude".into(),
            command: None,
            cwd: None,
            cols: 80,
            rows: 24,
            buffer_bytes: 1024,
            claude_config_dir: None,
            account: None,
            conversation_id: Some("abc-123".into()),
            resume: false,
            claude: ClaudeLaunch {
                settings: Some(IMAGE_SETTINGS_FILE.into()),
                mcp_config: Some("/run/vogt/mcp.json".into()),
                brief: None,
            },
        };
        // The host launcher's flags, in its order.
        assert_eq!(
            argv_of(&command_line(&config).unwrap()),
            [
                "--session-id",
                "abc-123",
                "--settings=/usr/local/share/vogt/driven-session-settings.json",
                "--strict-mcp-config",
                "--mcp-config=/run/vogt/mcp.json",
                "--add-dir=/work",
            ]
        );
        let resumed = Config {
            resume: true,
            ..config
        };
        let argv = argv_of(&command_line(&resumed).unwrap());
        assert_eq!(&argv[..2], ["--resume", "abc-123"]);
        assert!(argv.contains(&"--strict-mcp-config".to_string()));
    }

    fn claude_config(claude: ClaudeLaunch, resume: bool) -> Config {
        Config {
            session_id: "s".into(),
            engine_url: "http://127.0.0.1:1".into(),
            token: "t".into(),
            cli: "claude".into(),
            command: None,
            cwd: None,
            cols: 80,
            rows: 24,
            buffer_bytes: 1024,
            claude_config_dir: None,
            account: None,
            conversation_id: Some("abc-123".into()),
            resume,
            claude,
        }
    }

    /// WI-3355: the brief reached the entrypoint's file but never the CLI, so
    /// a pod started with a task opened at an empty prompt.
    #[test]
    fn a_fresh_claude_is_given_its_brief_as_the_prompt() {
        let brief = PathBuf::from("/work/.vogt/brief.md");
        let claude = ClaudeLaunch {
            brief: Some(brief.clone()),
            ..ClaudeLaunch::default()
        };
        let argv = argv_of(&command_line(&claude_config(claude.clone(), false)).unwrap());
        assert_eq!(argv.last().unwrap(), &brief_instruction(&brief));
        // `--add-dir` takes one value, so the prompt is not read as a directory.
        assert!(argv.contains(&"--add-dir=/work".to_string()), "{argv:?}");
        // A resumed conversation has read it already.
        let argv = argv_of(&command_line(&claude_config(claude, true)).unwrap());
        assert!(!argv.iter().any(|a| a.contains("brief")), "{argv:?}");
        // No brief, no prompt.
        let argv = argv_of(&command_line(&claude_config(ClaudeLaunch::default(), false)).unwrap());
        assert_eq!(argv, ["--session-id", "abc-123", "--add-dir=/work"]);
    }

    #[test]
    fn only_a_brief_with_text_in_it_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let full = dir.path().join("full.md");
        let empty = dir.path().join("empty.md");
        std::fs::write(&full, "# task\n").unwrap();
        std::fs::write(&empty, "\n").unwrap();
        let kept = |p: &Path| ClaudeLaunch::default().with_brief(Some(p.into())).brief;
        assert_eq!(kept(&full), Some(full.clone()));
        assert_eq!(kept(&empty), None);
        assert_eq!(kept(&dir.path().join("missing.md")), None);
        assert_eq!(ClaudeLaunch::default().with_brief(None).brief, None);
    }

    #[test]
    fn claude_in_a_pod_runs_with_its_updater_off() {
        let mut config = claude_config(ClaudeLaunch::default(), false);
        assert_eq!(launch_env(&config), [("DISABLE_AUTOUPDATER", "1")]);
        config.cli = "codex".into();
        assert!(launch_env(&config).is_empty());
    }

    #[test]
    fn codex_and_opencode_launch_by_name_and_others_are_refused() {
        let mut config = Config {
            session_id: "s".into(),
            engine_url: "http://127.0.0.1:1".into(),
            token: "t".into(),
            cli: "opencode".into(),
            command: None,
            cwd: None,
            cols: 80,
            rows: 24,
            buffer_bytes: 1024,
            claude_config_dir: None,
            account: None,
            conversation_id: None,
            resume: false,
            claude: ClaudeLaunch::default(),
        };
        assert_eq!(command_line(&config).unwrap(), "opencode");
        // Codex refuses extra writable roots, so it gets no `--add-dir`; the
        // PTY already starts in the session cwd.
        config.cli = "codex".into();
        assert_eq!(command_line(&config).unwrap(), "codex");
        config.cli = "nope".into();
        assert!(command_line(&config).is_err());
    }

    #[test]
    fn a_path_with_shell_syntax_stays_one_word() {
        let config = Config {
            session_id: "s".into(),
            engine_url: "http://127.0.0.1:1".into(),
            token: "t".into(),
            cli: "claude".into(),
            command: None,
            cwd: None,
            cols: 80,
            rows: 24,
            buffer_bytes: 1024,
            claude_config_dir: None,
            account: None,
            conversation_id: None,
            resume: false,
            claude: ClaudeLaunch {
                settings: Some("/a b/it's; touch /tmp/x.json".into()),
                mcp_config: None,
                brief: None,
            },
        };
        assert_eq!(
            argv_of(&command_line(&config).unwrap()),
            ["--settings=/a b/it's; touch /tmp/x.json", "--add-dir=/work"]
        );
    }

    #[test]
    fn the_settings_file_follows_the_engines_rule() {
        let image = tempfile::NamedTempFile::new().unwrap();
        let absent = Path::new("/no/such/policy.json");
        let launch = |settings: Option<&str>, image: &Path, mcp: Option<&str>| {
            ClaudeLaunch::resolve(settings, image, mcp.map(Into::into))
        };
        // Unset or empty: the image's, when it is there.
        assert_eq!(
            launch(None, image.path(), None).settings.as_deref(),
            Some(image.path())
        );
        assert_eq!(
            launch(Some(" "), image.path(), None).settings.as_deref(),
            Some(image.path())
        );
        assert_eq!(launch(None, absent, None).settings, None);
        // A named file is used as named, `off` turns the policy off.
        assert_eq!(
            launch(Some("/etc/p.json"), image.path(), None)
                .settings
                .as_deref(),
            Some(Path::new("/etc/p.json"))
        );
        assert_eq!(launch(Some("off"), image.path(), None).settings, None);
        // The MCP file is whatever the entrypoint named; empty is none.
        assert_eq!(
            launch(None, absent, Some("/run/vogt/mcp.json")).mcp_config,
            Some("/run/vogt/mcp.json".into())
        );
        assert_eq!(launch(None, absent, Some("")).mcp_config, None);
    }

    #[test]
    fn flush_frames_are_stable_json() {
        assert_eq!(
            serde_json::to_string(&Frame::Flush).unwrap(),
            "{\"type\":\"flush\"}"
        );
        let f = Frame::Flushed { ok: true };
        assert_eq!(
            serde_json::from_str::<Frame>(&serde_json::to_string(&f).unwrap()).unwrap(),
            f
        );
    }

    #[test]
    fn the_engine_env_value_becomes_the_spawned_argv() {
        // The value pod_template writes for ["bash", "-l"].
        let argv = parse_command(Some(r#"["bash","-l"]"#)).unwrap();
        assert_eq!(argv, ["bash", "-l"]);
        assert_eq!(parse_command(None), None);
        assert_eq!(parse_command(Some("[]")), None);
        assert_eq!(parse_command(Some("bash -l")), None);
    }

    #[test]
    fn command_argv_runs_instead_of_the_cli() {
        let config = Config {
            session_id: "s".into(),
            engine_url: "http://127.0.0.1:1".into(),
            token: "t".into(),
            cli: "claude".into(),
            command: Some(vec!["sh".into(), "-c".into(), "echo argv-ran".into()]),
            cwd: None,
            cols: 80,
            rows: 24,
            buffer_bytes: 1024,
            claude_config_dir: None,
            account: None,
            conversation_id: None,
            resume: false,
            claude: ClaudeLaunch::default(),
        };
        let (mut reader, _writer, mut child, _master) = spawn_pty(&config).unwrap();
        let mut out = String::new();
        let mut buf = [0u8; 256];
        while !out.contains("argv-ran") {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => out.push_str(&String::from_utf8_lossy(&buf[..n])),
            }
        }
        assert!(out.contains("argv-ran"), "got {out:?}");
        let _ = child.wait();
    }

    #[test]
    fn replay_round_trip() {
        let mut b = ReplayBuffer::new(64);
        b.append(b"hello ");
        b.append(b"world");
        assert_eq!(b.replay(0).unwrap(), b"hello world");
        b.ack(6);
        assert_eq!(b.replay(6).unwrap(), b"world");
    }
    #[test]
    fn overflow_reports_gap() {
        let mut b = ReplayBuffer::new(4);
        b.append(b"abcdef");
        assert_eq!(b.start(), 2);
        assert!(b.replay(0).is_none());
        assert_eq!(b.replay(2).unwrap(), b"cdef");
    }
    #[test]
    fn resend_frames_replay_what_is_held_and_mark_what_is_not() {
        let mut b = ReplayBuffer::new(4);
        b.append(b"abcdef");
        // Bytes 0..2 were trimmed; the engine at 0 is told so, then given the rest.
        assert_eq!(
            b.resend_frames(0),
            vec![
                Frame::Gap { offset: 0, lost: 2 },
                Frame::Output {
                    offset: 2,
                    data: b"cdef".to_vec()
                }
            ]
        );
        // An engine still inside the buffer needs no marker.
        assert_eq!(
            b.resend_frames(4),
            vec![Frame::Output {
                offset: 4,
                data: b"ef".to_vec()
            }]
        );
        assert!(b.resend_frames(6).is_empty());
    }
    #[test]
    fn resend_frames_chunk_a_large_replay() {
        let held = 3 * DEFAULT_BUFFER_BYTES;
        let mut b = ReplayBuffer::new(held);
        b.append(&vec![255; held + 10]);
        let frames = b.resend_frames(0);
        assert_eq!(
            frames[0],
            Frame::Gap {
                offset: 0,
                lost: 10
            }
        );
        let mut at = 10;
        for frame in &frames[1..] {
            let Frame::Output { offset, data } = frame else {
                panic!("expected output, got {frame:?}");
            };
            assert_eq!(*offset, at);
            assert!(data.len() <= RESEND_CHUNK_BYTES);
            // Well inside the engine's 1 MiB-scale WS message limit.
            assert!(serde_json::to_string(frame).unwrap().len() < 1024 * 1024);
            at += data.len() as u64;
        }
        assert_eq!(at, b.end());
    }
    #[test]
    fn frames_are_stable_json() {
        let f = Frame::Output {
            offset: 4,
            data: b"ok".to_vec(),
        };
        assert_eq!(
            serde_json::from_str::<Frame>(&serde_json::to_string(&f).unwrap()).unwrap(),
            f
        );
    }
    #[test]
    fn auth_is_bearer() {
        let mut req = http::Request::new(());
        authorize(&mut req, "secret").unwrap();
        assert_eq!(req.headers()["authorization"], "Bearer secret");
    }

    #[test]
    fn deltas_are_read_from_the_offset_in_bounded_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let mut data = vec![b'a'; TRANSCRIPT_DELTA_BYTES + 10];
        data[TRANSCRIPT_DELTA_BYTES..].fill(b'b');
        std::fs::write(&path, &data).unwrap();
        let first = read_delta(&path, 0).unwrap();
        assert_eq!(first.len(), TRANSCRIPT_DELTA_BYTES);
        let second = read_delta(&path, first.len() as u64).unwrap();
        assert_eq!(second, vec![b'b'; 10]);
        assert!(read_delta(&path, data.len() as u64).unwrap().is_empty());
    }

    #[test]
    fn the_engine_reply_decides_advance_resync_or_stop() {
        use serde_json::json;
        assert_eq!(
            delta_reply(200, &json!({"ok": true, "offset": 9})),
            DeltaReply::Advance(9)
        );
        assert_eq!(
            delta_reply(409, &json!({"ok": false, "resync": true, "offset": 4})),
            DeltaReply::Resync(4)
        );
        assert_eq!(delta_reply(403, &json!({"offset": 4})), DeltaReply::Stop);
        assert_eq!(delta_reply(500, &json!(null)), DeltaReply::Stop);
    }

    #[test]
    fn the_transcript_is_found_without_following_links() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("projects/work")).unwrap();
        assert!(transcript_path(dir.path(), "conv").is_none());
        std::fs::write(dir.path().join("projects/work/conv.jsonl"), b"x").unwrap();
        assert!(transcript_path(dir.path(), "conv").is_some());
        assert!(transcript_path(dir.path(), "../conv").is_none());
    }

    #[test]
    fn a_config_version_is_the_files_mtime_in_milliseconds() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".claude.json");
        assert_eq!(file_version(&path), None);
        std::fs::write(&path, b"{}").unwrap();
        let version = file_version(&path).unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as u64;
        assert!(version > 1_500_000_000_000 && version <= now + 1000);
    }

    #[test]
    fn content_marker_detects_same_length_rotation() {
        assert_ne!(bundle_marker(b"old-token"), bundle_marker(b"new-token"));
        assert_eq!(bundle_marker(b"old-token").len(), 32);
    }

    #[test]
    fn exited_frame_is_serialized_as_an_authenticated_stream_frame() {
        let mut req = http::Request::new(());
        authorize(&mut req, "session-secret").unwrap();
        let frame = Frame::Exited { status: Some(7) };
        let encoded = serde_json::to_string(&frame).unwrap();
        assert_eq!(req.headers()["authorization"], "Bearer session-secret");
        assert!(encoded.contains("\"type\":\"exited\""));
        assert!(encoded.contains("\"status\":7"));
    }

    #[tokio::test]
    async fn connect_retries_until_the_engine_admits_the_pod() {
        let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = probe.local_addr().unwrap();
        drop(probe); // refused until the server below binds
        let server = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(900)).await;
            let listener = tokio::net::TcpListener::bind(address).await.unwrap();
            let (stream, _) = listener.accept().await.unwrap();
            tokio_tungstenite::accept_async(stream).await.unwrap();
        });
        let request = format!("ws://{address}/").into_client_request().unwrap();
        let started = std::time::Instant::now();
        let result =
            connect_with_retry(&request, tls_config().unwrap(), Duration::from_secs(10)).await;
        assert!(result.is_ok(), "{:?}", result.err());
        assert!(started.elapsed() >= Duration::from_millis(800));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn connect_gives_up_after_the_total_budget() {
        let probe = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = probe.local_addr().unwrap();
        drop(probe);
        let request = format!("ws://{address}/").into_client_request().unwrap();
        let started = std::time::Instant::now();
        let result =
            connect_with_retry(&request, tls_config().unwrap(), Duration::from_secs(1)).await;
        assert!(result.is_err());
        assert!(started.elapsed() >= Duration::from_secs(1));
        assert!(started.elapsed() < Duration::from_secs(4));
    }

    #[tokio::test]
    async fn retry_does_not_repeat_non_retryable_errors() {
        let calls = std::sync::atomic::AtomicU32::new(0);
        let result: Result<(), &str> = retry_with_backoff(
            || async {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Err("401")
            },
            |_| false,
            Duration::from_millis(10),
            Duration::from_millis(20),
            Duration::from_secs(5),
        )
        .await;
        assert_eq!(result, Err("401"));
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn http_4xx_is_not_retryable_but_5xx_and_io_are() {
        use tokio_tungstenite::tungstenite::Error;
        let response = |status| {
            Error::Http(Box::new(
                http::Response::builder().status(status).body(None).unwrap(),
            ))
        };
        assert!(!connect_retryable(&response(401)));
        assert!(!connect_retryable(&response(404)));
        assert!(connect_retryable(&response(503)));
        assert!(connect_retryable(&Error::Io(
            std::io::ErrorKind::ConnectionRefused.into()
        )));
    }

    #[tokio::test]
    async fn extra_ca_file_trusts_a_local_tls_server() {
        let certified = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let temp = tempfile::tempdir().unwrap();
        let ca_path = temp.path().join("test-ca.pem");
        std::fs::write(&ca_path, certified.cert.pem()).unwrap();
        std::env::set_var("VOGT_EXTRA_CA_FILE", &ca_path);

        let server_config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![certified.cert.der().clone()],
            rustls::pki_types::PrivateKeyDer::Pkcs8(certified.key_pair.serialize_der().into()),
        )
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut stream = tokio_rustls::TlsAcceptor::from(Arc::new(server_config))
                .accept(stream)
                .await
                .unwrap();
            let mut request = [0u8; 1024];
            let _ = stream.read(&mut request).await.unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok")
                .await
                .unwrap();
        });

        let response = http_client(tls_config().unwrap())
            .unwrap()
            .get(format!("https://localhost:{}/", address.port()))
            .send()
            .await
            .unwrap();
        assert_eq!(response.text().await.unwrap(), "ok");
        server.await.unwrap();
        std::env::remove_var("VOGT_EXTRA_CA_FILE");
    }
}
