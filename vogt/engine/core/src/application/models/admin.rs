//! The instance-administration operations' parameter and result models:
//! the MCP stdio bridge, export, backup, restore, serve and connect.
//!
//! Ports `McpStdio*`, `Export*`, `Backup*`, `Restore*`, `Serve*` and
//! `Connect*` in `src/vogt/application/models.py`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{date_time, integer, optional_string};

/// `mcp stdio` takes nothing: the streams are this process's own.
#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("properties" = {}))]
pub struct McpStdioParams {}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct McpStdioResult {
    #[schemars(schema_with = "optional_string")]
    pub protocol_version: Option<String>,
    #[schemars(schema_with = "integer")]
    pub messages_handled: i64,
    pub supported_protocol_versions: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExportParams {
    pub destination: String,
    /// Export one project (by slug): its work items with their comments and relations, and only the initiatives, labels and actors they reference. Omitted, the whole instance is exported.
    #[serde(default)]
    pub project: Option<String>,
    #[schemars(length(min = 1))]
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExportResult {
    pub path: String,
    /// The file's format. 2 carries what an applying import needs.
    #[schemars(schema_with = "integer")]
    pub export_format_version: i64,
    /// The scope, if one.
    #[serde(default)]
    pub project: Option<String>,
    #[schemars(schema_with = "integer")]
    pub projects: i64,
    #[schemars(schema_with = "integer")]
    pub work_items: i64,
    #[serde(default)]
    #[schemars(schema_with = "integer")]
    pub comments: i64,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BackupParams {
    /// Defaults to <data-dir>/backups/<timestamp>.
    #[serde(default)]
    pub destination: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[schemars(length(min = 1))]
    pub reason: String,
}

/// What a backup covered, including the parts it could not.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BackupResult {
    pub path: String,
    pub instance_id: String,
    #[schemars(schema_with = "integer")]
    pub declared_schema_version: i64,
    #[schemars(schema_with = "integer")]
    pub observed_schema_version: i64,
    #[schemars(schema_with = "date_time")]
    pub taken_at: String,
    /// What happened to the session engine's state directory: copied, not configured, or a failure. Stated rather than implied — a backup that quietly covered two thirds of the product would be indistinguishable from one that covered all of it until somebody restored it.
    #[serde(default = "engine_state_not_configured")]
    #[schemars(extend("default" = "not configured"))]
    pub engine_state: String,
    /// Where imported projects lived when this was taken. A restore elsewhere leaves every project pointing at a path that is not there.
    #[serde(default)]
    pub import_root: Option<String>,
}

fn engine_state_not_configured() -> String {
    "not configured".to_string()
}

fn engine_state_not_in_backup() -> String {
    "not in this backup".to_string()
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RestoreParams {
    /// A backup directory containing manifest.json.
    pub source: String,
    /// Required: this replaces the live stores.
    #[serde(default)]
    pub confirm: bool,
    #[schemars(length(min = 1))]
    pub reason: String,
}

/// What came back, and whether the estate is still where it was.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RestoreResult {
    pub source: String,
    pub instance_id: String,
    #[schemars(schema_with = "date_time")]
    pub restored_from: String,
    pub migrations_applied: Vec<String>,
    #[schemars(schema_with = "integer")]
    pub declared_schema_version: i64,
    /// What happened to the session engine's state directory.
    #[serde(default = "engine_state_not_in_backup")]
    #[schemars(extend("default" = "not in this backup"))]
    pub engine_state: String,
    /// Where imported projects lived when the backup was taken.
    #[serde(default)]
    pub import_root_then: Option<String>,
    /// Where they will be looked for now. A difference is not an error and is not corrected — the paths are in the store — but it is the reason a restored session will not open, so it is reported here rather than discovered there.
    #[serde(default)]
    pub import_root_now: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ServeParams {
    /// Listen address. No default anywhere — it encodes exposure . The compose file supplies it.
    pub host: String,
    /// Listen port. No default, for the same reason as host.
    #[schemars(schema_with = "integer", range(min = 1, max = 65535))]
    pub port: i64,
    /// Operator-owned certificate, mounted read-only.
    #[serde(default)]
    pub tls_cert: Option<String>,
    #[serde(default)]
    pub tls_key: Option<String>,
    /// Serve without authentication. Only sane on loopback.
    #[serde(default)]
    pub no_auth: bool,
    /// Refuse every write, whatever scope a token holds.
    #[serde(default)]
    pub read_only: bool,
    /// Do not collect in the background. The schedule is on by default because an instance that never looks cannot tell stale evidence from none; this is the switch for a diagnostic run.
    #[serde(default)]
    pub no_schedule: bool,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ServeResult {
    pub url: String,
    pub api_path: String,
    pub mcp_path: String,
    pub auth_required: bool,
    // Whether background collection is running. Reported rather than assumed:
    // "the server is up" and "the server is looking" are different facts, and
    // only one of them keeps freshness small.
    #[serde(default = "collecting_default")]
    #[schemars(extend("default" = true))]
    pub collecting: bool,
    pub writes_enabled: bool,
}

fn collecting_default() -> bool {
    true
}

/// What a client needs in order to reach this instance.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConnectParams {
    /// `http` for a client that speaks streamable HTTP MCP — the ordinary case, and the one that needs nothing installed. `bridge` for a client that can only spawn a local process.
    #[serde(default = "client_http")]
    #[schemars(extend("default" = "http"))]
    pub client: ClientKind,
    /// `markdown` renders the connection document `DEPLOYMENT.md` describes; redirect it to CONNECTING.md if you want it on disk.
    #[serde(default = "format_json")]
    #[schemars(extend("default" = "json"))]
    pub format: ConnectFormat,
}

// `ClientKind` in Python: a literal, so the schema is inline.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[schemars(inline)]
pub enum ClientKind {
    #[serde(rename = "http")]
    Http,
    #[serde(rename = "bridge")]
    Bridge,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[schemars(inline)]
pub enum ConnectFormat {
    #[serde(rename = "json")]
    Json,
    #[serde(rename = "markdown")]
    Markdown,
}

fn client_http() -> ClientKind {
    ClientKind::Http
}

fn format_json() -> ConnectFormat {
    ConnectFormat::Json
}

/// The connection facts, and the client configuration built from them.
///
/// `url` is `None` when nobody has configured one. That is deliberately not
/// a guess: a URL the server invented would be wrong in exactly the
/// deployment this field exists for, and a client cannot tell a wrong URL
/// from an unreachable one.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConnectResult {
    #[serde(default)]
    pub url: Option<String>,
    pub api_path: String,
    pub mcp_path: String,
    #[serde(default)]
    pub mcp_url: Option<String>,
    pub supported_mcp_protocol_versions: Vec<String>,
    pub client: ClientKind,
    /// Whether the client needs Vogt's own code present. False for streamable HTTP, which is why it is the recommended path.
    pub requires_install: bool,
    /// Ready to use: JSON for a client config, or the document.
    pub configuration: String,
    #[serde(default)]
    pub detail: Option<String>,
}
