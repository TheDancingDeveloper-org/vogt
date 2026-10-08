//! Quick chats (WI-1097): persistent text conversations with an agent CLI,
//! driven over its stream-json protocol instead of a terminal.
//!
//! ## Shape
//!
//! A chat is a row in [`ChatStore`] plus, while it is being talked to, one
//! child process: Klaudia (the driver of the configured launch, by default
//! the `Klaudia (protected)` template's, without its credential wrapper) with
//! `--input-format stream-json --output-format stream-json`. Each message is
//! a `user` line on its stdin; the agent's messages, tool calls and the
//! `result` that ends a turn come back on stdout (msp-klaudia
//! `docs/embedding.md`). No PTY, no screen-scraping, no activity heuristic.
//!
//! Vogt picks the conversation id — the chat's own id — with `--session-id`
//! on the first launch and `--resume` on every later one, so a process that
//! was stopped for being idle, or lost to an engine restart, is relaunched on
//! the next message with the whole conversation. Promotion hands the same id
//! to a terminal session's `resume`, which continues it there, with the
//! session's own (full) credentials.
//!
//! ## What a chat's agent holds
//!
//! A chat reads pages and documents nobody vetted, so it is built on the
//! assumption that its agent can be talked into anything (review round 1):
//!
//! - **No secrets in its environment.** It starts from `env_clear()`, the
//!   [`CHAT_ENV_ALLOWLIST`], its gate's URL and token, and the one provider
//!   key [`ChatPolicy::provider_key`] names — no Vogt token, no brokered
//!   service tokens, no secrets-manager identity. So its MCP servers start
//!   without credentials: no MCP in chats (decision D1 on WI-1097).
//! - **Only its own directory is free.** [`classify`] lets through reads of
//!   the chat's scratch directory and tools that touch nothing; every other
//!   read, every network call (web, browser, MCP — decision D2), every command
//!   and edit waits for a person.
//! - **Bounded.** At most [`ChatPolicy::max_processes`] processes, and
//!   [`ChatPolicy::max_per_creator`] per person, with an idle one stopped to
//!   make room; a turn past [`ChatPolicy::turn_timeout`] is stopped; a tree
//!   over [`ChatPolicy::max_rss_bytes`] is killed; a previous engine's
//!   leftovers are ended at boot ([`sweep_orphans`]).
//!
//! ## The approval gate
//!
//! Klaudia's own permission modes cannot express that rule: `autonomous`
//! runs project work without asking, and `plan` refuses MCP and the network,
//! so a chat in it could not look anything up even with a person's leave.
//! So the engine gates calls itself. Each chat runs in a directory of its own
//! under `state_dir` whose `.klaudia/config.toml` (passed with
//! `--trusted-project-config`) declares a `PreToolUse` hook that posts every
//! tool call to `POST /api/chats/{id}/gate` with a per-process token. The
//! engine answers at once for a free call and holds anything else on an
//! approval card only a person can answer (the WI-983 rule), denying it when
//! [`ChatPolicy::approval_timeout`] passes.
//!
//! A hook is not a sandbox, and Klaudia runs a project's hooks only once
//! they are approved, so the gate is built to fail closed where it can:
//!
//! - the hook exits 2 (Klaudia's "block") on any failure to get an answer;
//! - the engine answers before the hook's own timeout, after which Klaudia
//!   would let the call through;
//! - Klaudia asks the driver whether this chat's hooks may run; the engine
//!   allows exactly its own file, byte for byte as it wrote it;
//! - a successful result of a gated call the gate never saw stops the chat
//!   with an error rather than carrying on ([`Live::gated`]).
//!
//! The gate is a driver's gate, not a boundary against an agent a person has
//! let run commands; that is WI-982's uid separation.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use dashmap::DashMap;
use parking_lot::Mutex;
use serde_json::{json, Value};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::Command,
    sync::{broadcast, mpsc, oneshot},
};
use uuid::Uuid;
use vogt_engine_contract::{
    ChatApproval, ChatConfigInfo, ChatCreateRequest, ChatDetail, ChatDriver, ChatEntry, ChatEvent,
    ChatModel, ChatPromoteRequest, ChatPromoteResult, ChatSendResult, ChatState, ChatSummary,
    ServerEvent, SessionSpec,
};

use crate::{
    chat_store::{preview, ChatQuery, ChatRecord, ChatStore},
    config::Config,
    error::{ApiError, Result},
    events::EventBus,
    sessions::SessionRegistry,
};

/// How chats are run (`ENGINE_CHAT_*`).
#[derive(Debug, Clone, PartialEq)]
pub struct ChatPolicy {
    /// `ENGINE_CHAT_ENABLED` (on). Off, every chat route answers 404 and the
    /// PWA hides the button.
    pub enabled: bool,
    /// `ENGINE_CHAT_COMMAND`: the driver's launch command, overriding the
    /// template. Words, or a JSON array.
    pub command: Option<Vec<String>>,
    /// `ENGINE_CHAT_TEMPLATE`: the session template whose command launches
    /// the driver. Unset, the first configured template that runs Klaudia.
    pub template: Option<String>,
    /// `ENGINE_CHAT_MODELS_JSON`: `[{"id","label"}]`, what the model picker
    /// offers. Empty offers only the driver's own default.
    pub models: Vec<ChatModel>,
    /// `ENGINE_CHAT_IDLE_AFTER` (10m): an idle chat's process exits after
    /// this; the next message relaunches it with `--resume`.
    pub idle_after: Duration,
    /// `ENGINE_CHAT_APPROVAL_TIMEOUT` (10m): an approval nobody answers is
    /// denied after this.
    pub approval_timeout: Duration,
    /// `ENGINE_CHAT_PROVIDER_KEY`: the name of the one credential the
    /// driver needs (its model provider's API key, e.g. `THECLAWBAY_API_KEY`).
    /// The engine resolves it — through the agent-auth manifest when the
    /// deployment brokers it, else from its own environment — and hands the
    /// chat that variable and nothing else secret.
    pub provider_key: Option<String>,
    /// `ENGINE_CHAT_MAX_PROCESSES` (4): chat processes running at once,
    /// engine-wide. A new one first stops the least recently active idle one.
    pub max_processes: usize,
    /// `ENGINE_CHAT_MAX_PER_CREATOR` (2): the same, per person.
    pub max_per_creator: usize,
    /// `ENGINE_CHAT_TURN_TIMEOUT` (20m): a turn running longer is stopped,
    /// and its process killed if it does not stop.
    pub turn_timeout: Duration,
    /// `ENGINE_CHAT_MAX_RSS` (2GiB): a chat whose process tree's resident
    /// memory passes this is killed.
    pub max_rss_bytes: u64,
}

impl Default for ChatPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            command: None,
            template: None,
            models: Vec::new(),
            idle_after: Duration::from_secs(600),
            approval_timeout: Duration::from_secs(600),
            provider_key: None,
            max_processes: 4,
            max_per_creator: 2,
            turn_timeout: Duration::from_secs(20 * 60),
            max_rss_bytes: 2 << 30,
        }
    }
}

impl ChatPolicy {
    pub fn validate(&self) -> std::result::Result<(), String> {
        if self.approval_timeout < Duration::from_secs(10)
            || self.approval_timeout > Duration::from_secs(3600)
        {
            return Err("ENGINE_CHAT_APPROVAL_TIMEOUT must be between 10s and 1h".into());
        }
        if self.max_processes == 0 || self.max_per_creator == 0 {
            return Err("ENGINE_CHAT_MAX_PROCESSES and _MAX_PER_CREATOR must be at least 1".into());
        }
        if self.turn_timeout < self.approval_timeout {
            return Err(
                "ENGINE_CHAT_TURN_TIMEOUT must be at least ENGINE_CHAT_APPROVAL_TIMEOUT, or a turn \
                 waiting on an approval would be stopped before it could be answered"
                    .into(),
            );
        }
        if let Some(name) = self.provider_key.as_deref() {
            if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') || name.is_empty() {
                return Err(format!(
                    "ENGINE_CHAT_PROVIDER_KEY={name:?} is not a variable name"
                ));
            }
        }
        if self.idle_after < Duration::from_secs(10) {
            return Err("ENGINE_CHAT_IDLE_AFTER must be at least 10s".into());
        }
        for model in &self.models {
            crate::agent_cli::validate("chat model", &model.id).map_err(|e| e.to_string())?;
            if model.id == "default" {
                return Err(
                    "\"default\" names the driver's own model; it is not a model id".into(),
                );
            }
        }
        Ok(())
    }
}

/// The only driver this engine speaks to.
const DRIVER: &str = "klaudia";
const DEFAULT_TITLE: &str = "New chat";
const MAX_MESSAGE_BYTES: usize = 64 * 1024;
/// A tool result is kept to this many characters in Vogt's copy; the
/// agent's own transcript keeps it whole.
const RESULT_CHARS: usize = 4_000;
/// What a tool call's input is shown as.
const INPUT_CHARS: usize = 600;
/// How long a driver-side approval counts for the gate that follows it, so
/// one decision is asked once (Klaudia's host gate asks before its hooks run).
const ALLOWED_REUSE: Duration = Duration::from_secs(120);
const STDERR_TAIL: usize = 4 * 1024;

/// The hook Klaudia runs before every tool call in a chat. Values come from
/// the environment the engine gives the process, so the file is the same
/// for every chat. `curl` failing in any way (refused, timed out, a non-2xx)
/// exits 2, which Klaudia reads as "blocked" — any other non-zero status
/// would read as a broken hook and let the call through.
fn hook_config(approval_timeout: Duration) -> String {
    let wait = approval_timeout.as_secs() + 60;
    let hook_timeout = wait + 60;
    format!(
        r#"# Written by the Vogt engine for one quick chat (WI-1097). Regenerated at
# every launch; edits are lost.
[[hooks]]
event = "PreToolUse"
timeout = "{hook_timeout}s"
command = '''curl -sS -f --max-time {wait} -X POST -H "Authorization: Bearer $VOGT_CHAT_GATE_TOKEN" -H 'Content-Type: application/json' --data-binary @- "$VOGT_CHAT_GATE_URL" || {{ echo 'Vogt could not reach the approval gate for this chat, so this call was refused.' >&2; exit 2; }}'''
"#
    )
}

/// What the gate does with a tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolClass {
    /// Runs without asking: a read of the chat's own scratch directory, or a
    /// tool that touches nothing outside the agent.
    Free,
    /// Waits for a person.
    Ask,
}

/// Tools that touch nothing outside the agent itself.
const INERT: &[&str] = &[
    "ToolSearch",
    "TodoWrite",
    "TaskGet",
    "TaskList",
    "Jobs",
    "BashOutput",
    "Skill",
    "AskUserQuestion",
    "ExitPlanMode",
];

/// How an argument of a read tool is treated.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Arg {
    /// A path to a file or directory: it must resolve inside the chat's
    /// directory.
    Path,
    /// A glob: its literal prefix must resolve inside the chat's directory.
    Glob,
    /// Not a path (an offset, a regex, a line number): no bearing on what
    /// is read.
    Other,
}

/// A read tool the gate understands argument by argument, from Klaudia's
/// own schemas (msp-klaudia `internal/tools/{read,glob,grep,lsp_tools}.go`
/// at 601e714).
struct ReadTool {
    name: &'static str,
    args: &'static [(&'static str, Arg)],
    /// The path argument that must be present. `None` where omitting every
    /// path means the working directory, which is the chat's own.
    required: Option<&'static str>,
}

const READ_TOOLS: &[ReadTool] = &[
    ReadTool {
        name: "Read",
        args: &[
            ("file_path", Arg::Path),
            ("offset", Arg::Other),
            ("limit", Arg::Other),
        ],
        required: Some("file_path"),
    },
    ReadTool {
        name: "Glob",
        args: &[("pattern", Arg::Glob), ("path", Arg::Path)],
        required: None,
    },
    ReadTool {
        name: "Grep",
        args: &[
            ("pattern", Arg::Other),
            ("path", Arg::Path),
            ("glob", Arg::Glob),
            ("output_mode", Arg::Other),
            ("-i", Arg::Other),
            ("-n", Arg::Other),
            ("multiline", Arg::Other),
            ("type", Arg::Other),
            ("-A", Arg::Other),
            ("-B", Arg::Other),
            ("-C", Arg::Other),
            ("head_limit", Arg::Other),
        ],
        required: None,
    },
    ReadTool {
        name: "Diagnostics",
        args: &[("file", Arg::Path)],
        required: Some("file"),
    },
    ReadTool {
        name: "Hover",
        args: &[
            ("file", Arg::Path),
            ("line", Arg::Other),
            ("character", Arg::Other),
        ],
        required: Some("file"),
    },
    ReadTool {
        name: "DocumentSymbols",
        args: &[("file", Arg::Path)],
        required: Some("file"),
    },
    ReadTool {
        name: "WorkspaceSymbol",
        args: &[("query", Arg::Other), ("file", Arg::Path)],
        required: None,
    },
];

/// Classify a tool call for the gate (WI-1097 review rounds 1 and 2).
///
/// Fail-closed by construction: a call runs free only when the tool is on an
/// explicit allowlist — [`INERT`], or a [`READ_TOOLS`] entry whose every
/// argument is one the table knows and whose every path argument is a string
/// that resolves (lexically, then through symlinks) inside the chat's own
/// directory. An unknown tool, an unknown argument, a missing required path
/// or a path that is not a string is a card. So a read anywhere else — the
/// process's own `/proc/self/environ`, the engine's state, another person's
/// files — waits for a person, and so does every tool that reaches the
/// network (web fetch and search, the browser, every MCP call: an argument
/// sent to a server is data leaving the chat). A prompt injection in a page
/// the chat was shown cannot read a secret and send it somewhere in one
/// unseen step: each half is a card naming what it would do.
pub fn classify(tool: &str, input: &Value, chat_dir: &Path) -> ToolClass {
    if INERT.contains(&tool) {
        return ToolClass::Free;
    }
    match READ_TOOLS.iter().find(|t| t.name == tool) {
        Some(spec) if read_is_local(spec, input, chat_dir) => ToolClass::Free,
        _ => ToolClass::Ask,
    }
}

fn read_is_local(spec: &ReadTool, input: &Value, dir: &Path) -> bool {
    let empty = serde_json::Map::new();
    let fields = match input {
        Value::Object(fields) => fields,
        Value::Null => &empty,
        _ => return false,
    };
    if let Some(required) = spec.required {
        if !fields
            .get(required)
            .and_then(Value::as_str)
            .is_some_and(|p| !p.trim().is_empty())
        {
            return false;
        }
    }
    let root = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    fields.iter().all(|(key, value)| {
        match spec
            .args
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, kind)| *kind)
        {
            // An argument the table does not know could name anything.
            None => false,
            Some(Arg::Other) => true,
            Some(Arg::Path | Arg::Glob) => value.as_str().is_some_and(|raw| inside(raw, &root)),
        }
    })
}

fn inside(raw: &str, root: &Path) -> bool {
    let raw = raw.trim();
    if raw.is_empty() {
        return true;
    }
    // `~` is the home directory, which is not the chat's.
    if raw.starts_with('~') {
        return false;
    }
    let joined = root.join(raw);
    // Lexically first, so `..` cannot climb out of a path that does not
    // exist yet; then through symlinks, for one that does.
    let mut lexical = PathBuf::new();
    for part in joined.components() {
        match part {
            std::path::Component::ParentDir => {
                if !lexical.pop() {
                    return false;
                }
            }
            std::path::Component::CurDir => {}
            other => lexical.push(other),
        }
    }
    if !lexical.starts_with(root) {
        return false;
    }
    // A glob is not a path; its literal prefix is what can be resolved.
    let literal: PathBuf = lexical
        .components()
        .take_while(|c| {
            !c.as_os_str()
                .to_string_lossy()
                .contains(['*', '?', '[', '{'])
        })
        .collect();
    let mut probe = literal.as_path();
    loop {
        if let Ok(real) = std::fs::canonicalize(probe) {
            return real.starts_with(root);
        }
        match probe.parent() {
            Some(parent) if parent.starts_with(root) => probe = parent,
            _ => return probe.starts_with(root),
        }
    }
}

/// How the driver is launched, resolved once at boot.
#[derive(Debug, Clone)]
struct Launch {
    argv: Vec<String>,
    /// `argv` from the driver on: a chat runs the driver itself, never a
    /// credential wrapper in front of it (`vogt-agent-auth run --`), which
    /// would hand it the deployment's whole manifest.
    driver: Vec<String>,
    /// The template the command came from, which promotion reuses.
    template: Option<String>,
}

/// The command from the agent binary on (after a wrapper's `--`).
fn driver_of(argv: &[String]) -> Vec<String> {
    match argv.iter().position(|a| a == "--") {
        Some(i) if i + 1 < argv.len() => argv[i + 1..].to_vec(),
        _ => argv.to_vec(),
    }
}

/// The engine's environment variables a chat's agent is given: enough to
/// run, resolve names, reach the network through a proxy and verify TLS.
/// Nothing else of the engine's — no tokens, no secrets-manager identity.
const CHAT_ENV_ALLOWLIST: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "LANG",
    "LANGUAGE",
    "LC_ALL",
    "LC_CTYPE",
    "LC_MESSAGES",
    "TZ",
    "TERM",
    "TMPDIR",
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
    "XDG_DATA_HOME",
    "XDG_RUNTIME_DIR",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "NODE_EXTRA_CA_CERTS",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "no_proxy",
    "KLAUDIA_CONFIG_DIR",
];

fn resolve_launch(
    policy: &ChatPolicy,
    templates: &[crate::config::SessionTemplate],
) -> Option<Launch> {
    if !policy.enabled {
        return None;
    }
    if let Some(argv) = policy.command.clone() {
        return (crate::agent_cli::agent_name(&argv).as_deref() == Some(DRIVER)).then_some(
            Launch {
                driver: driver_of(&argv),
                argv,
                template: None,
            },
        );
    }
    let template = match policy.template.as_deref() {
        Some(name) => crate::sessions::resolve_template_in(templates, name).ok()?,
        None => templates.iter().find(|t| {
            t.command
                .as_deref()
                .and_then(crate::agent_cli::agent_name)
                .as_deref()
                == Some(DRIVER)
        })?,
    };
    let argv = template.command.clone()?;
    (crate::agent_cli::agent_name(&argv).as_deref() == Some(DRIVER)).then(|| Launch {
        driver: driver_of(&argv),
        argv,
        template: Some(template.name.clone()),
    })
}

/// Who answers a pending approval, and how the answer gets back.
enum Responder {
    /// The hook is waiting on `POST /gate` for this.
    Gate(oneshot::Sender<Option<String>>),
    /// Klaudia asked with a `can_use_tool` control request.
    Driver { request_id: String },
}

struct Pending {
    approval: ChatApproval,
    responder: Responder,
    tool_name: String,
    input: Value,
}

struct Proc {
    generation: u64,
    stdin: mpsc::UnboundedSender<String>,
    gate_token: String,
    pid: Option<u32>,
}

#[derive(Default)]
struct Live {
    proc: Option<Proc>,
    turns: u32,
    approvals: HashMap<String, Pending>,
    last_activity: Option<Instant>,
    /// Gate decisions not yet matched to the tool result they were for.
    gated: Vec<(String, Value)>,
    /// Write-class calls the agent made, by `tool_use_id`, until their
    /// result arrives: the tripwire's half of [`Live::gated`].
    write_calls: HashMap<String, (String, Value)>,
    /// Each tool call's name by `tool_use_id`, until its result arrives.
    tool_names: HashMap<String, String>,
    /// Driver-side approvals a person allowed, which the gate honours once.
    allowed: Vec<(String, Value, Instant)>,
    /// The turn is being interrupted: its error result is a stop, not a fault.
    interrupting: bool,
    /// Who the chat is for, for the per-creator process cap.
    creator: String,
    /// When the running turn began, for the turn time limit.
    turn_started: Option<Instant>,
}

impl Live {
    fn state(&self) -> ChatState {
        if !self.approvals.is_empty() {
            ChatState::AwaitingApproval
        } else if self.turns > 0 {
            ChatState::Running
        } else {
            ChatState::Idle
        }
    }
}

struct ChatHandle {
    live: Mutex<Live>,
    events: broadcast::Sender<ChatEvent>,
    /// Serialises this chat's appends so entries number in the order they
    /// happened.
    append: tokio::sync::Mutex<()>,
}

pub struct ChatRuntime {
    cfg: Arc<Config>,
    store: ChatStore,
    bus: EventBus,
    sessions: Arc<SessionRegistry>,
    launch: Launch,
    handles: DashMap<Uuid, Arc<ChatHandle>>,
    launching: tokio::sync::Mutex<()>,
    generation: AtomicU64,
    gate_base: parking_lot::RwLock<String>,
}

/// The approval gate's verdict, as the hook prints it.
pub fn gate_allow() -> Value {
    json!({})
}

fn gate_block(reason: &str) -> Value {
    json!({ "decision": "block", "reason": reason })
}

impl ChatRuntime {
    /// `None` when chats are off or no Klaudia launch is configured.
    pub async fn from_config(
        cfg: Arc<Config>,
        bus: EventBus,
        sessions: Arc<SessionRegistry>,
    ) -> Option<Arc<Self>> {
        let launch = resolve_launch(&cfg.chat, &cfg.session_templates)?;
        let store = match ChatStore::new(&cfg.state_dir).await {
            Ok(store) => store,
            Err(e) => {
                tracing::warn!(error = %e, "quick chats disabled: the chat store would not open");
                return None;
            }
        };
        let gate_base = crate::sessions::engine_self_url(cfg.bind)
            .unwrap_or_else(|| sessions.secret_broker().loopback_url());
        // A previous engine's chat processes have no reader now: end them.
        sweep_orphans(&cfg.state_dir.join("chats"));
        Some(Self::with_parts(
            cfg, store, bus, sessions, launch, gate_base,
        ))
    }

    fn with_parts(
        cfg: Arc<Config>,
        store: ChatStore,
        bus: EventBus,
        sessions: Arc<SessionRegistry>,
        launch: Launch,
        gate_base: String,
    ) -> Arc<Self> {
        let runtime = Arc::new(Self {
            cfg,
            store,
            bus,
            sessions,
            launch,
            handles: DashMap::new(),
            launching: tokio::sync::Mutex::new(()),
            generation: AtomicU64::new(0),
            gate_base: parking_lot::RwLock::new(gate_base),
        });
        runtime.spawn_reaper();
        runtime
    }

    /// Where the gate is reached from inside a chat process. Set once the
    /// server listens, for a server bound to port 0 whose port the
    /// configuration does not name.
    pub fn set_gate_base(&self, base: String) {
        *self.gate_base.write() = base;
    }

    /// What `/api/config` advertises.
    pub fn info(&self) -> ChatConfigInfo {
        ChatConfigInfo {
            drivers: vec![ChatDriver {
                name: DRIVER.into(),
                label: "Klaudia".into(),
                models: self.cfg.chat.models.clone(),
            }],
        }
    }

    fn handle(&self, id: Uuid) -> Arc<ChatHandle> {
        Arc::clone(
            self.handles
                .entry(id)
                .or_insert_with(|| {
                    Arc::new(ChatHandle {
                        live: Mutex::new(Live::default()),
                        events: broadcast::channel(256).0,
                        append: tokio::sync::Mutex::new(()),
                    })
                })
                .value(),
        )
    }

    fn summary_of(&self, record: &ChatRecord) -> ChatSummary {
        let (state, live) = match self.handles.get(&record.id) {
            Some(h) => {
                let live = h.live.lock();
                (live.state(), live.proc.is_some())
            }
            None => (ChatState::Idle, false),
        };
        record.summary(state, live)
    }

    async fn record(&self, id: Uuid) -> Result<ChatRecord> {
        self.store.get_chat(id).await?.ok_or(ApiError::NotFound)
    }

    fn validate_model(&self, model: &str) -> Result<Option<String>> {
        let model = model.trim();
        if model.is_empty() || model == "default" {
            return Ok(None);
        }
        if self.cfg.chat.models.iter().any(|m| m.id == model) {
            return Ok(Some(model.to_string()));
        }
        let known: Vec<&str> = self.cfg.chat.models.iter().map(|m| m.id.as_str()).collect();
        Err(ApiError::BadRequest(if known.is_empty() {
            format!(
                "model {model:?} is not offered: this engine configures no chat models, so a chat runs the driver's default (ENGINE_CHAT_MODELS_JSON)"
            )
        } else {
            format!(
                "model {model:?} is not offered; configured: default, {}",
                known.join(", ")
            )
        }))
    }

    /// Publish a change to everyone following the chat and to the engine's
    /// event stream.
    async fn changed(&self, id: Uuid) {
        if let Ok(record) = self.record(id).await {
            let chat = self.summary_of(&record);
            let _ = self.handle(id).events.send(ChatEvent::Chat { chat });
        }
        self.bus.publish(ServerEvent::ChatChanged { id });
    }

    /// Record an entry, bump the chat's counters and tell followers.
    async fn append(&self, id: Uuid, entry: ChatEntry) -> Result<ChatEntry> {
        let handle = self.handle(id);
        let entry = {
            let _order = handle.append.lock().await;
            let entry = self.store.append_entry(id, entry).await?;
            if let Some(mut record) = self.store.get_chat(id).await? {
                record.updated_at = entry.at.clone();
                if matches!(entry.kind.as_str(), "user" | "assistant") {
                    record.message_count += 1;
                    record.preview = preview(&entry.text).or(record.preview);
                }
                self.store.update_chat(&record).await?;
            }
            entry
        };
        let _ = handle.events.send(ChatEvent::Entry {
            entry: entry.clone(),
        });
        self.bus.publish(ServerEvent::ChatChanged { id });
        Ok(entry)
    }

    async fn note(&self, id: Uuid, kind: &str, text: String, retryable: bool) {
        let entry = entry(kind, text, None, None, kind == "error", retryable, None);
        if let Err(e) = self.append(id, entry).await {
            tracing::warn!(chat = %id, error = %e, "could not record a chat entry");
        }
    }

    // ── operations ────────────────────────────────────────────────────

    pub async fn create(
        self: &Arc<Self>,
        req: ChatCreateRequest,
        creator: String,
    ) -> Result<ChatSendResult> {
        if let Some(driver) = req
            .driver
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty())
        {
            if driver != DRIVER {
                return Err(ApiError::BadRequest(format!(
                    "driver {driver:?} is not available; this engine drives chats with {DRIVER}"
                )));
            }
        }
        let model = match req.model.as_deref() {
            Some(m) => self.validate_model(m)?,
            None => self.cfg.chat.models.first().map(|m| m.id.clone()),
        };
        let work_item = vogt_engine_contract::normalize_work_item(req.work_item.as_deref())
            .map_err(ApiError::BadRequest)?;
        let now = now();
        let title = req
            .title
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(|t| truncate_chars(t, 120))
            .unwrap_or_else(|| DEFAULT_TITLE.to_string());
        let record = ChatRecord {
            id: Uuid::new_v4(),
            title,
            driver: DRIVER.into(),
            model,
            creator,
            created_at: now.clone(),
            updated_at: now,
            archived: false,
            work_item,
            promoted_session: None,
            started: false,
            message_count: 0,
            preview: None,
        };
        self.store.insert_chat(&record).await?;
        tracing::info!(
            target: "vogt::audit",
            event = "chat.created",
            chat = %record.id,
            creator = %record.creator,
            "quick chat created"
        );
        self.bus.publish(ServerEvent::ChatChanged { id: record.id });
        match req
            .message
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
        {
            Some(message) => {
                let by = record.creator.clone();
                self.send(record.id, message, by, Duration::ZERO).await
            }
            None => Ok(ChatSendResult {
                chat: self.summary_of(&record),
                entries: Vec::new(),
                finished: true,
            }),
        }
    }

    pub async fn list(&self, query: ChatQuery) -> Result<Vec<ChatSummary>> {
        Ok(self
            .store
            .list_chats(&query)
            .await?
            .iter()
            .map(|r| self.summary_of(r))
            .collect())
    }

    pub async fn get(&self, id: Uuid, tail: usize) -> Result<ChatDetail> {
        let record = self.record(id).await?;
        let entries = self.store.tail(id, tail.clamp(1, 5_000)).await?;
        let approvals = self.handles.get(&id).map_or_else(Vec::new, |h| {
            let mut pending: Vec<ChatApproval> = h
                .live
                .lock()
                .approvals
                .values()
                .map(|p| p.approval.clone())
                .collect();
            pending.sort_by(|a, b| a.requested_at.cmp(&b.requested_at));
            pending
        });
        Ok(ChatDetail {
            chat: self.summary_of(&record),
            entries,
            approvals,
        })
    }

    pub fn subscribe(&self, id: Uuid) -> broadcast::Receiver<ChatEvent> {
        self.handle(id).events.subscribe()
    }

    pub async fn send(
        self: &Arc<Self>,
        id: Uuid,
        text: &str,
        by: String,
        wait: Duration,
    ) -> Result<ChatSendResult> {
        let text = text.trim();
        if text.is_empty() {
            return Err(ApiError::BadRequest("a message needs some text".into()));
        }
        if text.len() > MAX_MESSAGE_BYTES {
            return Err(ApiError::TooLarge(format!(
                "a chat message is at most {MAX_MESSAGE_BYTES} bytes"
            )));
        }
        let mut record = self.record(id).await?;
        if let Some(session) = record.promoted_session {
            return Err(ApiError::Conflict(format!(
                "this chat was promoted to session {session}, which holds the conversation now; talk to it there"
            )));
        }
        if record.title == DEFAULT_TITLE {
            record.title = truncate_chars(&preview(text).unwrap_or_default(), 60);
            self.store.update_chat(&record).await?;
            self.store.index_title(id, &record.title).await?;
        }
        let handle = self.handle(id);
        let mut events = handle.events.subscribe();
        let user = self
            .append(
                id,
                entry(
                    "user",
                    text.to_string(),
                    None,
                    None,
                    false,
                    false,
                    Some(by.clone()),
                ),
            )
            .await?;
        tracing::info!(
            target: "vogt::audit",
            event = "chat.message",
            chat = %id,
            by = %by,
            bytes = text.len(),
            "quick chat message sent"
        );
        let stdin = match self.ensure_process(id).await {
            Ok(stdin) => stdin,
            Err(e) => {
                self.note(
                    id,
                    "error",
                    format!("The chat's agent could not be started: {e}"),
                    true,
                )
                .await;
                return Err(e);
            }
        };
        let line = json!({"type": "user", "message": {"role": "user", "content": text}});
        {
            let mut live = handle.live.lock();
            live.turns += 1;
            if live.turns == 1 {
                live.turn_started = Some(Instant::now());
            }
            live.last_activity = Some(Instant::now());
        }
        if stdin.send(line.to_string()).is_err() {
            handle.live.lock().turns = 0;
            self.note(
                id,
                "error",
                "The chat's agent stopped before it read the message.".into(),
                true,
            )
            .await;
            return Err(ApiError::Conflict(
                "the chat's agent stopped; send the message again".into(),
            ));
        }
        self.changed(id).await;

        let mut finished = false;
        if !wait.is_zero() {
            let deadline = tokio::time::Instant::now() + wait.min(Duration::from_secs(300));
            loop {
                if handle.live.lock().turns == 0 {
                    finished = true;
                    break;
                }
                match tokio::time::timeout_at(deadline, events.recv()).await {
                    Err(_) => break,
                    Ok(Err(broadcast::error::RecvError::Closed)) => break,
                    Ok(_) => {}
                }
            }
        }
        let record = self.record(id).await?;
        let entries = if wait.is_zero() {
            vec![user]
        } else {
            self.store.entries(id, user.seq - 1, 1_000).await?
        };
        Ok(ChatSendResult {
            chat: self.summary_of(&record),
            entries,
            finished,
        })
    }

    pub async fn set_model(self: &Arc<Self>, id: Uuid, model: &str) -> Result<ChatSummary> {
        let model = self.validate_model(model)?;
        let mut record = self.record(id).await?;
        record.model = model.clone();
        record.updated_at = now();
        self.store.update_chat(&record).await?;
        let stdin = self
            .handles
            .get(&id)
            .and_then(|h| h.live.lock().proc.as_ref().map(|p| p.stdin.clone()));
        if let Some(stdin) = stdin {
            // From the next turn; a relaunch passes `--model` instead.
            let request = json!({
                "type": "control_request",
                "request_id": format!("model-{}", Uuid::new_v4()),
                "request": {"subtype": "set_model", "model": model.as_deref().unwrap_or("default")},
            });
            let _ = stdin.send(request.to_string());
        }
        self.note(
            id,
            "notice",
            format!(
                "Model set to {} from the next message.",
                model.as_deref().unwrap_or("the driver's default")
            ),
            false,
        )
        .await;
        self.changed(id).await;
        Ok(self.summary_of(&self.record(id).await?))
    }

    pub async fn interrupt(self: &Arc<Self>, id: Uuid) -> Result<ChatSummary> {
        let record = self.record(id).await?;
        let handle = self.handle(id);
        let (stdin, gates) = {
            let mut live = handle.live.lock();
            if live.turns == 0 {
                return Ok(record.summary(live.state(), live.proc.is_some()));
            }
            live.interrupting = true;
            // Klaudia cancels its own parked asks; a gate the hook is waiting
            // on is denied here so the hook returns.
            let gates: Vec<Pending> = live.approvals.drain().map(|(_, p)| p).collect();
            (live.proc.as_ref().map(|p| p.stdin.clone()), gates)
        };
        for pending in gates {
            self.resolve(id, pending, false, "expired", Some("stopped".into()), None)
                .await;
        }
        if let Some(stdin) = stdin {
            let request = json!({
                "type": "control_request",
                "request_id": format!("stop-{}", Uuid::new_v4()),
                "request": {"subtype": "interrupt"},
            });
            let _ = stdin.send(request.to_string());
        }
        self.changed(id).await;
        Ok(self.summary_of(&self.record(id).await?))
    }

    pub async fn archive(&self, id: Uuid, archived: bool) -> Result<ChatSummary> {
        let mut record = self.record(id).await?;
        record.archived = archived;
        record.updated_at = now();
        self.store.update_chat(&record).await?;
        self.changed(id).await;
        Ok(self.summary_of(&record))
    }

    /// A person's answer to a pending approval.
    pub async fn decide(
        self: &Arc<Self>,
        id: Uuid,
        approval_id: &str,
        allow: bool,
        message: Option<String>,
        decided_by: String,
    ) -> Result<ChatApproval> {
        let pending = self
            .handle(id)
            .live
            .lock()
            .approvals
            .remove(approval_id)
            .ok_or_else(|| {
                ApiError::Conflict(format!(
                    "approval {approval_id} is not pending on this chat (answered, expired or stopped)"
                ))
            })?;
        tracing::info!(
            target: "vogt::audit",
            event = "chat.approval",
            chat = %id,
            approval = %approval_id,
            tool = %pending.tool_name,
            allowed = allow,
            by = %decided_by,
            "quick chat approval decided"
        );
        let status = if allow { "allowed" } else { "denied" };
        Ok(self
            .resolve(id, pending, allow, status, message, Some(decided_by))
            .await)
    }

    /// Answer a pending approval whichever way it was asked, and record it.
    async fn resolve(
        &self,
        id: Uuid,
        pending: Pending,
        allow: bool,
        status: &str,
        message: Option<String>,
        decided_by: Option<String>,
    ) -> ChatApproval {
        let handle = self.handle(id);
        let mut approval = pending.approval;
        approval.status = status.to_string();
        approval.decided_by = decided_by.clone();
        let reason = message
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| match status {
                "expired" => "Nobody answered the approval in time, so this was not run.".into(),
                _ => "A person denied this in Vogt.".into(),
            });
        match pending.responder {
            Responder::Gate(tx) => {
                let _ = tx.send((!allow).then_some(reason));
            }
            Responder::Driver { request_id } => {
                let response = if allow {
                    json!({"behavior": "allow"})
                } else {
                    json!({"behavior": "deny", "message": reason})
                };
                let line = json!({
                    "type": "control_response",
                    "response": {"subtype": "success", "request_id": request_id, "response": response},
                });
                let stdin = handle.live.lock().proc.as_ref().map(|p| p.stdin.clone());
                if let Some(stdin) = stdin {
                    let _ = stdin.send(line.to_string());
                }
                if allow {
                    handle.live.lock().allowed.push((
                        pending.tool_name.clone(),
                        pending.input.clone(),
                        Instant::now(),
                    ));
                }
            }
        }
        let verb = match status {
            "allowed" => "Allowed",
            "denied" => "Denied",
            _ => "Not run (no answer)",
        };
        let who = decided_by.map(|d| format!(" by {d}")).unwrap_or_default();
        self.note(
            id,
            "approval",
            format!("{verb}{who}: {} — {}", approval.tool_name, approval.summary),
            false,
        )
        .await;
        let _ = handle.events.send(ChatEvent::Approval {
            approval: approval.clone(),
        });
        self.changed(id).await;
        approval
    }

    /// The hook's question: may this tool call run?
    pub async fn gate(self: &Arc<Self>, id: Uuid, token: &str, body: Value) -> Result<Value> {
        let handle = self
            .handles
            .get(&id)
            .map(|h| Arc::clone(h.value()))
            .ok_or(ApiError::Unauthorized)?;
        {
            let live = handle.live.lock();
            let expected = live
                .proc
                .as_ref()
                .map(|p| p.gate_token.as_str())
                .unwrap_or("");
            if expected.is_empty()
                || !bool::from(subtle::ConstantTimeEq::ct_eq(
                    expected.as_bytes(),
                    token.as_bytes(),
                ))
            {
                return Err(ApiError::Unauthorized);
            }
        }
        let tool = body
            .get("tool_name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let input = body.get("tool_input").cloned().unwrap_or(Value::Null);
        if tool.is_empty() {
            return Ok(gate_block(
                "the approval gate was asked about a call with no tool name",
            ));
        }
        if classify(&tool, &input, &self.chat_dir(id)) == ToolClass::Free {
            return Ok(gate_allow());
        }
        {
            let mut live = handle.live.lock();
            live.allowed
                .retain(|(_, _, at)| at.elapsed() < ALLOWED_REUSE);
            if let Some(pos) = live
                .allowed
                .iter()
                .position(|(name, value, _)| *name == tool && *value == input)
            {
                live.allowed.remove(pos);
                live.gated.push((tool, input));
                return Ok(gate_allow());
            }
        }
        let (tx, rx) = oneshot::channel();
        let approval = self
            .raise(id, &tool, &input, Responder::Gate(tx), "gate")
            .await;
        let verdict = match tokio::time::timeout(self.cfg.chat.approval_timeout, rx).await {
            Ok(Ok(verdict)) => verdict,
            // Dropped: the chat stopped while it waited.
            Ok(Err(_)) => Some("The chat stopped before this was answered.".into()),
            Err(_) => {
                let pending = handle.live.lock().approvals.remove(&approval.id);
                if let Some(pending) = pending {
                    let _ = self
                        .resolve(id, pending, false, "expired", None, None)
                        .await;
                }
                Some("Nobody answered the approval in time, so this was not run.".into())
            }
        };
        handle.live.lock().gated.push((tool, input));
        Ok(match verdict {
            None => gate_allow(),
            Some(reason) => gate_block(&reason),
        })
    }

    async fn raise(
        &self,
        id: Uuid,
        tool: &str,
        input: &Value,
        responder: Responder,
        source: &str,
    ) -> ChatApproval {
        let handle = self.handle(id);
        let requested = time::OffsetDateTime::now_utc();
        let approval = ChatApproval {
            id: Uuid::new_v4().simple().to_string()[..12].to_string(),
            tool_name: tool.to_string(),
            summary: describe_call(tool, input),
            source: source.to_string(),
            status: "pending".into(),
            requested_at: fmt_time(requested),
            expires_at: fmt_time(requested + self.cfg.chat.approval_timeout),
            decided_by: None,
        };
        handle.live.lock().approvals.insert(
            approval.id.clone(),
            Pending {
                approval: approval.clone(),
                responder,
                tool_name: tool.to_string(),
                input: input.clone(),
            },
        );
        self.note(
            id,
            "approval",
            format!(
                "Approval needed: {} — {}",
                approval.tool_name, approval.summary
            ),
            false,
        )
        .await;
        let _ = handle.events.send(ChatEvent::Approval {
            approval: approval.clone(),
        });
        self.changed(id).await;
        approval
    }

    /// Continue the chat's conversation in a terminal session.
    pub async fn promote(
        self: &Arc<Self>,
        id: Uuid,
        req: ChatPromoteRequest,
    ) -> Result<ChatPromoteResult> {
        let mut record = self.record(id).await?;
        if let Some(session) = record.promoted_session {
            return Err(ApiError::Conflict(format!(
                "this chat was already promoted to session {session}"
            )));
        }
        if !record.started {
            return Err(ApiError::Conflict(
                "this chat has no conversation to continue yet; send it a message first".into(),
            ));
        }
        if self.handle(id).live.lock().turns > 0 {
            return Err(ApiError::Conflict(
                "a turn is still running; wait for it to finish, or stop it, then promote".into(),
            ));
        }
        // One writer per transcript: the chat's process goes before the
        // session resumes the same conversation.
        self.stop_process(id, Duration::from_secs(10)).await;
        let template = req
            .template
            .clone()
            .filter(|t| !t.trim().is_empty())
            .or_else(|| self.launch.template.clone());
        let name = req
            .name
            .clone()
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| format!("chat: {}", truncate_chars(&record.title, 40)));
        let spec = SessionSpec {
            name,
            command: template.is_none().then(|| self.launch.argv.clone()),
            template,
            cwd: req.cwd.clone(),
            resume: Some(id.to_string()),
            model: record.model.clone(),
            work_item: req.work_item.clone().or_else(|| record.work_item.clone()),
            ..Default::default()
        };
        let session = self.sessions.create_launched(spec).await?;
        let summary = session.summary();
        record.promoted_session = Some(session.id);
        record.updated_at = now();
        self.store.update_chat(&record).await?;
        tracing::info!(
            target: "vogt::audit",
            event = "chat.promoted",
            chat = %id,
            session = %session.id,
            "quick chat continued in a session"
        );
        self.note(
            id,
            "notice",
            format!("Continued in session “{}” ({}).", summary.name, session.id),
            false,
        )
        .await;
        self.changed(id).await;
        Ok(ChatPromoteResult {
            chat: self.summary_of(&self.record(id).await?),
            session: summary,
        })
    }

    // ── the process ──────────────────────────────────────────────────

    /// The running process's stdin, launching it first when there is none.
    async fn ensure_process(self: &Arc<Self>, id: Uuid) -> Result<mpsc::UnboundedSender<String>> {
        let handle = self.handle(id);
        if let Some(proc) = handle.live.lock().proc.as_ref() {
            return Ok(proc.stdin.clone());
        }
        // One launch at a time, so two can never both see room for one.
        let _launching = self.launching.lock().await;
        if let Some(proc) = handle.live.lock().proc.as_ref() {
            return Ok(proc.stdin.clone());
        }
        let record = self.record(id).await?;
        self.make_room(&record.creator).await?;
        let dir = self.chat_dir(id);
        std::fs::create_dir_all(dir.join(".klaudia"))?;
        std::fs::write(
            dir.join(".klaudia").join("config.toml"),
            hook_config(self.cfg.chat.approval_timeout),
        )?;

        let mut argv = self.launch.driver.clone();
        argv.extend(
            [
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--verbose",
                "--permission-mode",
                "autonomous",
                "--trusted-project-config",
            ]
            .map(str::to_string),
        );
        argv.push(format!(
            "--ask-timeout={}s",
            self.cfg.chat.approval_timeout.as_secs()
        ));
        if record.started {
            argv.extend(["--resume".to_string(), id.to_string()]);
        } else {
            argv.extend(["--session-id".to_string(), id.to_string()]);
        }
        if let Some(model) = record.model.as_deref() {
            argv.extend(["--model".to_string(), model.to_string()]);
        }

        let gate_token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..])
            .current_dir(&dir)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            // Nothing of the engine's environment by default (WI-1097 review
            // round 1): a chat's agent reads untrusted pages, so what it could
            // be talked into reading out of `/proc/self/environ` is limited to
            // the allowlist below, its gate token and its one provider key.
            .env_clear();
        for (k, v) in std::env::vars_os() {
            if k.to_str().is_some_and(|k| CHAT_ENV_ALLOWLIST.contains(&k)) {
                cmd.env(k, v);
            }
        }
        // The template's own variables are not carried over: a chat's
        // environment is the allowlist, and a name heuristic is no allowlist.
        if let Some((name, value)) = self.provider_key().await {
            cmd.env(name, value);
        }
        let base = self.gate_base.read().clone();
        cmd.env("VOGT_CHAT_ID", id.to_string())
            .env("VOGT_CHAT_GATE_URL", format!("{base}/api/chats/{id}/gate"))
            .env("VOGT_CHAT_GATE_TOKEN", &gate_token);
        #[cfg(unix)]
        cmd.process_group(0);

        let mut child = cmd
            .spawn()
            .map_err(|e| ApiError::Internal(format!("launching {}: {e}", argv[0])))?;
        let pid = child.id();
        if let Some(pid) = pid {
            write_pidfile(&dir, pid);
        }
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let mut child_stdin = child.stdin.take().expect("piped stdin");
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        {
            let mut live = handle.live.lock();
            live.proc = Some(Proc {
                generation,
                stdin: tx.clone(),
                gate_token,
                pid,
            });
            live.creator = record.creator.clone();
            live.last_activity = Some(Instant::now());
        }
        tracing::info!(
            target: "vogt::audit",
            event = "chat.launched",
            chat = %id,
            resumed = record.started,
            model = record.model.as_deref().unwrap_or("default"),
            "quick chat agent launched"
        );

        // stdin: one line per message; closing the channel is EOF, which ends
        // the session cleanly.
        tokio::spawn(async move {
            while let Some(line) = rx.recv().await {
                if child_stdin.write_all(line.as_bytes()).await.is_err()
                    || child_stdin.write_all(b"\n").await.is_err()
                    || child_stdin.flush().await.is_err()
                {
                    break;
                }
            }
        });
        let tail = Arc::new(Mutex::new(String::new()));
        let stderr_tail = Arc::clone(&tail);
        tokio::spawn(async move {
            let mut stderr = stderr;
            let mut buf = vec![0u8; 4096];
            while let Ok(n) = stderr.read(&mut buf).await {
                if n == 0 {
                    break;
                }
                let text = redact(&String::from_utf8_lossy(&buf[..n]));
                tracing::debug!(chat = %id, stderr = %text.trim_end(), "chat agent stderr");
                let mut tail = stderr_tail.lock();
                tail.push_str(&text);
                if tail.len() > STDERR_TAIL {
                    let cut = tail.len() - STDERR_TAIL;
                    let cut = (cut..tail.len())
                        .find(|i| tail.is_char_boundary(*i))
                        .unwrap_or(0);
                    tail.drain(..cut);
                }
            }
        });
        let runtime = Arc::clone(self);
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                runtime.on_line(id, generation, &line).await;
            }
            let status = child.wait().await.ok();
            // The group too: MCP servers and shells the agent started outlive
            // it otherwise.
            kill_group(pid);
            let stderr = tail.lock().clone();
            runtime
                .on_exit(id, generation, status.and_then(|s| s.code()), stderr)
                .await;
        });
        Ok(tx)
    }

    /// The driver's provider key, by the name `ENGINE_CHAT_PROVIDER_KEY`
    /// gives: through the agent-auth manifest when the deployment brokers it
    /// (the engine-side `get`, one entry, manifest-checked), else from the
    /// engine's own environment. `None` leaves the driver to its own config.
    async fn provider_key(&self) -> Option<(String, String)> {
        let name = self.cfg.chat.provider_key.clone()?;
        let broker = self.sessions.secret_broker();
        if let Some(entry) = broker.permitted(&name) {
            match broker.fetch(&entry).await {
                Ok(value) => return Some((name, value.trim_end_matches(['\r', '\n']).to_string())),
                Err(e) => {
                    tracing::warn!(var = %name, error = %e, "could not fetch the chat provider key")
                }
            }
        }
        std::env::var(&name).ok().map(|value| (name, value))
    }

    /// Keep under the process caps: stop the least recently active idle chat
    /// of the same creator (per-creator cap), then of anyone (global cap);
    /// refuse when every running chat is busy.
    async fn make_room(&self, creator: &str) -> Result<()> {
        for scope in [Some(creator), None] {
            let cap = if scope.is_some() {
                self.cfg.chat.max_per_creator
            } else {
                self.cfg.chat.max_processes
            };
            loop {
                let mut running = 0usize;
                let mut idle: Option<(Uuid, Instant)> = None;
                for entry in self.handles.iter() {
                    let live = entry.live.lock();
                    if live.proc.is_none() || scope.is_some_and(|c| live.creator != c) {
                        continue;
                    }
                    running += 1;
                    if live.turns == 0 && live.approvals.is_empty() {
                        let at = live.last_activity.unwrap_or_else(Instant::now);
                        if idle.is_none_or(|(_, oldest)| at < oldest) {
                            idle = Some((*entry.key(), at));
                        }
                    }
                }
                if running < cap {
                    break;
                }
                let Some((oldest, _)) = idle else {
                    return Err(ApiError::Conflict(format!(
                        "{running} chats are already working{}; wait for one to finish, or stop one",
                        if scope.is_some() { " for you" } else { "" }
                    )));
                };
                tracing::info!(chat = %oldest, "stopping an idle chat to make room for another");
                self.stop_process(oldest, Duration::from_secs(10)).await;
            }
        }
        Ok(())
    }

    /// Resident memory of a chat's process tree, in bytes.
    fn rss_of(pid: u32) -> u64 {
        crate::resources::tree_rss_bytes(pid)
    }

    /// Running chat processes and their total resident memory, for status.
    pub fn usage(&self) -> (usize, u64) {
        let pids: Vec<u32> = self
            .handles
            .iter()
            .filter_map(|h| h.live.lock().proc.as_ref().and_then(|p| p.pid))
            .collect();
        (pids.len(), pids.into_iter().map(Self::rss_of).sum())
    }

    fn chat_dir(&self, id: Uuid) -> PathBuf {
        self.cfg.state_dir.join("chats").join(id.to_string())
    }

    fn current(&self, id: Uuid, generation: u64) -> Option<Arc<ChatHandle>> {
        let handle = self.handles.get(&id).map(|h| Arc::clone(h.value()))?;
        let ok = handle
            .live
            .lock()
            .proc
            .as_ref()
            .is_some_and(|p| p.generation == generation);
        ok.then_some(handle)
    }

    async fn on_line(self: &Arc<Self>, id: Uuid, generation: u64, line: &str) {
        let Some(handle) = self.current(id, generation) else {
            return;
        };
        // Not JSON (a wrapper's banner): not part of the protocol.
        let Ok(msg) = serde_json::from_str::<Value>(line) else {
            return;
        };
        handle.live.lock().last_activity = Some(Instant::now());
        match msg.get("type").and_then(Value::as_str).unwrap_or("") {
            "system" if msg.get("subtype").and_then(Value::as_str) == Some("init") => {
                if let Ok(mut record) = self.record(id).await {
                    if !record.started {
                        record.started = true;
                        let _ = self.store.update_chat(&record).await;
                    }
                }
            }
            "assistant" => {
                for block in content_blocks(&msg) {
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            let text = block.get("text").and_then(Value::as_str).unwrap_or("");
                            if !text.trim().is_empty() {
                                let _ = self
                                    .append(
                                        id,
                                        entry(
                                            "assistant",
                                            text.to_string(),
                                            None,
                                            None,
                                            false,
                                            false,
                                            None,
                                        ),
                                    )
                                    .await;
                            }
                        }
                        Some("tool_use") => {
                            let name = block
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("tool")
                                .to_string();
                            let use_id =
                                block.get("id").and_then(Value::as_str).map(str::to_string);
                            let input = block.get("input").cloned().unwrap_or(Value::Null);
                            if let Some(use_id) = use_id.clone() {
                                let mut live = handle.live.lock();
                                live.tool_names.insert(use_id.clone(), name.clone());
                                if classify(&name, &input, &self.chat_dir(id)) == ToolClass::Ask {
                                    live.write_calls
                                        .insert(use_id, (name.clone(), input.clone()));
                                }
                            }
                            let _ = self
                                .append(
                                    id,
                                    entry(
                                        "tool-call",
                                        describe_call(&name, &input),
                                        Some(name),
                                        use_id,
                                        false,
                                        false,
                                        None,
                                    ),
                                )
                                .await;
                        }
                        _ => {}
                    }
                }
            }
            "user" => {
                for block in content_blocks(&msg) {
                    if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                        continue;
                    }
                    let use_id = block
                        .get("tool_use_id")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    let is_error = block
                        .get("is_error")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    let text = redact(&truncate_chars(
                        &result_text(block.get("content")),
                        RESULT_CHARS,
                    ));
                    let bypassed = use_id
                        .as_deref()
                        .is_some_and(|u| self.check_gated(&handle, u, is_error));
                    let name = use_id
                        .as_deref()
                        .and_then(|u| handle.live.lock().tool_names.remove(u));
                    let _ = self
                        .append(
                            id,
                            entry(
                                "tool-result",
                                text,
                                name,
                                use_id.clone(),
                                is_error,
                                false,
                                None,
                            ),
                        )
                        .await;
                    if bypassed {
                        tracing::warn!(
                            target: "vogt::audit",
                            event = "chat.gate_bypassed",
                            chat = %id,
                            tool_use_id = use_id.as_deref().unwrap_or(""),
                            "a write-class tool call ran without passing the approval gate; the chat was stopped"
                        );
                        self.note(
                            id,
                            "error",
                            "A tool call that should have waited for approval ran without passing Vogt's approval gate, so this chat's agent was stopped. Check that Klaudia runs the chat's hooks before sending more.".into(),
                            false,
                        )
                        .await;
                        self.kill_process(id);
                    }
                }
            }
            "tool_progress" => {
                let _ = handle.events.send(ChatEvent::Progress {
                    tool_name: msg
                        .get("tool_name")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    text: msg
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                });
            }
            "warning" | "notice" => {
                let text = msg
                    .get("content")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if !text.is_empty() {
                    self.note(id, "notice", text, false).await;
                }
            }
            "compaction" => {
                self.note(
                    id,
                    "notice",
                    "The conversation so far was summarised to fit the model.".into(),
                    false,
                )
                .await;
            }
            "control_request" => self.on_control_request(id, &handle, &msg).await,
            "control_response" => {
                let response = msg.get("response").unwrap_or(&Value::Null);
                if response.get("subtype").and_then(Value::as_str) == Some("error") {
                    let error = response
                        .get("error")
                        .and_then(Value::as_str)
                        .unwrap_or("refused");
                    self.note(
                        id,
                        "notice",
                        format!("The agent refused a request: {error}"),
                        false,
                    )
                    .await;
                }
            }
            "result" => self.on_result(id, &handle, &msg).await,
            _ => {}
        }
    }

    /// Whether the result for `use_id` is a write-class call that ran
    /// although the gate never saw it.
    fn check_gated(&self, handle: &ChatHandle, use_id: &str, is_error: bool) -> bool {
        let mut live = handle.live.lock();
        let Some((name, input)) = live.write_calls.remove(use_id) else {
            return false;
        };
        let position = live
            .gated
            .iter()
            // The exact call: a gated `Bash ls` must not stand in for an
            // ungated `Bash rm`.
            .position(|(n, v)| *n == name && *v == input);
        match position {
            Some(at) => {
                live.gated.remove(at);
                false
            }
            // Refused before the hook ran (Klaudia's own host gate, a bad
            // argument): nothing ran, so nothing slipped past.
            None => !is_error,
        }
    }

    async fn on_control_request(self: &Arc<Self>, id: Uuid, handle: &ChatHandle, msg: &Value) {
        let request_id = msg
            .get("request_id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let request = msg.get("request").cloned().unwrap_or(Value::Null);
        let stdin = handle.live.lock().proc.as_ref().map(|p| p.stdin.clone());
        let reply = |response: Value| {
            if let Some(stdin) = stdin.as_ref() {
                let _ = stdin
                    .send(json!({"type": "control_response", "response": response}).to_string());
            }
        };
        match request.get("subtype").and_then(Value::as_str) {
            Some("can_use_tool") => {
                let tool = request
                    .get("tool_name")
                    .and_then(Value::as_str)
                    .unwrap_or("tool")
                    .to_string();
                let own_hooks = tool == "Hooks"
                    && request
                        .pointer("/host_change/hooks")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                    && self.is_own_hook_file(id, &request);
                if own_hooks {
                    // The gate itself, which this engine wrote: run it.
                    reply(
                        json!({"subtype": "success", "request_id": request_id, "response": {"behavior": "allow"}}),
                    );
                    return;
                }
                let input = request
                    .get("input")
                    .cloned()
                    .unwrap_or_else(|| request.get("host_change").cloned().unwrap_or(Value::Null));
                self.raise(
                    id,
                    &tool,
                    &input,
                    Responder::Driver { request_id },
                    "driver",
                )
                .await;
            }
            Some(other) => {
                // `ask_user` / `exit_plan`: a chat has no way to put either to
                // a person yet. An error reads as "cancelled" and the model
                // carries on rather than guessing an answer.
                reply(json!({
                    "subtype": "error",
                    "request_id": request_id,
                    "error": format!("{other} is not supported in a Vogt chat; ask in your reply instead"),
                }));
            }
            None => {}
        }
    }

    /// Whether a hooks-approval ask names this chat's own config file.
    fn is_own_hook_file(&self, id: Uuid, request: &Value) -> bool {
        let own = self.chat_dir(id).join(".klaudia").join("config.toml");
        let named: Vec<&str> = request
            .pointer("/host_change/paths")
            .and_then(Value::as_array)
            .map(|paths| paths.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let specifier = request.get("specifier").and_then(Value::as_str);
        let same = |p: &str| {
            let path = Path::new(p);
            path == own
                || std::fs::canonicalize(path).ok() == std::fs::canonicalize(&own).ok()
                    && path.exists()
        };
        let named_ours = (!named.is_empty() && named.iter().all(|p| same(p)))
            || (named.is_empty() && specifier.is_some_and(same));
        // And still exactly what the engine wrote: an approved edit to the
        // file must not get its new hooks waved through.
        named_ours
            && std::fs::read_to_string(&own).ok().as_deref()
                == Some(hook_config(self.cfg.chat.approval_timeout).as_str())
    }

    async fn on_result(self: &Arc<Self>, id: Uuid, handle: &ChatHandle, msg: &Value) {
        let is_error = msg
            .get("is_error")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let interrupted = handle.live.lock().interrupting;
        // What the turn ended with is recorded before the turn counts as
        // over, so a sender waiting for the end reads it.
        if is_error {
            let text = msg
                .get("result")
                .and_then(Value::as_str)
                .unwrap_or("Error")
                .trim()
                .to_string();
            if interrupted {
                self.note(id, "notice", "Stopped.".into(), false).await;
            } else {
                // A provider's refusal mid-turn (WI-1007's opaque 400) is a
                // failed reply the person can send again, not a dead chat.
                self.note(id, "error", text, true).await;
            }
        }
        {
            let mut live = handle.live.lock();
            live.turns = live.turns.saturating_sub(1);
            // The next queued turn, if any, starts its own clock.
            live.turn_started = (live.turns > 0).then(Instant::now);
            if live.turns == 0 {
                live.interrupting = false;
            }
        }
        self.changed(id).await;
    }

    async fn on_exit(
        self: &Arc<Self>,
        id: Uuid,
        generation: u64,
        code: Option<i32>,
        stderr: String,
    ) {
        let Some(handle) = self.current(id, generation) else {
            return;
        };
        let (turns, pending) = {
            let mut live = handle.live.lock();
            live.proc = None;
            let turns = live.turns;
            live.interrupting = false;
            live.turn_started = None;
            live.write_calls.clear();
            live.tool_names.clear();
            live.gated.clear();
            let pending: Vec<Pending> = live.approvals.drain().map(|(_, p)| p).collect();
            (turns, pending)
        };
        for p in pending {
            let _ = self
                .resolve(
                    id,
                    p,
                    false,
                    "expired",
                    Some("The chat's agent stopped.".into()),
                    None,
                )
                .await;
        }
        tracing::info!(
            target: "vogt::audit",
            event = "chat.exited",
            chat = %id,
            code = code.unwrap_or(-1),
            "quick chat agent exited"
        );
        if turns > 0 {
            let last = stderr
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .trim();
            let detail = if last.is_empty() {
                String::new()
            } else {
                format!(": {last}")
            };
            self.note(
                id,
                "error",
                format!(
                    "The chat's agent stopped (exit code {}){detail}. Send the message again to restart it.",
                    code.map_or("unknown".into(), |c| c.to_string())
                ),
                true,
            )
            .await;
        }
        handle.live.lock().turns = 0;
        let _ = std::fs::remove_file(self.chat_dir(id).join(PIDFILE));
        self.changed(id).await;
    }

    /// End the process cleanly (stdin EOF), killing it after `grace`.
    async fn stop_process(&self, id: Uuid, grace: Duration) {
        let Some(handle) = self.handles.get(&id).map(|h| Arc::clone(h.value())) else {
            return;
        };
        let (pid, generation) = {
            let mut live = handle.live.lock();
            let Some(proc) = live.proc.as_mut() else {
                return;
            };
            // Swap the sender for a closed one: the writer task sees the
            // channel end and closes stdin.
            let (closed, _) = mpsc::unbounded_channel();
            proc.stdin = closed;
            (proc.pid, proc.generation)
        };
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline {
            if self.current(id, generation).is_none() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if self.current(id, generation).is_some() {
            kill_group(pid);
        }
    }

    /// Stop a chat's process as the idle reaper would, for tests.
    #[doc(hidden)]
    pub async fn stop_for_test(&self, id: Uuid) {
        self.stop_process(id, Duration::from_secs(10)).await;
    }

    fn kill_process(&self, id: Uuid) {
        if let Some(handle) = self.handles.get(&id) {
            let pid = handle.live.lock().proc.as_ref().and_then(|p| p.pid);
            kill_group(pid);
        }
    }

    /// One pass of the limits: stop a process idle for `idle_after`, stop a
    /// turn past `turn_timeout` (and kill it if it will not stop), kill a
    /// process tree over `max_rss_bytes`.
    async fn police(self: &Arc<Self>) {
        let policy = &self.cfg.chat;
        let mut idle = Vec::new();
        let mut overdue = Vec::new();
        let mut running = Vec::new();
        for h in self.handles.iter() {
            let live = h.live.lock();
            let Some(proc) = live.proc.as_ref() else {
                continue;
            };
            if let Some(pid) = proc.pid {
                running.push((*h.key(), pid));
            }
            if live.turns == 0 && live.approvals.is_empty() {
                if live
                    .last_activity
                    .is_none_or(|at| at.elapsed() >= policy.idle_after)
                {
                    idle.push(*h.key());
                }
            } else if let Some(started) = live.turn_started {
                let over = started.elapsed().saturating_sub(policy.turn_timeout);
                if !over.is_zero() {
                    overdue.push((*h.key(), over, live.interrupting));
                }
            }
        }
        for id in idle {
            let runtime = Arc::clone(self);
            tokio::spawn(async move {
                runtime.stop_process(id, Duration::from_secs(30)).await;
            });
        }
        for (id, over, interrupting) in overdue {
            if !interrupting {
                self.note(
                    id,
                    "error",
                    format!(
                        "This turn ran past the {} minute limit, so it was stopped.",
                        policy.turn_timeout.as_secs() / 60
                    ),
                    true,
                )
                .await;
                let _ = self.interrupt(id).await;
            } else if over > Duration::from_secs(30) {
                // Asked to stop half a minute ago and still going.
                self.kill_process(id);
            }
        }
        for (id, pid) in running {
            let rss = tokio::task::spawn_blocking(move || Self::rss_of(pid))
                .await
                .unwrap_or(0);
            if rss > policy.max_rss_bytes {
                tracing::warn!(chat = %id, rss, "chat process tree over its memory limit; killed");
                self.note(
                    id,
                    "error",
                    format!(
                        "The chat's agent used {} MiB, over the {} MiB limit, and was stopped. Send a message to start it again.",
                        rss >> 20,
                        policy.max_rss_bytes >> 20
                    ),
                    true,
                )
                .await;
                self.kill_process(id);
            }
        }
    }

    /// Stop every chat process that has been idle for `idle_after`.
    fn spawn_reaper(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(15));
            loop {
                tick.tick().await;
                let Some(runtime) = weak.upgrade() else {
                    return;
                };
                runtime.police().await;
            }
        });
    }
}

/// Where a running chat records its process, so the next engine can end
/// it if this one dies without doing so.
const PIDFILE: &str = "agent.pid";

fn write_pidfile(dir: &Path, pid: u32) {
    let started = process_start_ticks(pid).unwrap_or(0);
    if let Err(e) = std::fs::write(dir.join(PIDFILE), format!("{pid} {started}\n")) {
        tracing::warn!(error = %e, "could not record a chat's process");
    }
}

/// A process's start time, in clock ticks since boot (`/proc/<pid>/stat`
/// field 22), which tells a pid from a later process that reused it.
fn process_start_ticks(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after = stat.get(stat.rfind(')')? + 2..)?;
    after.split_whitespace().nth(19)?.parse().ok()
}

/// End every chat process group a previous engine left running: each wrote
/// its pid and start time beside its chat, and one still matching both is
/// that same process, with no reader and a gate nobody will answer.
fn sweep_orphans(chats: &Path) {
    let Ok(dirs) = std::fs::read_dir(chats) else {
        return;
    };
    for dir in dirs.flatten() {
        let file = dir.path().join(PIDFILE);
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        let mut parts = text.split_whitespace();
        let pid = parts.next().and_then(|p| p.parse::<u32>().ok());
        let started = parts.next().and_then(|p| p.parse::<u64>().ok());
        if let (Some(pid), Some(started)) = (pid, started) {
            if process_start_ticks(pid) == Some(started) {
                tracing::warn!(
                    pid,
                    chat = %dir.file_name().to_string_lossy(),
                    "ending a chat process a previous engine left running"
                );
                kill_group(Some(pid));
            }
        }
        let _ = std::fs::remove_file(&file);
    }
}

/// Mask what looks like a credential in text the engine keeps or logs: a
/// provider error that echoes a key or a header must not land in a
/// searchable transcript.
pub fn redact(text: &str) -> String {
    static PREFIXED: once_cell::sync::Lazy<Vec<regex::Regex>> = once_cell::sync::Lazy::new(|| {
        [
            r"(?i)(bearer\s+)[A-Za-z0-9._~+/=-]{8,}",
            r#"(?i)((?:api[_-]?key|token|secret|password|authorization)["']?\s*[:=]\s*["']?)[^\s"',;]{6,}"#,
        ]
        .iter()
        .map(|p| regex::Regex::new(p).expect("redaction pattern"))
        .collect()
    });
    static BARE: once_cell::sync::Lazy<Vec<regex::Regex>> = once_cell::sync::Lazy::new(|| {
        [r"\b(?:sk|pk|rk|ghp|gho|ghs|github_pat|xox[abp])[-_][A-Za-z0-9_-]{12,}"]
            .iter()
            .map(|p| regex::Regex::new(p).expect("redaction pattern"))
            .collect()
    });
    let mut out = text.to_string();
    for pattern in PREFIXED.iter() {
        out = pattern.replace_all(&out, "${1}[redacted]").into_owned();
    }
    for pattern in BARE.iter() {
        out = pattern.replace_all(&out, "[redacted]").into_owned();
    }
    // A long token mixing upper case, lower case and digits reads as a key;
    // a git SHA or a long identifier in one case does not, and stays.
    static LONG: once_cell::sync::Lazy<regex::Regex> = once_cell::sync::Lazy::new(|| {
        regex::Regex::new(r"\b[A-Za-z0-9_\-+/=]{32,}").expect("pattern")
    });
    out = LONG
        .replace_all(&out, |m: &regex::Captures<'_>| {
            let t = &m[0];
            let mixed = t.chars().any(|c| c.is_ascii_uppercase())
                && t.chars().any(|c| c.is_ascii_lowercase())
                && t.chars().any(|c| c.is_ascii_digit());
            if mixed {
                "[redacted]".to_string()
            } else {
                t.to_string()
            }
        })
        .into_owned();
    out
}

fn kill_group(pid: Option<u32>) {
    #[cfg(unix)]
    if let Some(pid) = pid.and_then(|p| i32::try_from(p).ok()) {
        // SAFETY: signalling a process group the engine started.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    let _ = pid;
}

fn content_blocks(msg: &Value) -> Vec<Value> {
    match msg.pointer("/message/content") {
        Some(Value::Array(blocks)) => blocks.clone(),
        _ => Vec::new(),
    }
}

fn result_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        Some(other) if !other.is_null() => other.to_string(),
        _ => String::new(),
    }
}

/// A tool call as a person reads it: the command, the path, the query —
/// whichever the tool's input carries — else its arguments.
pub fn describe_call(tool: &str, input: &Value) -> String {
    redact(&describe_call_raw(tool, input))
}

fn describe_call_raw(tool: &str, input: &Value) -> String {
    for key in [
        "command",
        "file_path",
        "path",
        "url",
        "query",
        "pattern",
        "description",
    ] {
        if let Some(text) = input.get(key).and_then(Value::as_str) {
            return truncate_chars(text, INPUT_CHARS);
        }
    }
    if let Some(summary) = input.get("summary").and_then(Value::as_str) {
        return truncate_chars(summary, INPUT_CHARS);
    }
    let args = match input {
        Value::Null => String::new(),
        other => other.to_string(),
    };
    if args.is_empty() || args == "{}" {
        tool.to_string()
    } else {
        truncate_chars(&args, INPUT_CHARS)
    }
}

fn entry(
    kind: &str,
    text: String,
    tool_name: Option<String>,
    tool_use_id: Option<String>,
    is_error: bool,
    retryable: bool,
    by: Option<String>,
) -> ChatEntry {
    ChatEntry {
        seq: 0,
        at: now(),
        kind: kind.to_string(),
        text,
        tool_name,
        tool_use_id,
        is_error,
        retryable,
        by,
    }
}

fn truncate_chars(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text.to_string(),
    }
}

fn now() -> String {
    fmt_time(time::OffsetDateTime::now_utc())
}

fn fmt_time(at: time::OffsetDateTime) -> String {
    at.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
