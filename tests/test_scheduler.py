"""The in-process collector schedule (FR-L3).

FR-L3 has two halves — "on an in-process schedule" and "triggerable on
demand" — and only the second was built. `DEPLOYMENT.md` §1 has listed
"collector scheduler (in-process background sweeps)" in the `serve` process
diagram since M4, so the deployment document described something that did not
run. Caught by walking every must-have requirement ID and asking which are
cited nowhere in `src/` or `tests/`.

The behaviour under test is mostly about what happens when things go wrong: a
schedule that stops on the first failure, or that stacks sweeps on top of each
other, is worse than no schedule at all.
"""

from __future__ import annotations

import asyncio
from collections.abc import Coroutine
from pathlib import Path
from typing import Any, TypeVar

import pytest
from fastapi.testclient import TestClient

from vogt.adapters.http.scheduler import (
    SCHEDULED_REASON,
    CollectorSchedule,
    scheduled,
)
from vogt.adapters.http.server import ServeOptions, build_server
from vogt.application.context import AppContext
from vogt.application.models import RegisterProjectParams
from vogt.application.services import register_project
from vogt.config import VogtConfig

WHY = "scheduler test"

T = TypeVar("T")


def task(schedule: CollectorSchedule) -> object | None:
    """The schedule's background task, read opaquely.

    Through a function so the type checker cannot narrow `_task` across a
    `with` block and then declare the assertion after it unreachable — the
    whole point is that the value changes while the server is up.
    """
    return schedule._task


def run(coro: Coroutine[Any, Any, T]) -> T:
    """Drive one coroutine to completion.

    Rather than adding `pytest-asyncio` or `anyio`'s plugin as a dev
    dependency for six tests: the schedule's contract is about what it does
    across a loop's lifetime, and `asyncio.run` gives exactly one loop per
    test with no shared state between them.
    """
    return asyncio.run(coro)


@pytest.fixture
def project(instance: AppContext, tmp_path: Path) -> AppContext:
    register_project(
        instance,
        RegisterProjectParams(
            name="Vogt", root_path=str(tmp_path / "vogt"), reason=WHY
        ),
    )
    return instance


# -- it runs, and keeps running --------------------------------------------


async def _test_the_schedule_sweeps_without_anyone_asking(project: AppContext) -> None:
    schedule = CollectorSchedule(lambda: project, interval_seconds=3600)
    await schedule.run_once()

    assert schedule.completed == 1
    assert project.observed.coverage(), (
        "a scheduled sweep must record coverage like any other — otherwise "
        "freshness cannot tell that anything looked"
    )


async def _test_a_failing_sweep_does_not_stop_the_schedule(
    project: AppContext, monkeypatch: pytest.MonkeyPatch
) -> None:
    """One bad collector must not turn into permanent silence.

    This is the failure the schedule exists to prevent, arriving by the back
    door: if an exception killed the loop, the instance would stop looking and
    every view would go on rendering as though it had.
    """
    calls = {"n": 0}

    def sometimes_broken() -> None:
        calls["n"] += 1
        if calls["n"] == 1:
            raise RuntimeError("a collector fell over")

    schedule = CollectorSchedule(lambda: project, interval_seconds=3600)
    monkeypatch.setattr(schedule, "_sweep", sometimes_broken)

    await schedule.run_once()
    await schedule.run_once()

    assert schedule.failed == 1
    assert schedule.completed == 1, "the loop survived and swept again"


async def _test_sweeps_do_not_overlap(project: AppContext) -> None:
    """The interval is a gap between sweeps, not a fixed drumbeat.

    A sweep is a writer. If the timer fired regardless of whether the previous
    sweep had finished, a slow estate would run concurrent writers against the
    same store — and the symptom would be lock contention under exactly the
    load that caused it.
    """
    running = 0
    peak = 0

    def slow() -> None:
        nonlocal running, peak
        running += 1
        peak = max(peak, running)
        # Long enough that a second tick would start if the loop let it.
        import time

        time.sleep(0.05)
        running -= 1

    schedule = CollectorSchedule(lambda: project, interval_seconds=0)
    object.__setattr__(schedule, "_interval", 0.01)
    schedule._sweep = slow  # type: ignore[method-assign]

    async with scheduled(schedule):
        await asyncio.sleep(0.25)

    assert schedule.completed >= 2, "the loop ticked more than once"
    assert peak == 1, f"{peak} sweeps ran at once; they must not overlap"


async def _test_stopping_waits_rather_than_leaving_a_sweep_half_done(
    project: AppContext,
) -> None:
    schedule = CollectorSchedule(lambda: project, interval_seconds=3600)
    async with scheduled(schedule):
        await asyncio.sleep(0.05)
    assert task(schedule) is None, "the task was awaited, not abandoned"


async def _test_shutdown_does_not_wait_out_the_interval(project: AppContext) -> None:
    """Sleeping on the stop event, not on the clock.

    With a fifteen-minute default, a schedule that slept on the clock would
    hold the process open for up to fifteen minutes after SIGTERM — long past
    any compose stop-grace period, so the container would be killed instead.
    The timeout here is what makes the assertion real: it is far shorter than
    the interval, so a clock-sleeping loop fails rather than passes slowly.
    """
    schedule = CollectorSchedule(lambda: project, interval_seconds=900)
    async with asyncio.timeout(5):
        async with scheduled(schedule):
            await asyncio.sleep(0.05)
    assert task(schedule) is None


# -- provenance -------------------------------------------------------------


def test_a_scheduled_sweep_runs_as_the_machine(instance: AppContext) -> None:
    """Not as whoever happened to start the process.

    A sweep writes evidence rather than declared rows, so today it leaves no
    audit record to inspect — the attribution lives in the context the
    schedule is handed. Asserted at the wiring, because that is where it is
    decided, and because the moment a collector does write a declared row the
    principal is already correct.
    """
    app = build_server(serve_options(require_auth=False), config=instance.config)
    schedule = app.state.collector_schedule
    ctx = schedule._context_factory()

    assert ctx.principal is not None
    assert ctx.principal.kind == "agent"
    assert "service:" in ctx.principal.identity_ref, (
        f"a scheduled sweep runs as {ctx.principal.identity_ref}, which names a person"
    )


def test_the_scheduled_reason_says_what_it_was() -> None:
    """Every write carries a reason (FR-W1), including ones nobody typed."""
    assert SCHEDULED_REASON.strip()
    assert "schedul" in SCHEDULED_REASON.lower()


# -- wiring -----------------------------------------------------------------


def serve_options(**overrides: object) -> ServeOptions:
    return ServeOptions(host="127.0.0.1", port=18999, **overrides)  # type: ignore[arg-type]


def test_serve_carries_a_schedule(instance: AppContext) -> None:
    app = build_server(serve_options(require_auth=False), config=instance.config)
    schedule = app.state.collector_schedule
    assert schedule.enabled, "the default is to keep looking"


def test_no_schedule_turns_it_off(instance: AppContext) -> None:
    app = build_server(
        serve_options(require_auth=False, schedule_collectors=False),
        config=instance.config,
    )
    assert not app.state.collector_schedule.enabled


def test_a_zero_interval_turns_it_off(data_dir: Path) -> None:
    """Configuration can disable it too, without a command-line flag."""
    config = VogtConfig(data_dir=data_dir, sweep_interval_seconds=0)
    app = build_server(serve_options(require_auth=False), config=config)
    assert not app.state.collector_schedule.enabled


def test_the_schedule_runs_for_exactly_the_life_of_the_server(
    project: AppContext,
) -> None:
    """Started by the lifespan, stopped by it — never outliving the listener.

    A background writer that survives shutdown is how a container that has
    "stopped" goes on writing to a database another process is restoring.
    """
    config = project.config.model_copy(update={"sweep_interval_seconds": 3600})
    app = build_server(serve_options(require_auth=False), config=config)
    schedule = app.state.collector_schedule

    assert task(schedule) is None, "nothing runs before the server starts"
    with TestClient(app) as client:
        assert client.get("/health/live").status_code == 200
        assert task(schedule) is not None, "the lifespan started it"
    assert task(schedule) is None, "the lifespan stopped it"


def test_building_the_app_alone_starts_no_background_writer(
    instance: AppContext,
) -> None:
    """A library caller must not acquire a writer by building an app.

    `build_app` takes no schedule; only `serve` supplies a lifespan. Tests,
    the loopback surface and anything embedding the API get a plain
    application that writes only when asked.
    """
    from vogt.adapters.http.app import build_app

    app = build_app(context_factory=lambda: instance)
    assert not hasattr(app.state, "collector_schedule")


# -- each coroutine above, driven on its own loop ---------------------------


def test_the_schedule_sweeps_without_anyone_asking(project: AppContext) -> None:
    run(_test_the_schedule_sweeps_without_anyone_asking(project=project))


def test_a_failing_sweep_does_not_stop_the_schedule(
    project: AppContext, monkeypatch: pytest.MonkeyPatch
) -> None:
    run(
        _test_a_failing_sweep_does_not_stop_the_schedule(
            project=project, monkeypatch=monkeypatch
        )
    )


def test_sweeps_do_not_overlap(project: AppContext) -> None:
    run(_test_sweeps_do_not_overlap(project=project))


def test_stopping_waits_rather_than_leaving_a_sweep_half_done(
    project: AppContext,
) -> None:
    run(_test_stopping_waits_rather_than_leaving_a_sweep_half_done(project=project))


def test_shutdown_does_not_wait_out_the_interval(project: AppContext) -> None:
    run(_test_shutdown_does_not_wait_out_the_interval(project=project))
