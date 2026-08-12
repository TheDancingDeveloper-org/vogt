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
