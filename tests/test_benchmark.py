"""The scale tripwire (NFR-S1, NFR-S4).

The two-store split means every aggregate view is an application-layer join,
and that cost needs a tripwire now rather than a discovery at M6. This seeds
a fixture at the v1 envelope and asserts the interactive target against it.

It is deliberately a *tripwire*, not a benchmark suite: it fails when a query
becomes order-of-magnitude slower, and it does not pretend a wall-clock
number measured on a shared CI runner is a performance metric. The threshold
is generous for that reason — what matters is that inserting an accidental
per-item query into a ranked view stops being invisible.
"""

from __future__ import annotations

import time
from datetime import UTC, datetime, timedelta
from pathlib import Path

import pytest

from vogt.application.context import AppContext, build_context
from vogt.application.models import BacklogParams, BugsParams, InitParams
from vogt.application.services import backlog, bugs, init_instance
from vogt.config import VogtConfig
from vogt.core.entities import Priority, Project, WorkItem, WorkKind
from vogt.core.principal import Principal

#: The NFR-S1 envelope is ~500 projects and ~100k work items. Seeding 100k
#: rows through the audited write path would take minutes and prove nothing
#: about the *query*, so the fixture writes them directly and scales down the
#: item count to what a per-run tripwire can afford. The shape being measured
#: — one wide read plus an application-layer join and scoring pass — is the
#: same at either size.
BENCHMARK_PROJECTS = 500
BENCHMARK_ITEMS = 5_000

#: NFR-S1 asks for interactive (< 1 s) queries. The margin absorbs a loaded
#: shared runner; an accidental N+1 blows straight past it.
INTERACTIVE_BUDGET_SECONDS = 3.0

NOW = datetime(2026, 8, 12, 12, 0, 0, tzinfo=UTC)
PRINCIPAL = Principal(
    identity_ref="local:benchmark", kind="human", display_name="benchmark"
)


def _seed(context: AppContext) -> None:
    """Write the envelope directly, bypassing the audited write path."""
    init_instance(context, InitParams())
    priorities: tuple[Priority, ...] = ("p0", "p1", "p2", "p3", "p4")
    kinds: tuple[WorkKind, ...] = ("bug", "feature", "chore", "question")

    with context.declared.write() as txn:
        projects = []
        for index in range(BENCHMARK_PROJECTS):
            project = Project(
                id=f"prj_{index:05d}",
                slug=f"project-{index:05d}",
                name=f"Project {index}",
                root_path=f"/srv/project-{index:05d}",
                created_at=NOW,
                updated_at=NOW,
            )
            txn.insert_project(project)
            projects.append(project)

        for index in range(BENCHMARK_ITEMS):
            project = projects[index % BENCHMARK_PROJECTS]
            txn.insert_work_item(
                WorkItem(
                    id=f"wrk_{index:06d}",
                    ref=f"WI-{index}",
                    kind=kinds[index % len(kinds)],
                    title=f"Seeded work item {index}",
                    state="open",
                    priority=priorities[index % len(priorities)],
                    project_id=project.id,
                    created_at=NOW,
                    updated_at=NOW - timedelta(days=index % 90),
                )
            )


@pytest.fixture(scope="module")
def seeded(tmp_path_factory: pytest.TempPathFactory) -> AppContext:
    root = tmp_path_factory.mktemp("benchmark")
    context = build_context(
        config=VogtConfig(data_dir=root / "instance"), principal=PRINCIPAL
    )
    _seed(context)
    return context


def _elapsed(call: object) -> float:
    started = time.perf_counter()
    call()  # type: ignore[operator]
    return time.perf_counter() - started


def test_the_fixture_is_at_the_envelope(seeded: AppContext) -> None:
    with seeded.declared.read() as view:
        counts = view.counts()
    assert counts.projects == BENCHMARK_PROJECTS
    assert counts.work_items == BENCHMARK_ITEMS


def test_the_global_backlog_stays_interactive(seeded: AppContext) -> None:
    elapsed = _elapsed(lambda: backlog(seeded, BacklogParams(limit=20)))
    assert elapsed < INTERACTIVE_BUDGET_SECONDS, (
        f"global backlog took {elapsed:.2f}s at "
        f"{BENCHMARK_ITEMS} items across {BENCHMARK_PROJECTS} projects"
    )


def test_the_global_bug_view_stays_interactive(seeded: AppContext) -> None:
    elapsed = _elapsed(lambda: bugs(seeded, BugsParams(limit=50)))
    assert elapsed < INTERACTIVE_BUDGET_SECONDS, f"global bugs took {elapsed:.2f}s"


def test_a_per_project_backlog_stays_interactive(seeded: AppContext) -> None:
    elapsed = _elapsed(
        lambda: backlog(seeded, BacklogParams(project="project-00042", limit=20))
    )
    assert elapsed < INTERACTIVE_BUDGET_SECONDS, (
        f"per-project backlog took {elapsed:.2f}s"
    )


def test_the_backlog_is_capped_rather_than_unbounded(seeded: AppContext) -> None:
    """The ranked views read a bounded slice, by design.

    Scoring happens in Python, so an uncapped read at the envelope would mean
    scoring every item in the estate to show twenty. The cap is what keeps
    that honest — and what this tripwire is really watching.
    """
    from vogt.application.services.views import RANKING_CANDIDATE_LIMIT

    result = backlog(seeded, BacklogParams(limit=20))
    assert len(result.items) == 20
    assert result.total_considered <= RANKING_CANDIDATE_LIMIT


def test_the_declared_store_stays_a_single_file(seeded: AppContext) -> None:
    """NFR-PO3: zero external services, one volume, one file per store."""
    data_dir = Path(seeded.config.resolved_data_dir)
    assert (data_dir / "declared.sqlite3").is_file()
    assert (data_dir / "observed.sqlite3").is_file()
