"""The observed store on SQLite.

Append-only evidence (`SCHEMA.md` §3). At M0 it holds metadata only — the
`sweeps`/`observations` tables land at M2 with the collectors that write
them. It exists now so that two stores, two migration ledgers and two
backup boundaries are normal from the first commit.
"""

from __future__ import annotations

import os
import socket
import sqlite3
from contextlib import suppress
from datetime import timedelta
from pathlib import Path

from vogt.core.clock import Clock, to_iso, utc_now
from vogt.storage.interface import MigrationReport
from vogt.storage.sqlite.connection import connect
from vogt.storage.sqlite.migrator import DEFAULT_STALE_AFTER, Migrator

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
        lock_stale_after: timedelta = DEFAULT_STALE_AFTER,
    ) -> None:
        self._path = path
        self._clock = clock
        self._migrator = Migrator(
            store="observed",
            directory=MIGRATIONS_DIR,
            holder=f"{socket.gethostname()}/{os.getpid()}",
            stale_after=lock_stale_after,
        )

    @property
    def path(self) -> Path:
        return self._path

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
        conn = connect(self._path, create=True)
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
        conn = connect(self._path, create=False)
        try:
            row = conn.execute(
                "SELECT value FROM meta WHERE key = ?", (META_INSTANCE_ID,)
            ).fetchone()
            return None if row is None else str(row["value"])
        except sqlite3.OperationalError:  # pragma: no cover - unmigrated file
            return None
        finally:
            conn.close()
