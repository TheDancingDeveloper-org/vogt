//! The pod-side Vogt session agent.
//!
//! The agent owns the PTY and makes the connection to the engine; a session pod
//! never listens for an engine connection. See the crate README for the wire
//! protocol.

pub mod codex_guidance;
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

/// How deep a Codex rollout may sit below `<home>/sessions`. Codex writes
/// `sessions/<yyyy>/<mm>/<dd>/<rollout>` (depth 4); one spare level, never an
/// unbounded walk.
const CODEX_ROLLOUT_MAX_DEPTH: usize = 5;

/// Whether `file` is Codex's rollout for `conversation`. Codex names it
/// `rollout-<timestamp>-<conversation>.jsonl`; the `rollout-` prefix keeps an
/// unrelated file with the same suffix out.
fn is_codex_rollout_name(file: &str, conversation: &str) -> bool {
    !conversation.is_empty()
        && !conversation.contains(['/', '\\'])
        && file.starts_with("rollout-")
        && file.ends_with(&format!("-{conversation}.jsonl"))
}

/// Whether `name` is the transcript bundle path for `conversation`, in either
/// agent's layout: Claude's `<conversation>.jsonl`, anywhere under its account
/// bundle, or Codex's `sessions/<date>/rollout-<timestamp>-<conversation>.jsonl`.
/// The engine asks this of a restored bundle to decide whether the pod can
/// resume; the pod asks it of `$CODEX_HOME/sessions` to find the rollout it
/// writes back, so the two cannot disagree about the name (WI-3873).
pub fn is_conversation_transcript(name: &str, conversation: &str) -> bool {
    if conversation.is_empty()
        || conversation == "."
        || conversation == ".."
        || conversation.contains(['/', '\\'])
    {
        return false;
    }
    // The first component selects the layout, as the bundle contract does:
    // `sessions/…` is Codex's rollout tree, everything else is Claude's
    // account bundle. A plain `<conversation>.jsonl` under `sessions/` is not
    // a Codex rollout and must not make the engine claim it can resume.
    let file = name.rsplit('/').next().unwrap_or(name);
    if name.split('/').next() == Some("sessions") {
        is_codex_rollout_name(file, conversation)
    } else {
        file == format!("{conversation}.jsonl")
    }
}

/// The session's own Codex rollout under `<home>/sessions`, found without
/// following symlinks. The file is `rollout-<timestamp>-<conversation>.jsonl`.
pub fn codex_transcript_path(home: &Path, conversation: &str) -> Option<PathBuf> {
    if conversation.is_empty() || conversation.contains(['/', '\\']) {
        return None;
    }
    walkdir::WalkDir::new(home.join("sessions"))
        .follow_links(false)
        .max_depth(CODEX_ROLLOUT_MAX_DEPTH)
        .into_iter()
        .filter_map(Result::ok)
        .find(|entry| {
            let name = entry.file_name().to_string_lossy();
            entry.file_type().is_file() && is_codex_rollout_name(&name, conversation)
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
    /// Where Codex keeps its session rollouts (`$CODEX_HOME`, else
    /// `$HOME/.codex`); a Codex pod has no Claude directory at all.
    pub codex_home: Option<PathBuf>,
    /// The read-only workspace tree the pod mounts (`VOGT_WORKSPACE_ROOT`,
    /// WI-3373). Unset means a pod with no workspace, which gets no pod-side
    /// guidance (the engine's host-computed body is used if it carried one).
    pub workspace_root: Option<PathBuf>,
    pub account: Option<String>,
    pub conversation_id: Option<String>,
    /// Resume the conversation from the transcript the engine seeded into the
    /// bundle instead of starting it (a pod woken from hibernation).
    pub resume: bool,
    /// The posture the engine started this session with (`VOGT_PERMISSION_MODE`),
    /// as the host launcher receives it. `None` is the default posture.
    pub permission_mode: Option<String>,
    /// The model the engine started this session with (`VOGT_MODEL`), as the
    /// host launcher receives it (WI-3881).
    pub model: Option<String>,
    /// The reasoning effort the engine started this session with
    /// (`VOGT_EFFORT`), as the host launcher receives it (WI-3881).
    pub effort: Option<String>,
    /// Codex's `approval_policy` for the default and accept-edits postures
    /// (`VOGT_CODEX_APPROVAL_POLICY`), defaulting to
    /// [`CODEX_APPROVAL_DEFAULT`] (WI-3864).
    pub codex_approval_policy: String,
    /// The files Claude Code is started with, as the engine starts one on a
    /// host (WI-3243).
    pub claude: ClaudeLaunch,
    /// The extra Codex launch data the engine cannot put in the pod's
    /// environment directly (WI-3882): guidance, charter, a compact limit and
    /// the template's own arguments.
    pub codex: CodexLaunch,
}

/// Where the image installs the default driven-session policy; the same path
/// as the merged image, so the engine's documentation of it holds for pods.
pub const IMAGE_SETTINGS_FILE: &str = "/usr/local/share/vogt/driven-session-settings.json";

/// The default checkout directory used by `deploy/session-entrypoint.sh`.
/// Keep the pod launcher's default in the same crate as the command builder so
/// a pod-backed agent and the entrypoint agree about the project Codex trusts.
pub const DEFAULT_WORK_DIR: &str = "/work";

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

impl CodexLaunch {
    /// Read the engine's per-launch Codex data from `VOGT_CODEX_LAUNCH_B64`
    /// and the entrypoint's full bootstrap fragment from
    /// `VOGT_CODEX_CONFIG_FILE`. A missing or malformed value is no extra
    /// data, not a launch failure: the pod still starts on its config file.
    pub fn from_env() -> Self {
        use base64::Engine as _;
        let mut launch = std::env::var("VOGT_CODEX_LAUNCH_B64")
            .ok()
            .and_then(|encoded| {
                base64::engine::general_purpose::STANDARD
                    .decode(encoded.trim())
                    .ok()
            })
            .and_then(|bytes| serde_json::from_slice::<CodexLaunch>(&bytes).ok())
            .unwrap_or_default();
        launch.config_file = std::env::var_os("VOGT_CODEX_CONFIG_FILE")
            .filter(|path| !path.is_empty())
            .map(PathBuf::from);
        launch
    }
}

/// `value` as one shell word, so a path from the environment cannot add
/// words or run anything on the line `command_line` builds.
fn sh_quote(value: &Path) -> String {
    format!("'{}'", value.to_string_lossy().replace('\'', "'\\''"))
}

/// Whether `name` is a shell variable name. Provider keys reach this crate in
/// the bootstrap-written Codex fragment, which is data: anything that is not
/// a plain name is never put on the launch line.
fn is_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

/// The shell prefix that hands the Codex child the brokered values it needs
/// (WI-3886, WI-4121). The bootstrap response and the Codex fragment carry
/// only names: provider `env_key`s and the `bearer_token_env_var` each MCP
/// server names. Each value is fetched through the pod's agent-auth broker
/// immediately before Codex starts and exported in the launch shell, so it
/// exists in the CLI's environment and nowhere in the pod's long-lived
/// process tree or its config. A name already set (the Vogt session token,
/// for instance) is left alone. A key that cannot be fetched is reported by
/// name and left unset: it is optional, and one broker failure must not stop
/// a session that may not use it.
fn codex_broker_fetch_prefix(providers: &[CodexProvider], mcp_bearer_envs: &[String]) -> String {
    let mut keys: Vec<&str> = providers
        .iter()
        .map(|provider| provider.env_key.as_str())
        .chain(mcp_bearer_envs.iter().map(String::as_str))
        .filter(|name| is_env_name(name))
        .collect();
    keys.sort_unstable();
    keys.dedup();
    if keys.is_empty() {
        return String::new();
    }
    let mut line = String::from("for __vogt_key in");
    for key in keys {
        line.push(' ');
        line.push_str(key);
    }
    line.push_str(
        "; do if eval \"[ -n \\\"\\${$__vogt_key:-}\\\" ]\"; then continue; fi; \
         if __vogt_value=\"$(vogt-agent-auth fetch \"$__vogt_key\")\"; then \
         export \"$__vogt_key=$__vogt_value\"; else \
         printf 'session-agent: brokered key %s was not delivered\\n' \"$__vogt_key\" >&2; \
         fi; done; unset __vogt_key __vogt_value; ",
    );
    line
}

fn toml_basic_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\u{08}' => out.push_str("\\b"),
            '\u{0C}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch.is_control() => out.push_str(&format!("\\u{:04X}", ch as u32)),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
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
            codex_home: std::env::var_os("CODEX_HOME")
                .map(PathBuf::from)
                .or_else(|| {
                    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex"))
                }),
            workspace_root: std::env::var_os("VOGT_WORKSPACE_ROOT")
                .filter(|root| !root.is_empty())
                .map(PathBuf::from),
            account: std::env::var("VOGT_SESSION_ACCOUNT").ok(),
            conversation_id: std::env::var("VOGT_SESSION_CONVERSATION_ID")
                .ok()
                .filter(|v| !v.is_empty()),
            resume: std::env::var("VOGT_SESSION_RESUME").is_ok_and(|v| v == "1"),
            permission_mode: std::env::var("VOGT_PERMISSION_MODE")
                .ok()
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty()),
            model: std::env::var("VOGT_MODEL")
                .ok()
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty()),
            effort: std::env::var("VOGT_EFFORT")
                .ok()
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty()),
            codex_approval_policy: std::env::var("VOGT_CODEX_APPROVAL_POLICY")
                .ok()
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| CODEX_APPROVAL_DEFAULT.to_owned()),
            claude: ClaudeLaunch::from_env(),
            codex: CodexLaunch::from_env(),
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
/// a transport failure is retried on the next flush. Claude's account files
/// need a Claude root and account; a Codex pod has neither and still ships its
/// rollout.
async fn flush_bundle(
    config: &Config,
    tls: &Arc<rustls::ClientConfig>,
    seen: &mut std::collections::HashMap<PathBuf, [u8; 32]>,
    offsets: &mut std::collections::HashMap<PathBuf, u64>,
) -> bool {
    // The login and global config are the Claude account's and are written
    // back with or without a conversation (WI-3334).
    let claude = match (
        config.claude_config_dir.as_deref(),
        config.account.as_deref(),
    ) {
        (Some(root), Some(account)) => Some((root, account)),
        _ => None,
    };
    let conversation = config.conversation_id.as_deref().unwrap_or("");
    if claude.is_none() && (conversation.is_empty() || config.codex_home.is_none()) {
        return true;
    }
    // The broker token is a credential and must only ever go to the engine.
    // In particular, do not let ambient HTTP(S)_PROXY redirect this request.
    let Ok(client) = http_client(tls.clone()) else {
        return false;
    };
    let mut transcript_ok = true;
    let mut url = config.engine_url.clone();
    url.push_str("/api/agent-auth/store/bundle/bundle-file");
    if let Some((root, account)) = claude {
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
                // The engine orders `.claude.json` writes by this and refuses
                // one without it: the file's own modification time, in
                // milliseconds.
                match file_version(&file.path) {
                    Some(version) => body["version"] = serde_json::json!(version),
                    None => continue,
                }
            }
            // A reply the engine will give again for the same bytes is final.
            // A rate limit or a server fault is not: the file is tried again
            // on the next flush.
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
    }
    // A Claude transcript lives under `CLAUDE_CONFIG_DIR/projects`; a Codex
    // pod's rollout lives under `$CODEX_HOME/sessions`. Both travel as
    // append-only deltas and share the path contract.
    let codex_transcript = || {
        config
            .codex_home
            .as_deref()
            .and_then(|home| codex_transcript_path(home, conversation).map(|path| (path, home, "")))
    };
    let transcript = if conversation.is_empty() {
        None
    } else {
        claude
            .and_then(|(root, account)| {
                transcript_path(root, conversation).map(|path| (path, root, account))
            })
            .or_else(codex_transcript)
    };
    if let Some((path, base, account)) = transcript {
        // Caught up when the delta is empty; any send failure leaves it behind.
        let mut caught_up = false;
        let relative = path
            .strip_prefix(base)
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

/// The `approval_policy` a driven Codex starts with when the deployment names
/// none (WI-2028). `never` matches the Claude Code workers, whose permissions
/// are bypassed: a worker runs the commands its task needs instead of stopping
/// at `-a on-request` for a person, or an oversight session, to answer.
pub const CODEX_APPROVAL_DEFAULT: &str = "never";

/// The `-a` values Codex accepts. The deployment's value is passed through
/// unchanged; anything else is refused at startup rather than handed to a CLI
/// that would fail to launch (or read as a different policy).
pub const CODEX_APPROVAL_POLICIES: [&str; 4] = ["untrusted", "on-failure", "on-request", "never"];

/// A permission posture as Codex flags (WI-1143, WI-2028, WI-3864).
///
/// The pod is the isolation boundary and cannot create a user namespace, so
/// `workspace-write` (and `--approve-for-me`, which hard-wires it) would fail
/// the launch. Every posture therefore uses `-s danger-full-access`. The
/// default and accept-edits postures pin `-a <approval>`, where `approval` is
/// the deployment's [`CODEX_APPROVAL_DEFAULT`] (`never` unless it says
/// otherwise): a driven worker runs without asking. A deployment that restores
/// `on-request` gets the old behaviour, and accept-edits then also routes those
/// asks through Codex's automatic reviewer (`-c
/// approvals_reviewer="auto_review"`), without changing the sandbox. Bypass
/// drops both approvals and the sandbox and is a person's grant only.
///
/// `exec` is a `codex exec` launch. That subcommand rejects `-a`, so the
/// approval pin is left off; the sandbox, and for accept-edits the reviewer,
/// stay.
///
/// `None` means the caller did not ask for a posture. That is not the default
/// posture: a launch that omitted the flags would run the CLI's own sandbox,
/// which a pod cannot satisfy.
///
/// The host launcher (`agent_cli`) calls this same function; the pod's own
/// launch line does too, so the two cannot drift.
pub fn codex_permission_flags(
    mode: Option<&str>,
    exec: bool,
    approval: &str,
) -> Result<Option<Vec<String>>, String> {
    let sandbox = ["-s".to_string(), "danger-full-access".to_string()];
    let ask = if exec || approval.trim().is_empty() {
        Vec::new()
    } else {
        vec!["-a".to_string(), approval.trim().to_string()]
    };
    match mode.map(str::trim).filter(|m| !m.is_empty()) {
        None => Ok(None),
        Some("default") => Ok(Some(ask.into_iter().chain(sandbox).collect())),
        Some("accept-edits") | Some("accept_edits") => Ok(Some(
            ask.into_iter()
                .chain([
                    "-c".to_string(),
                    "approvals_reviewer=\"auto_review\"".to_string(),
                ])
                .chain(sandbox)
                .collect(),
        )),
        Some("bypass") => Ok(Some(vec![
            "--dangerously-bypass-approvals-and-sandbox".to_string()
        ])),
        Some(other) => Err(format!(
            "permission_mode {other:?} is not one of default, accept-edits, bypass"
        )),
    }
}

/// The `model_auto_compact_token_limit_scope` every Vogt Codex launch uses:
/// the fixed prefix counts too, so the backed-off limit is a total.
pub const CODEX_COMPACT_SCOPE: &str = "total";

/// Haiku-class models are launched with this window rather than the
/// deployment's global `model_context_window` (WI-3998, the rust-2 port of the
/// WI-3976 override). Haiku's real window is 200k; the global 850k would let it
/// reach the provider limit without compacting.
pub const HAIKU_CONTEXT_WINDOW: u64 = 200_000;
/// The total-scope auto-compact limit for a Haiku-class launch (about 85% of
/// [`HAIKU_CONTEXT_WINDOW`], leaving room for one large tool result).
pub const HAIKU_COMPACT_LIMIT: u64 = 170_000;

/// Whether the model a launch runs is Haiku-class: its name carries `haiku`
/// (`claude-org-haiku`, `claude-haiku-4-5`, a bare `haiku`).
fn haiku_class(model: Option<&str>) -> bool {
    model
        .map(str::trim)
        .is_some_and(|model| model.to_ascii_lowercase().contains("haiku"))
}

/// The MCP servers whose tools a driven Codex may call without a per-call
/// approval prompt. This is the same operator-approved list the host's
/// `mcp-bootstrap.sh` writes into `config.toml` (WI-3882); `komodo` is
/// deliberately absent because its writes change running stacks.
pub const CODEX_APPROVED_MCP_SERVERS: [&str; 5] =
    ["vogt", "cadastre", "github-ro", "forgejo-ro", "grafana-ro"];

/// One `model_providers.<name>` table as the shared Codex builder needs it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodexProvider {
    pub name: String,
    pub base_url: String,
    pub wire_api: String,
    pub env_key: String,
    pub context_window: Option<u64>,
    pub auto_compact_token_limit: Option<u64>,
}

/// The per-launch data the engine carries to a pod that is not already in its
/// environment or its bootstrap `codex_config` (WI-3882). It travels as one
/// base64 JSON value so a YAML/TSV environment path cannot mangle it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CodexLaunch {
    /// Workspace guidance gathered by the engine, as the host launch would
    /// pass it: `developer_instructions`.
    pub guidance: Option<String>,
    /// The scope's charters, appended to the guidance.
    pub charter: Option<String>,
    /// The compact backoff limit, when the engine overrides the configured one.
    pub compact_limit: Option<u64>,
    /// The template's own arguments after the Codex binary (`-c ...`, `-m ...`),
    /// which a pod otherwise drops.
    pub template_args: Vec<String>,
    /// Where the entrypoint wrote the full bootstrap Codex fragment. Unset on
    /// the host; a pod's entrypoint exports `VOGT_CODEX_CONFIG_FILE`.
    pub config_file: Option<PathBuf>,
}

/// Everything a Codex launch carries beyond the binary (WI-3882).
///
/// The host launcher (`agent_cli::launch_with_codex`) and the pod's own
/// `command_line` both build their argv from this, so no Codex `-c` override
/// can exist in one and be missing from the other.
pub struct CodexArgs<'a> {
    /// The template's own arguments after the binary, in their original order.
    pub command_args: &'a [String],
    /// A previous conversation to `resume`, or a fresh id to pin.
    pub resume: Option<&'a str>,
    pub session_id: Option<&'a str>,
    /// The model the caller asked for, as `-m`.
    pub model: Option<&'a str>,
    /// The provider registry's model, as `-c model=...`, only when `model` is
    /// not named (the pod's bootstrap config selects one by default).
    pub config_model: Option<&'a str>,
    pub effort: Option<&'a str>,
    pub permission_mode: Option<&'a str>,
    pub approval_policy: &'a str,
    /// A `codex exec` launch: it rejects `-a`.
    pub exec: bool,
    pub compact_limit: Option<u64>,
    /// The selected `model_provider` registry key, as `-c model_provider=...`.
    pub provider: Option<&'a str>,
    pub providers: &'a [CodexProvider],
    /// A model catalog written for this launch, as `-c model_catalog_json=...`
    /// (a provider-registered model absent from the shared catalog).
    pub catalog: Option<&'a Path>,
    /// The checkout Codex should trust without asking (`projects={...}`).
    pub trust_cwd: Option<&'a str>,
    pub guidance: Option<&'a str>,
    pub charter: Option<&'a str>,
    /// MCP servers present in this launch, for `default_tools_approval_mode`.
    pub mcp_servers: &'a [String],
    /// The host's MCP trim overrides (`-c mcp_servers.NAME.enabled=...`).
    pub mcp_args: &'a [String],
    pub prompt: Option<&'a str>,
}

const MAX_CHARTER_INLINE_BYTES: usize = 4 * 1024;
const MAX_CHARTER_ARG_BYTES: usize = 120 * 1024;
const CHARTER_POINTER: &str = "## Charters\n\nThis scope has standing instructions \
(charters) too long to repeat here. They are in the `## Charters` section of \
the session brief; follow them as if they were written here.";

fn provider_inline(p: &CodexProvider) -> String {
    let s = toml_basic_string;
    format!(
        "{{ name = {}, base_url = {}, wire_api = {}, env_key = {}, requires_openai_auth = false{}{} }}",
        s(&p.name),
        s(&p.base_url),
        s(&p.wire_api),
        s(&p.env_key),
        p.context_window.map(|v| format!(", context_window = {v}")).unwrap_or_default(),
        p.auto_compact_token_limit.map(|v| format!(", auto_compact_token_limit = {v}")).unwrap_or_default()
    )
}

/// The `developer_instructions` override for a launch, with the scope's
/// charters appended after the workspace guidance (WI-2038). A charter over
/// the inline cap becomes a pointer to the brief; one that would push the
/// argument over Linux's argv limit is left to the brief alone.
pub fn codex_developer_instructions(
    guidance: Option<&str>,
    charter: Option<&str>,
) -> Option<String> {
    let guidance = guidance.map(str::trim).filter(|text| !text.is_empty());
    let charter = charter.map(str::trim).filter(|text| !text.is_empty());
    let charter = charter.map(|text| {
        if text.len() > MAX_CHARTER_INLINE_BYTES {
            tracing::info!(
                bytes = text.len(),
                "charter over the inline cap; Codex gets a pointer to the brief"
            );
            CHARTER_POINTER
        } else {
            text
        }
    });
    let mut body = match (guidance, charter) {
        (Some(guidance), Some(charter)) => format!("{guidance}\n\n{charter}"),
        (Some(guidance), None) => guidance.to_string(),
        (None, Some(charter)) => charter.to_string(),
        (None, None) => return None,
    };
    let mut value = format!("developer_instructions={}", toml_basic_string(&body));
    if value.len() > MAX_CHARTER_ARG_BYTES {
        tracing::warn!(
            bytes = value.len(),
            "codex developer_instructions with charter over the argv cap; charter left to the brief"
        );
        body = guidance?.to_string();
        value = format!("developer_instructions={}", toml_basic_string(&body));
    }
    Some(value)
}

/// Codex guidance for a pod that mounts the workspace tree (WI-3374). The
/// engine gathers a host session's body from its own filesystem, but a pod's
/// tree lives in a per-pod emptyDir it cannot see, so the pod's agent gathers
/// it here from the mount it does see. The tree is the top of the walk: a pod
/// has none of the host `$HOME` assets, and every pointer stays inside the
/// read-only mount the container can open. A cwd outside the tree gets
/// nothing, and the engine's body (if it sent one) still applies.
fn pod_workspace_guidance(config: &Config, cwd: &str) -> Option<String> {
    let root = config.workspace_root.as_deref()?;
    let cwd = Path::new(cwd);
    if !cwd.starts_with(root) {
        return None;
    }
    crate::codex_guidance::developer_instructions(cwd, Some(root))
}

/// Codex's arguments after the binary, in the host launcher's canonical order
/// (WI-3882). Both a host launch and a pod's `command_line` call this.
pub fn codex_args(args: &CodexArgs<'_>) -> Result<Vec<String>, String> {
    let mut argv = Vec::new();
    if let Some(id) = args.resume {
        argv.extend(["resume".to_string(), id.to_string()]);
    }
    if let Some(provider) = args.provider.map(str::trim).filter(|name| !name.is_empty()) {
        argv.extend([
            "-c".to_string(),
            format!("model_provider={}", toml_basic_string(provider)),
        ]);
    }
    for provider in args.providers {
        argv.extend([
            "-c".to_string(),
            format!(
                "model_providers.{}={}",
                provider.name,
                provider_inline(provider)
            ),
        ]);
    }
    if let Some(catalog) = args.catalog {
        argv.extend([
            "-c".to_string(),
            format!(
                "model_catalog_json={}",
                toml_basic_string(&catalog.to_string_lossy())
            ),
        ]);
    }
    // The selected provider supplies the context window; its compact limit is
    // emitted once, below, with the backoff (WI-3871). Both are authoritative
    // for that provider and never fall back to the deployment defaults.
    let selected = args
        .provider
        .and_then(|name| args.providers.iter().find(|p| p.name == name));
    // The model a launch runs: `-m` when named, else the registry's model.
    let effective_model = args
        .model
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .or_else(|| {
            args.config_model
                .map(str::trim)
                .filter(|model| !model.is_empty())
        });
    let haiku = haiku_class(effective_model);
    if haiku {
        // One window for the key: the provider's own window would be a second
        // `model_context_window`, and Codex takes the last one.
        argv.extend([
            "-c".to_string(),
            format!("model_context_window={HAIKU_CONTEXT_WINDOW}"),
        ]);
    } else if let Some(window) = selected.and_then(|p| p.context_window) {
        argv.extend(["-c".to_string(), format!("model_context_window={window}")]);
    }
    if let Some(cwd) = args.trust_cwd.map(str::trim).filter(|cwd| !cwd.is_empty()) {
        // WI-4025: a trailing slash names the same directory but a different
        // `projects` key to Codex, so trust would miss and the folder-trust
        // screen would stay up. Strip it, keeping a bare root as `/`.
        let trimmed = cwd.trim_end_matches('/');
        let trimmed = if trimmed.is_empty() { cwd } else { trimmed };
        let key = toml_basic_string(trimmed);
        argv.extend([
            "-c".to_string(),
            format!("projects={{{key}={{trust_level=\"trusted\"}}}}"),
        ]);
    }
    if let Some(value) = codex_developer_instructions(args.guidance, args.charter) {
        argv.extend(["-c".to_string(), value]);
    }
    for name in args.mcp_servers {
        if CODEX_APPROVED_MCP_SERVERS.contains(&name.as_str()) {
            argv.extend([
                "-c".to_string(),
                format!("mcp_servers.{name}.default_tools_approval_mode=\"approve\""),
            ]);
        }
    }
    // A client that does not go through the wrapper must not copy the pinned
    // binary out from under it (WI-1149); the host's bootstrap writes this into
    // the config, the pod's config is written by its entrypoint. Passing it on
    // the argv makes both launches say the same thing.
    argv.extend([
        "-c".to_string(),
        "features.daemon_auto_start=false".to_string(),
    ]);
    argv.extend(args.command_args.iter().cloned());
    if let Some(model) = args.model.map(str::trim).filter(|model| !model.is_empty()) {
        argv.extend(["-m".to_string(), model.to_string()]);
    } else if let Some(model) = args
        .config_model
        .map(str::trim)
        .filter(|model| !model.is_empty())
    {
        argv.extend([
            "-c".to_string(),
            format!("model={}", toml_basic_string(model)),
        ]);
    }
    if let Some(effort) = args
        .effort
        .map(str::trim)
        .filter(|effort| !effort.is_empty())
    {
        argv.extend(["-c".to_string(), format!("model_reasoning_effort={effort}")]);
    }
    let mode = args
        .permission_mode
        .map(str::trim)
        .filter(|mode| !mode.is_empty())
        .unwrap_or("default");
    if let Some(flags) = codex_permission_flags(Some(mode), args.exec, args.approval_policy)? {
        argv.extend(flags);
    }
    argv.extend([
        "-c".to_string(),
        "tui.terminal_title=[\"agent-state\"]".to_string(),
        "-c".to_string(),
        "tui.animations=false".to_string(),
    ]);
    // One emission per key: the selected provider's limit is the base, and an
    // engine backoff (computed from that base in `sessions`) may only raise
    // it. Emitting a second, lower value here would silently win, since Codex
    // takes the last `-c` for a key.
    let compact_limit = if haiku {
        // A backoff may still raise a Haiku launch, but never past the ceiling
        // at which Codex stops compacting for this window.
        let backoff = args
            .compact_limit
            .map_or(0, |limit| limit.min(HAIKU_CONTEXT_WINDOW / 10 * 9));
        Some(HAIKU_COMPACT_LIMIT.max(backoff))
    } else {
        [
            selected.and_then(|provider| provider.auto_compact_token_limit),
            args.compact_limit,
        ]
        .into_iter()
        .flatten()
        .max()
    };
    if let Some(limit) = compact_limit {
        argv.extend([
            "-c".to_string(),
            format!("model_auto_compact_token_limit={limit}"),
        ]);
        if args.compact_limit.is_some() || haiku {
            argv.extend([
                "-c".to_string(),
                format!("model_auto_compact_token_limit_scope=\"{CODEX_COMPACT_SCOPE}\""),
            ]);
        }
    }
    argv.extend(args.mcp_args.iter().cloned());
    if let Some(id) = args.session_id {
        argv.extend(["--session-id".to_string(), id.to_string()]);
    }
    if let Some(prompt) = args.prompt {
        argv.push(prompt.to_string());
    }
    Ok(argv)
}

/// A permission posture as Claude Code's flags (WI-926, WI-3881).
///
/// `None` is the default posture: no flag at all. The host launcher
/// (`agent_cli::permission_flags`) and the pod's own `claude_args` both call
/// this, so a posture cannot mean one thing on a host and another in a pod.
pub fn claude_permission_flags(mode: Option<&str>) -> Result<Option<Vec<String>>, String> {
    match mode.map(str::trim).filter(|m| !m.is_empty()) {
        None | Some("default") => Ok(None),
        Some("accept-edits") | Some("accept_edits") => Ok(Some(vec![
            "--permission-mode".to_string(),
            "acceptEdits".to_string(),
        ])),
        Some("bypass") => Ok(Some(vec!["--dangerously-skip-permissions".to_string()])),
        Some(other) => Err(format!(
            "permission_mode {other:?} is not one of default, accept-edits, bypass"
        )),
    }
}

/// Everything a Claude Code launch carries beyond the binary (WI-3881).
///
/// The host launcher (`agent_cli::launch`) and the pod's own `command_line`
/// both build their argv from this, so no Claude flag can exist in one and be
/// missing from the other.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClaudeArgs<'a> {
    pub model: Option<&'a str>,
    pub effort: Option<&'a str>,
    /// `(id, resume)`: a previous conversation to `--resume`, or a fresh id to
    /// pin with `--session-id`.
    pub conversation: Option<(&'a str, bool)>,
    pub permission_mode: Option<&'a str>,
    /// The driven-session policy, `--settings=<file>`.
    pub settings: Option<&'a Path>,
    /// The engine's MCP set, `--strict-mcp-config --mcp-config=<file>`.
    pub mcp_config: Option<&'a Path>,
    /// Directories Claude Code may read outside its cwd, as `--add-dir=<dir>`.
    pub add_dirs: &'a [String],
    /// The positional first prompt, which goes last.
    pub prompt: Option<&'a str>,
}

/// Claude Code's arguments after the binary, in the host launcher's order
/// (WI-3881).
///
/// The `=` forms keep `--settings`, `--mcp-config` and `--add-dir` from
/// taking the next word: Claude Code reads each as variadic, and the prompt
/// after them is positional.
pub fn claude_args(args: &ClaudeArgs<'_>) -> Result<Vec<String>, String> {
    let mut argv = Vec::new();
    if let Some(model) = args.model.map(str::trim).filter(|m| !m.is_empty()) {
        argv.extend(["--model".to_string(), model.to_string()]);
    }
    if let Some(effort) = args.effort.map(str::trim).filter(|e| !e.is_empty()) {
        argv.extend(["--effort".to_string(), effort.to_string()]);
    }
    if let Some((id, resume)) = args.conversation {
        argv.extend([
            if resume { "--resume" } else { "--session-id" }.to_string(),
            id.to_string(),
        ]);
    }
    if let Some(flags) = claude_permission_flags(args.permission_mode)? {
        argv.extend(flags);
    }
    if let Some(file) = args.settings {
        argv.push(format!("--settings={}", file.display()));
    }
    if let Some(file) = args.mcp_config {
        argv.push("--strict-mcp-config".to_string());
        argv.push(format!("--mcp-config={}", file.display()));
    }
    for dir in args.add_dirs {
        argv.push(format!("--add-dir={dir}"));
    }
    if let Some(prompt) = args.prompt {
        argv.push(prompt.to_string());
    }
    Ok(argv)
}

/// The provider, model and MCP tables the entrypoint wrote for this pod.
#[derive(Debug, Default)]
struct CodexConfigFile {
    model_provider: Option<String>,
    model: Option<String>,
    providers: Vec<CodexProvider>,
    mcp_servers: Vec<String>,
    /// The `bearer_token_env_var` each MCP server names (WI-4121), so the
    /// launch line resolves those through the broker too.
    mcp_bearer_envs: Vec<String>,
}

fn codex_file_config(
    launch: &CodexLaunch,
    codex_home: Option<&Path>,
) -> Result<CodexConfigFile, AgentError> {
    let path = launch.config_file.clone().or_else(|| {
        codex_home.map(|home| {
            let fragment = home.join("vogt-codex.toml");
            if fragment.is_file() {
                fragment
            } else {
                home.join("config.toml")
            }
        })
    });
    let Some(path) = path else {
        return Ok(CodexConfigFile::default());
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(CodexConfigFile::default())
        }
        Err(error) => {
            return Err(AgentError::Pty(format!(
                "reading Codex config {}: {error}",
                path.display()
            )))
        }
    };
    let table: toml::Table = toml::from_str(&text).map_err(|error| {
        AgentError::Pty(format!(
            "Codex config {} is not valid TOML: {error}",
            path.display()
        ))
    })?;
    let mut file = CodexConfigFile {
        model_provider: table
            .get("model_provider")
            .and_then(toml::Value::as_str)
            .map(str::to_owned),
        model: table
            .get("model")
            .and_then(toml::Value::as_str)
            .map(str::to_owned),
        ..Default::default()
    };
    if let Some(providers) = table.get("model_providers").and_then(toml::Value::as_table) {
        for (name, value) in providers {
            let Some(table) = value.as_table() else {
                continue;
            };
            let Some(base_url) = table.get("base_url").and_then(toml::Value::as_str) else {
                continue;
            };
            let Some(wire_api) = table.get("wire_api").and_then(toml::Value::as_str) else {
                continue;
            };
            let Some(env_key) = table.get("env_key").and_then(toml::Value::as_str) else {
                continue;
            };
            file.providers.push(CodexProvider {
                name: name.clone(),
                base_url: base_url.to_string(),
                wire_api: wire_api.to_string(),
                env_key: env_key.to_string(),
                context_window: table
                    .get("context_window")
                    .and_then(toml::Value::as_integer)
                    .and_then(|v| u64::try_from(v).ok()),
                auto_compact_token_limit: table
                    .get("auto_compact_token_limit")
                    .and_then(toml::Value::as_integer)
                    .and_then(|v| u64::try_from(v).ok()),
            });
        }
    }
    if let Some(servers) = table.get("mcp_servers").and_then(toml::Value::as_table) {
        file.mcp_servers = servers.keys().cloned().collect();
        for value in servers.values() {
            if let Some(name) = value
                .as_table()
                .and_then(|server| server.get("bearer_token_env_var"))
                .and_then(toml::Value::as_str)
            {
                file.mcp_bearer_envs.push(name.to_string());
            }
        }
    }
    Ok(file)
}

/// The shell line that starts the CLI. A conversation is pinned to the
/// engine's conversation id (`--session-id` for Claude and Codex) or resumed
/// from the transcript the engine seeded (`--resume` for Claude, `codex
/// resume <id>` for Codex), so the transcript this agent writes back is the
/// one a later wake resumes (WI-3873). Claude Code also gets the
/// driven-session policy and the engine's MCP set when the pod has them
/// (WI-3243), so a pod session has the permission posture a host session has.
pub fn command_line(config: &Config) -> Result<String, AgentError> {
    let conversation = config.conversation_id.as_deref().filter(|id| {
        id.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    });
    match config.cli.as_str() {
        "claude" => {
            // The same builder the host launcher uses (WI-3881), then shell
            // quoting only: the flags themselves exist in one place. The
            // entrypoint exports `VOGT_SESSION_CWD`, which `Config.cwd` is
            // read from, so the concrete directory is quoted here rather
            // than left for the shell to expand.
            let cwd = [config
                .cwd
                .clone()
                .unwrap_or_else(|| DEFAULT_WORK_DIR.to_string())];
            let prompt = config
                .claude
                .brief
                .as_deref()
                .filter(|_| !config.resume)
                .map(brief_instruction);
            let args = claude_args(&ClaudeArgs {
                model: config.model.as_deref(),
                effort: config.effort.as_deref(),
                conversation: conversation.map(|id| (id, config.resume)),
                permission_mode: config.permission_mode.as_deref(),
                settings: config.claude.settings.as_deref(),
                mcp_config: config.claude.mcp_config.as_deref(),
                add_dirs: &cwd,
                prompt: prompt.as_deref(),
            })
            .map_err(AgentError::Pty)?;
            let mut line = String::from("claude");
            for arg in args {
                line.push(' ');
                line.push_str(&sh_quote(Path::new(&arg)));
            }
            Ok(line)
        }
        // No `--add-dir`: Codex's effective sandbox refuses extra writable
        // roots and exits. The PTY already starts in the session cwd, which
        // is the workspace root Codex uses. The pod's `/work` carries a git
        // view, and Codex asks its folder-trust question for a git cwd it has
        // no trust entry for, which a fresh pod never has (WI-3839). The
        // entry is an inline table: `-c` splits its key on every `.`, so a
        // dotted `projects."/work".trust_level` names a project called
        // `"/work"`, quotes included, and never matches. `--skip-git-repo-check`
        // is an `exec` flag the interactive CLI refuses. The posture flags come
        // from `codex_permission_flags`, the same function the host launch
        // uses (WI-3864): the pod cannot create the user namespace the CLI's
        // default sandbox needs, so the absent posture is still `default`, not
        // "let the CLI decide" (`-s danger-full-access -a never`).
        "codex" => {
            // The same builder the host launcher uses (WI-3882). The provider
            // tables and model selection reach it from the bootstrap fragment;
            // guidance, charter, a compact limit and the template's own
            // arguments arrive in `VOGT_CODEX_LAUNCH_B64`.
            let file = codex_file_config(&config.codex, config.codex_home.as_deref())?;
            let cwd = config.cwd.as_deref().unwrap_or(DEFAULT_WORK_DIR);
            let workspace_guidance = pod_workspace_guidance(config, cwd);
            let base = config.codex.template_args.as_slice();
            let pin = conversation.filter(|_| !config.resume && base.is_empty());
            // The brief is the session's first prompt, as it is for Claude
            // Code and for a host Codex (WI-3355, WI-4068). The entrypoint
            // wrote the file before this process started; leaving it unread
            // opened a task session at an empty composer, so a session with
            // autopilot off never started a turn at all.
            let prompt = config
                .claude
                .brief
                .as_deref()
                .filter(|_| !config.resume)
                .map(brief_instruction);
            let args = codex_args(&CodexArgs {
                command_args: base,
                resume: conversation.filter(|_| config.resume),
                session_id: pin,
                model: config.model.as_deref(),
                config_model: file.model.as_deref(),
                effort: config.effort.as_deref(),
                permission_mode: config.permission_mode.as_deref(),
                approval_policy: &config.codex_approval_policy,
                exec: false,
                compact_limit: config.codex.compact_limit,
                provider: file.model_provider.as_deref(),
                providers: &file.providers,
                catalog: None,
                trust_cwd: Some(cwd),
                guidance: workspace_guidance
                    .as_deref()
                    .or(config.codex.guidance.as_deref()),
                charter: config.codex.charter.as_deref(),
                mcp_servers: &file.mcp_servers,
                mcp_args: &[],
                prompt: prompt.as_deref(),
            })
            .map_err(AgentError::Pty)?;
            let mut line = codex_broker_fetch_prefix(&file.providers, &file.mcp_bearer_envs);
            line.push_str("codex");
            for arg in args {
                line.push(' ');
                line.push_str(&sh_quote(Path::new(&arg)));
            }
            Ok(line)
        }
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

/// Whether this launch starts Codex: the agent CLI, or an argv naming it.
fn launches_codex(config: &Config) -> bool {
    match &config.command {
        None => config.cli == "codex",
        Some(argv) => argv.iter().any(|arg| {
            Path::new(arg)
                .file_name()
                .is_some_and(|name| name == "codex")
        }),
    }
}

/// Refuse a Codex launch whose managed policy hook this pod cannot enforce
/// (WI-3335). The engine does not check a pod session's hook against its own
/// filesystem, which is not the one Codex runs on; the pod checks its own,
/// fail closed: no requirements file, one that does not wire the hook, or a
/// hook program the session user could replace, and Codex does not start.
pub fn codex_guard(config: &Config, system: &Path, hook_bin: &Path) -> Result<(), AgentError> {
    if !launches_codex(config) {
        return Ok(());
    }
    codex_policy::guarded(system, hook_bin, true)
        .map(|_| ())
        .map_err(AgentError::Pty)
}

pub fn spawn_pty(config: &Config) -> Result<PtyIo, AgentError> {
    codex_guard(
        config,
        Path::new(codex_policy::CODEX_REQUIREMENTS_FILE),
        Path::new(codex_policy::CODEX_POLICY_HOOK_BIN),
    )?;
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
            codex_home: None,
            workspace_root: None,
            account: None,
            conversation_id: None,
            resume: false,
            model: None,
            effort: None,
            permission_mode: None,
            codex_approval_policy: CODEX_APPROVAL_DEFAULT.into(),
            claude: ClaudeLaunch::default(),
            codex: CodexLaunch::default(),
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
            codex_home: None,
            workspace_root: None,
            account: None,
            conversation_id: None,
            resume: false,
            model: None,
            effort: None,
            permission_mode: None,
            codex_approval_policy: CODEX_APPROVAL_DEFAULT.into(),
            claude: ClaudeLaunch::default(),
            codex: CodexLaunch::default(),
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

    /// A fake engine for bundle POSTs only: each request body is recorded and
    /// answered 200 with the offset after the bytes it carried.
    async fn record_bundle_posts() -> (String, Arc<std::sync::Mutex<Vec<serde_json::Value>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let bodies = Arc::new(std::sync::Mutex::new(Vec::new()));
        let record = bodies.clone();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let record = record.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 8192];
                    let (header_end, length) = loop {
                        let n = stream.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
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
                        let n = stream.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&chunk[..n]);
                    }
                    let body: serde_json::Value =
                        serde_json::from_slice(&buf[header_end..header_end + length]).unwrap();
                    let sent = body["bytes"]
                        .as_str()
                        .map(|bytes| {
                            base64::Engine::decode(
                                &base64::engine::general_purpose::STANDARD,
                                bytes,
                            )
                            .unwrap()
                            .len()
                        })
                        .unwrap_or(0);
                    let next = body["offset"].as_u64().unwrap_or(0) + sent as u64;
                    record.lock().unwrap().push(body);
                    let reply = format!("{{\"ok\":true,\"offset\":{next}}}");
                    let _ = stream
                        .write_all(
                            format!(
                                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{reply}",
                                reply.len()
                            )
                            .as_bytes(),
                        )
                        .await;
                });
            }
        });
        (format!("http://{address}"), bodies)
    }

    // WI-3868: a Codex pod has no Claude root or account; its own rollout must
    // still travel as a transcript delta at the path contract.
    #[tokio::test]
    async fn a_codex_only_config_flushes_the_rollout_at_its_contract_path() {
        let dir = tempfile::tempdir().unwrap();
        let relative = "sessions/2026/11/09/rollout-2026-11-09T00-00-00-conv1.jsonl";
        let rollout = dir.path().join(relative);
        std::fs::create_dir_all(rollout.parent().unwrap()).unwrap();
        let content = b"{\"type\":\"session\"}\n";
        std::fs::write(&rollout, content).unwrap();
        let (url, bodies) = record_bundle_posts().await;
        let mut config = kill_config(url);
        config.claude_config_dir = None;
        config.account = None;
        config.conversation_id = Some("conv1".into());
        config.codex_home = Some(dir.path().to_path_buf());
        let tls = tls_config().unwrap();
        let mut seen = std::collections::HashMap::new();
        let mut offsets = std::collections::HashMap::new();
        assert!(flush_bundle(&config, &tls, &mut seen, &mut offsets).await);
        let bodies = bodies.lock().unwrap();
        assert_eq!(bodies.len(), 1, "expected one delta: {bodies:?}");
        assert_eq!(bodies[0]["kind"], "transcript");
        assert_eq!(bodies[0]["account"], "");
        assert_eq!(bodies[0]["conversation_id"], "conv1");
        assert_eq!(bodies[0]["path"], relative);
        let bytes = base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            bodies[0]["bytes"].as_str().unwrap(),
        )
        .unwrap();
        assert_eq!(bytes, content);
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
            codex_home: None,
            workspace_root: None,
            account: None,
            conversation_id: None,
            resume: false,
            model: None,
            effort: None,
            permission_mode: None,
            codex_approval_policy: CODEX_APPROVAL_DEFAULT.into(),
            claude: ClaudeLaunch::default(),
            codex: CodexLaunch::default(),
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
            codex_home: None,
            workspace_root: None,
            account: None,
            conversation_id: Some("abc-123".into()),
            resume: false,
            model: None,
            effort: None,
            permission_mode: None,
            codex_approval_policy: CODEX_APPROVAL_DEFAULT.into(),
            claude: ClaudeLaunch::default(),
            codex: CodexLaunch::default(),
        };
        let argv = argv_of(&command_line(&config).unwrap());
        assert_eq!(&argv[..2], ["--session-id", "abc-123"]);
        config.resume = true;
        let argv = argv_of(&command_line(&config).unwrap());
        assert_eq!(&argv[..2], ["--resume", "abc-123"]);
        // An id that is not plain is never put on a shell line.
        config.conversation_id = Some("x; rm -rf /".into());
        assert_eq!(command_line(&config).unwrap(), "claude '--add-dir=/work'");
    }

    /// WI-3873: Codex, like Claude, must be told the engine's conversation id
    /// at launch. Without it Codex writes `rollout-<ts>-<its own uuid>.jsonl`,
    /// nothing matches `codex_transcript_path`, and wake starts fresh.
    #[test]
    fn codex_is_pinned_to_or_resumes_the_engine_conversation() {
        let mut config = claude_config(ClaudeLaunch::default(), false);
        config.cli = "codex".into();
        let argv = argv_of_cli("codex", &command_line(&config).unwrap());
        assert!(
            argv.windows(2).any(|w| w == ["--session-id", "abc-123"]),
            "{argv:?}"
        );
        config.resume = true;
        let argv = argv_of_cli("codex", &command_line(&config).unwrap());
        assert_eq!(&argv[..2], ["resume", "abc-123"], "{argv:?}");
        // An id that is not plain never reaches the shell line.
        config.resume = false;
        config.conversation_id = Some("x; rm -rf /".into());
        let argv = argv_of_cli("codex", &command_line(&config).unwrap());
        assert!(!argv.iter().any(|a| a == "--session-id"), "{argv:?}");
    }

    /// WI-3374: a pod with a workspace tree gathers its Codex guidance from
    /// that tree, because the engine cannot see a per-pod emptyDir. The
    /// checkout's own file is inlined; the workspace files above it are named.
    #[test]
    fn a_workspace_pod_gathers_codex_guidance_from_its_own_tree() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Working");
        let session = root.join("session");
        std::fs::create_dir_all(&session).unwrap();
        std::fs::create_dir_all(root.join(".claude/skills/ci-log")).unwrap();
        std::fs::create_dir_all(root.join(".claude/rules")).unwrap();
        std::fs::write(root.join("AGENTS.md"), "workspace agents body").unwrap();
        std::fs::write(root.join("CLAUDE.md"), "workspace claude body").unwrap();
        std::fs::write(
            root.join(".claude/rules/redact-before-print.md"),
            "redact rule",
        )
        .unwrap();
        std::fs::write(session.join("CLAUDE.md"), "checkout claude body").unwrap();
        std::fs::write(root.join(".claude/skills/ci-log/SKILL.md"), "# ci-log").unwrap();

        let mut config = claude_config(ClaudeLaunch::default(), false);
        config.cli = "codex".into();
        config.cwd = Some(session.display().to_string());
        config.workspace_root = Some(root.clone());
        let argv = argv_of_cli("codex", &command_line(&config).unwrap());
        let value = argv
            .iter()
            .find_map(|arg| arg.strip_prefix("developer_instructions="))
            .unwrap_or_else(|| panic!("no guidance in {argv:?}"));
        let parsed: toml::Table = toml::from_str(&format!("v = {value}")).unwrap();
        let body = parsed["v"].as_str().unwrap();
        assert!(body.contains("checkout claude body"), "{body}");
        assert!(
            body.contains(root.to_str().unwrap()),
            "workspace root named: {body}"
        );
        assert!(body.contains("AGENTS.md"), "{body}");
        assert!(body.contains(".claude/rules"), "{body}");
        assert!(body.contains(".claude/skills"), "{body}");
        assert!(
            !body.contains("workspace agents body"),
            "workspace files are named, not inlined: {body}"
        );

        // Without the workspace the launch keeps whatever the engine sent.
        config.workspace_root = None;
        config.codex.guidance = Some("engine body".into());
        let argv = argv_of_cli("codex", &command_line(&config).unwrap());
        assert!(
            argv.iter()
                .any(|arg| arg.contains("developer_instructions=") && arg.contains("engine body")),
            "{argv:?}"
        );
    }

    /// WI-3886: the provider key names reach the launch line; the values do
    /// not. Only plain variable names are put on the line, so a fragment the
    /// engine never wrote cannot inject a shell word.
    #[test]
    fn a_codex_provider_key_prefix_names_plain_variables_only() {
        let provider = |env_key: &str| CodexProvider {
            name: "p".into(),
            base_url: "http://litellm:4000/v1".into(),
            wire_api: "chat".into(),
            env_key: env_key.into(),
            context_window: None,
            auto_compact_token_limit: None,
        };
        let prefix = codex_broker_fetch_prefix(
            &[provider("DS_KEY"), provider("BAD-KEY"), provider("DS_KEY")],
            &[
                "CADASTRE_HTTP_TOKEN".into(),
                "BAD NAME".into(),
                String::new(),
            ],
        );
        assert!(
            prefix.starts_with("for __vogt_key in CADASTRE_HTTP_TOKEN DS_KEY;"),
            "{prefix}"
        );
        assert!(!prefix.contains("BAD-KEY"), "{prefix}");
        assert!(!prefix.contains("BAD NAME"), "{prefix}");
        assert_eq!(codex_broker_fetch_prefix(&[], &[]), "");
    }

    /// WI-3886: the key is fetched through the pod broker immediately before
    /// Codex starts, so the CLI receives it and the launch line carries only
    /// its name. A failed fetch is reported and leaves Codex runnable.
    #[test]
    fn a_codex_child_receives_its_provider_key_from_the_broker() {
        let dir = tempfile::tempdir().unwrap();
        let fragment = dir.path().join("vogt-codex.toml");
        std::fs::write(
            &fragment,
            "model_providers.ds = { name = \"ds\", base_url = \"http://litellm:4000/v1\", \
             wire_api = \"chat\", env_key = \"DS_KEY\" }\n",
        )
        .unwrap();
        let mut config = claude_config(ClaudeLaunch::default(), false);
        config.cli = "codex".into();
        config.codex.config_file = Some(fragment);
        let line = command_line(&config).unwrap();
        assert!(line.contains("vogt-agent-auth fetch"), "{line}");
        assert!(!line.contains("fetched-secret"), "{line}");

        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let helper = bin.join("vogt-agent-auth");
        std::fs::write(
            &helper,
            "#!/bin/sh\ncase \"$1\" in\nfetch)\n  [ -z \"${HELPER_FAIL:-}\" ] || { printf 'broker down\\n' >&2; exit 1; }\n  printf fetched-secret ;;\n*) exit 1 ;;\nesac\n",
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );

        let run = |fail: bool| {
            std::process::Command::new("/bin/sh")
                .arg("-c")
                .arg(format!(
                    "codex() {{ printf 'key=%s\\n' \"${{DS_KEY:-unset}}\"; }}; {line}"
                ))
                .env("PATH", &path)
                .env("HELPER_FAIL", if fail { "1" } else { "" })
                .output()
                .unwrap()
        };
        let ok = run(false);
        assert!(ok.status.success(), "{ok:?}");
        assert_eq!(
            String::from_utf8_lossy(&ok.stdout).trim(),
            "key=fetched-secret"
        );

        let failed = run(true);
        assert!(failed.status.success(), "{failed:?}");
        assert_eq!(String::from_utf8_lossy(&failed.stdout).trim(), "key=unset");
        assert!(
            String::from_utf8_lossy(&failed.stderr).contains("was not delivered"),
            "{failed:?}"
        );
    }

    /// WI-4121: an MCP server whose Codex table names a bearer by environment
    /// variable gets that value from the broker immediately before Codex
    /// starts, beside the provider keys. A value already in the environment is
    /// left as it is, so the Vogt session token is never fetched again.
    #[test]
    fn a_codex_child_receives_an_mcp_bearer_from_the_broker() {
        let dir = tempfile::tempdir().unwrap();
        let fragment = dir.path().join("vogt-codex.toml");
        std::fs::write(
            &fragment,
            "[mcp_servers.cadastre]\nurl = \"https://cadastre.example.test/mcp\"\n\
             bearer_token_env_var = \"CADASTRE_HTTP_TOKEN\"\n",
        )
        .unwrap();
        let mut config = claude_config(ClaudeLaunch::default(), false);
        config.cli = "codex".into();
        config.codex.config_file = Some(fragment);
        let line = command_line(&config).unwrap();
        assert!(line.contains("CADASTRE_HTTP_TOKEN"), "{line}");
        assert!(line.contains("vogt-agent-auth fetch"), "{line}");
        assert!(!line.contains("fetched-secret"), "{line}");

        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let helper = bin.join("vogt-agent-auth");
        std::fs::write(
            &helper,
            "#!/bin/sh\ncase \"$1\" in\nfetch)\n  printf fetched-secret ;;\n*) exit 1 ;;\nesac\n",
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path = format!(
            "{}:{}",
            bin.display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let run = |existing: Option<&str>| {
            let mut command = std::process::Command::new("/bin/sh");
            command.arg("-c").arg(format!(
                "codex() {{ printf 'cad=%s\\n' \"${{CADASTRE_HTTP_TOKEN:-unset}}\"; }}; {line}"
            ));
            command.env("PATH", &path).env_remove("CADASTRE_HTTP_TOKEN");
            if let Some(value) = existing {
                command.env("CADASTRE_HTTP_TOKEN", value);
            }
            command.output().unwrap()
        };
        let fetched = run(None);
        assert_eq!(
            String::from_utf8_lossy(&fetched.stdout).trim(),
            "cad=fetched-secret"
        );
        let kept = run(Some("already-there"));
        assert_eq!(
            String::from_utf8_lossy(&kept.stdout).trim(),
            "cad=already-there"
        );
    }

    /// WI-3873: the rollout name comes from the launch line, not from a file
    /// the test planted under a name it already believed in. The generated
    /// argv names the conversation; Codex names its rollout from that same
    /// argv, and `codex_transcript_path` finds it.
    #[test]
    fn a_codex_rollout_is_named_from_the_generated_launch_line() {
        let config = {
            let mut config = claude_config(ClaudeLaunch::default(), false);
            config.cli = "codex".into();
            config.cwd = Some("/work".into());
            config.conversation_id = Some("conv-from-line".into());
            config
        };
        let argv = argv_of_cli("codex", &command_line(&config).unwrap());
        let pinned = argv
            .windows(2)
            .find(|pair| pair[0] == "--session-id")
            .map(|pair| pair[1].clone())
            .expect("a fresh Codex launch names the conversation");
        assert_eq!(pinned, "conv-from-line");

        // What Codex writes: its own date and timestamp, the id from argv.
        let dir = tempfile::tempdir().unwrap();
        let relative = format!("sessions/2026/11/09/rollout-2026-11-09T00-00-00-{pinned}.jsonl");
        let rollout = dir.path().join(&relative);
        std::fs::create_dir_all(rollout.parent().unwrap()).unwrap();
        std::fs::write(&rollout, b"{\"type\":\"session\"}\n").unwrap();
        assert!(is_codex_rollout_name(
            rollout.file_name().unwrap().to_str().unwrap(),
            &pinned
        ));
        assert!(is_conversation_transcript(&relative, &pinned));
        assert_eq!(codex_transcript_path(dir.path(), &pinned), Some(rollout));
    }

    #[test]
    fn a_conversation_transcript_is_claude_or_codex_shaped() {
        // Claude: any depth under the account root, or a bare file name.
        assert!(is_conversation_transcript(
            "projects/work/conv.jsonl",
            "conv"
        ));
        assert!(is_conversation_transcript("conv.jsonl", "conv"));
        // Codex: the rollout contract path.
        assert!(is_conversation_transcript(
            "sessions/2026/11/09/rollout-2026-11-09T00-00-00-conv.jsonl",
            "conv"
        ));
        // Another conversation, a non-rollout file with the same suffix, and
        // an id that could walk out of the tree are all refused.
        assert!(!is_conversation_transcript(
            "sessions/2026/11/09/rollout-x-other.jsonl",
            "conv"
        ));
        assert!(!is_conversation_transcript(
            "sessions/2026/11/09/conv.jsonl",
            "conv"
        ));
        assert!(!is_conversation_transcript("projects/../conv.jsonl", ".."));
        assert!(!is_conversation_transcript(
            "projects/conv.jsonl",
            "../conv"
        ));
    }

    /// The argv `name` sees when the shell runs `line`, one word per line.
    fn argv_of_cli(name: &str, line: &str) -> Vec<String> {
        let out = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("{name}() {{ printf '%s\\n' \"$@\"; }}; {line}"))
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

    /// The argv `claude` sees when the shell runs `line`, one word per line.
    fn argv_of(line: &str) -> Vec<String> {
        argv_of_cli("claude", line)
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
            codex_home: None,
            workspace_root: None,
            account: None,
            conversation_id: Some("abc-123".into()),
            resume: false,
            model: Some("claude-opus-4-6".into()),
            effort: Some("high".into()),
            permission_mode: Some("accept-edits".into()),
            codex_approval_policy: CODEX_APPROVAL_DEFAULT.into(),
            claude: ClaudeLaunch {
                settings: Some(IMAGE_SETTINGS_FILE.into()),
                mcp_config: Some("/run/vogt/mcp.json".into()),
                brief: None,
            },
            codex: CodexLaunch::default(),
        };
        // The host launcher's flags, in its order (WI-3881).
        assert_eq!(
            argv_of(&command_line(&config).unwrap()),
            [
                "--model",
                "claude-opus-4-6",
                "--effort",
                "high",
                "--session-id",
                "abc-123",
                "--permission-mode",
                "acceptEdits",
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
        assert!(
            argv.windows(2).any(|w| w == ["--resume", "abc-123"]),
            "{argv:?}"
        );
        assert!(argv.contains(&"--strict-mcp-config".to_string()));
    }

    /// WI-3881: Claude's posture flags live here, beside the argv they go on,
    /// so the host launcher and the pod cannot spell one differently.
    #[test]
    fn claude_postures_map_to_its_flags_and_default_adds_none() {
        assert_eq!(claude_permission_flags(None).unwrap(), None);
        assert_eq!(claude_permission_flags(Some("default")).unwrap(), None);
        assert_eq!(
            claude_permission_flags(Some("accept_edits")).unwrap(),
            Some(vec!["--permission-mode".into(), "acceptEdits".into()])
        );
        assert_eq!(
            claude_permission_flags(Some("bypass")).unwrap(),
            Some(vec!["--dangerously-skip-permissions".into()])
        );
        assert!(claude_permission_flags(Some("yolo")).is_err());

        let args = claude_args(&ClaudeArgs {
            model: Some("claude-opus-4-6"),
            effort: Some("high"),
            conversation: Some(("abc-123", false)),
            permission_mode: Some("accept-edits"),
            ..ClaudeArgs::default()
        })
        .unwrap();
        assert_eq!(
            args,
            [
                "--model",
                "claude-opus-4-6",
                "--effort",
                "high",
                "--session-id",
                "abc-123",
                "--permission-mode",
                "acceptEdits",
            ]
        );
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
            codex_home: None,
            workspace_root: None,
            account: None,
            conversation_id: Some("abc-123".into()),
            resume,
            model: None,
            effort: None,
            permission_mode: None,
            codex_approval_policy: CODEX_APPROVAL_DEFAULT.into(),
            claude,
            codex: CodexLaunch::default(),
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

    /// WI-4068: pod Codex had the same gap Claude Code had in WI-3355. The
    /// engine's rewritten argv (which carries the brief pointer) is dropped
    /// for an agent template, and the pod rebuilt the line with `prompt:
    /// None`, so a task session opened at an empty composer. With autopilot
    /// off nothing ever typed into it and the session sat at 0 turns.
    #[test]
    fn a_fresh_codex_is_given_its_brief_as_the_prompt() {
        let brief = PathBuf::from("/work/.vogt/brief.md");
        let mut config = claude_config(
            ClaudeLaunch {
                brief: Some(brief.clone()),
                ..ClaudeLaunch::default()
            },
            false,
        );
        config.cli = "codex".into();
        let argv = argv_of_cli("codex", &command_line(&config).unwrap());
        assert_eq!(argv.last().unwrap(), &brief_instruction(&brief), "{argv:?}");
        // A resume already names the conversation and must not also give the
        // CLI a fresh first prompt.
        config.resume = true;
        let argv = argv_of_cli("codex", &command_line(&config).unwrap());
        assert!(!argv.iter().any(|a| a.contains("brief")), "{argv:?}");
        // No brief, no prompt.
        config.resume = false;
        config.claude.brief = None;
        let argv = argv_of_cli("codex", &command_line(&config).unwrap());
        assert!(!argv.iter().any(|a| a.contains("brief")), "{argv:?}");
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
            codex_home: None,
            workspace_root: None,
            account: None,
            conversation_id: None,
            resume: false,
            model: None,
            effort: None,
            permission_mode: None,
            codex_approval_policy: CODEX_APPROVAL_DEFAULT.into(),
            claude: ClaudeLaunch::default(),
            codex: CodexLaunch::default(),
        };
        assert_eq!(command_line(&config).unwrap(), "opencode");
        // Codex refuses extra writable roots, so it gets no `--add-dir`; the
        // PTY already starts in the session cwd.
        config.cli = "codex".into();
        assert_eq!(
            argv_of_cli("codex", &command_line(&config).unwrap()),
            [
                "-c",
                r#"projects={"/work"={trust_level="trusted"}}"#,
                "-c",
                "features.daemon_auto_start=false",
                "-a",
                "never",
                "-s",
                "danger-full-access",
                "-c",
                r#"tui.terminal_title=["agent-state"]"#,
                "-c",
                "tui.animations=false",
            ]
        );
        config.cli = "nope".into();
        assert!(command_line(&config).is_err());
    }

    /// WI-4025: the pod's trust entry must name the directory Codex actually
    /// runs in. A cwd with a trailing slash (what a clone-root session
    /// resolves to) must trust the same key as the bare directory, or the
    /// folder-trust screen appears and the session never runs.
    #[test]
    fn a_pod_codex_trusts_the_work_dir_with_or_without_a_trailing_slash() {
        let trust_arg = |cwd: &str| {
            let args = codex_args(&CodexArgs {
                command_args: &[],
                resume: None,
                session_id: None,
                model: None,
                config_model: None,
                effort: None,
                permission_mode: None,
                approval_policy: "never",
                exec: false,
                compact_limit: None,
                provider: None,
                providers: &[],
                catalog: None,
                trust_cwd: Some(cwd),
                guidance: None,
                charter: None,
                mcp_servers: &[],
                mcp_args: &[],
                prompt: None,
            })
            .unwrap();
            args.into_iter()
                .find(|a| a.starts_with("projects="))
                .unwrap_or_default()
        };
        assert_eq!(
            trust_arg("/work"),
            r#"projects={"/work"={trust_level="trusted"}}"#
        );
        assert_eq!(
            trust_arg("/work/"),
            r#"projects={"/work"={trust_level="trusted"}}"#,
            "a trailing slash must not move the trust entry off /work"
        );
    }

    /// WI-3864: a pod Codex gets the same posture flags as a host Codex. The
    /// pod cannot create the user namespace `workspace-write` needs, so every
    /// posture names `danger-full-access`; a bypass drops the sandbox and the
    /// approvals together.
    #[test]
    fn a_pod_codex_line_carries_the_same_posture_as_a_host_launch() {
        let mut config = claude_config(ClaudeLaunch::default(), false);
        config.cli = "codex".into();

        // Default: the deployment's approval policy, never by default.
        let argv = argv_of_cli("codex", &command_line(&config).unwrap());
        assert!(argv.windows(2).any(|w| w == ["-a", "never"]), "{argv:?}");
        assert!(
            argv.windows(2).any(|w| w == ["-s", "danger-full-access"]),
            "{argv:?}"
        );
        assert!(
            argv.windows(2).any(|w| w == ["--session-id", "abc-123"]),
            "{argv:?}"
        );

        // accept-edits keeps the sandbox and adds the auto reviewer.
        config.permission_mode = Some("accept-edits".into());
        let argv = argv_of_cli("codex", &command_line(&config).unwrap());
        assert!(
            argv.windows(2)
                .any(|w| w == ["-c", "approvals_reviewer=\"auto_review\""]),
            "{argv:?}"
        );
        assert!(argv.windows(2).any(|w| w == ["-a", "never"]), "{argv:?}");
        assert!(
            argv.windows(2).any(|w| w == ["-s", "danger-full-access"]),
            "{argv:?}"
        );

        // A deployment that restores asking passes `on-request` through.
        config.permission_mode = Some("accept_edits".into());
        config.codex_approval_policy = "on-request".into();
        let argv = argv_of_cli("codex", &command_line(&config).unwrap());
        assert!(
            argv.windows(2).any(|w| w == ["-a", "on-request"]),
            "{argv:?}"
        );
        assert!(argv.contains(&"-s".to_string()), "{argv:?}");
        assert!(argv.contains(&"danger-full-access".to_string()), "{argv:?}");

        // Bypass drops both, and the pod never falls back to the CLI's own
        // `workspace-write` sandbox.
        config.permission_mode = Some("bypass".into());
        config.codex_approval_policy = "never".into();
        let line = command_line(&config).unwrap();
        assert!(
            line.contains("--dangerously-bypass-approvals-and-sandbox"),
            "{line}"
        );
        assert!(!line.contains(" -s "), "{line}");

        // An unknown posture is refused, not handed to the CLI.
        config.permission_mode = Some("yolo".into());
        assert!(command_line(&config).is_err());
    }

    /// The Codex binary for the argv parse check (WI-3839): `VOGT_CODEX_BIN`,
    /// which CI sets to the pinned fork. Without it the check is skipped,
    /// unless `VOGT_CODEX_REQUIRED` says this run must have one.
    #[cfg(unix)]
    fn real_codex() -> Option<PathBuf> {
        match std::env::var_os("VOGT_CODEX_BIN") {
            Some(bin) => Some(PathBuf::from(bin)),
            None if std::env::var_os("VOGT_CODEX_REQUIRED").is_some() => {
                panic!("VOGT_CODEX_REQUIRED is set but VOGT_CODEX_BIN is not")
            }
            None => {
                eprintln!("skipped: set VOGT_CODEX_BIN to check argv against Codex");
                None
            }
        }
    }

    /// Whether the real Codex parses `line` (a shell command naming `codex`):
    /// clap rejects an unknown flag before it reaches the trailing `--help`.
    #[cfg(unix)]
    fn codex_parses(codex: &Path, line: &str) -> bool {
        let bin = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(codex, bin.path().join("codex")).unwrap();
        let path = format!(
            "{}:{}",
            bin.path().display(),
            std::env::var("PATH").unwrap_or_default()
        );
        std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("{line} --help"))
            .env("PATH", path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
    }

    /// WI-3839: the interactive CLI refused `--skip-git-repo-check`, an
    /// `exec` flag, and every pod Codex launch exited at once. A string test
    /// cannot see that, so the generated line goes through Codex's own parser.
    #[cfg(unix)]
    #[test]
    fn the_pod_codex_line_parses_with_the_real_codex() {
        let Some(codex) = real_codex() else { return };
        let mut config = Config {
            cli: "codex".into(),
            cwd: Some("/work".into()),
            ..claude_config(ClaudeLaunch::default(), false)
        };
        let line = command_line(&config).unwrap();
        assert!(codex_parses(&codex, &line), "codex refused: {line}");
        // A wake is `codex resume <id>` with the same options after it.
        config.resume = true;
        let line = command_line(&config).unwrap();
        assert!(codex_parses(&codex, &line), "codex refused: {line}");
        assert!(!codex_parses(&codex, "codex --skip-git-repo-check"));
    }

    /// WI-3335: the engine does not judge a pod's Codex by its own files, so
    /// the pod does, fail closed, before Codex is spawned.
    #[test]
    fn a_pod_codex_without_an_enforceable_policy_hook_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let requirements = dir.path().join("requirements.toml");
        let hook = dir.path().join("vogt-policy-hook");
        let mut config = Config {
            session_id: "s".into(),
            engine_url: "http://127.0.0.1:1".into(),
            token: "t".into(),
            cli: "codex".into(),
            command: None,
            cwd: None,
            cols: 80,
            rows: 24,
            buffer_bytes: 1024,
            claude_config_dir: None,
            codex_home: None,
            workspace_root: None,
            account: None,
            conversation_id: None,
            resume: false,
            model: None,
            effort: None,
            permission_mode: None,
            codex_approval_policy: CODEX_APPROVAL_DEFAULT.into(),
            claude: ClaudeLaunch::default(),
            codex: CodexLaunch::default(),
        };
        // No requirements file in the pod.
        let err = codex_guard(&config, &requirements, &hook).unwrap_err();
        assert!(err.to_string().contains("cannot be read"), "{err}");
        // A file that does not wire the managed hook.
        std::fs::write(&requirements, "[features]\nhooks = true\n").unwrap();
        let err = codex_guard(&config, &requirements, &hook).unwrap_err();
        assert!(
            err.to_string()
                .contains("does not wire the managed policy hook"),
            "{err}"
        );
        // The shipped file, but the requirements and hook are both owned by
        // the session user.
        std::fs::write(
            &requirements,
            include_str!("../../deploy/driven-session-codex-requirements.toml"),
        )
        .unwrap();
        std::fs::write(&hook, "#!/bin/sh\nexit 0\n").unwrap();
        // The refusal is about ownership, so the fixtures must not be owned by
        // root. A root runner (some CI containers) makes everything it writes
        // root-owned; hand the files to an unprivileged uid to describe the
        // session user, and skip the case only where root cannot chown (a
        // rootless user namespace), which is never the production shape.
        let session_owned = session_owns(&[dir.path(), &requirements, &hook]);
        if session_owned {
            let err = codex_guard(&config, &requirements, &hook).unwrap_err();
            assert!(err.to_string().contains("root-owned"), "{err}");
        }
        // An argv naming Codex is held to the same check.
        config.cli = "claude".into();
        config.command = Some(vec!["/usr/local/bin/codex".into()]);
        if session_owned {
            assert!(codex_guard(&config, &requirements, &hook).is_err());
        }
        // Other CLIs never consult it.
        config.command = None;
        assert!(codex_guard(&config, &requirements, &hook).is_ok());
        config.cli = "opencode".into();
        assert!(codex_guard(&config, &requirements, &hook).is_ok());
    }

    /// Gives `paths` an unprivileged owner, as a session's own files have.
    /// Returns whether the ownership case can be exercised: true when the
    /// runner is not root, or when it can hand the files to uid 65534.
    fn session_owns(paths: &[&std::path::Path]) -> bool {
        use std::os::unix::fs::MetadataExt as _;
        if paths[0].metadata().unwrap().uid() != 0 {
            return true;
        }
        paths
            .iter()
            .all(|path| std::os::unix::fs::chown(path, Some(65534), Some(65534)).is_ok())
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
            codex_home: None,
            workspace_root: None,
            account: None,
            conversation_id: None,
            resume: false,
            model: None,
            effort: None,
            permission_mode: None,
            codex_approval_policy: CODEX_APPROVAL_DEFAULT.into(),
            claude: ClaudeLaunch {
                settings: Some("/a b/it's; touch /tmp/x.json".into()),
                mcp_config: None,
                brief: None,
            },
            codex: CodexLaunch::default(),
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
            codex_home: None,
            workspace_root: None,
            account: None,
            conversation_id: None,
            resume: false,
            model: None,
            effort: None,
            permission_mode: None,
            codex_approval_policy: CODEX_APPROVAL_DEFAULT.into(),
            claude: ClaudeLaunch::default(),
            codex: CodexLaunch::default(),
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
    fn a_codex_transcript_is_found_without_following_links() {
        let dir = tempfile::tempdir().unwrap();
        let rollout = dir
            .path()
            .join("sessions/2026/11/09/rollout-2026-11-09T00-00-00-conv.jsonl");
        assert!(codex_transcript_path(dir.path(), "conv").is_none());
        std::fs::create_dir_all(rollout.parent().unwrap()).unwrap();
        std::fs::write(&rollout, b"x").unwrap();
        assert_eq!(
            codex_transcript_path(dir.path(), "conv"),
            Some(rollout.clone())
        );
        // Another conversation's rollout, and a file that is not a rollout,
        // are not this conversation's transcript.
        assert!(codex_transcript_path(dir.path(), "other").is_none());
        std::fs::write(rollout.parent().unwrap().join("conv.jsonl"), b"x").unwrap();
        assert_eq!(
            codex_transcript_path(dir.path(), "conv"),
            Some(rollout.clone())
        );
        // A conversation id cannot walk out of the sessions tree.
        assert!(codex_transcript_path(dir.path(), "../conv").is_none());
        // The walk is bounded: a rollout buried too deep is not found.
        let deep = dir
            .path()
            .join("sessions/a/b/c/d/e/f/rollout-2026-11-09T00-00-00-deep.jsonl");
        std::fs::create_dir_all(deep.parent().unwrap()).unwrap();
        std::fs::write(&deep, b"x").unwrap();
        assert!(codex_transcript_path(dir.path(), "deep").is_none());
        // A symlink pointing at the rollout is not the rollout.
        std::fs::rename(&rollout, dir.path().join("real.jsonl")).unwrap();
        std::os::unix::fs::symlink(dir.path().join("real.jsonl"), &rollout).unwrap();
        assert!(codex_transcript_path(dir.path(), "conv").is_none());
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

#[cfg(test)]
mod haiku_window_tests {
    use super::*;

    fn provider(window: u64, compact: u64) -> CodexProvider {
        CodexProvider {
            name: "litellm".into(),
            base_url: "http://router.invalid/v1".into(),
            wire_api: "chat".into(),
            env_key: "KEY".into(),
            context_window: Some(window),
            auto_compact_token_limit: Some(compact),
        }
    }

    fn argv(model: Option<&str>, compact_limit: Option<u64>) -> Vec<String> {
        let providers = [provider(850_000, 800_000)];
        codex_args(&CodexArgs {
            command_args: &[],
            resume: None,
            session_id: None,
            model,
            config_model: None,
            effort: None,
            permission_mode: None,
            approval_policy: "never",
            exec: false,
            compact_limit,
            provider: Some("litellm"),
            providers: &providers,
            catalog: None,
            trust_cwd: None,
            guidance: None,
            charter: None,
            mcp_servers: &[],
            mcp_args: &[],
            prompt: None,
        })
        .unwrap()
    }

    /// The value Codex would take for `key`: its last `-c key=value` pair.
    fn last_value(argv: &[String], key: &str) -> Option<String> {
        let prefix = format!("{key}=");
        argv.windows(2)
            .rfind(|pair| pair[0] == "-c" && pair[1].starts_with(&prefix))
            .map(|pair| pair[1][prefix.len()..].to_string())
    }

    #[test]
    fn a_haiku_launch_gets_its_own_window_and_compact_limit() {
        let args = argv(Some("claude-org-haiku"), None);
        assert_eq!(
            last_value(&args, "model_context_window").as_deref(),
            Some("200000")
        );
        assert_eq!(
            last_value(&args, "model_auto_compact_token_limit").as_deref(),
            Some("170000")
        );
        assert_eq!(
            last_value(&args, "model_auto_compact_token_limit_scope").as_deref(),
            Some("\"total\"")
        );
        // One window key only: the provider's 850k must not also be emitted.
        assert!(!args.iter().any(|a| a == "model_context_window=850000"));
    }

    #[test]
    fn a_haiku_backoff_may_raise_but_not_past_the_window_ceiling() {
        let raised = argv(Some("haiku"), Some(175_000));
        assert_eq!(
            last_value(&raised, "model_auto_compact_token_limit").as_deref(),
            Some("175000")
        );
        let capped = argv(Some("haiku"), Some(999_999));
        assert_eq!(
            last_value(&capped, "model_auto_compact_token_limit").as_deref(),
            Some("180000")
        );
    }

    #[test]
    fn other_models_keep_the_provider_window_and_limit() {
        let args = argv(Some("grok"), None);
        assert_eq!(
            last_value(&args, "model_context_window").as_deref(),
            Some("850000")
        );
        assert_eq!(
            last_value(&args, "model_auto_compact_token_limit").as_deref(),
            Some("800000")
        );
        assert_eq!(
            last_value(&argv(None, None), "model_context_window").as_deref(),
            Some("850000")
        );
    }
}
