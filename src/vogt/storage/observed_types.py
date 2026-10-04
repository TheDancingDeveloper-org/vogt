"""Value types the observed-store interface speaks in.

Separate from both the interface and the SQLite backend so that neither has
to import the other: collectors build `PendingObservation`s, the application
builds `DepRefRow`s, and any backend consumes them.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from datetime import datetime


@dataclass(frozen=True)
class PendingObservation:
    """A finding on its way into the store, before it has an id."""

    kind: str
    subject_key: str
    payload: dict[str, object]
    content_digest: str
    project_id: str | None = None
    source_url: str | None = None
    promoted: bool = False


@dataclass(frozen=True)
class AppendStats:
    """What appending a collector's findings actually changed.

    `unchanged` is the interesting number: it is the evidence that digest
    dedup is working, and a sweep that reports thousands of new rows for an
    unchanged repository is a bug in a collector's subject keys.
    """

    new: int = 0
    unchanged: int = 0

    @property
    def total(self) -> int:
        return self.new + self.unchanged


@dataclass(frozen=True)
class DepRefRow:
    """A resolved dependency reference, ready to replace the projection."""

    subject_key: str
    from_project_id: str
    ref_kind: str
    raw_target: str
    manifest: str | None
    to_project_id: str | None
    observed_at: datetime


@dataclass(frozen=True)
class PruneReport:
    """What retention removed, and what protected the rest."""

    removed: int = 0
    kept_latest: int = 0
    kept_referenced: int = 0


@dataclass(frozen=True)
class SweepReport:
    """The outcome of running one collector over one scope."""

    collector: str
    sweep_id: str
    outcome: str
    projects: int = 0
    new: int = 0
    unchanged: int = 0
    failures: dict[str, str] = field(default_factory=dict)
    detail: str | None = None


# -- agent activity index ----------------------------------------------------


@dataclass(frozen=True)
class TranscriptCursor:
    """How far one transcript file has been indexed.

    `offset` is a byte position at a line boundary: everything before it has
    been read and its calls stored in the same transaction that moved it, so
    a crash re-reads at most the batch that did not commit. `agent_session_id`
    and `cwd` are carried because Codex states them once, at the top of the
    file, and a later batch starting mid-file still needs them.
    """

    path: str
    agent: str
    offset: int
    size: int
    agent_session_id: str | None = None
    cwd: str | None = None


@dataclass(frozen=True)
class ActivityCall:
    """One tool call, already redacted, on its way into the index."""

    source_path: str
    call_id: str
    agent: str
    agent_session_id: str
    cwd: str | None
    tool: str
    summary: str
    services: tuple[str, ...]
    #: The call dumps configuration or environment, so whatever its result
    #: says is never excerpted — even when the result lands in a later batch.
    withheld: bool
    at: datetime


@dataclass(frozen=True)
class ActivityResult:
    """The outcome of a call, matched to it by (file, call id)."""

    source_path: str
    call_id: str
    error: bool
    excerpt: str | None
    at: datetime | None


@dataclass(frozen=True)
class ActivityBatch:
    """What one bounded read of the transcript roots produced."""

    calls: list[ActivityCall] = field(default_factory=list)
    results: list[ActivityResult] = field(default_factory=list)
    cursors: list[TranscriptCursor] = field(default_factory=list)
    files: int = 0
    bytes_read: int = 0
    #: Bytes known to be waiting past this batch's budget.
    backlog_bytes: int = 0
    skipped: dict[str, str] = field(default_factory=dict)


@dataclass(frozen=True)
class ActivityIndexStats:
    calls: int = 0
    results: int = 0


@dataclass(frozen=True)
class ActivityQuery:
    """A filter over the index. Every field narrows; none widens."""

    q: str | None = None
    service: str | None = None
    tool: str | None = None
    errors_only: bool = False
    since: datetime | None = None
    until: datetime | None = None
    agent_session_ids: tuple[str, ...] | None = None
    #: Working directories a call must be in or under, e.g. a project root.
    cwd_roots: tuple[str, ...] | None = None


@dataclass(frozen=True)
class ActivityEventRow:
    id: str
    agent: str
    agent_session_id: str
    cwd: str | None
    tool: str
    summary: str
    services: tuple[str, ...]
    error: bool
    excerpt: str | None
    at: datetime
    finished_at: datetime | None


@dataclass(frozen=True)
class ActivitySessionRow:
    agent: str
    agent_session_id: str
    cwd: str | None
    first_at: datetime
    last_at: datetime
    calls: int
    errors: int
    #: Calls whose result has been seen.
    finished: int
    #: Sum of call→result time over finished calls, in milliseconds.
    wait_ms: int
    tools: dict[str, int] = field(default_factory=dict)
    services: dict[str, int] = field(default_factory=dict)
