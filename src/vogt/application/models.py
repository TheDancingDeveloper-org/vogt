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

from pydantic import BaseModel, ConfigDict, Field

from vogt.core.entities import (
    Actor,
    AuditRecord,
    Comment,
    Effort,
    Event,
    Initiative,
    InitiativeState,
    Label,
    LifecycleState,
    Name,
    Priority,
    Project,
    Reason,
    RelationKind,
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


class NotCollected(Result):
    """A value no collector has produced yet.

    Present from M1 so that the shape of an answer does not change when M2
    starts filling it in, and so "we have not looked" is visibly different
    from "there is nothing" (FR-O4, DESIGN §6).
    """

    status: str = "not_collected"
    detail: str


class RankedItem(Result):
    """A work item with its score, as ranked views return it."""

    item: WorkItem
    score: float


class ProjectBriefParams(Params):
    slug: str
    backlog_limit: int = Field(default=10, ge=1, le=100)


class ProjectBriefResult(Result):
    """The per-repo view in one call (FR-P2)."""

    project: Project
    open_work: int
    open_bugs: int
    by_state: dict[str, int]
    by_kind: dict[str, int]
    top_backlog: list[RankedItem]
    current_version: str | None
    compliance_status: str
    compliance_checked_at: datetime | None
    ci_status: NotCollected
    dependencies: NotCollected
    freshness: NotCollected


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
    scope: str
    freshness: NotCollected


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
