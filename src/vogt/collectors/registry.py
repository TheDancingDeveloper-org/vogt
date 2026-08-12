"""The collector plugin registry (FR-O1).

Core collectors require no network and are always present. Optional ones —
the GitHub adapter — register themselves only when configured, and their
absence yields "not collected", never failure (NFR-PO1).
"""

from __future__ import annotations

from collections.abc import Iterator

from vogt.collectors.base import Collector
from vogt.collectors.contract_checker import ContractCheckerCollector
from vogt.collectors.dep_refs import DepRefCollector
from vogt.collectors.git_local import GitLocalCollector
from vogt.collectors.source_markers import SourceMarkerCollector
from vogt.errors import NotFound


def core_collectors() -> list[Collector]:
    """The offline collectors every instance has.

    `contract-checker` is here but marked `on_demand_only`, so a plain
    `sweep` does not run it: nothing re-checks compliance on a timer (r3,
    FR-G5 deferred). Naming it explicitly runs it.
    """
    return [
        GitLocalCollector(),
        SourceMarkerCollector(),
        DepRefCollector(),
        ContractCheckerCollector(),
    ]


def is_on_demand(collector: Collector) -> bool:
    """Whether this collector opts out of unnamed sweeps."""
    return bool(getattr(collector, "on_demand_only", False))


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
        """Resolve a caller's choice of collectors.

        An unnamed sweep skips on-demand-only collectors. That is the
        mechanism behind "the contract is evaluated when someone asks":
        without it, adding a scheduler at M4 would quietly reintroduce the
        continuous re-checking r3 deleted.
        """
        if names:
            return [
                collector
                for collector in (self.get(name) for name in names)
                if not (offline_only and collector.requires_network)
            ]
        return [
            collector
            for collector in self.all()
            if not is_on_demand(collector)
            and not (offline_only and collector.requires_network)
        ]
