//! Instance, forge, event, inbox and audit operations' models.
//!
//! Ported from `src/vogt/application/models.py`; each schema is checked against
//! Python's live registry by `tests/test_rust_schemas.py`.

use schemars::{json_schema, JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};

use super::entities::{Freshness, Project};
use super::views::{
    AuditRecord, CloneInfo, DeployedLaneView, DiagnosticCheck, DiagnosticsResultStatus,
    EngineAgentTaskStatus, EngineDiskStatus, EngineHistoryStatus, EngineLag, Event,
    ForgeImportParamsLifecycleState, ForgeRepoView, ImportChange, ImportTally, InboxCoverage,
    InboxEntry, InboxListParamsActor, InboxListParamsSourcesItem, InboxListParamsTriageStatesItem,
    InboxListResultEngineStatus, InboxSavedFilter, MigratedItem, NotificationView, PeerDiagnostics,
    RecentLog, RegistryOperationManifest, SetWriteBackParamsPolicy, StoreCounts, StoreMigration,
    WriteBackActionView, WriteBackListParamsOutcome,
};
use super::{
    date_time, integer, number, optional_date_time, optional_integer, optional_number,
    optional_string,
};

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("properties" = {}))]
pub struct StatusParams {}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StatusResult {
    pub vogt_version: String,
    pub instance_id: String,
    pub data_dir: String,
    pub principal: String,
    pub revision: i64,
    pub declared_schema_version: i64,
    pub observed_schema_version: i64,
    pub counts: StoreCounts,
    /// Set when this instance's data was cloned from another instance's backup (`vogt clone`); absent on an instance that never was.
    #[serde(default)]
    pub clone: Option<CloneInfo>,
    /// The workspace volume, when a session engine answers. needs_attention once used space passes its threshold.
    #[serde(default)]
    pub disk: Option<EngineDiskStatus>,
}
/// `registry.dump` takes nothing: the registry is the whole answer.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("properties" = {}))]
pub struct RegistryDumpParams {}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RegistryDumpResult {
    pub operations: Vec<RegistryOperationManifest>,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticsParams {
    /// Also ask the configured peer instance (`diagnostics_peer_url`) for its diagnostics, e.g. prod from dev.
    #[serde(default)]
    pub peer: bool,
    /// How many recent warning-or-worse log lines to return.
    #[serde(default = "default_diagnostics_params_log_lines")]
    #[schemars(range(min = 0, max = 200))]
    pub log_lines: i64,
}
/// Is this instance what it should be, and is it well — in one read.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticsResult {
    /// The worst of `checks`; not_configured checks do not count.
    pub status: DiagnosticsResultStatus,
    pub vogt_version: String,
    /// As the deployment stated it (`image_digest`); null if unstated.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub image_digest: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub instance_id: Option<String>,
    #[schemars(schema_with = "date_time")]
    pub started_at: String,
    pub uptime_seconds: i64,
    pub checks: Vec<DiagnosticCheck>,
    pub migrations: std::collections::BTreeMap<String, StoreMigration>,
    pub recent_log: RecentLog,
    pub peer: PeerDiagnostics,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("properties" = {}))]
pub struct EngineStatusParams {}
/// The session engine's operational report, as the GUI's Settings shows it.
///
/// `engine` is set, and the rest left at its defaults, when the engine is not
/// configured or could not be asked: an outage is said, never rendered as an
/// idle engine.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EngineStatusResult {
    /// Why the engine could not be asked; null when it answered.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub engine: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub version: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub source_ref: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub source_sha: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub release_url: Option<String>,
    #[serde(default)]
    pub session_count: Option<i64>,
    #[serde(default)]
    pub push_subscription_count: Option<i64>,
    #[serde(default)]
    pub gui_process_count: Option<i64>,
    #[serde(default)]
    pub gui_stream_configured: Option<bool>,
    #[serde(default)]
    pub fcm_enabled: Option<bool>,
    /// Whether the engine resolves agent credentials at launch.
    #[serde(default)]
    pub auto_agent_auth: Option<bool>,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub state_dir: Option<String>,
    /// The root every session and file path is under.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub workspace_root: Option<String>,
    #[serde(default)]
    pub history: Option<EngineHistoryStatus>,
    #[serde(default)]
    pub agent_tasks: Option<EngineAgentTaskStatus>,
    /// Event subscribers that fell behind since the engine started, by name. Empty is healthy.
    #[serde(default)]
    pub event_lag: std::collections::BTreeMap<String, EngineLag>,
    /// The workspace volume. needs_attention once used space passes the engine's threshold (85% by default).
    #[serde(default)]
    pub disk: Option<EngineDiskStatus>,
}
/// The shell's bounded, aggregate navigation counts.
///
/// The Inbox count honours the caller's saved Inbox filter (the
/// `inbox.filter` preference) — the same answer `inbox.list` gives under it.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("properties" = {}))]
pub struct PlaceMetricsParams {}
/// One read for the shell's glanceable place badges.
///
/// A nullable field means that only that provider failed; it never turns an
/// unavailable answer into a misleading zero.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlaceMetricsResult {
    /// The Inbox badge: entries matching the caller's saved Inbox filter (all active entries when none is saved).
    #[serde(default)]
    pub inbox_active: Option<i64>,
    /// Every active Inbox entry, ignoring the filter.
    #[serde(default)]
    pub inbox_active_unfiltered: Option<i64>,
    /// The saved filter the badge applied; null when the caller has none (or only the default), so the badge is unfiltered.
    #[serde(default)]
    pub inbox_filter: Option<InboxSavedFilter>,
    #[serde(default)]
    pub projects_total: Option<i64>,
    #[serde(default)]
    pub work_total: Option<i64>,
    #[serde(default)]
    pub backlog_total_considered: Option<i64>,
    #[serde(default)]
    pub drift_present: Option<bool>,
    pub revision: i64,
    #[schemars(schema_with = "date_time")]
    pub generated_at: String,
}
/// Restore another instance's backup here, as a copy rather than a move.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CloneParams {
    /// A backup directory containing manifest.json.
    pub source: String,
    /// Required: this replaces the live stores.
    #[serde(default)]
    pub confirm: bool,
    /// Also copy the source engine's session history (history.db, session-logs/, assistant-log.db) into this engine's state directory. Push subscriptions and agent tasks are never copied.
    #[serde(default)]
    pub include_engine_state: bool,
    #[schemars(length(min = 1))]
    pub reason: String,
}
/// What came across, and what was deliberately left behind.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CloneResult {
    pub source: String,
    /// This instance's id, which it keeps.
    pub instance_id: String,
    pub source_instance_id: String,
    /// When the source's backup was taken.
    #[schemars(schema_with = "date_time")]
    pub restored_from: String,
    #[schemars(schema_with = "date_time")]
    pub cloned_at: String,
    pub migrations_applied: Vec<String>,
    pub declared_schema_version: i64,
    /// This instance's own tokens, carried over.
    pub tokens_kept: i64,
    /// Live tokens from the source, revoked in the copy.
    pub source_tokens_revoked: i64,
    pub password_logins_kept: i64,
    pub source_password_logins_dropped: i64,
    pub forge_accounts_kept: i64,
    pub source_forge_accounts_dropped: i64,
    /// Projects whose write-back was armed in the source, now `none`.
    pub write_back_reset: Vec<String>,
    /// Source sessions recorded as running, now recorded as stopped.
    pub sessions_closed: i64,
    /// What happened to the session engine's state directory.
    pub engine_state: String,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub import_root_then: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub import_root_now: Option<String>,
}
/// Merge an export into this instance — a dry run unless `apply`.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportParams {
    /// An export file written by `vogt export`.
    pub source: String,
    /// Merge only this project (by slug): its work items, comments and relations, and the initiatives, labels and actors they reference.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub project: Option<String>,
    /// Write the merge. Without it the import is a dry run: the same report, nothing written.
    #[serde(default)]
    pub apply: bool,
    /// Required with apply: this writes into the live store.
    #[serde(default)]
    pub confirm: bool,
    /// Fail, writing nothing, if any entity changed on both sides, instead of keeping this instance's version and recording the incoming one as a conflict comment.
    #[serde(default)]
    pub strict: bool,
    #[schemars(length(min = 1))]
    pub reason: String,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportResult {
    pub source: String,
    /// The instance the export was taken from.
    pub instance_id: String,
    /// 1 for an export written before applying import existed.
    pub export_format_version: i64,
    pub projects: i64,
    pub work_items: i64,
    pub applied: bool,
    pub detail: String,
    /// The scope, if one.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub project: Option<String>,
    /// The baseline change is measured against: an entity whose updated_at is later changed since the two instances last agreed.
    #[serde(default)]
    #[schemars(schema_with = "optional_date_time")]
    pub base: Option<String>,
    /// Where the baseline came from, or why there is none.
    #[serde(default)]
    pub base_source: String,
    #[serde(default)]
    pub created: i64,
    #[serde(default)]
    pub updated: i64,
    #[serde(default)]
    pub conflicted: i64,
    #[serde(default)]
    pub skipped: i64,
    #[serde(default)]
    pub unchanged: i64,
    #[serde(default)]
    pub by_entity: std::collections::BTreeMap<String, ImportTally>,
    #[serde(default)]
    pub changes: Vec<ImportChange>,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SetWriteBackParams {
    pub project: String,
    /// none: observe and never speak. comment_only: post comments authored here. full: also create, label, and close/reopen. Never deletion, history rewriting or force.
    pub policy: SetWriteBackParamsPolicy,
    #[schemars(length(min = 1))]
    pub reason: String,
}
/// Make a registered project upstream-truth.
///
/// Validated, not assumed: the project must carry a `repo_url` a registered
/// provider matches and a usable credential (the acting actor's linked PAT,
/// or the instance file token). A missing precondition is a typed
/// refusal that names it.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ForgeLinkParams {
    /// Project slug.
    pub project: String,
    #[schemars(length(min = 1))]
    pub reason: String,
}
/// `forge.link`'s receipt: the linked project, and what migrated.
///
/// `migrated` lists every open native item published upstream and re-keyed
/// during this link, oldest first. A mid-migration provider failure raises
/// instead — naming which items migrated and which are still native — so a
/// result in hand means every listed item, and only those, moved.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ForgeLinkResult {
    pub project: Project,
    #[serde(default)]
    #[schemars(extend("x-keep-default" = true))]
    pub migrated: Vec<MigratedItem>,
}
/// Create the remote and make the project upstream-truth.
///
/// The first verb that creates upstream state and pushes commits, which is
/// why every precondition is typed and checked up front: not already
/// linked, no `repo_url` to clobber, a clean checkout on a named branch,
/// and a usable credential. The push is plain — never forced.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ForgePublishParams {
    /// Project slug.
    pub project: String,
    /// Repository name to create; defaults to the project slug.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub name: Option<String>,
    /// Whether the new repository is private. Private by default: publishing makes a repository, not an announcement.
    #[serde(default = "default_forge_publish_params_private")]
    pub private: bool,
    /// Repository description, if any.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub description: Option<String>,
    #[schemars(length(min = 1))]
    pub reason: String,
}
/// What `forge.publish` did, in the order it did it.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ForgePublishResult {
    pub project: Project,
    /// The created repository's web URL.
    pub repo: String,
    /// The local branch pushed as the default.
    pub branch: String,
    /// The commit the push left as the remote head.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub revision: Option<String>,
    #[serde(default)]
    #[schemars(extend("x-keep-default" = true))]
    pub migrated: Vec<MigratedItem>,
}
/// Enumerate the repositories a credential can see, to pick one to import.
///
/// The credential is the acting actor's linked PAT when they have one,
/// and the instance file token otherwise — so a person sees *their*
/// repositories, including private ones their PAT reaches. This lists; it never
/// crawls, and picking one is the only way an import ever begins.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ForgeReposParams {
    /// The configured forge host to enumerate, for example github.com or a Forgejo host named under forge_token_files.
    #[serde(default = "default_forge_repos_params_host")]
    pub host: String,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ForgeReposResult {
    pub repos: Vec<ForgeRepoView>,
    /// Whose credential enumerated these — the linked actor login, or null when the instance file token was used.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub login: Option<String>,
    /// Why the list is empty when it is — no forge configured for the host, for instance — so an empty picker reads as 'not collected' rather than 'you have no repositories'.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub detail: Option<String>,
}
/// Import a repository the picker listed, as a project.
///
/// Named exactly as `forge.repos` shows it — `owner` and `name` — because
/// this is the verb that turns one of those listed rows into a project:
/// clone under the acting credential, register, and consolidate. The
/// credential that reaches the repository to list it is the credential the
/// clone runs under, so a private repository a personal PAT can see imports
/// without the instance file token ever needing access to it.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ForgeImportParams {
    /// Repository owner, as `forge repos` lists it.
    #[schemars(length(min = 1))]
    pub owner: String,
    /// Repository name, as `forge repos` lists it.
    #[schemars(length(min = 1))]
    pub name: String,
    /// The configured forge host, for example github.com or a Forgejo host named under forge_token_files.
    #[serde(default = "default_forge_import_params_host")]
    pub host: String,
    /// Project display name. Defaults to the repository name.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub display_name: Option<String>,
    /// Where to clone to. Defaults to `<import_root>/<slug>`; supply this only when the repository must live somewhere specific.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub root_path: Option<String>,
    #[serde(default = "default_forge_import_params_lifecycle_state")]
    pub lifecycle_state: ForgeImportParamsLifecycleState,
    /// Read existing issues, PRs, labels and releases after registering . Read-only, and on by default so the imported project arrives as upstream-truth rather than looking empty.
    #[serde(default = "default_forge_import_params_consolidate")]
    pub consolidate: bool,
    /// Why this write is being made (audited).
    #[schemars(length(min = 1))]
    pub reason: String,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WriteBackListParams {
    #[serde(default)]
    pub outcome: Option<WriteBackListParamsOutcome>,
    #[serde(default = "default_write_back_list_params_limit")]
    #[schemars(range(min = 1, max = 500))]
    pub limit: i64,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WriteBackListResult {
    pub actions: Vec<WriteBackActionView>,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListEventsParams {
    /// Cursor: return events with seq greater than this.
    #[serde(default)]
    #[schemars(range(min = 0))]
    pub after: i64,
    #[serde(default = "default_list_events_params_limit")]
    #[schemars(range(min = 1, max = 1000))]
    pub limit: i64,
    /// Only events about this entity. The id, not the ref — the same shape audit.list takes, so the two feeds narrow alike.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub entity_id: Option<String>,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EventListResult {
    pub events: Vec<Event>,
    pub next_cursor: i64,
}
/// Filters over the collected forge inbox.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NotificationsParams {
    /// Project slug. Omit for every registered project.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub project: Option<String>,
    /// GitHub's reason, e.g. mention, review_requested, ci_activity.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub reason: Option<String>,
    /// Only threads GitHub still considers unread for this token.
    #[serde(default)]
    pub unread_only: bool,
    #[serde(default = "default_notifications_params_limit")]
    #[schemars(range(min = 1, max = 500))]
    pub limit: i64,
    #[serde(default)]
    #[schemars(range(min = 0))]
    pub offset: i64,
}
/// The inbox, and what it is honestly able to be.
///
///  `freshness` is here for the same reason every other aggregate carries one
/// : an empty inbox with no sweep behind it means nobody has looked.
///  `scope` states out loud that these belong to the configured token's
///  account rather than to the reading actor — a limit of the design, not of
///  this response.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NotificationsResult {
    pub notifications: Vec<NotificationView>,
    pub total: i64,
    #[serde(default)]
    #[schemars(extend("x-keep-default" = true))]
    pub by_reason: std::collections::BTreeMap<String, i64>,
    #[serde(default)]
    pub unread: i64,
    /// Whose inbox this is.
    #[serde(default = "default_notifications_result_scope")]
    pub scope: String,
    #[serde(default)]
    pub freshness: Freshness,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub detail: Option<String>,
}
/// Which configured deployment lanes to report (`deploy_lanes`).
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeployedVersionsParams {
    /// Project slug. Omit for every configured lane.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub project: Option<String>,
    /// One lane by name, e.g. `dev`. Omit for all.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub lane: Option<String>,
}
/// Every configured lane, honest about lanes nobody has read yet.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeployedVersionsResult {
    pub lanes: Vec<DeployedLaneView>,
    /// How many lanes `deploy_lanes` configures in total.
    pub configured: i64,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub detail: Option<String>,
    #[serde(default)]
    pub freshness: Freshness,
}

/// The usage board takes no filter: it is one view of every account.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("properties" = {}))]
pub struct UsageBoardParams {}

/// One rate-limit window a provider reported for an account.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UsageWindowView {
    /// `primary` / `secondary` (Codex), or `limit` for a Claude limit-hit message.
    pub name: String,
    #[serde(default)]
    #[schemars(schema_with = "optional_number")]
    pub used_percent: Option<f64>,
    #[serde(default)]
    #[schemars(schema_with = "optional_integer")]
    pub window_minutes: Option<i64>,
    #[serde(default)]
    #[schemars(schema_with = "optional_date_time")]
    pub resets_at: Option<String>,
    /// The provider's own words for the reset.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub resets_text: Option<String>,
}

/// One provider account: its reported windows, and whether it is limited.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UsageAccountView {
    pub name: String,
    pub agent: String,
    /// A window at 100% or a hit not yet reset.
    pub limited: bool,
    #[serde(default)]
    pub windows: Vec<UsageWindowView>,
    #[serde(default)]
    #[schemars(schema_with = "optional_date_time")]
    pub limited_at: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub limit_reached: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub plan: Option<String>,
    /// Tokens the transcripts record in the last 5h, each message once, cache reads excluded (Claude only).
    #[serde(default)]
    #[schemars(schema_with = "integer")]
    pub tokens_5h: i64,
    /// The newest transcript entry read.
    #[serde(default)]
    #[schemars(schema_with = "optional_date_time")]
    pub observed_at: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub detail: Option<String>,
}

/// One LiteLLM key's spend, never the key itself.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UsageKeyView {
    pub name: String,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub alias: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "optional_number")]
    pub spend_usd: Option<f64>,
    /// True when `spend_usd` is today's (a one-day budget window).
    #[serde(default)]
    pub daily: bool,
    #[serde(default)]
    #[schemars(schema_with = "optional_number")]
    pub max_budget_usd: Option<f64>,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub budget_duration: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "optional_date_time")]
    pub budget_reset_at: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "optional_number")]
    pub alert_usd: Option<f64>,
    #[serde(default)]
    pub alert: bool,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub detail: Option<String>,
}

/// A spend alert: a key at or over its day threshold.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UsageAlert {
    pub key: String,
    #[schemars(schema_with = "number")]
    pub spend_usd: f64,
    #[schemars(schema_with = "number")]
    pub threshold_usd: f64,
    pub message: String,
}

/// Every configured account and key, read now from what each reported.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UsageBoardResult {
    pub accounts: Vec<UsageAccountView>,
    pub keys: Vec<UsageKeyView>,
    pub alerts: Vec<UsageAlert>,
    #[schemars(schema_with = "date_time")]
    pub generated_at: String,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub detail: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InboxListParams {
    #[serde(default)]
    pub sources: Option<Vec<InboxListParamsSourcesItem>>,
    #[serde(default = "default_inbox_list_params_triage_states")]
    pub triage_states: Vec<InboxListParamsTriageStatesItem>,
    /// Who caused the entry: any; external (people outside the repository's org — never bots, and entries whose author is unknown are hidden and counted in actor_unknown_hidden); org (org members); bot (bots and the instance's own drift/CI/agent entries).
    #[serde(default = "default_inbox_list_params_actor")]
    pub actor: InboxListParamsActor,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub project: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub work_item: Option<String>,
    #[serde(default = "default_inbox_list_params_limit")]
    #[schemars(range(min = 1, max = 100))]
    pub limit: i64,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub cursor: Option<String>,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InboxListResult {
    pub entries: Vec<InboxEntry>,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub next_cursor: Option<String>,
    #[schemars(schema_with = "date_time")]
    pub snapshot_at: String,
    #[schemars(required)]
    #[schemars(schema_with = "high_water_by_source")]
    pub high_water: std::collections::BTreeMap<String, Option<String>>,
    #[schemars(required)]
    #[schemars(schema_with = "coverage_by_source")]
    pub coverage: std::collections::BTreeMap<String, InboxCoverage>,
    pub counts: std::collections::BTreeMap<String, i64>,
    #[serde(default = "default_inbox_list_result_github_scope")]
    pub github_scope: String,
    #[serde(default = "default_inbox_list_result_instance_scope")]
    pub instance_scope: String,
    pub engine_status: InboxListResultEngineStatus,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub engine_detail: Option<String>,
    #[serde(default = "default_inbox_list_result_engine_available")]
    pub engine_available: bool,
    /// With actor=external: how many entries matching every other filter were hidden because their author is unknown.
    #[serde(default)]
    pub actor_unknown_hidden: i64,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InboxArchiveParams {
    pub entry_key: String,
    #[schemars(length(min = 1))]
    pub reason: String,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InboxTriageResult {
    pub entry: InboxEntry,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InboxSnoozeParams {
    pub entry_key: String,
    #[schemars(schema_with = "date_time")]
    pub until: String,
    #[schemars(length(min = 1))]
    pub reason: String,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InboxRestoreParams {
    pub entry_key: String,
    #[schemars(length(min = 1))]
    pub reason: String,
}
/// How the audit log is narrowed.
///
/// `limit`/`offset`/`total` rather than a cursor, which is the idiom every
/// other filtered list here uses (`work.list`, `observations.list`). The
/// events feed's `after` cursor is the exception and earns it: that feed is
/// read forwards by a poller, while the audit log is read newest-first by a
/// person, and a cursor cannot answer "how many records match at all" —
/// which is what tells a reader whether they are looking at the whole story.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListAuditParams {
    #[serde(default = "default_list_audit_params_limit")]
    #[schemars(range(min = 1, max = 500))]
    pub limit: i64,
    /// Records to skip. Paging a log that is being written to can repeat a record, because new rows arrive at the front.
    #[serde(default)]
    #[schemars(range(min = 0))]
    pub offset: i64,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub actor_id: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub operation: Option<String>,
    /// An entity's id. A work item's trail also carries the writes audited against that item's comments.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub entity_id: Option<String>,
    /// Project slug. Keeps the writes this instance can attribute to that project; see the operation's docstring for the kinds that carry one.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub project: Option<String>,
    /// Inclusive lower bound on the write's time.
    #[serde(default)]
    #[schemars(schema_with = "optional_date_time")]
    pub since: Option<String>,
    /// Exclusive upper bound on the write's time.
    #[serde(default)]
    #[schemars(schema_with = "optional_date_time")]
    pub until: Option<String>,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AuditListResult {
    pub records: Vec<AuditRecord>,
    /// Records matching the filters, ignoring limit and offset.
    pub total: i64,
}

fn default_diagnostics_params_log_lines() -> i64 {
    20
}

fn default_forge_publish_params_private() -> bool {
    true
}

fn default_forge_repos_params_host() -> String {
    "github.com".to_string()
}

fn default_forge_import_params_host() -> String {
    "github.com".to_string()
}

fn default_forge_import_params_lifecycle_state() -> ForgeImportParamsLifecycleState {
    ForgeImportParamsLifecycleState::Active
}

fn default_forge_import_params_consolidate() -> bool {
    true
}

fn default_write_back_list_params_limit() -> i64 {
    100
}

fn default_list_events_params_limit() -> i64 {
    100
}

fn default_notifications_params_limit() -> i64 {
    50
}

fn default_notifications_result_scope() -> String {
    "the GitHub account whose token this instance is configured with; notifications are instance-scoped, not per-actor".to_string()
}

fn default_inbox_list_params_triage_states() -> Vec<InboxListParamsTriageStatesItem> {
    vec![InboxListParamsTriageStatesItem::Active]
}

fn default_inbox_list_params_actor() -> InboxListParamsActor {
    InboxListParamsActor::Any
}

fn default_inbox_list_params_limit() -> i64 {
    50
}

fn high_water_by_source(_: &mut SchemaGenerator) -> Schema {
    json_schema!({ "type": "object", "propertyNames": { "enum": ["github", "drift", "ci", "agent"] }, "additionalProperties": { "anyOf": [{ "type": "string", "format": "date-time" }, { "type": "null" }] } })
}

fn coverage_by_source(generator: &mut SchemaGenerator) -> Schema {
    json_schema!({ "type": "object", "propertyNames": { "enum": ["github", "drift", "ci", "agent"] }, "additionalProperties": generator.subschema_for::<InboxCoverage>() })
}

fn default_inbox_list_result_github_scope() -> String {
    "registered projects only".to_string()
}

fn default_inbox_list_result_instance_scope() -> String {
    "registered projects only".to_string()
}

fn default_inbox_list_result_engine_available() -> bool {
    true
}

fn default_list_audit_params_limit() -> i64 {
    50
}
