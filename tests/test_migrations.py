"""Migrations are forward-only, locked, and honest about it (NFR-I3)."""

from __future__ import annotations

from datetime import UTC, datetime, timedelta
from pathlib import Path

import pytest

from vogt.core.entities import CodingSession, Project
from vogt.errors import MigrationError, MigrationLocked
from vogt.storage.sqlite.connection import connect, split_statements
from vogt.storage.sqlite.declared import MIGRATIONS_DIR as DECLARED_MIGRATIONS
from vogt.storage.sqlite.declared import SqliteDeclaredStore
from vogt.storage.sqlite.migrator import Migrator, load_migrations
from vogt.storage.sqlite.observed import MIGRATIONS_DIR as OBSERVED_MIGRATIONS
from vogt.storage.sqlite.observed import SqliteObservedStore

from tests.conftest import TEST_PRINCIPAL, StepClock

NOW = datetime(2026, 8, 12, 5, 0, 0, tzinfo=UTC)

#: The migration that added `coding_sessions`. Named rather than derived from
#: "the newest one", so the upgrade this test covers stays the upgrade it
#: covers after the next migration lands.
SESSIONS_MIGRATION = 7

# These are identities already delivered to instances, not an inventory that
# must equal the directory forever. New migrations append after this prefix
# without requiring an edit here. Renaming, removing, or reordering one of
# these entries is unsafe because a deployed database records the full id.
SHIPPED_DECLARED_MIGRATION_IDS = (
    "0001_foundation",
    "0002_work",
    "0003_observed_first",
    "0004_drift",
    "0005_tokens",
    "0006_writeback",
    "0007_sessions",
    "0008_superseded_drift",
    "0009_inbox_triage",
    "0010_session_model",
)
SHIPPED_OBSERVED_MIGRATION_IDS = (
    "0001_foundation",
    "0002_evidence",
    "0003_inherited_dep_refs",
)


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


@pytest.mark.parametrize(
    ("store", "directory", "shipped_ids"),
    [
        ("declared", DECLARED_MIGRATIONS, SHIPPED_DECLARED_MIGRATION_IDS),
        ("observed", OBSERVED_MIGRATIONS, SHIPPED_OBSERVED_MIGRATION_IDS),
    ],
)
def test_shipped_migration_ids_are_an_append_only_prefix(
    store: str, directory: Path, shipped_ids: tuple[str, ...]
) -> None:
    """#56: an applied migration identity may never move or disappear.

    The full filename stem is persisted in every instance. Comparing only the
    numeric schema version misses a rename, while exact equality would make a
    safe append fail until somebody edited this list. A prefix assertion pins
    what has shipped and permits only the forward operation.
    """
    available_ids = tuple(migration.id for migration in load_migrations(directory))

    for position, shipped_id in enumerate(shipped_ids):
        actual_id = (
            available_ids[position] if position < len(available_ids) else "<missing>"
        )
        assert actual_id == shipped_id, (
            f"{store} migration {shipped_id!r} shipped at position "
            f"{position + 1} but is now {actual_id!r}; applied migration ids "
            "are forward-only, so restore the shipped id and append a new "
            "migration instead"
        )


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


def test_a_pre_session_instance_migrates_forward_with_its_data(
    tmp_path: Path,
) -> None:
    """An instance created before 0007 gains sessions without losing anything.

    Built the same way as the M0 case below: a copy of the shipped directory
    with the newest migration withheld, so the upgrade path exercised here is
    the real one rather than a hand-written old schema.
    """
    shipped = load_migrations(DECLARED_MIGRATIONS)
    old_migrations = tmp_path / "before-sessions"
    old_migrations.mkdir()
    for migration in shipped:
        if migration.number < SESSIONS_MIGRATION:
            (old_migrations / f"{migration.id}.sql").write_text(
                migration.sql, encoding="utf-8"
            )

    path = tmp_path / "declared.sqlite3"
    conn = connect(path, create=True)
    Migrator(store="declared", directory=old_migrations, holder="old/1").migrate(
        conn, now=NOW
    )
    conn.close()

    store = SqliteDeclaredStore(path, clock=StepClock())
    store.bootstrap(TEST_PRINCIPAL)
    assert store.schema_version() == SESSIONS_MIGRATION - 1
    with store.write() as txn:
        actor = txn.actor_by_identity(TEST_PRINCIPAL.identity_ref)
        assert actor is not None
        actor_id = actor.id
        txn.insert_project(
            Project(
                id="prj_old",
                slug="already-here",
                name="Already Here",
                root_path="/srv/already-here",
                created_at=NOW,
                updated_at=NOW,
            )
        )

    report = store.migrate()
    assert "0007_sessions" in report.applied
    assert store.schema_version() == shipped[-1].number

    with store.write() as txn:
        txn.insert_session(
            CodingSession(
                id="ses_first",
                engine_session_id="eng-1",
                project_id="prj_old",
                actor_id=actor_id,
                cwd="/srv/already-here",
                reason="the first session on an upgraded instance",
                started_at=NOW,
            )
        )
    with store.read() as view:
        assert view.project_by_slug("already-here") is not None
        assert [s.id for s in view.list_sessions(limit=10, offset=0)] == ["ses_first"]


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
