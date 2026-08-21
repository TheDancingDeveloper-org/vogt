"""Parameter and result models.

These are the argument schemas the operation registry publishes: the CLI
builds its flags from them, FastAPI builds its request and response schemas
from them, and MCP builds its `inputSchema` from them. One definition, three
transports — which is the mechanical half of transport parity (FR-A2).

Two conventions worth knowing before adding one:

- **Callers name things the way humans and agents do.** Parameters take
  `WI-7`, a project slug, an actor's `identity_ref` — not ULIDs. Resolution
  to ids happens in the services, so a mistyped reference fails with "no work
  item WI-70" rather than a foreign-key error.
- **Clearing is explicit.** A field left unset means "leave it alone"; a
  `clear_*` flag means "unset it". Without that split, unassigning somebody
  and not mentioning the assignee are the same request.
"""

from __future__ import annotations

from datetime import datetime
from typing import Literal

from pydantic import BaseModel, ConfigDict, Field

from vogt.core.entities import (
    Actor,
    AuditRecord,
    AuthDecision,
    Comment,
    DepRef,
    DriftProposal,
    Effort,
    Event,
    Initiative,
    InitiativeState,
    Label,
    LifecycleState,
    Name,
    Observation,
    Priority,
    Project,
    Reason,
    RelationKind,
    Suppression,
    Token,
    TrustState,
    WorkItem,
    WorkKind,
)

#: Applied to collection within a project (FR-G12). Recorded at registration
#: so the value exists before the collectors that honour it arrive at M2.
DEFAULT_EXCLUSIONS: tuple[str, ...] = (
    ".venv/",
    "node_modules/",
    "target/",
    "dist/",
    "build/",
    ".git/",
    # Agent scratch space. Worktrees under `.claude/` carry complete copies of
    # a project's manifests, so without this a repository's dependency graph is
    # inflated by however many worktrees happen to be lying around — `vogt`
    # reported six references out, three of them duplicates from one
    # throwaway worktree.
    ".claude/",
)


class Params(BaseModel):
    """Base for operation parameters."""

    model_config = ConfigDict(extra="forbid")


class Result(BaseModel):
    """Base for operation results."""

    model_config = ConfigDict(extra="forbid")


# -- normalized attention inbox -------------------------------------------


InboxSource = Literal["github", "drift", "ci", "agent"]
InboxTriageState = Literal["active", "archived", "snoozed"]


class InboxAction(Result):
    """A typed target for the action a row can take."""

    kind: Literal["drift", "observation", "session"]
    drift_id: str | None = None
    subject_key: str | None = None
    session_id: str | None = None


class InboxEntry(Result):
    """One server-normalized occurrence in the attention stream."""

    entry_key: str
    source: InboxSource
    kind: str
    occurred_at: datetime | None = None
    observed_at: datetime | None = None
    title: str
    summary: str = Field(default="", max_length=1000)
    project_slug: str | None = None
    work_item_ref: str | None = None
    session_id: str | None = None
    source_subject_key: str
    source_url: str | None = None
    trust_state: TrustState = "unverified"
    freshness: Literal["current", "stale", "provisional", "live", "unknown"] = "unknown"
    provisional: bool = False
    triage_state: InboxTriageState = "active"
    snooze_until: datetime | None = None
    action: InboxAction | None = None
    evidence_snapshot: dict[str, object] | None = None
    proposed_change: dict[str, object] | None = None


class InboxCoverage(Result):
    """Coverage and count for one normalized source."""

    source: InboxSource
    status: str
    count: int = 0
    observed_at: datetime | None = None
    projects: int = 0
    registered: int = 0
    detail: str | None = None


class InboxListParams(Params):
    sources: list[InboxSource] | None = None
    triage_states: list[InboxTriageState] = Field(default=["active"])
    project: str | None = None
    work_item: str | None = None
    limit: int = Field(default=50, ge=1, le=100)
    cursor: str | None = None


class InboxListResult(Result):
    entries: list[InboxEntry]
    next_cursor: str | None = None
    snapshot_at: datetime
    """Per-source high-water marks for this server-owned read window."""
    high_water: dict[InboxSource, datetime | None]
    coverage: dict[InboxSource, InboxCoverage]
    counts: dict[str, int]
    github_scope: str = "registered projects only"
    instance_scope: str = "registered projects only"
    engine_status: Literal["not_configured", "available", "unreachable"]
    engine_detail: str | None = None
    engine_available: bool = True


class InboxArchiveParams(Params):
    entry_key: str
    reason: Reason


class InboxSnoozeParams(Params):
    entry_key: str
    until: datetime
    reason: Reason


class InboxRestoreParams(Params):
    entry_key: str
    reason: Reason


class InboxTriageResult(Result):
    entry: InboxEntry


# -- instance --------------------------------------------------------------


class InitParams(Params):
    """`init` takes nothing: the data directory comes from configuration."""


class InitResult(Result):
    instance_id: str
    data_dir: str
    created: bool
    declared_schema_version: int
    observed_schema_version: int
    migrations_applied: list[str]


class MigrateParams(Params):
    """`migrate` takes nothing: the data directory comes from configuration."""


class MigrateResult(Result):
    """What `migrate` moved, and whether anything is still behind.

    Reports the expected versions beside the applied ones for the same reason
    `/health/ready` does (NFR-I3): the applied number alone cannot answer
    "is this instance current?", which is the only question the operator
    running this has.
    """

    data_dir: str
    declared_schema_version: int
    observed_schema_version: int
    declared_schema_expected: int
    observed_schema_expected: int
    migrations_applied: list[str]


class McpStdioParams(Params):
    """`mcp stdio` takes nothing: the streams are this process's own."""


class McpStdioResult(Result):
    protocol_version: str | None
    messages_handled: int
    supported_protocol_versions: list[str]


class StatusParams(Params):
    pass


class StoreCounts(Result):
    projects: int
    actors: int
    events: int
    audit: int
    work_items: int
    initiatives: int


class StatusResult(Result):
    vogt_version: str
    instance_id: str
    data_dir: str
    principal: str
    revision: int
    declared_schema_version: int
    observed_schema_version: int
    counts: StoreCounts


# -- projects --------------------------------------------------------------


class RegisterProjectParams(Params):
    name: Name = Field(description="Display name; the slug is derived from it.")
    root_path: str = Field(description="Folder or git repository this project is.")
    repo_url: str | None = Field(
        default=None, description="Optional remote the project is published at."
    )
    lifecycle_state: LifecycleState = Field(
        default="active", description="incubating / active / maintenance / archived."
    )
    exclusions: list[str] | None = Field(
        default=None,
        description=(
            "Paths collection skips, replacing the defaults entirely rather "
            "than adding to them. Omit for the defaults. A repository that "
            "vendors a corpus or carries agent worktrees needs this at "
            "registration, because the first sweep is the baseline every "
            "later answer is compared against."
        ),
    )
    reason: Reason = Field(description="Why this write is being made (audited).")


class UpdateProjectParams(Params):
    """Correct a project's declaration after registration.

    Deliberately narrow. `lifecycle_state` is absent because it has its own
    operation with a validated edge (`project transition`), and the observed
    fields are absent because nothing declares them. What is left is the two
    facts a registration can get wrong and nothing else could fix: where the
    project is published, and what collection should skip.
    """

    slug: str = Field(description="Project to update.")
    repo_url: str | None = Field(
        default=None, description="Leave unset to keep the current value."
    )
    exclusions: list[str] | None = Field(
        default=None,
        description=(
            "Replaces the current list entirely. Leave unset to keep it; pass "
            "an empty list to collect everything."
        ),
    )
    reason: Reason = Field(description="Why this write is being made (audited).")


class CreateProjectParams(Params):
    name: Name = Field(description="Display name; the slug is derived from it.")
    root_path: str = Field(
        description="Directory to scaffold into. Created if absent; "
        "existing files are never overwritten."
    )
    owner: str | None = Field(
        default=None,
        description="Recorded in the scaffold. Defaults to the acting principal.",
    )
    repo_url: str | None = None
    lifecycle_state: LifecycleState = "incubating"
    reason: Reason = Field(description="Why this write is being made (audited).")


class ProjectResult(Result):
    project: Project


class CreateProjectResult(Result):
    project: Project
    created_paths: list[str] = Field(
        description="Files and directories written. Existing ones are left alone."
    )
    skipped_paths: list[str] = Field(
        description="Contract paths that already existed and were not touched."
    )


class GetProjectParams(Params):
    slug: str = Field(description="Project slug.")


class ListProjectsParams(Params):
    limit: int = Field(default=50, ge=1, le=500)
    offset: int = Field(default=0, ge=0)


class ProjectListResult(Result):
    projects: list[Project]
    total: int


class TransitionProjectParams(Params):
    slug: str
    to_state: LifecycleState = Field(description="Target lifecycle state.")
    reason: Reason


class Freshness(Result):
    """How old the evidence behind an answer is (FR-V4).

    Every aggregating answer carries one. `oldest_relevant_sweep` is the
    honest number: an answer is exactly as fresh as the least fresh thing it
    depends on.
    """

    status: Literal["fresh", "never_swept", "partial"] = "never_swept"
    oldest_relevant_sweep: datetime | None = None
    age_seconds: int | None = None
    collectors: dict[str, str] = Field(
        default={}, description="Per collector: how long ago it last completed."
    )
    detail: str | None = None


class NotCollected(Result):
    """A value no collector has produced yet.

    Present from M1 so that the shape of an answer does not change when M2
    starts filling it in, and so "we have not looked" is visibly different
    from "there is nothing" (FR-O4, DESIGN §6).
    """

    status: str = "not_collected"
    detail: str


class RankedItem(Result):
    """One entry of a ranked view — declared or observed.

    Observed-first means both halves appear in the same list, ordered by the
    same weights (FR-W4). The common fields are what ranking needs and what a
    reader wants; `item` carries the whole work item when there is one, and
    `subject_key` points at the evidence when there is not.
    """

    origin: Literal["declared", "observed"]
    classified: bool = Field(
        default=True,
        description=(
            "Whether anything actually said what kind of work this is. False "
            "means `kind` is this product's guess from an absence of signal — "
            "an unlabelled forge issue — and that the subject may be missing "
            "from the view a reader expected to find it in. Declared work is "
            "always classified: somebody typed the kind in."
        ),
    )
    ref: str = Field(
        description="`WI-7` for declared work, or the observed subject key."
    )
    title: str
    kind: WorkKind
    state: str
    priority: Priority
    project_slug: str | None = None
    trust_state: TrustState = "unverified"
    labels: list[str] = []
    score: float
    updated_at: datetime
    #: Present only for declared work.
    item: WorkItem | None = None
    #: Present only for observed subjects.
    observation_kind: str | None = None
    source_url: str | None = None
    observed_at: datetime | None = None
    adopted_as: str | None = Field(
        default=None,
        description="Work item reference, when this subject has been adopted.",
    )


class ProjectBriefParams(Params):
    slug: str
    backlog_limit: int = Field(default=10, ge=1, le=100)


class CiSummary(Result):
    """The CI story for a project (FR-O6), or the absence of one.

    Every field below describes **one revision** — the newest any retained
    check names. That scope is the point: the same shape without it reported
    a project as `failing` on the strength of a build that broke days before
    the current head and had since been fixed. FR-O4's rule — partial
    coverage is disclosed, never silently returned as complete — is the same
    rule one layer up from the observations it was written about.
    """

    status: Literal["not_collected", "no_checks", "passing", "failing"] = (
        "not_collected"
    )
    checks: int = Field(
        default=0, description="Checks observed on `revision`, not in total."
    )
    failing: list[str] = []
    revision: str | None = None
    revisions_observed: int = Field(
        default=0,
        description=(
            "How many distinct revisions the retained window covers. The "
            "denominator `checks` is a numerator of, so a reader can tell a "
            "quiet project from a busy one."
        ),
    )
    earlier_failures: int = Field(
        default=0,
        description=(
            "Failing checks on revisions behind `revision`. History, not "
            "verdict: these do not make `status` failing, and a project that "
            "fixes its build goes green here without waiting for retention."
        ),
    )
    detail: str | None = None


class DependencySummary(Result):
    """What this project references, and what references it (FR-D1–D4)."""

    status: Literal["not_collected", "collected"] = "not_collected"
    references_out: int = 0
    referenced_by: int = 0
    unresolved: int = 0
    detail: str | None = None


class ProjectBriefResult(Result):
    """The per-repo view in one call (FR-P2)."""

    project: Project
    open_work: int = Field(
        description=(
            "Outstanding work across both populations, declared and observed "
            "— the same set `backlog --project` ranks. Split below, because "
            "a total that does not name its populations is not an answer."
        )
    )
    open_bugs: int
    declared_work: int = Field(
        default=0, description="Of `open_work`, how much somebody typed in."
    )
    observed_work: int = Field(
        default=0,
        description=(
            "Of `open_work`, how much a collector found — chiefly what "
            "`forge onboard` consolidated. Zero here with a non-zero "
            "`declared_work` is a project nobody has collected for; the "
            "reverse is a project nobody has declared work on."
        ),
    )
    by_state: dict[str, int] = Field(
        description=(
            "Declared work items only, terminal states included. Observed "
            "subjects have no workflow state to count (DESIGN §3.6)."
        )
    )
    by_kind: dict[str, int] = Field(description="Declared work items only, as above.")
    top_backlog: list[RankedItem]
    current_version: str | None
    declared_version: str | None = None
    observed_version: str | None = Field(
        default=None,
        description="Newest tag or release seen by a collector (FR-P3).",
    )
    version_matches: bool | None = Field(
        default=None,
        description=(
            "Whether declared and observed versions agree. Null when either "
            "is unknown; the drift proposal this feeds arrives at M3."
        ),
    )
    compliance_status: str
    compliance_checked_at: datetime | None
    ci_status: CiSummary
    dependencies: DependencySummary
    freshness: Freshness


# -- work ------------------------------------------------------------------


# Defined here rather than with the rest of the session models below,
# because `WorkResult` carries it: a work item's view shows what is
# running for it (FR-E4), and a forward reference would leave the model
# incomplete until something remembered to rebuild it.
class SessionSummary(Result):
    """One session, as Vogt knows it and as the engine currently reports it.

    Two halves on purpose. `id`, `project`, `work_item` and `reason` are
    Vogt's declared link — written once, audited, and true whatever the
    engine is doing. `activity` and `alive` are read from the engine at the
    moment of asking and are never stored: a cached activity state would be
    a claim about a running process, which is the one thing this product
    refuses to invent (FR-E2).
    """

    id: str
    engine_session_id: str
    project: str | None = None
    work_item: str | None = Field(
        default=None, description="Work item ref, e.g. WI-7, when opened for one."
    )
    actor: str = Field(description="Actor the session's writes are attributed to.")
    cwd: str
    template: str | None = None
    model: str | None = Field(
        default=None,
        description=(
            "The model this session was started with (FR-T11). What was asked "
            "for, not what the agent is using now."
        ),
    )
    effort: str | None = Field(
        default=None, description="The reasoning effort it was started with."
    )
    reason: str
    started_at: datetime
    stopped_at: datetime | None = None
    activity: str | None = Field(
        default=None,
        description=(
            "Live from the engine: idle / running / waiting-for-input / "
            "errored. None when the engine could not be asked."
        ),
    )
    alive: bool | None = Field(
        default=None,
        description="Whether the engine still has this session. None if unasked.",
    )


class CreateWorkParams(Params):
    kind: WorkKind = Field(description="feature / bug / chore / question.")
    title: Name
    body: str = ""
    priority: Priority = "p2"
    effort: Effort | None = None
    project: str | None = Field(default=None, description="Project slug.")
    initiative: str | None = Field(default=None, description="Initiative slug.")
    assignee: str | None = Field(
        default=None, description="Actor identity_ref, e.g. local:sprooty."
    )
    labels: list[str] | None = Field(
        default=None, description="Existing label names to attach."
    )
    reason: Reason


class WorkResult(Result):
    item: WorkItem
    comments: list[Comment] = []
    #: Sessions opened for this item, live activity included (FR-E4).
    #: Populated by `work.get`; empty on the write operations, which answer
    #: about the change they made rather than about what is running.
    sessions: list[SessionSummary] = []


class GetWorkParams(Params):
    ref: str = Field(description="Work item reference, e.g. WI-7.")
    comment_limit: int = Field(default=50, ge=0, le=500)


class ListWorkParams(Params):
    project: str | None = Field(default=None, description="Project slug.")
    kinds: list[WorkKind] | None = None
    states: list[str] | None = None
    priorities: list[Priority] | None = None
    assignee: str | None = Field(default=None, description="Actor identity_ref.")
    initiative: str | None = Field(default=None, description="Initiative slug.")
    label: str | None = None
    include_finished: bool = Field(
        default=False, description="Include done and wont_do items."
    )
    limit: int = Field(default=50, ge=1, le=500)
    offset: int = Field(default=0, ge=0)


class WorkListResult(Result):
    items: list[WorkItem]
    total: int


BoardLaneMode = Literal["none", "project", "initiative"]


class BoardCellParams(Params):
    """One cell requested in a batched Board read."""

    lane_key: str = Field(
        default="",
        description=(
            "Project slug or initiative id for the selected lane mode; blank is "
            "the sole lane in `none` mode and the unassigned lane otherwise."
        ),
    )
    state: str
    cursor: str | None = Field(
        default=None,
        description="Opaque continuation returned for this exact cell.",
    )


class BoardListParams(Params):
    """One bounded, server-owned batch of independently pageable cells."""

    project: str | None = Field(default=None, description="Project slug.")
    kinds: list[WorkKind] | None = None
    states: list[str] | None = None
    priorities: list[Priority] | None = None
    assignee: str | None = Field(default=None, description="Actor identity_ref.")
    initiative: str | None = Field(default=None, description="Initiative slug.")
    label: str | None = None
    lane_mode: BoardLaneMode = "none"
    cells: list[BoardCellParams] = Field(min_length=1, max_length=40)
    page_size: int = Field(default=30, ge=1, le=100)
    snapshot: str | None = Field(
        default=None,
        description=(
            "Opaque snapshot returned by the first batch. Required when adding "
            "another cell or continuing one against that same Board view."
        ),
    )


class BoardCellResult(Result):
    lane_key: str
    state: str
    items: list[WorkItem]
    total: int
    next_cursor: str | None = None


class BoardListResult(Result):
    cells: list[BoardCellResult]
    column_totals: dict[str, int]
    lane_totals: dict[str, int]
    #: The declared Board population: how many declared work items match this
    #: filter across every column, terminal ones included. Unchanged in
    #: meaning — the number the Board draws — so existing callers do not shift.
    total: int
    backlog_candidates: int = Field(
        default=0,
        description=(
            "How many things the Backlog would consider for this same scope "
            "(#187): declared work plus open forge subjects that are not yet "
            "tracked as work items. The Board draws only the declared cards, so "
            "this is almost always larger, and a surface that shows `total` "
            "without it silently reads a small Board as the size of the estate. "
            "Computed the observed-inclusive way the Backlog computes it, over "
            "the same project/kind/priority/assignee/initiative/label filters."
        ),
    )
    declared_total: int = Field(
        default=0,
        description=(
            "The declared-only slice of `backlog_candidates` — non-terminal "
            "declared work in this scope, the same population the rail counts. "
            "`backlog_candidates - declared_total` is the observed subjects the "
            "Board is currently silent about."
        ),
    )
    snapshot: str
    snapshot_at: datetime
    revision: int


class UpdateWorkParams(Params):
    ref: str
    title: Name | None = None
    body: str | None = None
    priority: Priority | None = None
    effort: Effort | None = None
    project: str | None = Field(default=None, description="Project slug.")
    initiative: str | None = Field(default=None, description="Initiative slug.")
    assignee: str | None = Field(default=None, description="Actor identity_ref.")
    clear_effort: bool = False
    clear_assignee: bool = False
    clear_initiative: bool = False
    add_labels: list[str] | None = None
    remove_labels: list[str] | None = None
    reason: Reason


class TransitionWorkParams(Params):
    ref: str
    to_state: str = Field(description="Target state, e.g. in_progress.")
    reason: Reason


class RelateWorkParams(Params):
    ref: str
    kind: RelationKind = Field(
        description="depends_on / relates_to / duplicate_of / parent_of."
    )
    target: str = Field(description="The other work item's reference.")
    reason: Reason


class UnrelateWorkParams(Params):
    ref: str
    kind: RelationKind
    target: str
    reason: Reason


class CommentParams(Params):
    ref: str
    body: Name = Field(description="The comment text.")
    reason: Reason


class CommentResult(Result):
    comment: Comment
    write_back: str = Field(
        default="skipped",
        description=(
            "What happened upstream: skipped (the usual case), succeeded, "
            "or failed. A failure never fails the local write."
        ),
    )


# -- taxonomy --------------------------------------------------------------


class CreateLabelParams(Params):
    name: Name
    color: str | None = Field(default=None, description="Hex colour, e.g. #d73a4a.")
    reason: Reason


class LabelResult(Result):
    label: Label


class ListLabelsParams(Params):
    limit: int = Field(default=100, ge=1, le=500)
    offset: int = Field(default=0, ge=0)


class LabelListResult(Result):
    labels: list[Label]


class CreateInitiativeParams(Params):
    title: Name
    body: str = ""
    weight: int = Field(
        default=0, ge=0, le=100, description="Feeds ranking; 0 means no lift."
    )
    state: InitiativeState = "open"
    reason: Reason


class InitiativeResult(Result):
    initiative: Initiative


class ListInitiativesParams(Params):
    limit: int = Field(default=100, ge=1, le=500)
    offset: int = Field(default=0, ge=0)


class InitiativeListResult(Result):
    initiatives: list[Initiative]


class CreateActorParams(Params):
    identity_ref: Name = Field(
        description="Stable identity, e.g. agent:claude-code or local:sprooty."
    )
    kind: str = Field(default="agent", description="human or agent.")
    display_name: Name
    reason: Reason


class ActorResult(Result):
    actor: Actor


class ListActorsParams(Params):
    limit: int = Field(default=100, ge=1, le=500)
    offset: int = Field(default=0, ge=0)


class ActorListResult(Result):
    actors: list[Actor]


# -- views -----------------------------------------------------------------


class BacklogParams(Params):
    project: str | None = Field(
        default=None, description="Project slug; omit for the global backlog."
    )
    kinds: list[WorkKind] | None = None
    priorities: list[Priority] | None = None
    assignee: str | None = None
    initiative: str | None = None
    label: str | None = None
    trust_states: list[TrustState] | None = None
    include_prs: bool = Field(
        default=True,
        description=(
            "Whether open pull requests rank in the backlog (#170). On by "
            "default: a synced PR is worklike, and now that closure is "
            "observed (#173) a merged one self-heals out rather than "
            "lingering. Set false to see issues and markers only — a view "
            "choice, not a data one; the PRs stay queryable through "
            "`observations list`."
        ),
    )
    limit: int = Field(default=20, ge=1, le=200)
    offset: int = Field(
        default=0,
        ge=0,
        description="Ranked rows to skip. `total_considered` is the full "
        "count, so a caller knows whether another page exists. Ranking is "
        "recomputed per request and staleness grows with the clock, so two "
        "pages fetched far enough apart can repeat or skip an item near a "
        "score boundary — the same caveat `audit.list` carries, for the same "
        "reason.",
    )


class BacklogResult(Result):
    items: list[RankedItem]
    total_considered: int
    declared: int = 0
    observed: int = 0
    suppressed: int = 0
    closed_upstream: int = Field(
        default=0,
        description=(
            "Observed subjects left out because their source says they are "
            "closed. Reported rather than silently dropped, so a short list is "
            "distinguishable from a filtered one — they remain queryable "
            "through `observations list`."
        ),
    )
    scope: str
    freshness: Freshness


class BugsParams(Params):
    project: str | None = None
    priorities: list[Priority] | None = None
    assignee: str | None = None
    label: str | None = None
    limit: int = Field(default=50, ge=1, le=200)
    offset: int = Field(
        default=0,
        ge=0,
        description="Ranked rows to skip. `total_considered` is the full "
        "count, so a caller knows whether another page exists. Ranking is "
        "recomputed per request and staleness grows with the clock, so two "
        "pages fetched far enough apart can repeat or skip an item near a "
        "score boundary — the same caveat `audit.list` carries, for the same "
        "reason.",
    )


class WhyParams(Params):
    ref: str


class ContributionView(Result):
    input: str
    detail: str
    value: float
    weight: float
    contribution: float


class WhyResult(Result):
    ref: str
    title: str
    total: float
    contributions: list[ContributionView]
    inputs_not_yet_available: dict[str, str] = Field(
        description="Documented ranking inputs that cannot fire in this build."
    )


class WorkflowListParams(Params):
    pass


class WorkflowView(Result):
    kind: str
    initial_state: str
    states: list[str]
    transitions: dict[str, list[str]]


class WorkflowListResult(Result):
    workflows: list[WorkflowView]


# -- history ---------------------------------------------------------------


class ListEventsParams(Params):
    after: int = Field(
        default=0, ge=0, description="Cursor: return events with seq greater than this."
    )
    limit: int = Field(default=100, ge=1, le=1000)
    entity_id: str | None = Field(
        default=None,
        description=(
            "Only events about this entity. The id, not the ref — the same "
            "shape audit.list takes, so the two feeds narrow alike."
        ),
    )


class EventListResult(Result):
    events: list[Event]
    next_cursor: int


class ListAuditParams(Params):
    """How the audit log is narrowed (FR-S6, FR-U19).

    `limit`/`offset`/`total` rather than a cursor, which is the idiom every
    other filtered list here uses (`work.list`, `observations.list`). The
    events feed's `after` cursor is the exception and earns it: that feed is
    read forwards by a poller, while the audit log is read newest-first by a
    person, and a cursor cannot answer "how many records match at all" —
    which is what tells a reader whether they are looking at the whole story.
    """

    limit: int = Field(default=50, ge=1, le=500)
    offset: int = Field(
        default=0,
        ge=0,
        description="Records to skip. Paging a log that is being written to "
        "can repeat a record, because new rows arrive at the front.",
    )
    actor_id: str | None = None
    operation: str | None = None
    entity_id: str | None = Field(
        default=None,
        description="An entity's id. A work item's trail also carries the "
        "writes audited against that item's comments.",
    )
    project: str | None = Field(
        default=None,
        description="Project slug. Keeps the writes this instance can "
        "attribute to that project; see the operation's docstring for the "
        "kinds that carry one.",
    )
    since: datetime | None = Field(
        default=None, description="Inclusive lower bound on the write's time."
    )
    until: datetime | None = Field(
        default=None, description="Exclusive upper bound on the write's time."
    )


class AuditListResult(Result):
    records: list[AuditRecord]
    total: int = Field(
        description="Records matching the filters, ignoring limit and offset."
    )


# -- collection ------------------------------------------------------------


class SweepParams(Params):
    project: str | None = Field(
        default=None,
        description="Narrow the sweep to one project. Scope is never widened.",
    )
    collectors: list[str] | None = Field(
        default=None, description="Collector names; omit to run all of them."
    )
    offline_only: bool = Field(
        default=False,
        description="Skip every collector that needs the network (NFR-PO2).",
    )
    reason: Reason


class SweepReportView(Result):
    collector: str
    sweep_id: str
    outcome: str
    projects: int
    new: int
    unchanged: int
    failures: dict[str, str] = {}


class SweepResult(Result):
    scope: str
    projects: int
    subjects: int
    dep_refs: int
    reports: list[SweepReportView]


class CoverageParams(Params):
    pass


class CoverageEntry(Result):
    collector: str
    status: str
    last_swept_at: datetime | None = None
    age_seconds: int | None = None
    projects: int = Field(
        default=0,
        description=(
            "How many projects this collector has ever swept. Cumulative, "
            "which is what the operation's name promises; it used to be the "
            "scope of the most recent sweep, so a `--project`-scoped run made "
            "every collector look as though it had only ever seen one."
        ),
    )
    registered: int = Field(
        default=0,
        description="The denominator: projects registered on this instance.",
    )
    last_sweep_scope: int = Field(
        default=0,
        description=(
            "How many projects the most recent sweep was asked about. Reported "
            "separately so a scoped sweep and a collector that failed on seven "
            "of eight projects stop looking alike."
        ),
    )
    never_swept: int = Field(
        default=0,
        description=(
            "Registered projects this collector has never looked at. The "
            "number a reader is usually after, and the one that used to have "
            "to be inferred from a count that could not support it."
        ),
    )
    detail: str | None = None


class CoverageResult(Result):
    collectors: list[CoverageEntry]
    swept_project_ids: list[str]
    unswept_project_ids: list[str] = Field(
        default_factory=list,
        description=(
            "Registered projects no collector has ever swept. A registered "
            "project nothing has looked at has no evidence behind anything it "
            "claims, and is the case FR-O4 exists to keep visible."
        ),
    )


class ObservationsParams(Params):
    project: str | None = None
    kind: str | None = Field(
        default=None, description="Observation kind, e.g. marker or forge.issue."
    )
    subject_key: str | None = None
    promoted_only: bool = False
    latest_only: bool = Field(
        default=True,
        description="Newest per subject. Set false for the full history.",
    )
    limit: int = Field(default=100, ge=1, le=1000)
    offset: int = Field(default=0, ge=0)


class ObservationsResult(Result):
    observations: list[Observation]
    total: int = Field(
        description=(
            "How many rows this page holds — not how many exist. The store "
            "is queried a page at a time and no count is taken behind it, so "
            "`total == limit` means 'there may be more', never 'that is all'."
        )
    )
    detail: str | None = Field(
        default=None,
        description=(
            "Why an empty answer is empty, where that is not 'there are "
            "none'. An unswept instance has no evidence tables at all, and "
            "returning `[]` for that reads as a collector that found nothing "
            "(FR-O4)."
        ),
    )


class DepsParams(Params):
    project: str = Field(description="Project slug.")


class MirroredSource(Result):
    """The same source in two places, reported and never judged (FR-D8).

    A path member of one project that declares the package a separately
    registered project publishes — `rustnzb`'s `crates/nzb-core` and the
    standalone `nzb-core`. Contents are not compared and no divergence is
    asserted; the two declared versions are recorded as the facts they are.
    """

    package: str
    project: str = Field(description="The project that carries the copy.")
    mirrors: str = Field(description="The registered project that publishes it.")
    local_path: str = Field(description="Where the copy sits, inside `project`.")
    manifest: str | None = None
    local_version: str | None = None
    published_version: str | None = None
    observed_at: datetime


class DepsResult(Result):
    """The dependency graph around one project.

    Carries `freshness` because a graph is an aggregate over sweeps, and an
    empty graph with no sweep behind it means "not collected", not "this
    project depends on nothing" (FR-O4, FR-U2). `status` narrows the same
    point to this project: estate-wide freshness cannot say whether the
    dependency collector has ever walked *this* tree.
    """

    project: str
    references_out: list[DepRef]
    referenced_by: list[DepRef]
    unresolved: int = 0
    mirrors: list[MirroredSource] = Field(
        default=[],
        description="Copies this project carries of other projects' source (FR-D8).",
    )
    mirrored_by: list[MirroredSource] = Field(
        default=[],
        description="Projects carrying a copy of this project's source (FR-D8).",
    )
    status: Literal["not_collected", "collected"] = Field(
        default="not_collected",
        description=(
            "Whether the dependency collector has walked this project. "
            "`not_collected` makes the counts above meaningless rather than "
            "informative: nothing was read, so nothing was found."
        ),
    )
    manifests_read: int = Field(
        default=0,
        description="How many manifests the last walk actually parsed.",
    )
    unsupported_manifests: list[str] = Field(
        default=[],
        description=(
            "Manifests present in a format the collector does not read "
            "(`go.mod`, `pom.xml`, …). A project whose graph lives entirely "
            "in one of these reports no references and has plenty."
        ),
    )
    unreadable_manifests: list[str] = Field(
        default=[],
        description="Manifests in a supported format that would not parse.",
    )
    detail: str | None = None
    freshness: Freshness = Freshness()


class PruneParams(Params):
    reason: Reason


class PruneResult(Result):
    removed: int
    kept_latest: int
    kept_referenced: int
    horizon_days: int


# -- suppression and adoption ---------------------------------------------


class SuppressParams(Params):
    subject: str = Field(
        description="An exact subject key, or a glob pattern when --pattern is set."
    )
    pattern: bool = Field(
        default=False, description="Treat `subject` as a glob pattern."
    )
    project: str | None = Field(
        default=None, description="Limit the suppression to one project."
    )
    reason: Reason


class SuppressionResult(Result):
    suppression: Suppression


class ListSuppressionsParams(Params):
    include_revoked: bool = False
    limit: int = Field(default=100, ge=1, le=500)


class SuppressionListResult(Result):
    suppressions: list[Suppression]


class RevokeSuppressionParams(Params):
    id: str
    reason: Reason


class AdoptParams(Params):
    subject: str = Field(description="The observed subject key to adopt.")
    kind: WorkKind | None = Field(
        default=None, description="Override the inferred work kind."
    )
    priority: Priority | None = Field(
        default=None, description="Override the inferred priority."
    )
    project: str | None = Field(
        default=None, description="Override the project the subject belongs to."
    )
    assignee: str | None = None
    reason: Reason


class AdoptResult(Result):
    item: WorkItem
    subject_key: str
    inferred_kind: WorkKind
    inferred_priority: Priority


# -- contract and compliance ----------------------------------------------


class CriterionView(Result):
    rule: str
    target: str
    satisfied: bool
    detail: str
    applicable: bool = Field(
        default=True,
        description=(
            "Whether this criterion can apply to this project at all "
            "(FR-G19). False is a declaration somebody made and gave a reason "
            "for — a Cargo workspace has no root `src/` — and an inapplicable "
            "criterion is reported but never counted as failing."
        ),
    )
    tracked: bool | None = Field(
        default=None,
        description=(
            "Whether the repository carries this. Null where it could not be "
            "asked — an unregistered path, or a directory that is not a "
            "checkout. False alongside a file that exists on disk is the "
            "present-but-untracked case: no clone would have it."
        ),
    )


class RecommendationView(Result):
    """What would close one failing criterion, and who has to decide it.

    FR-G18: advisory output. A `scaffold` remedy is one `project scaffold`
    performs; a `judgement` remedy is an instruction addressed to an actor —
    readable by a person, executable by an agent, applied implicitly by
    neither. Nothing may treat any of it as authority (FR-G13).
    """

    rule: str
    target: str
    remedy: Literal["scaffold", "judgement"]
    instruction: str


class ContractEvaluateParams(Params):
    """A dry run against any path. Reads, stores nothing, needs no reason.

    Split from `ContractCheckParams` because the two are different
    operations wearing one name. This one changes nothing, so FR-S1 has
    nothing to audit — and demanding a reason for it meant the CLI collected a
    justification for a write that never happened, then discarded it. It also
    forced `project.write` scope on a read, so a read-only token could not
    evaluate a contract against a folder at all.
    """

    path: str = Field(
        description="Any folder or repository, registered or not. Stores nothing."
    )


class ContractCheckParams(Params):
    """Evaluate a registered project and record the result (FR-G14)."""

    project: str = Field(
        description="A registered project slug. Records the result with its age."
    )
    reason: Reason


class ContractCheckResult(Result):
    path: str
    project: str | None
    contract_version: str
    status: str
    criteria: list[CriterionView] = Field(
        description="Every rule evaluated — not only the failures (FR-G3)."
    )
    failing: list[CriterionView]
    inapplicable: list[CriterionView] = Field(
        default=[],
        description="Criteria declared unmeetable here, with their reasons.",
    )
    recommendations: list[RecommendationView] = Field(
        default=[],
        description="What would close each failing criterion (FR-G18).",
    )
    recorded: bool
    checked_at: datetime | None
    detail: str | None = Field(
        default=None,
        description=(
            "Why this answer is the answer, where the status alone would not "
            "say: a project that has not adopted the contract, or a root path "
            "that could not be read."
        ),
    )


class ComplianceParams(Params):
    project: str


class ComplianceResult(Result):
    project: str
    status: str
    contract_version: str
    checked_at: datetime | None
    age_seconds: int | None = Field(
        description="How old this answer is. Never refreshed implicitly."
    )
    failing: list[CriterionView] = []
    adopted: bool = Field(
        default=False,
        description=(
            "Whether this project opted into the contract (FR-G16). A project "
            "that has not is `not_applicable`, which is not a fault."
        ),
    )
    adopted_at: datetime | None = None
    inapplicable: list[CriterionView] = []
    detail: str | None = None


class ContractAdoptParams(Params):
    """Opt a registered project into the contract (FR-G16)."""

    project: str = Field(description="A registered project slug.")
    reason: Reason


class ContractAdoptResult(Result):
    project: str
    adopted: bool
    adopted_at: datetime | None
    status: str = Field(
        description="What compliance reports for this project after the change."
    )
    detail: str


class ContractInapplicableParams(Params):
    """Declare that a criterion cannot apply to a project (FR-G19)."""

    project: str = Field(description="A registered project slug.")
    rule: str = Field(
        description="The criterion's rule, as an evaluation reports it: "
        "`required_file` or `required_dir`."
    )
    target: str = Field(
        description="The criterion's target, as an evaluation reports it: "
        "`LICENSE`, `src`, `design`."
    )
    reason: Reason


class ContractApplicableParams(Params):
    """Withdraw an inapplicability declaration (FR-G19)."""

    project: str
    rule: str
    target: str
    reason: Reason


class ContractExemptionView(Result):
    rule: str
    target: str
    reason: str
    declared_by: str
    declared_at: datetime


class ContractExemptionResult(Result):
    project: str
    declared: bool = Field(
        description="True when the criterion is now inapplicable here."
    )
    exemptions: list[ContractExemptionView]
    detail: str


class ScaffoldProjectParams(Params):
    """Lay the contract's skeleton into an already-registered project."""

    project: str = Field(description="A registered project slug.")
    reason: Reason


class ScaffoldProjectResult(Result):
    project: str
    root_path: str
    created: list[str] = Field(
        description="Paths written, relative to the project's root."
    )
    skipped: list[str] = Field(
        description="Paths already present and therefore left exactly as they were."
    )
    detail: str


# -- drift -----------------------------------------------------------------


class DriftDetectParams(Params):
    auto_accept: bool = Field(
        default=True,
        description=(
            "Apply the shipped low-risk policy: state-sync kinds are accepted "
            "automatically, destructive or structural ones never are (FR-R3)."
        ),
    )
    reason: Reason


class DriftDetectResult(Result):
    raised: list[DriftProposal]
    auto_accepted: list[str]
    already_open: int = Field(
        description="Findings that already had an open proposal, so were not re-raised."
    )
    superseded: list[str] = Field(
        default=[],
        description=(
            "Open proposals this run marked as raised under evidence a later "
            "sweep no longer reproduces (FR-R6). They are still open and "
            "still need a person; the flag only tells the inbox which ones "
            "are worth reading first."
        ),
    )
    not_collected: list[str] = Field(
        default=[],
        description=(
            "Registered projects no collector has ever swept. Nothing could "
            "be raised for them, which is a different answer from finding no "
            "drift there (FR-O4)."
        ),
    )
    auto_acceptable_kinds: list[str]


class DriftListParams(Params):
    status: str | None = Field(
        default="open", description="open / accepted / rejected / contested."
    )
    kind: str | None = None
    project: str | None = None
    limit: int = Field(default=100, ge=1, le=500)


class DriftListResult(Result):
    """The drift inbox.

    `freshness` is load-bearing here rather than decorative: an empty inbox
    is reassuring only if something has looked recently. Without it, a
    collector that stopped running reads as "no drift" (FR-U2).
    """

    proposals: list[DriftProposal]
    human_gated: dict[str, str] = Field(
        default={},
        description="Kinds the default policy never auto-accepts, and why.",
    )
    freshness: Freshness = Freshness()


class DriftResolveParams(Params):
    id: str
    resolution: Literal["accepted", "rejected", "contested"]
    reason: Reason


class DriftResult(Result):
    proposal: DriftProposal
    change_applied: bool


# -- auth ------------------------------------------------------------------


class IssueTokenParams(Params):
    actor: Name = Field(description="Actor identity_ref the token is bound to.")
    name: Name = Field(description="What this token is for, e.g. 'claude-code'.")
    scopes: Name = Field(
        default="read",
        description=(
            "Comma-separated: read, work.write, project.write, admin, writeback."
        ),
    )
    expires_in_days: int | None = Field(
        default=None,
        ge=1,
        le=3650,
        description="Omit for a token that does not expire.",
    )
    reason: Reason


class IssueTokenResult(Result):
    token: Token
    secret: str = Field(
        description="Shown once. Not stored, not recoverable — rotate if lost."
    )
    warning: str


class TokenResult(Result):
    token: Token


class ListTokensParams(Params):
    include_revoked: bool = False
    limit: int = Field(default=100, ge=1, le=500)


class TokenListResult(Result):
    tokens: list[Token]


class RevokeTokenParams(Params):
    id: str
    reason: Reason


class AuthDecisionListParams(Params):
    decision: Literal["allow", "deny"] | None = Field(
        default=None, description="Filter to allows or denials."
    )
    limit: int = Field(default=100, ge=1, le=500)


class AuthDecisionListResult(Result):
    decisions: list[AuthDecision]


# -- lifecycle -------------------------------------------------------------


class ServeParams(Params):
    host: str = Field(
        description=(
            "Listen address. No default anywhere — it encodes exposure "
            "(NFR-D2). The compose file supplies it."
        )
    )
    port: int = Field(
        ge=1,
        le=65535,
        description="Listen port. No default, for the same reason as host.",
    )
    tls_cert: str | None = Field(
        default=None, description="Operator-owned certificate, mounted read-only."
    )
    tls_key: str | None = None
    no_auth: bool = Field(
        default=False,
        description="Serve without authentication. Only sane on loopback.",
    )
    read_only: bool = Field(
        default=False,
        description="Refuse every write, whatever scope a token holds (FR-S4).",
    )
    no_schedule: bool = Field(
        default=False,
        description=(
            "Do not collect in the background (FR-L3). The schedule is on by "
            "default because an instance that never looks cannot tell stale "
            "evidence from none; this is the switch for a diagnostic run."
        ),
    )


class ServeResult(Result):
    url: str
    api_path: str
    mcp_path: str
    auth_required: bool
    #: Whether background collection is running. Reported rather than
    #: assumed: "the server is up" and "the server is looking" are different
    #: facts, and only one of them keeps freshness small.
    collecting: bool = True
    writes_enabled: bool


class BackupParams(Params):
    destination: str | None = Field(
        default=None, description="Defaults to <data-dir>/backups/<timestamp>."
    )
    label: str | None = None
    reason: Reason


class BackupResult(Result):
    """What a backup covered, including the parts it could not (NFR-I6)."""

    path: str
    instance_id: str
    declared_schema_version: int
    observed_schema_version: int
    taken_at: datetime
    engine_state: str = Field(
        default="not configured",
        description=(
            "What happened to the session engine's state directory: copied, "
            "not configured, or a failure. Stated rather than implied — a "
            "backup that quietly covered two thirds of the product would be "
            "indistinguishable from one that covered all of it until "
            "somebody restored it."
        ),
    )
    import_root: str | None = Field(
        default=None,
        description=(
            "Where imported projects lived when this was taken. A restore "
            "elsewhere leaves every project pointing at a path that is not "
            "there (FR-E3)."
        ),
    )


class RestoreParams(Params):
    source: str = Field(description="A backup directory containing manifest.json.")
    confirm: bool = Field(
        default=False, description="Required: this replaces the live stores."
    )
    reason: Reason


class RestoreResult(Result):
    """What came back, and whether the estate is still where it was."""

    source: str
    instance_id: str
    restored_from: datetime
    migrations_applied: list[str]
    declared_schema_version: int
    engine_state: str = Field(
        default="not in this backup",
        description="What happened to the session engine's state directory.",
    )
    import_root_then: str | None = Field(
        default=None,
        description="Where imported projects lived when the backup was taken.",
    )
    import_root_now: str | None = Field(
        default=None,
        description=(
            "Where they will be looked for now. A difference is not an error "
            "and is not corrected — the paths are in the store — but it is "
            "the reason a restored session will not open, so it is reported "
            "here rather than discovered there (FR-E3)."
        ),
    )


class ExportParams(Params):
    destination: str
    reason: Reason


class ExportResult(Result):
    path: str
    projects: int
    work_items: int


class ImportParams(Params):
    source: str
    reason: Reason


class ImportResult(Result):
    source: str
    instance_id: str
    projects: int
    work_items: int
    applied: bool
    detail: str


# -- forge module (M5) -----------------------------------------------------


class SetWriteBackParams(Params):
    project: str
    policy: Literal["none", "comment_only", "full"] = Field(
        description=(
            "none: observe and never speak. comment_only: post comments "
            "authored here. full: also create, label, and close/reopen. "
            "Never deletion, history rewriting or force (FR-B4)."
        )
    )
    reason: Reason


class WriteBackActionView(Result):
    id: str
    at: datetime
    action: str
    subject_key: str | None
    policy: str
    outcome: str
    reason: str
    detail: str | None = None
    source_url: str | None = None


class WriteBackListParams(Params):
    outcome: Literal["attempted", "succeeded", "failed", "skipped"] | None = None
    limit: int = Field(default=100, ge=1, le=500)


class WriteBackListResult(Result):
    actions: list[WriteBackActionView]


class ForgeAccountLinkParams(Params):
    """Link the acting actor's own forge account by pasting a PAT (#179).

    The token is validated against the forge, then stored encrypted at rest
    and never echoed. Linking arms upstream writes attributed to *you*: the
    scope of what those writes can do is the scope of the token you paste.
    """

    host: str = Field(
        default="github.com",
        description="The forge host to link. Only github.com is supported in v1.",
    )
    token: str = Field(
        min_length=1,
        description=(
            "The Personal Access Token. Validated, then stored encrypted at "
            "rest under `forge_account_key_file` and never returned by any "
            "surface. To revoke it, unlink here and revoke it upstream too."
        ),
    )
    reason: Reason = Field(description="Why this write is being made (audited).")


class ForgeAccountUnlinkParams(Params):
    host: str = Field(
        default="github.com",
        description="The forge host to unlink. Deletes the stored token.",
    )
    reason: Reason = Field(description="Why this write is being made (audited).")


class ForgeAccountStatusParams(Params):
    host: str | None = Field(
        default=None,
        description="Restrict to one host; unset lists every host you linked.",
    )


class ForgeAccountView(Result):
    """One linked account, never carrying the token."""

    host: str
    login: str
    scopes: str = Field(
        description=(
            "The token's granted scopes as the forge reports them "
            "(`X-OAuth-Scopes`), or empty when the forge did not say."
        )
    )
    linked: bool = True


class ForgeAccountResult(Result):
    """The outcome of a link or unlink — status only, never the token."""

    host: str
    login: str | None = Field(
        default=None,
        description="The linked login; null once unlinked.",
    )
    scopes: str = ""
    linked: bool


class ForgeAccountStatusResult(Result):
    accounts: list[ForgeAccountView]


class OnboardParams(Params):
    project: str = Field(description="Project slug to consolidate.")
    max_pages: int = Field(
        default=10,
        ge=1,
        le=100,
        description=(
            "How far back to walk. Bounded so a busy repository cannot turn "
            "onboarding into an unbounded run; coverage reports what was "
            "actually swept."
        ),
    )
    reason: Reason


class OnboardResult(Result):
    project: str
    repo: str | None
    issues: int
    pull_requests: int
    labels: int
    releases: int
    new: int
    unchanged: int
    mutations: int = Field(
        default=0,
        description=(
            "Always zero. Onboarding is read-only (FR-B3); the field exists "
            "so the claim is asserted rather than assumed."
        ),
    )
    supported: bool = Field(
        default=True,
        description=(
            "Whether an adapter could read this repository's host at all. "
            "False makes the zeros below meaningless rather than informative: "
            "nothing was read, so nothing was found, and `detail` says why. "
            "The import playbook treats an empty consolidation as a signal, "
            "and this is what makes that signal readable."
        ),
    )
    detail: str | None = None


class ImportProjectParams(Params):
    """Bring a repository that lives on GitHub into Vogt (FR-P6).

    `repo` is named by the caller and is never chosen from a list: there is
    no listing operation, and adding one would be the registration-candidate
    listing r3 removed (FR-G15, `REQUIREMENTS.md` §3).
    """

    repo: str = Field(
        description="Repository to import: `owner/name`, or any GitHub URL.",
        min_length=1,
    )
    name: str | None = Field(
        default=None,
        description="Display name. Defaults to the repository's own name.",
    )
    root_path: str | None = Field(
        default=None,
        description=(
            "Where to clone to. Defaults to `<import_root>/<slug>`; supply "
            "this only when one repository must live somewhere specific."
        ),
    )
    lifecycle_state: LifecycleState = "active"
    consolidate: bool = Field(
        default=True,
        description=(
            "Read existing issues, PRs, labels and releases after "
            "registering (FR-B3). Read-only, and on by default because a "
            "project that arrives empty looks like a project with no work."
        ),
    )
    reason: Reason = Field(description="Why this write is being made (audited).")


class ImportProjectResult(Result):
    project: Project
    remote: str = Field(description="The remote that was cloned, without credentials.")
    root_path: str
    revision: str | None = Field(
        default=None, description="HEAD at the moment of import."
    )
    default_branch: str | None = None
    cloned: bool = Field(
        default=True,
        description=(
            "False when the destination already held a clone of the same "
            "remote and was registered as it stood (FR-P7)."
        ),
    )
    consolidated: OnboardResult | None = Field(
        default=None,
        description="What the read-only consolidation found, if it ran.",
    )
    detail: str | None = None


# -- notifications (FR-N3) -------------------------------------------------


class NotificationsParams(Params):
    """Filters over the collected forge inbox (FR-N3)."""

    project: str | None = Field(
        default=None, description="Project slug. Omit for every registered project."
    )
    reason: str | None = Field(
        default=None,
        description="GitHub's reason, e.g. mention, review_requested, ci_activity.",
    )
    unread_only: bool = Field(
        default=False,
        description="Only threads GitHub still considers unread for this token.",
    )
    limit: int = Field(default=50, ge=1, le=500)
    offset: int = Field(default=0, ge=0)


class NotificationView(Result):
    """One collected notification, flattened for reading."""

    thread: str
    project_slug: str | None = None
    repo: str | None = None
    title: str
    reason: str | None = None
    subject_type: str | None = None
    unread: bool = False
    url: str | None = None
    updated_at: datetime | None = None
    observed_at: datetime


class NotificationsResult(Result):
    """The inbox, and what it is honestly able to be.

    `freshness` is here for the same reason every other aggregate carries one
    (FR-U2): an empty inbox with no sweep behind it means nobody has looked.
    `scope` states out loud that these belong to the configured token's
    account rather than to the reading actor — a limit of the design, not of
    this response (FR-N3).
    """

    notifications: list[NotificationView]
    total: int
    by_reason: dict[str, int] = {}
    unread: int = 0
    scope: str = Field(
        default=(
            "the GitHub account whose token this instance is configured with; "
            "notifications are instance-scoped, not per-actor"
        ),
        description="Whose inbox this is.",
    )
    freshness: Freshness = Freshness()
    detail: str | None = None


# -- connecting a client (FR-A8) -------------------------------------------


ClientKind = Literal["http", "bridge"]


class ConnectParams(Params):
    """What a client needs in order to reach this instance (FR-A8)."""

    client: ClientKind = Field(
        default="http",
        description=(
            "`http` for a client that speaks streamable HTTP MCP — the "
            "ordinary case, and the one that needs nothing installed. "
            "`bridge` for a client that can only spawn a local process."
        ),
    )
    format: Literal["json", "markdown"] = Field(
        default="json",
        description=(
            "`markdown` renders the connection document `DEPLOYMENT.md` §4.3 "
            "describes; redirect it to CONNECTING.md if you want it on disk."
        ),
    )


class ConnectResult(Result):
    """The connection facts, and the client configuration built from them.

    `url` is `None` when nobody has configured one. That is deliberately not
    a guess: a URL the server invented would be wrong in exactly the
    deployment this field exists for, and a client cannot tell a wrong URL
    from an unreachable one.
    """

    url: str | None = None
    api_path: str
    mcp_path: str
    mcp_url: str | None = None
    supported_mcp_protocol_versions: list[str]
    client: ClientKind
    requires_install: bool = Field(
        description=(
            "Whether the client needs Vogt's own code present. False for "
            "streamable HTTP, which is why it is the recommended path."
        )
    )
    configuration: str = Field(
        description="Ready to use: JSON for a client config, or the document."
    )
    detail: str | None = None


# -- coding sessions -------------------------------------------------------


class StartSessionParams(Params):
    """Open a terminal for a work item, or for a project.

    At most one of `work_item` and `project`. A session always belongs to a
    project — the work item's own, when one is given — because the working
    directory comes from the project registry and nowhere else (FR-E3).

    *(r16, FR-T11)* Giving **neither** is allowed only when the deployment has
    configured a scratch project, and resolves to it. It exists for the spoken
    request with no subject — "research the best risotto in Wollongong" — which
    has no work item and no repository but still needs a registered tree to
    open in. The scratch project is a project like any other: registered, with
    a root path somebody chose. What is *not* allowed is inventing a directory,
    which is the failure FR-E3 exists against.
    """

    work_item: str | None = Field(default=None, description="Work item ref, e.g. WI-7.")
    project: str | None = Field(default=None, description="Project slug.")
    template: str | None = Field(
        default=None,
        description=(
            "Named session template to run, e.g. an agent CLI. Omitted means "
            "the engine's default shell."
        ),
    )
    name: str | None = Field(
        default=None, description="Session name. Derived from the subject if omitted."
    )
    model: str | None = Field(
        default=None,
        description=(
            "Model id for the agent CLI this template runs, e.g. "
            "'gpt-5.6' or 'claude-sonnet-4-5'. Omitted means the CLI's own "
            "default. A template that cannot be told which model to use "
            "refuses rather than ignoring this."
        ),
    )
    effort: str | None = Field(
        default=None,
        description=(
            "Reasoning effort for the agent CLI, e.g. 'low' / 'medium' / "
            "'high'. Omitted means the CLI's own default."
        ),
    )
    reason: Reason = Field(description="Why this write is being made (audited).")


class StopSessionParams(Params):
    id: str = Field(description="Session id, e.g. ses_01J8… .")
    reason: Reason = Field(description="Why this write is being made (audited).")


class ListSessionsParams(Params):
    project: str | None = Field(default=None, description="Project slug.")
    work_item: str | None = Field(default=None, description="Work item ref.")
    include_stopped: bool = Field(
        default=False, description="Include sessions Vogt has already stopped."
    )
    limit: int = Field(default=50, ge=1, le=500)
    offset: int = Field(default=0, ge=0)


class SessionResult(Result):
    session: SessionSummary


class SessionListResult(Result):
    sessions: list[SessionSummary] = []
    engine: str | None = Field(
        default=None,
        description=(
            "What the engine said, when it could not be asked. The links are "
            "still returned: Vogt's record of what it started does not depend "
            "on the engine being up (FR-E9)."
        ),
    )
