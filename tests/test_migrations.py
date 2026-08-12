"""Migrations are forward-only, locked, and honest about it (NFR-I3)."""

from __future__ import annotations

from datetime import UTC, datetime, timedelta
from pathlib import Path

import pytest

from vogt.errors import MigrationError, MigrationLocked
from vogt.storage.sqlite.connection import connect, split_statements
from vogt.storage.sqlite.declared import MIGRATIONS_DIR as DECLARED_MIGRATIONS
from vogt.storage.sqlite.declared import SqliteDeclaredStore
from vogt.storage.sqlite.migrator import Migrator, load_migrations
from vogt.storage.sqlite.observed import SqliteObservedStore

from tests.conftest import TEST_PRINCIPAL, StepClock

NOW = datetime(2026, 8, 12, 5, 0, 0, tzinfo=UTC)


def _migrator(directory: Path, *, holder: str = "test/1") -> Migrator:
    return Migrator(store="test", directory=directory, holder=holder)


def _write_migration(directory: Path, name: str, sql: str) -> Path:
    directory.mkdir(parents=True, exist_ok=True)
    path = directory / name
    path.write_text(sql, encoding="utf-8")
    return path


def test_applies_pending_migrations_in_order(tmp_path: Path) -> None:
    migrations = tmp_path / "migrations"
    _write_migration(migrations, "0001_a.sql", "CREATE TABLE a (id TEXT);")
    _write_migration(migrations, "0002_b.sql", "CREATE TABLE b (id TEXT);")
    conn = connect(tmp_path / "db.sqlite3", create=True)

    report = _migrator(migrations).migrate(conn, now=NOW)

    assert report.applied == ("0001_a", "0002_b")
    assert report.version == 2
    conn.close()


def test_migrating_twice_applies_nothing(tmp_path: Path) -> None:
    migrations = tmp_path / "migrations"
    _write_migration(migrations, "0001_a.sql", "CREATE TABLE a (id TEXT);")
    conn = connect(tmp_path / "db.sqlite3", create=True)
    migrator = _migrator(migrations)

    migrator.migrate(conn, now=NOW)
    second = migrator.migrate(conn, now=NOW)

    assert second.applied == ()
    assert second.version == 1
    conn.close()


def test_editing_an_applied_migration_fails_loudly(tmp_path: Path) -> None:
    migrations = tmp_path / "migrations"
    path = _write_migration(migrations, "0001_a.sql", "CREATE TABLE a (id TEXT);")
    conn = connect(tmp_path / "db.sqlite3", create=True)
    _migrator(migrations).migrate(conn, now=NOW)

    path.write_text("CREATE TABLE a (id TEXT, extra TEXT);", encoding="utf-8")

    with pytest.raises(MigrationError, match="forward-only"):
        _migrator(migrations).migrate(conn, now=NOW)
    conn.close()


def test_a_database_ahead_of_the_code_fails_loudly(tmp_path: Path) -> None:
    migrations = tmp_path / "migrations"
    _write_migration(migrations, "0001_a.sql", "CREATE TABLE a (id TEXT);")
    newer = _write_migration(migrations, "0002_b.sql", "CREATE TABLE b (id TEXT);")
    conn = connect(tmp_path / "db.sqlite3", create=True)
    _migrator(migrations).migrate(conn, now=NOW)

    newer.unlink()

    with pytest.raises(MigrationError, match="ahead of the code"):
        _migrator(migrations).migrate(conn, now=NOW)
    conn.close()


def test_a_failing_migration_rolls_back_and_is_not_recorded(tmp_path: Path) -> None:
    migrations = tmp_path / "migrations"
    _write_migration(
        migrations, "0001_a.sql", "CREATE TABLE a (id TEXT);\nCREATE TABLE a (x TEXT);"
    )
    conn = connect(tmp_path / "db.sqlite3", create=True)

    with pytest.raises(MigrationError, match="0001_a failed"):
        _migrator(migrations).migrate(conn, now=NOW)

    tables = {
        row["name"]
        for row in conn.execute("SELECT name FROM sqlite_master WHERE type='table'")
    }
    assert "a" not in tables
    assert conn.execute("SELECT COUNT(*) AS n FROM migrations").fetchone()["n"] == 0
    conn.close()


def test_a_held_lock_blocks_a_second_migrator(tmp_path: Path) -> None:
    migrations = tmp_path / "migrations"
    _write_migration(migrations, "0001_a.sql", "CREATE TABLE a (id TEXT);")
    conn = connect(tmp_path / "db.sqlite3", create=True)
    holder = _migrator(migrations, holder="holder/1")
    holder._ensure_framework(conn)
    holder._acquire_lock(conn, now=NOW)

    with pytest.raises(MigrationLocked, match="holder/1"):
        _migrator(migrations, holder="other/2").migrate(conn, now=NOW)
    conn.close()


def test_a_stale_lock_is_stolen(tmp_path: Path) -> None:
    """A crashed process must not wedge the instance forever."""
    migrations = tmp_path / "migrations"
    _write_migration(migrations, "0001_a.sql", "CREATE TABLE a (id TEXT);")
    conn = connect(tmp_path / "db.sqlite3", create=True)
    dead = _migrator(migrations, holder="dead/1")
    dead._ensure_framework(conn)
    dead._acquire_lock(conn, now=NOW)

    report = _migrator(migrations, holder="live/2").migrate(
        conn, now=NOW + timedelta(hours=1)
    )

    assert report.applied == ("0001_a",)
    conn.close()


def test_duplicate_migration_numbers_are_rejected(tmp_path: Path) -> None:
    migrations = tmp_path / "migrations"
    _write_migration(migrations, "0001_a.sql", "CREATE TABLE a (id TEXT);")
    _write_migration(migrations, "0001_b.sql", "CREATE TABLE b (id TEXT);")

    with pytest.raises(MigrationError, match="duplicate migration number"):
        load_migrations(migrations)


def test_split_statements_ignores_comments_and_strings() -> None:
    script = """
    -- a comment with a ; semicolon
    INSERT INTO t (v) VALUES ('a;b');
    CREATE TABLE u (id TEXT)
    """
    assert split_statements(script) == [
        "INSERT INTO t (v) VALUES ('a;b')",
        "CREATE TABLE u (id TEXT)",
    ]


def test_shipped_migrations_bring_both_stores_up(tmp_path: Path) -> None:
    declared = SqliteDeclaredStore(tmp_path / "declared.sqlite3")
    observed = SqliteObservedStore(tmp_path / "observed.sqlite3")

    assert declared.schema_version() == 0
    assert observed.schema_version() == 0

    # Deliberately not asserting a fixed list: this test is about the
    # framework applying whatever ships, and re-editing it on every migration
    # would train the next person to edit it without reading it.
    declared_report = declared.migrate()
    observed_report = observed.migrate()

    assert declared_report.applied[0] == "0001_foundation"
    assert declared_report.applied == tuple(sorted(declared_report.applied))
    assert observed_report.applied[0] == "0001_foundation"

    assert declared.schema_version() == len(declared_report.applied)
    assert observed.schema_version() == len(observed_report.applied)
    assert not declared.is_initialized()
    assert not observed.is_initialized()


def test_an_m0_instance_migrates_forward_with_its_data(tmp_path: Path) -> None:
    """An instance created before 0002 must survive gaining the work plane.

    Built by applying only migration 0001 from a copy of the shipped
    directory, so this exercises the real upgrade path rather than a
    hand-written approximation of an old schema.
    """
    old_migrations = tmp_path / "only-0001"
    old_migrations.mkdir()
    first = next(m for m in load_migrations(DECLARED_MIGRATIONS) if m.number == 1)
    (old_migrations / f"{first.id}.sql").write_text(first.sql, encoding="utf-8")

    path = tmp_path / "declared.sqlite3"
    conn = connect(path, create=True)
    Migrator(store="declared", directory=old_migrations, holder="old/1").migrate(
        conn, now=NOW
    )
    conn.close()

    store = SqliteDeclaredStore(path, clock=StepClock())
    bootstrapped = store.bootstrap(TEST_PRINCIPAL)

    report = store.migrate()
    assert "0002_work" in report.applied

    with store.read() as view:
        assert view.instance_id() == bootstrapped.instance_id
        assert view.counts().work_items == 0
        # No workflow_defs rows exist on this path; the defaults still answer.
        assert view.workflow_for("bug").initial_state == "open"
