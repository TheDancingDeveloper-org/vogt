use std::{net::SocketAddr, path::Path};

use serde::{Deserialize, Serialize};

use crate::auth::ScopedTokenConfig;
use crate::error::{ApiError, Result};

const DEFAULT_SCROLLBACK_BYTES: usize = 4 * 1024 * 1024;
const DEFAULT_MUTATING_REQUEST_LIMIT_PER_MINUTE: u32 = 600;

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
    pub extra_tokens: Vec<ScopedTokenConfig>,
    pub scrollback_bytes: usize,
    pub default_shell: String,
    pub default_cwd: std::path::PathBuf,
    pub activity_idle_after_ms: u64,
    /// Root the file API operates inside. Any request path is resolved
    /// against this and rejected if it escapes the root.
    pub workspace_root: std::path::PathBuf,
    /// URL the web UI's GUI tab should iframe. Phase 5: point this at
    /// Selkies-GStreamer or KasmVNC. None disables the GUI tab.
    pub gui_stream_url: Option<String>,
    /// Where persistent state lives (push subscriptions, VAPID keys).
    /// Defaults to $HOME/.local/share/mydevenv2.
    pub state_dir: std::path::PathBuf,
    /// FCM service-account JSON (the full contents, not a path). Sourced
    /// from `MYDEVENV2_FCM_SERVICE_ACCOUNT_JSON` env or config file. Empty
    /// disables FCM push (web-push still works for browser subscriptions).
    pub fcm_service_account_json: Option<String>,
    /// VAPID `subject` (`mailto:` or `https:` URL). RFC 8292 requires this on
    /// the JWT we sign for web-push.
    pub vapid_subject: String,
    /// Comma-separated allow-list of origins for the CORS layer. Defaults to
    /// the production origin plus the local Vite dev origin. Override with
    /// `MYDEVENV2_ALLOWED_ORIGINS` (comma-separated) or the config file.
    pub allowed_origins: Vec<String>,
    /// When enabled, default interactive sessions are started through the
    /// agent-auth helper so Forgejo/Woodpecker/GitHub/Komodo credentials are
    /// available in the child shell without exporting them from PID 1.
    pub auto_agent_auth: bool,
    /// Helper executable used when `auto_agent_auth` is enabled.
    pub agent_auth_helper: std::path::PathBuf,
    /// Session templates available for quick session creation.
    pub session_templates: Vec<SessionTemplate>,
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
    workspace_root: Option<String>,
    gui_stream_url: Option<String>,
    state_dir: Option<String>,
    fcm_service_account_json: Option<String>,
    vapid_subject: Option<String>,
    allowed_origins: Option<Vec<String>>,
    auto_agent_auth: Option<bool>,
    agent_auth_helper: Option<String>,
    session_templates: Option<Vec<SessionTemplate>>,
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

    let bind_str = cli_bind
        .or(from_file.bind)
        .unwrap_or_else(|| "127.0.0.1:8910".to_string());
    let bind: SocketAddr = bind_str
        .parse()
        .map_err(|e| ApiError::Config(format!("invalid bind {bind_str:?}: {e}")))?;

    let token = cli_token.or(from_file.token).ok_or_else(|| {
        ApiError::Config("token required (MYDEVENV2_TOKEN env or config.token)".into())
    })?;
    if token.len() < 16 {
        return Err(ApiError::Config(
            "token must be at least 16 characters".into(),
        ));
    }
    let token_mutating_request_limit_per_minute =
        parse_u32_env("MYDEVENV2_MUTATING_REQUEST_LIMIT_PER_MINUTE")?
            .or(from_file.token_mutating_request_limit_per_minute)
            .unwrap_or(DEFAULT_MUTATING_REQUEST_LIMIT_PER_MINUTE);

    let mut extra_tokens = from_file.extra_tokens.unwrap_or_default();
    if let Some(env_tokens) = parse_extra_tokens_env("MYDEVENV2_EXTRA_TOKENS_JSON")? {
        extra_tokens.extend(env_tokens);
    }
    validate_extra_tokens(&token, &extra_tokens)?;

    let workspace_root_raw = from_file.workspace_root.map(std::path::PathBuf::from);
    let workspace_root = workspace_root_raw
        .or_else(|| dirs_home().map(|h| h.join("Working")))
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| std::path::PathBuf::from("/"));
    let workspace_root = workspace_root
        .canonicalize()
        .map_err(|e| ApiError::Config(format!("workspace_root {workspace_root:?}: {e}")))?;

    let auto_agent_auth = match std::env::var("MYDEVENV2_AUTO_AGENT_AUTH") {
        Ok(v) => Some(parse_bool_env("MYDEVENV2_AUTO_AGENT_AUTH", &v)?),
        Err(std::env::VarError::NotPresent) => None,
        Err(e) => {
            return Err(ApiError::Config(format!(
                "reading MYDEVENV2_AUTO_AGENT_AUTH: {e}"
            )));
        }
    }
    .or(from_file.auto_agent_auth)
    .unwrap_or(false);

    let agent_auth_helper = std::env::var("MYDEVENV2_AGENT_AUTH_HELPER")
        .ok()
        .or(from_file.agent_auth_helper)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("/usr/local/bin/mydevenv2-agent-auth"));

    Ok(Config {
        bind,
        token,
        token_mutating_request_limit_per_minute,
        extra_tokens,
        scrollback_bytes: parse_usize_env("MYDEVENV2_SCROLLBACK_BYTES")?
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
        workspace_root,
        gui_stream_url: from_file
            .gui_stream_url
            .or_else(|| std::env::var("GUI_STREAM_URL").ok())
            .filter(|s| !s.is_empty()),
        state_dir: from_file
            .state_dir
            .map(std::path::PathBuf::from)
            .or_else(|| dirs_home().map(|h| h.join(".local/share/mydevenv2")))
            .unwrap_or_else(|| std::path::PathBuf::from("/var/lib/mydevenv2")),
        fcm_service_account_json: from_file
            .fcm_service_account_json
            .or_else(|| std::env::var("MYDEVENV2_FCM_SERVICE_ACCOUNT_JSON").ok())
            .filter(|s| !s.trim().is_empty()),
        vapid_subject: from_file
            .vapid_subject
            .or_else(|| std::env::var("MYDEVENV2_VAPID_SUBJECT").ok())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "mailto:admin@example.invalid".to_string()),
        allowed_origins: parse_allowed_origins(
            from_file.allowed_origins,
            std::env::var("MYDEVENV2_ALLOWED_ORIGINS").ok(),
        ),
        auto_agent_auth,
        agent_auth_helper,
        session_templates: from_file
            .session_templates
            .unwrap_or_else(SessionTemplate::default_templates),
    })
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
    match std::env::var(name) {
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
    match std::env::var(name) {
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

fn parse_extra_tokens_env(name: &str) -> Result<Option<Vec<ScopedTokenConfig>>> {
    match std::env::var(name) {
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
    // Defaults: deployed PWA + Vite dev server. Adjust via MYDEVENV2_ALLOWED_ORIGINS
    // for staging/preview environments.
    vec![
        "https://mydevenv2.sprooty.com".to_string(),
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
        const NAME: &str = "MYDEVENV2_TEST_SCROLLBACK_BYTES_VALID";
        std::env::set_var(NAME, "1048576");
        let parsed = parse_usize_env(NAME).unwrap();
        std::env::remove_var(NAME);

        assert_eq!(parsed, Some(1_048_576));
    }

    #[test]
    fn rejects_invalid_scrollback_bytes_env() {
        const NAME: &str = "MYDEVENV2_TEST_SCROLLBACK_BYTES_INVALID";
        std::env::set_var(NAME, "not-a-number");
        let err = parse_usize_env(NAME).unwrap_err();
        std::env::remove_var(NAME);

        assert!(err.to_string().contains(NAME));
    }

    #[test]
    fn parses_extra_tokens_env_json() {
        const NAME: &str = "MYDEVENV2_TEST_EXTRA_TOKENS_JSON";
        std::env::set_var(
            NAME,
            r#"[{"name":"readonly","token":"1234567890abcdef","capabilities":["sessions"]}]"#,
        );
        let parsed = parse_extra_tokens_env(NAME).unwrap().unwrap();
        std::env::remove_var(NAME);

        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].name, "readonly");
    }
}
