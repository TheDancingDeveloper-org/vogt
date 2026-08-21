"""Running collectors, and recording that they ran.

The coverage record is the point (FR-O3). A sweep row says which collector
looked at which projects, when it started and finished, and how it went — and
that is what makes "absent" different from "not collected" (FR-O4). Most of
cadastre's "missing" drift turned out to be collector-coverage artefact; this
is the mechanism that prevents the same mistake here.

Failure is contained per project. One unreadable repository makes a sweep
`partial` and names the project; it does not cost the estate the other
eleven, and it never touches declared data (NFR-I2).
"""

from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime
from typing import Protocol, runtime_checkable

from vogt.collectors.base import Collector, CollectorContext, Finding
from vogt.core.entities import Project
from vogt.storage.interface import ObservedStore
from vogt.storage.observed_types import SweepReport


@runtime_checkable
class PostAppend(Protocol):
    """A collector that has bookkeeping to commit once its append lands.

    The incremental forge sync advances its watermark and records which
    subjects it confirmed here rather than in `collect`, so a watermark can
    never move past observations that failed to persist (D1)."""

    def after_append(self, *, at: datetime) -> None: ...


@dataclass(frozen=True)
class SweepOutcomeSet:
    """Every collector's report from one `sweep` invocation."""

    reports: list[SweepReport]
    subjects: int = 0
    dep_refs: int = 0

    @property
    def new(self) -> int:
        return sum(report.new for report in self.reports)

    @property
    def unchanged(self) -> int:
        return sum(report.unchanged for report in self.reports)

    @property
    def worst_outcome(self) -> str:
        outcomes = {report.outcome for report in self.reports}
        for outcome in ("failed", "partial", "ok"):
            if outcome in outcomes:
                return outcome
        return "ok"


class Sweeper:
    """Runs collectors over a scope and writes the coverage records."""

    def __init__(self, store: ObservedStore, ctx: CollectorContext) -> None:
        self._store = store
        self._ctx = ctx

    def run(
        self, collectors: list[Collector], projects: list[Project]
    ) -> list[SweepReport]:
        """Run each collector over every project in scope.

        One sweep row per collector, because that is the unit coverage is
        asked about: "has anything looked at this repo lately" is answered
        per collector, not per run.
        """
        return [self.run_one(collector, projects) for collector in collectors]

    def run_one(self, collector: Collector, projects: list[Project]) -> SweepReport:
        started = self._ctx.clock()
        sweep = self._store.begin_sweep(
            collector=collector.name,
            scope=[project.id for project in projects],
            at=started,
        )

        findings: list[Finding] = []
        failures: dict[str, str] = {}
        for project in projects:
            try:
                findings.extend(collector.collect(self._ctx, project))
            except Exception as exc:  # a bad project must not end the sweep
                failures[project.slug] = f"{type(exc).__name__}: {exc}"

        stats = self._store.append(sweep.id, findings, at=self._ctx.clock())

        if isinstance(collector, PostAppend):
            # Only now that the append has committed does the collector's own
            # bookkeeping (the sync watermark, `subject_seen`) advance — never
            # ahead of the evidence it summarises (D1).
            collector.after_append(at=self._ctx.clock())

        if failures and len(failures) == len(projects) and projects:
            outcome = "failed"
        elif failures:
            outcome = "partial"
        else:
            outcome = "ok"

        detail = (
            None
            if not failures
            else "; ".join(f"{slug}: {why}" for slug, why in sorted(failures.items()))
        )
        self._store.finish_sweep(
            sweep.id,
            outcome=outcome,  # type: ignore[arg-type]
            stats={
                "projects": len(projects),
                "new": stats.new,
                "unchanged": stats.unchanged,
                "failed_projects": len(failures),
            },
            at=self._ctx.clock(),
            detail=detail,
        )
        return SweepReport(
            collector=collector.name,
            sweep_id=sweep.id,
            outcome=outcome,
            projects=len(projects),
            new=stats.new,
            unchanged=stats.unchanged,
            failures=failures,
            detail=detail,
        )
