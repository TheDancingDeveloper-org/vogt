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
#: `not_applicable` is not stored on a project row: it is what a project that
#: never adopted the contract *reports* (FR-G16). The three stored values are
#: what a check found; the fourth is what a reader is told when no check was
#: ever asked for.
ComplianceStatus = Literal[
    "compliant", "non_compliant", "not_checked", "not_applicable"
]
TrustState = Literal["verified", "stale", "unverified", "disputed"]

#: Whether a project's work model is the forge's (#181, decision 1). Set by
#: an explicit act — `project.import`'s clone+consolidate, `forge.link`, and
#: (from #182) `forge.publish` — never inferred per call from what tokens
#: happen to resolve. On a `linked` project the work items *are* the upstream
#: issues joined to the local overlay, and the write verbs write through.
LinkState = Literal["unlinked", "linked"]

WorkKind = Literal["feature", "bug", "chore", "question"]
Priority = Literal["p0", "p1", "p2", "p3", "p4"]
Effort = Literal["xs", "s", "m", "l", "xl"]
Origin = Literal["created", "adopted", "observed"]
#: The typed edges out of a work item. The first four are declared by hand
#: (`work relate`). `implemented_by` is the exception: it is **observed only**
#: (#284) — the forge sync reads it from a PR's closing keywords and branch
#: name and it points at a PR subject, so `work relate` refuses it. Unlike
#: `depends_on` it never blocks completion; it informs, it does not enforce.
RelationKind = Literal[
    "depends_on", "relates_to", "duplicate_of", "parent_of", "implemented_by"
]

#: Relation kinds a person may declare by hand. `implemented_by` is absent:
#: it is a fact the forge reports, never one typed in (#284).
DECLARABLE_RELATION_KINDS: frozenset[str] = frozenset(
    {"depends_on", "relates_to", "duplicate_of", "parent_of"}
)
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
    #: When this project opted into the contract (FR-G16). Null is the
    #: ordinary case and carries no fault: the contract is something a
    #: project adopts, not something it is measured against by default.
    contract_adopted_at: datetime | None = None
    write_back: Literal["none", "comment_only", "full"] = "none"
    #: `linked` means this project's work items are its upstream issues
    #: (#181). Persisted, and set only by an explicit act — import's
    #: clone+consolidate or `forge.link` — so "is this linked" has one
    #: answer everywhere rather than one per credential lookup.
    link_state: LinkState = "unlinked"
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
    #: Set when the native item migrated upstream on link/publish (#183): the
    #: subject key that is now the item. A superseded row is retired, not
    #: deleted — it anchors its comments, relations, ledger rows and audit
    #: trail, resolves by ref for anyone following an old trail, and is
    #: excluded from every work view so each issue is counted exactly once.
    superseded_by: str | None = None
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


class ContractExemption(Entity):
    """A declared statement that a criterion cannot apply to a project.

    FR-G19: a criterion a project *has not met* and one it *cannot meet by
    construction* are different facts, and a single word for both makes the
    word useless. A Cargo workspace has no root `src/`; saying so is a
    declaration with an author and a reason, not a silent exemption, which is
    why this is a declared-store row and an audited write rather than a
    heuristic about the ecosystem.
    """

    id: str
    project_id: str
    project_slug: str | None = None
    rule: str
    target: str
    reason: str
    declared_by: str
    declared_at: datetime


class WorkOverlay(Entity):
    """The vogt-local half of an upstream-truth work item (#181, decision 2).

    Keyed by the upstream subject key, not a `wrk_*` id, because on a linked
    project there is no declared row: the observed mirror is the truth for
    title/body/labels/open-closed, and this carries only what must never
    cross the boundary — rank, a workflow state richer than open/closed, and
    priority/effort/assignee/initiative. Writing any of it produces zero
    provider calls, and that invariant is pinned by test.
    """

    subject_key: str
    project_id: str
    rank: float | None = None
    workflow_state: str | None = None
    priority: Priority | None = None
    effort: Effort | None = None
    assignee_actor_id: str | None = None
    initiative_id: str | None = None
    created_at: datetime
    updated_at: datetime


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
    #: What was *asked for* (FR-T11), not what the agent is using now. A
    #: session whose operator typed `/model` at the prompt has moved on and
    #: this field will not know — the same line FR-E2 draws around activity.
    model: str | None = None
    effort: str | None = None
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


class ForgeAccount(Entity):
    """An actor's own forge identity, linked so writes are attributed to them.

    Deliberately carries no token field. The encrypted PAT lives only in the
    `forge_accounts.encrypted_token` column and is read through a dedicated
    secret accessor, so this entity — which crosses every transport boundary —
    can never carry a credential back out. `login` and `scopes` are the
    cleartext columns a status read needs without touching the key (#179).
    """

    actor_id: str
    host: str
    login: str
    scopes: str
    created_at: datetime
    updated_at: datetime


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
    superseded_at: datetime | None = None
    """When a later sweep stopped reproducing the condition that raised this.

    Set on an *open* proposal, and cleared again if the condition comes back.
    It resolves nothing: the proposal is still open, still human-gated
    (FR-R2), and still carries the snapshot it was raised with. It says only
    that fresher evidence exists and no longer says what the old evidence
    said — the reconciliation somebody did by hand across thirty-six
    proposals after WI-2's fix landed (#48).
    """
    superseded_detail: str | None = None
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
