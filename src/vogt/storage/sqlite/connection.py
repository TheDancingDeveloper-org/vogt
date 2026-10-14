"""Connection handling for the SQLite backend.

Write connections are opened per transaction and closed after it. That is
slightly wasteful and completely thread-safe, which is the right trade while
the same data directory is reachable from a CLI process, a server process and
a stdio MCP process at the same time (`DEPLOYMENT.md`).

Read connections are pooled per database file (`borrow_read`, WI-1224). A
read transaction opened on a fresh connection paid for the open, four pragmas
and a cold page cache every time, and one shell badge read opened ~130 of
them. A pooled connection is handed to one thread at a time, always outside a
transaction, and is still a plain SQLite connection: another process's
commits are visible to its next `BEGIN` exactly as they would be to a new one.
"""

from __future__ import annotations

import sqlite3
import threading
from collections import OrderedDict
from collections.abc import Iterator
from contextlib import contextmanager, suppress
from dataclasses import dataclass, field
from pathlib import Path

BUSY_TIMEOUT_MS = 5_000

#: Durability of a commit, as a WAL-mode `synchronous` setting.
#:
#: `NORMAL` rather than SQLite's default `FULL`, which is the conventional
#: pairing for WAL. Be clear about what it buys here, which is *not much*:
#: measured on a deployed data volume, a write costs 28.7ms
#: at `FULL` and 24.7ms at `NORMAL`, against 0.1ms with fsync off entirely.
#:
#: The reason the setting hardly matters is above this line — connections are
#: opened per transaction and closed, and closing the last connection to a WAL
#: database checkpoints it, which fsyncs whatever `synchronous` says about
#: commits. The ~25ms is the checkpoint, not the commit. Fixing that means
#: changing connection lifetime, not this pragma.
#:
#: What `NORMAL` does trade: a power loss or OS crash can lose the last few
#: committed transactions. The database is never corrupted, and an application
#: crash loses nothing, because the WAL is already handed to the kernel.
#: `FULL` is available for anyone who wants the stronger guarantee.
#:
#: The test suite sets `off`, where nothing outlives the run. That is where
#: this knob earns its keep today: it took the suite from 197s to 16s.
DEFAULT_SYNCHRONOUS = "normal"


def connect(
    path: Path,
    *,
    create: bool,
    synchronous: str = DEFAULT_SYNCHRONOUS,
    shared: bool = False,
) -> sqlite3.Connection:
    """Open a connection with Vogt's pragmas applied.

    `isolation_level=None` puts the driver in autocommit mode so that this
    package issues its own `BEGIN`/`COMMIT`. Transaction boundaries are a
    correctness concern here and are not left to the driver.

    `shared` lets the connection be used from a thread other than the one
    that opened it — one thread at a time, which `borrow_read` guarantees.
    """
    if not create and not path.exists():
        msg = f"no database at {path}"
        raise FileNotFoundError(msg)
    if create:
        path.parent.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(path, isolation_level=None, check_same_thread=not shared)
    conn.row_factory = sqlite3.Row
    conn.execute("PRAGMA foreign_keys = ON")
    conn.execute(f"PRAGMA busy_timeout = {BUSY_TIMEOUT_MS}")
    conn.execute("PRAGMA journal_mode = WAL")
    # Validated against a fixed set rather than interpolated blindly: this
    # lands in a PRAGMA, which takes no bound parameters.
    if synchronous.lower() not in ("off", "normal", "full", "extra"):
        msg = f"unknown synchronous setting: {synchronous!r}"
        raise ValueError(msg)
    conn.execute(f"PRAGMA synchronous = {synchronous.upper()}")
    return conn


#: Idle read connections kept per database file. Enough for a burst of
#: concurrent reads in one process; more only holds file descriptors.
READ_POOL_IDLE_MAX = 8

#: Database files with a pool at once, least recently used dropped first. A
#: server has two; a test suite creates thousands of throwaway instances, and
#: their idle connections must not pile up as open descriptors.
READ_POOL_FILES_MAX = 4

#: What identifies the database a connection was opened on: the database
#: file's and its WAL index's `(st_dev, st_ino)`. A clone renames a new file
#: over the path; a restore copies over it in place but deletes the `-shm`
#: beside it, so a new index is created by the next connection. Either way
#: the identity moves and the idle connections, opened on the database that
#: was there before, are closed rather than lent.
FileIdentity = tuple[int, int, int, int]


@dataclass
class _ReadPool:
    """Idle read connections to one database file, as it was when used."""

    identity: FileIdentity
    idle: list[sqlite3.Connection] = field(default_factory=list)


_pools: OrderedDict[tuple[str, str], _ReadPool] = OrderedDict()
_pools_lock = threading.Lock()
#: Moves on every drain. A connection lent before a drain is closed when it
#: comes back rather than pooled.
_drains = 0


def _file_identity(path: Path) -> FileIdentity:
    stat = path.stat()
    try:
        index = path.with_name(f"{path.name}-shm").stat()
    except FileNotFoundError:
        return (stat.st_dev, stat.st_ino, 0, 0)
    return (stat.st_dev, stat.st_ino, index.st_dev, index.st_ino)


def _close_all(conns: list[sqlite3.Connection]) -> None:
    for conn in conns:
        with suppress(sqlite3.Error):
            conn.close()


@contextmanager
def borrow_read(
    path: Path, *, synchronous: str = DEFAULT_SYNCHRONOUS
) -> Iterator[sqlite3.Connection]:
    """Lend a pooled connection for one read, outside any transaction.

    The caller opens and ends its own transaction. A connection returned
    still inside one, or after an error, is closed rather than pooled, so the
    next borrower always starts clean.
    """
    if not path.exists():
        msg = f"no database at {path}"
        raise FileNotFoundError(msg)
    key = (str(path), synchronous)
    conn: sqlite3.Connection | None = None
    stale: list[sqlite3.Connection] = []
    with _pools_lock:
        drains = _drains
        pool = _pools.get(key)
        if pool is not None and pool.idle:
            if pool.identity == _file_identity(path):
                conn = pool.idle.pop()
            else:
                stale, pool.idle = pool.idle, []
    _close_all(stale)
    if conn is None:
        conn = connect(path, create=False, synchronous=synchronous, shared=True)
    healthy = False
    try:
        yield conn
        healthy = not conn.in_transaction
    finally:
        _return(key, path, conn, healthy=healthy, drains=drains)


def _return(
    key: tuple[str, str],
    path: Path,
    conn: sqlite3.Connection,
    *,
    healthy: bool,
    drains: int,
) -> None:
    evicted: list[sqlite3.Connection] = []
    keep = False
    if healthy:
        try:
            # Taken after use, so the WAL index the connection created or
            # joined is part of it.
            identity = _file_identity(path)
        except FileNotFoundError:
            healthy = False
    if healthy:
        with _pools_lock:
            pool = _pools.get(key)
            if pool is None:
                pool = _pools[key] = _ReadPool(identity=identity)
            elif pool.identity != identity:
                evicted, pool.idle = pool.idle, []
                pool.identity = identity
            _pools.move_to_end(key)
            if drains == _drains and len(pool.idle) < READ_POOL_IDLE_MAX:
                pool.idle.append(conn)
                keep = True
            while len(_pools) > READ_POOL_FILES_MAX:
                _, dropped = _pools.popitem(last=False)
                evicted.extend(dropped.idle)
    if not keep:
        evicted.append(conn)
    _close_all(evicted)


def drain_read_pool(path: Path | None = None) -> None:
    """Close every idle pooled connection (to one file, or to all).

    Called before a restore or clone replaces a store's files, so no
    connection outlives the database it was opened on, and by tests. A
    connection lent out at the time is closed when it comes back.
    """
    global _drains
    with _pools_lock:
        _drains += 1
        if path is None:
            dropped = list(_pools.values())
            _pools.clear()
        else:
            dropped = [_pools.pop(key) for key in list(_pools) if key[0] == str(path)]
    for pool in dropped:
        _close_all(pool.idle)


def split_statements(script: str) -> list[str]:
    """Split a migration script into individual statements.

    Migrations are plain DDL: no triggers, no `BEGIN ... END` bodies, no
    semicolons inside identifiers. `executescript` cannot be used because it
    commits any open transaction first, and each migration must apply inside
    one.
    """
    statements: list[str] = []
    buffer: list[str] = []
    in_string = False
    index = 0
    while index < len(script):
        char = script[index]
        if in_string:
            buffer.append(char)
            if char == "'":
                in_string = False
        elif char == "'":
            in_string = True
            buffer.append(char)
        elif char == "-" and script[index : index + 2] == "--":
            end = script.find("\n", index)
            index = len(script) if end == -1 else end
            continue
        elif char == ";":
            statement = "".join(buffer).strip()
            if statement:
                statements.append(statement)
            buffer = []
        else:
            buffer.append(char)
        index += 1
    tail = "".join(buffer).strip()
    if tail:
        statements.append(tail)
    return statements
