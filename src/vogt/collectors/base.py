"""What a collector is.

A collector answers one question about one registered project and returns
findings. It does not decide whether they are interesting, does not write
anything, and does not know whether it is being run on a schedule or on
demand.
"""

from __future__ import annotations

from collections.abc import Iterable, Iterator
from dataclasses import dataclass, field
from pathlib import Path
from typing import Protocol

from vogt.config import VogtConfig
from vogt.core.clock import Clock
from vogt.core.digest import digest_of
from vogt.core.entities import Project
from vogt.errors import VogtError
from vogt.storage.observed_types import PendingObservation

#: Findings are `PendingObservation`s; the alias exists so collectors read as
#: "return findings" rather than "return pending observations".
Finding = PendingObservation


class CollectorError(VogtError):
    """A collector could not complete for one project.

    Raised by collectors and caught by the sweeper, which records the sweep
    as `partial` and names the project. One unreadable repository must not
    cost the estate its whole sweep.
    """

    code = "collector_failed"
    http_status = 500


def finding(
    *,
    kind: str,
    subject_key: str,
    payload: dict[str, object],
    project: Project | None = None,
    source_url: str | None = None,
    promoted: bool = False,
) -> Finding:
    """Build a finding, digesting the payload for dedup (FR-O7).

    The digest covers the payload only — not the sweep, not the timestamp —
    so re-observing an unchanged subject writes nothing however often it is
    observed (NFR-S2).
    """
    return PendingObservation(
        kind=kind,
        subject_key=subject_key,
        payload=payload,
        content_digest=digest_of(payload),
        project_id=None if project is None else project.id,
        source_url=source_url,
        promoted=promoted,
    )


@dataclass(frozen=True)
class CollectorContext:
    """What a collector is allowed to know."""

    config: VogtConfig
    clock: Clock
    #: Extra per-collector settings, e.g. a forge token file. Absent means
    #: the optional integration is not configured, never that it failed.
    options: dict[str, object] = field(default_factory=dict)


class Collector(Protocol):
    """One source of observations about a project."""

    @property
    def name(self) -> str:
        """Stable identifier, recorded on every sweep and observation."""
        ...

    @property
    def requires_network(self) -> bool:
        """Whether this collector talks to anything outside the machine.

        The forge-less test layer runs only collectors that answer `False`,
        which is how NFR-PO2 stays true rather than aspirational.
        """
        ...

    def collect(self, ctx: CollectorContext, project: Project) -> Iterable[Finding]:
        """Look at one project and return what was found."""
        ...


def walk_project(
    root: Path,
    *,
    exclusions: tuple[str, ...],
    extensions: tuple[str, ...] = (),
    max_files: int = 20_000,
) -> Iterator[Path]:
    """Walk a project's files, honouring its exclusions (FR-G12).

    Exclusions are applied *before* a file is opened, so vendored and
    generated content never becomes an observation at all — the cheapest of
    the three noise mechanisms, and the only one that has to handle volume
    (DESIGN §3.6).
    """
    if not root.is_dir():
        return
    seen = 0
    stack = [root]
    while stack:
        directory = stack.pop()
        try:
            entries = sorted(directory.iterdir())
        except OSError:  # pragma: no cover - unreadable directory
            continue
        for entry in entries:
            if _excluded(entry, root=root, exclusions=exclusions):
                continue
            if entry.is_dir():
                if not entry.is_symlink():
                    stack.append(entry)
                continue
            if extensions and entry.suffix not in extensions:
                continue
            seen += 1
            if seen > max_files:
                return
            yield entry


def _excluded(path: Path, *, root: Path, exclusions: tuple[str, ...]) -> bool:
    try:
        relative = path.relative_to(root)
    except ValueError:  # pragma: no cover - walk never leaves the root
        return True
    name = path.name
    for pattern in exclusions:
        trimmed = pattern.rstrip("/")
        if not trimmed:
            continue
        if name == trimmed or relative.match(trimmed) or relative.match(f"{trimmed}/*"):
            return True
        if any(part == trimmed for part in relative.parts):
            return True
    return False
