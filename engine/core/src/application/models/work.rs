//! Work-item, board, initiative and project-health operations' models.
//!
//! Ported from `src/vogt/application/models.py`; each schema is checked against
//! Python's live registry by `tests/test_rust_schemas.py`.

use schemars::{json_schema, JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Serialize};

use super::entities::{Freshness, Project};
use super::views::{
    AdoptParamsKind, AdoptParamsPriority, AdoptResultInferredKind, AdoptResultInferredPriority,
    BacklogParamsKindsItem, BacklogParamsMode, BacklogParamsPrioritiesItem,
    BacklogParamsTrustStatesItem, BacklogResultLinkState, BoardCellParams, BoardCellResult,
    BoardListParamsKindsItem, BoardListParamsLaneMode, BoardListParamsPrioritiesItem,
    BoardListResultLinkState, BugsParamsPrioritiesItem, CiSummary, Comment, CommentParamsKind,
    ContractExemptionView, CoverageEntry, CreateInitiativeParamsState, CreateWorkParamsEffort,
    CreateWorkParamsKind, CreateWorkParamsPriority, CriterionView, DepRef, DependencySummary,
    DepsResultStatus, DriftProposal, DriftResolveParamsResolution, EngineDiskStatus,
    GetWorkParamsCommentOrder, GetWorkParamsMode, HandoffFields, Initiative,
    InitiativeTrackingIssue, ListInitiativesParamsMode, ListWorkParamsKindsItem,
    ListWorkParamsMode, ListWorkParamsPrioritiesItem, MirroredSource, Observation,
    ProjectBriefParamsMode, PublishInitiativeResultState, RankedItem, RecommendationView,
    RelateWorkParamsKind, SessionSummary, SweepReportView, UnrelateWorkParamsKind,
    UpdateInitiativeParamsState, UpdateWorkParamsEffort, UpdateWorkParamsPriority, WorkItem,
    WorkItemBranchView, WorkItemGitStory, WorkItemRow, WorkListResultItems,
    WorkListResultLinkState, WorkListResultMode,
};
use super::{date_time, optional_date_time, optional_string};

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectBriefParams {
    /// Project slug. Aliases: `project`, `id`.
    pub slug: String,
    #[serde(default = "default_project_brief_params_backlog_limit")]
    #[schemars(range(min = 1, max = 100))]
    pub backlog_limit: i64,
    /// `summary` (default) ranks `top_backlog` without each row's full work item (`item` is null); `full` includes it, bodies and all.
    #[serde(default = "default_project_brief_params_mode")]
    pub mode: ProjectBriefParamsMode,
}
/// The per-repo view in one call.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ProjectBriefResult {
    pub project: Project,
    /// Outstanding work across both populations, declared and observed — the same set `backlog --project` ranks. Split below, because a total that does not name its populations is not an answer.
    pub open_work: i64,
    pub open_bugs: i64,
    /// Of `open_work`, how much somebody typed in.
    #[serde(default)]
    pub declared_work: i64,
    /// Of `open_work`, how much a collector found — chiefly what `forge onboard` consolidated. Zero here with a non-zero `declared_work` is a project nobody has collected for; the reverse is a project nobody has declared work on.
    #[serde(default)]
    pub observed_work: i64,
    /// Declared work items only, terminal states included. Observed subjects have no workflow state to count (ARCHITECTURE.md).
    pub by_state: std::collections::BTreeMap<String, i64>,
    /// Declared work items only, as above.
    pub by_kind: std::collections::BTreeMap<String, i64>,
    pub top_backlog: Vec<RankedItem>,
    #[schemars(required)]
    #[schemars(schema_with = "optional_project_brief_result_current_version")]
    pub current_version: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub declared_version: Option<String>,
    /// Newest tag or release seen by a collector.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub observed_version: Option<String>,
    /// Whether declared and observed versions agree. Null when either is unknown; a disagreement feeds the version-mismatch drift kind.
    #[serde(default)]
    pub version_matches: Option<bool>,
    pub compliance_status: String,
    #[schemars(schema_with = "optional_date_time")]
    #[schemars(required)]
    pub compliance_checked_at: Option<String>,
    pub ci_status: CiSummary,
    pub dependencies: DependencySummary,
    pub freshness: Freshness,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateWorkParams {
    /// feature / bug / chore / question.
    pub kind: CreateWorkParamsKind,
    #[schemars(length(min = 1))]
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default = "default_create_work_params_priority")]
    pub priority: CreateWorkParamsPriority,
    #[serde(default)]
    pub effort: Option<CreateWorkParamsEffort>,
    /// Project slug.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub project: Option<String>,
    /// Initiative slug.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub initiative: Option<String>,
    /// Actor identity_ref, e.g. local:sprooty.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub assignee: Option<String>,
    /// Existing label names to attach.
    #[serde(default)]
    pub labels: Option<Vec<String>>,
    /// Create the item locally without writing it upstream. On a linked project a create normally goes through to the forge, and a write-back policy that does not permit 'create' refuses it; pass this to opt in to a native, local-only item instead — it carries no upstream subject yet and is exactly what a later `forge link` or `forge writeback` migrates upstream. Default off: the refusal stays the default so nothing is created locally by surprise.
    #[serde(default)]
    pub local_only: bool,
    #[schemars(length(min = 1))]
    pub reason: String,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkResult {
    pub item: WorkItem,
    #[serde(default)]
    #[schemars(extend("x-keep-default" = true))]
    pub comments: Vec<Comment>,
    #[serde(default)]
    #[schemars(extend("x-keep-default" = true))]
    pub sessions: Vec<SessionSummary>,
    #[serde(default)]
    #[schemars(extend("x-keep-default" = true))]
    pub branches: Vec<WorkItemBranchView>,
    #[serde(default)]
    pub git: Option<WorkItemGitStory>,
    #[serde(default)]
    #[schemars(extend("x-keep-default" = true))]
    pub walked: Vec<String>,
    #[serde(default)]
    #[schemars(extend("x-keep-default" = true))]
    pub live_sessions: Vec<SessionSummary>,
    #[serde(default)]
    pub claim_check: Option<ClaimCheck>,
}

/// A pull request a completion claim was checked against (WI-1835).
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClaimPull {
    pub number: i64,
    pub state: String,
    #[serde(default)]
    #[schemars(schema_with = "optional_date_time")]
    pub merged_at: Option<String>,
    /// Who merged it, when the forge observation says.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub merged_by: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub checks: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub head_ref: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema)]
#[schemars(inline)]
pub enum ClaimKind {
    #[serde(rename = "autopilot-done")]
    AutopilotDone,
    #[serde(rename = "review")]
    Review,
    #[serde(rename = "done")]
    Done,
}

/// A session's completion claim checked against git and the forge (WI-1835).
///
/// Recorded when a session says `AUTOPILOT: DONE` or moves its item to
/// review or done. `mismatches` lists, in words, where the claim and the
/// facts disagree; `unchecked` lists the facts that could not be read, so
/// an empty `mismatches` is never mistaken for verified.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClaimCheck {
    pub claim: ClaimKind,
    /// The session's Vogt id.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub session: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub work_item: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "optional_date_time")]
    pub session_started_at: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub branch: Option<String>,
    /// The worktree's HEAD commit.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub head: Option<String>,
    /// Whether HEAD is on the branch's upstream.
    #[serde(default)]
    pub pushed: Option<bool>,
    /// Commits on HEAD committed since the session started.
    #[serde(default)]
    pub new_commits: Option<i64>,
    #[serde(default)]
    #[schemars(extend("x-keep-default" = true))]
    pub pulls: Vec<ClaimPull>,
    #[serde(default)]
    #[schemars(extend("x-keep-default" = true))]
    pub mismatches: Vec<String>,
    #[serde(default)]
    #[schemars(extend("x-keep-default" = true))]
    pub unchecked: Vec<String>,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetWorkParams {
    /// Work item reference, e.g. WI-7. Alias: `id`.
    pub r#ref: String,
    #[serde(default = "default_get_work_params_comment_limit")]
    #[schemars(range(min = 0, max = 500))]
    pub comment_limit: i64,
    /// `summary` omits the item body and comments; `full` (default) keeps them.
    #[serde(default = "default_get_work_params_mode")]
    pub mode: GetWorkParamsMode,
    /// Which end of the comment thread `comment_limit` keeps. `newest` (default) returns the newest comments, newest first; `oldest` returns the oldest, oldest first.
    #[serde(default = "default_get_work_params_comment_order")]
    pub comment_order: GetWorkParamsCommentOrder,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GetManyWorkParams {
    /// Work item references to read, e.g. [WI-7, WI-9]; a subject key resolves as it does for `work.get`. A repeated ref is read once.
    #[schemars(extend("minItems" = 1))]
    #[schemars(extend("maxItems" = 200))]
    pub refs: Vec<String>,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkManyResult {
    /// One compact row per ref that names a work item, in the order asked; `work.get` with a ref returns the rest of it.
    pub items: Vec<WorkItemRow>,
    /// Refs that name no work item, in the order asked.
    #[serde(default)]
    pub missing: Vec<String>,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListWorkParams {
    /// Project slug.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub project: Option<String>,
    #[serde(default)]
    pub kinds: Option<Vec<ListWorkParamsKindsItem>>,
    /// Workflow states to include, e.g. [open, in_progress]. Aliases: `status`, `state` (a single state or a comma-separated string is accepted). Naming a terminal state (done, wont_do) includes finished items without needing `include_finished`.
    #[serde(default)]
    pub states: Option<Vec<String>>,
    /// Case-insensitive text matched against the title, body and ref. Aliases: `text`, `search`, `q`.
    #[serde(default)]
    #[schemars(schema_with = "optional_list_work_params_query")]
    pub query: Option<String>,
    #[serde(default)]
    pub priorities: Option<Vec<ListWorkParamsPrioritiesItem>>,
    /// Actor identity_ref.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub assignee: Option<String>,
    /// Initiative slug.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub initiative: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub label: Option<String>,
    /// Include done and wont_do items.
    #[serde(default)]
    pub include_finished: bool,
    /// `summary` (default) returns compact rows — ref, title, kind, state, priority, project — so a long page fits one tool result; `full` returns every field, bodies included.
    #[serde(default = "default_list_work_params_mode")]
    pub mode: ListWorkParamsMode,
    #[serde(default = "default_list_work_params_limit")]
    #[schemars(range(min = 1, max = 500))]
    pub limit: i64,
    #[serde(default)]
    #[schemars(range(min = 0))]
    pub offset: i64,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkListResult {
    /// Compact `WorkItemRow`s in `summary` mode, whole work items in `full` mode.
    pub items: WorkListResultItems,
    pub total: i64,
    #[serde(default = "default_work_list_result_mode")]
    pub mode: WorkListResultMode,
    /// The `offset` of the next page; null on the last page.
    #[serde(default)]
    pub next_offset: Option<i64>,
    /// Set when the list is scoped to one project: `unlinked` is the machine-readable marker that this project has no work surface yet — the items are empty because linking or publishing is the way forward, not because there is nothing to do. Global lists carry null.
    #[serde(default)]
    pub link_state: Option<WorkListResultLinkState>,
    /// On an `unlinked` project with open native items: how many there are, a few refs, and that they still take comments and transitions by ref. Null otherwise.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub detail: Option<String>,
}
/// One bounded, server-owned batch of independently pageable cells.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BoardListParams {
    /// Project slug.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub project: Option<String>,
    #[serde(default)]
    pub kinds: Option<Vec<BoardListParamsKindsItem>>,
    #[serde(default)]
    pub states: Option<Vec<String>>,
    #[serde(default)]
    pub priorities: Option<Vec<BoardListParamsPrioritiesItem>>,
    /// Actor identity_ref.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub assignee: Option<String>,
    /// Initiative slug.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub initiative: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub label: Option<String>,
    #[serde(default = "default_board_list_params_lane_mode")]
    pub lane_mode: BoardListParamsLaneMode,
    #[schemars(extend("minItems" = 1))]
    #[schemars(extend("maxItems" = 40))]
    pub cells: Vec<BoardCellParams>,
    #[serde(default = "default_board_list_params_page_size")]
    #[schemars(range(min = 1, max = 100))]
    pub page_size: i64,
    /// Opaque snapshot returned by the first batch. Required when adding another cell or continuing one against that same Board view.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub snapshot: Option<String>,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BoardListResult {
    pub cells: Vec<BoardCellResult>,
    pub column_totals: std::collections::BTreeMap<String, i64>,
    pub lane_totals: std::collections::BTreeMap<String, i64>,
    pub total: i64,
    /// How many things the Backlog would consider for this same scope : declared work plus open forge subjects that are not yet tracked as work items. The Board draws only the declared cards, so this is almost always larger, and a surface that shows `total` without it silently reads a small Board as the size of the estate. Computed the observed-inclusive way the Backlog computes it, over the same project/kind/priority/assignee/initiative/label filters.
    #[serde(default)]
    pub backlog_candidates: i64,
    /// The declared-only slice of `backlog_candidates` — non-terminal declared work in this scope, the same population the rail counts. `backlog_candidates - declared_total` is the observed subjects the Board is currently silent about.
    #[serde(default)]
    pub declared_total: i64,
    /// Set for a project scope: `unlinked` marks a project with no Board — the cells are empty because linking or publishing is the way forward. The global Board carries null.
    #[serde(default)]
    pub link_state: Option<BoardListResultLinkState>,
    /// Declared rows this filter would have drawn whose project is unlinked, excluded by the forge-less withdrawal and reported so the Board's totals stay honest about what they leave out; on an unlinked project scope it is the count of open native items a link or publish would migrate.
    #[serde(default)]
    pub excluded_unlinked: i64,
    pub snapshot: String,
    #[schemars(schema_with = "date_time")]
    pub snapshot_at: String,
    pub revision: i64,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateWorkParams {
    pub r#ref: String,
    #[serde(default)]
    #[schemars(schema_with = "optional_update_work_params_title")]
    pub title: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub body: Option<String>,
    #[serde(default)]
    pub priority: Option<UpdateWorkParamsPriority>,
    #[serde(default)]
    pub effort: Option<UpdateWorkParamsEffort>,
    /// Project slug.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub project: Option<String>,
    /// Initiative slug.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub initiative: Option<String>,
    /// Actor identity_ref.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub assignee: Option<String>,
    #[serde(default)]
    pub clear_effort: bool,
    #[serde(default)]
    pub clear_assignee: bool,
    #[serde(default)]
    pub clear_initiative: bool,
    #[serde(default)]
    pub add_labels: Option<Vec<String>>,
    #[serde(default)]
    pub remove_labels: Option<Vec<String>>,
    #[schemars(length(min = 1))]
    pub reason: String,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TransitionWorkParams {
    /// Work item reference, e.g. WI-7. Alias: `id`.
    pub r#ref: String,
    /// Target state, e.g. in_progress. Aliases: `to`, `state`.
    pub to_state: String,
    /// Why the item is moving (audited, required on every write). With `walk`, each hop records this reason annotated with the hop.
    #[schemars(length(min = 1))]
    pub reason: String,
    /// Walk the shortest path of valid edges to `to_state`, one audited transition per hop (e.g. open -> in_progress -> review -> done). Only the workflow's own edges are taken and finished states are never passed through, so no review step is skipped.
    #[serde(default)]
    pub walk: bool,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RelateWorkParams {
    pub r#ref: String,
    /// depends_on / relates_to / duplicate_of / parent_of. (implemented_by is observed from pull requests, not declarable.)
    pub kind: RelateWorkParamsKind,
    /// The other work item's reference.
    pub target: String,
    #[schemars(length(min = 1))]
    pub reason: String,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UnrelateWorkParams {
    pub r#ref: String,
    pub kind: UnrelateWorkParamsKind,
    pub target: String,
    #[schemars(length(min = 1))]
    pub reason: String,
}
/// Declare the git branch a work item is worked on (the declared path).
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BindBranchParams {
    /// Work item reference, e.g. WI-7.
    pub r#ref: String,
    /// The branch name to declare. Defaults from `branch_binding_template` when omitted (`WI-7` becomes `wi-7`, an upstream item its `gh-<n>` form). Declaring a branch already on the item is a no-op, not an error. Additive and forward-only: this records a name, it never creates, renames or deletes a branch in git.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub branch: Option<String>,
    #[schemars(length(min = 1))]
    pub reason: String,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CommentParams {
    pub r#ref: String,
    /// The comment text.
    #[schemars(length(min = 1))]
    pub body: String,
    /// `note` (default) is an ordinary remark. `handoff` is the structured hand-off a fresh session starts from, and requires `handoff`.
    #[serde(default = "default_comment_params_kind")]
    pub kind: CommentParamsKind,
    /// The structured fields of a hand-off: branch, head_sha, base, done, remaining, touch_points, gates_run, blockers, session. Required when kind is `handoff`, refused on a note.
    #[serde(default)]
    pub handoff: Option<HandoffFields>,
    #[schemars(length(min = 1))]
    pub reason: String,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CommentResult {
    pub comment: Comment,
    /// What happened upstream: skipped (the usual case), succeeded, or failed. A failure never fails the local write.
    #[serde(default = "default_comment_result_write_back")]
    pub write_back: String,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BacklogParams {
    /// Project slug; omit for the global backlog.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub project: Option<String>,
    #[serde(default)]
    pub kinds: Option<Vec<BacklogParamsKindsItem>>,
    #[serde(default)]
    pub priorities: Option<Vec<BacklogParamsPrioritiesItem>>,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub assignee: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub initiative: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub label: Option<String>,
    #[serde(default)]
    pub trust_states: Option<Vec<BacklogParamsTrustStatesItem>>,
    /// Whether open pull requests rank in the backlog. On by default: a synced PR is worklike, and now that closure is observed a merged one self-heals out rather than lingering. Set false to see issues and markers only — a view choice, not a data one; the PRs stay queryable through `observations list`.
    #[serde(default = "default_backlog_params_include_prs")]
    pub include_prs: bool,
    /// `summary` (default) returns each ranked row without its full work item (`item` is null; ref, title, kind, state, priority and score remain); `full` includes it, bodies and all.
    #[serde(default = "default_backlog_params_mode")]
    pub mode: BacklogParamsMode,
    #[serde(default = "default_backlog_params_limit")]
    #[schemars(range(min = 1, max = 200))]
    pub limit: i64,
    /// Ranked rows to skip. `total_considered` is the full count, so a caller knows whether another page exists. Ranking is recomputed per request and staleness grows with the clock, so two pages fetched far enough apart can repeat or skip an item near a score boundary — the same caveat `audit.list` carries, for the same reason.
    #[serde(default)]
    #[schemars(range(min = 0))]
    pub offset: i64,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BacklogResult {
    pub items: Vec<RankedItem>,
    pub total_considered: i64,
    /// The `offset` of the next page; null on the last page.
    #[serde(default)]
    pub next_offset: Option<i64>,
    #[serde(default)]
    pub declared: i64,
    #[serde(default)]
    pub observed: i64,
    #[serde(default)]
    pub suppressed: i64,
    /// Observed subjects left out because their source says they are closed. Reported rather than silently dropped, so a short list is distinguishable from a filtered one — they remain queryable through `observations list`.
    #[serde(default)]
    pub closed_upstream: i64,
    /// Set for a project scope: `unlinked` marks a project with no work surface — link or publish it to track work upstream. The global view carries null.
    #[serde(default)]
    pub link_state: Option<BacklogResultLinkState>,
    /// Native declared items left out because their project is unlinked (the withdrawal of the forge-less work layer). Reported rather than silently dropped, so the arithmetic over `declared` stays honest; on an unlinked project scope it is the count of open native items a link or publish would migrate.
    #[serde(default)]
    pub excluded_unlinked: i64,
    pub scope: String,
    pub freshness: Freshness,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BugsParams {
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub project: Option<String>,
    #[serde(default)]
    pub priorities: Option<Vec<BugsParamsPrioritiesItem>>,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub assignee: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub label: Option<String>,
    #[serde(default = "default_bugs_params_limit")]
    #[schemars(range(min = 1, max = 200))]
    pub limit: i64,
    /// Ranked rows to skip. `total_considered` is the full count, so a caller knows whether another page exists. Ranking is recomputed per request and staleness grows with the clock, so two pages fetched far enough apart can repeat or skip an item near a score boundary — the same caveat `audit.list` carries, for the same reason.
    #[serde(default)]
    #[schemars(range(min = 0))]
    pub offset: i64,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateInitiativeParams {
    #[schemars(length(min = 1))]
    pub title: String,
    #[serde(default)]
    pub body: String,
    /// Feeds ranking; 0 means no lift.
    #[serde(default)]
    #[schemars(range(min = 0, max = 100))]
    pub weight: i64,
    #[serde(default = "default_create_initiative_params_state")]
    pub state: CreateInitiativeParamsState,
    #[schemars(length(min = 1))]
    pub reason: String,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InitiativeResult {
    pub initiative: Initiative,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ListInitiativesParams {
    #[serde(default = "default_list_initiatives_params_limit")]
    #[schemars(range(min = 1, max = 500))]
    pub limit: i64,
    #[serde(default)]
    #[schemars(range(min = 0))]
    pub offset: i64,
    /// `summary` omits each initiative's body; `full` (default) keeps it.
    #[serde(default = "default_list_initiatives_params_mode")]
    pub mode: ListInitiativesParamsMode,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InitiativeListResult {
    pub initiatives: Vec<Initiative>,
}
/// Correct an initiative after creation, or close and reopen it.
///
/// The slug is its identity — forge tracking issues carry the
/// ``initiative:<slug>`` label — so a new title leaves the slug as it was.
/// Every field left unset keeps its current value.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateInitiativeParams {
    /// Initiative to update.
    pub slug: String,
    #[serde(default)]
    #[schemars(schema_with = "optional_update_initiative_params_title")]
    pub title: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub body: Option<String>,
    /// Feeds ranking; 0 means no lift.
    #[serde(default)]
    #[schemars(schema_with = "optional_update_initiative_params_weight")]
    pub weight: Option<i64>,
    /// `closed` ends it; `initiative publish` then proposes closing its tracking issues.
    #[serde(default)]
    pub state: Option<UpdateInitiativeParamsState>,
    #[schemars(length(min = 1))]
    pub reason: String,
}
/// Project an initiative onto a forge tracking issue per linked repo.
///
/// Additive and forward-only: for each forge-linked project the initiative
/// spans, Vogt creates or adopts one tracking issue labelled
/// ``initiative:<slug>`` and re-renders its managed task list. Nothing is ever
/// closed or deleted — a closed initiative *proposes* its tracking issues be
/// closed (drift), it never writes the close.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PublishInitiativeParams {
    /// Initiative slug to publish.
    pub slug: String,
    #[schemars(length(min = 1))]
    pub reason: String,
}
/// What `initiative.publish` did, one row per repo the initiative spans.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PublishInitiativeResult {
    pub slug: String,
    pub state: PublishInitiativeResultState,
    #[serde(default)]
    #[schemars(extend("x-keep-default" = true))]
    pub tracking_issues: Vec<InitiativeTrackingIssue>,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SweepParams {
    /// Narrow the sweep to one project. Scope is never widened.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub project: Option<String>,
    /// Collector names; omit to run all of them.
    #[serde(default)]
    pub collectors: Option<Vec<String>>,
    /// Skip every collector that needs the network (NFR-PO2).
    #[serde(default)]
    pub offline_only: bool,
    #[schemars(length(min = 1))]
    pub reason: String,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SweepResult {
    pub scope: String,
    pub projects: i64,
    pub subjects: i64,
    pub dep_refs: i64,
    pub reports: Vec<SweepReportView>,
    /// The workspace volume, when a session engine answers. needs_attention once used space passes its threshold.
    #[serde(default)]
    pub disk: Option<EngineDiskStatus>,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("properties" = {}))]
pub struct CoverageParams {}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CoverageResult {
    pub collectors: Vec<CoverageEntry>,
    pub swept_project_ids: Vec<String>,
    /// Registered projects no collector has ever swept. A registered project nothing has looked at has no evidence behind anything it claims, and is the case the not-collected rule exists to keep visible.
    #[serde(default)]
    pub unswept_project_ids: Vec<String>,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ObservationsParams {
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub project: Option<String>,
    /// Observation kind, e.g. marker or forge.issue.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub kind: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub subject_key: Option<String>,
    #[serde(default)]
    pub promoted_only: bool,
    /// Newest per subject. Set false for the full history.
    #[serde(default = "default_observations_params_latest_only")]
    pub latest_only: bool,
    #[serde(default = "default_observations_params_limit")]
    #[schemars(range(min = 1, max = 1000))]
    pub limit: i64,
    #[serde(default)]
    #[schemars(range(min = 0))]
    pub offset: i64,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ObservationsResult {
    pub observations: Vec<Observation>,
    /// How many rows this page holds — not how many exist. The store is queried a page at a time and no count is taken behind it, so `total == limit` means 'there may be more', never 'that is all'.
    pub total: i64,
    /// Why an empty answer is empty, where that is not 'there are none'. An unswept instance has no evidence tables at all, and returning `[]` for that reads as a collector that found nothing .
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub detail: Option<String>,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DepsParams {
    /// Project slug.
    pub project: String,
}
/// The dependency graph around one project.
///
/// Carries `freshness` because a graph is an aggregate over sweeps, and an
/// empty graph with no sweep behind it means "not collected", not "this
/// project depends on nothing". `status` narrows the same
/// point to this project: estate-wide freshness cannot say whether the
/// dependency collector has ever walked *this* tree.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DepsResult {
    pub project: String,
    pub references_out: Vec<DepRef>,
    pub referenced_by: Vec<DepRef>,
    #[serde(default)]
    pub unresolved: i64,
    /// Copies this project carries of other projects' source.
    #[serde(default)]
    #[schemars(extend("x-keep-default" = true))]
    pub mirrors: Vec<MirroredSource>,
    /// Projects carrying a copy of this project's source.
    #[serde(default)]
    #[schemars(extend("x-keep-default" = true))]
    pub mirrored_by: Vec<MirroredSource>,
    /// Whether the dependency collector has walked this project. `not_collected` makes the counts above meaningless rather than informative: nothing was read, so nothing was found.
    #[serde(default = "default_deps_result_status")]
    pub status: DepsResultStatus,
    /// How many manifests the last walk actually parsed.
    #[serde(default)]
    pub manifests_read: i64,
    /// Manifests present in a format the collector does not read (`go.mod`, `pom.xml`, …). A project whose graph lives entirely in one of these reports no references and has plenty.
    #[serde(default)]
    #[schemars(extend("x-keep-default" = true))]
    pub unsupported_manifests: Vec<String>,
    /// Manifests in a supported format that would not parse.
    #[serde(default)]
    #[schemars(extend("x-keep-default" = true))]
    pub unreadable_manifests: Vec<String>,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub detail: Option<String>,
    #[serde(default)]
    pub freshness: Freshness,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AdoptParams {
    /// The observed subject key to adopt.
    pub subject: String,
    /// Override the inferred work kind.
    #[serde(default)]
    pub kind: Option<AdoptParamsKind>,
    /// Override the inferred priority.
    #[serde(default)]
    pub priority: Option<AdoptParamsPriority>,
    /// Override the project the subject belongs to.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub project: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub assignee: Option<String>,
    #[schemars(length(min = 1))]
    pub reason: String,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AdoptResult {
    pub item: WorkItem,
    pub subject_key: String,
    pub inferred_kind: AdoptResultInferredKind,
    pub inferred_priority: AdoptResultInferredPriority,
}
/// A dry run against any path. Reads, stores nothing, needs no reason.
///
/// Split from `ContractCheckParams` because the two are different
/// operations wearing one name. This one changes nothing, so the audit has
/// nothing to record — and demanding a reason for it meant the CLI collected a
/// justification for a write that never happened, then discarded it. It also
/// forced `project.write` scope on a read, so a read-only token could not
/// evaluate a contract against a folder at all.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContractEvaluateParams {
    /// Any folder or repository, registered or not. Stores nothing.
    pub path: String,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContractCheckResult {
    pub path: String,
    #[schemars(required)]
    #[schemars(schema_with = "optional_contract_check_result_project")]
    pub project: Option<String>,
    pub contract_version: String,
    pub status: String,
    /// Every rule evaluated — not only the failures.
    pub criteria: Vec<CriterionView>,
    pub failing: Vec<CriterionView>,
    /// Criteria declared unmeetable here, with their reasons.
    #[serde(default)]
    #[schemars(extend("x-keep-default" = true))]
    pub inapplicable: Vec<CriterionView>,
    /// What would close each failing criterion.
    #[serde(default)]
    #[schemars(extend("x-keep-default" = true))]
    pub recommendations: Vec<RecommendationView>,
    pub recorded: bool,
    #[schemars(schema_with = "optional_date_time")]
    #[schemars(required)]
    pub checked_at: Option<String>,
    /// Why this answer is the answer, where the status alone would not say: a project that has not adopted the contract, or a root path that could not be read.
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub detail: Option<String>,
}
/// Evaluate a registered project and record the result.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContractCheckParams {
    /// A registered project slug. Records the result with its age.
    pub project: String,
    #[schemars(length(min = 1))]
    pub reason: String,
}
/// Declare that a criterion cannot apply to a project.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContractInapplicableParams {
    /// A registered project slug.
    pub project: String,
    /// The criterion's rule, as an evaluation reports it: `required_file` or `required_dir`.
    pub rule: String,
    /// The criterion's target, as an evaluation reports it: `LICENSE`, `src`, `design`.
    pub target: String,
    #[schemars(length(min = 1))]
    pub reason: String,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContractExemptionResult {
    pub project: String,
    /// True when the criterion is now inapplicable here.
    pub declared: bool,
    pub exemptions: Vec<ContractExemptionView>,
    pub detail: String,
}
/// Withdraw an inapplicability declaration.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ContractApplicableParams {
    pub project: String,
    pub rule: String,
    pub target: String,
    #[schemars(length(min = 1))]
    pub reason: String,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComplianceParams {
    pub project: String,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ComplianceResult {
    pub project: String,
    pub status: String,
    pub contract_version: String,
    #[schemars(schema_with = "optional_date_time")]
    #[schemars(required)]
    pub checked_at: Option<String>,
    /// How old this answer is. Never refreshed implicitly.
    #[schemars(required)]
    #[schemars(schema_with = "optional_compliance_result_age_seconds")]
    pub age_seconds: Option<i64>,
    #[serde(default)]
    #[schemars(extend("x-keep-default" = true))]
    pub failing: Vec<CriterionView>,
    /// Whether this project opted into the contract. A project that has not is `not_applicable`, which is not a fault.
    #[serde(default)]
    pub adopted: bool,
    #[serde(default)]
    #[schemars(schema_with = "optional_date_time")]
    pub adopted_at: Option<String>,
    #[serde(default)]
    #[schemars(extend("x-keep-default" = true))]
    pub inapplicable: Vec<CriterionView>,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub detail: Option<String>,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DriftDetectParams {
    /// Apply the shipped low-risk policy: state-sync kinds are accepted automatically, destructive or structural ones never are.
    #[serde(default = "default_drift_detect_params_auto_accept")]
    pub auto_accept: bool,
    #[schemars(length(min = 1))]
    pub reason: String,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DriftDetectResult {
    pub raised: Vec<DriftProposal>,
    pub auto_accepted: Vec<String>,
    /// Findings that already had an open proposal, so were not re-raised.
    pub already_open: i64,
    /// Open proposals this run marked as raised under evidence a later sweep no longer reproduces. They are still open and still need a person; the flag only tells the inbox which ones are worth reading first.
    #[serde(default)]
    #[schemars(extend("x-keep-default" = true))]
    pub superseded: Vec<String>,
    /// Registered projects no collector has ever swept. Nothing could be raised for them, which is a different answer from finding no drift there.
    #[serde(default)]
    #[schemars(extend("x-keep-default" = true))]
    pub not_collected: Vec<String>,
    pub auto_acceptable_kinds: Vec<String>,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DriftListParams {
    /// open / accepted / rejected / contested.
    #[serde(default = "default_drift_list_params_status")]
    #[schemars(schema_with = "optional_string")]
    pub status: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub kind: Option<String>,
    #[serde(default)]
    #[schemars(schema_with = "optional_string")]
    pub project: Option<String>,
    #[serde(default = "default_drift_list_params_limit")]
    #[schemars(range(min = 1, max = 500))]
    pub limit: i64,
}
/// The drift inbox.
///
/// `freshness` is load-bearing here rather than decorative: an empty inbox
/// is reassuring only if something has looked recently. Without it, a
/// collector that stopped running reads as "no drift".
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DriftListResult {
    pub proposals: Vec<DriftProposal>,
    /// Kinds the default policy never auto-accepts, and why.
    #[serde(default)]
    #[schemars(extend("x-keep-default" = true))]
    pub human_gated: std::collections::BTreeMap<String, String>,
    #[serde(default)]
    pub freshness: Freshness,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DriftResolveParams {
    pub id: String,
    pub resolution: DriftResolveParamsResolution,
    #[schemars(length(min = 1))]
    pub reason: String,
}
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DriftResult {
    pub proposal: DriftProposal,
    pub change_applied: bool,
}

fn default_project_brief_params_backlog_limit() -> i64 {
    10
}

fn default_project_brief_params_mode() -> ProjectBriefParamsMode {
    ProjectBriefParamsMode::Summary
}

fn optional_project_brief_result_current_version(_: &mut SchemaGenerator) -> Schema {
    json_schema!({ "anyOf": [{"type": "string"}, { "type": "null" }] })
}

fn default_create_work_params_priority() -> CreateWorkParamsPriority {
    CreateWorkParamsPriority::P2
}

fn default_get_work_params_comment_limit() -> i64 {
    50
}

fn default_get_work_params_mode() -> GetWorkParamsMode {
    GetWorkParamsMode::Full
}

fn default_get_work_params_comment_order() -> GetWorkParamsCommentOrder {
    GetWorkParamsCommentOrder::Newest
}

fn optional_list_work_params_query(_: &mut SchemaGenerator) -> Schema {
    json_schema!({ "anyOf": [{"maxLength": 200, "type": "string"}, { "type": "null" }] })
}

fn default_list_work_params_mode() -> ListWorkParamsMode {
    ListWorkParamsMode::Summary
}

fn default_list_work_params_limit() -> i64 {
    50
}

fn default_work_list_result_mode() -> WorkListResultMode {
    WorkListResultMode::Full
}

fn default_board_list_params_lane_mode() -> BoardListParamsLaneMode {
    BoardListParamsLaneMode::None
}

fn default_board_list_params_page_size() -> i64 {
    30
}

fn optional_update_work_params_title(_: &mut SchemaGenerator) -> Schema {
    json_schema!({ "anyOf": [{"minLength": 1, "type": "string"}, { "type": "null" }] })
}

fn default_comment_params_kind() -> CommentParamsKind {
    CommentParamsKind::Note
}

fn default_comment_result_write_back() -> String {
    "skipped".to_string()
}

fn default_backlog_params_include_prs() -> bool {
    true
}

fn default_backlog_params_mode() -> BacklogParamsMode {
    BacklogParamsMode::Summary
}

fn default_backlog_params_limit() -> i64 {
    20
}

fn default_bugs_params_limit() -> i64 {
    50
}

fn default_create_initiative_params_state() -> CreateInitiativeParamsState {
    CreateInitiativeParamsState::Open
}

fn default_list_initiatives_params_limit() -> i64 {
    100
}

fn default_list_initiatives_params_mode() -> ListInitiativesParamsMode {
    ListInitiativesParamsMode::Full
}

fn optional_update_initiative_params_title(_: &mut SchemaGenerator) -> Schema {
    json_schema!({ "anyOf": [{"minLength": 1, "type": "string"}, { "type": "null" }] })
}

fn optional_update_initiative_params_weight(_: &mut SchemaGenerator) -> Schema {
    json_schema!({ "anyOf": [{"maximum": 100, "minimum": 0, "type": "integer"}, { "type": "null" }] })
}

fn default_observations_params_latest_only() -> bool {
    true
}

fn default_observations_params_limit() -> i64 {
    100
}

fn default_deps_result_status() -> DepsResultStatus {
    DepsResultStatus::NotCollected
}

fn optional_contract_check_result_project(_: &mut SchemaGenerator) -> Schema {
    json_schema!({ "anyOf": [{"type": "string"}, { "type": "null" }] })
}

fn optional_compliance_result_age_seconds(_: &mut SchemaGenerator) -> Schema {
    json_schema!({ "anyOf": [{"type": "integer"}, { "type": "null" }] })
}

fn default_drift_detect_params_auto_accept() -> bool {
    true
}

fn default_drift_list_params_status() -> Option<String> {
    Some("open".to_string())
}

fn default_drift_list_params_limit() -> i64 {
    100
}
