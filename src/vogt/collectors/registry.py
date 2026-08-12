"""The collector plugin registry (FR-O1).

Core collectors require no network and are always present. Optional ones —
the GitHub adapter — register themselves only when configured, and their
absence yields "not collected", never failure (NFR-PO1).
"""

from __future__ import annotations

from collections.abc import Iterator

from vogt.collectors.base import Collector
from vogt.collectors.dep_refs import DepRefCollector
from vogt.collectors.git_local import GitLocalCollector
from vogt.collectors.source_markers import SourceMarkerCollector
from vogt.errors import NotFound


def core_collectors() -> list[Collector]:
    """The offline collectors every instance has.

    `contract-checker` joins this list at M3, and it runs on demand only —
    nothing re-checks compliance on a timer (FR-G5, deferred by r3).
    """
    return [GitLocalCollector(), SourceMarkerCollector(), DepRefCollector()]


class CollectorRegistry:
    """The collectors this instance can run."""

    def __init__(self, collectors: list[Collector] | None = None) -> None:
        self._collectors: dict[str, Collector] = {}
        for collector in collectors if collectors is not None else core_collectors():
            self.add(collector)

    def add(self, collector: Collector) -> None:
        if collector.name in self._collectors:
            msg = f"duplicate collector name: {collector.name}"
            raise ValueError(msg)
        self._collectors[collector.name] = collector

    def __iter__(self) -> Iterator[Collector]:
        return iter(self.all())

    def __len__(self) -> int:
        return len(self._collectors)

    @property
    def names(self) -> tuple[str, ...]:
        return tuple(sorted(self._collectors))

    def all(self) -> list[Collector]:
        return [self._collectors[name] for name in self.names]

    def offline(self) -> list[Collector]:
        """Only the collectors that touch nothing outside this machine.

        The forge-less test layer runs exactly this set, which is what keeps
        NFR-PO2 true rather than aspirational.
        """
        return [c for c in self.all() if not c.requires_network]

    def get(self, name: str) -> Collector:
        try:
            return self._collectors[name]
        except KeyError as exc:
            msg = (
                f"no collector named {name!r} "
                f"(available: {', '.join(self.names) or 'none'})"
            )
            raise NotFound(msg) from exc

    def select(self, names: tuple[str, ...], *, offline_only: bool) -> list[Collector]:
        """Resolve a caller's choice of collectors."""
        chosen = [self.get(name) for name in names] if names else self.all()
        if offline_only:
            chosen = [c for c in chosen if not c.requires_network]
        return chosen
