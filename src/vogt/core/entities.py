"""Declared-store entities.

These are the shapes the application layer reads and writes, and — because
every surface is generated from one registry — they are also the shapes the
REST, CLI and MCP surfaces return. One definition, three transports.
"""

from __future__ import annotations

from datetime import datetime
from typing import Annotated, Literal

from pydantic import BaseModel, ConfigDict, StringConstraints

from vogt.core.principal import ActorKind

LifecycleState = Literal["incubating", "active", "maintenance", "archived"]
ComplianceStatus = Literal["compliant", "non_compliant", "not_checked"]
TrustState = Literal["verified", "stale", "unverified", "disputed"]

WorkKind = Literal["feature", "bug", "chore", "question"]
Priority = Literal["p0", "p1", "p2", "p3", "p4"]
Effort = Literal["xs", "s", "m", "l", "xl"]
Origin = Literal["created", "adopted", "observed"]
RelationKind = Literal["depends_on", "relates_to", "duplicate_of", "parent_of"]
InitiativeState = Literal["open", "closed"]

#: A reason is required on every audited write and may not be blank.
#: Whitespace-only reasons are a way of technically complying while
#: explaining nothing, so they are stripped and then rejected.
Reason = Annotated[str, StringConstraints(strip_whitespace=True, min_length=1)]

#: A non-empty single-line name.
Name = Annotated[str, StringConstraints(strip_whitespace=True, min_length=1)]


class Entity(BaseModel):
    """Base for entities that cross a transport boundary."""

    model_config = ConfigDict(extra="forbid", frozen=True)


class Actor(Entity):
    """A human or an agent. Both are first-class (FR-W7)."""

    id: str
    kind: ActorKind
    display_name: str
    identity_ref: str
    disabled: bool = False
    created_at: datetime


class Project(Entity):
    """One explicitly registered repository or folder (FR-P1, FR-P5)."""

    id: str
    slug: str
    name: str
    root_path: str
    repo_url: str | None = None
    lifecycle_state: LifecycleState = "active"
    current_version: str | None = None
    contract_version: str | None = None
    compliance_status: ComplianceStatus = "not_checked"
    compliance_checked_at: datetime | None = None
    write_back: Literal["none", "comment_only", "full"] = "none"
    exclusions: list[str] = []
    trust_state: TrustState = "unverified"
    created_at: datetime
    updated_at: datetime


class Label(Entity):
    """A free-form tag, shared per instance and GitHub-label aligned (FR-W9)."""

    id: str
    name: str
    color: str | None = None
    created_at: datetime


class Initiative(Entity):
    """A cross-project epic, carrying weight that feeds ranking (FR-W3)."""

    id: str
    slug: str
    title: str
    body: str = ""
    state: InitiativeState = "open"
    weight: int = 0
    created_at: datetime
    updated_at: datetime


class Relation(Entity):
    """One typed edge out of a work item (FR-W8)."""

    kind: RelationKind
    related_id: str
    related_ref: str
    related_title: str
    related_state: str


class Comment(Entity):
    """A remark on a work item, attributed to an actor (FR-W6)."""

    id: str
    work_item_id: str
    actor_id: str
    actor_display_name: str
    body: str
    created_at: datetime


class WorkItem(Entity):
    """The unit of work.

    `ref` is the short human- and agent-facing handle (`WI-7`). Ids are
    ULID-shaped and stable; refs are what somebody actually types.
    """

    id: str
    ref: str
    kind: WorkKind
    title: str
    body: str = ""
    state: str
    priority: Priority = "p2"
    effort: Effort | None = None
    project_id: str | None = None
    project_slug: str | None = None
    initiative_id: str | None = None
    origin: Origin = "created"
    trust_state: TrustState = "unverified"
    assignee_actor_id: str | None = None
    assignee_identity_ref: str | None = None
    labels: list[str] = []
    relations: list[Relation] = []
    created_at: datetime
    updated_at: datetime


class Suppression(Entity):
    """An audited decision that an observed subject is not work (FR-W10).

    Lives in the declared store because it is a decision, not an
    observation — which is exactly why it survives re-observation of the same
    subject, and why a dismissal recorded in the evidence store could never
    have worked.
    """

    id: str
    match_kind: Literal["exact", "pattern"]
    subject_key_or_pattern: str
    scope_project_id: str | None = None
    scope_project_slug: str | None = None
    actor_id: str
    actor_identity_ref: str | None = None
    reason: str
    created_at: datetime
    revoked_at: datetime | None = None
    revoked_reason: str | None = None

    @property
    def active(self) -> bool:
        return self.revoked_at is None


class WorkLink(Entity):
    """The maintained link from adopted work back to what was observed."""

    work_item_id: str
    subject_key: str
    origin_kind: str
    source_url: str | None = None
    relation: Literal["completion", "reference"] = "completion"
    created_at: datetime


class CodingSession(Entity):
    """A terminal the engine runs, and what Vogt started it for (FR-E4).

    The *declared* half of a session: Vogt asked for it, with a reason, and
    the write is audited like any other. The engine keeps the live half —
    activity state, scrollback, exit code — and is asked for it, because a
    cached copy of somebody else's running state is wrong the moment it is
    written (FR-E2).

    `work_item_id` is `None` for a session opened on a project, which is the
    other half of FR-E3: every session has a project and a `cwd` the registry
    recorded, and only some have an item.
    """

    id: str
    engine_session_id: str
    project_id: str
    work_item_id: str | None = None
    actor_id: str
    cwd: str
    template: str | None = None
    reason: str
    started_at: datetime
    stopped_at: datetime | None = None


class Token(Entity):
    """A scoped credential bound to an actor (FR-S3).

    Never carries the secret: only the hash is stored, and the secret is
    shown once at issue time. A model that could round-trip the credential
    is a model that leaks it into logs and API responses.
    """

    id: str
    actor_id: str
    actor_identity_ref: str | None = None
    name: str
    scopes: list[str]
    created_at: datetime
    expires_at: datetime | None = None
    last_used_at: datetime | None = None
    revoked_at: datetime | None = None
    revoked_reason: str | None = None

    @property
    def active(self) -> bool:
        return self.revoked_at is None


class AuthDecision(Entity):
    """One allow or deny, recorded (FR-S5)."""

    id: str
    at: datetime
    decision: Literal["allow", "deny"]
    reason_code: str
    operation: str
    scope: str | None = None
    actor_id: str | None = None
    token_id: str | None = None
    identity_ref: str | None = None
    transport: str
    detail: str | None = None


class WriteBackRecord(Entity):
    """One thing Vogt said to a forge, and what came back (FR-B2).

    Recorded even when it was skipped, because "we did not speak" is the
    answer for almost every project and should be visible as a decision
    rather than as an absence.
    """

    id: str
    at: datetime
    project_id: str | None = None
    work_item_id: str | None = None
    actor_id: str
    action: Literal["create", "comment", "label", "close", "reopen"]
    subject_key: str | None = None
    policy: str
    outcome: Literal["attempted", "succeeded", "failed", "skipped"]
    reason: str
    detail: str | None = None
    source_url: str | None = None


class DriftProposal(Entity):
    """A machine-raised question, resolved by a human or an agent (FR-R1)."""

    id: str
    kind: str
    subject_kind: str
    subject_id: str
    project_id: str | None = None
    project_slug: str | None = None
    summary: str
    evidence_observation_id: str | None = None
    evidence_snapshot: dict[str, object] = {}
    proposed_change: dict[str, object] = {}
    status: Literal["open", "accepted", "rejected", "contested"] = "open"
    opened_at: datetime
    resolved_by_actor_id: str | None = None
    resolved_by_identity_ref: str | None = None
    resolved_at: datetime | None = None
    resolution_reason: str | None = None


SweepOutcome = Literal["running", "ok", "partial", "failed"]
RefKind = Literal["path", "git", "declared", "inherited"]


class Sweep(Entity):
    """A coverage record: which collector looked at what, when, and how it went.

    The thing that makes "absent" different from "not collected" (FR-O3,
    FR-O4). Absence may only be asserted inside a scope a completed sweep
    provably covered.
    """

    id: str
    collector: str
    scope: list[str]
    started_at: datetime
    finished_at: datetime | None = None
    outcome: SweepOutcome = "running"
    stats: dict[str, int] = {}
    detail: str | None = None


class Observation(Entity):
    """One immutable thing a collector found (FR-O2)."""

    id: str
    sweep_id: str
    collector: str
    kind: str
    project_id: str | None = None
    subject_key: str
    payload: dict[str, object] = {}
    content_digest: str
    source_url: str | None = None
    promoted: bool = False
    observed_at: datetime


class InboxTriage(Entity):
    """The shared, audited decision attached to one Inbox occurrence."""

    entry_key: str
    state: Literal["active", "archived", "snoozed"] = "active"
    snooze_until: datetime | None = None
    actor_id: str
    actor_identity_ref: str | None = None
    decided_at: datetime
    occurrence_snapshot: dict[str, object] = {}


class DepRef(Entity):
    """One reference from a project to another project (FR-D1–D4).

    Records *which projects reference which*, by filesystem path or
    repository URL, and stops there: no lockfile is parsed and no package
    version is resolved (r2 decision, DESIGN §3.5).
    """

    subject_key: str
    from_project_id: str
    from_project_slug: str | None = None
    ref_kind: RefKind
    raw_target: str
    manifest: str | None = None
    to_project_id: str | None = None
    to_project_slug: str | None = None
    observed_at: datetime


class AuditRecord(Entity):
    """One declared write, explained (FR-S1)."""

    id: str
    txn_id: str
    revision: int
    actor_id: str
    actor_identity_ref: str
    operation: str
    entity_kind: str
    entity_id: str
    reason: str
    payload_digest: str
    at: datetime


class Event(Entity):
    """One row of the ordered notification feed (FR-N1).

    `seq` is the `/events` cursor — a single monotonic sequence over a single
    table, so no client ever merges orderings across the two stores.
    """

    seq: int
    kind: str
    entity_kind: str
    entity_id: str
    actor_id: str | None = None
    audit_id: str | None = None
    summary: dict[str, object] = {}
    at: datetime
