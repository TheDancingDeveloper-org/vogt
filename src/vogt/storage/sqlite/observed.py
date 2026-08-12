"""The observed store on SQLite.

Append-only evidence (`SCHEMA.md` §3). Two disciplines live here and must not
be confused: `sweeps` and `observations` are immutable history that only
retention may delete, while `latest_*` are projections rebuilt from that
history and droppable at any time (NFR-I4).

Nothing in this module knows what a project *is*. Resolving a dependency
reference to a registered project is a cross-store question, so the
application layer answers it and hands the result down (`SCHEMA.md` §1).
"""

from __future__ import annotations

import json
import os
import socket
import sqlite3
from contextlib import suppress
from datetime import datetime, timedelta
from pathlib import Path

from vogt.core.clock import Clock, from_iso, to_iso, utc_now
from vogt.core.entities import DepRef, Observation, Sweep, SweepOutcome
from vogt.core.ids import IdFactory, new_id
from vogt.storage.interface import MigrationReport
from vogt.storage.observed_types import (
    AppendStats,
    DepRefRow,
    PendingObservation,
    PruneReport,
)
from vogt.storage.sqlite.connection import DEFAULT_SYNCHRONOUS, connect
from vogt.storage.sqlite.migrator import DEFAULT_STALE_AFTER, Migrator, table_exists

MIGRATIONS_DIR = Path(__file__).parent / "migrations" / "observed"

META_INSTANCE_ID = "instance_id"
META_CREATED_AT = "created_at"


class SqliteObservedStore:
    """Append-only evidence store."""

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
            store="observed",
            directory=MIGRATIONS_DIR,
            holder=f"{socket.gethostname()}/{os.getpid()}",
            stale_after=lock_stale_after,
        )

    @property
    def path(self) -> Path:
        return self._path

    # -- lifecycle ---------------------------------------------------------

    def migrate(self) -> MigrationReport:
        conn = connect(self._path, create=True, synchronous=self._synchronous)
        try:
            return self._migrator.migrate(conn, now=self._clock())
        finally:
            conn.close()

    def schema_version(self) -> int:
        if not self._path.exists():
            return 0
        conn = connect(self._path, create=False, synchronous=self._synchronous)
        try:
            return self._migrator.applied_version(conn)
        finally:
            conn.close()

    def is_initialized(self) -> bool:
        if not self._path.exists():
            return False
        conn = connect(self._path, create=False, synchronous=self._synchronous)
        try:
            row = conn.execute(
                "SELECT value FROM meta WHERE key = ?", (META_INSTANCE_ID,)
            ).fetchone()
            return row is not None
        except sqlite3.OperationalError:
            return False
        finally:
            conn.close()

    def bind_instance(self, instance_id: str) -> None:
        """Stamp this store with the instance it belongs to.

        The two stores are backed up and restored independently, so each
        carries the instance id; a restore that pairs mismatched files is
        then a detectable error rather than a silent one (FR-L2, at M4).
        """
        conn = connect(self._path, create=True, synchronous=self._synchronous)
        try:
            conn.execute("BEGIN IMMEDIATE")
            conn.execute(
                "INSERT INTO meta (key, value) VALUES (?, ?)",
                (META_INSTANCE_ID, instance_id),
            )
            conn.execute(
                "INSERT INTO meta (key, value) VALUES (?, ?)",
                (META_CREATED_AT, to_iso(self._clock())),
            )
            conn.execute("COMMIT")
        except BaseException:
            with suppress(sqlite3.OperationalError):  # pragma: no cover
                conn.execute("ROLLBACK")
            raise
        finally:
            conn.close()

    def instance_id(self) -> str | None:
        if not self._path.exists():
            return None
        conn = connect(self._path, create=False, synchronous=self._synchronous)
        try:
            row = conn.execute(
                "SELECT value FROM meta WHERE key = ?", (META_INSTANCE_ID,)
            ).fetchone()
            return None if row is None else str(row["value"])
        except sqlite3.OperationalError:  # pragma: no cover - unmigrated file
            return None
        finally:
            conn.close()

    # -- sweeps ------------------------------------------------------------

    def begin_sweep(self, *, collector: str, scope: list[str], at: datetime) -> Sweep:
        """Open a coverage record before any evidence is collected.

        Written up front, and left `running` if the process dies, so a sweep
        that never finished is visible as such rather than absent — which is
        the difference between "we looked and found nothing" and "we crashed".
        """
        sweep = Sweep(
            id=self._id_factory("swp"),
            collector=collector,
            scope=scope,
            started_at=at,
            outcome="running",
        )
        with self._write() as conn:
            conn.execute(
                "INSERT INTO sweeps (id, collector, scope, started_at, outcome, "
                "stats) VALUES (?, ?, ?, ?, ?, ?)",
                (
                    sweep.id,
                    collector,
                    json.dumps(scope),
                    to_iso(at),
                    "running",
                    "{}",
                ),
            )
        return sweep

    def finish_sweep(
        self,
        sweep_id: str,
        *,
        outcome: SweepOutcome,
        stats: dict[str, int],
        at: datetime,
        detail: str | None = None,
    ) -> None:
        with self._write() as conn:
            conn.execute(
                "UPDATE sweeps SET finished_at = ?, outcome = ?, stats = ?, "
                "detail = ? WHERE id = ?",
                (to_iso(at), outcome, json.dumps(stats), detail, sweep_id),
            )

    def append(
        self, sweep_id: str, findings: list[PendingObservation], *, at: datetime
    ) -> AppendStats:
        """Append findings, skipping any whose subject has not changed (FR-O7).

        Dedup is by (subject_key, content_digest) against the newest row for
        that subject. Growth therefore tracks change, not polling frequency
        (NFR-S2) — and a stable subject's newest row can be much older than
        the history window, which retention has to respect.
        """
        new = 0
        unchanged = 0
        with self._write() as conn:
            collector = _collector_of(conn, sweep_id)
            for finding in findings:
                current = conn.execute(
                    "SELECT content_digest FROM observations WHERE subject_key = ? "
                    "ORDER BY observed_at DESC, id DESC LIMIT 1",
                    (finding.subject_key,),
                ).fetchone()
                if current is not None and str(current["content_digest"]) == (
                    finding.content_digest
                ):
                    unchanged += 1
                    continue
                conn.execute(
                    "INSERT INTO observations (id, sweep_id, collector, kind, "
                    "project_id, subject_key, payload, content_digest, source_url, "
                    "promoted, observed_at) "
                    "VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                    (
                        self._id_factory("obs"),
                        sweep_id,
                        collector,
                        finding.kind,
                        finding.project_id,
                        finding.subject_key,
                        json.dumps(finding.payload, default=str, sort_keys=True),
                        finding.content_digest,
                        finding.source_url,
                        int(finding.promoted),
                        to_iso(at),
                    ),
                )
                new += 1
        return AppendStats(new=new, unchanged=unchanged)

    def list_sweeps(
        self, *, collector: str | None = None, limit: int = 50
    ) -> list[Sweep]:
        clause = "WHERE collector = ?" if collector else ""
        params: tuple[object, ...] = (collector, limit) if collector else (limit,)
        with self._read() as conn:
            rows = conn.execute(
                f"SELECT * FROM sweeps {clause} "
                "ORDER BY started_at DESC, id DESC LIMIT ?",
                params,
            ).fetchall()
        return [_row_to_sweep(row) for row in rows]

    def coverage(self) -> dict[str, Sweep]:
        """The newest *completed* sweep per collector.

        Completed, because a running sweep says nothing about coverage yet,
        and freshness computed from one would claim an answer is newer than
        the evidence behind it.
        """
        with self._read() as conn:
            rows = conn.execute(
                "SELECT * FROM sweeps WHERE finished_at IS NOT NULL "
                "ORDER BY collector, finished_at DESC, id DESC"
            ).fetchall()
        newest: dict[str, Sweep] = {}
        for row in rows:
            sweep = _row_to_sweep(row)
            newest.setdefault(sweep.collector, sweep)
        return newest

    # -- reads -------------------------------------------------------------

    def list_observations(
        self,
        *,
        kind: str | None = None,
        project_id: str | None = None,
        subject_key: str | None = None,
        limit: int = 100,
        offset: int = 0,
    ) -> list[Observation]:
        clauses: list[str] = []
        params: list[object] = []
        for column, value in (
            ("kind", kind),
            ("project_id", project_id),
            ("subject_key", subject_key),
        ):
            if value is not None:
                clauses.append(f"{column} = ?")
                params.append(value)
        where = f"WHERE {' AND '.join(clauses)}" if clauses else ""
        params += [limit, offset]
        with self._read() as conn:
            rows = conn.execute(
                f"SELECT * FROM observations {where} "
                "ORDER BY observed_at DESC, id DESC LIMIT ? OFFSET ?",
                tuple(params),
            ).fetchall()
        return [_row_to_observation(row) for row in rows]

    def latest(
        self,
        *,
        kinds: tuple[str, ...] = (),
        project_id: str | None = None,
        promoted_only: bool = False,
        limit: int = 1000,
    ) -> list[Observation]:
        """The newest observation per subject, filtered."""
        clauses: list[str] = []
        params: list[object] = []
        if kinds:
            placeholders = ", ".join("?" for _ in kinds)
            clauses.append(f"kind IN ({placeholders})")
            params.extend(kinds)
        if project_id is not None:
            clauses.append("project_id = ?")
            params.append(project_id)
        if promoted_only:
            clauses.append("promoted = 1")
        where = f"WHERE {' AND '.join(clauses)}" if clauses else ""
        params.append(limit)
        with self._read() as conn:
            rows = conn.execute(
                "SELECT subject_key, observation_id AS id, observation_id, collector, "
                "kind, project_id, payload, content_digest, source_url, promoted, "
                f"observed_at, '' AS sweep_id FROM latest_observations {where} "
                "ORDER BY observed_at DESC, subject_key LIMIT ?",
                tuple(params),
            ).fetchall()
        return [_row_to_observation(row) for row in rows]

    def dep_refs(
        self, *, from_project_id: str | None = None, to_project_id: str | None = None
    ) -> list[DepRef]:
        clauses: list[str] = []
        params: list[object] = []
        if from_project_id is not None:
            clauses.append("from_project_id = ?")
            params.append(from_project_id)
        if to_project_id is not None:
            clauses.append("to_project_id = ?")
            params.append(to_project_id)
        where = f"WHERE {' AND '.join(clauses)}" if clauses else ""
        with self._read() as conn:
            rows = conn.execute(
                f"SELECT * FROM latest_dep_refs {where} ORDER BY subject_key",
                tuple(params),
            ).fetchall()
        return [_row_to_dep_ref(row) for row in rows]

    def counts(self) -> dict[str, int]:
        with self._read() as conn:
            return {
                "sweeps": _count(conn, "sweeps"),
                "observations": _count(conn, "observations"),
                "subjects": _count(conn, "latest_observations"),
                "dep_refs": _count(conn, "latest_dep_refs"),
            }

    # -- projections -------------------------------------------------------

    def rebuild_latest(self) -> int:
        """Rebuild `latest_observations` from history (NFR-I4).

        Transactional and total: the projection is dropped and recomputed
        rather than patched, so it cannot drift from the evidence in ways
        nobody notices. It is bounded by the retention horizon, which is why
        retention always keeps the newest row per subject.
        """
        with self._write() as conn:
            conn.execute("DELETE FROM latest_observations")
            conn.execute(
                "INSERT INTO latest_observations (subject_key, observation_id, "
                "collector, kind, project_id, payload, content_digest, source_url, "
                "promoted, observed_at) "
                "SELECT o.subject_key, o.id, o.collector, o.kind, o.project_id, "
                "o.payload, o.content_digest, o.source_url, o.promoted, o.observed_at "
                "FROM observations o WHERE o.id = ("
                "  SELECT id FROM observations x WHERE x.subject_key = o.subject_key "
                "  ORDER BY x.observed_at DESC, x.id DESC LIMIT 1"
                ")"
            )
            row = conn.execute(
                "SELECT COUNT(*) AS n FROM latest_observations"
            ).fetchone()
            return int(row["n"])

    def replace_dep_refs(self, rows: list[DepRefRow]) -> int:
        """Replace the dependency projection with resolved rows."""
        with self._write() as conn:
            conn.execute("DELETE FROM latest_dep_refs")
            for row in rows:
                conn.execute(
                    "INSERT INTO latest_dep_refs (subject_key, from_project_id, "
                    "ref_kind, raw_target, manifest, to_project_id, observed_at) "
                    "VALUES (?, ?, ?, ?, ?, ?, ?)",
                    (
                        row.subject_key,
                        row.from_project_id,
                        row.ref_kind,
                        row.raw_target,
                        row.manifest,
                        row.to_project_id,
                        to_iso(row.observed_at),
                    ),
                )
        return len(rows)

    # -- retention ---------------------------------------------------------

    def prune(
        self,
        *,
        before: datetime,
        protected_observation_ids: frozenset[str] = frozenset(),
    ) -> PruneReport:
        """Prune history, in the precedence order NFR-I5 sets out.

        A row survives if any rule protects it:

        1. It is the newest observation for its subject. Digest dedup means a
           stable subject's newest row can be older than the window, so age
           alone must never prune it.
        2. A drift proposal references it (FR-R5) — passed in, because drift
           lives in the other store. Evidence must never become unreachable
           through retention.
        3. Otherwise it is history, and the window applies.
        """
        with self._write() as conn:
            newest = {
                str(row["id"])
                for row in conn.execute(
                    "SELECT id FROM observations o WHERE o.id = ("
                    "  SELECT id FROM observations x WHERE x.subject_key = "
                    "  o.subject_key ORDER BY x.observed_at DESC, x.id DESC LIMIT 1"
                    ")"
                ).fetchall()
            }
            candidates = [
                str(row["id"])
                for row in conn.execute(
                    "SELECT id FROM observations WHERE observed_at < ?",
                    (to_iso(before),),
                ).fetchall()
            ]
            doomed = [
                candidate
                for candidate in candidates
                if candidate not in newest
                and candidate not in protected_observation_ids
            ]
            for observation_id in doomed:
                conn.execute("DELETE FROM observations WHERE id = ?", (observation_id,))
            return PruneReport(
                removed=len(doomed),
                kept_latest=sum(1 for c in candidates if c in newest),
                kept_referenced=sum(
                    1
                    for c in candidates
                    if c not in newest and c in protected_observation_ids
                ),
            )

    def has_evidence_tables(self) -> bool:
        """Whether this store has been migrated to hold evidence yet."""
        if not self._path.exists():
            return False
        conn = connect(self._path, create=False, synchronous=self._synchronous)
        try:
            return table_exists(conn, "observations")
        finally:
            conn.close()

    # -- connections -------------------------------------------------------

    class _Ctx:
        def __init__(self, conn: sqlite3.Connection, *, write: bool) -> None:
            self._conn = conn
            self._write = write

        def __enter__(self) -> sqlite3.Connection:
            if self._write:
                self._conn.execute("BEGIN IMMEDIATE")
            return self._conn

        def __exit__(self, exc_type: object, exc: object, tb: object) -> None:
            try:
                if self._write:
                    if exc_type is None:
                        self._conn.execute("COMMIT")
                    else:
                        with suppress(sqlite3.OperationalError):
                            self._conn.execute("ROLLBACK")
            finally:
                self._conn.close()

    def _write(self) -> SqliteObservedStore._Ctx:
        return self._Ctx(
            connect(self._path, create=True, synchronous=self._synchronous), write=True
        )

    def _read(self) -> SqliteObservedStore._Ctx:
        return self._Ctx(
            connect(self._path, create=False, synchronous=self._synchronous),
            write=False,
        )


# -- row mapping -----------------------------------------------------------


def _row_to_sweep(row: sqlite3.Row) -> Sweep:
    finished = row["finished_at"]
    return Sweep(
        id=str(row["id"]),
        collector=str(row["collector"]),
        scope=json.loads(str(row["scope"])),
        started_at=from_iso(str(row["started_at"])),
        finished_at=None if finished is None else from_iso(str(finished)),
        outcome=row["outcome"],
        stats=json.loads(str(row["stats"])),
        detail=None if row["detail"] is None else str(row["detail"]),
    )


def _row_to_observation(row: sqlite3.Row) -> Observation:
    return Observation(
        id=str(row["id"]),
        sweep_id=str(row["sweep_id"]),
        collector=str(row["collector"]),
        kind=str(row["kind"]),
        project_id=None if row["project_id"] is None else str(row["project_id"]),
        subject_key=str(row["subject_key"]),
        payload=json.loads(str(row["payload"])),
        content_digest=str(row["content_digest"]),
        source_url=None if row["source_url"] is None else str(row["source_url"]),
        promoted=bool(row["promoted"]),
        observed_at=from_iso(str(row["observed_at"])),
    )


def _row_to_dep_ref(row: sqlite3.Row) -> DepRef:
    return DepRef(
        subject_key=str(row["subject_key"]),
        from_project_id=str(row["from_project_id"]),
        ref_kind=row["ref_kind"],
        raw_target=str(row["raw_target"]),
        manifest=None if row["manifest"] is None else str(row["manifest"]),
        to_project_id=(
            None if row["to_project_id"] is None else str(row["to_project_id"])
        ),
        observed_at=from_iso(str(row["observed_at"])),
    )


def _collector_of(conn: sqlite3.Connection, sweep_id: str) -> str:
    row = conn.execute(
        "SELECT collector FROM sweeps WHERE id = ?", (sweep_id,)
    ).fetchone()
    return "" if row is None else str(row["collector"])


def _count(conn: sqlite3.Connection, table: str) -> int:
    # `table` is never caller-supplied: every call site passes a literal.
    row = conn.execute(f"SELECT COUNT(*) AS n FROM {table}").fetchone()
    return int(row["n"])
