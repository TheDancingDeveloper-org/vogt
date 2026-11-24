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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[schemars(inline)]
pub enum WorkspaceCleanParamsMode {
    #[serde(rename = "build_output")]
    BuildOutput,
    #[serde(rename = "remove")]
    Remove,
}

impl WorkspaceCleanParamsMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BuildOutput => "build_output",
            Self::Remove => "remove",
        }
    }
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceCleanParams {
    /// The session whose workspace to clean (`ses_…` or engine UUID).
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub id: Option<String>,
    /// The workspace path, instead of `id`.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub path: Option<String>,
    /// `build_output`: drop target/, node_modules, .venv. `remove`: `git worktree remove` it, deleting the branch when merged.
    pub mode: WorkspaceCleanParamsMode,
    /// Remove a worktree whose branch is unmerged (the branch is kept). A person only. Never removes a dirty tree or one a session uses.
    #[serde(default)]
    pub force: bool,
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceCleanResult {
    pub path: String,
    pub mode: String,
    pub bytes_freed: i64,
    #[serde(default)]
    pub removed: bool,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub branch_deleted: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub cleaned_at: Option<String>,
}
