//! The record operations' parameter and result models: labels, the caller's
//! preferences, charters, workflows and the ranking explanation.
//!
//! Ports the `ListLabels*`, `CreateLabel*`, `Preference*`, `Workflow*` and
//! `Why*` classes in `src/vogt/application/models.py`.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::entities::{ContributionView, Label, PreferenceView, WorkflowView};
use super::{date_time, integer, number, open_object};

fn label_limit_default() -> i64 {
    100
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("properties" = {}))]
pub struct WorkflowListParams {}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkflowListResult {
    pub workflows: Vec<WorkflowView>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListLabelsParams {
    #[serde(default = "label_limit_default")]
    #[schemars(schema_with = "integer", range(min = 1, max = 500), extend("default" = 100))]
    pub limit: i64,
    #[serde(default)]
    #[schemars(schema_with = "integer", range(min = 0))]
    pub offset: i64,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LabelListResult {
    pub labels: Vec<Label>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateLabelParams {
    #[schemars(length(min = 1))]
    pub name: String,
    /// Hex colour, e.g. #d73a4a.
    #[serde(default)]
    pub color: Option<String>,
    #[schemars(length(min = 1))]
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LabelResult {
    pub label: Label,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PreferenceGetParams {
    /// One key, e.g. inbox.filter; omit for every key you hold.
    #[serde(default)]
    #[schemars(length(max = 64))]
    pub key: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PreferenceGetResult {
    pub preferences: Vec<PreferenceView>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PreferenceSetParams {
    /// Namespaced key, e.g. inbox.filter.
    #[schemars(
        length(min = 1, max = 64),
        pattern(r"^[a-z][a-z0-9_-]*(\.[a-z0-9_-]+)*$")
    )]
    pub key: String,
    /// A JSON object. {} clears the setting back to defaults. From the CLI pass it as JSON text.
    #[schemars(schema_with = "open_object")]
    pub value: serde_json::Map<String, serde_json::Value>,
    /// Apply only if the stored version is this one (0: only if the key has never been written). Omit to write unconditionally.
    #[serde(default)]
    #[schemars(range(min = 0))]
    pub expected_version: Option<i64>,
    /// Why this write is being made (audited).
    #[schemars(length(min = 1))]
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PreferenceSetResult {
    pub preference: PreferenceView,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WhyParams {
    #[serde(rename = "ref")]
    pub reference: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WhyResult {
    #[serde(rename = "ref")]
    pub reference: String,
    pub title: String,
    #[schemars(schema_with = "number")]
    pub total: f64,
    pub contributions: Vec<ContributionView>,
    /// Documented ranking inputs that cannot fire in this build.
    pub inputs_not_yet_available: BTreeMap<String, String>,
}

// -- charters (WI-2038) ---------------------------------------------------

fn charter_limit_default() -> i64 {
    20
}

// `CharterScopeKind` in Python: a literal, so the schema is inline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(inline)]
pub enum CharterScopeKind {
    #[serde(rename = "workspace")]
    Workspace,
    #[serde(rename = "project")]
    Project,
    #[serde(rename = "initiative")]
    Initiative,
}

impl CharterScopeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Workspace => "workspace",
            Self::Project => "project",
            Self::Initiative => "initiative",
        }
    }
}

/// One version of a scope's charter.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CharterView {
    pub scope: CharterScopeKind,
    /// Project or initiative slug; none for workspace.
    #[serde(default, rename = "ref")]
    pub reference: Option<String>,
    #[schemars(schema_with = "integer")]
    pub version: i64,
    pub body: String,
    /// identity_ref of whoever set this version.
    pub author: String,
    #[schemars(schema_with = "date_time")]
    pub created_at: String,
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CharterGetParams {
    /// workspace, project or initiative.
    pub scope: CharterScopeKind,
    /// Project or initiative slug; omitted for workspace.
    #[serde(default, rename = "ref")]
    pub reference: Option<String>,
    /// Also return earlier versions, newest first.
    #[serde(default)]
    pub history: bool,
    #[serde(default = "charter_limit_default")]
    #[schemars(schema_with = "integer", range(min = 1, max = 200), extend("default" = 20))]
    pub limit: i64,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CharterGetResult {
    /// The newest version, or none when never set. An empty body is a cleared charter.
    #[serde(default)]
    pub current: Option<CharterView>,
    #[serde(default)]
    #[schemars(extend("default" = [], "x-keep-default" = true))]
    pub history: Vec<CharterView>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CharterSetParams {
    /// workspace, project or initiative.
    pub scope: CharterScopeKind,
    /// Project or initiative slug; omitted for workspace.
    #[serde(default, rename = "ref")]
    pub reference: Option<String>,
    /// The standing instructions, in Markdown. Empty clears it.
    #[schemars(length(max = 8000))]
    pub body: String,
    /// Write only if the current version is this (0: never set).
    #[serde(default)]
    #[schemars(range(min = 0))]
    pub expected_version: Option<i64>,
    /// Why this write is being made (audited).
    #[schemars(length(min = 1))]
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CharterSetResult {
    pub charter: CharterView,
}
