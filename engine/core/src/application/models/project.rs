//! The contract, scaffold and project operations' parameter and result models.
//!
//! Ports the `ContractAdopt*`, `ScaffoldProject*` classes in
//! `src/vogt/application/models.py`. `contract.decline` shares the adopt
//! models, as it does in Python.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::entities::{default_true, lifecycle_active, AgentPolicy, LifecycleState, Project};
use super::forge::OnboardResult;
use super::optional_date_time;

/// Opt a registered project into the contract.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContractAdoptParams {
    /// A registered project slug.
    pub project: String,
    #[schemars(length(min = 1))]
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContractAdoptResult {
    pub project: String,
    pub adopted: bool,
    #[schemars(schema_with = "optional_date_time")]
    #[schemars(required)]
    pub adopted_at: Option<String>,
    /// What compliance reports for this project after the change.
    pub status: String,
    pub detail: String,
}

/// Lay the contract's skeleton into an already-registered project.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScaffoldProjectParams {
    /// A registered project slug.
    pub project: String,
    #[schemars(length(min = 1))]
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScaffoldProjectResult {
    pub project: String,
    pub root_path: String,
    /// Paths written, relative to the project's root.
    pub created: Vec<String>,
    /// Paths already present and therefore left exactly as they were.
    pub skipped: Vec<String>,
    pub detail: String,
}

fn lifecycle_incubating() -> LifecycleState {
    LifecycleState::Incubating
}

fn list_limit() -> i64 {
    50
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RegisterProjectParams {
    /// Display name; the slug is derived from it.
    #[schemars(length(min = 1))]
    pub name: String,
    /// Folder or git repository this project is.
    pub root_path: String,
    /// Optional remote the project is published at.
    #[serde(default)]
    pub repo_url: Option<String>,
    /// incubating / active / maintenance / archived.
    #[serde(default = "lifecycle_active")]
    pub lifecycle_state: LifecycleState,
    /// Paths collection skips, replacing the defaults entirely rather than adding to them. Omit for the defaults. A repository that vendors a corpus or carries agent worktrees needs this at registration, because the first sweep is the baseline every later answer is compared against.
    #[serde(default)]
    pub exclusions: Option<Vec<String>>,
    /// Why this write is being made (audited).
    #[schemars(length(min = 1))]
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectResult {
    pub project: Project,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateProjectParams {
    /// Display name; the slug is derived from it.
    #[schemars(length(min = 1))]
    pub name: String,
    /// Directory to scaffold into. Created if absent; existing files are never overwritten.
    pub root_path: String,
    /// Recorded in the scaffold. Defaults to the acting principal.
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub repo_url: Option<String>,
    #[serde(default = "lifecycle_incubating")]
    pub lifecycle_state: LifecycleState,
    /// Why this write is being made (audited).
    #[schemars(length(min = 1))]
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateProjectResult {
    pub project: Project,
    /// Files and directories written. Existing ones are left alone.
    pub created_paths: Vec<String>,
    /// Contract paths that already existed and were not touched.
    pub skipped_paths: Vec<String>,
}

/// Bring a repository that lives on a configured forge into Vogt.
///
/// `repo` is named by the caller and is never chosen from a list: there is
/// no listing operation, and adding one would be the registration-candidate
/// listing that was deliberately removed.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportProjectParams {
    /// Repository to import: `owner/name`, `host/owner/name`, or an HTTPS/SSH repository URL for GitHub or a configured Forgejo host.
    #[schemars(length(min = 1))]
    pub repo: String,
    /// Display name. Defaults to the repository's own name.
    #[serde(default)]
    pub name: Option<String>,
    /// Where to clone to. Defaults to `<import_root>/<slug>`; supply this only when one repository must live somewhere specific.
    #[serde(default)]
    pub root_path: Option<String>,
    #[serde(default = "lifecycle_active")]
    pub lifecycle_state: LifecycleState,
    /// Read existing issues, PRs, labels and releases after registering. Read-only, and on by default because a project that arrives empty looks like a project with no work.
    #[serde(default = "default_true")]
    pub consolidate: bool,
    /// Why this write is being made (audited).
    #[schemars(length(min = 1))]
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportProjectResult {
    pub project: Project,
    /// The remote that was cloned, without credentials.
    pub remote: String,
    pub root_path: String,
    /// HEAD at the moment of import.
    #[serde(default)]
    pub revision: Option<String>,
    #[serde(default)]
    pub default_branch: Option<String>,
    /// False when the destination already held a clone of the same remote and was registered as it stood.
    #[serde(default = "default_true")]
    pub cloned: bool,
    /// What the read-only consolidation found, if it ran.
    #[serde(default)]
    pub consolidated: Option<OnboardResult>,
    #[serde(default)]
    pub detail: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetProjectParams {
    /// Project slug.
    pub slug: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListProjectsParams {
    #[serde(default = "list_limit")]
    #[schemars(range(min = 1, max = 500))]
    pub limit: i64,
    #[serde(default)]
    #[schemars(range(min = 0))]
    pub offset: i64,
}

/// A project, plus whether `work.create` would land there right now.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[schemars(extend("additionalProperties" = false))]
pub struct ProjectListing {
    #[serde(flatten)]
    pub project: Project,
    /// Whether a default `work.create` (no `local_only`) succeeds on this project now: it is forge-linked, its write-back policy permits `create`, a forge credential resolves and its repo_url parses. False means the create refuses with `project_not_linked` or `upstream_write_refused`; `writable_reason` says which and how to fix it.
    pub writable: bool,
    /// Why `writable` is what it is, naming the way forward.
    pub writable_reason: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectListResult {
    pub projects: Vec<ProjectListing>,
    pub total: i64,
}

/// Correct a project's declaration after registration.
///
/// Deliberately narrow. `lifecycle_state` is absent because it has its own
/// operation with a validated edge (`project transition`), and the observed
/// fields are absent because nothing declares them. What is left is what a
/// registration can get wrong and nothing else could fix — where the project
/// is published, what collection should skip — and the agent policy, which is
/// data the project states rather than something derived.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateProjectParams {
    /// Project to update.
    pub slug: String,
    /// Leave unset to keep the current value.
    #[serde(default)]
    pub repo_url: Option<String>,
    /// Replaces the current list entirely. Leave unset to keep it; pass an empty list to collect everything.
    #[serde(default)]
    pub exclusions: Option<Vec<String>>,
    /// Replaces the project's agent policy entirely. Leave unset to keep it. `forbidden_text` entries are regexes and must compile, and so must `landing.branch_pattern` once `{n}` stands for a number; `landing.merge_mode` is `fast_forward` or `rebase` — `merge` is refused. A pattern that does not compile is refused rather than stored.
    #[serde(default)]
    pub agent_policy: Option<AgentPolicy>,
    /// Why this write is being made (audited).
    #[schemars(length(min = 1))]
    pub reason: String,
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TransitionProjectParams {
    pub slug: String,
    /// Target lifecycle state.
    pub to_state: LifecycleState,
    #[schemars(length(min = 1))]
    pub reason: String,
}
