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
    project: str
    references_out: list[DepRef]
    referenced_by: list[DepRef]
    unresolved: int = 0


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
    proposals: list[DriftProposal]
    human_gated: dict[str, str] = Field(
        default={},
        description="Kinds the default policy never auto-accepts, and why.",
    )


class DriftResolveParams(Params):
    id: str
    resolution: Literal["accepted", "rejected", "contested"]
    reason: Reason


class DriftResult(Result):
    proposal: DriftProposal
    change_applied: bool
