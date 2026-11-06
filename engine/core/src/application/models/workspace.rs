//! Workspace operations' models (WI-1340).
//!
//! Ported from `src/vogt/application/models.py`; each schema is checked against
//! Python's live registry by `tests/test_rust_schemas.py`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::optional_string;

#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceListParams {
    /// Only workspaces of this project's sessions (slug).
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub project: Option<String>,
    /// true: only running sessions; false: only stopped or hibernated.
    #[serde(default)]
    pub alive: Option<bool>,
}

/// One session's working tree (WI-1340).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceRow {
    pub path: String,
    /// `worktree` or `checkout`.
    pub kind: String,
    pub branch: String,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub base_sha: Option<String>,
    /// Bytes under `path`; null until first measured.
    #[serde(default)]
    pub size_bytes: Option<i64>,
    /// When `workspace.clean` last dropped its build output.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub cleaned_at: Option<String>,
    pub alive: bool,
    /// The branch tip is in the project's default branch (git-local collector); null when the collector has not observed the branch.
    #[serde(default)]
    pub merged: Option<bool>,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub work_item: Option<String>,
    /// `ses_…` id, or the engine UUID when unlinked.
    pub session: String,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub project: Option<String>,
}

#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceListResult {
    #[serde(default)]
    #[schemars(extend("x-keep-default" = true))]
    pub workspaces: Vec<WorkspaceRow>,
}
