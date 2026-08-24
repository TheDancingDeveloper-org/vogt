#!/usr/bin/env python3
"""A registry-driven load generator that records numbers, not a pass/fail (#297).

Every capability of the product is already a callable in the operation
registry (`vogt.registry.operations`), so a load generator does not need a
server, an HTTP client, or a parallel bootstrap: it builds one `AppContext`
over a temporary SQLite database and drives the *real* operations through
``Operation.run`` — the same handlers the CLI, REST and MCP surfaces reach.
Every mutating call goes through `audited_write` with a principal and a
reason, exactly as a human write would, because a load number measured on a
shortcut is a number about the shortcut.

What it exercises (increment 1):

- ``project.register`` — N projects, each pointed at a small on-disk tree so
  a sweep has something real to walk.
- ``work.create`` — N × M native declared work items. Native (project-less)
  rather than in a linked project, because `work.create` on a *linked*
  project writes through to the forge (decision 9 of #181) and that needs a
  live GitHub — out of scope for an in-process run. Project-less native rows
  are a first-class shape the ranked views serve (they pass the #183
  withdrawal filter), so the ranking work below is genuine.
- ``work.relate`` — R declared edges between the created items.
- ``backlog`` / ``bugs`` — the ranked views, timed at scale (NFR-S1/S4): the
  declared+observed merge every aggregate answer performs. Timed both before
  a sweep (declared only) and after one (declared + observed), so the merge
  cost is in the recorded ``backlog`` numbers.
- ``sweep`` — the offline collectors over the registered trees.

For each operation it records p50/p95 latency (ms) and, once, the process
RSS (MB), into a JSON report.

Increment 2 adds the SOAK dimension (`--mode soak`) and a RECORDED-NUMBERS
mechanism. A soak seeds a base dataset, then drives a sustained steady mix
(`work.create` → `work.update` → `work.get` → `work.list` → `backlog` →
`bugs`, with an offline `sweep` interleaved on a cadence as the in-process
stand-in for the live collector schedule) for N iterations. It records
throughput, per-operation p50/p95/p99 latency, an error rate, and RSS
start/end/growth — the last being the leak signal a soak exists to watch — and
labels the report with *how* the numbers were produced (`--produced-by`).
`bench/soak_baseline.json` is the committed baseline; `--check-baseline`
compares a fresh run to it and exits non-zero on drift past the issue's 2×
rule (`compare_to_baseline`), for the nightly job to gate on.

DEFERRED still (not built here): the *S-hour* soak on the self-hosted runner
against a stood-up stack, whose numbers are the authoritative baseline (this
increment records honest in-process numbers as the starting point — see
`bench/README.md`); the K concurrent WebSocket attach clients (that needs the
Rust engine); and the nightly CI job / `vogt/bench` results branch.

The numbers this prints are local/dev-box measurements, not production SLAs.

Usage::

    uv run python scripts/load.py --scale 1 --out /tmp/load.json
    uv run python scripts/load.py --mode soak --iterations 200 \\
        --check-baseline bench/soak_baseline.json
"""

from __future__ import annotations

import argparse
import json
import platform
import resource
import sys
import tempfile
import time
from collections.abc import Sequence
from dataclasses import dataclass, field
from datetime import UTC, datetime
from pathlib import Path
from random import Random
from typing import Any

from vogt.application.context import AppContext, build_context
from vogt.application.models import (
    BacklogParams,
    BugsParams,
    CreateWorkParams,
    GetWorkParams,
    InitParams,
    ListWorkParams,
    RegisterProjectParams,
    RelateWorkParams,
    SweepParams,
    UpdateWorkParams,
)
from vogt.application.services import init_instance
from vogt.config import VogtConfig
from vogt.core.entities import Priority, RelationKind, WorkKind
from vogt.core.principal import Principal
from vogt.registry.registry import OperationRegistry, default_registry

#: The schema version of the JSON report. Bumped when its shape changes so a
#: downstream reader (the deferred nightly comparison) can refuse an old file
#: rather than misread it.
REPORT_SCHEMA_VERSION = 1

#: The schema version of the soak report (increment 2). Separate from the load
#: report's version because the two shapes differ (soak carries throughput, an
#: error rate, an RSS start/end/growth and p99) and evolve independently.
SOAK_REPORT_SCHEMA_VERSION = 1

#: The regression factor the issue names: a metric more than this multiple
#: worse than its recorded baseline is drift worth failing on. Not pass/fail
#: on the absolute number — only on the movement.
DRIFT_THRESHOLD = 2.0

#: The principal every write is attributed to. A load run is not a person, and
#: says so, rather than borrowing whoever ran it.
LOAD_PRINCIPAL = Principal(
    identity_ref="local:load-generator",
    kind="agent",
    display_name="load-generator",
)

#: Cycled so the ranked views have variety to sort by rather than one bucket.
_KINDS: tuple[WorkKind, ...] = ("feature", "bug", "chore", "question")
_PRIORITIES: tuple[Priority, ...] = ("p0", "p1", "p2", "p3", "p4")
#: `implemented_by` is observed, never declared (#284), so it is absent here.
_RELATION_KINDS: tuple[RelationKind, ...] = (
    "depends_on",
    "relates_to",
    "duplicate_of",
    "parent_of",
)


# -- pure logic: percentiles, stats and the report shape -------------------


def percentile(values: Sequence[float], pct: float) -> float:
    """The ``pct``-th percentile of ``values`` by nearest-rank on a sorted copy.

    Nearest-rank rather than an interpolating estimator on purpose: with a
    handful of samples the interpolated value is a fiction between two real
    measurements, and a load report should quote a number that was actually
    observed. Ranks are 1-based; ``pct`` is a fraction in ``[0, 1]``.
    """
    if not values:
        msg = "percentile of an empty sample is undefined"
        raise ValueError(msg)
    if not 0.0 <= pct <= 1.0:
        msg = f"pct must be in [0, 1], not {pct}"
        raise ValueError(msg)
    ordered = sorted(values)
    if pct == 0.0:
        return ordered[0]
    rank = max(1, -(-len(ordered) * int(pct * 100) // 100))  # ceil, no float error
    return ordered[min(rank, len(ordered)) - 1]


@dataclass(frozen=True)
class OperationStats:
    """The recorded numbers for one operation across a run."""

    count: int
    p50_ms: float
    p95_ms: float

    def to_dict(self) -> dict[str, float | int]:
        return {
            "count": self.count,
            "p50_ms": round(self.p50_ms, 3),
            "p95_ms": round(self.p95_ms, 3),
        }


@dataclass
class LatencyRecorder:
    """Accumulates per-operation latency samples, in milliseconds."""

    _samples: dict[str, list[float]] = field(default_factory=dict)

    def record(self, operation: str, elapsed_ms: float) -> None:
        self._samples.setdefault(operation, []).append(elapsed_ms)

    def time(self, operation: str, call: Any) -> Any:  # noqa: ANN401
        """Run ``call``, record how long it took under ``operation``, return it."""
        started = time.perf_counter()
        result = call()
        self.record(operation, (time.perf_counter() - started) * 1000.0)
        return result

    def stats(self) -> dict[str, OperationStats]:
        return {
            name: OperationStats(
                count=len(samples),
                p50_ms=percentile(samples, 0.50),
                p95_ms=percentile(samples, 0.95),
            )
            for name, samples in self._samples.items()
        }


@dataclass(frozen=True)
class SoakOperationStats:
    """The recorded numbers for one operation across a soak.

    Adds p99 and an error count to the load-run shape: a soak runs the same
    call thousands of times, so the tail past p95 is now a population worth
    quoting, and an operation that *sometimes* raises under sustained load is
    exactly the failure a soak exists to surface — so errors are counted
    rather than allowed to abort the run.
    """

    count: int
    errors: int
    p50_ms: float
    p95_ms: float
    p99_ms: float

    def to_dict(self) -> dict[str, float | int]:
        return {
            "count": self.count,
            "errors": self.errors,
            "p50_ms": round(self.p50_ms, 3),
            "p95_ms": round(self.p95_ms, 3),
            "p99_ms": round(self.p99_ms, 3),
        }


@dataclass
class SoakRecorder:
    """Latency samples *and* error counts per operation, over a soak.

    A soak must not stop the first time one call raises: a transient failure
    under sustained load is a number to record (the error rate), not a reason
    to throw the whole run away. `guard` therefore catches, counts, and lets
    the loop continue; a call that succeeds contributes a latency sample as
    usual. The successful-call total across every operation is what the report
    turns into throughput.
    """

    _samples: dict[str, list[float]] = field(default_factory=dict)
    _errors: dict[str, int] = field(default_factory=dict)

    def record(self, operation: str, elapsed_ms: float) -> None:
        self._samples.setdefault(operation, []).append(elapsed_ms)

    def record_error(self, operation: str) -> None:
        self._errors[operation] = self._errors.get(operation, 0) + 1

    def guard(self, operation: str, call: Any) -> None:  # noqa: ANN401
        """Run ``call``, timing it on success and counting it on failure."""
        started = time.perf_counter()
        try:
            call()
        except Exception:  # a soak records failures, it never aborts
            self.record_error(operation)
            return
        self.record(operation, (time.perf_counter() - started) * 1000.0)

    @property
    def successful_calls(self) -> int:
        return sum(len(samples) for samples in self._samples.values())

    @property
    def total_errors(self) -> int:
        return sum(self._errors.values())

    def stats(self) -> dict[str, SoakOperationStats]:
        return {
            name: SoakOperationStats(
                count=len(samples),
                errors=self._errors.get(name, 0),
                p50_ms=percentile(samples, 0.50),
                p95_ms=percentile(samples, 0.95),
                p99_ms=percentile(samples, 0.99),
            )
            for name, samples in self._samples.items()
        }


@dataclass(frozen=True)
class DatasetPlan:
    """What a run will build and exercise. Pure description, no side effects."""

    projects: int
    work_items_per_project: int
    relations: int
    ranking_iterations: int

    def __post_init__(self) -> None:
        for name in ("projects", "work_items_per_project", "ranking_iterations"):
            if getattr(self, name) < 1:
                msg = f"{name} must be >= 1, not {getattr(self, name)}"
                raise ValueError(msg)
        if self.relations < 0:
            msg = f"relations must be >= 0, not {self.relations}"
            raise ValueError(msg)

    @property
    def total_work_items(self) -> int:
        return self.projects * self.work_items_per_project

    def to_dict(self) -> dict[str, int]:
        return {
            "projects": self.projects,
            "work_items_per_project": self.work_items_per_project,
            "total_work_items": self.total_work_items,
            "relations": self.relations,
            "ranking_iterations": self.ranking_iterations,
        }


def plan_from_scale(scale: int) -> DatasetPlan:
    """A default plan sized by a single knob.

    ``scale`` multiplies the project count (and, through it, the item count):
    the default of 1 is deliberately small so a local run finishes in
    seconds, and ``--scale 10`` reaches a thousand items without anyone
    editing the individual counts.
    """
    if scale < 1:
        msg = f"scale must be >= 1, not {scale}"
        raise ValueError(msg)
    return DatasetPlan(
        projects=5 * scale,
        work_items_per_project=20,
        relations=20 * scale,
        ranking_iterations=20,
    )


@dataclass(frozen=True)
class SoakPlan:
    """A sustained run: seed a base dataset, then drive a steady mix for a while.

    ``base`` is the dataset the soak stands up first (the same builder the load
    run uses). ``iterations`` is how many rounds of the steady operation mix to
    then drive — the soak's length. ``sweep_every`` interleaves an offline
    sweep every N iterations, which is the in-process stand-in for the live
    collector schedule the issue asks a soak to run under: `serve` ticks
    `sweep` on a timer (``adapters/http/scheduler.py``), and without the HTTP
    server up, driving `sweep` on an iteration cadence is the honest
    equivalent — the same operation, the same write path, no wall clock to
    wait on.
    """

    base: DatasetPlan
    iterations: int
    sweep_every: int

    def __post_init__(self) -> None:
        if self.iterations < 1:
            msg = f"iterations must be >= 1, not {self.iterations}"
            raise ValueError(msg)
        if self.sweep_every < 1:
            msg = f"sweep_every must be >= 1, not {self.sweep_every}"
            raise ValueError(msg)

    @property
    def sweeps(self) -> int:
        """How many scheduled sweeps the soak will drive."""
        return self.iterations // self.sweep_every

    def to_dict(self) -> dict[str, Any]:
        return {
            "iterations": self.iterations,
            "sweep_every": self.sweep_every,
            "sweeps": self.sweeps,
            "base": self.base.to_dict(),
        }


def soak_plan_from_scale(scale: int, *, iterations: int, sweep_every: int) -> SoakPlan:
    """A soak plan sized by the same single knob the load plan uses.

    The base dataset reuses `plan_from_scale`, but with fewer ranking
    iterations up front: the soak's own steady mix is where the ranked views
    get hammered, so seeding them twenty times before the soak even starts is
    wasted warm-up.
    """
    base = plan_from_scale(scale)
    seeded = DatasetPlan(
        projects=base.projects,
        work_items_per_project=base.work_items_per_project,
        relations=base.relations,
        ranking_iterations=1,
    )
    return SoakPlan(base=seeded, iterations=iterations, sweep_every=sweep_every)


def relation_triples(
    refs: Sequence[str], count: int, *, seed: int
) -> list[tuple[str, str, RelationKind]]:
    """Deterministic, self-free, duplicate-free ``(ref, target, kind)`` edges.

    `work.relate` refuses a self-edge and a duplicate ``(kind, target)`` pair
    (Conflict), so the generator must not produce either — otherwise a run
    fails for a reason that has nothing to do with load. Seeded so a run is
    reproducible and a test can assert the exact edges.
    """
    if count <= 0:
        return []
    if len(refs) < 2:
        msg = "at least two work items are needed to relate any"
        raise ValueError(msg)
    rng = Random(seed)
    seen: set[tuple[str, str, RelationKind]] = set()
    triples: list[tuple[str, str, RelationKind]] = []
    max_edges = len(refs) * (len(refs) - 1) * len(_RELATION_KINDS)
    attempts = 0
    attempt_budget = max_edges * 4
    while len(triples) < count and attempts < attempt_budget:
        attempts += 1
        source = rng.choice(refs)
        target = rng.choice(refs)
        if source == target:
            continue
        kind = rng.choice(_RELATION_KINDS)
        triple = (source, target, kind)
        if triple in seen:
            continue
        seen.add(triple)
        triples.append(triple)
    return triples


def build_report(
    *,
    plan: DatasetPlan,
    stats: dict[str, OperationStats],
    rss_mb: float,
    seed: int,
    duration_s: float,
    generated_at: datetime,
) -> dict[str, Any]:
    """Assemble the JSON-serialisable report. Pure, given its inputs."""
    return {
        "schema_version": REPORT_SCHEMA_VERSION,
        "generated_at": generated_at.astimezone(UTC).isoformat(),
        "note": (
            "Local/dev-box measurements from scripts/load.py (#297, "
            "increment 1). Not a production SLA."
        ),
        "host": {
            "python": platform.python_version(),
            "platform": platform.platform(),
        },
        "seed": seed,
        "duration_s": round(duration_s, 3),
        "rss_mb": round(rss_mb, 1),
        "plan": plan.to_dict(),
        "operations": {name: stat.to_dict() for name, stat in sorted(stats.items())},
    }


def build_soak_report(
    *,
    plan: SoakPlan,
    stats: dict[str, SoakOperationStats],
    successful_calls: int,
    total_errors: int,
    rss_start_mb: float,
    rss_end_mb: float,
    rss_peak_mb: float,
    seed: int,
    duration_s: float,
    produced_by: str,
    generated_at: datetime,
) -> dict[str, Any]:
    """Assemble the JSON-serialisable soak report. Pure, given its inputs.

    ``produced_by`` labels *how* the numbers were measured — an in-process run
    on a dev box or a self-hosted runner — because a soak number is only
    comparable to another taken the same way, and the recorded-numbers file
    exists to be compared against.
    """
    attempts = successful_calls + total_errors
    throughput = successful_calls / duration_s if duration_s > 0 else 0.0
    error_rate = total_errors / attempts if attempts > 0 else 0.0
    return {
        "schema_version": SOAK_REPORT_SCHEMA_VERSION,
        "kind": "soak",
        "generated_at": generated_at.astimezone(UTC).isoformat(),
        "produced_by": produced_by,
        "note": (
            "Soak measurements from scripts/load.py --mode soak (#297, "
            "increment 2). Not a production SLA; comparable only to another "
            "run with the same `produced_by`."
        ),
        "host": {
            "python": platform.python_version(),
            "platform": platform.platform(),
        },
        "seed": seed,
        "duration_s": round(duration_s, 3),
        "throughput_ops_per_s": round(throughput, 2),
        "error_rate": round(error_rate, 6),
        "total_calls": attempts,
        "total_errors": total_errors,
        "rss_mb": {
            "start": round(rss_start_mb, 1),
            "end": round(rss_end_mb, 1),
            "growth": round(rss_end_mb - rss_start_mb, 1),
            # `ru_maxrss` and `/proc/self/statm` count residency slightly
            # differently, so the peak can read just under a current sample;
            # clamp so "peak" is never below what we actually observed live.
            "peak": round(max(rss_peak_mb, rss_start_mb, rss_end_mb), 1),
        },
        "plan": plan.to_dict(),
        "operations": {name: stat.to_dict() for name, stat in sorted(stats.items())},
    }


@dataclass(frozen=True)
class Regression:
    """One metric that drifted past the drift threshold against a baseline."""

    metric: str
    baseline: float
    current: float
    ratio: float

    def describe(self) -> str:
        return (
            f"{self.metric}: {self.current:g} vs baseline {self.baseline:g} "
            f"({self.ratio:.2f}x)"
        )


def compare_to_baseline(
    baseline: dict[str, Any],
    current: dict[str, Any],
    *,
    threshold: float = DRIFT_THRESHOLD,
) -> list[Regression]:
    """Return the metrics in ``current`` that regressed past ``threshold``.

    The issue's rule is "alert on regression > 2x against the previous run".
    Two directions count as regression: a per-operation p95 latency that grew
    to more than ``threshold``x its baseline, and an overall throughput that
    fell to less than ``1/threshold`` of its baseline. A metric absent from
    either report is skipped rather than treated as infinite drift — a report
    that simply did not exercise an operation is not a regression in it.

    Pure and side-effect free: it reads two decoded reports and returns
    findings. Whether findings *fail* a run is the caller's decision (the
    ``--check-baseline`` CLI mode makes it a non-zero exit; a unit test just
    asserts the list).
    """
    if threshold <= 0:
        msg = f"threshold must be > 0, not {threshold}"
        raise ValueError(msg)
    regressions: list[Regression] = []

    base_ops = baseline.get("operations", {})
    cur_ops = current.get("operations", {})
    for name in sorted(set(base_ops) & set(cur_ops)):
        base_p95 = base_ops[name].get("p95_ms")
        cur_p95 = cur_ops[name].get("p95_ms")
        if not base_p95 or cur_p95 is None:
            continue
        ratio = cur_p95 / base_p95
        if ratio > threshold:
            regressions.append(Regression(f"{name}.p95_ms", base_p95, cur_p95, ratio))

    base_tput = baseline.get("throughput_ops_per_s")
    cur_tput = current.get("throughput_ops_per_s")
    if base_tput and cur_tput is not None and cur_tput > 0:
        ratio = base_tput / cur_tput
        if ratio > threshold:
            regressions.append(
                Regression("throughput_ops_per_s", base_tput, cur_tput, ratio)
            )

    return regressions


def rss_mb() -> float:
    """Peak resident set size of this process, in megabytes.

    `ru_maxrss` is kilobytes on Linux and bytes on macOS; this run targets
    Linux (the CI and dev boxes), so it divides by 1024. Peak rather than
    current because that is the number that decides how much memory the box
    must have for the run to fit.
    """
    return resource.getrusage(resource.RUSAGE_SELF).ru_maxrss / 1024.0


def current_rss_mb() -> float:
    """*Current* resident set size of this process, in megabytes.

    `rss_mb` (peak) only ever rises, so it cannot answer the one question a
    soak is for: did steady-state memory grow while the load stayed flat? This
    reads the live figure from ``/proc/self/statm`` (field 2 is resident
    pages), which is what a leak actually moves. Off Linux — where that file
    is absent — it falls back to the peak, so the caller always gets a number
    and the soak still runs; the growth figure just degenerates to zero there.
    """
    try:
        fields = Path("/proc/self/statm").read_text(encoding="ascii").split()
        resident_pages = int(fields[1])
    except (OSError, IndexError, ValueError):  # pragma: no cover - non-Linux
        return rss_mb()
    page_size = resource.getpagesize()
    return resident_pages * page_size / (1024.0 * 1024.0)


# -- the run: driving real operations through the registry -----------------


def _register_projects(
    registry: OperationRegistry,
    ctx: AppContext,
    recorder: LatencyRecorder,
    plan: DatasetPlan,
    root: Path,
) -> None:
    """Register N projects, each a small on-disk tree a sweep can walk."""
    op = registry.get("project.register")
    for index in range(plan.projects):
        tree = root / f"project-{index:05d}"
        tree.mkdir(parents=True, exist_ok=True)
        # One real manifest so the dependency collector has a file to open;
        # this is what makes the sweep timing a measurement of work rather
        # than of an empty walk.
        (tree / "Cargo.toml").write_text(
            f'[package]\nname = "project-{index:05d}"\n', encoding="utf-8"
        )
        params = RegisterProjectParams(
            name=f"Load Project {index}",
            root_path=str(tree),
            reason="load generator: registering a project (#297)",
        )
        recorder.time("project.register", lambda p=params: op.run(ctx, p))


def _create_work_items(
    registry: OperationRegistry,
    ctx: AppContext,
    recorder: LatencyRecorder,
    plan: DatasetPlan,
) -> list[str]:
    """Create N × M native declared items, returning their refs in order."""
    op = registry.get("work.create")
    refs: list[str] = []
    total = plan.total_work_items
    for index in range(total):
        params = CreateWorkParams(
            kind=_KINDS[index % len(_KINDS)],
            title=f"Load work item {index}",
            body="Generated by scripts/load.py for #297.",
            priority=_PRIORITIES[index % len(_PRIORITIES)],
            reason="load generator: creating a work item (#297)",
        )
        result = recorder.time("work.create", lambda p=params: op.run(ctx, p))
        refs.append(str(result.item.ref))
    return refs


def _relate_work_items(
    registry: OperationRegistry,
    ctx: AppContext,
    recorder: LatencyRecorder,
    refs: Sequence[str],
    plan: DatasetPlan,
    seed: int,
) -> None:
    """Add R declared edges between the created items."""
    op = registry.get("work.relate")
    for source, target, kind in relation_triples(refs, plan.relations, seed=seed):
        params = RelateWorkParams(
            ref=source,
            kind=kind,
            target=target,
            reason="load generator: relating two work items (#297)",
        )
        recorder.time("work.relate", lambda p=params: op.run(ctx, p))


def _time_ranked_views(
    registry: OperationRegistry,
    ctx: AppContext,
    recorder: LatencyRecorder,
    iterations: int,
) -> None:
    """Time the ranked views — the declared+observed merge — at scale."""
    backlog_op = registry.get("backlog")
    bugs_op = registry.get("bugs")
    for _ in range(iterations):
        recorder.time("backlog", lambda: backlog_op.run(ctx, BacklogParams(limit=20)))
        recorder.time("bugs", lambda: bugs_op.run(ctx, BugsParams(limit=50)))


def _sweep(
    registry: OperationRegistry, ctx: AppContext, recorder: LatencyRecorder
) -> None:
    """Run the offline collectors over every registered tree, once."""
    op = registry.get("sweep")
    params = SweepParams(
        offline_only=True, reason="load generator: sweeping the estate (#297)"
    )
    recorder.time("sweep", lambda: op.run(ctx, params))


def run_load(
    plan: DatasetPlan,
    *,
    data_dir: Path,
    seed: int = 0,
    registry: OperationRegistry | None = None,
) -> dict[str, Any]:
    """Build the dataset through the real write path, time it, return a report.

    ``data_dir`` is where the temporary SQLite stores live; the caller owns
    its lifetime (a temp directory it deletes, in the usual case).
    """
    registry = registry or default_registry()
    ctx = build_context(
        config=VogtConfig(data_dir=data_dir, sqlite_synchronous="off"),
        principal=LOAD_PRINCIPAL,
    )
    init_instance(ctx, InitParams())

    recorder = LatencyRecorder()
    trees = data_dir / "trees"
    trees.mkdir(parents=True, exist_ok=True)

    started = time.perf_counter()
    _register_projects(registry, ctx, recorder, plan, trees)
    refs = _create_work_items(registry, ctx, recorder, plan)
    _relate_work_items(registry, ctx, recorder, refs, plan, seed)
    # Ranking before any sweep: the declared-only path.
    _time_ranked_views(registry, ctx, recorder, plan.ranking_iterations)
    _sweep(registry, ctx, recorder)
    # Ranking after the sweep: now the merge has observed evidence to fold in,
    # so these samples exercise the declared+observed join the views perform.
    _time_ranked_views(registry, ctx, recorder, plan.ranking_iterations)
    duration_s = time.perf_counter() - started

    return build_report(
        plan=plan,
        stats=recorder.stats(),
        rss_mb=rss_mb(),
        seed=seed,
        duration_s=duration_s,
        generated_at=datetime.now(UTC),
    )


def _steady_iteration(
    registry: OperationRegistry,
    ctx: AppContext,
    recorder: SoakRecorder,
    refs: list[str],
    index: int,
) -> None:
    """One round of the soak's steady mix: create, mutate, read, list, rank.

    A representative slice of what a live instance does under sustained use —
    weighted to reads, which is where the two-store merge cost lives — so the
    recorded numbers describe steady operation rather than one hot path. The
    new item's ref is appended so the working set grows over the soak, the way
    a real estate's does.
    """
    create = registry.get("work.create")
    update = registry.get("work.update")
    get = registry.get("work.get")
    listing = registry.get("work.list")
    backlog = registry.get("backlog")
    bugs = registry.get("bugs")

    create_params = CreateWorkParams(
        kind=_KINDS[index % len(_KINDS)],
        title=f"Soak item {index}",
        body="Generated by scripts/load.py --mode soak for #297.",
        priority=_PRIORITIES[index % len(_PRIORITIES)],
        reason="soak: creating a work item (#297)",
    )

    def _create_and_track() -> None:
        result = create.run(ctx, create_params)
        refs.append(str(result.item.ref))

    recorder.guard("work.create", _create_and_track)

    target = refs[index % len(refs)]
    recorder.guard(
        "work.update",
        lambda: update.run(
            ctx,
            UpdateWorkParams(
                ref=target,
                priority=_PRIORITIES[index % len(_PRIORITIES)],
                reason="soak: updating a work item (#297)",
            ),
        ),
    )
    recorder.guard("work.get", lambda: get.run(ctx, GetWorkParams(ref=target)))
    recorder.guard("work.list", lambda: listing.run(ctx, ListWorkParams(limit=50)))
    recorder.guard("backlog", lambda: backlog.run(ctx, BacklogParams(limit=20)))
    recorder.guard("bugs", lambda: bugs.run(ctx, BugsParams(limit=50)))


def run_soak(
    plan: SoakPlan,
    *,
    data_dir: Path,
    seed: int = 0,
    produced_by: str = "in-process",
    registry: OperationRegistry | None = None,
) -> dict[str, Any]:
    """Seed a base dataset, then drive a steady mix for N iterations, timed.

    The seed reuses the load run's builders; only the steady phase is timed
    and recorded, so the one-off cost of standing the dataset up does not
    pollute the soak's per-operation numbers. RSS is sampled at the start and
    end of the steady phase — its growth over a flat load is the leak signal
    the soak is really watching.
    """
    registry = registry or default_registry()
    ctx = build_context(
        config=VogtConfig(data_dir=data_dir, sqlite_synchronous="off"),
        principal=LOAD_PRINCIPAL,
    )
    init_instance(ctx, InitParams())

    trees = data_dir / "trees"
    trees.mkdir(parents=True, exist_ok=True)

    # Seed with the load run's builders, but time nothing here.
    seed_recorder = LatencyRecorder()
    _register_projects(registry, ctx, seed_recorder, plan.base, trees)
    refs = _create_work_items(registry, ctx, seed_recorder, plan.base)
    _relate_work_items(registry, ctx, seed_recorder, refs, plan.base, seed)
    _sweep(registry, ctx, seed_recorder)

    recorder = SoakRecorder()
    sweep_op = registry.get("sweep")
    rss_start = current_rss_mb()
    started = time.perf_counter()
    for index in range(plan.iterations):
        _steady_iteration(registry, ctx, recorder, refs, index)
        # The in-process stand-in for the live collector schedule: a sweep on
        # the iteration cadence, driven through the same op `serve` ticks.
        if (index + 1) % plan.sweep_every == 0:
            recorder.guard(
                "sweep",
                lambda: sweep_op.run(
                    ctx,
                    SweepParams(
                        offline_only=True, reason="soak: scheduled sweep (#297)"
                    ),
                ),
            )
    duration_s = time.perf_counter() - started
    rss_end = current_rss_mb()

    return build_soak_report(
        plan=plan,
        stats=recorder.stats(),
        successful_calls=recorder.successful_calls,
        total_errors=recorder.total_errors,
        rss_start_mb=rss_start,
        rss_end_mb=rss_end,
        rss_peak_mb=rss_mb(),
        seed=seed,
        duration_s=duration_s,
        produced_by=produced_by,
        generated_at=datetime.now(UTC),
    )


# -- CLI -------------------------------------------------------------------


def _default_out() -> Path:
    """A temp path, so a default run never writes into the repository."""
    return Path(tempfile.gettempdir()) / "vogt-load-report.json"


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="load",
        description=(
            "Registry-driven load and soak generator for the Vogt core "
            "(#297). `--mode load` records p50/p95 per operation and RSS; "
            "`--mode soak` drives a sustained mix and records throughput, "
            "latency percentiles, error rate and RSS growth into a "
            "recorded-numbers file."
        ),
    )
    parser.add_argument(
        "--mode",
        choices=("load", "soak"),
        default="load",
        help="`load` (default) times a one-shot build; `soak` runs sustained.",
    )
    parser.add_argument(
        "--iterations",
        type=int,
        default=200,
        help="Soak only: rounds of the steady operation mix to drive.",
    )
    parser.add_argument(
        "--sweep-every",
        type=int,
        default=25,
        help=(
            "Soak only: drive an offline sweep every N iterations, the "
            "in-process stand-in for the live collector schedule."
        ),
    )
    parser.add_argument(
        "--produced-by",
        default="in-process",
        help=(
            "Soak only: how the numbers were measured, recorded in the report "
            "(e.g. 'in-process' on a dev box, 'runner' on the self-hosted "
            "runner). A soak number is only comparable to another taken the "
            "same way."
        ),
    )
    parser.add_argument(
        "--check-baseline",
        type=Path,
        default=None,
        help=(
            "Soak only: after the run, compare it to this recorded-numbers "
            "JSON and exit non-zero if any operation's p95 grew past "
            f"{DRIFT_THRESHOLD:g}x, or throughput fell past 1/{DRIFT_THRESHOLD:g}x."
        ),
    )
    parser.add_argument(
        "--scale",
        type=int,
        default=1,
        help=(
            "Single knob sizing the default plan (projects = 5 x scale, "
            "20 items each). Default 1 (small, seconds). Individual --projects "
            "/ --work-items-per-project / --relations override it."
        ),
    )
    parser.add_argument("--projects", type=int, default=None)
    parser.add_argument("--work-items-per-project", type=int, default=None)
    parser.add_argument("--relations", type=int, default=None)
    parser.add_argument("--ranking-iterations", type=int, default=None)
    parser.add_argument(
        "--seed", type=int, default=0, help="RNG seed for the relation edges."
    )
    parser.add_argument(
        "--out",
        type=Path,
        default=None,
        help=(
            "Where to write the JSON report. Defaults to a temp path so it is "
            "never committed."
        ),
    )
    parser.add_argument(
        "--data-dir",
        type=Path,
        default=None,
        help=(
            "Where the temporary SQLite stores live. Defaults to a fresh "
            "temp directory that is removed after the run."
        ),
    )
    return parser


def plan_from_args(args: argparse.Namespace) -> DatasetPlan:
    """Resolve the plan: --scale sets the base, explicit flags override."""
    base = plan_from_scale(args.scale)
    return DatasetPlan(
        projects=args.projects if args.projects is not None else base.projects,
        work_items_per_project=(
            args.work_items_per_project
            if args.work_items_per_project is not None
            else base.work_items_per_project
        ),
        relations=args.relations if args.relations is not None else base.relations,
        ranking_iterations=(
            args.ranking_iterations
            if args.ranking_iterations is not None
            else base.ranking_iterations
        ),
    )


def soak_plan_from_args(args: argparse.Namespace) -> SoakPlan:
    """Resolve the soak plan: --scale/explicit flags size the base dataset."""
    base = plan_from_args(args)
    seeded = DatasetPlan(
        projects=base.projects,
        work_items_per_project=base.work_items_per_project,
        relations=base.relations,
        ranking_iterations=1,
    )
    return SoakPlan(
        base=seeded, iterations=args.iterations, sweep_every=args.sweep_every
    )


def _run_with_data_dir(args: argparse.Namespace, run: Any) -> dict[str, Any]:  # noqa: ANN401
    """Call ``run(data_dir)`` against a caller-supplied or ephemeral data dir."""
    if args.data_dir is not None:
        args.data_dir.mkdir(parents=True, exist_ok=True)
        return run(args.data_dir / "instance")  # type: ignore[no-any-return]
    with tempfile.TemporaryDirectory(prefix="vogt-load-") as tmp:
        return run(Path(tmp) / "instance")  # type: ignore[no-any-return]


def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    out = args.out if args.out is not None else _default_out()

    if args.mode == "soak":
        soak_plan = soak_plan_from_args(args)
        report = _run_with_data_dir(
            args,
            lambda data_dir: run_soak(
                soak_plan,
                data_dir=data_dir,
                seed=args.seed,
                produced_by=args.produced_by,
            ),
        )
    else:
        plan = plan_from_args(args)
        report = _run_with_data_dir(
            args, lambda data_dir: run_load(plan, data_dir=data_dir, seed=args.seed)
        )

    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")

    json.dump(report, sys.stdout, indent=2)
    sys.stdout.write(f"\n\nreport written to {out}\n")

    if args.check_baseline is not None:
        baseline = json.loads(args.check_baseline.read_text(encoding="utf-8"))
        regressions = compare_to_baseline(baseline, report)
        if regressions:
            sys.stdout.write(
                f"\nDRIFT vs {args.check_baseline} (>{DRIFT_THRESHOLD:g}x):\n"
            )
            for regression in regressions:
                sys.stdout.write(f"  {regression.describe()}\n")
            return 1
        sys.stdout.write(f"\nno drift vs {args.check_baseline}\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
