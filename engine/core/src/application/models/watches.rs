//! Watch parameter and result models. Ports the watch models in
//! `src/vogt/application/models.py` (WI-2036).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::optional_string;

/// What a watch waits for (WI-2036). `kind` picks the fields that apply:
///
/// - `pr`: `project` + `number`; fires when the PR reads one of `on`
///   (default merged or closed).
/// - `ci`: `project` + `sha` or `pr`; fires when every run on that revision
///   (a PR's head) — each named required check in `checks` (at least
///   one) — has been observed and concluded.
/// - `work_item`: `work_item`; fires when it is in one of `states` (default
///   a terminal state).
/// - `session`: `session`; fires when it ends (`until=ended`: exited,
///   hibernated or stopped) or also when it declares itself done
///   (`until=done`).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WatchTriggerModel {
    pub kind: WatchKind,
    /// Project slug (pr, ci).
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub project: Option<String>,
    /// PR number (pr).
    #[serde(default)]
    #[schemars(schema_with = "optional_ge1")]
    pub number: Option<i64>,
    /// PR states that fire the watch (pr).
    #[serde(default = "default_pr_events")]
    pub on: Vec<PrWatchEvent>,
    /// Revision (ci).
    #[serde(default)]
    #[schemars(schema_with = "optional_sha")]
    pub sha: Option<String>,
    /// PR whose head revision to watch (ci).
    #[serde(default)]
    #[schemars(schema_with = "optional_ge1")]
    pub pr: Option<i64>,
    /// Required check names (ci), at least one.
    #[serde(default)]
    #[schemars(length(max = 50))]
    pub checks: Vec<String>,
    /// WI ref (work_item).
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub work_item: Option<String>,
    /// States that fire the watch (work_item); default terminal.
    #[serde(default)]
    pub states: Vec<String>,
    /// Session to watch, ses_… or engine UUID (session).
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub session: Option<String>,
    /// What counts as finished (session).
    #[serde(default)]
    pub until: WatchUntil,
}

fn default_pr_events() -> Vec<PrWatchEvent> {
    vec![PrWatchEvent::Merged, PrWatchEvent::Closed]
}

fn optional_ge1(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "anyOf": [
            {"type": "integer", "minimum": 1},
            {"type": "null"}
        ]
    })
}

fn optional_sha(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "anyOf": [
            {"type": "string", "minLength": 7, "maxLength": 64},
            {"type": "null"}
        ]
    })
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WatchKind {
    Pr,
    Ci,
    WorkItem,
    Session,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PrWatchEvent {
    Merged,
    Closed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum WatchUntil {
    #[default]
    Ended,
    Done,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WatchStateView {
    Active,
    Fired,
    Failed,
    Cancelled,
    Expired,
}

/// Arm a watch: wake a session with the outcome when the trigger fires.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateWatchParams {
    pub trigger: WatchTriggerModel,
    /// The session to wake (ses_… or engine UUID). Default: your own session, from your token.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub session: Option<String>,
    /// Appended to the wake message, e.g. what to do next.
    #[serde(default)]
    #[schemars(length(max = 400))]
    pub note: String,
    /// How long the watch stays armed: default 24 h, at most 14 d.
    #[serde(default = "default_ttl")]
    #[schemars(range(min = 60, max = 1_209_600))]
    pub ttl_seconds: i64,
    /// Why this write is being made (audited).
    #[schemars(length(min = 1))]
    pub reason: String,
}

fn default_ttl() -> i64 {
    86_400
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CancelWatchParams {
    /// The watch id (wat_…).
    pub id: String,
    /// Why this write is being made (audited).
    #[schemars(length(min = 1))]
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListWatchesParams {
    #[serde(default)]
    pub state: Option<WatchStateView>,
    /// Only watches that wake this session.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub session: Option<String>,
    #[serde(default = "default_list_limit")]
    #[schemars(range(min = 1, max = 200))]
    pub limit: i64,
}

fn default_list_limit() -> i64 {
    50
}

/// A watch as callers read it.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WatchView {
    pub id: String,
    /// identity_ref of who armed it.
    pub owner: String,
    /// Engine id of the session it wakes.
    pub session: String,
    pub kind: WatchKind,
    #[schemars(schema_with = "string_map")]
    pub trigger: serde_json::Map<String, serde_json::Value>,
    /// The trigger in a few words.
    pub waits_for: String,
    #[serde(default)]
    pub note: String,
    pub state: WatchStateView,
    pub created_at: String,
    pub expires_at: String,
    #[serde(default)]
    pub fired_at: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "optional_object")]
    pub outcome: Option<serde_json::Map<String, serde_json::Value>>,
    /// Whether the wake message reached the session.
    #[serde(default)]
    pub delivered: Option<bool>,
    #[serde(default)]
    pub attempts: i64,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub last_error: Option<String>,
    pub reason: String,
}

fn string_map(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({"type": "object"})
}

fn optional_object(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({
        "anyOf": [{"type": "object"}, {"type": "null"}]
    })
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WatchResult {
    pub watch: WatchView,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListWatchesResult {
    #[serde(default)]
    #[schemars(extend("x-keep-default" = true))]
    pub watches: Vec<WatchView>,
}
