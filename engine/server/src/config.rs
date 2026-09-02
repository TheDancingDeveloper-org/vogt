use std::{net::SocketAddr, path::Path};

use serde::{Deserialize, Serialize};

use crate::auth::ScopedTokenConfig;
use crate::error::{ApiError, Result};

const DEFAULT_SCROLLBACK_BYTES: usize = 4 * 1024 * 1024;
const DEFAULT_MUTATING_REQUEST_LIMIT_PER_MINUTE: u32 = 600;

/// Voicemode's STT base-URL list, exactly (1:1 with `VOICEMODE_STT_BASE_URLS`):
/// a local Whisper.cpp first, OpenAI's transcription endpoint as the fallback.
/// Documented, not defaulted — see `DEFAULT_STT_BASE_URLS` for why.
#[cfg(test)]
const VOICEMODE_STT_BASE_URLS: &[&str] = &["http://127.0.0.1:2022/v1", "https://api.openai.com/v1"];
/// Voicemode's TTS base-URL list, exactly (1:1 with `VOICEMODE_TTS_BASE_URLS`):
/// a local Kokoro first, OpenAI's speech endpoint as the fallback.
#[cfg(test)]
const VOICEMODE_TTS_BASE_URLS: &[&str] = &["http://127.0.0.1:8880/v1", "https://api.openai.com/v1"];

/// **Server-side speech is off until a deployment says where it runs.**
///
/// These defaulted to voicemode's local-first list, and that made `/api/config`
/// lie: a half is "enabled" when its list is non-empty, so a deployment with
/// nothing listening on `127.0.0.1:2022` still advertised
/// `assistant_stt_enabled: true`. The client then offered a microphone whose
/// every request fell through the list to a 404 — working, by the fallback
/// rule, but advertised as a capability the deployment does not have. That is
/// the shape FR-O4 exists to forbid: absence must read as absence.
///
/// Empty is therefore the honest default, and it is r20's rule applied to the
/// other end of the same wire — a key with no stated destination is an error,
/// so a destination nobody stated is not a configuration. An operator running
/// voicemode's backends pastes its list (above, and in
/// `deploy/vogt-stack.env.example`) and gets exactly voicemode's behaviour.
const DEFAULT_STT_BASE_URLS: &[&str] = &[];
const DEFAULT_TTS_BASE_URLS: &[&str] = &[];

/// One named OpenAI-compatible backend the assistant may run a turn against
/// (FR-T9).
///
/// A profile is a *route*, not a vendor integration: `base_url`, a key, and
/// the model and effort to use when a request does not say. That is why
/// adding OpenRouter beside The Claw Bay is configuration rather than code,
/// and why a Claude subscription is not expressible here — it has no HTTP
/// API to point a `base_url` at, and is spent through the `claude` CLI
/// session template instead (r16).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssistantProfile {
    /// How a request asks for this profile. Unique across the deployment.
    pub name: String,
    pub base_url: String,
    /// Server-side only. Never advertised — `/api/config` carries names and
    /// models so a client can offer a choice, and nothing that could spend it.
    pub api_key: String,
    pub model: String,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    /// Per-profile, because the hang FR-T7 refuses is a property of one
    /// proxy's routes and not of the deployment: a second profile whose proxy
    /// serves `claude-*` correctly may say so without speaking for the first.
    #[serde(default)]
    pub allow_claude_proxy: bool,
}

/// The name the flat `assistant_*` keys are exposed under, so a deployment
/// that never heard of profiles still has one and can be named by a request.
pub const IMPLICIT_PROFILE_NAME: &str = "default";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionTemplate {
    pub name: String,
    pub description: String,
    pub command: Option<Vec<String>>,
    pub cwd: Option<String>,
    pub env: Vec<(String, String)>,
    #[serde(default)]
    pub default_name: Option<String>,
    #[serde(default)]
    pub match_repo_names: Vec<String>,
    #[serde(default)]
    pub match_path_prefixes: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

impl SessionTemplate {
    pub fn default_templates() -> Vec<Self> {
        vec![
            SessionTemplate {
                name: "Shell".to_string(),
                description: "Default shell session".to_string(),
                command: None,
                cwd: None,
                env: vec![],
                default_name: Some("shell-{timestamp}".to_string()),
                match_repo_names: vec![],
                match_path_prefixes: vec![],
                tags: vec!["shell".to_string()],
            },
            SessionTemplate {
                name: "Node Dev".to_string(),
                description: "Node.js development environment".to_string(),
                command: Some(vec!["bash".to_string()]),
                cwd: None,
                env: vec![("NODE_ENV".to_string(), "development".to_string())],
                default_name: Some("{repo_name}-node-{timestamp}".to_string()),
                match_repo_names: vec![],
                match_path_prefixes: vec![],
                tags: vec!["node".to_string(), "web".to_string()],
            },
            SessionTemplate {
                name: "Rust Build".to_string(),
                description: "Rust development with cargo".to_string(),
                command: Some(vec!["bash".to_string()]),
                cwd: None,
                env: vec![("RUST_BACKTRACE".to_string(), "1".to_string())],
                default_name: Some("{repo_name}-rust-{timestamp}".to_string()),
                match_repo_names: vec![],
                match_path_prefixes: vec![],
                tags: vec!["rust".to_string()],
            },
            // Agent CLIs, wrapped in the credential broker explicitly.
            // Automatic agent-auth wraps only sessions created *without* a
            // command, so a template that supplies one must opt in itself or
            // the provider starts with no brokered credentials.
            SessionTemplate {
                name: "Claude Code (protected)".to_string(),
                description: "Claude Code through agent-auth".to_string(),
                command: Some(vec![
                    "mydevenv2-agent-auth".to_string(),
                    "run".to_string(),
                    "--".to_string(),
                    "claude".to_string(),
                ]),
                cwd: None,
                env: vec![],
                default_name: Some("{repo_name}-claude-{timestamp}".to_string()),
                match_repo_names: vec![],
                match_path_prefixes: vec![],
                tags: vec!["agent".to_string(), "claude".to_string()],
            },
            SessionTemplate {
                name: "Codex (protected)".to_string(),
                description: "Codex CLI through agent-auth".to_string(),
                command: Some(vec![
                    "mydevenv2-agent-auth".to_string(),
                    "run".to_string(),
                    "--".to_string(),
                    "codex".to_string(),
                ]),
                cwd: None,
                env: vec![],
                default_name: Some("{repo_name}-codex-{timestamp}".to_string()),
                match_repo_names: vec![],
                match_path_prefixes: vec![],
                tags: vec!["agent".to_string(), "codex".to_string()],
            },
            SessionTemplate {
                name: "OpenCode (protected)".to_string(),
                description: "OpenCode through agent-auth; capture is the sanitized export"
                    .to_string(),
                command: Some(vec![
                    "mydevenv2-agent-auth".to_string(),
                    "run".to_string(),
                    "--".to_string(),
                    "opencode".to_string(),
                ]),
                cwd: None,
                env: vec![],
                default_name: Some("{repo_name}-opencode-{timestamp}".to_string()),
                match_repo_names: vec![],
                match_path_prefixes: vec![],
                tags: vec!["agent".to_string(), "opencode".to_string()],
            },
            SessionTemplate {
                name: "Python Env".to_string(),
                description: "Python development environment".to_string(),
                command: Some(vec!["bash".to_string()]),
                cwd: None,
                env: vec![("PYTHONUNBUFFERED".to_string(), "1".to_string())],
                default_name: Some("{repo_name}-py-{timestamp}".to_string()),
                match_repo_names: vec![],
                match_path_prefixes: vec![],
                tags: vec!["python".to_string()],
            },
        ]
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub bind: SocketAddr,
    pub token: String,
    pub token_mutating_request_limit_per_minute: u32,
    /// The front door's scoped tokens. Each may name the vogt-core token it is
    /// paired with (FR-S9); by the time `load` returns, any
    /// `vogt_core_token_file` on an entry has been read into its
    /// `vogt_core_token`.
    pub extra_tokens: Vec<ScopedTokenConfig>,
    pub scrollback_bytes: usize,
    pub default_shell: String,
    pub default_cwd: std::path::PathBuf,
    pub activity_idle_after_ms: u64,
    /// How long a session may sit continuously `Idle` before the idle-stall
    /// watcher fires a one-shot push notification. Distinct from
    /// `activity_idle_after_ms` (which is a short quiet-window before
    /// `Running` collapses to `Idle`); this is a much longer "nobody has
    /// looked at this in a while" threshold.
    pub idle_stall_after_ms: u64,
    /// Root the file API operates inside. Any request path is resolved
    /// against this and rejected if it escapes the root.
    pub workspace_root: std::path::PathBuf,
    /// URL the web UI's GUI tab should iframe. Phase 5: point this at
    /// Selkies-GStreamer or KasmVNC. None disables the GUI tab.
    pub gui_stream_url: Option<String>,
    /// Operator attestation that the configured stream has passed an
    /// end-to-end launch-and-render check. False keeps every GUI affordance
    /// withdrawn even when a URL and streamer package are present.
    pub gui_stream_verified: bool,
    /// Whether the deprecated `?token=` query parameter on the WebSocket
    /// attach is still accepted (#518). Off by default: a token in the query
    /// string lands in every front proxy's access log, browser history and
    /// Referer-adjacent surface, and the PWA has spoken first-frame auth for
    /// releases. Set `ENGINE_WS_QUERY_TOKEN=true` only to keep a not-yet-
    /// redeployed client working; every use is logged and the path is slated
    /// for removal.
    pub ws_query_token_allowed: bool,
    /// Where persistent state lives (push subscriptions, VAPID keys).
    /// Defaults to $HOME/.local/share/mydevenv2.
    pub state_dir: std::path::PathBuf,
    /// FCM service-account JSON (the full contents, not a path). Sourced
    /// from `ENGINE_FCM_SERVICE_ACCOUNT_JSON` env or config file. Empty
    /// disables FCM push (web-push still works for browser subscriptions).
    pub fcm_service_account_json: Option<String>,
    /// VAPID `subject` (`mailto:` or `https:` URL). RFC 8292 requires this on
    /// the JWT we sign for web-push.
    pub vapid_subject: String,
    /// Comma-separated allow-list of origins for the CORS layer. Defaults to
    /// the production origin plus the local Vite dev origin. Override with
    /// `ENGINE_ALLOWED_ORIGINS` (comma-separated) or the config file.
    pub allowed_origins: Vec<String>,
    /// When enabled, default interactive sessions are started through the
    /// agent-auth helper so Forgejo/Woodpecker/GitHub/Komodo credentials are
    /// available in the child shell without exporting them from PID 1.
    pub auto_agent_auth: bool,
    /// Helper executable used when `auto_agent_auth` is enabled.
    pub agent_auth_helper: std::path::PathBuf,
    /// Session templates available for quick session creation.
    pub session_templates: Vec<SessionTemplate>,
    /// Bearer key for the assistant's LLM backend. Sourced from
    /// `ENGINE_ASSISTANT_API_KEY` env or config file. Empty disables the
    /// assistant surface entirely (routes 404, PWA hides the tab).
    pub assistant_api_key: Option<String>,
    /// OpenAI-compatible base URL for the assistant backend.
    pub assistant_base_url: String,
    /// Model id sent to the assistant backend.
    pub assistant_model: String,
    /// Upper bound on tool-call rounds per user message.
    pub assistant_max_tool_calls: u32,
    /// Optional `reasoning_effort` forwarded to the backend (e.g. "minimal").
    pub assistant_reasoning_effort: Option<String>,
    /// Send `claude-*` model ids to the OpenAI-compatible backend anyway.
    ///
    /// Off by default because those proxy routes hang rather than answer
    /// (`docs/ENGINE.md` §6, validated August 2026) and a hang is the worst
    /// failure a chat surface can have: it looks like thinking. The escape
    /// hatch exists because the fault is a *proxy's*, not the model's — a
    /// deployment whose proxy serves them correctly is entitled to say so,
    /// and to own the result.
    pub assistant_allow_claude_proxy: bool,
    /// Named backends beside the one the flat keys above describe (FR-T9).
    /// The flat keys become the implicit `default` profile when a key is set,
    /// so this list is *additional* and may be empty.
    pub assistant_profiles: Vec<AssistantProfile>,
    /// Which profile a request that names none runs against. `None` means the
    /// implicit `default` when it exists, else the first configured profile.
    pub assistant_default_profile: Option<String>,
    /// Retention horizon for the durable assistant interaction log (FR-T14), in
    /// days. Enforced on a schedule so the horizon is a configured maximum
    /// rather than whatever the last caller passed. Defaults to 30.
    pub assistant_log_retention_days: u32,
    /// Retention horizon for archived session history (the FTS index and the
    /// raw scrollback logs), in days. Enforced by a daily sweep, mirroring the
    /// assistant-log horizon, so `history.db` and `session-logs/` cannot grow
    /// without bound. Defaults to 30; `0` disables the sweep (keep forever).
    pub history_retention_days: u32,
    /// How many trailing bytes of each *live* session's scrollback a history
    /// search scans when `include_live` is set. Bounds the per-search cost of
    /// live coverage (no DB writes; the scan is read-only). Defaults to 256
    /// KiB.
    pub history_live_scan_bytes: u64,
    /// Server-side speech (FR-T12), configured **independently of the chat
    /// profile**: the whole point of the requirement is that a deployment
    /// whose chat runs through OpenRouter (which does not front audio
    /// uniformly) can still point STT and TTS at their own OpenAI-compatible
    /// audio provider — a cloud endpoint or a local Whisper.cpp/Kokoro pair,
    /// interchangeable by configuration.
    ///
    /// The base URLs are an **ordered list**, adopting voicemode's
    /// `VOICEMODE_STT_BASE_URLS` / `VOICEMODE_TTS_BASE_URLS` semantics: the
    /// engine tries entry 1, and on a connection failure or non-2xx moves to
    /// the next — local first, cloud fallback. Only when the whole list is
    /// unconfigured (empty) or every entry fails does the route return 404, so
    /// the client falls back to on-device or typed input (FR-T6).
    ///
    /// The key is reused for whichever entry needs one — the cloud endpoint —
    /// while a local Whisper.cpp / Kokoro entry needs none, so an entry may
    /// succeed with no key. A key set against an *empty* list is a startup
    /// error, the same destination rule the chat key follows (r20).
    ///
    /// Defaults mirror voicemode exactly (1:1 with `VOICEMODE_*`): STT
    /// `http://127.0.0.1:2022/v1,https://api.openai.com/v1` with model
    /// `whisper-1`; TTS `http://127.0.0.1:8880/v1,https://api.openai.com/v1`
    /// with model `tts-1-hd` and voice `nova`.
    pub assistant_stt_base_urls: Vec<String>,
    pub assistant_stt_api_key: Option<String>,
    /// Transcription model sent to `/audio/transcriptions`. Defaults to
    /// `whisper-1` (voicemode's OpenAI transcription model; a local Whisper.cpp
    /// server serving `large-v2` answers the same OpenAI-compatible route).
    pub assistant_stt_model: String,
    pub assistant_tts_base_urls: Vec<String>,
    pub assistant_tts_api_key: Option<String>,
    /// Speech model sent to `/audio/speech`. Defaults to `tts-1-hd` (voicemode).
    pub assistant_tts_model: String,
    /// Voice name sent to `/audio/speech`; `/audio/speech` requires one.
    /// Defaults to `nova` (voicemode's cloud default).
    pub assistant_tts_voice: String,
    /// Bounded per-attempt timeout for a single speech upstream, in
    /// milliseconds. Applied to each entry in the base-URL list independently,
    /// so a hanging or dead first endpoint cannot stall the request — the
    /// engine gives up on it within this window and moves to the next. Defaults
    /// to 30_000 (30s), enough for a cold local Whisper/Kokoro model yet
    /// bounded; a test can lower it. This is the *per-attempt* bound, not the
    /// whole request, which may try several entries.
    pub assistant_speech_attempt_timeout_ms: u64,
    /// Where vogt-core imports repositories, when this container runs one.
    /// Read from `VOGT_IMPORT_ROOT` — the core's own variable name, left
    /// unprefixed on purpose: one name for one thing. Read once here rather
    /// than per request, so a readiness check
    /// reports the configuration the process started with.
    pub vogt_import_root: Option<std::path::PathBuf>,
    /// The directory vogt-core has been told is *this* server's `state_dir`,
    /// read from `VOGT_ENGINE_STATE_DIR` — the core's own variable name, for
    /// the same reason as the one above. It is what makes `vogt backup` cover
    /// the product rather than the core (NFR-I6), and it is a second place
    /// naming a path this process already knows: if the two drift, the backup
    /// keeps succeeding and covers a directory the engine does not use.
    pub vogt_engine_state_dir: Option<std::path::PathBuf>,
    /// The URL clients reach *this* server at (FR-A8, MERGE §5.3).
    ///
    /// Configured, never inferred, and no default: the URL is an **exposure**
    /// value under NFR-D2. This process binds a container port and is
    /// published somewhere else entirely — a tailnet address, a reverse proxy,
    /// a different port — and a URL it guessed would be wrong in exactly the
    /// deployment the field exists for. From a client, a wrong URL and an
    /// unreachable one look the same.
    ///
    /// r7 made that argument for the core, when the core was the door. r10
    /// makes it one hop out, because it did not weaken on the way: a fronted
    /// process can no more infer this door's published address than this door
    /// could infer its own. `None` is reported as "nobody has said", never
    /// filled in.
    pub public_url: Option<String>,
    /// Base URL of vogt-core on loopback. None disables `/api/vogt` and
    /// `/mcp`: they answer 503 with a named reason rather than
    /// pretending the core is empty (FR-U21). The engine itself keeps
    /// serving — sessions do not depend on the core (FR-E9).
    pub vogt_core_url: Option<String>,
    /// The core token the front door injects on `/api/vogt` for the primary
    /// token, and for any extra token with no pairing of its own (FR-S9).
    /// Server-side only: a browser holds a *front-door* token, never this one.
    /// The per-token pairings live on `extra_tokens`; this is what the proxy
    /// falls back to.
    pub vogt_core_token: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    bind: Option<String>,
    token: Option<String>,
    token_mutating_request_limit_per_minute: Option<u32>,
    extra_tokens: Option<Vec<ScopedTokenConfig>>,
    scrollback_bytes: Option<usize>,
    default_shell: Option<String>,
    default_cwd: Option<String>,
    activity_idle_after_ms: Option<u64>,
    idle_stall_after_ms: Option<u64>,
    workspace_root: Option<String>,
    gui_stream_url: Option<String>,
    gui_stream_verified: Option<bool>,
    ws_query_token_allowed: Option<bool>,
    state_dir: Option<String>,
    fcm_service_account_json: Option<String>,
    vapid_subject: Option<String>,
    allowed_origins: Option<Vec<String>>,
    auto_agent_auth: Option<bool>,
    agent_auth_helper: Option<String>,
    session_templates: Option<Vec<SessionTemplate>>,
    assistant_api_key: Option<String>,
    assistant_base_url: Option<String>,
    assistant_model: Option<String>,
    assistant_allow_claude_proxy: Option<bool>,
    assistant_max_tool_calls: Option<u32>,
    assistant_reasoning_effort: Option<String>,
    assistant_profiles: Option<Vec<AssistantProfile>>,
    assistant_default_profile: Option<String>,
    assistant_log_retention_days: Option<u32>,
    history_retention_days: Option<u32>,
    history_live_scan_bytes: Option<u64>,
    assistant_stt_base_urls: Option<Vec<String>>,
    assistant_stt_api_key: Option<String>,
    assistant_stt_model: Option<String>,
    assistant_tts_base_urls: Option<Vec<String>>,
    assistant_tts_api_key: Option<String>,
    assistant_tts_model: Option<String>,
    assistant_tts_voice: Option<String>,
    assistant_speech_attempt_timeout_ms: Option<u64>,
    public_url: Option<String>,
    vogt_core_url: Option<String>,
    vogt_core_token: Option<String>,
    vogt_core_token_file: Option<String>,
}

pub fn load(
    file: Option<&Path>,
    cli_bind: Option<String>,
    cli_token: Option<String>,
) -> Result<Config> {
    let from_file: FileConfig = match file {
        Some(path) => {
            let raw = std::fs::read_to_string(path)
                .map_err(|e| ApiError::Config(format!("reading {}: {e}", path.display())))?;
            toml::from_str(&raw)
                .map_err(|e| ApiError::Config(format!("parsing {}: {e}", path.display())))?
        }
        None => FileConfig::default(),
    };

    // `ENGINE_BIND`/`ENGINE_TOKEN` are the primary env names (#203); the CLI
    // flag still wins, and `engine_env` supplies the legacy `MYDEVENV2_*`
    // fallback with its deprecation warning. Precedence: CLI flag > env >
    // config file.
    let bind_str = cli_bind
        .or_else(|| {
            engine_env("ENGINE_BIND")
                .ok()
                .filter(|s| !s.trim().is_empty())
        })
        .or(from_file.bind)
        .unwrap_or_else(|| "127.0.0.1:8910".to_string());
    let bind: SocketAddr = bind_str
        .parse()
        .map_err(|e| ApiError::Config(format!("invalid bind {bind_str:?}: {e}")))?;

    let token = cli_token
        .or_else(|| {
            engine_env("ENGINE_TOKEN")
                .ok()
                .filter(|s| !s.trim().is_empty())
        })
        .or(from_file.token)
        .ok_or_else(|| {
            ApiError::Config("token required (ENGINE_TOKEN env or config.token)".into())
        })?;
    if token.len() < 16 {
        return Err(ApiError::Config(
            "token must be at least 16 characters".into(),
        ));
    }
    let token_mutating_request_limit_per_minute =
        parse_u32_env("ENGINE_MUTATING_REQUEST_LIMIT_PER_MINUTE")?
            .or(from_file.token_mutating_request_limit_per_minute)
            .unwrap_or(DEFAULT_MUTATING_REQUEST_LIMIT_PER_MINUTE);

    let mut extra_tokens = from_file.extra_tokens.unwrap_or_default();
    if let Some(env_tokens) = parse_extra_tokens_env("ENGINE_EXTRA_TOKENS_JSON")? {
        extra_tokens.extend(env_tokens);
    }
    validate_extra_tokens(&token, &extra_tokens)?;
    resolve_paired_core_tokens(&mut extra_tokens)?;

    // Profiles from the file, then from the environment, then validated as one
    // list — a container that adds OpenRouter with a variable and a file that
    // already names it must not silently end up with two `openrouter`s, one of
    // which is never reachable.
    let mut assistant_profiles = from_file.assistant_profiles.unwrap_or_default();
    if let Some(env_profiles) = parse_assistant_profiles_env("ENGINE_ASSISTANT_PROFILES_JSON")? {
        assistant_profiles.extend(env_profiles);
    }
    let assistant_default_profile = from_file
        .assistant_default_profile
        .or_else(|| engine_env("ENGINE_ASSISTANT_DEFAULT_PROFILE").ok())
        .filter(|s| !s.trim().is_empty());
    let assistant_api_key = from_file
        .assistant_api_key
        .clone()
        .or_else(|| engine_env("ENGINE_ASSISTANT_API_KEY").ok())
        .filter(|s| !s.trim().is_empty());
    let assistant_key_present = assistant_api_key.is_some();
    validate_assistant_profiles(
        assistant_key_present,
        &assistant_profiles,
        assistant_default_profile.as_deref(),
    )?;

    // Server-side speech (FR-T12). Read independently of the chat keys above:
    // the base URLs and key here point at an OpenAI-compatible *audio* provider,
    // which need not be — and under the POC's OpenRouter chat profile, is not —
    // the same place chat runs. The base URLs are an ordered fallback list
    // (voicemode's `VOICEMODE_*_BASE_URLS` semantics: local first, cloud next);
    // a half is enabled when its list is non-empty. A key against an empty list
    // is refused (r20), matching the chat key's destination rule.
    let assistant_stt_base_urls = parse_url_list(
        from_file.assistant_stt_base_urls,
        engine_env("ENGINE_ASSISTANT_STT_BASE_URLS").ok(),
        DEFAULT_STT_BASE_URLS,
    );
    let assistant_stt_api_key = from_file
        .assistant_stt_api_key
        .or_else(|| engine_env("ENGINE_ASSISTANT_STT_API_KEY").ok())
        .filter(|s| !s.trim().is_empty());
    validate_speech_half(
        "STT",
        "stt",
        &assistant_stt_base_urls,
        &assistant_stt_api_key,
    )?;
    let assistant_tts_base_urls = parse_url_list(
        from_file.assistant_tts_base_urls,
        engine_env("ENGINE_ASSISTANT_TTS_BASE_URLS").ok(),
        DEFAULT_TTS_BASE_URLS,
    );
    let assistant_tts_api_key = from_file
        .assistant_tts_api_key
        .or_else(|| engine_env("ENGINE_ASSISTANT_TTS_API_KEY").ok())
        .filter(|s| !s.trim().is_empty());
    validate_speech_half(
        "TTS",
        "tts",
        &assistant_tts_base_urls,
        &assistant_tts_api_key,
    )?;

    let workspace_root_raw = from_file.workspace_root.map(std::path::PathBuf::from);
    let workspace_root_candidate = workspace_root_raw
        .or_else(|| dirs_home().map(|h| h.join("Working")))
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| std::path::PathBuf::from("."));
    // Canonicalise if possible, but do not make a missing default tree fatal:
    // a stranger cloning the repo has no `$HOME/Working`, so fall back to the
    // current directory (canonicalised) and finally to the raw path rather than
    // aborting startup. A deployment that wants a specific root sets
    // `workspace_root` in the config file and gets that path back.
    let workspace_root = workspace_root_candidate
        .canonicalize()
        .or_else(|_| std::env::current_dir().and_then(|d| d.canonicalize()))
        .unwrap_or(workspace_root_candidate);

    let auto_agent_auth = match engine_env("ENGINE_AUTO_AGENT_AUTH") {
        Ok(v) => Some(parse_bool_env("ENGINE_AUTO_AGENT_AUTH", &v)?),
        Err(std::env::VarError::NotPresent) => None,
        Err(e) => {
            return Err(ApiError::Config(format!(
                "reading ENGINE_AUTO_AGENT_AUTH: {e}"
            )));
        }
    }
    .or(from_file.auto_agent_auth)
    .unwrap_or(false);

    let gui_stream_verified = match std::env::var("GUI_STREAM_VERIFIED") {
        Ok(value) => Some(parse_bool_env("GUI_STREAM_VERIFIED", &value)?),
        Err(std::env::VarError::NotPresent) => None,
        Err(error) => {
            return Err(ApiError::Config(format!(
                "reading GUI_STREAM_VERIFIED: {error}"
            )));
        }
    }
    .or(from_file.gui_stream_verified)
    .unwrap_or(false);

    let ws_query_token_allowed = match std::env::var("ENGINE_WS_QUERY_TOKEN") {
        Ok(value) => Some(parse_bool_env("ENGINE_WS_QUERY_TOKEN", &value)?),
        Err(std::env::VarError::NotPresent) => None,
        Err(error) => {
            return Err(ApiError::Config(format!(
                "reading ENGINE_WS_QUERY_TOKEN: {error}"
            )));
        }
    }
    .or(from_file.ws_query_token_allowed)
    .unwrap_or(false);

    let agent_auth_helper = engine_env("ENGINE_AGENT_AUTH_HELPER")
        .ok()
        .or(from_file.agent_auth_helper)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("/usr/local/bin/mydevenv2-agent-auth"));

    // A file wins over a bare value everywhere it appears: a deployment that
    // brokered the token into a file has gone to the trouble deliberately,
    // and reading an inline copy it also set would silently undo that.
    //
    // The config file can name the file too, and not only the value. It was
    // value-only at first, while a *per-token* pairing could already be a
    // path — so the recommended form was expressible for every token except
    // the deployment-wide one, which is the sort of asymmetry that is
    // discovered by an operator writing the obvious key and getting a
    // refusal that names something else.
    let vogt_core_token = match read_token_path(from_file.vogt_core_token_file.as_deref())? {
        Some(value) => Some(value),
        None => match from_file.vogt_core_token {
            Some(value) => Some(value),
            None => match read_token_file("VOGT_CORE_TOKEN_FILE")? {
                Some(value) => Some(value),
                None => std::env::var("VOGT_CORE_TOKEN").ok(),
            },
        },
    };

    Ok(Config {
        bind,
        token,
        token_mutating_request_limit_per_minute,
        extra_tokens,
        scrollback_bytes: parse_usize_env("ENGINE_SCROLLBACK_BYTES")?
            .or(from_file.scrollback_bytes)
            .unwrap_or(DEFAULT_SCROLLBACK_BYTES),
        default_shell: from_file
            .default_shell
            .or_else(|| std::env::var("SHELL").ok())
            .unwrap_or_else(|| "/bin/bash".to_string()),
        default_cwd: from_file
            .default_cwd
            .map(std::path::PathBuf::from)
            .or_else(dirs_home)
            .unwrap_or_else(|| std::path::PathBuf::from("/tmp")),
        activity_idle_after_ms: from_file.activity_idle_after_ms.unwrap_or(1_500),
        idle_stall_after_ms: parse_u64_env("ENGINE_IDLE_STALL_AFTER_MS")?
            .or(from_file.idle_stall_after_ms)
            .unwrap_or(10 * 60 * 1_000),
        workspace_root,
        gui_stream_url: from_file
            .gui_stream_url
            .or_else(|| std::env::var("GUI_STREAM_URL").ok())
            .filter(|s| !s.is_empty()),
        gui_stream_verified,
        ws_query_token_allowed,
        state_dir: from_file
            .state_dir
            .map(std::path::PathBuf::from)
            .or_else(|| dirs_home().map(|h| h.join(".local/share/mydevenv2")))
            .unwrap_or_else(|| std::path::PathBuf::from("/var/lib/mydevenv2")),
        fcm_service_account_json: from_file
            .fcm_service_account_json
            .or_else(|| engine_env("ENGINE_FCM_SERVICE_ACCOUNT_JSON").ok())
            .filter(|s| !s.trim().is_empty()),
        vapid_subject: from_file
            .vapid_subject
            .or_else(|| engine_env("ENGINE_VAPID_SUBJECT").ok())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "mailto:admin@example.invalid".to_string()),
        allowed_origins: parse_allowed_origins(
            from_file.allowed_origins,
            engine_env("ENGINE_ALLOWED_ORIGINS").ok(),
        ),
        auto_agent_auth,
        agent_auth_helper,
        session_templates: from_file
            .session_templates
            .unwrap_or_else(SessionTemplate::default_templates),
        assistant_api_key,
        assistant_base_url: match from_file
            .assistant_base_url
            .or_else(|| engine_env("ENGINE_ASSISTANT_BASE_URL").ok())
            .filter(|s| !s.trim().is_empty())
        {
            Some(url) => url,
            // The endpoint an API key is sent to is an exposure decision, so
            // it is never guessed: a key with no stated destination is a
            // configuration error, not a silent default provider.
            None if assistant_key_present => {
                return Err(ApiError::Config(
                    "assistant_api_key is set but assistant_base_url is not; \
                     name the OpenAI-compatible endpoint the key belongs to \
                     (assistant_base_url / ENGINE_ASSISTANT_BASE_URL)"
                        .into(),
                ));
            }
            // Without a key the assistant is disabled and this value is
            // never dialled; a non-routable placeholder keeps the field
            // total without inventing a provider.
            None => "https://assistant.invalid/v1".to_string(),
        },
        assistant_model: from_file
            .assistant_model
            .or_else(|| engine_env("ENGINE_ASSISTANT_MODEL").ok())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "gpt-5.4-mini".to_string()),
        assistant_max_tool_calls: parse_u32_env("ENGINE_ASSISTANT_MAX_TOOL_CALLS")?
            .or(from_file.assistant_max_tool_calls)
            .unwrap_or(8),
        assistant_reasoning_effort: from_file
            .assistant_reasoning_effort
            .or_else(|| engine_env("ENGINE_ASSISTANT_REASONING_EFFORT").ok())
            .filter(|s| !s.trim().is_empty()),
        assistant_allow_claude_proxy: from_file
            .assistant_allow_claude_proxy
            .or_else(|| {
                engine_env("ENGINE_ASSISTANT_ALLOW_CLAUDE_PROXY")
                    .ok()
                    .map(|value| matches!(value.trim(), "1" | "true" | "yes"))
            })
            .unwrap_or(false),
        assistant_profiles,
        assistant_default_profile,
        assistant_log_retention_days: parse_u32_env("ENGINE_ASSISTANT_LOG_RETENTION_DAYS")?
            .or(from_file.assistant_log_retention_days)
            .unwrap_or(30),
        history_retention_days: parse_u32_env("ENGINE_HISTORY_RETENTION_DAYS")?
            .or(from_file.history_retention_days)
            .unwrap_or(30),
        history_live_scan_bytes: parse_u64_env("ENGINE_HISTORY_LIVE_SCAN_BYTES")?
            .or(from_file.history_live_scan_bytes)
            .unwrap_or(256 * 1024),
        assistant_stt_base_urls,
        assistant_stt_api_key,
        assistant_stt_model: from_file
            .assistant_stt_model
            .or_else(|| engine_env("ENGINE_ASSISTANT_STT_MODEL").ok())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "whisper-1".to_string()),
        assistant_tts_base_urls,
        assistant_tts_api_key,
        assistant_tts_model: from_file
            .assistant_tts_model
            .or_else(|| engine_env("ENGINE_ASSISTANT_TTS_MODEL").ok())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "tts-1-hd".to_string()),
        assistant_tts_voice: from_file
            .assistant_tts_voice
            .or_else(|| engine_env("ENGINE_ASSISTANT_TTS_VOICE").ok())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "nova".to_string()),
        assistant_speech_attempt_timeout_ms: parse_u64_env("ENGINE_ASSISTANT_SPEECH_TIMEOUT_MS")?
            .or(from_file.assistant_speech_attempt_timeout_ms)
            .unwrap_or(30_000),
        // `ENGINE_PUBLIC_URL` is this process's own address, and is not
        // `VOGT_PUBLIC_URL`: that one is the *core's* view of where it is
        // published, which in the merged shape is an internal detail no
        // client sees. Two names because they are two facts — reusing one
        // would force an operator to keep the inner and outer addresses in
        // sync forever, and getting it wrong is silent (MERGE §5.3).
        public_url: from_file
            .public_url
            .or_else(|| engine_env("ENGINE_PUBLIC_URL").ok())
            .map(|value| value.trim().trim_end_matches('/').to_string())
            .filter(|value| !value.is_empty()),
        // Unprefixed on purpose: these are vogt's own variable names, and the
        // same values configure its CLI inside the container. One name for one
        // thing.
        vogt_core_url: from_file
            .vogt_core_url
            .or_else(|| std::env::var("VOGT_CORE_URL").ok())
            .filter(|s| !s.trim().is_empty()),
        vogt_core_token: vogt_core_token.filter(|s| !s.trim().is_empty()),
        vogt_import_root: std::env::var("VOGT_IMPORT_ROOT")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .map(std::path::PathBuf::from),
        vogt_engine_state_dir: std::env::var("VOGT_ENGINE_STATE_DIR")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .map(std::path::PathBuf::from),
    })
}

/// The engine config-file path from the environment, honouring the historical
/// `MYDEVENV2_CONFIG` alias through `engine_env` (#203).
///
/// Resolved here beside `load` rather than as a clap `env` binding so all three
/// CLI-owned settings — the config path, the bind address and the token — share
/// the one `ENGINE_`-aware lookup and its deprecation warning. A `--config`
/// flag on the command line still wins over this in `main`.
pub fn config_path_from_env() -> Option<std::path::PathBuf> {
    engine_env("ENGINE_CONFIG")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
}

/// Read a token from the file `name` points at, if it points anywhere.
///
/// The file form is the one the container uses: `mydevenv2-agent-auth`
/// brokers Infisical secrets into files under a private temporary directory
/// and exports their paths, which keeps the value out of the process
/// environment — where `/proc/<pid>/environ` and every `docker inspect`
/// would otherwise show it.
/// Read a token from a path the config file gave, if it gave one.
fn read_token_path(path: Option<&str>) -> Result<Option<String>> {
    let Some(path) = path.map(str::trim).filter(|p| !p.is_empty()) else {
        return Ok(None);
    };
    let raw = std::fs::read_to_string(path)
        .map_err(|e| ApiError::Config(format!("reading vogt_core_token_file ({path}): {e}")))?;
    let value = raw.trim().to_string();
    if value.is_empty() {
        // An empty brokered file is a deployment mid-rotation, not a request
        // to fall back to a shared actor; failing the boot is the safe read.
        return Err(ApiError::Config(format!(
            "vogt_core_token_file ({path}) is empty"
        )));
    }
    Ok(Some(value))
}

fn read_token_file(name: &str) -> Result<Option<String>> {
    let Ok(path) = engine_env(name) else {
        return Ok(None);
    };
    if path.trim().is_empty() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(path.trim())
        .map_err(|e| ApiError::Config(format!("reading {name} ({path}): {e}")))?;
    Ok(Some(raw.trim().to_string()).filter(|s| !s.is_empty()))
}

/// Read an engine setting, accepting the historical `MYDEVENV2_` name.
///
/// The prefix is `ENGINE_` (#144). `MYDEVENV2_` is the name of a product that
/// no longer exists, and several of these are extension points a customiser
/// has to type — `PUBLIC_URL`, `ASSISTANT_BASE_URL`, `ALLOWED_ORIGINS`,
/// `BIND` — so they are poor public surface.
///
/// The prefix is not `VOGT_`: that belongs to the core, whose settings this
/// process shares an environment with in the merged image, and `VOGT_ENGINE_`
/// is worse still because `VOGT_ENGINE_URL`, `_STATE_DIR` and `_TOKEN_FILE`
/// are already the *core's* settings describing how it reaches this process.
/// Mixing the two in one namespace would be a worse public surface than the
/// historical name, not a better one.
///
/// Both names are accepted for one release. The old one warns rather than
/// failing, because a deployment moves when its compose file moves and a
/// stack that has not been touched yet must keep starting.
fn engine_env(name: &str) -> std::result::Result<String, std::env::VarError> {
    // `std::env::var`, not `engine_env` — this *is* the lookup.
    match std::env::var(name) {
        Err(std::env::VarError::NotPresent) => {
            let Some(suffix) = name.strip_prefix("ENGINE_") else {
                return Err(std::env::VarError::NotPresent);
            };
            let legacy = format!("MYDEVENV2_{suffix}");
            match std::env::var(&legacy) {
                Ok(value) => {
                    tracing::warn!(
                        legacy = %legacy,
                        current = %name,
                        "environment variable renamed; the old name still works \
                         this release and will stop"
                    );
                    Ok(value)
                }
                Err(_) => Err(std::env::VarError::NotPresent),
            }
        }
        other => other,
    }
}

fn parse_bool_env(name: &str, raw: &str) -> Result<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(ApiError::Config(format!(
            "{name} must be one of 1/0, true/false, yes/no, or on/off"
        ))),
    }
}

fn parse_usize_env(name: &str) -> Result<Option<usize>> {
    match engine_env(name) {
        Ok(v) => {
            let trimmed = v.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            let parsed = trimmed
                .parse::<usize>()
                .map_err(|e| ApiError::Config(format!("{name} must be an integer: {e}")))?;
            Ok(Some(parsed))
        }
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(e) => Err(ApiError::Config(format!("reading {name}: {e}"))),
    }
}

fn parse_u32_env(name: &str) -> Result<Option<u32>> {
    match engine_env(name) {
        Ok(v) => {
            let trimmed = v.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            let parsed = trimmed
                .parse::<u32>()
                .map_err(|e| ApiError::Config(format!("{name} must be an integer: {e}")))?;
            Ok(Some(parsed))
        }
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(e) => Err(ApiError::Config(format!("reading {name}: {e}"))),
    }
}

fn parse_u64_env(name: &str) -> Result<Option<u64>> {
    match engine_env(name) {
        Ok(v) => {
            let trimmed = v.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            let parsed = trimmed
                .parse::<u64>()
                .map_err(|e| ApiError::Config(format!("{name} must be an integer: {e}")))?;
            Ok(Some(parsed))
        }
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(e) => Err(ApiError::Config(format!("reading {name}: {e}"))),
    }
}

fn parse_extra_tokens_env(name: &str) -> Result<Option<Vec<ScopedTokenConfig>>> {
    match engine_env(name) {
        Ok(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            let parsed = serde_json::from_str::<Vec<ScopedTokenConfig>>(trimmed)
                .map_err(|e| ApiError::Config(format!("parsing {name}: {e}")))?;
            Ok(Some(parsed))
        }
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(e) => Err(ApiError::Config(format!("reading {name}: {e}"))),
    }
}

fn parse_assistant_profiles_env(name: &str) -> Result<Option<Vec<AssistantProfile>>> {
    match engine_env(name) {
        Ok(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            let parsed = serde_json::from_str::<Vec<AssistantProfile>>(trimmed)
                .map_err(|e| ApiError::Config(format!("parsing {name}: {e}")))?;
            Ok(Some(parsed))
        }
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(e) => Err(ApiError::Config(format!("reading {name}: {e}"))),
    }
}

/// Refuse at startup rather than at the first request (FR-T9).
///
/// Every fault here is one whose runtime symptom is a request that names a
/// profile and gets somebody else's — a duplicate name resolving to whichever
/// entry the lookup reached first, or a default that points at nothing and
/// silently falls back. Both are the kind of wrong that answers.
fn validate_assistant_profiles(
    implicit_profile_exists: bool,
    profiles: &[AssistantProfile],
    default_profile: Option<&str>,
) -> Result<()> {
    let mut seen: Vec<&str> = Vec::new();
    if implicit_profile_exists {
        seen.push(IMPLICIT_PROFILE_NAME);
    }
    for profile in profiles {
        let name = profile.name.trim();
        if name.is_empty() {
            return Err(ApiError::Config(
                "assistant profile name must not be empty".into(),
            ));
        }
        if seen.contains(&name) {
            return Err(ApiError::Config(format!(
                "duplicate assistant profile {name:?}: a request naming it would \
                 reach whichever one was read first"
            )));
        }
        if profile.base_url.trim().is_empty() {
            return Err(ApiError::Config(format!(
                "assistant profile {name:?} has no base_url"
            )));
        }
        if profile.api_key.trim().is_empty() {
            return Err(ApiError::Config(format!(
                "assistant profile {name:?} has no api_key"
            )));
        }
        if profile.model.trim().is_empty() {
            return Err(ApiError::Config(format!(
                "assistant profile {name:?} has no model"
            )));
        }
        seen.push(name);
    }
    if let Some(default) = default_profile.map(str::trim).filter(|s| !s.is_empty()) {
        if !seen.contains(&default) {
            return Err(ApiError::Config(format!(
                "assistant_default_profile {default:?} is not configured; known \
                 profiles: {}",
                if seen.is_empty() {
                    "none".to_string()
                } else {
                    seen.join(", ")
                }
            )));
        }
    }
    Ok(())
}

/// One half of the server-side speech pipeline (FR-T12) is enabled when its
/// ordered base-URL list is non-empty. A key set against an *empty* list is
/// refused at startup, the same destination rule the chat key follows (r20): a
/// credential the process would carry with nowhere to send it is a
/// misconfiguration, and a silent fall-back to some default audio provider is
/// precisely the wrong repair.
///
/// The reverse — base URLs with no key — is a valid, common configuration: a
/// local Whisper.cpp / Kokoro entry needs no key, and the key (when present) is
/// reused for whichever entry does, the cloud fallback.
fn validate_speech_half(
    label: &str,
    prefix: &str,
    base_urls: &[String],
    api_key: &Option<String>,
) -> Result<()> {
    if api_key.is_some() && base_urls.is_empty() {
        return Err(ApiError::Config(format!(
            "assistant_{prefix}_api_key is set but assistant_{prefix}_base_urls is empty; \
             name the OpenAI-compatible {label} endpoint(s) the key belongs to \
             (assistant_{prefix}_base_urls / ENGINE_ASSISTANT_{}_BASE_URLS)",
            prefix.to_ascii_uppercase()
        )));
    }
    Ok(())
}

/// Resolve an ordered base-URL list: a config-file list wins; else a
/// comma-separated env value (voicemode's form) — where an env var set to the
/// empty string means "disabled", so an operator can turn a half off; else the
/// built-in default. Entries are trimmed and empties dropped.
fn parse_url_list(file: Option<Vec<String>>, env: Option<String>, default: &[&str]) -> Vec<String> {
    if let Some(list) = file {
        return list
            .into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }
    if let Some(raw) = env {
        // A set-but-empty value is a deliberate "off", so it is honoured as an
        // empty list rather than falling through to the default.
        return raw
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }
    default.iter().map(|s| s.to_string()).collect()
}

fn validate_extra_tokens(primary_token: &str, extra_tokens: &[ScopedTokenConfig]) -> Result<()> {
    for token in extra_tokens {
        if token.name.trim().is_empty() {
            return Err(ApiError::Config(
                "extra token name must not be empty".into(),
            ));
        }
        if token.token.len() < 16 {
            return Err(ApiError::Config(format!(
                "extra token {} must be at least 16 characters",
                token.name
            )));
        }
        if token.token == primary_token {
            return Err(ApiError::Config(format!(
                "extra token {} must not duplicate the primary token",
                token.name
            )));
        }
    }
    Ok(())
}

/// Turn each front-door token's `vogt_core_token_file` into the value the
/// proxy will inject for it (FR-S9).
///
/// Read here, once, rather than per request: a credential that changes under a
/// running process would make two requests from the same caller reach the core
/// as two different actors, and the loader is where every other secret in this
/// config is already resolved.
///
/// An unreadable or empty file is a boot failure, not a downgrade. The
/// alternative — treating it as "no pairing" — silently falls back to the
/// shared core token, which is precisely the actor confusion this mapping
/// exists to end, and it would do so without anyone noticing.
fn resolve_paired_core_tokens(extra_tokens: &mut [ScopedTokenConfig]) -> Result<()> {
    for entry in extra_tokens.iter_mut() {
        let Some(path) = entry.vogt_core_token_file.as_deref().map(str::trim) else {
            continue;
        };
        if path.is_empty() {
            continue;
        }
        let raw = std::fs::read_to_string(path).map_err(|e| {
            ApiError::Config(format!(
                "reading vogt_core_token_file for extra token {} ({path}): {e}",
                entry.name
            ))
        })?;
        let value = raw.trim().to_string();
        if value.is_empty() {
            return Err(ApiError::Config(format!(
                "vogt_core_token_file for extra token {} ({path}) is empty",
                entry.name
            )));
        }
        entry.vogt_core_token = Some(value);
    }
    Ok(())
}

fn parse_allowed_origins(file: Option<Vec<String>>, env: Option<String>) -> Vec<String> {
    if let Some(list) = file {
        return list
            .into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }
    if let Some(s) = env {
        let list: Vec<String> = s
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if !list.is_empty() {
            return list;
        }
    }
    // Estate-neutral default: only the local Vite dev server, so a clean clone
    // works for local development without shipping any maintainer's hostnames.
    // A real deployment names its own PWA origin(s) via ENGINE_ALLOWED_ORIGINS
    // (comma-separated) or the config file — the estate stack sets it in
    // `deploy/estate.overlay.yml`.
    vec![
        "http://localhost:5173".to_string(),
        "http://127.0.0.1:5173".to_string(),
    ]
}

fn dirs_home() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(std::path::PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_scrollback_bytes_env() {
        const NAME: &str = "ENGINE_TEST_SCROLLBACK_BYTES_VALID";
        std::env::set_var(NAME, "1048576");
        let parsed = parse_usize_env(NAME).unwrap();
        std::env::remove_var(NAME);

        assert_eq!(parsed, Some(1_048_576));
    }

    #[test]
    fn rejects_invalid_scrollback_bytes_env() {
        const NAME: &str = "ENGINE_TEST_SCROLLBACK_BYTES_INVALID";
        std::env::set_var(NAME, "not-a-number");
        let err = parse_usize_env(NAME).unwrap_err();
        std::env::remove_var(NAME);

        assert!(err.to_string().contains(NAME));
    }

    fn scoped(name: &str) -> ScopedTokenConfig {
        ScopedTokenConfig {
            name: name.to_string(),
            token: "1234567890abcdef".to_string(),
            capabilities: vec![],
            mutating_requests_per_minute: 600,
            vogt_core_token_file: None,
            vogt_core_token: None,
        }
    }

    #[test]
    fn the_deployment_wide_core_token_can_be_a_file_in_the_config() {
        // The recommended form is a brokered file, and until this existed the
        // config file could express it for every *paired* token and not for
        // the fallback — so the obvious key was silently ignored and the
        // refusal named something else entirely.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("core-token");
        std::fs::write(&path, " fallback-core-token \n").unwrap();

        assert_eq!(
            read_token_path(Some(&path.display().to_string())).unwrap(),
            Some("fallback-core-token".to_string())
        );
        assert_eq!(read_token_path(None).unwrap(), None);
        assert_eq!(read_token_path(Some("   ")).unwrap(), None);
    }

    #[test]
    fn an_empty_core_token_file_fails_the_boot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("core-token");
        std::fs::write(&path, "\n").unwrap();

        let err = read_token_path(Some(&path.display().to_string())).unwrap_err();
        assert!(
            err.to_string().contains("is empty"),
            "a deployment mid-rotation must not quietly fall through to a \
             shared actor: {err}"
        );
    }

    #[test]
    fn a_missing_core_token_file_fails_the_boot() {
        let err = read_token_path(Some("/nonexistent/core-token")).unwrap_err();
        assert!(err.to_string().contains("vogt_core_token_file"));
    }

    #[test]
    fn a_paired_core_token_file_is_read_and_wins_over_a_value() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("paired-core-token");
        std::fs::write(&path, "  brokered-core-token\n").unwrap();

        let mut tokens = vec![scoped("agent"), scoped("browser")];
        tokens[0].vogt_core_token_file = Some(path.display().to_string());
        tokens[0].vogt_core_token = Some("the-value-someone-also-left".into());
        tokens[1].vogt_core_token = Some("config-file-core-token".into());

        resolve_paired_core_tokens(&mut tokens).unwrap();

        assert_eq!(
            tokens[0].vogt_core_token.as_deref(),
            Some("brokered-core-token")
        );
        assert_eq!(
            tokens[1].vogt_core_token.as_deref(),
            Some("config-file-core-token"),
            "an entry with no file keeps the value it was configured with"
        );
    }

    #[test]
    fn an_empty_paired_core_token_file_fails_the_boot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty-token");
        std::fs::write(&path, "\n").unwrap();

        let mut tokens = vec![scoped("agent")];
        tokens[0].vogt_core_token_file = Some(path.display().to_string());

        let err = resolve_paired_core_tokens(&mut tokens).unwrap_err();
        let message = err.to_string();
        assert!(message.contains("agent"), "{message}");
        assert!(
            message.contains("empty"),
            "a silent fallback to the shared token is the failure this refuses: {message}"
        );
    }

    #[test]
    fn parses_extra_tokens_env_json() {
        const NAME: &str = "ENGINE_TEST_EXTRA_TOKENS_JSON";
        std::env::set_var(
            NAME,
            r#"[{"name":"readonly","token":"1234567890abcdef","capabilities":["sessions"]}]"#,
        );
        let parsed = parse_extra_tokens_env(NAME).unwrap().unwrap();
        std::env::remove_var(NAME);

        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].name, "readonly");
    }

    // -- Assistant provider profiles (FR-T9) --------------------------------

    fn profile(name: &str, model: &str) -> AssistantProfile {
        AssistantProfile {
            name: name.into(),
            base_url: "https://openrouter.ai/api/v1".into(),
            api_key: "sk-or".into(),
            model: model.into(),
            reasoning_effort: None,
            allow_claude_proxy: false,
        }
    }

    #[test]
    fn two_profiles_with_one_name_are_refused_at_startup() {
        // The runtime symptom of a duplicate is a request that names a
        // profile and reaches whichever entry the lookup happened to find
        // first — an answer, on the wrong key, that nothing reports.
        let err = validate_assistant_profiles(
            false,
            &[profile("openrouter", "a"), profile("openrouter", "b")],
            None,
        )
        .expect_err("a duplicate name must not boot");
        let message = format!("{err:?}");
        assert!(message.contains("openrouter"), "{message}");
    }

    #[test]
    fn a_named_profile_may_not_collide_with_the_implicit_one() {
        assert!(validate_assistant_profiles(true, &[profile("default", "a")], None).is_err());
        assert!(validate_assistant_profiles(false, &[profile("default", "a")], None).is_ok());
    }

    #[test]
    fn a_default_that_names_nothing_is_refused_and_lists_what_exists() {
        // Otherwise it falls back silently and every request runs somewhere
        // other than where the operator wrote down.
        let err =
            validate_assistant_profiles(true, &[profile("openrouter", "a")], Some("openrouterr"))
                .expect_err("an unknown default must not boot");
        let message = format!("{err:?}");
        assert!(message.contains("openrouterr"), "{message}");
        assert!(message.contains("default"), "{message}");
        assert!(message.contains("openrouter"), "{message}");
    }

    #[test]
    fn the_implicit_profile_can_be_the_named_default() {
        assert!(
            validate_assistant_profiles(true, &[profile("openrouter", "a")], Some("default"))
                .is_ok()
        );
        assert!(
            validate_assistant_profiles(false, &[profile("openrouter", "a")], Some("default"))
                .is_err()
        );
    }

    #[test]
    fn a_profile_missing_a_route_or_a_key_is_refused_by_name() {
        // Each of these is a profile that would exist, be offerable in
        // `/api/config`, and fail only when somebody spoke to it.
        type Break = fn(&mut AssistantProfile);
        let cases: [(&str, Break); 3] = [
            ("base_url", |p| p.base_url = "  ".into()),
            ("api_key", |p| p.api_key = String::new()),
            ("model", |p| p.model = String::new()),
        ];
        for (field, mutate) in cases {
            let mut broken = profile("openrouter", "a");
            mutate(&mut broken);
            let err = validate_assistant_profiles(false, &[broken], None)
                .expect_err("a profile with no {field} must not boot");
            let message = format!("{err:?}");
            assert!(message.contains("openrouter"), "{field}: {message}");
            assert!(message.contains(field), "{field}: {message}");
        }
    }

    #[test]
    fn an_empty_profile_name_is_refused() {
        assert!(validate_assistant_profiles(false, &[profile("  ", "a")], None).is_err());
    }

    // -- Server-side speech (FR-T12) ----------------------------------------

    #[test]
    fn a_speech_key_with_an_empty_url_list_is_refused_and_names_the_setting() {
        // The runtime symptom otherwise is a key the process carries with
        // nowhere to send it, or a silent fall-back to some default audio
        // provider — the same failure the chat key's r20 rule refuses.
        let err = validate_speech_half("STT", "stt", &[], &Some("sk-audio".to_string()))
            .expect_err("a key with an empty base-URL list must not boot");
        let message = format!("{err:?}");
        assert!(message.contains("assistant_stt_base_urls"), "{message}");
        assert!(
            message.contains("ENGINE_ASSISTANT_STT_BASE_URLS"),
            "{message}"
        );
    }

    #[test]
    fn a_speech_half_may_have_urls_without_a_key() {
        // A local Whisper.cpp / Kokoro entry needs no key; the key, when
        // present, is reused for whichever entry does (the cloud fallback).
        let urls = vec![
            "http://127.0.0.1:2022/v1".to_string(),
            "https://api.openai.com/v1".to_string(),
        ];
        assert!(validate_speech_half("STT", "stt", &urls, &None).is_ok());
        assert!(validate_speech_half("STT", "stt", &urls, &Some("sk".into())).is_ok());
        // Empty list, no key: disabled, which is a valid state (404 ⇒ client
        // falls back).
        assert!(validate_speech_half("STT", "stt", &[], &None).is_ok());
    }

    #[test]
    fn the_url_list_is_off_unless_configured_and_parses_voicemodes_when_given() {
        // Unset ⇒ off. Advertising a backend nobody configured is the lie
        // FR-O4 forbids, so absence stays absence (see DEFAULT_STT_BASE_URLS).
        assert!(parse_url_list(None, None, DEFAULT_STT_BASE_URLS).is_empty());
        // Voicemode's own list, when a deployment states it, parses 1:1.
        assert_eq!(
            parse_url_list(None, None, VOICEMODE_STT_BASE_URLS),
            vec![
                "http://127.0.0.1:2022/v1".to_string(),
                "https://api.openai.com/v1".to_string(),
            ]
        );
        // A comma-separated env value is split, in order; an env set to empty
        // is a deliberate "off".
        assert_eq!(
            parse_url_list(None, Some(" a , b ,, c ".into()), DEFAULT_STT_BASE_URLS),
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
        assert!(parse_url_list(None, Some("".into()), DEFAULT_STT_BASE_URLS).is_empty());
        // Both halves are off by default, and both take voicemode's list.
        assert!(parse_url_list(None, None, DEFAULT_TTS_BASE_URLS).is_empty());
        assert_eq!(
            parse_url_list(None, None, VOICEMODE_TTS_BASE_URLS),
            vec![
                "http://127.0.0.1:8880/v1".to_string(),
                "https://api.openai.com/v1".to_string(),
            ]
        );
    }
}

#[cfg(test)]
mod prefix_tests {
    use super::*;

    /// Both names work, and the historical one is not silently preferred.
    ///
    /// The rename (#144) has to be survivable by a deployment that has not
    /// been touched yet: a stack still setting `MYDEVENV2_*` must keep
    /// starting, and one setting `ENGINE_*` must win if both are present.
    #[test]
    fn engine_env_accepts_both_names_and_prefers_the_current_one() {
        let current = "ENGINE_PREFIX_TEST_ONE";
        let legacy = "MYDEVENV2_PREFIX_TEST_ONE";

        assert!(engine_env(current).is_err(), "unset means unset");

        unsafe { std::env::set_var(legacy, "from-legacy") };
        assert_eq!(engine_env(current).unwrap(), "from-legacy");

        unsafe { std::env::set_var(current, "from-current") };
        assert_eq!(
            engine_env(current).unwrap(),
            "from-current",
            "the current name wins when both are set"
        );

        unsafe {
            std::env::remove_var(current);
            std::env::remove_var(legacy);
        }
    }

    /// A name that is not `ENGINE_`-prefixed gets no fallback invented for it.
    #[test]
    fn engine_env_only_falls_back_for_engine_names() {
        let other = "SOME_OTHER_PREFIX_TEST";
        unsafe { std::env::set_var("MYDEVENV2_OTHER_PREFIX_TEST", "x") };
        assert!(engine_env(other).is_err());
        unsafe { std::env::remove_var("MYDEVENV2_OTHER_PREFIX_TEST") };
    }

    /// The three settings the CLI parser owns — the token, the bind address and
    /// the config path — used to read their env forms *only* under the legacy
    /// `MYDEVENV2_*` names, so a deployment setting `ENGINE_TOKEN` was silently
    /// ignored. `load` now resolves them through `engine_env`, so `ENGINE_*` is
    /// honoured and `MYDEVENV2_*` still works as a deprecated alias (#203).
    ///
    /// Kept in one test because these three env names are process-global: two
    /// tests setting `ENGINE_TOKEN` in parallel would race. No other test in
    /// this crate touches these names or calls `load`.
    #[test]
    fn load_honours_engine_token_and_bind_and_keeps_the_legacy_aliases() {
        // A config file that only fixes `workspace_root` (to a directory that
        // exists, so the canonicalize in `load` succeeds) — the token and bind
        // are deliberately left for the environment to supply.
        let dir = tempfile::tempdir().unwrap();
        let cfg_path = dir.path().join("engine.toml");
        std::fs::write(
            &cfg_path,
            format!("workspace_root = \"{}\"\n", dir.path().display()),
        )
        .unwrap();

        let clear = || unsafe {
            std::env::remove_var("ENGINE_TOKEN");
            std::env::remove_var("MYDEVENV2_TOKEN");
            std::env::remove_var("ENGINE_BIND");
            std::env::remove_var("MYDEVENV2_BIND");
        };
        clear();

        // `ENGINE_TOKEN` / `ENGINE_BIND` are honoured (they were ignored before).
        unsafe {
            std::env::set_var("ENGINE_TOKEN", "engine-primary-token-0123456789");
            std::env::set_var("ENGINE_BIND", "127.0.0.1:9001");
        }
        let cfg = load(Some(&cfg_path), None, None).unwrap();
        assert_eq!(cfg.token, "engine-primary-token-0123456789");
        assert_eq!(cfg.bind.port(), 9001);

        // The legacy `MYDEVENV2_*` names still work as aliases.
        clear();
        unsafe {
            std::env::set_var("MYDEVENV2_TOKEN", "legacy-alias-token-0123456789");
            std::env::set_var("MYDEVENV2_BIND", "127.0.0.1:9002");
        }
        let cfg = load(Some(&cfg_path), None, None).unwrap();
        assert_eq!(cfg.token, "legacy-alias-token-0123456789");
        assert_eq!(cfg.bind.port(), 9002);

        // The current name wins when both are set.
        unsafe {
            std::env::set_var("ENGINE_TOKEN", "engine-wins-token-0123456789");
        }
        let cfg = load(Some(&cfg_path), None, None).unwrap();
        assert_eq!(cfg.token, "engine-wins-token-0123456789");

        // A `--token` flag still beats the environment entirely.
        let cfg = load(
            Some(&cfg_path),
            None,
            Some("cli-flag-token-0123456789".to_string()),
        )
        .unwrap();
        assert_eq!(cfg.token, "cli-flag-token-0123456789");

        clear();
    }

    /// `ENGINE_CONFIG` resolves the config path with the same `MYDEVENV2_CONFIG`
    /// alias, so `main` can prefer a `--config` flag and fall back to either
    /// env name (#203).
    #[test]
    fn config_path_from_env_honours_both_names() {
        let clear = || unsafe {
            std::env::remove_var("ENGINE_CONFIG");
            std::env::remove_var("MYDEVENV2_CONFIG");
        };
        clear();
        assert!(config_path_from_env().is_none(), "unset means unset");

        unsafe { std::env::set_var("MYDEVENV2_CONFIG", "/etc/legacy.toml") };
        assert_eq!(
            config_path_from_env(),
            Some(std::path::PathBuf::from("/etc/legacy.toml"))
        );

        unsafe { std::env::set_var("ENGINE_CONFIG", "/etc/engine.toml") };
        assert_eq!(
            config_path_from_env(),
            Some(std::path::PathBuf::from("/etc/engine.toml")),
            "the current name wins when both are set"
        );
        clear();
    }
}
