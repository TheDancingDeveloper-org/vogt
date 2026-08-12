"""The declared store on SQLite.

This module owns the only SQL that touches authoritative data. Everything it
exposes upward is entities and transactions (`storage/interface.py`), which
is what keeps a Postgres backend a swap rather than a rewrite (NFR-S3).
"""

from __future__ import annotations

import json
import os
import socket
import sqlite3
from collections.abc import Iterator
from contextlib import contextmanager, suppress
from datetime import datetime, timedelta
from pathlib import Path

from vogt.core.clock import Clock, from_iso, to_iso, utc_now
from vogt.core.entities import Actor, AuditRecord, Event, Project
from vogt.core.ids import IdFactory, new_id
from vogt.core.principal import Principal
from vogt.errors import AlreadyInitialized, NotInitialized
from vogt.storage.interface import (
    BootstrapResult,
    Counts,
    MigrationReport,
)
from vogt.storage.sqlite.connection import connect
from vogt.storage.sqlite.migrator import DEFAULT_STALE_AFTER, Migrator

MIGRATIONS_DIR = Path(__file__).parent / "migrations" / "declared"

META_INSTANCE_ID = "instance_id"
META_REVISION = "revision"
META_CREATED_AT = "created_at"

#: The audit operation recorded when an instance is created. `init` is the
#: one bootstrap that is *not* a declared write: it creates the instance
#: rather than changing anything inside it, so it lands an audit row at
#: revision 0 and emits no event. The `/events` cursor therefore starts at
#: the first real change a client could care about.
INIT_OPERATION = "instance.init"
INIT_REASON = "instance bootstrap"


def _holder_name() -> str:
    return f"{socket.gethostname()}/{os.getpid()}"


class SqliteDeclaredStore:
    """Authoritative store: mutable, audited, revisioned."""

    def __init__(
        self,
        path: Path,
        *,
        clock: Clock = utc_now,
        id_factory: IdFactory = new_id,
        lock_stale_after: timedelta = DEFAULT_STALE_AFTER,
    ) -> None:
        self._path = path
        self._clock = clock
        self._id_factory = id_factory
        self._migrator = Migrator(
            store="declared",
            directory=MIGRATIONS_DIR,
            holder=_holder_name(),
            stale_after=lock_stale_after,
        )

    @property
    def path(self) -> Path:
        return self._path

    # -- lifecycle ---------------------------------------------------------

    def migrate(self) -> MigrationReport:
        conn = connect(self._path, create=True)
        try:
            return self._migrator.migrate(conn, now=self._clock())
        finally:
            conn.close()

    def schema_version(self) -> int:
        if not self._path.exists():
            return 0
        conn = connect(self._path, create=False)
        try:
            return self._migrator.applied_version(conn)
        finally:
            conn.close()

    def is_initialized(self) -> bool:
        if not self._path.exists():
            return False
        conn = connect(self._path, create=False)
        try:
            return _meta_get(conn, META_INSTANCE_ID) is not None
        except sqlite3.OperationalError:
            return False
        finally:
            conn.close()

    def bootstrap(self, principal: Principal) -> BootstrapResult:
        """Create the instance: metadata, the initiating actor, one audit row.

        Not a declared write (see `INIT_OPERATION`): revision stays 0 and no
        event is emitted, so a client that connects afterwards sees an empty
        feed rather than an instance-creation event it cannot act on.
        """
        now = self._clock()
        conn = connect(self._path, create=True)
        try:
            conn.execute("BEGIN IMMEDIATE")
            if _meta_get(conn, META_INSTANCE_ID) is not None:
                conn.execute("ROLLBACK")
                msg = f"an instance already exists at {self._path.parent}"
                raise AlreadyInitialized(msg)
            instance_id = self._id_factory("ins")
            _meta_set(conn, META_INSTANCE_ID, instance_id)
            _meta_set(conn, META_REVISION, "0")
            _meta_set(conn, META_CREATED_AT, to_iso(now))
            actor = Actor(
                id=self._id_factory("act"),
                kind=principal.kind,
                display_name=principal.display_name,
                identity_ref=principal.identity_ref,
                disabled=False,
                created_at=now,
            )
            _insert_actor(conn, actor)
            _insert_audit(
                conn,
                AuditRecord(
                    id=self._id_factory("aud"),
                    txn_id=self._id_factory("txn"),
                    revision=0,
                    actor_id=actor.id,
                    actor_identity_ref=actor.identity_ref,
                    operation=INIT_OPERATION,
                    entity_kind="instance",
                    entity_id=instance_id,
                    reason=INIT_REASON,
                    payload_digest="sha256:" + "0" * 64,
                    at=now,
                ),
            )
            conn.execute("COMMIT")
            return BootstrapResult(instance_id=instance_id, actor=actor)
        except BaseException:
            _rollback_quietly(conn)
            raise
        finally:
            conn.close()

    # -- access ------------------------------------------------------------

    @contextmanager
    def read(self) -> Iterator[SqliteReadView]:
        conn = self._open_initialized()
        try:
            yield SqliteReadView(conn)
        finally:
            conn.close()

    @contextmanager
    def write(self) -> Iterator[SqliteWriteTxn]:
        """Open one atomic declared write (NFR-I1).

        The revision is allocated when the transaction opens and every row
        written inside it — entity, audit, event — carries it. A failure
        anywhere rolls the whole thing back, revision included.
        """
        conn = self._open_initialized()
        try:
            conn.execute("BEGIN IMMEDIATE")
            revision = _bump_revision(conn)
            txn = SqliteWriteTxn(
                conn,
                txn_id=self._id_factory("txn"),
                revision=revision,
                id_factory=self._id_factory,
            )
            yield txn
            conn.execute("COMMIT")
        except BaseException:
            _rollback_quietly(conn)
            raise
        finally:
            conn.close()

    def _open_initialized(self) -> sqlite3.Connection:
        if not self._path.exists():
            raise NotInitialized(self._not_initialized_message())
        conn = connect(self._path, create=False)
        try:
            if _meta_get(conn, META_INSTANCE_ID) is None:
                raise NotInitialized(self._not_initialized_message())
        except sqlite3.OperationalError as exc:
            conn.close()
            raise NotInitialized(self._not_initialized_message()) from exc
        except BaseException:
            conn.close()
            raise
        return conn

    def _not_initialized_message(self) -> str:
        return f"no Vogt instance in {self._path.parent} — run `vogt init` first"


class SqliteReadView:
    """Reads over one connection. Also the base for a write transaction."""

    def __init__(self, conn: sqlite3.Connection) -> None:
        self._conn = conn

    def instance_id(self) -> str:
        value = _meta_get(self._conn, META_INSTANCE_ID)
        if value is None:  # pragma: no cover - guarded by _open_initialized
            raise NotInitialized("instance id missing")
        return value

    def current_revision(self) -> int:
        value = _meta_get(self._conn, META_REVISION)
        return 0 if value is None else int(value)

    def counts(self) -> Counts:
        return Counts(
            projects=_count(self._conn, "projects"),
            actors=_count(self._conn, "actors"),
            events=_count(self._conn, "events"),
            audit=_count(self._conn, "audit"),
        )

    def actor_by_identity(self, identity_ref: str) -> Actor | None:
        row = self._conn.execute(
            "SELECT * FROM actors WHERE identity_ref = ?", (identity_ref,)
        ).fetchone()
        return None if row is None else _row_to_actor(row)

    def actor_by_id(self, actor_id: str) -> Actor | None:
        row = self._conn.execute(
            "SELECT * FROM actors WHERE id = ?", (actor_id,)
        ).fetchone()
        return None if row is None else _row_to_actor(row)

    def project_by_slug(self, slug: str) -> Project | None:
        row = self._conn.execute(
            "SELECT * FROM projects WHERE slug = ?", (slug,)
        ).fetchone()
        return None if row is None else _row_to_project(row)

    def list_projects(self, *, limit: int, offset: int) -> list[Project]:
        rows = self._conn.execute(
            "SELECT * FROM projects ORDER BY slug LIMIT ? OFFSET ?",
            (limit, offset),
        ).fetchall()
        return [_row_to_project(row) for row in rows]

    def list_events(self, *, after: int, limit: int) -> list[Event]:
        rows = self._conn.execute(
            "SELECT * FROM events WHERE seq > ? ORDER BY seq LIMIT ?",
            (after, limit),
        ).fetchall()
        return [_row_to_event(row) for row in rows]

    def list_audit(
        self,
        *,
        limit: int,
        actor_id: str | None = None,
        operation: str | None = None,
        entity_id: str | None = None,
    ) -> list[AuditRecord]:
        clauses: list[str] = []
        params: list[object] = []
        if actor_id is not None:
            clauses.append("a.actor_id = ?")
            params.append(actor_id)
        if operation is not None:
            clauses.append("a.operation = ?")
            params.append(operation)
        if entity_id is not None:
            clauses.append("a.entity_id = ?")
            params.append(entity_id)
        where = f"WHERE {' AND '.join(clauses)}" if clauses else ""
        params.append(limit)
        rows = self._conn.execute(
            "SELECT a.*, ac.identity_ref AS actor_identity_ref "
            "FROM audit a JOIN actors ac ON ac.id = a.actor_id "
            f"{where} ORDER BY a.revision DESC, a.at DESC, a.id DESC LIMIT ?",
            tuple(params),
        ).fetchall()
        return [_row_to_audit(row) for row in rows]


class SqliteWriteTxn(SqliteReadView):
    """One open declared write."""

    def __init__(
        self,
        conn: sqlite3.Connection,
        *,
        txn_id: str,
        revision: int,
        id_factory: IdFactory,
    ) -> None:
        super().__init__(conn)
        self._txn_id = txn_id
        self._revision = revision
        self._id_factory = id_factory

    @property
    def txn_id(self) -> str:
        return self._txn_id

    @property
    def revision(self) -> int:
        return self._revision

    def insert_actor(self, actor: Actor) -> None:
        _insert_actor(self._conn, actor)

    def insert_project(self, project: Project) -> None:
        self._conn.execute(
            "INSERT INTO projects (id, slug, name, root_path, repo_url, "
            "lifecycle_state, current_version, contract_version, "
            "compliance_status, compliance_checked_at, exclusions, trust_state, "
            "created_at, updated_at) "
            "VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            (
                project.id,
                project.slug,
                project.name,
                project.root_path,
                project.repo_url,
                project.lifecycle_state,
                project.current_version,
                project.contract_version,
                project.compliance_status,
                None
                if project.compliance_checked_at is None
                else to_iso(project.compliance_checked_at),
                json.dumps(project.exclusions),
                project.trust_state,
                to_iso(project.created_at),
                to_iso(project.updated_at),
            ),
        )

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
    ) -> AuditRecord:
        record = AuditRecord(
            id=self._id_factory("aud"),
            txn_id=self._txn_id,
            revision=self._revision,
            actor_id=actor.id,
            actor_identity_ref=actor.identity_ref,
            operation=operation,
            entity_kind=entity_kind,
            entity_id=entity_id,
            reason=reason,
            payload_digest=payload_digest,
            at=at,
        )
        _insert_audit(self._conn, record)
        return record

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
    ) -> Event:
        cursor = self._conn.execute(
            "INSERT INTO events (kind, entity_kind, entity_id, actor_id, "
            "audit_id, summary, at) VALUES (?, ?, ?, ?, ?, ?, ?)",
            (
                kind,
                entity_kind,
                entity_id,
                actor_id,
                audit_id,
                json.dumps(summary, default=str),
                to_iso(at),
            ),
        )
        seq = cursor.lastrowid
        if seq is None:  # pragma: no cover - sqlite always reports it
            msg = "event insert did not yield a sequence number"
            raise RuntimeError(msg)
        return Event(
            seq=seq,
            kind=kind,
            entity_kind=entity_kind,
            entity_id=entity_id,
            actor_id=actor_id,
            audit_id=audit_id,
            summary=summary,
            at=at,
        )


# -- row mapping -----------------------------------------------------------


def _row_to_actor(row: sqlite3.Row) -> Actor:
    return Actor(
        id=str(row["id"]),
        kind="agent" if row["kind"] == "agent" else "human",
        display_name=str(row["display_name"]),
        identity_ref=str(row["identity_ref"]),
        disabled=bool(row["disabled"]),
        created_at=from_iso(str(row["created_at"])),
    )


def _row_to_project(row: sqlite3.Row) -> Project:
    checked_at = row["compliance_checked_at"]
    return Project.model_validate(
        {
            "id": str(row["id"]),
            "slug": str(row["slug"]),
            "name": str(row["name"]),
            "root_path": str(row["root_path"]),
            "repo_url": row["repo_url"],
            "lifecycle_state": row["lifecycle_state"],
            "current_version": row["current_version"],
            "contract_version": row["contract_version"],
            "compliance_status": row["compliance_status"],
            "compliance_checked_at": (
                None if checked_at is None else from_iso(str(checked_at))
            ),
            "exclusions": json.loads(str(row["exclusions"])),
            "trust_state": row["trust_state"],
            "created_at": from_iso(str(row["created_at"])),
            "updated_at": from_iso(str(row["updated_at"])),
        }
    )


def _row_to_audit(row: sqlite3.Row) -> AuditRecord:
    return AuditRecord(
        id=str(row["id"]),
        txn_id=str(row["txn_id"]),
        revision=int(row["revision"]),
        actor_id=str(row["actor_id"]),
        actor_identity_ref=str(row["actor_identity_ref"]),
        operation=str(row["operation"]),
        entity_kind=str(row["entity_kind"]),
        entity_id=str(row["entity_id"]),
        reason=str(row["reason"]),
        payload_digest=str(row["payload_digest"]),
        at=from_iso(str(row["at"])),
    )


def _row_to_event(row: sqlite3.Row) -> Event:
    summary: dict[str, object] = json.loads(str(row["summary"]))
    return Event(
        seq=int(row["seq"]),
        kind=str(row["kind"]),
        entity_kind=str(row["entity_kind"]),
        entity_id=str(row["entity_id"]),
        actor_id=None if row["actor_id"] is None else str(row["actor_id"]),
        audit_id=None if row["audit_id"] is None else str(row["audit_id"]),
        summary=summary,
        at=from_iso(str(row["at"])),
    )


# -- shared SQL ------------------------------------------------------------


def _insert_actor(conn: sqlite3.Connection, actor: Actor) -> None:
    conn.execute(
        "INSERT INTO actors (id, kind, display_name, identity_ref, disabled, "
        "created_at) VALUES (?, ?, ?, ?, ?, ?)",
        (
            actor.id,
            actor.kind,
            actor.display_name,
            actor.identity_ref,
            int(actor.disabled),
            to_iso(actor.created_at),
        ),
    )


def _insert_audit(conn: sqlite3.Connection, record: AuditRecord) -> None:
    conn.execute(
        "INSERT INTO audit (id, txn_id, revision, actor_id, operation, "
        "entity_kind, entity_id, reason, payload_digest, at) "
        "VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        (
            record.id,
            record.txn_id,
            record.revision,
            record.actor_id,
            record.operation,
            record.entity_kind,
            record.entity_id,
            record.reason,
            record.payload_digest,
            to_iso(record.at),
        ),
    )


def _meta_get(conn: sqlite3.Connection, key: str) -> str | None:
    row = conn.execute("SELECT value FROM meta WHERE key = ?", (key,)).fetchone()
    return None if row is None else str(row["value"])


def _meta_set(conn: sqlite3.Connection, key: str, value: str) -> None:
    updated = conn.execute(
        "UPDATE meta SET value = ? WHERE key = ?", (value, key)
    ).rowcount
    if updated == 0:
        conn.execute("INSERT INTO meta (key, value) VALUES (?, ?)", (key, value))


def _bump_revision(conn: sqlite3.Connection) -> int:
    current = _meta_get(conn, META_REVISION)
    if current is None:  # pragma: no cover - guarded by _open_initialized
        raise NotInitialized("instance metadata missing")
    revision = int(current) + 1
    _meta_set(conn, META_REVISION, str(revision))
    return revision


def _count(conn: sqlite3.Connection, table: str) -> int:
    # `table` is never caller-supplied: every call site passes a literal.
    row = conn.execute(f"SELECT COUNT(*) AS n FROM {table}").fetchone()
    return int(row["n"])


def _rollback_quietly(conn: sqlite3.Connection) -> None:
    """Roll back if a transaction is open; say nothing if one is not.

    Called from `except` blocks, where the failure being handled matters more
    than "cannot rollback - no transaction is active".
    """
    with suppress(sqlite3.OperationalError):
        conn.execute("ROLLBACK")
