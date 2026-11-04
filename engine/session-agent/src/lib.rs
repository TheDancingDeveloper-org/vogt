//! The pod-side Vogt session agent.
//!
//! The agent owns the PTY and makes the connection to the engine; a session pod
//! never listens for an engine connection. See the crate README for the wire
//! protocol.

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
        if let Some(bytes) = self.replay(at).filter(|b| !b.is_empty()) {
            frames.push(Frame::Output {
                offset: at,
                data: bytes,
            });
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
    let (Some(root), Some(account), Some(conversation)) = (
        config.claude_config_dir.as_deref(),
        config.account.as_deref(),
        config.conversation_id.as_deref(),
    ) else {
        return true;
    };
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
        let body = serde_json::json!({"account": account, "conversation_id": conversation, "path": file.path.strip_prefix(root).unwrap_or(&file.path).to_string_lossy(), "kind": kind, "bytes": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &file.bytes)});
        match client
            .post(&url)
            .bearer_auth(&config.token)
            .json(&body)
            .send()
            .await
        {
            Ok(_) => {
                seen.insert(file.path, marker);
            }
            Err(_) => {}
        }
    }
    if let Some(path) = transcript_path(root, conversation) {
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
/// the one a later wake resumes.
pub fn command_line(config: &Config) -> Result<String, AgentError> {
    let conversation = config.conversation_id.as_deref().filter(|id| {
        id.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    });
    match config.cli.as_str() {
        "claude" => Ok(match (conversation, config.resume) {
            (Some(id), true) => format!("claude --resume {id} --add-dir \"$VOGT_SESSION_CWD\""),
            (Some(id), false) => {
                format!("claude --session-id {id} --add-dir \"$VOGT_SESSION_CWD\"")
            }
            (None, _) => "claude --add-dir \"$VOGT_SESSION_CWD\"".into(),
        }),
        "codex" => Ok("codex --add-dir \"$VOGT_SESSION_CWD\"".into()),
        _ => Err(AgentError::Pty("VOGT_CLI must be claude or codex".into())),
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
                                    Frame::Signal { signal } if signal == "kill" => {
                                        if let Some(c) = child.as_mut() { c.kill().map_err(|e| AgentError::Pty(e.to_string()))?; }
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
                                send_frame(&mut sink, &Frame::Exited { status: Some(status.exit_code() as i32) }).await?;
                                return Ok(());
                            }
                        }
                        send_frame(&mut sink, &Frame::Heartbeat { cursor }).await?;
                    }
                }
            }
        }.await;
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
            "claude --add-dir \"$VOGT_SESSION_CWD\""
        );
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
