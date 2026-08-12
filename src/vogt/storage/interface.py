"""The storage interface the application layer is allowed to know about.

NFR-S3: nothing above this line may depend on SQLite-only semantics, so that
a Postgres backend stays possible behind the same interface. Concretely that
means no rowids, no `INSERT OR REPLACE`, no dynamic typing, and no SQL
anywhere outside a backend package.
"""

from __future__ import annotations

from contextlib import AbstractContextManager
from dataclasses import dataclass, field
from datetime import datetime
from typing import Protocol

from vogt.core.entities import (
    Actor,
    AuditRecord,
    Comment,
    DepRef,
    Event,
    Initiative,
    Label,
    Observation,
    Project,
    RelationKind,
    Suppression,
    Sweep,
    SweepOutcome,
    WorkItem,
    WorkLink,
)
from vogt.core.principal import Principal
from vogt.core.workflow import Workflow
from vogt.storage.observed_types import (
    AppendStats,
    DepRefRow,
    PendingObservation,
    PruneReport,
)


@dataclass(frozen=True)
class MigrationReport:
    """What `migrate()` did, so `status` and CI can say it plainly."""

    store: str
    applied: tuple[str, ...]
    version: int


@dataclass(frozen=True)
class Counts:
    """Row counts behind `status`."""

    projects: int
    actors: int
    events: int
    audit: int
    work_items: int
    initiatives: int


@dataclass(frozen=True)
class BootstrapResult:
    """The outcome of creating an instance."""

    instance_id: str
    actor: Actor


@dataclass(frozen=True)
class WorkFilter:
    """How the views narrow the work set.

    One object rather than a dozen keyword arguments because every ranked
    view, the per-project brief and the bug view all filter the same way, and
    they must keep filtering the same way as filters are added.
    """

    project_id: str | None = None
    kinds: tuple[str, ...] = ()
    states: tuple[str, ...] = ()
    priorities: tuple[str, ...] = ()
    assignee_actor_id: str | None = None
    initiative_id: str | None = None
    label: str | None = None
    trust_states: tuple[str, ...] = ()
    exclude_terminal: bool = False
    limit: int = 100
    offset: int = 0


@dataclass(frozen=True)
class Blocker:
    """An unfinished `depends_on` target, named so a rejection can list it."""

    ref: str
    state: str


@dataclass(frozen=True)
class ProjectUpdate:
    """Fields of a project a write may change. Unset fields are untouched."""

    lifecycle_state: str | None = None
    repo_url: str | None = None
    current_version: str | None = None
    compliance_status: str | None = None
    compliance_checked_at: datetime | None = None
    exclusions: tuple[str, ...] | None = None


@dataclass(frozen=True)
class WorkItemUpdate:
    """Fields of a work item a write may change. Unset fields are untouched.

    `None` means "leave alone"; clearing a nullable field is expressed by the
    matching `clear_*` flag, because otherwise unassigning somebody and not
    mentioning the assignee are the same request.
    """

    title: str | None = None
    body: str | None = None
    state: str | None = None
    priority: str | None = None
    effort: str | None = None
    assignee_actor_id: str | None = None
    initiative_id: str | None = None
    project_id: str | None = None
    clear_effort: bool = False
    clear_assignee: bool = False
    clear_initiative: bool = False
    add_labels: tuple[str, ...] = field(default=())
    remove_labels: tuple[str, ...] = field(default=())


class ReadView(Protocol):
    """Read access to the declared store within one consistent snapshot."""

    def instance_id(self) -> str: ...

    def current_revision(self) -> int: ...

    def counts(self) -> Counts: ...

    # -- identity ----------------------------------------------------------

    def actor_by_identity(self, identity_ref: str) -> Actor | None: ...

    def actor_by_id(self, actor_id: str) -> Actor | None: ...

    def list_actors(self, *, limit: int, offset: int) -> list[Actor]: ...

    # -- projects ----------------------------------------------------------

    def project_by_slug(self, slug: str) -> Project | None: ...

    def project_by_id(self, project_id: str) -> Project | None: ...

    def list_projects(self, *, limit: int, offset: int) -> list[Project]: ...

    # -- work --------------------------------------------------------------

    def work_item_by_id(self, work_item_id: str) -> WorkItem | None: ...

    def work_item_by_ref(self, ref: str) -> WorkItem | None: ...

    def list_work_items(self, work_filter: WorkFilter) -> list[WorkItem]: ...

    def count_work_items(self, work_filter: WorkFilter) -> int: ...

    def blocking_fan_out(self, work_item_ids: list[str]) -> dict[str, int]:
        """How many items declare `depends_on` each of these."""
        ...

    def unfinished_blockers(
        self, work_item_id: str, *, terminal_states: tuple[str, ...]
    ) -> list[Blocker]: ...

    def comments_for(self, work_item_id: str, *, limit: int) -> list[Comment]: ...

    # -- taxonomy ----------------------------------------------------------

    def label_by_name(self, name: str) -> Label | None: ...

    def list_labels(self, *, limit: int, offset: int) -> list[Label]: ...

    def initiative_by_id(self, initiative_id: str) -> Initiative | None: ...

    def initiative_by_slug(self, slug: str) -> Initiative | None: ...

    def list_initiatives(self, *, limit: int, offset: int) -> list[Initiative]: ...

    def workflow_for(self, kind: str) -> Workflow: ...

    # -- observed-first ----------------------------------------------------

    def list_suppressions(
        self, *, include_revoked: bool = False, limit: int = 100
    ) -> list[Suppression]: ...

    def suppression_by_id(self, suppression_id: str) -> Suppression | None: ...

    def work_links_for_subjects(self, subject_keys: list[str]) -> dict[str, str]:
        """Map subject key to the work item ref that adopted it."""
        ...

    def work_item_by_subject(self, subject_key: str) -> WorkItem | None: ...

    # -- history -----------------------------------------------------------

    def list_events(self, *, after: int, limit: int) -> list[Event]: ...

    def list_audit(
        self,
        *,
        limit: int,
        actor_id: str | None = None,
        operation: str | None = None,
        entity_id: str | None = None,
    ) -> list[AuditRecord]: ...


class WriteTxn(ReadView, Protocol):
    """One atomic declared write (NFR-I1).

    Entity change, audit row, event row and revision bump commit together or
    not at all. The revision and transaction id are allocated when the
    transaction opens, so every row written inside it shares them.
    """

    @property
    def txn_id(self) -> str: ...

    @property
    def revision(self) -> int: ...

    def insert_actor(self, actor: Actor) -> None: ...

    def insert_project(self, project: Project) -> None: ...

    def update_project(
        self, project_id: str, update: ProjectUpdate, *, at: datetime
    ) -> None: ...

    def next_work_ref(self) -> str: ...

    def insert_work_item(self, item: WorkItem) -> None: ...

    def update_work_item(
        self, work_item_id: str, update: WorkItemUpdate, *, at: datetime
    ) -> None: ...

    def insert_relation(
        self,
        *,
        work_item_id: str,
        related_id: str,
        kind: RelationKind,
        at: datetime,
    ) -> None: ...

    def delete_relation(
        self, *, work_item_id: str, related_id: str, kind: RelationKind
    ) -> bool: ...

    def insert_label(self, label: Label) -> None: ...

    def insert_initiative(self, initiative: Initiative) -> None: ...

    def insert_comment(self, comment: Comment) -> None: ...

    def insert_suppression(self, suppression: Suppression) -> None: ...

    def revoke_suppression(
        self, suppression_id: str, *, actor_id: str, reason: str, at: datetime
    ) -> bool: ...

    def insert_work_link(self, link: WorkLink) -> None: ...

    def upsert_workflow(self, workflow: Workflow, *, at: datetime) -> None: ...

    def append_audit(
        self,
        *,
        actor: Actor,
        operation: str,
        entity_kind: str,
        entity_id: str,
        reason: str,
        payload_digest: str,
        at: datetime,
    ) -> AuditRecord: ...

    def append_event(
        self,
        *,
        kind: str,
        entity_kind: str,
        entity_id: str,
        actor_id: str | None,
        audit_id: str | None,
        summary: dict[str, object],
        at: datetime,
    ) -> Event: ...


class DeclaredStore(Protocol):
    """The authoritative store. Only the application layer writes here."""

    def migrate(self) -> MigrationReport: ...

    def is_initialized(self) -> bool: ...

    def schema_version(self) -> int: ...

    def bootstrap(self, principal: Principal) -> BootstrapResult: ...

    def publish_event(
        self,
        *,
        kind: str,
        entity_kind: str,
        entity_id: str,
        summary: dict[str, object],
        at: datetime,
    ) -> Event:
        """Append an event that is not a declared write (FR-N1, SCHEMA §2.5).

        The feed has two producers and one table. Declared writes insert
        their event inside the same transaction as the entity change and the
        audit row. Observed-side happenings — a sweep finishing, a CI state
        changing — are published here by the application layer on the
        collectors' behalf: no audit row, because nobody declared anything,
        and no revision bump, because the authoritative state did not change.

        Collectors still never write the declared store themselves.
        """
        ...

    def read(self) -> AbstractContextManager[ReadView]: ...

    def write(self) -> AbstractContextManager[WriteTxn]: ...


class ObservedStore(Protocol):
    """The append-only evidence store. Only collectors write here.

    Nothing here knows what a project is: resolving a dependency reference to
    a registered project is a cross-store question the application layer
    answers and hands down (`SCHEMA.md` §1).
    """

    def migrate(self) -> MigrationReport: ...

    def is_initialized(self) -> bool: ...

    def schema_version(self) -> int: ...

    def bind_instance(self, instance_id: str) -> None:
        """Stamp this store with the instance it belongs to.

        The two stores are backed up and restored independently, so a
        mismatched pair has to be detectable rather than merely unlikely.
        """
        ...

    def instance_id(self) -> str | None: ...

    def has_evidence_tables(self) -> bool: ...

    # -- sweeps ------------------------------------------------------------

    def begin_sweep(
        self, *, collector: str, scope: list[str], at: datetime
    ) -> Sweep: ...

    def finish_sweep(
        self,
        sweep_id: str,
        *,
        outcome: SweepOutcome,
        stats: dict[str, int],
        at: datetime,
        detail: str | None = None,
    ) -> None: ...

    def append(
        self, sweep_id: str, findings: list[PendingObservation], *, at: datetime
    ) -> AppendStats: ...

    def list_sweeps(
        self, *, collector: str | None = None, limit: int = 50
    ) -> list[Sweep]: ...

    def coverage(self) -> dict[str, Sweep]:
        """The newest completed sweep per collector."""
        ...

    # -- reads -------------------------------------------------------------

    def list_observations(
        self,
        *,
        kind: str | None = None,
        project_id: str | None = None,
        subject_key: str | None = None,
        limit: int = 100,
        offset: int = 0,
    ) -> list[Observation]: ...

    def latest(
        self,
        *,
        kinds: tuple[str, ...] = (),
        project_id: str | None = None,
        promoted_only: bool = False,
        limit: int = 1000,
    ) -> list[Observation]: ...

    def dep_refs(
        self, *, from_project_id: str | None = None, to_project_id: str | None = None
    ) -> list[DepRef]: ...

    def counts(self) -> dict[str, int]: ...

    # -- projections and retention -----------------------------------------

    def rebuild_latest(self) -> int: ...

    def replace_dep_refs(self, rows: list[DepRefRow]) -> int: ...

    def prune(
        self,
        *,
        before: datetime,
        protected_observation_ids: frozenset[str] = frozenset(),
    ) -> PruneReport: ...


__all__ = [
    "Blocker",
    "BootstrapResult",
    "Counts",
    "DeclaredStore",
    "MigrationReport",
    "ObservedStore",
    "ProjectUpdate",
    "ReadView",
    "WorkFilter",
    "WorkItemUpdate",
    "WriteTxn",
]
