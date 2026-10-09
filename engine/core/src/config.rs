//! Configuration — the single source of truth, ported from `src/vogt/config.py`.
//!
//! Precedence, highest first: explicit overrides, `VOGT_*` environment
//! variables, the TOML file named by `VOGT_CONFIG_FILE`, then the schema
//! defaults. No dotenv. A missing config file is not an error; unknown keys
//! in that file are ignored, while unknown keys passed as overrides are not.

use std::collections::BTreeMap;
use std::env;
use std::fmt;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

pub const ENV_PREFIX: &str = "VOGT_";
pub const CONFIG_FILE_ENV: &str = "VOGT_CONFIG_FILE";

pub const DECLARED_DB_NAME: &str = "declared.sqlite3";
pub const OBSERVED_DB_NAME: &str = "observed.sqlite3";
pub const BACKUPS_DIR_NAME: &str = "backups";
pub const IMPORT_DIR_NAME: &str = "repos";

/// Copied from `src/vogt/core/branches.py`. The matcher is not ported here.
pub const DEFAULT_BRANCH_PATTERNS: [&str; 2] =
    [r"(?i)\bwi-?(?P<n>\d+)\b", r"(?i)\bgh-(?P<forge>\d+)\b"];
pub const DEFAULT_BRANCH_TEMPLATE: &str = "wi-{number}";

/// What a setting's value decides, which is what determines whether it may
/// carry a default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefaultPolicy {
    Exposure,
    Allocation,
    Behaviour,
}

impl DefaultPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exposure => "exposure",
            Self::Allocation => "allocation",
            Self::Behaviour => "behaviour",
        }
    }
}

impl fmt::Display for DefaultPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Debug,
    Info,
    Warning,
    Error,
}

impl LogLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }

    fn parse(raw: &str) -> Result<Self, String> {
        match raw {
            "debug" => Ok(Self::Debug),
            "info" => Ok(Self::Info),
            "warning" => Ok(Self::Warning),
            "error" => Ok(Self::Error),
            other => Err(format!(
                "log_level must be one of debug, info, warning, error, not {other:?}"
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    Text,
    Json,
}

impl LogFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Json => "json",
        }
    }

    fn parse(raw: &str) -> Result<Self, String> {
        match raw {
            "text" => Ok(Self::Text),
            "json" => Ok(Self::Json),
            other => Err(format!(
                "log_format must be one of text, json, not {other:?}"
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqliteSynchronous {
    Off,
    Normal,
    Full,
    Extra,
}

impl SqliteSynchronous {
    fn parse(raw: &str) -> Result<Self, String> {
        match raw {
            "off" => Ok(Self::Off),
            "normal" => Ok(Self::Normal),
            "full" => Ok(Self::Full),
            "extra" => Ok(Self::Extra),
            other => Err(format!(
                "sqlite_synchronous must be one of off, normal, full, extra, not {other:?}"
            )),
        }
    }

    /// The pragma value, which is what SQLite is given.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Normal => "normal",
            Self::Full => "full",
            Self::Extra => "extra",
        }
    }
}

/// One deployment lane whose deployed revision Vogt reads (`deploy_lanes`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployLane {
    pub name: String,
    pub project: String,
    pub branch: String,
    pub receipt_repo: Option<String>,
    pub receipt_path: Option<String>,
    pub version_url: Option<String>,
}

impl DeployLane {
    fn from_value(value: &Value) -> Result<Self, String> {
        let obj = value
            .as_object()
            .ok_or_else(|| "a deploy lane must be a table".to_string())?;
        for key in obj.keys() {
            match key.as_str() {
                "name" | "project" | "branch" | "receipt_repo" | "receipt_path" | "version_url" => {
                }
                other => return Err(format!("deploy lane: unknown key {other:?}")),
            }
        }
        let name = required_string(obj, "name")?;
        if name.is_empty() {
            return Err("deploy lane name must not be empty".to_string());
        }
        let project = required_string(obj, "project")?;
        if project.is_empty() {
            return Err(format!("deploy lane {name:?}: project must not be empty"));
        }
        let branch = optional_string(obj, "branch")?.unwrap_or_else(|| "main".to_string());
        if branch.is_empty() {
            return Err(format!("deploy lane {name:?}: branch must not be empty"));
        }
        let receipt_repo = optional_string(obj, "receipt_repo")?;
        let receipt_path = optional_string(obj, "receipt_path")?;
        let version_url = optional_string(obj, "version_url")?;
        if receipt_repo.is_none() != receipt_path.is_none() {
            return Err(format!(
                "deploy lane {name:?}: give receipt_repo and receipt_path together"
            ));
        }
        if receipt_repo.is_none() && version_url.is_none() {
            return Err(format!(
                "deploy lane {name:?} names no receipt and no version_url"
            ));
        }
        if let Some(url) = &version_url {
            if !(url.starts_with("https://") || url.starts_with("http://")) {
                return Err(format!("deploy lane {name:?}: version_url must be http(s)"));
            }
        }
        Ok(Self {
            name,
            project,
            branch,
            receipt_repo,
            receipt_path,
            version_url,
        })
    }
}

fn required_string(obj: &Map<String, Value>, key: &str) -> Result<String, String> {
    match obj.get(key) {
        Some(Value::String(text)) => Ok(text.clone()),
        Some(_) => Err(format!("deploy lane {key} must be a string")),
        None => Err(format!("deploy lane missing {key}")),
    }
}

fn optional_string(obj: &Map<String, Value>, key: &str) -> Result<Option<String>, String> {
    match obj.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(format!("deploy lane {key} must be a string")),
    }
}

/// The configuration of one Vogt instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VogtConfig {
    pub data_dir: PathBuf,
    pub import_root: Option<PathBuf>,
    pub public_url: Option<String>,
    pub fronted: bool,
    pub log_level: LogLevel,
    pub log_format: LogFormat,
    pub log_requests: bool,
    pub log_slow_request_ms: i64,
    pub log_quiet_paths: Vec<String>,
    pub contract_required_files: Vec<String>,
    pub contract_required_dirs: Vec<String>,
    pub contract_required_meta: Vec<String>,
    pub contract_version: String,
    pub marker_promotion_patterns: Vec<String>,
    pub inbox_bot_logins: Vec<String>,
    pub ci_alert_branches: Vec<String>,
    pub ci_alert_tags: Vec<String>,
    pub ci_watch_notify_sessions: bool,
    pub deploy_lanes: Vec<DeployLane>,
    pub marker_file_extensions: Vec<String>,
    pub branch_binding_patterns: Vec<String>,
    pub branch_binding_template: String,
    pub retention_days: i64,
    pub github_token_file: Option<PathBuf>,
    pub forge_account_key_file: Option<PathBuf>,
    pub forge_token_files: BTreeMap<String, PathBuf>,
    pub agent_activity_roots: BTreeMap<String, PathBuf>,
    pub session_transcript_roots: BTreeMap<String, PathBuf>,
    pub agent_activity_max_bytes_per_sweep: i64,
    pub agent_activity_services: BTreeMap<String, String>,
    pub engine_url: Option<String>,
    pub session_scratch_project: Option<String>,
    pub engine_state_dir: Option<PathBuf>,
    pub engine_token_file: Option<PathBuf>,
    pub bootstrap_core_token_file: Option<PathBuf>,
    pub bootstrap_core_token_actor: String,
    pub bootstrap_core_token_scopes: String,
    pub agent_session_scopes: String,
    pub bootstrap_agent_token_file: Option<PathBuf>,
    pub bootstrap_agent_token_actor: String,
    pub session_ttl_days: i64,
    pub install_bootstrap_enabled: bool,
    pub sqlite_synchronous: SqliteSynchronous,
    pub sweep_interval_seconds: i64,
    pub verify_horizon_hours: i64,
    pub image_digest: Option<String>,
    pub diagnostics_peer_url: Option<String>,
    pub diagnostics_peer_token_file: Option<PathBuf>,
}

/// Where an instance lives when the operator has not said otherwise.
pub fn default_data_dir() -> PathBuf {
    match env::var("XDG_DATA_HOME") {
        Ok(xdg) if !xdg.is_empty() => PathBuf::from(xdg).join("vogt"),
        _ => home_dir().join(".local").join("share").join("vogt"),
    }
}

fn home_dir() -> PathBuf {
    env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn expand_user(path: &Path) -> PathBuf {
    let raw = path.to_string_lossy();
    if raw == "~" {
        return home_dir();
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        return home_dir().join(rest);
    }
    path.to_path_buf()
}

impl Default for VogtConfig {
    fn default() -> Self {
        Self {
            data_dir: default_data_dir(),
            import_root: None,
            public_url: None,
            fronted: false,
            log_level: LogLevel::Info,
            log_format: LogFormat::Text,
            log_requests: true,
            log_slow_request_ms: 1000,
            log_quiet_paths: vec![
                "/health/live".to_string(),
                "/health/ready".to_string(),
                "/version".to_string(),
            ],
            contract_required_files: vec![
                "AGENTS.md".to_string(),
                "README.md".to_string(),
                "LICENSE".to_string(),
            ],
            contract_required_dirs: vec![
                "docs".to_string(),
                "design".to_string(),
                "src".to_string(),
            ],
            contract_required_meta: vec![
                "name".to_string(),
                "lifecycle_state".to_string(),
                "owner".to_string(),
            ],
            contract_version: "v1".to_string(),
            marker_promotion_patterns: vec!["TODO(vogt)".to_string(), "FIXME(vogt)".to_string()],
            inbox_bot_logins: vec![
                "dependabot".to_string(),
                "renovate".to_string(),
                "renovate-bot".to_string(),
                "github-actions".to_string(),
            ],
            ci_alert_branches: vec!["main".to_string(), "master".to_string(), "prod".to_string()],
            ci_alert_tags: vec!["v*".to_string()],
            ci_watch_notify_sessions: true,
            deploy_lanes: Vec::new(),
            marker_file_extensions: vec![
                ".py".to_string(),
                ".rs".to_string(),
                ".ts".to_string(),
                ".tsx".to_string(),
                ".js".to_string(),
                ".jsx".to_string(),
                ".go".to_string(),
                ".java".to_string(),
                ".rb".to_string(),
                ".sh".to_string(),
                ".sql".to_string(),
                ".toml".to_string(),
                ".yaml".to_string(),
                ".yml".to_string(),
                ".md".to_string(),
            ],
            branch_binding_patterns: DEFAULT_BRANCH_PATTERNS
                .iter()
                .map(|pattern| (*pattern).to_string())
                .collect(),
            branch_binding_template: DEFAULT_BRANCH_TEMPLATE.to_string(),
            retention_days: 180,
            github_token_file: None,
            forge_account_key_file: None,
            forge_token_files: BTreeMap::new(),
            agent_activity_roots: BTreeMap::new(),
            session_transcript_roots: default_session_roots(),
            agent_activity_max_bytes_per_sweep: 32 * 1024 * 1024,
            agent_activity_services: BTreeMap::new(),
            engine_url: None,
            session_scratch_project: None,
            engine_state_dir: None,
            engine_token_file: None,
            bootstrap_core_token_file: None,
            bootstrap_core_token_actor: "agent:vogt-engine".to_string(),
            bootstrap_core_token_scopes: "read,work.write,project.write".to_string(),
            agent_session_scopes: "read,work.write,project.write,writeback".to_string(),
            bootstrap_agent_token_file: None,
            bootstrap_agent_token_actor: "agent:vogt-sessions".to_string(),
            session_ttl_days: 30,
            install_bootstrap_enabled: true,
            sqlite_synchronous: SqliteSynchronous::Normal,
            sweep_interval_seconds: 900,
            verify_horizon_hours: 24,
            image_digest: None,
            diagnostics_peer_url: None,
            diagnostics_peer_token_file: None,
        }
    }
}

fn default_session_roots() -> BTreeMap<String, PathBuf> {
    let mut roots = BTreeMap::new();
    roots.insert("claude".to_string(), PathBuf::from("~/.claude/projects"));
    roots.insert("codex".to_string(), PathBuf::from("~/.codex/sessions"));
    roots.insert("klaudia".to_string(), PathBuf::from("~/.klaudia/sessions"));
    roots
}

impl VogtConfig {
    pub fn declared_db_path(&self) -> PathBuf {
        self.resolved_data_dir().join(DECLARED_DB_NAME)
    }

    pub fn observed_db_path(&self) -> PathBuf {
        self.resolved_data_dir().join(OBSERVED_DB_NAME)
    }

    pub fn backups_dir(&self) -> PathBuf {
        self.resolved_data_dir().join(BACKUPS_DIR_NAME)
    }

    pub fn resolved_data_dir(&self) -> PathBuf {
        expand_user(&self.data_dir)
    }

    /// Where `project.import` clones to. Unset means `<data_dir>/repos`.
    pub fn resolved_import_root(&self) -> PathBuf {
        match &self.import_root {
            Some(root) => expand_user(root),
            None => self.resolved_data_dir().join(IMPORT_DIR_NAME),
        }
    }
}

/// Build the configuration, applying the documented precedence.
const MAP_FIELDS: &[&str] = &[
    "forge_token_files",
    "agent_activity_roots",
    "session_transcript_roots",
    "agent_activity_services",
];

/// Pydantic-settings deep-merges the map settings across the file, the
/// environment and explicit overrides. A higher source adds or replaces keys;
/// it does not drop the ones a lower source set.
fn merge_value(into: &mut Map<String, Value>, key: String, value: Value) {
    if MAP_FIELDS.contains(&key.as_str()) {
        if let (Some(Value::Object(existing)), Value::Object(incoming)) =
            (into.get_mut(&key), &value)
        {
            for (inner, entry) in incoming {
                existing.insert(inner.clone(), entry.clone());
            }
            return;
        }
    }
    into.insert(key, value);
}

pub fn load_config(overrides: &Map<String, Value>) -> Result<VogtConfig, String> {
    let mut merged = Map::new();
    if let Some(file) = read_config_file()? {
        for (key, value) in file {
            merge_value(&mut merged, key, value);
        }
    }
    for (key, value) in env_settings()? {
        merge_value(&mut merged, key, value);
    }
    for (key, value) in overrides {
        if !FIELD_NAMES.contains(&key.as_str()) {
            return Err(format!("unknown setting {key:?}"));
        }
        merge_value(&mut merged, key.clone(), value.clone());
    }
    config_from_map(&merged)
}

fn read_config_file() -> Result<Option<Map<String, Value>>, String> {
    let Ok(raw) = env::var(CONFIG_FILE_ENV) else {
        return Ok(None);
    };
    if raw.is_empty() {
        return Ok(None);
    }
    let path = expand_user(Path::new(&raw));
    if !path.is_file() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path).map_err(|err| format!("{err}"))?;
    let table: toml::Table = text.parse().map_err(|err| format!("{err}"))?;
    let mut known = Map::new();
    for (key, value) in table {
        if FIELD_NAMES.contains(&key.as_str()) {
            known.insert(key, toml_to_json(value));
        }
    }
    Ok(Some(known))
}

fn toml_to_json(value: toml::Value) -> Value {
    match value {
        toml::Value::String(text) => Value::String(text),
        toml::Value::Integer(number) => Value::Number(number.into()),
        toml::Value::Float(number) => serde_json::Number::from_f64(number)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        toml::Value::Boolean(flag) => Value::Bool(flag),
        toml::Value::Datetime(stamp) => Value::String(stamp.to_string()),
        toml::Value::Array(items) => Value::Array(items.into_iter().map(toml_to_json).collect()),
        toml::Value::Table(table) => {
            let mut map = Map::new();
            for (key, value) in table {
                map.insert(key, toml_to_json(value));
            }
            Value::Object(map)
        }
    }
}

fn env_settings() -> Result<Map<String, Value>, String> {
    let mut values = Map::new();
    for field in FIELD_CATALOGUE {
        let key = format!("{ENV_PREFIX}{}", field.name.to_ascii_uppercase());
        // pydantic-settings matches the prefix case-insensitively, so an
        // existing stack that exports `vogt_log_level` loads the same value
        // as one that exports `VOGT_LOG_LEVEL`.
        let Some(raw) = env_value(&key) else {
            continue;
        };
        values.insert(
            field.name.to_string(),
            parse_env_value(field, &raw).map_err(|err| format!("{key}: {err}"))?,
        );
    }
    Ok(values)
}

fn env_value(name: &str) -> Option<String> {
    env::vars().find_map(|(key, value)| key.eq_ignore_ascii_case(name).then_some(value))
}

fn parse_env_value(field: &FieldDoc, raw: &str) -> Result<Value, String> {
    match field.kind {
        Kind::Bool => Ok(Value::Bool(parse_bool(raw)?)),
        Kind::Int { .. } => Ok(Value::Number(
            int_from_str(raw)
                .ok_or_else(|| format!("expected an integer, got {raw:?}"))?
                .into(),
        )),
        Kind::String
        | Kind::Path
        | Kind::OptString
        | Kind::OptPath
        | Kind::LogLevel
        | Kind::LogFormat
        | Kind::Sqlite => Ok(Value::String(raw.to_string())),
        Kind::StringList
        | Kind::DeployLanes
        | Kind::PathMap
        | Kind::StringMap
        | Kind::ActivityRoots
        | Kind::SessionRoots => serde_json::from_str(raw)
            .map_err(|_| format!("expected JSON for {kind_name}", kind_name = field.name)),
    }
}

fn parse_bool(raw: &str) -> Result<bool, String> {
    match raw.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" | "y" | "t" => Ok(true),
        "0" | "false" | "no" | "off" | "n" | "f" => Ok(false),
        _ => Err(format!("expected a boolean, got {raw:?}")),
    }
}

/// Pydantic's lax mode. A bool may arrive as a string (`"yes"`, `"1"`) or an
/// integer (`1`, `0`), and an int as a string (`"7"`, `"1_000"`, `" 5 "`), a
/// whole float (`7.0`) or a bool (`true` → 1). Rejecting these strictly is what
/// made an existing stack's TOML fail to load.
fn as_bool(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(flag) => Some(*flag),
        Value::Number(number) => number
            .as_i64()
            .or_else(|| {
                number
                    .as_f64()
                    .and_then(|n| (n.fract() == 0.0).then_some(n as i64))
            })
            .and_then(|n| match n {
                1 => Some(true),
                0 => Some(false),
                _ => None,
            }),
        // Pydantic does not trim a bool string: " yes " is an error.
        Value::String(text) => parse_bool(text).ok(),
        _ => None,
    }
}

fn as_int(value: &Value) -> Option<i64> {
    match value {
        Value::Number(number) => number.as_i64().or_else(|| {
            number.as_f64().and_then(|n| {
                (n.fract() == 0.0 && n >= i64::MIN as f64 && n <= i64::MAX as f64)
                    .then_some(n as i64)
            })
        }),
        Value::String(text) => int_from_str(text),
        Value::Bool(flag) => Some(i64::from(*flag)),
        _ => None,
    }
}

fn int_from_str(raw: &str) -> Option<i64> {
    // Pydantic accepts surrounding whitespace, a single underscore between
    // digits (`1_000`) and a fractional part that is all zeros (`5.0`,
    // `1_000.0`). It rejects scientific notation, a leading or doubled
    // underscore, a non-zero fraction and a trailing dot (`1e3`, `_1000`,
    // `1__000`, `5.5`, `5.`).
    let trimmed = raw.trim();
    let mut cleaned = String::new();
    let chars: Vec<char> = trimmed.chars().collect();
    for (index, ch) in chars.iter().enumerate() {
        if *ch == '_' {
            let before = chars[..index].last().is_some_and(|c| c.is_ascii_digit());
            let after = chars.get(index + 1).is_some_and(|c| c.is_ascii_digit());
            if before && after {
                continue;
            }
            return None;
        }
        cleaned.push(*ch);
    }
    if let Some((whole, fraction)) = cleaned.split_once('.') {
        if fraction.is_empty() || !fraction.chars().all(|c| c == '0') {
            return None;
        }
        cleaned = whole.to_string();
    }
    cleaned.parse::<i64>().ok()
}

fn config_from_map(values: &Map<String, Value>) -> Result<VogtConfig, String> {
    for key in values.keys() {
        if !FIELD_NAMES.contains(&key.as_str()) {
            return Err(format!("unknown setting {key:?}"));
        }
    }
    let mut config = VogtConfig::default();
    for field in FIELD_CATALOGUE {
        let Some(value) = values.get(field.name) else {
            continue;
        };
        apply_field(&mut config, field, value)?;
    }
    validate(&config)?;
    Ok(config)
}

fn apply_field(config: &mut VogtConfig, field: &FieldDoc, value: &Value) -> Result<(), String> {
    let name = field.name;
    match field.kind {
        Kind::Path => set_path(name, value, |path| config.data_dir = path)?,
        Kind::OptPath => {
            let path = opt_path(name, value)?;
            match name {
                "import_root" => config.import_root = path,
                "github_token_file" => config.github_token_file = path,
                "forge_account_key_file" => config.forge_account_key_file = path,
                "engine_state_dir" => config.engine_state_dir = path,
                "engine_token_file" => config.engine_token_file = path,
                "bootstrap_core_token_file" => config.bootstrap_core_token_file = path,
                "bootstrap_agent_token_file" => config.bootstrap_agent_token_file = path,
                "diagnostics_peer_token_file" => config.diagnostics_peer_token_file = path,
                _ => unreachable!(),
            }
        }
        Kind::OptString => {
            let text = opt_string(name, value)?;
            match name {
                "public_url" => config.public_url = text,
                "engine_url" => config.engine_url = text,
                "session_scratch_project" => config.session_scratch_project = text,
                "image_digest" => config.image_digest = text,
                "diagnostics_peer_url" => config.diagnostics_peer_url = text,
                _ => unreachable!(),
            }
        }
        Kind::Bool => {
            let flag = as_bool(value).ok_or_else(|| format!("{name} must be a boolean"))?;
            match name {
                "fronted" => config.fronted = flag,
                "log_requests" => config.log_requests = flag,
                "ci_watch_notify_sessions" => config.ci_watch_notify_sessions = flag,
                "install_bootstrap_enabled" => config.install_bootstrap_enabled = flag,
                _ => unreachable!(),
            }
        }
        Kind::LogLevel => {
            config.log_level = LogLevel::parse(expect_str(name, value)?)?;
        }
        Kind::LogFormat => {
            config.log_format = LogFormat::parse(expect_str(name, value)?)?;
        }
        Kind::Sqlite => {
            config.sqlite_synchronous = SqliteSynchronous::parse(expect_str(name, value)?)?;
        }
        Kind::Int { min, max } => {
            let number = expect_int(name, value, min, max)?;
            match name {
                "log_slow_request_ms" => config.log_slow_request_ms = number,
                "retention_days" => config.retention_days = number,
                "agent_activity_max_bytes_per_sweep" => {
                    config.agent_activity_max_bytes_per_sweep = number
                }
                "session_ttl_days" => config.session_ttl_days = number,
                "sweep_interval_seconds" => config.sweep_interval_seconds = number,
                "verify_horizon_hours" => config.verify_horizon_hours = number,
                _ => unreachable!(),
            }
        }
        Kind::String => {
            let text = expect_str(name, value)?.to_string();
            match name {
                "contract_version" => config.contract_version = text,
                "branch_binding_template" => config.branch_binding_template = text,
                "bootstrap_core_token_actor" => config.bootstrap_core_token_actor = text,
                "bootstrap_core_token_scopes" => config.bootstrap_core_token_scopes = text,
                "agent_session_scopes" => config.agent_session_scopes = text,
                "bootstrap_agent_token_actor" => config.bootstrap_agent_token_actor = text,
                _ => unreachable!(),
            }
        }
        Kind::StringList => {
            let list = expect_string_list(name, value)?;
            match name {
                "log_quiet_paths" => config.log_quiet_paths = list,
                "contract_required_files" => config.contract_required_files = list,
                "contract_required_dirs" => config.contract_required_dirs = list,
                "contract_required_meta" => config.contract_required_meta = list,
                "marker_promotion_patterns" => config.marker_promotion_patterns = list,
                "inbox_bot_logins" => config.inbox_bot_logins = list,
                "ci_alert_branches" => config.ci_alert_branches = list,
                "ci_alert_tags" => config.ci_alert_tags = list,
                "marker_file_extensions" => config.marker_file_extensions = list,
                "branch_binding_patterns" => config.branch_binding_patterns = list,
                _ => unreachable!(),
            }
        }
        Kind::DeployLanes => config.deploy_lanes = expect_lanes(value)?,
        Kind::PathMap => config.forge_token_files = expect_path_map(name, value)?,
        Kind::ActivityRoots => config.agent_activity_roots = expect_path_map(name, value)?,
        Kind::SessionRoots => config.session_transcript_roots = expect_path_map(name, value)?,
        Kind::StringMap => config.agent_activity_services = expect_string_map(name, value)?,
    }
    Ok(())
}

fn set_path(name: &str, value: &Value, slot: impl FnOnce(PathBuf)) -> Result<(), String> {
    slot(expect_path(name, value)?);
    Ok(())
}

fn expect_str<'a>(name: &str, value: &'a Value) -> Result<&'a str, String> {
    value
        .as_str()
        .ok_or_else(|| format!("{name} must be a string"))
}

fn expect_path(name: &str, value: &Value) -> Result<PathBuf, String> {
    Ok(PathBuf::from(expect_str(name, value)?))
}

fn opt_string(name: &str, value: &Value) -> Result<Option<String>, String> {
    match value {
        Value::Null => Ok(None),
        Value::String(text) => Ok(Some(text.clone())),
        _ => Err(format!("{name} must be a string")),
    }
}

fn opt_path(name: &str, value: &Value) -> Result<Option<PathBuf>, String> {
    Ok(opt_string(name, value)?.map(PathBuf::from))
}

fn expect_int(name: &str, value: &Value, min: i64, max: Option<i64>) -> Result<i64, String> {
    let number = as_int(value).ok_or_else(|| format!("{name} must be an integer"))?;
    if number < min {
        return Err(format!("{name} must be >= {min}"));
    }
    if let Some(max) = max {
        if number > max {
            return Err(format!("{name} must be <= {max}"));
        }
    }
    Ok(number)
}

fn expect_string_list(name: &str, value: &Value) -> Result<Vec<String>, String> {
    let items = value
        .as_array()
        .ok_or_else(|| format!("{name} must be a list"))?;
    items
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_string)
                .ok_or_else(|| format!("{name} entries must be strings"))
        })
        .collect()
}

fn expect_lanes(value: &Value) -> Result<Vec<DeployLane>, String> {
    let items = value
        .as_array()
        .ok_or_else(|| "deploy_lanes must be a list".to_string())?;
    items.iter().map(DeployLane::from_value).collect()
}

fn expect_path_map(name: &str, value: &Value) -> Result<BTreeMap<String, PathBuf>, String> {
    let obj = value
        .as_object()
        .ok_or_else(|| format!("{name} must be a table"))?;
    let mut map = BTreeMap::new();
    for (key, entry) in obj {
        let path = entry
            .as_str()
            .ok_or_else(|| format!("{name}[{key:?}] must be a path"))?;
        map.insert(key.clone(), PathBuf::from(path));
    }
    Ok(map)
}

fn expect_string_map(name: &str, value: &Value) -> Result<BTreeMap<String, String>, String> {
    let obj = value
        .as_object()
        .ok_or_else(|| format!("{name} must be a table"))?;
    let mut map = BTreeMap::new();
    for (key, entry) in obj {
        let text = entry
            .as_str()
            .ok_or_else(|| format!("{name}[{key:?}] must be a string"))?;
        map.insert(key.clone(), text.to_string());
    }
    Ok(map)
}

fn validate(config: &VogtConfig) -> Result<(), String> {
    let unknown: Vec<_> = config
        .agent_activity_roots
        .keys()
        .filter(|key| *key != "claude" && *key != "codex")
        .cloned()
        .collect();
    if !unknown.is_empty() {
        let listed = unknown
            .iter()
            .map(|key| format!("{key:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "transcript root keys must be 'claude' or 'codex', not {listed}"
        ));
    }
    let unknown: Vec<_> = config
        .session_transcript_roots
        .keys()
        .filter(|key| *key != "claude" && *key != "codex" && *key != "klaudia")
        .cloned()
        .collect();
    if !unknown.is_empty() {
        let listed = unknown
            .iter()
            .map(|key| format!("{key:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "session transcript root keys must be 'claude', 'codex' or 'klaudia', not {listed}"
        ));
    }
    for (name, pattern) in &config.agent_activity_services {
        if fancy_regex::Regex::new(pattern).is_err() {
            return Err(format!(
                "agent_activity_services[{name:?}] is not a valid regex"
            ));
        }
    }
    Ok(())
}

// -- documentation -----------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Path,
    OptPath,
    OptString,
    Bool,
    LogLevel,
    LogFormat,
    Sqlite,
    Int { min: i64, max: Option<i64> },
    String,
    StringList,
    DeployLanes,
    PathMap,
    ActivityRoots,
    SessionRoots,
    StringMap,
}

/// One documented configuration field, as the generators see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldDoc {
    pub name: &'static str,
    pub env_var: &'static str,
    pub type_label: &'static str,
    pub default_label: &'static str,
    pub policy: DefaultPolicy,
    pub description: &'static str,
    kind: Kind,
    example: &'static str,
}

pub fn describe_fields() -> Vec<FieldDoc> {
    FIELD_CATALOGUE.to_vec()
}

const FIELD_CATALOGUE: &[FieldDoc] = &[
    FieldDoc {
        name: "data_dir",
        env_var: "VOGT_DATA_DIR",
        type_label: "path",
        default_label: "`$XDG_DATA_HOME/vogt`, else `~/.local/share/vogt`",
        policy: DefaultPolicy::Allocation,
        description: "Directory holding declared.sqlite3, observed.sqlite3 and backups. One instance per directory.",
        kind: Kind::Path,
        example: "\"/var/lib/vogt\"",
    },
    FieldDoc {
        name: "import_root",
        env_var: "VOGT_IMPORT_ROOT",
        type_label: "path, optional",
        default_label: "`<data_dir>/repos`",
        policy: DefaultPolicy::Allocation,
        description: "Directory imported repositories are cloned into, one per project slug. An allocation value, so it has a default rather than a gate: unset means `<data_dir>/repos`, which keeps the clone with the instance that registered it. Deployments that observe a workspace on a mounted host directory point this at that workspace instead, so an imported project lands where the rest of the work already lives.",
        kind: Kind::OptPath,
        example: "\"/var/lib/vogt/repos\"",
    },
    FieldDoc {
        name: "public_url",
        env_var: "VOGT_PUBLIC_URL",
        type_label: "string, optional",
        default_label: "*(no default — must be set)*",
        policy: DefaultPolicy::Exposure,
        description: "The URL clients reach this instance at, e.g. `https://vogt.example.com`. An exposure value, so it has no default and is never guessed: the process binds `0.0.0.0:8000` inside a container and is published somewhere else entirely, so the server cannot know its own address — only the operator does. Unset means `connect` reports that nobody has said, which is a different answer from reporting a URL that does not work.",
        kind: Kind::OptString,
        example: "\"https://vogt.example.com\"",
    },
    FieldDoc {
        name: "fronted",
        env_var: "VOGT_FRONTED",
        type_label: "boolean",
        default_label: "`False`",
        policy: DefaultPolicy::Behaviour,
        description: "Whether this instance runs behind an optional front door that publishes it at a different address and mount points. When true, `connect` and `/connection-info` render against the identity the door states per request (`X-Vogt-Public-Url`, `X-Vogt-Api-Path`, `X-Vogt-Mcp-Path`), because the door is the only thing that knows where clients arrive. Off by default and never inferred: an instance that has not been told it is fronted ignores those headers entirely, so nobody who can reach it can make `connect` render a client configuration — a document meant to be pasted next to a token — against an address they chose.",
        kind: Kind::Bool,
        example: "false",
    },
    FieldDoc {
        name: "log_level",
        env_var: "VOGT_LOG_LEVEL",
        type_label: "one of `debug`, `info`, `warning`, `error`",
        default_label: "`info`",
        policy: DefaultPolicy::Behaviour,
        description: "Verbosity of Vogt's own diagnostics — the `vogt.*` logger namespace, not its dependencies'.",
        kind: Kind::LogLevel,
        example: "\"info\"",
    },
    FieldDoc {
        name: "log_format",
        env_var: "VOGT_LOG_FORMAT",
        type_label: "one of `text`, `json`",
        default_label: "`text`",
        policy: DefaultPolicy::Behaviour,
        description: "How each line is rendered. `text` is for a person reading `docker logs`; `json` is one object per line for a log that is queried rather than read (Loki). The fields are the same either way, so a query written against one describes the other.",
        kind: Kind::LogFormat,
        example: "\"text\"",
    },
    FieldDoc {
        name: "log_requests",
        env_var: "VOGT_LOG_REQUESTS",
        type_label: "boolean",
        default_label: "`True`",
        policy: DefaultPolicy::Behaviour,
        description: "Whether every served request produces an access line carrying its duration. On by default: stock uvicorn access lines carry no timing at all, so a single slow endpoint would leave no trace. Turning this off still leaves correlation ids honoured and echoed.",
        kind: Kind::Bool,
        example: "true",
    },
    FieldDoc {
        name: "log_slow_request_ms",
        env_var: "VOGT_LOG_SLOW_REQUEST_MS",
        type_label: "integer",
        default_label: "`1000`",
        policy: DefaultPolicy::Behaviour,
        description: "A request whose response takes longer than this to *start* is logged at WARNING rather than INFO. Judged on time to first byte, so a long-lived `/mcp` stream is not reported as a pathological request every time it ends.",
        kind: Kind::Int { min: 0, max: None },
        example: "1000",
    },
    FieldDoc {
        name: "log_quiet_paths",
        env_var: "VOGT_LOG_QUIET_PATHS",
        type_label: "list of strings",
        default_label: "`/health/live`, `/health/ready`, `/version`",
        policy: DefaultPolicy::Behaviour,
        description: "Paths whose access lines drop to DEBUG. Probes, by default: an orchestrator calls them every few seconds forever, and a log that is 100% `/healthz` with no application output in it is not a log. Suppressed and not dropped — a probe that crosses `log_slow_request_ms` still warns.",
        kind: Kind::StringList,
        example: "[\"/health/live\", \"/health/ready\", \"/version\"]",
    },
    FieldDoc {
        name: "contract_required_files",
        env_var: "VOGT_CONTRACT_REQUIRED_FILES",
        type_label: "list of strings",
        default_label: "`AGENTS.md`, `README.md`, `LICENSE`",
        policy: DefaultPolicy::Behaviour,
        description: "Files a compliant project must contain. The contract is a value you read, never a barrier you pass: changing this changes what `contract check` reports and gates nothing.",
        kind: Kind::StringList,
        example: "[\"AGENTS.md\", \"README.md\", \"LICENSE\"]",
    },
    FieldDoc {
        name: "contract_required_dirs",
        env_var: "VOGT_CONTRACT_REQUIRED_DIRS",
        type_label: "list of strings",
        default_label: "`docs`, `design`, `src`",
        policy: DefaultPolicy::Behaviour,
        description: "Directories a compliant project must contain.",
        kind: Kind::StringList,
        example: "[\"docs\", \"design\", \"src\"]",
    },
    FieldDoc {
        name: "contract_required_meta",
        env_var: "VOGT_CONTRACT_REQUIRED_META",
        type_label: "list of strings",
        default_label: "`name`, `lifecycle_state`, `owner`",
        policy: DefaultPolicy::Behaviour,
        description: "Metadata keys a compliant project must declare. Read from the project's own manifest, not from Vogt's registration.",
        kind: Kind::StringList,
        example: "[\"name\", \"lifecycle_state\", \"owner\"]",
    },
    FieldDoc {
        name: "contract_version",
        env_var: "VOGT_CONTRACT_VERSION",
        type_label: "string",
        default_label: "`v1`",
        policy: DefaultPolicy::Behaviour,
        description: "Names which contract a recorded compliance status was evaluated against. If the rules above differ from the built-in defaults and this is left at its own default, Vogt appends a short digest of the rules — a status must never claim to be the stock `v1` when it is not, and an operator who edits the rules should not have to remember to rename them.",
        kind: Kind::String,
        example: "\"v1\"",
    },
    FieldDoc {
        name: "marker_promotion_patterns",
        env_var: "VOGT_MARKER_PROMOTION_PATTERNS",
        type_label: "list of strings",
        default_label: "`TODO(vogt)`, `FIXME(vogt)`",
        policy: DefaultPolicy::Behaviour,
        description: "Source markers containing one of these enter backlog and bug views. Every other marker is still observed, still queryable and still counted; it just does not claim to be work. Widening this is how you drown the ranked view.",
        kind: Kind::StringList,
        example: "[\"TODO(vogt)\", \"FIXME(vogt)\"]",
    },
    FieldDoc {
        name: "inbox_bot_logins",
        env_var: "VOGT_INBOX_BOT_LOGINS",
        type_label: "list of strings",
        default_label: "`dependabot`, `renovate`, `renovate-bot`, `github-actions`",
        policy: DefaultPolicy::Behaviour,
        description: "Forge logins the Inbox treats as bots even when the forge does not mark them as one. Matched case-insensitively, with or without a `[bot]` suffix. An account the forge reports as a `Bot`, or whose login ends in `[bot]`, is a bot regardless. Bots are never 'external', so this list is what keeps automation out of the Inbox's *External people only* filter. Applied when the Inbox is read, so a change here needs no re-sweep.",
        kind: Kind::StringList,
        example: "[\"dependabot\", \"renovate\", \"renovate-bot\", \"github-actions\"]",
    },
    FieldDoc {
        name: "ci_alert_branches",
        env_var: "VOGT_CI_ALERT_BRANCHES",
        type_label: "list of strings",
        default_label: "`main`, `master`, `prod`",
        policy: DefaultPolicy::Behaviour,
        description: "Branches (glob patterns) whose failed CI runs raise an Inbox alert. Only pushed, scheduled and manually dispatched runs count — a pull-request run never alerts, whatever its branch. An alert names the failing jobs, links the log, and clears itself when a later run of the same workflow on the same branch succeeds.",
        kind: Kind::StringList,
        example: "[\"main\", \"master\", \"prod\"]",
    },
    FieldDoc {
        name: "ci_alert_tags",
        env_var: "VOGT_CI_ALERT_TAGS",
        type_label: "list of strings",
        default_label: "`v*`",
        policy: DefaultPolicy::Behaviour,
        description: "Tags (glob patterns) whose failed CI runs raise an Inbox alert — the release workflows a tag push starts, which block no pull request and so fail silently otherwise. Tags are one lane per workflow: a later tag's successful run of the same workflow clears an earlier tag's failure, because the release it would have shipped has been superseded.",
        kind: Kind::StringList,
        example: "[\"v*\"]",
    },
    FieldDoc {
        name: "ci_watch_notify_sessions",
        env_var: "VOGT_CI_WATCH_NOTIFY_SESSIONS",
        type_label: "boolean",
        default_label: "`True`",
        policy: DefaultPolicy::Behaviour,
        description: "When CI finishes on a branch bound to a work item (`work.bind_branch`), type a one-line pass/fail notice into each live session started for that item, so an agent wakes on the result instead of polling. Sent once per run conclusion, by the sweep that observed it. The Inbox entry is raised either way; this only turns the session nudge off.",
        kind: Kind::Bool,
        example: "true",
    },
    FieldDoc {
        name: "deploy_lanes",
        env_var: "VOGT_DEPLOY_LANES",
        type_label: "list of `DeployLane` tables",
        default_label: "*(empty)*",
        policy: DefaultPolicy::Behaviour,
        description: "Deployment lanes `deployed_versions` reports on, each a table with `name`, `project` (a registered slug), `branch` (default `main`), and a source: `receipt_repo` + `receipt_path` (a JSON receipt a deploy pipeline commits, read through the configured forge) and/or `version_url` (a running instance's public JSON version endpoint, e.g. Vogt's `/api/config`). Empty means the `deploy-lanes` collector is not registered and deployed versions are reported as not configured. A receipt whose status says the deploy or its smoke test failed raises an Inbox alert.",
        kind: Kind::DeployLanes,
        example: "[{ name = \"dev\", project = \"my-app\", version_url = \"https://dev.example.com/api/config\" }]",
    },
    FieldDoc {
        name: "marker_file_extensions",
        env_var: "VOGT_MARKER_FILE_EXTENSIONS",
        type_label: "list of strings",
        default_label: "`.py`, `.rs`, `.ts`, `.tsx`, `.js`, `.jsx`, `.go`, `.java`, `.rb`, `.sh`, `.sql`, `.toml`, `.yaml`, `.yml`, `.md`",
        policy: DefaultPolicy::Behaviour,
        description: "File types the marker collector reads. Configuration rather than a hard-coded list, because which extensions hold source is a workspace's business, not Vogt's.",
        kind: Kind::StringList,
        example: "[\".py\", \".rs\", \".ts\", \".tsx\", \".js\", \".jsx\", \".go\", \".java\", \".rb\", \".sh\", \".sql\", \".toml\", \".yaml\", \".yml\", \".md\"]",
    },
    FieldDoc {
        name: "branch_binding_patterns",
        env_var: "VOGT_BRANCH_BINDING_PATTERNS",
        type_label: "list of strings",
        default_label: "`(?i)\\bwi-?(?P<n>\\d+)\\b`, `(?i)\\bgh-(?P<forge>\\d+)\\b`",
        policy: DefaultPolicy::Behaviour,
        description: "Regular expressions that recognise which work item a git branch belongs to. A branch whose name matches one is that item's branch: the default set reads `wi-7/…` and `feature/WI-7-…` (the vogt work-item number, via a `n` capture group) and `gh-264-…` (a linked project's forge issue number, via a `forge` group). The `git-local` collector applies these to every branch it observes and records the match; widening the set is how a team teaches Vogt its own branch conventions. Reported, never enforced: matching a branch never creates, renames or deletes one.",
        kind: Kind::StringList,
        example: "[\"(?i)\\\\bwi-?(?P<n>\\\\d+)\\\\b\", \"(?i)\\\\bgh-(?P<forge>\\\\d+)\\\\b\"]",
    },
    FieldDoc {
        name: "branch_binding_template",
        env_var: "VOGT_BRANCH_BINDING_TEMPLATE",
        type_label: "string",
        default_label: "`wi-{number}`",
        policy: DefaultPolicy::Behaviour,
        description: "The branch a session started from Vogt for a work item declares it will use, formatted with a single `{number}` field: `WI-7` becomes `wi-7`. This is the *declared* half of the branch binding, recorded on the item's overlay and kept separate from what a sweep observes — an upstream item always declares the forge form `gh-<number>` regardless of this template, because that is the shape its own default project recognises.",
        kind: Kind::String,
        example: "\"wi-{number}\"",
    },
    FieldDoc {
        name: "retention_days",
        env_var: "VOGT_RETENTION_DAYS",
        type_label: "integer",
        default_label: "`180`",
        policy: DefaultPolicy::Behaviour,
        description: "How long observation *history* is kept. The newest observation per subject is kept indefinitely regardless, and so is anything a drift proposal references.",
        kind: Kind::Int { min: 1, max: None },
        example: "180",
    },
    FieldDoc {
        name: "github_token_file",
        env_var: "VOGT_GITHUB_TOKEN_FILE",
        type_label: "path, optional",
        default_label: "*(no default — must be set)*",
        policy: DefaultPolicy::Behaviour,
        description: "Path to a file containing a GitHub token. Its absence is what switches the optional forge adapter off, so there is no default: not configured is the ordinary case, and it means forge subjects are 'not collected' rather than absent. A file rather than an environment variable or an argument, so the token never appears in a process listing.",
        kind: Kind::OptPath,
        example: "null",
    },
    FieldDoc {
        name: "forge_account_key_file",
        env_var: "VOGT_FORGE_ACCOUNT_KEY_FILE",
        type_label: "path, optional",
        default_label: "*(no default — must be set)*",
        policy: DefaultPolicy::Behaviour,
        description: "Path to a file holding a urlsafe-base64 Fernet key, used to encrypt per-actor forge Personal Access Tokens at rest. A linked PAT must be *recoverable* to call the upstream API on the actor's behalf, which is the deliberate opposite of the vogt-issued tokens in `0005_tokens` — those store only a hash because they never need recovery. Its absence is what switches account linking off: with no key there is nowhere safe to keep a token, so the feature is disabled rather than insecure, and the file-token fallback (`github_token_file`) keeps working for sweeps and unlinked actors. A file rather than an environment variable, so the key never appears in a process listing.",
        kind: Kind::OptPath,
        example: "null",
    },
    FieldDoc {
        name: "forge_token_files",
        env_var: "VOGT_FORGE_TOKEN_FILES",
        type_label: "map of string to path",
        default_label: "*(empty)*",
        policy: DefaultPolicy::Behaviour,
        description: "Per-host forge token files, mapping a forge host to a file holding a token for it — a TOML table `[forge_token_files]` with, e.g., `\"github.com\" = \"/run/secrets/github_token\"`. This is the general form of `github_token_file`, which stays as the alias for github.com; a host set here wins over the alias. A host absent from this map has no provider registered, which is what keeps its subjects 'not collected' rather than reported as absent — the same honesty rule the single-token field has always followed. Files rather than environment variables or arguments, so a token never appears in a process listing.",
        kind: Kind::PathMap,
        example: "{ \"github.com\" = \"/run/secrets/github_token\" }",
    },
    FieldDoc {
        name: "agent_activity_roots",
        env_var: "VOGT_AGENT_ACTIVITY_ROOTS",
        type_label: "map of string to path",
        default_label: "*(empty)*",
        policy: DefaultPolicy::Behaviour,
        description: "Agent transcript directories to index, by format — a TOML table `[agent_activity_roots]` with `claude = \"~/.claude/projects\"` and/or `codex = \"~/.codex/sessions\"`. Empty, the `agent-activity` collector is not registered and nothing is read: transcripts hold credentials, so indexing them is opt-in. Configured, each sweep reads new transcript lines incrementally into the observed store's activity index, redacting before anything is kept; search it with `agent_activity.search` and `.summary`.",
        kind: Kind::ActivityRoots,
        example: "{ claude = \"~/.claude/projects\", codex = \"~/.codex/sessions\" }",
    },
    FieldDoc {
        name: "session_transcript_roots",
        env_var: "VOGT_SESSION_TRANSCRIPT_ROOTS",
        type_label: "map of string to path",
        default_label: "`claude = \"~/.claude/projects\"`, `codex = \"~/.codex/sessions\"`, `klaudia = \"~/.klaudia/sessions\"`",
        policy: DefaultPolicy::Behaviour,
        description: "Where `session.last_reply` and the `last_reply_excerpt` in `session.list` look for a session's own agent transcript, by agent (`claude`, `codex`, `klaudia`). Read on request only — the last few assistant messages of the conversation a session runs, redacted — never indexed. The defaults are the agents' own directories under the home of the user the core runs as, which in the all-in-one stack is the user its sessions run as; a missing directory makes the reply honestly unavailable. An empty table turns both off.",
        kind: Kind::SessionRoots,
        example: "{ claude = \"~/.claude/projects\", codex = \"~/.codex/sessions\", klaudia = \"~/.klaudia/sessions\" }",
    },
    FieldDoc {
        name: "agent_activity_max_bytes_per_sweep",
        env_var: "VOGT_AGENT_ACTIVITY_MAX_BYTES_PER_SWEEP",
        type_label: "integer",
        default_label: "`33554432`",
        policy: DefaultPolicy::Behaviour,
        description: "The most transcript bytes one sweep reads, newest files first. A first sweep over months of transcripts catches up over several sweeps rather than holding the schedule; the backlog still waiting is in the sweep's stats.",
        kind: Kind::Int { min: 4096, max: None },
        example: "33554432",
    },
    FieldDoc {
        name: "agent_activity_services",
        env_var: "VOGT_AGENT_ACTIVITY_SERVICES",
        type_label: "map of string to string",
        default_label: "*(empty)*",
        policy: DefaultPolicy::Behaviour,
        description: "Extra service tags for the agent activity index, mapping a tag to a case-insensitive regular expression over a tool call's name and input — e.g. `ci = 'ci\\.example\\.org'`. Merged over the built-in table (github, docker, komodo, infisical, …); an empty pattern removes a built-in tag. Applies to calls indexed after the change.",
        kind: Kind::StringMap,
        example: "{ ci = 'ci\\.example\\.org' }",
    },
    FieldDoc {
        name: "engine_url",
        env_var: "VOGT_ENGINE_URL",
        type_label: "string, optional",
        default_label: "*(no default — must be set)*",
        policy: DefaultPolicy::Exposure,
        description: "Where the session engine listens, e.g. `http://127.0.0.1:8910`. The engine is the other half of the merged product: it owns the PTYs a work item's session runs in. Unset means the `session.*` operations report that no engine is configured — absence of the engine costs sessions and nothing else. An exposure value, so it is never guessed: co-located today, its address is still the operator's to state.",
        kind: Kind::OptString,
        example: "null",
    },
    FieldDoc {
        name: "session_scratch_project",
        env_var: "VOGT_SESSION_SCRATCH_PROJECT",
        type_label: "string, optional",
        default_label: "*(no default — must be set)*",
        policy: DefaultPolicy::Behaviour,
        description: "The project slug a session with no work item and no project resolves to. It exists for the spoken request that has no subject — 'research the best risotto in Wollongong' — which still needs a registered working tree to open in. Unset means such a request is refused by name rather than opened somewhere guessed: the working directory comes from the registry, and a scratch project is a registered project like any other.",
        kind: Kind::OptString,
        example: "null",
    },
    FieldDoc {
        name: "engine_state_dir",
        env_var: "VOGT_ENGINE_STATE_DIR",
        type_label: "path, optional",
        default_label: "*(no default — must be set)*",
        policy: DefaultPolicy::Behaviour,
        description: "The session engine's state directory, when this process can read it — the merged deployment runs both halves in one container, so it usually can. `backup` copies it and `restore` puts it back, because half a restore is the failure to prevent: the work items come back and the terminals' history, push subscriptions and agent tasks do not. Its absence is what makes a backup a core-only backup, so there is no default — the same reason `github_token_file` has none. Unset, the manifest says 'not configured' rather than quietly covering less than it appears to.",
        kind: Kind::OptPath,
        example: "null",
    },
    FieldDoc {
        name: "engine_token_file",
        env_var: "VOGT_ENGINE_TOKEN_FILE",
        type_label: "path, optional",
        default_label: "*(no default — must be set)*",
        policy: DefaultPolicy::Behaviour,
        description: "Path to a file containing the credential Vogt calls the engine with. Unset, `bootstrap_core_token_file` is used: the one stack secret both halves share is recognised by the engine as the core's own identity, so a deployment needs no second token for this direction. Set it only to give the core a distinct engine credential. A file rather than an environment variable, for the same reason as `github_token_file` — a token in the environment is a token in every `docker inspect`.",
        kind: Kind::OptPath,
        example: "null",
    },
    FieldDoc {
        name: "bootstrap_core_token_file",
        env_var: "VOGT_BOOTSTRAP_CORE_TOKEN_FILE",
        type_label: "path, optional",
        default_label: "*(no default — must be set)*",
        policy: DefaultPolicy::Behaviour,
        description: "Path to a file holding the core token the front door will present, adopted at `init` if no token with that secret exists yet. This is what lets a fronted deployment come up in one deploy: without it the token can only be minted by a running core, so the front door's `/api/vogt` answers 401 until somebody execs in, mints one, edits the configuration and deploys again. Supplying it is the same act as choosing `engine_token_file`'s value — the mirror of this token, which has always been operator-chosen. Idempotent: a boot that finds the secret already present changes nothing. Unset, the mint-then-configure path is unchanged.",
        kind: Kind::OptPath,
        example: "null",
    },
    FieldDoc {
        name: "bootstrap_core_token_actor",
        env_var: "VOGT_BOOTSTRAP_CORE_TOKEN_ACTOR",
        type_label: "string",
        default_label: "`agent:vogt-engine`",
        policy: DefaultPolicy::Behaviour,
        description: "Identity the adopted core token is bound to, created if absent. Audit rows name it, so it should say which front door acted — not a person, and not something shared with another instance.",
        kind: Kind::String,
        example: "\"agent:vogt-engine\"",
    },
    FieldDoc {
        name: "bootstrap_core_token_scopes",
        env_var: "VOGT_BOOTSTRAP_CORE_TOKEN_SCOPES",
        type_label: "string",
        default_label: "`read,work.write,project.write`",
        policy: DefaultPolicy::Behaviour,
        description: "Scopes for the adopted core token. Everything in the pod runs as one uid and can read the token file, so this scope *is* that pod's blast radius — narrow it to what the front door actually needs. `admin` is deliberately not the default.",
        kind: Kind::String,
        example: "\"read,work.write,project.write\"",
    },
    FieldDoc {
        name: "agent_session_scopes",
        env_var: "VOGT_AGENT_SESSION_SCOPES",
        type_label: "string",
        default_label: "`read,work.write,project.write,writeback`",
        policy: DefaultPolicy::Behaviour,
        description: "Scopes every agent session's own token holds — one deployment decision for what a session may do, applied the same however the session was launched. Comma-separated, parsed with the same rules as any token's scopes. The default is everything except `admin`, which gates only token minting, actor creation and instance ops (init/migrate/backup/restore/serve) that no session needs. Scopes are instance-wide and everything in the pod shares one uid, so a narrower set here is more a rule to explain than a boundary it enforces (FR-S10) — narrow it only if this instance truly wants to. `admin` is accepted if an operator writes it: that is consent, not a mistake to guard against. Per-session attribution is unchanged — each session still mints its own actor-bound token; only the scope set is shared.",
        kind: Kind::String,
        example: "\"read,work.write,project.write,writeback\"",
    },
    FieldDoc {
        name: "bootstrap_agent_token_file",
        env_var: "VOGT_BOOTSTRAP_AGENT_TOKEN_FILE",
        type_label: "path, optional",
        default_label: "*(no default — must be set)*",
        policy: DefaultPolicy::Behaviour,
        description: "Path to a file holding the brokered agent token adopted at `init` — the session-side mirror of `bootstrap_core_token_file` (#199). It lets the pod-wide token an engine default shell or a command-launched session brokers be a deploy-time secret instead of something minted by a running core (an `admin` op plus an Infisical rotation plus an engine restart). Its scopes are `agent_session_scopes` — the same knob as `session.start` — so widening what sessions may do is 'change the secret, redeploy'. Idempotent: a boot that finds the secret already present changes nothing. Unset, the mint-then-configure path is unchanged.",
        kind: Kind::OptPath,
        example: "null",
    },
    FieldDoc {
        name: "bootstrap_agent_token_actor",
        env_var: "VOGT_BOOTSTRAP_AGENT_TOKEN_ACTOR",
        type_label: "string",
        default_label: "`agent:vogt-sessions`",
        policy: DefaultPolicy::Behaviour,
        description: "Identity the adopted agent token is bound to, created if absent. Audit rows name it, so it should say this stack's sessions acted — not a person, and not something shared with another instance. It must be an agent: every session's agent presents this token, and bound to a person it would count as that person (granting `bypass`, writing as them), so an existing person here fails start-up (WI-926).",
        kind: Kind::String,
        example: "\"agent:vogt-sessions\"",
    },
    FieldDoc {
        name: "session_ttl_days",
        env_var: "VOGT_SESSION_TTL_DAYS",
        type_label: "integer",
        default_label: "`30`",
        policy: DefaultPolicy::Behaviour,
        description: "How long a session minted by a password login (`auth.login`) stays valid without being used. The expiry slides: once less than half of it is left, an authenticated request extends it to this many days from then, so a device in regular use stays signed in. A session is a token like any other — revocable by `auth.logout` or `token.revoke` — so a browser or phone that is lost, or simply left unused for this long, stops working on its own.",
        kind: Kind::Int { min: 1, max: Some(3650) },
        example: "30",
    },
    FieldDoc {
        name: "install_bootstrap_enabled",
        env_var: "VOGT_INSTALL_BOOTSTRAP_ENABLED",
        type_label: "boolean",
        default_label: "`True`",
        policy: DefaultPolicy::Behaviour,
        description: "Whether the unauthenticated first-run install bootstrap (`POST /api/install/bootstrap`) may mint the first admin token while no person holds a credential (agent-bound tokens such as the adopted stack secret do not count). It is safe on the loopback topology `serve` defaults to — only parties who could already mint a token over loopback reach it — but a fronted deployment proxies it through the public front door, so on every fresh deploy or store reset there is a window where any internet caller can take the instance. A deployment that creates its first operator another way (`vogt user create --scopes admin` in the container) never needs the HTTP bootstrap: set this `false` to refuse it outright and close that window. When disabled the status route reports the mode closed.",
        kind: Kind::Bool,
        example: "true",
    },
    FieldDoc {
        name: "sqlite_synchronous",
        env_var: "VOGT_SQLITE_SYNCHRONOUS",
        type_label: "one of `off`, `normal`, `full`, `extra`",
        default_label: "`normal`",
        policy: DefaultPolicy::Behaviour,
        description: "How hard SQLite works to survive a power cut. `normal` is the standard pairing for WAL and the default here; `full` fsyncs the write-ahead log on every commit, which on a contended disk costs tens of milliseconds per write. Under `normal` a power loss or OS crash can lose the last few committed transactions — the database is never corrupted, and an application crash loses nothing. Set `full` if you would rather pay that cost per write.",
        kind: Kind::Sqlite,
        example: "\"normal\"",
    },
    FieldDoc {
        name: "sweep_interval_seconds",
        env_var: "VOGT_SWEEP_INTERVAL_SECONDS",
        type_label: "integer",
        default_label: "`900`",
        policy: DefaultPolicy::Behaviour,
        description: "How often `serve` runs collectors in the background. Zero disables the schedule, leaving sweeps on-demand only. A default rather than a required value because an instance that never looks is the failure this product exists to prevent: stale evidence and no evidence are indistinguishable from the outside, so the safe default is to keep looking.",
        kind: Kind::Int { min: 0, max: None },
        example: "900",
    },
    FieldDoc {
        name: "verify_horizon_hours",
        env_var: "VOGT_VERIFY_HORIZON_HOURS",
        type_label: "integer",
        default_label: "`24`",
        policy: DefaultPolicy::Behaviour,
        description: "How recently a subject must have been observed for a linked declared entity to count as `verified` rather than `stale`. Trust is computed from this, never hand-set.",
        kind: Kind::Int { min: 1, max: None },
        example: "24",
    },
    FieldDoc {
        name: "image_digest",
        env_var: "VOGT_IMAGE_DIGEST",
        type_label: "string, optional",
        default_label: "*(no default — must be set)*",
        policy: DefaultPolicy::Behaviour,
        description: "The digest of the image this instance runs, as the deployment pinned it (e.g. `sha256:...`), reported by `instance.diagnostics`. A running container cannot see its own digest, so the deployment states it — typically by setting `VOGT_IMAGE_DIGEST` from the same value Compose pins. Unset is reported as unknown, never guessed.",
        kind: Kind::OptString,
        example: "null",
    },
    FieldDoc {
        name: "diagnostics_peer_url",
        env_var: "VOGT_DIAGNOSTICS_PEER_URL",
        type_label: "string, optional",
        default_label: "*(no default — must be set)*",
        policy: DefaultPolicy::Exposure,
        description: "The REST base of a peer Vogt instance (e.g. prod for a dev instance) that `instance.diagnostics --peer` asks for its diagnostics: the core's `/api` prefix, or a front door's `/api/vogt`, e.g. `https://vogt.example.com/api/vogt`. Lets an agent confirm a peer's deploy without tailnet or orchestrator access. An exposure value, so it is never guessed; unset means the peer half of the answer says it is not configured.",
        kind: Kind::OptString,
        example: "null",
    },
    FieldDoc {
        name: "diagnostics_peer_token_file",
        env_var: "VOGT_DIAGNOSTICS_PEER_TOKEN_FILE",
        type_label: "path, optional",
        default_label: "*(no default — must be set)*",
        policy: DefaultPolicy::Behaviour,
        description: "Path to a file holding a read-scoped token for `diagnostics_peer_url`. A file rather than an environment variable for the same reason as `github_token_file`. Unset sends no credential, which a peer that requires authentication refuses.",
        kind: Kind::OptPath,
        example: "null",
    },
];

const FIELD_NAMES: &[&str] = &[
    "data_dir",
    "import_root",
    "public_url",
    "fronted",
    "log_level",
    "log_format",
    "log_requests",
    "log_slow_request_ms",
    "log_quiet_paths",
    "contract_required_files",
    "contract_required_dirs",
    "contract_required_meta",
    "contract_version",
    "marker_promotion_patterns",
    "inbox_bot_logins",
    "ci_alert_branches",
    "ci_alert_tags",
    "ci_watch_notify_sessions",
    "deploy_lanes",
    "marker_file_extensions",
    "branch_binding_patterns",
    "branch_binding_template",
    "retention_days",
    "github_token_file",
    "forge_account_key_file",
    "forge_token_files",
    "agent_activity_roots",
    "session_transcript_roots",
    "agent_activity_max_bytes_per_sweep",
    "agent_activity_services",
    "engine_url",
    "session_scratch_project",
    "engine_state_dir",
    "engine_token_file",
    "bootstrap_core_token_file",
    "bootstrap_core_token_actor",
    "bootstrap_core_token_scopes",
    "agent_session_scopes",
    "bootstrap_agent_token_file",
    "bootstrap_agent_token_actor",
    "session_ttl_days",
    "install_bootstrap_enabled",
    "sqlite_synchronous",
    "sweep_interval_seconds",
    "verify_horizon_hours",
    "image_digest",
    "diagnostics_peer_url",
    "diagnostics_peer_token_file",
];

const GENERATED_BANNER: &str = "<!-- Generated by scripts/gen_config_docs.py from src/vogt/config.py. Do not edit by hand; CI fails on drift. -->";

pub fn render_config_reference() -> String {
    let fields = describe_fields();
    let mut lines = vec![
        GENERATED_BANNER.to_string(),
        String::new(),
        "# Vogt — Configuration Reference".to_string(),
        String::new(),
        "Every value below comes from `src/vogt/config.py`, which is the".to_string(),
        "single source of truth. Precedence, highest first:".to_string(),
        "explicit arguments, `VOGT_*` environment variables, the TOML file".to_string(),
        format!("named by `{CONFIG_FILE_ENV}`, then the defaults shown here."),
        String::new(),
        "## Settings".to_string(),
        String::new(),
        "| Setting | Env var | Type | Default | Default policy |".to_string(),
        "|---|---|---|---|---|".to_string(),
    ];
    for field in &fields {
        lines.push(format!(
            "| `{}` | `{}` | {} | {} | {} |",
            field.name, field.env_var, field.type_label, field.default_label, field.policy
        ));
    }
    lines.push(String::new());
    lines.push("## What each setting decides".to_string());
    lines.push(String::new());
    for field in &fields {
        lines.push(format!("### `{}`", field.name));
        lines.push(String::new());
        lines.push(field.description.to_string());
        lines.push(String::new());
    }
    lines.push("## Default policy".to_string());
    lines.push(String::new());
    lines.push("- **exposure** — hostnames, bind addresses, published ports, URLs a".to_string());
    lines.push("  client will trust. These never carry a default, in code, images,".to_string());
    lines.push("  docs or examples.".to_string());
    lines.push("- **allocation** — paths and slots on a host the operator owns.".to_string());
    lines.push("  These always carry a default: gating them produces broken deploys,".to_string());
    lines.push("  not safety (`DEPLOYMENT.md`, Configuration).".to_string());
    lines.push("- **behaviour** — tuning that decides neither; unconstrained.".to_string());
    lines.push(String::new());
    lines.join("\n")
}

pub fn render_example_config() -> String {
    let fields = describe_fields();
    let mut lines = vec![
        "# Generated by scripts/gen_config_docs.py from src/vogt/config.py.".to_string(),
        "# Do not edit by hand; CI fails on drift.".to_string(),
        "#".to_string(),
        "# Every setting is commented out and shows its default. Point Vogt at".to_string(),
        format!("# a copy of this file with {CONFIG_FILE_ENV}=/path/to/vogt.toml."),
        String::new(),
    ];
    for field in &fields {
        lines.push(format!("# {}", field.description));
        lines.push(format!("# default policy: {}", field.policy));
        lines.push(format!("# {} = {}", field.name, field.example));
        lines.push(String::new());
    }
    lines.join("\n")
}

/// The generated files, keyed by their committed location.
pub fn config_artifacts(root: &Path) -> BTreeMap<PathBuf, String> {
    let mut artifacts = BTreeMap::new();
    artifacts.insert(
        root.join("docs").join("CONFIG.md"),
        render_config_reference(),
    );
    artifacts.insert(root.join("config.example.toml"), render_example_config());
    artifacts
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvGuard {
        saved: Vec<(String, Option<String>)>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl EnvGuard {
        fn new() -> Self {
            Self {
                saved: Vec::new(),
                _lock: ENV_LOCK.lock().unwrap_or_else(|err| err.into_inner()),
            }
        }

        fn set(&mut self, key: &str, value: &str) {
            if !self.saved.iter().any(|(saved, _)| saved == key) {
                self.saved.push((key.to_string(), env::var(key).ok()));
            }
            env::set_var(key, value);
        }

        fn unset(&mut self, key: &str) {
            if !self.saved.iter().any(|(saved, _)| saved == key) {
                self.saved.push((key.to_string(), env::var(key).ok()));
            }
            env::remove_var(key);
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, value) in self.saved.drain(..) {
                match value {
                    Some(value) => env::set_var(key, value),
                    None => env::remove_var(key),
                }
            }
        }
    }

    fn clean() -> EnvGuard {
        let mut guard = EnvGuard::new();
        guard.unset(CONFIG_FILE_ENV);
        guard.unset("XDG_DATA_HOME");
        for field in FIELD_CATALOGUE {
            guard.unset(field.env_var);
        }
        guard
    }

    #[test]
    fn explicit_arguments_win() {
        let mut guard = clean();
        guard.set("VOGT_DATA_DIR", "/from/env");
        let mut overrides = Map::new();
        overrides.insert(
            "data_dir".to_string(),
            Value::String("/from/arg".to_string()),
        );
        let config = load_config(&overrides).unwrap();
        assert_eq!(config.data_dir, PathBuf::from("/from/arg"));
    }

    #[test]
    fn environment_beats_the_file() {
        let dir = std::env::temp_dir().join(format!("vogt-cfg-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("vogt.toml");
        std::fs::write(&file, "data_dir = \"/from/file\"\n").unwrap();
        let mut guard = clean();
        guard.set(CONFIG_FILE_ENV, file.to_str().unwrap());
        guard.set("VOGT_DATA_DIR", "/from/env");
        let config = load_config(&Map::new()).unwrap_or_else(|err| panic!("{err}"));
        assert_eq!(config.data_dir, PathBuf::from("/from/env"));
    }

    #[test]
    fn an_env_name_matches_regardless_of_case() {
        let mut guard = clean();
        guard.set("vogt_log_level", "debug");
        guard.set("Vogt_Retention_Days", "9");
        let config = load_config(&Map::new()).unwrap();
        assert_eq!(config.log_level, LogLevel::Debug);
        assert_eq!(config.retention_days, 9);
    }

    #[test]
    fn coercion_matches_pydantics_edges() {
        let dir = std::env::temp_dir().join(format!("vogt-cfg-edge-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("vogt.toml");
        std::fs::write(&file, "fronted = 1.0\n").unwrap();
        let _guard = clean();
        unsafe { std::env::set_var(CONFIG_FILE_ENV, file.to_str().unwrap()) };
        unsafe { std::env::set_var("VOGT_RETENTION_DAYS", " 5 ") };
        let config = load_config(&Map::new()).unwrap();
        assert!(config.fronted);
        assert_eq!(config.retention_days, 5);
        for rejected in ["1e3", "_1000", "1__000"] {
            unsafe { std::env::set_var("VOGT_RETENTION_DAYS", rejected) };
            assert!(
                load_config(&Map::new()).is_err(),
                "{rejected} should be rejected"
            );
        }
        unsafe { std::env::set_var("VOGT_RETENTION_DAYS", "7") };
        unsafe { std::env::set_var("VOGT_FRONTED", " yes ") };
        assert!(
            load_config(&Map::new()).is_err(),
            "a padded bool is rejected"
        );
    }

    #[test]
    fn a_lookahead_is_a_valid_service_pattern() {
        let dir = std::env::temp_dir().join(format!("vogt-cfg-re-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("vogt.toml");
        std::fs::write(
            &file,
            "[agent_activity_services]\nclaude = \"foo(?=bar)\"\n",
        )
        .unwrap();
        let mut guard = clean();
        guard.set(CONFIG_FILE_ENV, file.to_str().unwrap());
        let config = load_config(&Map::new()).unwrap();
        assert_eq!(
            config
                .agent_activity_services
                .get("claude")
                .map(String::as_str),
            Some("foo(?=bar)")
        );
    }

    #[test]
    fn lax_values_load_the_way_pydantic_loads_them() {
        let dir = std::env::temp_dir().join(format!("vogt-cfg-lax-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("vogt.toml");
        std::fs::write(
            &file,
            "fronted = \"yes\"\nretention_days = \"7\"\nlog_slow_request_ms = 250.0\n",
        )
        .unwrap();
        let mut guard = clean();
        guard.set(CONFIG_FILE_ENV, file.to_str().unwrap());
        guard.set("VOGT_FRONTED", "y");
        guard.set("VOGT_RETENTION_DAYS", "1_000");
        let config = load_config(&Map::new()).unwrap();
        assert!(config.fronted);
        assert_eq!(config.retention_days, 1000);
        assert_eq!(config.log_slow_request_ms, 250);

        // A fractional part of all zeros is the integer. A real fraction, a
        // trailing dot and scientific notation are not.
        for (raw, want) in [
            ("5.0", Some(5)),
            (" 5.0 ", Some(5)),
            ("1_000.0", Some(1000)),
        ] {
            guard.set("VOGT_RETENTION_DAYS", raw);
            assert_eq!(
                load_config(&Map::new()).unwrap().retention_days,
                want.unwrap()
            );
        }
        for raw in ["5.5", "5.", "1e3"] {
            guard.set("VOGT_RETENTION_DAYS", raw);
            assert!(load_config(&Map::new()).is_err(), "{raw} must be rejected");
        }
    }

    #[test]
    fn map_settings_merge_across_sources() {
        let dir = std::env::temp_dir().join(format!("vogt-cfg-merge-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("vogt.toml");
        std::fs::write(&file, "[forge_token_files]\ngithub = \"/from/file\"\n").unwrap();
        let mut guard = clean();
        guard.set(CONFIG_FILE_ENV, file.to_str().unwrap());
        guard.set("VOGT_FORGE_TOKEN_FILES", "{\"gitlab\":\"/from/env\"}");
        let config = load_config(&Map::new()).unwrap();
        assert_eq!(
            config.forge_token_files.get("github").map(PathBuf::as_path),
            Some(Path::new("/from/file"))
        );
        assert_eq!(
            config.forge_token_files.get("gitlab").map(PathBuf::as_path),
            Some(Path::new("/from/env"))
        );
    }

    #[test]
    fn the_file_beats_the_default() {
        let dir = std::env::temp_dir().join(format!("vogt-cfg-file-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("vogt.toml");
        std::fs::write(&file, "log_level = \"debug\"\n").unwrap();
        let mut guard = clean();
        guard.set(CONFIG_FILE_ENV, file.to_str().unwrap());
        let config = load_config(&Map::new()).unwrap();
        assert_eq!(config.log_level, LogLevel::Debug);
    }

    #[test]
    fn a_missing_config_file_is_not_an_error() {
        let mut guard = clean();
        guard.set(CONFIG_FILE_ENV, "/no/such/vogt.toml");
        let config = load_config(&Map::new()).unwrap();
        assert_eq!(config.log_level, LogLevel::Info);
    }

    #[test]
    fn unknown_settings_are_refused() {
        let _guard = clean();
        let mut overrides = Map::new();
        overrides.insert("not_a_setting".to_string(), Value::from(1));
        let err = load_config(&overrides).unwrap_err();
        assert!(err.contains("not_a_setting"), "{err}");
    }

    #[test]
    fn unknown_file_keys_are_ignored() {
        let dir = std::env::temp_dir().join(format!("vogt-cfg-unk-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("vogt.toml");
        std::fs::write(&file, "not_a_setting = 1\nlog_level = \"warning\"\n").unwrap();
        let mut guard = clean();
        guard.set(CONFIG_FILE_ENV, file.to_str().unwrap());
        let config = load_config(&Map::new()).unwrap();
        assert_eq!(config.log_level, LogLevel::Warning);
    }

    #[test]
    fn data_dir_follows_xdg() {
        let mut guard = clean();
        guard.set("XDG_DATA_HOME", "/xdg/data");
        assert_eq!(default_data_dir(), PathBuf::from("/xdg/data/vogt"));
    }

    #[test]
    fn store_paths_hang_off_the_data_dir() {
        let _guard = clean();
        let mut overrides = Map::new();
        overrides.insert(
            "data_dir".to_string(),
            Value::String("/instance".to_string()),
        );
        let config = load_config(&overrides).unwrap();
        assert_eq!(
            config.declared_db_path(),
            PathBuf::from("/instance/declared.sqlite3")
        );
        assert_eq!(
            config.observed_db_path(),
            PathBuf::from("/instance/observed.sqlite3")
        );
        assert_eq!(config.backups_dir(), PathBuf::from("/instance/backups"));
        assert_eq!(
            config.resolved_import_root(),
            PathBuf::from("/instance/repos")
        );
    }

    #[test]
    fn a_tilde_in_the_data_dir_is_expanded() {
        let _guard = clean();
        let mut overrides = Map::new();
        overrides.insert(
            "data_dir".to_string(),
            Value::String("~/vogt-test".to_string()),
        );
        let config = load_config(&overrides).unwrap();
        assert!(!config.resolved_data_dir().to_string_lossy().contains('~'));
    }

    #[test]
    fn exposure_values_never_carry_a_default() {
        for field in describe_fields() {
            if field.policy == DefaultPolicy::Exposure {
                assert!(
                    field.default_label.starts_with("*(no default"),
                    "{} decides exposure and must not default",
                    field.name
                );
            }
        }
    }

    #[test]
    fn allocation_values_always_carry_a_default() {
        for field in describe_fields() {
            if field.policy == DefaultPolicy::Allocation {
                assert!(
                    !field.default_label.starts_with("*(no default"),
                    "{} is a host allocation and must carry a default",
                    field.name
                );
            }
        }
    }

    #[test]
    fn every_field_is_documented_and_classified() {
        for field in describe_fields() {
            assert!(
                !field.description.trim().is_empty(),
                "{} has no description",
                field.name
            );
            assert!(matches!(
                field.policy,
                DefaultPolicy::Exposure | DefaultPolicy::Allocation | DefaultPolicy::Behaviour
            ));
        }
    }

    #[test]
    fn generated_text_leaks_neither_pathlib_nor_union() {
        for content in config_artifacts(Path::new("/unused")).values() {
            assert!(!content.contains("pathlib"));
            assert!(!content.contains("| Union |"));
        }
    }

    #[test]
    fn config_artifacts_match_the_committed_files() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let artifacts = config_artifacts(&root);
        for relative in ["docs/CONFIG.md", "config.example.toml"] {
            let path = root.join(relative);
            let expected =
                std::fs::read_to_string(&path).unwrap_or_else(|err| panic!("{relative}: {err}"));
            let rendered = artifacts.get(&path).unwrap();
            assert_eq!(rendered, &expected, "{relative} drifted");
        }
    }

    #[test]
    fn deploy_lane_requires_a_source_and_http() {
        let _guard = clean();
        let bad = serde_json::json!([{"name": "dev", "project": "app"}]);
        let mut overrides = Map::new();
        overrides.insert("deploy_lanes".to_string(), bad);
        assert!(load_config(&overrides).unwrap_err().contains("no receipt"));

        let bad = serde_json::json!([{
            "name": "dev", "project": "app", "version_url": "ftp://x"
        }]);
        let mut overrides = Map::new();
        overrides.insert("deploy_lanes".to_string(), bad);
        assert!(load_config(&overrides).unwrap_err().contains("http(s)"));

        let bad = serde_json::json!([{
            "name": "dev", "project": "app", "receipt_repo": "org/repo"
        }]);
        let mut overrides = Map::new();
        overrides.insert("deploy_lanes".to_string(), bad);
        assert!(load_config(&overrides).unwrap_err().contains("together"));
    }

    #[test]
    fn activity_roots_and_services_are_checked() {
        let _guard = clean();
        let mut overrides = Map::new();
        overrides.insert(
            "agent_activity_roots".to_string(),
            serde_json::json!({"gemini": "/tmp"}),
        );
        assert!(load_config(&overrides).unwrap_err().contains("claude"));

        let mut overrides = Map::new();
        overrides.insert(
            "session_transcript_roots".to_string(),
            serde_json::json!({"gemini": "/tmp"}),
        );
        assert!(load_config(&overrides).unwrap_err().contains("klaudia"));

        let mut overrides = Map::new();
        overrides.insert(
            "agent_activity_services".to_string(),
            serde_json::json!({"ci": "(unclosed"}),
        );
        assert!(load_config(&overrides).unwrap_err().contains("regex"));
    }

    #[test]
    fn numeric_bounds_and_sqlite_values() {
        let _guard = clean();
        let mut overrides = Map::new();
        overrides.insert("session_ttl_days".to_string(), Value::from(0));
        assert!(load_config(&overrides).is_err());
        overrides.insert("session_ttl_days".to_string(), Value::from(3651));
        assert!(load_config(&overrides).is_err());
        overrides.insert(
            "sqlite_synchronous".to_string(),
            Value::String("OFF".into()),
        );
        assert!(load_config(&overrides).is_err());
        overrides.insert(
            "sqlite_synchronous".to_string(),
            Value::String("extra".into()),
        );
        overrides.insert("session_ttl_days".to_string(), Value::from(30));
        assert!(load_config(&overrides).is_ok());
    }
}
