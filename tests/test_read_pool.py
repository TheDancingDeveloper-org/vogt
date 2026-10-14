"""Pooled read connections (WI-1224).

A read used to open a fresh connection, and one badge read opened ~130 of
them. A pooled connection must still behave exactly like a fresh one: lent to
one borrower at a time, never inside a transaction, seeing other connections'
commits, and never outliving the database file it was opened on.
"""

from __future__ import annotations

import shutil
import sqlite3
import threading
from collections.abc import Iterator
from pathlib import Path

import pytest

from vogt.storage.sqlite import connection
from vogt.storage.sqlite.connection import borrow_read, connect, drain_read_pool


@pytest.fixture
def db(tmp_path: Path) -> Iterator[Path]:
    path = tmp_path / "pool.sqlite3"
    conn = connect(path, create=True)
    conn.execute("CREATE TABLE t (v INTEGER)")
    conn.execute("INSERT INTO t VALUES (1)")
    conn.close()
    yield path
    drain_read_pool()


def _write(path: Path, value: int) -> None:
    conn = connect(path, create=False)
    try:
        conn.execute("UPDATE t SET v = ?", (value,))
    finally:
        conn.close()


def _read(conn: sqlite3.Connection) -> int:
    return int(conn.execute("SELECT v FROM t").fetchone()["v"])


def test_a_returned_connection_is_lent_again(db: Path) -> None:
    with borrow_read(db) as first:
        pass
    with borrow_read(db) as second:
        assert second is first


def test_a_pooled_connection_sees_another_connections_commit(db: Path) -> None:
    with borrow_read(db) as conn:
        assert _read(conn) == 1
    _write(db, 2)
    with borrow_read(db) as again:
        assert again is conn
        assert _read(again) == 2


def test_concurrent_borrowers_get_distinct_connections(db: Path) -> None:
    with borrow_read(db) as one, borrow_read(db) as two:
        assert one is not two


def test_a_connection_returned_inside_a_transaction_is_closed(db: Path) -> None:
    with borrow_read(db) as conn:
        conn.execute("BEGIN")
    with pytest.raises(sqlite3.ProgrammingError):
        conn.execute("SELECT 1")
    with borrow_read(db) as fresh:
        assert fresh is not conn
        assert not fresh.in_transaction


def test_a_connection_returned_by_an_error_is_closed(db: Path) -> None:
    with pytest.raises(RuntimeError), borrow_read(db) as conn:
        raise RuntimeError("boom")
    with pytest.raises(sqlite3.ProgrammingError):
        conn.execute("SELECT 1")


def test_a_drain_closes_idle_and_lent_connections(db: Path) -> None:
    with borrow_read(db) as idle:
        pass
    with borrow_read(db) as lent:
        assert lent is idle
        drain_read_pool(db)
    with pytest.raises(sqlite3.ProgrammingError):
        lent.execute("SELECT 1")
    with borrow_read(db) as fresh:
        assert fresh is not lent


def test_a_file_renamed_over_the_path_is_a_different_database(
    db: Path, tmp_path: Path
) -> None:
    """A clone swaps a new file in by rename; the old connections must not be
    lent for it."""
    with borrow_read(db) as old:
        assert _read(old) == 1
    replacement = tmp_path / "replacement.sqlite3"
    conn = connect(replacement, create=True)
    conn.execute("CREATE TABLE t (v INTEGER)")
    conn.execute("INSERT INTO t VALUES (7)")
    conn.close()
    replacement.replace(db)
    with borrow_read(db) as new:
        assert new is not old
        assert _read(new) == 7


def test_a_restore_in_place_is_a_different_database(db: Path, tmp_path: Path) -> None:
    """A restore copies over the file in place (same inode) and deletes the
    WAL index beside it; the index's identity is what gives it away when the
    restore ran in another process and nothing drained this one."""
    backup = tmp_path / "backup.sqlite3"
    conn = connect(backup, create=True)
    conn.execute("CREATE TABLE t (v INTEGER)")
    conn.execute("INSERT INTO t VALUES (9)")
    conn.close()
    with borrow_read(db) as old:
        assert _read(old) == 1
    # What `restore` does, without the drain it does in its own process.
    shutil.copy2(backup, db)
    for suffix in ("-wal", "-shm"):
        stale = db.with_name(f"{db.name}{suffix}")
        if stale.exists():
            stale.unlink()
    with borrow_read(db) as new:
        assert new is not old
        assert _read(new) == 9


def test_only_a_few_files_keep_a_pool(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(connection, "READ_POOL_FILES_MAX", 2)
    lent: list[sqlite3.Connection] = []
    for index in range(3):
        path = tmp_path / f"db{index}.sqlite3"
        connect(path, create=True).close()
        with borrow_read(path) as conn:
            lent.append(conn)
    # The least recently used file's connection was closed, not kept open.
    with pytest.raises(sqlite3.ProgrammingError):
        lent[0].execute("SELECT 1")
    lent[2].execute("SELECT 1")
    drain_read_pool()


def test_a_missing_database_is_refused(tmp_path: Path) -> None:
    with (
        pytest.raises(FileNotFoundError),
        borrow_read(tmp_path / "absent.sqlite3"),
    ):
        pass


def test_pooled_connections_are_safe_across_threads(db: Path) -> None:
    errors: list[BaseException] = []

    def reader() -> None:
        try:
            for _ in range(50):
                with borrow_read(db) as conn:
                    conn.execute("BEGIN")
                    _read(conn)
                    conn.execute("ROLLBACK")
        except BaseException as exc:  # pragma: no cover - the failure itself
            errors.append(exc)

    threads = [threading.Thread(target=reader) for _ in range(8)]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join(10)
    assert errors == []


def test_the_observed_generation_moves_on_every_rebuild(tmp_path: Path) -> None:
    from datetime import UTC, datetime

    from vogt.storage.sqlite.observed import SqliteObservedStore

    store = SqliteObservedStore(
        tmp_path / "observed.sqlite3", clock=lambda: datetime.now(UTC)
    )
    assert store.latest_generation() == 0
    store.migrate()
    assert store.latest_generation() == 0
    store.rebuild_latest()
    store.rebuild_latest()
    assert store.latest_generation() == 2
