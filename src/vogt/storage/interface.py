"""The storage interface the application layer is allowed to know about.

NFR-S3: nothing above this line may depend on SQLite-only semantics, so that
a Postgres backend stays possible behind the same interface. Concretely that
means no rowids, no `INSERT OR REPLACE`, no dynamic typing, and no SQL
anywhere outside a backend package.
"""

from __future__ import annotations

from contextlib import AbstractContextManager
from dataclasses import dataclass
from datetime import datetime
from typing import Protocol

from vogt.core.entities import Actor, AuditRecord, Event, Project
from vogt.core.principal import Principal


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


@dataclass(frozen=True)
class BootstrapResult:
    """The outcome of creating an instance."""

    instance_id: str
    actor: Actor


class ReadView(Protocol):
    """Read access to the declared store within one consistent snapshot."""

    def instance_id(self) -> str: ...

    def current_revision(self) -> int: ...

    def counts(self) -> Counts: ...

    def actor_by_identity(self, identity_ref: str) -> Actor | None: ...

    def actor_by_id(self, actor_id: str) -> Actor | None: ...

    def project_by_slug(self, slug: str) -> Project | None: ...

    def list_projects(self, *, limit: int, offset: int) -> list[Project]: ...

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

    def read(self) -> AbstractContextManager[ReadView]: ...

    def write(self) -> AbstractContextManager[WriteTxn]: ...


class ObservedStore(Protocol):
    """The append-only evidence store. Only collectors write here (from M2)."""

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


__all__ = [
    "BootstrapResult",
    "Counts",
    "DeclaredStore",
    "MigrationReport",
    "ObservedStore",
    "ReadView",
    "WriteTxn",
]
