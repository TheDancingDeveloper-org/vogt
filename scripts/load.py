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

DEFERRED to later increments of #297 (not built here): the S-hour soak with
the live scheduler, the K concurrent WebSocket attach clients (that needs the
Rust engine), and the nightly CI job that commits to a `vogt/bench` results
branch with a >2x regression alert. See the issue.

The numbers this prints are local/dev-box measurements, not production SLAs.

Usage::

    uv run python scripts/load.py --scale 1 --out /tmp/load.json
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
    InitParams,
    RegisterProjectParams,
    RelateWorkParams,
    SweepParams,
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


def rss_mb() -> float:
    """Peak resident set size of this process, in megabytes.

    `ru_maxrss` is kilobytes on Linux and bytes on macOS; this run targets
    Linux (the CI and dev boxes), so it divides by 1024. Peak rather than
    current because that is the number that decides how much memory the box
    must have for the run to fit.
    """
    return resource.getrusage(resource.RUSAGE_SELF).ru_maxrss / 1024.0


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


# -- CLI -------------------------------------------------------------------


def _default_out() -> Path:
    """A temp path, so a default run never writes into the repository."""
    return Path(tempfile.gettempdir()) / "vogt-load-report.json"


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="load",
        description=(
            "Registry-driven load and timing generator for the Vogt core "
            "(#297, increment 1). Records p50/p95 per operation and RSS."
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


def main(argv: Sequence[str] | None = None) -> int:
    args = build_parser().parse_args(argv)
    plan = plan_from_args(args)
    out = args.out if args.out is not None else _default_out()

    if args.data_dir is not None:
        args.data_dir.mkdir(parents=True, exist_ok=True)
        report = run_load(plan, data_dir=args.data_dir / "instance", seed=args.seed)
    else:
        with tempfile.TemporaryDirectory(prefix="vogt-load-") as tmp:
            report = run_load(plan, data_dir=Path(tmp) / "instance", seed=args.seed)

    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")

    json.dump(report, sys.stdout, indent=2)
    sys.stdout.write(f"\n\nreport written to {out}\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
