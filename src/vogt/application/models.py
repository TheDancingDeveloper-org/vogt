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
)


class Params(BaseModel):
    """Base for operation parameters."""

    model_config = ConfigDict(extra="forbid")


class Result(BaseModel):
    """Base for operation results."""

    model_config = ConfigDict(extra="forbid")


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
    """The CI story for a project (FR-O6), or the absence of one."""

    status: Literal["not_collected", "no_checks", "passing", "failing"] = (
        "not_collected"
    )
    checks: int = 0
    failing: list[str] = []
    revision: str | None = None
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
    open_work: int
    open_bugs: int
    by_state: dict[str, int]
    by_kind: dict[str, int]
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
    limit: int = Field(default=20, ge=1, le=200)


class BacklogResult(Result):
    items: list[RankedItem]
    total_considered: int
    declared: int = 0
    observed: int = 0
    suppressed: int = 0
    scope: str
    freshness: Freshness


class BugsParams(Params):
    project: str | None = None
    priorities: list[Priority] | None = None
    assignee: str | None = None
    label: str | None = None
    limit: int = Field(default=50, ge=1, le=200)


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


class EventListResult(Result):
    events: list[Event]
    next_cursor: int


class ListAuditParams(Params):
    limit: int = Field(default=50, ge=1, le=500)
    actor_id: str | None = None
    operation: str | None = None
    entity_id: str | None = None


class AuditListResult(Result):
    records: list[AuditRecord]


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
    projects: int = 0
    detail: str | None = None


class CoverageResult(Result):
    collectors: list[CoverageEntry]
    swept_project_ids: list[str]


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
    total: int


class DepsParams(Params):
    project: str = Field(description="Project slug.")


class DepsResult(Result):
    """The dependency graph around one project.

    Carries `freshness` because a graph is an aggregate over sweeps, and an
    empty graph with no sweep behind it means "not collected", not "this
    project depends on nothing" (FR-O4, FR-U2).
    """

    project: str
    references_out: list[DepRef]
    referenced_by: list[DepRef]
    unresolved: int = 0
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


class ContractCheckParams(Params):
    path: str | None = Field(
        default=None,
        description="Any folder or repository, registered or not. Stores nothing.",
    )
    project: str | None = Field(
        default=None,
        description="A registered project slug. Records the result with its age.",
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
    recorded: bool
    checked_at: datetime | None


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
    detail: str | None = None


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
    path: str
    instance_id: str
    declared_schema_version: int
    observed_schema_version: int
    taken_at: datetime


class RestoreParams(Params):
    source: str = Field(description="A backup directory containing manifest.json.")
    confirm: bool = Field(
        default=False, description="Required: this replaces the live stores."
    )
    reason: Reason


class RestoreResult(Result):
    source: str
    instance_id: str
    restored_from: datetime
    migrations_applied: list[str]
    declared_schema_version: int


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

    Exactly one of `work_item` and `project` is required. A session always
    belongs to a project — the work item's own, when one is given — because
    the working directory comes from the project registry and nowhere else
    (FR-E3).
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
