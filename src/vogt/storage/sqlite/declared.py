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
from datetime import UTC, datetime, timedelta
from pathlib import Path

from vogt.core.clock import Clock, from_iso, to_iso, utc_now
from vogt.core.entities import (
    Actor,
    AuditRecord,
    AuthDecision,
    CodingSession,
    Comment,
    ContractExemption,
    DriftProposal,
    Event,
    ForgeAccount,
    InboxTriage,
    Initiative,
    Label,
    Project,
    Relation,
    RelationKind,
    Suppression,
    Token,
    WorkItem,
    WorkKind,
    WorkLink,
    WorkOverlay,
    WriteBackRecord,
)
from vogt.core.ids import IdFactory, new_id
from vogt.core.principal import Principal
from vogt.core.workflow import TERMINAL_STATES, Workflow, default_workflow
from vogt.errors import AlreadyInitialized, NotFound, NotInitialized
from vogt.storage.interface import (
    Blocker,
    BoardCellQuery,
    BootstrapResult,
    Counts,
    MigrationReport,
    ProjectUpdate,
    WorkFilter,
    WorkItemUpdate,
)
from vogt.storage.sqlite.connection import DEFAULT_SYNCHRONOUS, connect
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

#: How a project id resolves to the entity ids of one audited entity kind
#: (FR-U19's project filter).
#:
#: Every audited kind that the declared store can relate to a project is
#: here, so the filter cannot silently drop one: `project` itself, the work
#: items in it, the comments on those items, the coding sessions opened in
#: it, the drift proposals raised against it and the suppressions scoped to
#: it. The kinds that are absent — `instance`, `actor`, `label`,
#: `initiative`, `token` — are absent because they belong to the instance
#: rather than to any project, so no project's trail is missing them.
#:
#: Each is a semi-join through the owning table's own foreign key. Nothing
#: about a project is copied onto an audit row: `audit` describes one write
#: and is never rewritten when the thing it named later moves, and a
#: denormalised `project_id` would either be wrong after a work item is
#: reassigned or would have to be back-filled — editing history to make a
#: query cheaper.
_PROJECT_SCOPED_AUDIT: tuple[tuple[str, str], ...] = (
    ("project", "SELECT id FROM projects WHERE id = ?"),
    ("work_item", "SELECT id FROM work_items WHERE project_id = ?"),
    (
        "comment",
        "SELECT c.id FROM comments c JOIN work_items w ON w.id = c.work_item_id "
        "WHERE w.project_id = ?",
    ),
    ("session", "SELECT id FROM coding_sessions WHERE project_id = ?"),
    ("drift_proposal", "SELECT id FROM drift_proposals WHERE project_id = ?"),
    ("suppression", "SELECT id FROM suppressions WHERE scope_project_id = ?"),
)

#: The order the audit log is read in, newest write first. `id` is the
#: primary key, so the three columns are a total order: two pages taken from
#: an unchanged log neither skip a record nor repeat one.
_AUDIT_ORDER = "ORDER BY a.revision DESC, a.at DESC, a.id DESC"


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
        synchronous: str = DEFAULT_SYNCHRONOUS,
    ) -> None:
        self._path = path
        self._synchronous = synchronous
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
        conn = connect(self._path, create=True, synchronous=self._synchronous)
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
        conn = connect(self._path, create=False, synchronous=self._synchronous)
        try:
            return self._migrator.applied_version(conn)
        finally:
            conn.close()

    def bundled_schema_version(self) -> int:
        """What this build expects (NFR-I3). Touches no database."""
        return self._migrator.bundled_version()

    def is_initialized(self) -> bool:
        if not self._path.exists():
            return False
        conn = connect(self._path, create=False, synchronous=self._synchronous)
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
        conn = connect(self._path, create=True, synchronous=self._synchronous)
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

    def publish_event(
        self,
        *,
        kind: str,
        entity_kind: str,
        entity_id: str,
        summary: dict[str, object],
        at: datetime,
    ) -> Event:
        """Append an observed-side event (FR-N1, `SCHEMA.md` §2.5).

        No audit row and no revision bump: nobody declared anything and the
        authoritative state did not change. This is the application layer
        publishing on the collectors' behalf, which is what lets collectors
        keep their promise never to write the declared store (FR-O2).
        """
        conn = self._open_initialized()
        try:
            conn.execute("BEGIN IMMEDIATE")
            txn = SqliteWriteTxn(
                conn,
                txn_id=self._id_factory("txn"),
                revision=_read_revision(conn),
                id_factory=self._id_factory,
            )
            event = txn.append_event(
                kind=kind,
                entity_kind=entity_kind,
                entity_id=entity_id,
                actor_id=None,
                audit_id=None,
                summary=summary,
                at=at,
            )
            conn.execute("COMMIT")
            return event
        except BaseException:
            _rollback_quietly(conn)
            raise
        finally:
            conn.close()

    def record_auth_decision(self, decision: AuthDecision) -> None:
        """Append an authorization decision (FR-S5).

        Like `publish_event`, this is not a declared write: nothing changed,
        nobody supplied a reason, and it happens on reads too. Recording it
        in `audit` would make "every audit row is a change" false.
        """
        conn = self._open_initialized()
        try:
            conn.execute("BEGIN IMMEDIATE")
            conn.execute(
                "INSERT INTO auth_decisions (id, at, decision, reason_code, "
                "operation, scope, actor_id, token_id, identity_ref, transport, "
                "detail) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                (
                    decision.id,
                    to_iso(decision.at),
                    decision.decision,
                    decision.reason_code,
                    decision.operation,
                    decision.scope,
                    decision.actor_id,
                    decision.token_id,
                    decision.identity_ref,
                    decision.transport,
                    decision.detail,
                ),
            )
            conn.execute("COMMIT")
        except BaseException:
            _rollback_quietly(conn)
            raise
        finally:
            conn.close()

    def touch_token(self, token_id: str, *, at: datetime) -> None:
        """Record that a token was used, for the operator's benefit."""
        conn = self._open_initialized()
        try:
            conn.execute("BEGIN IMMEDIATE")
            conn.execute(
                "UPDATE tokens SET last_used_at = ? WHERE id = ?",
                (to_iso(at), token_id),
            )
            conn.execute("COMMIT")
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
            # A ReadView promises one consistent snapshot. SQLite otherwise
            # starts and ends a read transaction around each SELECT, which
            # would let a count and its following page observe two revisions.
            conn.execute("BEGIN")
            yield SqliteReadView(conn)
        finally:
            _rollback_quietly(conn)
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
        conn = connect(self._path, create=False, synchronous=self._synchronous)
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

    def board_high_water(self, work_filter: WorkFilter) -> tuple[datetime, str] | None:
        """Newest stable work key in this filtered Board snapshot."""
        where, params = _work_where(work_filter)
        row = self._conn.execute(
            f"SELECT w.created_at, w.ref {_WORK_JOINS} {where} "
            "ORDER BY w.created_at DESC, w.ref DESC LIMIT 1",
            tuple(params),
        ).fetchone()
        if row is None:
            return None
        return from_iso(str(row["created_at"])), str(row["ref"])

    def board_counts(
        self,
        work_filter: WorkFilter,
        *,
        lane_mode: str,
        high_water: tuple[datetime, str] | None,
    ) -> dict[tuple[str, str], int]:
        """Exact cell totals, grouped in one query under the snapshot bound."""
        if high_water is None:
            return {}
        where, params = _work_where(work_filter)
        where, params = _with_board_high_water(where, params, high_water)
        lane = _board_lane_expression(lane_mode)
        rows = self._conn.execute(
            f"SELECT {lane} AS board_lane, w.state, COUNT(*) AS n "
            f"{_WORK_JOINS} {where} GROUP BY {lane}, w.state",
            tuple(params),
        ).fetchall()
        return {
            (str(row["board_lane"]), str(row["state"])): int(row["n"]) for row in rows
        }

    def board_work_items(
        self,
        work_filter: WorkFilter,
        *,
        lane_mode: str,
        cells: tuple[BoardCellQuery, ...],
        high_water: tuple[datetime, str] | None,
        limit: int,
    ) -> dict[tuple[str, str], list[WorkItem]]:
        """Read bounded pages for all requested cells in one SQL query."""
        result: dict[tuple[str, str], list[WorkItem]] = {
            (cell.lane_key, cell.state): [] for cell in cells
        }
        if not cells or high_water is None:
            return result

        where, params = _work_where(work_filter)
        where, params = _with_board_high_water(where, params, high_water)
        lane = _board_lane_expression(lane_mode)
        requested: list[str] = []
        for cell in cells:
            clause = [f"{lane} = ?", "w.state = ?"]
            params.extend((cell.lane_key, cell.state))
            if cell.after_created_at is not None and cell.after_ref is not None:
                clause.append("(w.created_at > ? OR (w.created_at = ? AND w.ref > ?))")
                moment = to_iso(cell.after_created_at)
                params.extend((moment, moment, cell.after_ref))
            requested.append(f"({' AND '.join(clause)})")
        where = _append_where(where, f"({' OR '.join(requested)})")

        rows = self._conn.execute(
            "WITH requested AS ("
            f"SELECT {_WORK_COLUMNS}, {lane} AS board_lane, "
            "ROW_NUMBER() OVER ("
            f"PARTITION BY {lane}, w.state ORDER BY w.created_at, w.ref"
            f") AS board_row {_WORK_JOINS} {where}"
            ") SELECT * FROM requested WHERE board_row <= ? "
            "ORDER BY board_lane, state, created_at, ref",
            (*params, limit),
        ).fetchall()
        hydrated = self._hydrate(rows)
        for row, item in zip(rows, hydrated, strict=True):
            result[(str(row["board_lane"]), item.state)].append(item)
        return result

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

    # -- observed-first ----------------------------------------------------

    def list_suppressions(
        self, *, include_revoked: bool = False, limit: int = 100
    ) -> list[Suppression]:
        clause = "" if include_revoked else "WHERE s.revoked_at IS NULL"
        rows = self._conn.execute(
            "SELECT s.*, a.identity_ref AS actor_identity_ref, "
            "p.slug AS scope_project_slug FROM suppressions s "
            "JOIN actors a ON a.id = s.actor_id "
            "LEFT JOIN projects p ON p.id = s.scope_project_id "
            f"{clause} ORDER BY s.created_at DESC, s.id DESC LIMIT ?",
            (limit,),
        ).fetchall()
        return [_row_to_suppression(row) for row in rows]

    def contract_exemptions(self, project_id: str) -> list[ContractExemption]:
        rows = self._conn.execute(
            "SELECT e.*, p.slug AS project_slug FROM contract_exemptions e "
            "JOIN projects p ON p.id = e.project_id "
            "WHERE e.project_id = ? ORDER BY e.rule, e.target",
            (project_id,),
        ).fetchall()
        return [_row_to_contract_exemption(row) for row in rows]

    def suppression_by_id(self, suppression_id: str) -> Suppression | None:
        row = self._conn.execute(
            "SELECT s.*, a.identity_ref AS actor_identity_ref, "
            "p.slug AS scope_project_slug FROM suppressions s "
            "JOIN actors a ON a.id = s.actor_id "
            "LEFT JOIN projects p ON p.id = s.scope_project_id "
            "WHERE s.id = ?",
            (suppression_id,),
        ).fetchone()
        return None if row is None else _row_to_suppression(row)

    def work_links_for_subjects(self, subject_keys: list[str]) -> dict[str, str]:
        if not subject_keys:
            return {}
        placeholders = ", ".join("?" for _ in subject_keys)
        rows = self._conn.execute(
            "SELECT l.subject_key AS subject_key, w.ref AS ref FROM work_links l "
            "JOIN work_items w ON w.id = l.work_item_id "
            f"WHERE l.subject_key IN ({placeholders})",
            tuple(subject_keys),
        ).fetchall()
        return {str(row["subject_key"]): str(row["ref"]) for row in rows}

    # -- tokens ------------------------------------------------------------

    def token_by_hash(self, token_hash: str) -> Token | None:
        row = self._conn.execute(
            "SELECT t.*, a.identity_ref AS actor_identity_ref FROM tokens t "
            "JOIN actors a ON a.id = t.actor_id WHERE t.token_hash = ?",
            (token_hash,),
        ).fetchone()
        return None if row is None else _row_to_token(row)

    def token_by_id(self, token_id: str) -> Token | None:
        row = self._conn.execute(
            "SELECT t.*, a.identity_ref AS actor_identity_ref FROM tokens t "
            "JOIN actors a ON a.id = t.actor_id WHERE t.id = ?",
            (token_id,),
        ).fetchone()
        return None if row is None else _row_to_token(row)

    def list_tokens(
        self, *, include_revoked: bool = False, limit: int = 100
    ) -> list[Token]:
        clause = "" if include_revoked else "WHERE t.revoked_at IS NULL"
        rows = self._conn.execute(
            "SELECT t.*, a.identity_ref AS actor_identity_ref FROM tokens t "
            f"JOIN actors a ON a.id = t.actor_id {clause} "
            "ORDER BY t.created_at DESC, t.id DESC LIMIT ?",
            (limit,),
        ).fetchall()
        return [_row_to_token(row) for row in rows]

    def tokens_for_actor(
        self, actor_id: str, *, include_revoked: bool = False
    ) -> list[Token]:
        clause = "" if include_revoked else "AND t.revoked_at IS NULL"
        rows = self._conn.execute(
            "SELECT t.*, a.identity_ref AS actor_identity_ref FROM tokens t "
            f"JOIN actors a ON a.id = t.actor_id WHERE t.actor_id = ? {clause} "
            "ORDER BY t.created_at DESC, t.id DESC",
            (actor_id,),
        ).fetchall()
        return [_row_to_token(row) for row in rows]

    def list_auth_decisions(
        self, *, decision: str | None = None, limit: int = 100
    ) -> list[AuthDecision]:
        clause = "WHERE decision = ?" if decision else ""
        params: tuple[object, ...] = (decision, limit) if decision else (limit,)
        rows = self._conn.execute(
            f"SELECT * FROM auth_decisions {clause} ORDER BY at DESC, id DESC LIMIT ?",
            params,
        ).fetchall()
        return [_row_to_auth_decision(row) for row in rows]

    # -- forge accounts (per-actor PATs, #179) -----------------------------

    def forge_account(self, *, actor_id: str, host: str) -> ForgeAccount | None:
        row = self._conn.execute(
            "SELECT actor_id, host, login, scopes, created_at, updated_at "
            "FROM forge_accounts WHERE actor_id = ? AND host = ?",
            (actor_id, host),
        ).fetchone()
        return None if row is None else _row_to_forge_account(row)

    def forge_accounts_for_actor(self, actor_id: str) -> list[ForgeAccount]:
        rows = self._conn.execute(
            "SELECT actor_id, host, login, scopes, created_at, updated_at "
            "FROM forge_accounts WHERE actor_id = ? ORDER BY created_at DESC, host",
            (actor_id,),
        ).fetchall()
        return [_row_to_forge_account(row) for row in rows]

    def forge_account_secret(self, *, actor_id: str, host: str) -> str | None:
        row = self._conn.execute(
            "SELECT encrypted_token FROM forge_accounts "
            "WHERE actor_id = ? AND host = ?",
            (actor_id, host),
        ).fetchone()
        return None if row is None else str(row["encrypted_token"])

    # -- drift -------------------------------------------------------------

    def list_drift(
        self,
        *,
        status: str | None = "open",
        kind: str | None = None,
        project_id: str | None = None,
        limit: int = 100,
    ) -> list[DriftProposal]:
        clauses: list[str] = []
        params: list[object] = []
        for column, value in (
            ("d.status", status),
            ("d.kind", kind),
            ("d.project_id", project_id),
        ):
            if value is not None:
                clauses.append(f"{column} = ?")
                params.append(value)
        where = f"WHERE {' AND '.join(clauses)}" if clauses else ""
        params.append(limit)
        rows = self._conn.execute(
            "SELECT d.*, p.slug AS project_slug, a.identity_ref AS resolved_by "
            "FROM drift_proposals d "
            "LEFT JOIN projects p ON p.id = d.project_id "
            "LEFT JOIN actors a ON a.id = d.resolved_by_actor_id "
            f"{where} ORDER BY d.opened_at DESC, d.id DESC LIMIT ?",
            tuple(params),
        ).fetchall()
        return [_row_to_drift(row) for row in rows]

    # -- inbox triage -------------------------------------------------------

    def inbox_triage_by_key(self, entry_key: str) -> InboxTriage | None:
        row = self._conn.execute(
            "SELECT t.*, a.identity_ref AS actor_identity_ref "
            "FROM inbox_triage t JOIN actors a ON a.id = t.actor_id "
            "WHERE t.entry_key = ?",
            (entry_key,),
        ).fetchone()
        return None if row is None else _row_to_inbox_triage(row)

    def list_inbox_triage(self, *, limit: int = 10_000) -> list[InboxTriage]:
        rows = self._conn.execute(
            "SELECT t.*, a.identity_ref AS actor_identity_ref "
            "FROM inbox_triage t JOIN actors a ON a.id = t.actor_id "
            "ORDER BY t.decided_at DESC, t.entry_key DESC LIMIT ?",
            (limit,),
        ).fetchall()
        return [_row_to_inbox_triage(row) for row in rows]

    def drift_by_id(self, proposal_id: str) -> DriftProposal | None:
        row = self._conn.execute(
            "SELECT d.*, p.slug AS project_slug, a.identity_ref AS resolved_by "
            "FROM drift_proposals d "
            "LEFT JOIN projects p ON p.id = d.project_id "
            "LEFT JOIN actors a ON a.id = d.resolved_by_actor_id "
            "WHERE d.id = ?",
            (proposal_id,),
        ).fetchone()
        return None if row is None else _row_to_drift(row)

    def open_drift_subjects(self) -> set[tuple[str, str, str]]:
        """(kind, subject_kind, subject_id) for every open proposal."""
        rows = self._conn.execute(
            "SELECT kind, subject_kind, subject_id FROM drift_proposals "
            "WHERE status = 'open'"
        ).fetchall()
        return {
            (str(r["kind"]), str(r["subject_kind"]), str(r["subject_id"])) for r in rows
        }

    def list_writeback_actions(
        self, *, outcome: str | None = None, limit: int = 100
    ) -> list[WriteBackRecord]:
        clause = "WHERE outcome = ?" if outcome else ""
        params: tuple[object, ...] = (outcome, limit) if outcome else (limit,)
        rows = self._conn.execute(
            f"SELECT * FROM writeback_actions {clause} "
            "ORDER BY at DESC, id DESC LIMIT ?",
            params,
        ).fetchall()
        return [_row_to_writeback(row) for row in rows]

    def drift_evidence_ids(self) -> frozenset[str]:
        """Observation ids any proposal references, of any status (FR-R5)."""
        rows = self._conn.execute(
            "SELECT DISTINCT evidence_observation_id FROM drift_proposals "
            "WHERE evidence_observation_id IS NOT NULL"
        ).fetchall()
        return frozenset(str(row["evidence_observation_id"]) for row in rows)

    def work_links_for_subjects_by_item(self, work_item_id: str) -> dict[str, str]:
        """Subject key to origin kind, for one work item."""
        rows = self._conn.execute(
            "SELECT subject_key, origin_kind FROM work_links WHERE work_item_id = ?",
            (work_item_id,),
        ).fetchall()
        return {str(r["subject_key"]): str(r["origin_kind"]) for r in rows}

    def work_item_by_subject(self, subject_key: str) -> WorkItem | None:
        row = self._conn.execute(
            f"SELECT {_WORK_COLUMNS} {_WORK_JOINS} "
            "JOIN work_links l ON l.work_item_id = w.id WHERE l.subject_key = ? "
            "LIMIT 1",
            (subject_key,),
        ).fetchone()
        if row is None:
            return None
        return self._hydrate([row])[0]

    # -- the upstream-truth overlay (#181) ---------------------------------

    def work_overlay(self, subject_key: str) -> WorkOverlay | None:
        row = self._conn.execute(
            "SELECT * FROM work_overlay WHERE subject_key = ?", (subject_key,)
        ).fetchone()
        return None if row is None else _row_to_overlay(row)

    def work_overlays(self, subject_keys: list[str]) -> dict[str, WorkOverlay]:
        if not subject_keys:
            return {}
        placeholders = ", ".join("?" for _ in subject_keys)
        rows = self._conn.execute(
            f"SELECT * FROM work_overlay WHERE subject_key IN ({placeholders})",
            tuple(subject_keys),
        ).fetchall()
        return {str(row["subject_key"]): _row_to_overlay(row) for row in rows}

    # -- sessions ----------------------------------------------------------

    def session_by_id(self, session_id: str) -> CodingSession | None:
        row = self._conn.execute(
            "SELECT * FROM coding_sessions WHERE id = ?", (session_id,)
        ).fetchone()
        return None if row is None else _row_to_session(row)

    def session_by_engine_id(self, engine_session_id: str) -> CodingSession | None:
        row = self._conn.execute(
            "SELECT * FROM coding_sessions WHERE engine_session_id = ?",
            (engine_session_id,),
        ).fetchone()
        return None if row is None else _row_to_session(row)

    def list_sessions(
        self,
        *,
        project_id: str | None = None,
        work_item_id: str | None = None,
        include_stopped: bool = False,
        limit: int,
        offset: int,
    ) -> list[CodingSession]:
        clauses: list[str] = []
        params: list[object] = []
        for column, value in (
            ("project_id", project_id),
            ("work_item_id", work_item_id),
        ):
            if value is not None:
                clauses.append(f"{column} = ?")
                params.append(value)
        if not include_stopped:
            clauses.append("stopped_at IS NULL")
        where = f"WHERE {' AND '.join(clauses)}" if clauses else ""
        params.extend((limit, offset))
        rows = self._conn.execute(
            f"SELECT * FROM coding_sessions {where} "
            "ORDER BY started_at DESC, id DESC LIMIT ? OFFSET ?",
            tuple(params),
        ).fetchall()
        return [_row_to_session(row) for row in rows]

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

    def list_events(
        self, *, after: int, limit: int, entity_id: str | None = None
    ) -> list[Event]:
        # Narrowed in SQL rather than after the read, because the feed is a
        # single ordered table and an entity's history is a sparse slice of
        # it: filtering a page would return a page of *the feed* that happens
        # to contain some of this entity's events, and a caller paging that
        # would conclude the history had ended at the first quiet stretch.
        where = "seq > ?"
        params: list[object] = [after]
        if entity_id is not None:
            where += " AND entity_id = ?"
            params.append(entity_id)
        params.append(limit)
        rows = self._conn.execute(
            f"SELECT * FROM events WHERE {where} ORDER BY seq LIMIT ?",
            tuple(params),
        ).fetchall()
        return [_row_to_event(row) for row in rows]

    def list_audit(
        self,
        *,
        limit: int,
        offset: int = 0,
        actor_id: str | None = None,
        operation: str | None = None,
        entity_id: str | None = None,
        project_id: str | None = None,
        since: datetime | None = None,
        until: datetime | None = None,
    ) -> list[AuditRecord]:
        where, params = _audit_where(
            actor_id=actor_id,
            operation=operation,
            entity_id=entity_id,
            project_id=project_id,
            since=since,
            until=until,
        )
        params.extend((limit, offset))
        rows = self._conn.execute(
            "SELECT a.*, ac.identity_ref AS actor_identity_ref "
            "FROM audit a JOIN actors ac ON ac.id = a.actor_id "
            f"{where} {_AUDIT_ORDER} LIMIT ? OFFSET ?",
            tuple(params),
        ).fetchall()
        return [_row_to_audit(row) for row in rows]

    def count_audit(
        self,
        *,
        actor_id: str | None = None,
        operation: str | None = None,
        entity_id: str | None = None,
        project_id: str | None = None,
        since: datetime | None = None,
        until: datetime | None = None,
    ) -> int:
        where, params = _audit_where(
            actor_id=actor_id,
            operation=operation,
            entity_id=entity_id,
            project_id=project_id,
            since=since,
            until=until,
        )
        row = self._conn.execute(
            f"SELECT COUNT(*) AS n FROM audit a {where}", tuple(params)
        ).fetchone()
        return int(row["n"])


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
            "compliance_status, compliance_checked_at, contract_adopted_at, "
            "write_back, link_state, exclusions, trust_state, created_at, "
            "updated_at) "
            "VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
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
                None
                if project.contract_adopted_at is None
                else to_iso(project.contract_adopted_at),
                project.write_back,
                project.link_state,
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
            ("write_back", update.write_back),
            ("link_state", update.link_state),
        ):
            if value is not None:
                assignments.append(f"{column} = ?")
                params.append(value)
        if update.compliance_checked_at is not None:
            assignments.append("compliance_checked_at = ?")
            params.append(to_iso(update.compliance_checked_at))
        if update.contract_adopted_at is not None:
            assignments.append("contract_adopted_at = ?")
            params.append(to_iso(update.contract_adopted_at))
        elif update.clear_contract_adopted_at:
            assignments.append("contract_adopted_at = NULL")
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
            ("superseded_by", update.superseded_by),
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

    def insert_suppression(self, suppression: Suppression) -> None:
        self._conn.execute(
            "INSERT INTO suppressions (id, match_kind, subject_key_or_pattern, "
            "scope_project_id, actor_id, reason, created_at) "
            "VALUES (?, ?, ?, ?, ?, ?, ?)",
            (
                suppression.id,
                suppression.match_kind,
                suppression.subject_key_or_pattern,
                suppression.scope_project_id,
                suppression.actor_id,
                suppression.reason,
                to_iso(suppression.created_at),
            ),
        )

    def insert_contract_exemption(self, exemption: ContractExemption) -> None:
        self._conn.execute(
            "INSERT INTO contract_exemptions "
            "(id, project_id, rule, target, reason, declared_by, declared_at) "
            "VALUES (?, ?, ?, ?, ?, ?, ?) "
            # Re-declaring is a fresh statement with a fresh reason, not a
            # duplicate row and not an error: somebody looked again.
            "ON CONFLICT (project_id, rule, target) DO UPDATE SET "
            "reason = excluded.reason, declared_by = excluded.declared_by, "
            "declared_at = excluded.declared_at",
            (
                exemption.id,
                exemption.project_id,
                exemption.rule,
                exemption.target,
                exemption.reason,
                exemption.declared_by,
                to_iso(exemption.declared_at),
            ),
        )

    def delete_contract_exemption(
        self, *, project_id: str, rule: str, target: str
    ) -> bool:
        cursor = self._conn.execute(
            "DELETE FROM contract_exemptions "
            "WHERE project_id = ? AND rule = ? AND target = ?",
            (project_id, rule, target),
        )
        return cursor.rowcount > 0

    def revoke_suppression(
        self, suppression_id: str, *, actor_id: str, reason: str, at: datetime
    ) -> bool:
        cursor = self._conn.execute(
            "UPDATE suppressions SET revoked_at = ?, revoked_by_actor_id = ?, "
            "revoked_reason = ? WHERE id = ? AND revoked_at IS NULL",
            (to_iso(at), actor_id, reason, suppression_id),
        )
        return cursor.rowcount > 0

    def insert_token(self, token: Token, *, token_hash: str) -> None:
        self._conn.execute(
            "INSERT INTO tokens (id, actor_id, name, token_hash, scopes, "
            "created_at, expires_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
            (
                token.id,
                token.actor_id,
                token.name,
                token_hash,
                json.dumps(token.scopes),
                to_iso(token.created_at),
                None if token.expires_at is None else to_iso(token.expires_at),
            ),
        )

    def revoke_token(self, token_id: str, *, reason: str, at: datetime) -> bool:
        cursor = self._conn.execute(
            "UPDATE tokens SET revoked_at = ?, revoked_reason = ? "
            "WHERE id = ? AND revoked_at IS NULL",
            (to_iso(at), reason, token_id),
        )
        return cursor.rowcount > 0

    def upsert_forge_account(
        self,
        *,
        actor_id: str,
        host: str,
        login: str,
        scopes: str,
        encrypted_token: str,
        at: datetime,
    ) -> None:
        stamp = to_iso(at)
        # Re-linking keeps the original created_at and rotates everything else,
        # including the ciphertext. Written portably (no INSERT OR REPLACE)
        # because NFR-S3 forbids SQLite-only semantics above the backend.
        cursor = self._conn.execute(
            "UPDATE forge_accounts SET login = ?, scopes = ?, "
            "encrypted_token = ?, updated_at = ? WHERE actor_id = ? AND host = ?",
            (login, scopes, encrypted_token, stamp, actor_id, host),
        )
        if cursor.rowcount == 0:
            self._conn.execute(
                "INSERT INTO forge_accounts (actor_id, host, login, scopes, "
                "encrypted_token, created_at, updated_at) "
                "VALUES (?, ?, ?, ?, ?, ?, ?)",
                (actor_id, host, login, scopes, encrypted_token, stamp, stamp),
            )

    def delete_forge_account(self, *, actor_id: str, host: str) -> bool:
        cursor = self._conn.execute(
            "DELETE FROM forge_accounts WHERE actor_id = ? AND host = ?",
            (actor_id, host),
        )
        return cursor.rowcount > 0

    def insert_writeback(self, record: WriteBackRecord) -> None:
        self._conn.execute(
            "INSERT INTO writeback_actions (id, at, project_id, work_item_id, "
            "actor_id, action, subject_key, policy, outcome, reason, detail, "
            "source_url) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            (
                record.id,
                to_iso(record.at),
                record.project_id,
                record.work_item_id,
                record.actor_id,
                record.action,
                record.subject_key,
                record.policy,
                record.outcome,
                record.reason,
                record.detail,
                record.source_url,
            ),
        )

    def insert_session(self, session: CodingSession) -> None:
        self._conn.execute(
            "INSERT INTO coding_sessions (id, engine_session_id, project_id, "
            "work_item_id, actor_id, cwd, template, model, effort, reason, "
            "started_at, stopped_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            (
                session.id,
                session.engine_session_id,
                session.project_id,
                session.work_item_id,
                session.actor_id,
                session.cwd,
                session.template,
                session.model,
                session.effort,
                session.reason,
                to_iso(session.started_at),
                None if session.stopped_at is None else to_iso(session.stopped_at),
            ),
        )

    def mark_session_stopped(self, session_id: str, *, at: datetime) -> None:
        self._conn.execute(
            "UPDATE coding_sessions SET stopped_at = ? "
            "WHERE id = ? AND stopped_at IS NULL",
            (to_iso(at), session_id),
        )

    def insert_drift(self, proposal: DriftProposal) -> None:
        self._conn.execute(
            "INSERT INTO drift_proposals (id, kind, subject_kind, subject_id, "
            "project_id, summary, evidence_observation_id, evidence_snapshot, "
            "proposed_change, status, opened_at) "
            "VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            (
                proposal.id,
                proposal.kind,
                proposal.subject_kind,
                proposal.subject_id,
                proposal.project_id,
                proposal.summary,
                proposal.evidence_observation_id,
                json.dumps(proposal.evidence_snapshot, default=str),
                json.dumps(proposal.proposed_change, default=str),
                proposal.status,
                to_iso(proposal.opened_at),
            ),
        )

    def upsert_inbox_triage(self, triage: InboxTriage) -> None:
        self._conn.execute(
            "INSERT INTO inbox_triage (entry_key, state, snooze_until, actor_id, "
            "decided_at, occurrence_snapshot) VALUES (?, ?, ?, ?, ?, ?) "
            "ON CONFLICT(entry_key) DO UPDATE SET state = excluded.state, "
            "snooze_until = excluded.snooze_until, actor_id = excluded.actor_id, "
            "decided_at = excluded.decided_at, "
            "occurrence_snapshot = excluded.occurrence_snapshot",
            (
                triage.entry_key,
                triage.state,
                None if triage.snooze_until is None else to_iso(triage.snooze_until),
                triage.actor_id,
                to_iso(triage.decided_at),
                json.dumps(triage.occurrence_snapshot, default=str, sort_keys=True),
            ),
        )

    def mark_drift_superseded(
        self,
        proposal_id: str,
        *,
        detail: str | None,
        at: datetime | None,
    ) -> bool:
        """Flag, or un-flag, an open proposal as raised under stale evidence.

        Only open proposals: a resolved one is history, and rewriting history
        because a later sweep disagreed with it would destroy the record of
        what somebody decided and on what basis.
        """
        cursor = self._conn.execute(
            "UPDATE drift_proposals SET superseded_at = ?, superseded_detail = ? "
            "WHERE id = ? AND status = 'open'",
            (None if at is None else to_iso(at), detail, proposal_id),
        )
        return cursor.rowcount > 0

    def resolve_drift(
        self,
        proposal_id: str,
        *,
        status: str,
        actor_id: str,
        reason: str,
        at: datetime,
    ) -> bool:
        cursor = self._conn.execute(
            "UPDATE drift_proposals SET status = ?, resolved_by_actor_id = ?, "
            "resolution_reason = ?, resolved_at = ? "
            "WHERE id = ? AND status = 'open'",
            (status, actor_id, reason, to_iso(at), proposal_id),
        )
        return cursor.rowcount > 0

    def upsert_work_overlay(self, overlay: WorkOverlay) -> None:
        # `created_at` keeps the existing row's value on conflict, so the
        # overlay records when local semantics first attached to the subject,
        # not when they last moved — `updated_at` carries that.
        self._conn.execute(
            "INSERT INTO work_overlay (subject_key, project_id, rank, "
            "workflow_state, priority, effort, assignee_actor_id, "
            "initiative_id, created_at, updated_at) "
            "VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) "
            "ON CONFLICT(subject_key) DO UPDATE SET "
            "project_id = excluded.project_id, rank = excluded.rank, "
            "workflow_state = excluded.workflow_state, "
            "priority = excluded.priority, effort = excluded.effort, "
            "assignee_actor_id = excluded.assignee_actor_id, "
            "initiative_id = excluded.initiative_id, "
            "updated_at = excluded.updated_at",
            (
                overlay.subject_key,
                overlay.project_id,
                overlay.rank,
                overlay.workflow_state,
                overlay.priority,
                overlay.effort,
                overlay.assignee_actor_id,
                overlay.initiative_id,
                to_iso(overlay.created_at),
                to_iso(overlay.updated_at),
            ),
        )

    def insert_work_link(self, link: WorkLink) -> None:
        self._conn.execute(
            "INSERT INTO work_links (work_item_id, subject_key, origin_kind, "
            "source_url, relation, created_at) VALUES (?, ?, ?, ?, ?, ?)",
            (
                link.work_item_id,
                link.subject_key,
                link.origin_kind,
                link.source_url,
                link.relation,
                to_iso(link.created_at),
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
    adopted_at = row["contract_adopted_at"]
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
            "write_back": row["write_back"],
            "link_state": row["link_state"],
            "compliance_checked_at": (
                None if checked_at is None else from_iso(str(checked_at))
            ),
            "exclusions": json.loads(str(row["exclusions"])),
            "contract_adopted_at": (
                None if adopted_at is None else from_iso(str(adopted_at))
            ),
            "trust_state": row["trust_state"],
            "created_at": from_iso(str(row["created_at"])),
            "updated_at": from_iso(str(row["updated_at"])),
        }
    )


def _row_to_contract_exemption(row: sqlite3.Row) -> ContractExemption:
    return ContractExemption.model_validate(
        {
            "id": str(row["id"]),
            "project_id": str(row["project_id"]),
            "project_slug": row["project_slug"],
            "rule": str(row["rule"]),
            "target": str(row["target"]),
            "reason": str(row["reason"]),
            "declared_by": str(row["declared_by"]),
            "declared_at": from_iso(str(row["declared_at"])),
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
            "superseded_by": row["superseded_by"],
            "created_at": from_iso(str(row["created_at"])),
            "updated_at": from_iso(str(row["updated_at"])),
        }
    )


def _row_to_overlay(row: sqlite3.Row) -> WorkOverlay:
    return WorkOverlay.model_validate(
        {
            "subject_key": str(row["subject_key"]),
            "project_id": str(row["project_id"]),
            "rank": row["rank"],
            "workflow_state": row["workflow_state"],
            "priority": row["priority"],
            "effort": row["effort"],
            "assignee_actor_id": row["assignee_actor_id"],
            "initiative_id": row["initiative_id"],
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

    if not work_filter.include_superseded:
        # A native row that migrated upstream (#183) is retired — the
        # subject-keyed upstream item is the item now, and listing the husk
        # beside it would be the double-count the migration exists to end.
        # The row stays reachable by ref/id for anyone following an old
        # trail, and export asks for it explicitly.
        clauses.append("w.superseded_by IS NULL")

    if work_filter.exclude_unlinked_native:
        # The #183 surface withdrawal: a native row on an unlinked project is
        # not part of the curated work surfaces any more. Project-less items
        # pass — there is no project to be linked.
        clauses.append(
            "(w.project_id IS NULL OR EXISTS (SELECT 1 FROM projects lp "
            "WHERE lp.id = w.project_id AND lp.link_state = 'linked'))"
        )

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


def _append_where(where: str, clause: str) -> str:
    """Append one backend-owned predicate to a shared work filter."""
    return f"{where} AND {clause}" if where else f"WHERE {clause}"


def _with_board_high_water(
    where: str,
    params: list[object],
    high_water: tuple[datetime, str],
) -> tuple[str, list[object]]:
    moment = to_iso(high_water[0])
    return (
        _append_where(
            where,
            "(w.created_at < ? OR (w.created_at = ? AND w.ref <= ?))",
        ),
        [*params, moment, moment, high_water[1]],
    )


def _board_lane_expression(lane_mode: str) -> str:
    """SQL lane expression selected only from the closed application enum."""
    if lane_mode == "none":
        return "''"
    if lane_mode == "project":
        return "COALESCE(p.slug, '')"
    if lane_mode == "initiative":
        return "COALESCE(w.initiative_id, '')"
    msg = f"unknown Board lane mode: {lane_mode}"
    raise ValueError(msg)


def _audit_bound(moment: datetime | None) -> str | None:
    """Render a time bound the way `at` is stored, so text compares right.

    Every `at` is written through `to_iso`, which converts to UTC first, so
    stored timestamps all carry `+00:00` and sort lexicographically in the
    order they happened. A bound has to be rendered the same way to be
    compared against them — and a *naive* bound is read as UTC, the same rule
    `from_iso` applies to stored text. Passing it to `astimezone` instead
    would silently reinterpret it in whatever zone the server happens to sit
    in, which would move the boundary of an audit query by an hour on one
    host and not on another.
    """
    if moment is None:
        return None
    if moment.tzinfo is None:
        moment = moment.replace(tzinfo=UTC)
    return to_iso(moment)


def _audit_where(
    *,
    actor_id: str | None,
    operation: str | None,
    entity_id: str | None,
    project_id: str | None,
    since: datetime | None,
    until: datetime | None,
) -> tuple[str, list[object]]:
    """Build the WHERE clause both audit reads filter through.

    One builder because `list_audit` and `count_audit` answer the same
    question — a total that counted a different set from the records beside
    it would be a page indicator that lies.
    """
    clauses: list[str] = []
    params: list[object] = []

    if actor_id is not None:
        clauses.append("a.actor_id = ?")
        params.append(actor_id)
    if operation is not None:
        clauses.append("a.operation = ?")
        params.append(operation)
    if entity_id is not None:
        # An entity's trail, not its rows. A comment is audited against the
        # comment — `entity_kind = 'comment'`, `entity_id = <comment id>` —
        # so an exact match on a work item's id returns its creation, its
        # updates and its transitions and silently omits everything anybody
        # said about it. `comments` already carries the link and is indexed
        # on it (`idx_comments_work_item`), so the trail is a semi-join and
        # nothing has to be written twice.
        #
        # The rejected alternative is denormalising `work_item_id` onto
        # `audit`: it would need a back-fill to answer for rows already
        # written, and back-filling `audit` means editing the record of what
        # happened — the one table in this product that must only ever be
        # appended to. A third option, deriving the item from the comment id,
        # is not available either: ids here are opaque (`cmt_0001`) and carry
        # no parent, deliberately.
        #
        # The clause is written so it needs no lookup first: an `entity_id`
        # that names something other than a work item simply matches no
        # comment.
        clauses.append(
            "(a.entity_id = ? OR (a.entity_kind = 'comment' AND a.entity_id IN "
            "(SELECT id FROM comments WHERE work_item_id = ?)))"
        )
        params.extend((entity_id, entity_id))
    if project_id is not None:
        scoped: list[str] = []
        for kind, resolver in _PROJECT_SCOPED_AUDIT:
            scoped.append(f"(a.entity_kind = '{kind}' AND a.entity_id IN ({resolver}))")
            params.append(project_id)
        clauses.append(f"({' OR '.join(scoped)})")
    # `since` is inclusive and `until` is exclusive, so consecutive windows
    # tile the log exactly: [Mon, Tue) and [Tue, Wed) between them contain
    # every write, once. Two inclusive bounds would return a write made at
    # midnight in both windows, and a reader counting records across a week
    # would count that write seven times.
    since_text = _audit_bound(since)
    if since_text is not None:
        clauses.append("a.at >= ?")
        params.append(since_text)
    until_text = _audit_bound(until)
    if until_text is not None:
        clauses.append("a.at < ?")
        params.append(until_text)

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


def _row_to_forge_account(row: sqlite3.Row) -> ForgeAccount:
    return ForgeAccount(
        actor_id=str(row["actor_id"]),
        host=str(row["host"]),
        login=str(row["login"]),
        scopes=str(row["scopes"]),
        created_at=from_iso(str(row["created_at"])),
        updated_at=from_iso(str(row["updated_at"])),
    )


def _row_to_token(row: sqlite3.Row) -> Token:
    def _at(column: str) -> datetime | None:
        value = row[column]
        return None if value is None else from_iso(str(value))

    return Token(
        id=str(row["id"]),
        actor_id=str(row["actor_id"]),
        actor_identity_ref=(
            None
            if row["actor_identity_ref"] is None
            else str(row["actor_identity_ref"])
        ),
        name=str(row["name"]),
        scopes=json.loads(str(row["scopes"])),
        created_at=from_iso(str(row["created_at"])),
        expires_at=_at("expires_at"),
        last_used_at=_at("last_used_at"),
        revoked_at=_at("revoked_at"),
        revoked_reason=(
            None if row["revoked_reason"] is None else str(row["revoked_reason"])
        ),
    )


def _row_to_auth_decision(row: sqlite3.Row) -> AuthDecision:
    return AuthDecision(
        id=str(row["id"]),
        at=from_iso(str(row["at"])),
        decision=row["decision"],
        reason_code=str(row["reason_code"]),
        operation=str(row["operation"]),
        scope=None if row["scope"] is None else str(row["scope"]),
        actor_id=None if row["actor_id"] is None else str(row["actor_id"]),
        token_id=None if row["token_id"] is None else str(row["token_id"]),
        identity_ref=(
            None if row["identity_ref"] is None else str(row["identity_ref"])
        ),
        transport=str(row["transport"]),
        detail=None if row["detail"] is None else str(row["detail"]),
    )


def _row_to_writeback(row: sqlite3.Row) -> WriteBackRecord:
    return WriteBackRecord(
        id=str(row["id"]),
        at=from_iso(str(row["at"])),
        project_id=None if row["project_id"] is None else str(row["project_id"]),
        work_item_id=(
            None if row["work_item_id"] is None else str(row["work_item_id"])
        ),
        actor_id=str(row["actor_id"]),
        action=row["action"],
        subject_key=None if row["subject_key"] is None else str(row["subject_key"]),
        policy=str(row["policy"]),
        outcome=row["outcome"],
        reason=str(row["reason"]),
        detail=None if row["detail"] is None else str(row["detail"]),
        source_url=None if row["source_url"] is None else str(row["source_url"]),
    )


def _row_to_session(row: sqlite3.Row) -> CodingSession:
    stopped = row["stopped_at"]
    return CodingSession(
        id=str(row["id"]),
        engine_session_id=str(row["engine_session_id"]),
        project_id=str(row["project_id"]),
        work_item_id=(
            None if row["work_item_id"] is None else str(row["work_item_id"])
        ),
        actor_id=str(row["actor_id"]),
        cwd=str(row["cwd"]),
        template=None if row["template"] is None else str(row["template"]),
        model=None if row["model"] is None else str(row["model"]),
        effort=None if row["effort"] is None else str(row["effort"]),
        reason=str(row["reason"]),
        started_at=from_iso(str(row["started_at"])),
        stopped_at=None if stopped is None else from_iso(str(stopped)),
    )


def _row_to_drift(row: sqlite3.Row) -> DriftProposal:
    resolved = row["resolved_at"]
    return DriftProposal(
        id=str(row["id"]),
        kind=str(row["kind"]),
        subject_kind=str(row["subject_kind"]),
        subject_id=str(row["subject_id"]),
        project_id=None if row["project_id"] is None else str(row["project_id"]),
        project_slug=(
            None if row["project_slug"] is None else str(row["project_slug"])
        ),
        summary=str(row["summary"]),
        evidence_observation_id=(
            None
            if row["evidence_observation_id"] is None
            else str(row["evidence_observation_id"])
        ),
        evidence_snapshot=json.loads(str(row["evidence_snapshot"])),
        proposed_change=json.loads(str(row["proposed_change"])),
        status=row["status"],
        opened_at=from_iso(str(row["opened_at"])),
        superseded_at=(
            None
            if row["superseded_at"] is None
            else from_iso(str(row["superseded_at"]))
        ),
        superseded_detail=(
            None if row["superseded_detail"] is None else str(row["superseded_detail"])
        ),
        resolved_by_actor_id=(
            None
            if row["resolved_by_actor_id"] is None
            else str(row["resolved_by_actor_id"])
        ),
        resolved_by_identity_ref=(
            None if row["resolved_by"] is None else str(row["resolved_by"])
        ),
        resolved_at=None if resolved is None else from_iso(str(resolved)),
        resolution_reason=(
            None if row["resolution_reason"] is None else str(row["resolution_reason"])
        ),
    )


def _row_to_inbox_triage(row: sqlite3.Row) -> InboxTriage:
    snooze_until = row["snooze_until"]
    return InboxTriage(
        entry_key=str(row["entry_key"]),
        state=row["state"],
        snooze_until=(None if snooze_until is None else from_iso(str(snooze_until))),
        actor_id=str(row["actor_id"]),
        actor_identity_ref=str(row["actor_identity_ref"]),
        decided_at=from_iso(str(row["decided_at"])),
        occurrence_snapshot=json.loads(str(row["occurrence_snapshot"])),
    )


def _row_to_suppression(row: sqlite3.Row) -> Suppression:
    revoked = row["revoked_at"]
    return Suppression(
        id=str(row["id"]),
        match_kind=row["match_kind"],
        subject_key_or_pattern=str(row["subject_key_or_pattern"]),
        scope_project_id=(
            None if row["scope_project_id"] is None else str(row["scope_project_id"])
        ),
        scope_project_slug=(
            None
            if row["scope_project_slug"] is None
            else str(row["scope_project_slug"])
        ),
        actor_id=str(row["actor_id"]),
        actor_identity_ref=str(row["actor_identity_ref"]),
        reason=str(row["reason"]),
        created_at=from_iso(str(row["created_at"])),
        revoked_at=None if revoked is None else from_iso(str(revoked)),
        revoked_reason=(
            None if row["revoked_reason"] is None else str(row["revoked_reason"])
        ),
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


def _read_revision(conn: sqlite3.Connection) -> int:
    current = _meta_get(conn, META_REVISION)
    return 0 if current is None else int(current)


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
