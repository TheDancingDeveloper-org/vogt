"""Talking to the session engine (FR-E1, FR-E8).

The engine is the other half of the merged product — the Rust process that
owns PTYs, scrollback and activity state. Vogt asks it for nine things and
nothing else: start a session, list sessions, describe one, stop one, read
the archive of one that has ended, list the scheduled agent tasks, and — the
history-search trio (#491) — list archived sessions, search session output
(live sessions included), and read the tail of one session's log. That list
is the whole coupling, and keeping it that short is what makes the two-process
shape (NFR-D11) worth having rather than merely tolerable.

The archive read arrived with FR-E6, the task-run read with FR-E7, and the
history-search trio with #491; all are *reads*: what a session left behind,
what a bound task's run found, and what any session has printed come back to
Vogt by being asked for on demand, never by being pushed into the observed
store from outside. That is `SCHEMA.md` §1's rule — nothing writes
`observed.sqlite3` except collectors — held rather than bent.

The adapter is optional in exactly the way the forge adapter is. No engine
configured means the `session.*` operations report that, and every other
operation is unaffected — FR-E9 read from Vogt's side.
"""

from vogt.adapters.engine.client import (
    EngineAgentTask,
    EngineArchivedSession,
    EngineClient,
    EngineHistoryMatch,
    EngineHistorySession,
    EngineSession,
    EngineSessionLog,
    EngineTaskFinding,
    EngineTaskRun,
    EngineUnavailable,
)

__all__ = [
    "EngineAgentTask",
    "EngineArchivedSession",
    "EngineClient",
    "EngineHistoryMatch",
    "EngineHistorySession",
    "EngineSession",
    "EngineSessionLog",
    "EngineTaskFinding",
    "EngineTaskRun",
    "EngineUnavailable",
]
