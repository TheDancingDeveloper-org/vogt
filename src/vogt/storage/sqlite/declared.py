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
from vogt.core.entities import (
    Actor,
    AuditRecord,
    Comment,
    Event,
    Initiative,
    Label,
    Project,
    Relation,
    RelationKind,
    WorkItem,
    WorkKind,
)
from vogt.core.ids import IdFactory, new_id
from vogt.core.principal import Principal
from vogt.core.workflow import TERMINAL_STATES, Workflow, default_workflow
from vogt.errors import AlreadyInitialized, NotFound, NotInitialized
from vogt.storage.interface import (
    Blocker,
    BootstrapResult,
    Counts,
    MigrationReport,
    ProjectUpdate,
    WorkFilter,
    WorkItemUpdate,
)
from vogt.storage.sqlite.connection import connect
from vogt.storage.sqlite.migrator import (
    DEFAULT_STALE_AFTER,
    Migrator,
)
from vogt.storage.sqlite.migrator import (
    table_exists as _sqlite_table_exists,
)

MIGRATIONS_DIR = Path(__file__).parent / "migrations" / "declared"

META_INSTANCE_ID = "instance_id"
META_REVISION = "revision"
META_CREATED_AT = "created_at"
META_WORK_REF_SEQ = "work_ref_seq"

WORK_REF_PREFIX = "WI-"

WORK_KINDS: tuple[WorkKind, ...] = ("feature", "bug", "chore", "question")

_WORK_COLUMNS = "w.*, p.slug AS project_slug, a.identity_ref AS assignee_identity_ref"
_WORK_JOINS = (
    "FROM work_items w "
    "LEFT JOIN projects p ON p.id = w.project_id "
    "LEFT JOIN actors a ON a.id = w.assignee_actor_id"
)

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
        """Apply migrations, then guarantee the seed rows they imply.

        Seeding lives here rather than in bootstrap because an instance
        created before a migration existed never runs bootstrap again, and
        rather than in the migration SQL because the defaults would then be
        spelled twice — once in Python, once as JSON literals — and drift.
        Both steps are idempotent, so `migrate` stays safe to run on every
        start (`DEPLOYMENT.md` §5).
        """
        conn = connect(self._path, create=True)
        try:
            now = self._clock()
            report = self._migrator.migrate(conn, now=now)
            self._ensure_default_workflows(conn, now=now)
            return report
        finally:
            conn.close()

    def _ensure_default_workflows(
        self, conn: sqlite3.Connection, *, now: datetime
    ) -> None:
        if not _sqlite_table_exists(conn, "workflow_defs"):
            return
        conn.execute("BEGIN IMMEDIATE")
        try:
            for kind in WORK_KINDS:
                existing = conn.execute(
                    "SELECT kind FROM workflow_defs WHERE kind = ?", (kind,)
                ).fetchone()
                if existing is None:
                    _upsert_workflow(conn, default_workflow(kind), at=now)
            conn.execute("COMMIT")
        except BaseException:
            _rollback_quietly(conn)
            raise

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
            _meta_set(conn, META_WORK_REF_SEQ, "0")
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
            work_items=_count(self._conn, "work_items"),
            initiatives=_count(self._conn, "initiatives"),
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

    def list_actors(self, *, limit: int, offset: int) -> list[Actor]:
        rows = self._conn.execute(
            "SELECT * FROM actors ORDER BY identity_ref LIMIT ? OFFSET ?",
            (limit, offset),
        ).fetchall()
        return [_row_to_actor(row) for row in rows]

    def project_by_slug(self, slug: str) -> Project | None:
        row = self._conn.execute(
            "SELECT * FROM projects WHERE slug = ?", (slug,)
        ).fetchone()
        return None if row is None else _row_to_project(row)

    def project_by_id(self, project_id: str) -> Project | None:
        row = self._conn.execute(
            "SELECT * FROM projects WHERE id = ?", (project_id,)
        ).fetchone()
        return None if row is None else _row_to_project(row)

    def list_projects(self, *, limit: int, offset: int) -> list[Project]:
        rows = self._conn.execute(
            "SELECT * FROM projects ORDER BY slug LIMIT ? OFFSET ?",
            (limit, offset),
        ).fetchall()
        return [_row_to_project(row) for row in rows]

    # -- work --------------------------------------------------------------

    def work_item_by_id(self, work_item_id: str) -> WorkItem | None:
        row = self._conn.execute(
            f"SELECT {_WORK_COLUMNS} {_WORK_JOINS} WHERE w.id = ?", (work_item_id,)
        ).fetchone()
        if row is None:
            return None
        return self._hydrate([row])[0]

    def work_item_by_ref(self, ref: str) -> WorkItem | None:
        row = self._conn.execute(
            f"SELECT {_WORK_COLUMNS} {_WORK_JOINS} WHERE w.ref = ?", (ref,)
        ).fetchone()
        if row is None:
            return None
        return self._hydrate([row])[0]

    def list_work_items(self, work_filter: WorkFilter) -> list[WorkItem]:
        where, params = _work_where(work_filter)
        rows = self._conn.execute(
            f"SELECT {_WORK_COLUMNS} {_WORK_JOINS} {where} "
            "ORDER BY w.created_at, w.ref LIMIT ? OFFSET ?",
            (*params, work_filter.limit, work_filter.offset),
        ).fetchall()
        return self._hydrate(rows)

    def count_work_items(self, work_filter: WorkFilter) -> int:
        where, params = _work_where(work_filter)
        row = self._conn.execute(
            f"SELECT COUNT(*) AS n {_WORK_JOINS} {where}", tuple(params)
        ).fetchone()
        return int(row["n"])

    def blocking_fan_out(self, work_item_ids: list[str]) -> dict[str, int]:
        if not work_item_ids:
            return {}
        placeholders = ", ".join("?" for _ in work_item_ids)
        rows = self._conn.execute(
            "SELECT related_id, COUNT(*) AS n FROM work_relations "
            f"WHERE kind = 'depends_on' AND related_id IN ({placeholders}) "
            "GROUP BY related_id",
            tuple(work_item_ids),
        ).fetchall()
        return {str(row["related_id"]): int(row["n"]) for row in rows}

    def unfinished_blockers(
        self, work_item_id: str, *, terminal_states: tuple[str, ...]
    ) -> list[Blocker]:
        placeholders = ", ".join("?" for _ in terminal_states) or "''"
        rows = self._conn.execute(
            "SELECT t.ref AS ref, t.state AS state FROM work_relations r "
            "JOIN work_items t ON t.id = r.related_id "
            "WHERE r.work_item_id = ? AND r.kind = 'depends_on' "
            f"AND t.state NOT IN ({placeholders}) ORDER BY t.ref",
            (work_item_id, *terminal_states),
        ).fetchall()
        return [Blocker(ref=str(r["ref"]), state=str(r["state"])) for r in rows]

    def comments_for(self, work_item_id: str, *, limit: int) -> list[Comment]:
        rows = self._conn.execute(
            "SELECT c.*, a.display_name AS actor_display_name FROM comments c "
            "JOIN actors a ON a.id = c.actor_id "
            "WHERE c.work_item_id = ? ORDER BY c.created_at, c.id LIMIT ?",
            (work_item_id, limit),
        ).fetchall()
        return [_row_to_comment(row) for row in rows]

    # -- taxonomy ----------------------------------------------------------

    def label_by_name(self, name: str) -> Label | None:
        row = self._conn.execute(
            "SELECT * FROM labels WHERE name = ?", (name,)
        ).fetchone()
        return None if row is None else _row_to_label(row)

    def list_labels(self, *, limit: int, offset: int) -> list[Label]:
        rows = self._conn.execute(
            "SELECT * FROM labels ORDER BY name LIMIT ? OFFSET ?", (limit, offset)
        ).fetchall()
        return [_row_to_label(row) for row in rows]

    def initiative_by_id(self, initiative_id: str) -> Initiative | None:
        row = self._conn.execute(
            "SELECT * FROM initiatives WHERE id = ?", (initiative_id,)
        ).fetchone()
        return None if row is None else _row_to_initiative(row)

    def initiative_by_slug(self, slug: str) -> Initiative | None:
        row = self._conn.execute(
            "SELECT * FROM initiatives WHERE slug = ?", (slug,)
        ).fetchone()
        return None if row is None else _row_to_initiative(row)

    def list_initiatives(self, *, limit: int, offset: int) -> list[Initiative]:
        rows = self._conn.execute(
            "SELECT * FROM initiatives ORDER BY slug LIMIT ? OFFSET ?",
            (limit, offset),
        ).fetchall()
        return [_row_to_initiative(row) for row in rows]

    def workflow_for(self, kind: str) -> Workflow:
        """The stored machine for a kind, or the shipped default.

        The fallback matters for an instance created before migration 0002:
        it has no `workflow_defs` rows and there is no sensible way to
        backfill them in SQL, so the code that owns the defaults supplies
        them instead of the migration guessing.
        """
        row = self._conn.execute(
            "SELECT definition FROM workflow_defs WHERE kind = ?", (kind,)
        ).fetchone()
        if row is None:
            return default_workflow(kind)  # type: ignore[arg-type]
        definition: dict[str, object] = json.loads(str(row["definition"]))
        return Workflow.from_definition(kind, definition)  # type: ignore[arg-type]

    # -- hydration ---------------------------------------------------------

    def _hydrate(self, rows: list[sqlite3.Row]) -> list[WorkItem]:
        """Attach labels and relations in two queries, not two per item."""
        if not rows:
            return []
        ids = [str(row["id"]) for row in rows]
        placeholders = ", ".join("?" for _ in ids)

        labels: dict[str, list[str]] = {work_id: [] for work_id in ids}
        for row in self._conn.execute(
            "SELECT wl.work_item_id AS work_item_id, l.name AS name "
            "FROM work_item_labels wl JOIN labels l ON l.id = wl.label_id "
            f"WHERE wl.work_item_id IN ({placeholders}) ORDER BY l.name",
            tuple(ids),
        ).fetchall():
            labels[str(row["work_item_id"])].append(str(row["name"]))

        relations: dict[str, list[Relation]] = {work_id: [] for work_id in ids}
        for row in self._conn.execute(
            "SELECT r.work_item_id AS work_item_id, r.kind AS kind, "
            "t.id AS related_id, t.ref AS related_ref, t.title AS related_title, "
            "t.state AS related_state FROM work_relations r "
            "JOIN work_items t ON t.id = r.related_id "
            f"WHERE r.work_item_id IN ({placeholders}) ORDER BY r.kind, t.ref",
            tuple(ids),
        ).fetchall():
            relations[str(row["work_item_id"])].append(
                Relation(
                    kind=str(row["kind"]),  # type: ignore[arg-type]
                    related_id=str(row["related_id"]),
                    related_ref=str(row["related_ref"]),
                    related_title=str(row["related_title"]),
                    related_state=str(row["related_state"]),
                )
            )

        return [
            _row_to_work_item(
                row, labels=labels[str(row["id"])], relations=relations[str(row["id"])]
            )
            for row in rows
        ]

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

    def update_project(
        self, project_id: str, update: ProjectUpdate, *, at: datetime
    ) -> None:
        assignments: list[str] = []
        params: list[object] = []
        for column, value in (
            ("lifecycle_state", update.lifecycle_state),
            ("repo_url", update.repo_url),
            ("current_version", update.current_version),
            ("compliance_status", update.compliance_status),
        ):
            if value is not None:
                assignments.append(f"{column} = ?")
                params.append(value)
        if update.compliance_checked_at is not None:
            assignments.append("compliance_checked_at = ?")
            params.append(to_iso(update.compliance_checked_at))
        if update.exclusions is not None:
            assignments.append("exclusions = ?")
            params.append(json.dumps(list(update.exclusions)))
        if not assignments:
            return
        assignments.append("updated_at = ?")
        params.append(to_iso(at))
        params.append(project_id)
        self._conn.execute(
            f"UPDATE projects SET {', '.join(assignments)} WHERE id = ?",
            tuple(params),
        )

    def next_work_ref(self) -> str:
        """Allocate the next `WI-n` handle.

        Inside the caller's transaction, so a rolled-back creation does not
        burn a reference and leave a gap nobody can explain.
        """
        current = _meta_get(self._conn, META_WORK_REF_SEQ) or "0"
        nxt = int(current) + 1
        _meta_set(self._conn, META_WORK_REF_SEQ, str(nxt))
        return f"{WORK_REF_PREFIX}{nxt}"

    def insert_work_item(self, item: WorkItem) -> None:
        self._conn.execute(
            "INSERT INTO work_items (id, ref, kind, title, body, state, priority, "
            "effort, project_id, initiative_id, origin, trust_state, "
            "assignee_actor_id, created_at, updated_at) "
            "VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            (
                item.id,
                item.ref,
                item.kind,
                item.title,
                item.body,
                item.state,
                item.priority,
                item.effort,
                item.project_id,
                item.initiative_id,
                item.origin,
                item.trust_state,
                item.assignee_actor_id,
                to_iso(item.created_at),
                to_iso(item.updated_at),
            ),
        )
        for name in item.labels:
            self._attach_label_by_name(item.id, name)

    def update_work_item(
        self, work_item_id: str, update: WorkItemUpdate, *, at: datetime
    ) -> None:
        assignments: list[str] = []
        params: list[object] = []
        for column, value in (
            ("title", update.title),
            ("body", update.body),
            ("state", update.state),
            ("priority", update.priority),
            ("effort", update.effort),
            ("assignee_actor_id", update.assignee_actor_id),
            ("initiative_id", update.initiative_id),
            ("project_id", update.project_id),
        ):
            if value is not None:
                assignments.append(f"{column} = ?")
                params.append(value)
        for column, clear in (
            ("effort", update.clear_effort),
            ("assignee_actor_id", update.clear_assignee),
            ("initiative_id", update.clear_initiative),
        ):
            if clear:
                assignments.append(f"{column} = NULL")

        if assignments:
            assignments.append("updated_at = ?")
            params.append(to_iso(at))
            params.append(work_item_id)
            self._conn.execute(
                f"UPDATE work_items SET {', '.join(assignments)} WHERE id = ?",
                tuple(params),
            )

        for name in update.add_labels:
            self._attach_label_by_name(work_item_id, name)
        for name in update.remove_labels:
            self._conn.execute(
                "DELETE FROM work_item_labels WHERE work_item_id = ? AND label_id IN "
                "(SELECT id FROM labels WHERE name = ?)",
                (work_item_id, name),
            )
        if update.add_labels or update.remove_labels:
            self._conn.execute(
                "UPDATE work_items SET updated_at = ? WHERE id = ?",
                (to_iso(at), work_item_id),
            )

    def insert_relation(
        self,
        *,
        work_item_id: str,
        related_id: str,
        kind: RelationKind,
        at: datetime,
    ) -> None:
        self._conn.execute(
            "INSERT INTO work_relations (work_item_id, related_id, kind, created_at) "
            "VALUES (?, ?, ?, ?)",
            (work_item_id, related_id, kind, to_iso(at)),
        )

    def delete_relation(
        self, *, work_item_id: str, related_id: str, kind: RelationKind
    ) -> bool:
        cursor = self._conn.execute(
            "DELETE FROM work_relations WHERE work_item_id = ? AND related_id = ? "
            "AND kind = ?",
            (work_item_id, related_id, kind),
        )
        return cursor.rowcount > 0

    def insert_label(self, label: Label) -> None:
        self._conn.execute(
            "INSERT INTO labels (id, name, color, created_at) VALUES (?, ?, ?, ?)",
            (label.id, label.name, label.color, to_iso(label.created_at)),
        )

    def insert_initiative(self, initiative: Initiative) -> None:
        self._conn.execute(
            "INSERT INTO initiatives (id, slug, title, body, state, weight, "
            "created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            (
                initiative.id,
                initiative.slug,
                initiative.title,
                initiative.body,
                initiative.state,
                initiative.weight,
                to_iso(initiative.created_at),
                to_iso(initiative.updated_at),
            ),
        )

    def insert_comment(self, comment: Comment) -> None:
        self._conn.execute(
            "INSERT INTO comments (id, work_item_id, actor_id, body, created_at) "
            "VALUES (?, ?, ?, ?, ?)",
            (
                comment.id,
                comment.work_item_id,
                comment.actor_id,
                comment.body,
                to_iso(comment.created_at),
            ),
        )

    def upsert_workflow(self, workflow: Workflow, *, at: datetime) -> None:
        _upsert_workflow(self._conn, workflow, at=at)

    def _attach_label_by_name(self, work_item_id: str, name: str) -> None:
        row = self._conn.execute(
            "SELECT id FROM labels WHERE name = ?", (name,)
        ).fetchone()
        if row is None:
            msg = f"no label named {name!r}"
            raise NotFound(msg)
        self._conn.execute(
            "INSERT INTO work_item_labels (work_item_id, label_id) "
            "SELECT ?, ? WHERE NOT EXISTS ("
            "  SELECT 1 FROM work_item_labels WHERE work_item_id = ? AND label_id = ?"
            ")",
            (work_item_id, str(row["id"]), work_item_id, str(row["id"])),
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


def _row_to_work_item(
    row: sqlite3.Row, *, labels: list[str], relations: list[Relation]
) -> WorkItem:
    return WorkItem.model_validate(
        {
            "id": str(row["id"]),
            "ref": str(row["ref"]),
            "kind": row["kind"],
            "title": str(row["title"]),
            "body": str(row["body"]),
            "state": str(row["state"]),
            "priority": row["priority"],
            "effort": row["effort"],
            "project_id": row["project_id"],
            "project_slug": row["project_slug"],
            "initiative_id": row["initiative_id"],
            "origin": row["origin"],
            "trust_state": row["trust_state"],
            "assignee_actor_id": row["assignee_actor_id"],
            "assignee_identity_ref": row["assignee_identity_ref"],
            "labels": labels,
            "relations": relations,
            "created_at": from_iso(str(row["created_at"])),
            "updated_at": from_iso(str(row["updated_at"])),
        }
    )


def _row_to_label(row: sqlite3.Row) -> Label:
    return Label(
        id=str(row["id"]),
        name=str(row["name"]),
        color=None if row["color"] is None else str(row["color"]),
        created_at=from_iso(str(row["created_at"])),
    )


def _row_to_initiative(row: sqlite3.Row) -> Initiative:
    return Initiative.model_validate(
        {
            "id": str(row["id"]),
            "slug": str(row["slug"]),
            "title": str(row["title"]),
            "body": str(row["body"]),
            "state": row["state"],
            "weight": int(row["weight"]),
            "created_at": from_iso(str(row["created_at"])),
            "updated_at": from_iso(str(row["updated_at"])),
        }
    )


def _row_to_comment(row: sqlite3.Row) -> Comment:
    return Comment(
        id=str(row["id"]),
        work_item_id=str(row["work_item_id"]),
        actor_id=str(row["actor_id"]),
        actor_display_name=str(row["actor_display_name"]),
        body=str(row["body"]),
        created_at=from_iso(str(row["created_at"])),
    )


def _work_where(work_filter: WorkFilter) -> tuple[str, list[object]]:
    """Build the shared WHERE clause every work view filters through."""
    clauses: list[str] = []
    params: list[object] = []

    if work_filter.project_id is not None:
        clauses.append("w.project_id = ?")
        params.append(work_filter.project_id)
    if work_filter.assignee_actor_id is not None:
        clauses.append("w.assignee_actor_id = ?")
        params.append(work_filter.assignee_actor_id)
    if work_filter.initiative_id is not None:
        clauses.append("w.initiative_id = ?")
        params.append(work_filter.initiative_id)

    for column, values in (
        ("w.kind", work_filter.kinds),
        ("w.state", work_filter.states),
        ("w.priority", work_filter.priorities),
        ("w.trust_state", work_filter.trust_states),
    ):
        if values:
            placeholders = ", ".join("?" for _ in values)
            clauses.append(f"{column} IN ({placeholders})")
            params.extend(values)

    if work_filter.exclude_terminal:
        terminal = sorted(TERMINAL_STATES)
        placeholders = ", ".join("?" for _ in terminal)
        clauses.append(f"w.state NOT IN ({placeholders})")
        params.extend(terminal)

    if work_filter.label is not None:
        clauses.append(
            "EXISTS (SELECT 1 FROM work_item_labels wl JOIN labels l "
            "ON l.id = wl.label_id WHERE wl.work_item_id = w.id AND l.name = ?)"
        )
        params.append(work_filter.label)

    where = f"WHERE {' AND '.join(clauses)}" if clauses else ""
    return where, params


def _upsert_workflow(
    conn: sqlite3.Connection, workflow: Workflow, *, at: datetime
) -> None:
    definition = json.dumps(workflow.to_definition(), sort_keys=True)
    updated = conn.execute(
        "UPDATE workflow_defs SET definition = ?, updated_at = ? WHERE kind = ?",
        (definition, to_iso(at), workflow.kind),
    ).rowcount
    if updated == 0:
        conn.execute(
            "INSERT INTO workflow_defs (kind, definition, updated_at) VALUES (?, ?, ?)",
            (workflow.kind, definition, to_iso(at)),
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
