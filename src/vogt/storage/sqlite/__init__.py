"""The SQLite backend — the only place SQL is allowed to live."""

from __future__ import annotations

from vogt.storage.sqlite.declared import SqliteDeclaredStore
from vogt.storage.sqlite.observed import SqliteObservedStore

__all__ = ["SqliteDeclaredStore", "SqliteObservedStore"]
