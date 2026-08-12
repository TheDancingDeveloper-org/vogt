"""Forward-only migrations, under a lock (NFR-I3).

Three properties this buys, all of which are cheaper to have from the first
commit than to retrofit:

1. **Forward-only.** Every applied migration's checksum is verified on every
   run. Editing a migration that has already been applied somewhere is a
   loud failure, not a silent divergence between two databases.
2. **Locked.** One writer at a time, so two processes starting together
   cannot both apply `0002`. A lock older than `stale_after` is stolen,
   because a crashed process must not wedge the instance forever.
3. **Gated.** Readiness waits on this completing (`DEPLOYMENT.md` §5).
"""

from __future__ import annotations

import hashlib
import sqlite3
from dataclasses import dataclass
from datetime import datetime, timedelta
from pathlib import Path

from vogt.core.clock import from_iso, to_iso
from vogt.errors import MigrationError, MigrationLocked
from vogt.storage.interface import MigrationReport
from vogt.storage.sqlite.connection import split_statements

DEFAULT_STALE_AFTER = timedelta(minutes=15)

_FRAMEWORK_STATEMENTS = (
    """
    CREATE TABLE IF NOT EXISTS migrations (
        id         TEXT PRIMARY KEY NOT NULL,
        applied_at TEXT NOT NULL,
        checksum   TEXT NOT NULL
    )
    """,
    """
    CREATE TABLE IF NOT EXISTS migration_lock (
        id          INTEGER PRIMARY KEY NOT NULL CHECK (id = 1),
        holder      TEXT,
        acquired_at TEXT
    )
    """,
    """
    INSERT INTO migration_lock (id, holder, acquired_at)
    SELECT 1, NULL, NULL
    WHERE NOT EXISTS (SELECT 1 FROM migration_lock WHERE id = 1)
    """,
)


@dataclass(frozen=True)
class Migration:
    """One numbered migration file."""

    id: str
    sql: str
    checksum: str

    @property
    def number(self) -> int:
        return int(self.id.split("_", 1)[0])


def checksum_of(sql: str) -> str:
    """Checksum a migration body, ignoring trailing-newline noise."""
    return hashlib.sha256(sql.strip().encode("utf-8")).hexdigest()


def load_migrations(directory: Path) -> list[Migration]:
    """Load `NNNN_name.sql` files from a directory, in order."""
    migrations: list[Migration] = []
    for path in sorted(directory.glob("*.sql")):
        sql = path.read_text(encoding="utf-8")
        migrations.append(Migration(id=path.stem, sql=sql, checksum=checksum_of(sql)))
    seen: set[int] = set()
    for migration in migrations:
        if migration.number in seen:
            msg = f"duplicate migration number in {directory}: {migration.id}"
            raise MigrationError(msg)
        seen.add(migration.number)
    return migrations


class Migrator:
    """Applies a directory of migrations to one SQLite database."""

    def __init__(
        self,
        *,
        store: str,
        directory: Path,
        holder: str,
        stale_after: timedelta = DEFAULT_STALE_AFTER,
    ) -> None:
        self._store = store
        self._directory = directory
        self._holder = holder
        self._stale_after = stale_after

    def applied_version(self, conn: sqlite3.Connection) -> int:
        """Highest applied migration number, or 0 on an empty database."""
        if not _table_exists(conn, "migrations"):
            return 0
        row = conn.execute("SELECT id FROM migrations ORDER BY id DESC").fetchone()
        return 0 if row is None else int(str(row["id"]).split("_", 1)[0])

    def migrate(self, conn: sqlite3.Connection, *, now: datetime) -> MigrationReport:
        """Bring the database forward to the newest migration on disk."""
        self._ensure_framework(conn)
        available = load_migrations(self._directory)
        self._acquire_lock(conn, now=now)
        try:
            applied = self._applied(conn)
            self._verify_forward_only(applied, available)
            pending = [m for m in available if m.id not in applied]
            for migration in pending:
                self._apply(conn, migration, now=now)
            return MigrationReport(
                store=self._store,
                applied=tuple(m.id for m in pending),
                version=self.applied_version(conn),
            )
        finally:
            self._release_lock(conn)

    def _ensure_framework(self, conn: sqlite3.Connection) -> None:
        conn.execute("BEGIN IMMEDIATE")
        try:
            for statement in _FRAMEWORK_STATEMENTS:
                conn.execute(statement)
            conn.execute("COMMIT")
        except Exception:
            conn.execute("ROLLBACK")
            raise

    def _applied(self, conn: sqlite3.Connection) -> dict[str, str]:
        rows = conn.execute("SELECT id, checksum FROM migrations").fetchall()
        return {str(row["id"]): str(row["checksum"]) for row in rows}

    def _verify_forward_only(
        self, applied: dict[str, str], available: list[Migration]
    ) -> None:
        by_id = {m.id: m for m in available}
        for migration_id, checksum in sorted(applied.items()):
            known = by_id.get(migration_id)
            if known is None:
                msg = (
                    f"{self._store}: migration {migration_id} is applied in the "
                    "database but absent from this build — the database is ahead "
                    "of the code. Migrations are forward-only; restore a backup "
                    "or deploy the newer build."
                )
                raise MigrationError(msg)
            if known.checksum != checksum:
                msg = (
                    f"{self._store}: migration {migration_id} was modified after "
                    "being applied. Migrations are forward-only — add a new "
                    "migration instead of editing an applied one."
                )
                raise MigrationError(msg)

    def _apply(
        self, conn: sqlite3.Connection, migration: Migration, *, now: datetime
    ) -> None:
        conn.execute("BEGIN IMMEDIATE")
        try:
            for statement in split_statements(migration.sql):
                conn.execute(statement)
            conn.execute(
                "INSERT INTO migrations (id, applied_at, checksum) VALUES (?, ?, ?)",
                (migration.id, to_iso(now), migration.checksum),
            )
            conn.execute("COMMIT")
        except Exception as exc:
            conn.execute("ROLLBACK")
            msg = f"{self._store}: migration {migration.id} failed: {exc}"
            raise MigrationError(msg) from exc

    def _acquire_lock(self, conn: sqlite3.Connection, *, now: datetime) -> None:
        conn.execute("BEGIN IMMEDIATE")
        try:
            row = conn.execute(
                "SELECT holder, acquired_at FROM migration_lock WHERE id = 1"
            ).fetchone()
            holder = None if row is None else row["holder"]
            if holder is not None:
                acquired_at = from_iso(str(row["acquired_at"]))
                if now - acquired_at < self._stale_after:
                    conn.execute("ROLLBACK")
                    msg = (
                        f"{self._store}: migration lock held by {holder} since "
                        f"{to_iso(acquired_at)}"
                    )
                    raise MigrationLocked(msg)
            conn.execute(
                "UPDATE migration_lock SET holder = ?, acquired_at = ? WHERE id = 1",
                (self._holder, to_iso(now)),
            )
            conn.execute("COMMIT")
        except MigrationLocked:
            raise
        except Exception:
            conn.execute("ROLLBACK")
            raise

    def _release_lock(self, conn: sqlite3.Connection) -> None:
        conn.execute("BEGIN IMMEDIATE")
        try:
            conn.execute(
                "UPDATE migration_lock SET holder = NULL, acquired_at = NULL "
                "WHERE id = 1 AND holder = ?",
                (self._holder,),
            )
            conn.execute("COMMIT")
        except Exception:  # pragma: no cover - release must not mask the cause
            conn.execute("ROLLBACK")
            raise


def _table_exists(conn: sqlite3.Connection, name: str) -> bool:
    row = conn.execute(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?",
        (name,),
    ).fetchone()
    return row is not None
