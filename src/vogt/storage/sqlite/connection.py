"""Connection handling for the SQLite backend.

Connections are opened per transaction and closed after it. That is slightly
wasteful and completely thread-safe, which is the right trade while the same
data directory is reachable from a CLI process, a server process and a stdio
MCP process at the same time (`DEPLOYMENT.md` §2.1).
"""

from __future__ import annotations

import sqlite3
from pathlib import Path

BUSY_TIMEOUT_MS = 5_000


def connect(path: Path, *, create: bool) -> sqlite3.Connection:
    """Open a connection with Vogt's pragmas applied.

    `isolation_level=None` puts the driver in autocommit mode so that this
    package issues its own `BEGIN`/`COMMIT`. Transaction boundaries are a
    correctness concern here (NFR-I1) and are not left to the driver.
    """
    if not create and not path.exists():
        msg = f"no database at {path}"
        raise FileNotFoundError(msg)
    if create:
        path.parent.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(path, isolation_level=None)
    conn.row_factory = sqlite3.Row
    conn.execute("PRAGMA foreign_keys = ON")
    conn.execute(f"PRAGMA busy_timeout = {BUSY_TIMEOUT_MS}")
    conn.execute("PRAGMA journal_mode = WAL")
    return conn


def split_statements(script: str) -> list[str]:
    """Split a migration script into individual statements.

    Migrations are plain DDL: no triggers, no `BEGIN ... END` bodies, no
    semicolons inside identifiers. `executescript` cannot be used because it
    commits any open transaction first, and each migration must apply inside
    one (NFR-I3).
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
