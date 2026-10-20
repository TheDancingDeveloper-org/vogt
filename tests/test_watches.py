"""Watches: hibernate until an event fires, then wake with the outcome (WI-2036).

The scenario these pin is the one that burned the most model turns: a worker
pushed, then polled CI and the PR in a loop. With a watch it hibernates with
`wake_on`, and the core's watch loop wakes it — through `session_input`,
which resumes a hibernated session — when the trigger fires, even across a
restart of the core.
"""

from __future__ import annotations

import asyncio
import dataclasses
from datetime import timedelta
from typing import Any

import pytest

from vogt.adapters.engine import EngineClient
from vogt.adapters.http.scheduler import WatchSchedule
from vogt.application.context import AppContext, build_context
from vogt.application.models import (
    CancelWatchParams,
    CreateWatchParams,
    HibernateSessionParams,
    ListWatchesParams,
    RegisterProjectParams,
    StartSessionParams,
    TransitionWorkParams,
    WatchTriggerModel,
)
from vogt.application.services import (
    cancel_watch,
    create_watch,
    hibernate_session,
    list_watches,
    register_project,
    sessions,
    start_session,
    transition_work,
)
from vogt.application.services.watches import MAX_ATTEMPTS, evaluate_watches
from vogt.collectors.base import Finding, finding
from vogt.core import watches as rules
from vogt.core.entities import Observation, Project
from vogt.errors import GrantRefused, InvalidRequest

from tests.conftest import StepClock, native_work_item
from tests.test_session_hibernation import ROOT, Engine

WHY = "watch test"
HEAD = "c0ffee" + "0" * 34


@pytest.fixture(autouse=True)
def _instant_confirm(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(sessions, "_CONFIRM_READS", 2)
    monkeypatch.setattr(sessions, "_CONFIRM_PAUSE_S", 0)


@pytest.fixture
def engine() -> Engine:
    return Engine()


@pytest.fixture
def wired(instance: AppContext, engine: Engine) -> AppContext:
    ctx = dataclasses.replace(
        instance,
        engine=EngineClient(base_url="http://127.0.0.1:8910", transport=engine),
    )
    register_project(
        ctx,
        RegisterProjectParams(
            name="Vogt",
            root_path=ROOT,
            repo_url="https://github.com/acme/vogt",
            reason=WHY,
        ),
    )
    return ctx


def _started(ctx: AppContext) -> tuple[str, str]:
    result = start_session(
        ctx, StartSessionParams(project="vogt", template="claude", reason=WHY)
    )
    return result.session.id, result.session.engine_session_id


def _project(ctx: AppContext) -> Project:
    with ctx.declared.read() as view:
        project = view.project_by_slug("vogt")
    assert project is not None
    return project


def _seed(ctx: AppContext, findings: list[Finding]) -> None:
    project = _project(ctx)
    now = ctx.clock()
    row = ctx.observed.begin_sweep(collector="forge-prs", scope=[project.id], at=now)
    ctx.observed.append(row.id, findings, at=now)
    ctx.observed.finish_sweep(row.id, outcome="ok", stats={"projects": 1}, at=now)
    ctx.observed.rebuild_latest()


def _pull(ctx: AppContext, state: str, number: int = 42) -> Finding:
    return finding(
        kind="forge.pull_request",
        subject_key=f"gh:acme/vogt#{number}",
        project=_project(ctx),
        source_url=f"https://github.com/acme/vogt/pull/{number}",
        payload={
            "number": number,
            "title": "Watch triggers",
            "state": state,
            "head": HEAD,
            "repo": "acme/vogt",
        },
    )


def _check(ctx: AppContext, name: str, conclusion: str | None) -> Finding:
    return finding(
        kind="ci.check",
        subject_key=f"ci:acme/vogt@{HEAD}:{name}@wi-2036",
        project=_project(ctx),
        source_url=f"https://github.com/acme/vogt/actions/runs/{name}",
        payload={
            "revision": HEAD,
            "check": name,
            "conclusion": conclusion,
            "status": "completed" if conclusion else "in_progress",
            "branch": "wi-2036",
            "event": "pull_request",
            "updated_at": "2026-10-20T01:00:00Z",
        },
    )


def _typed(engine: Engine, engine_id: str) -> list[str]:
    return [
        row["body"]["text"]
        for row in engine.sent
        if row["path"] == f"/api/sessions/{engine_id}/input" and row["body"].get("text")
    ]


def _pr_trigger(**extra: Any) -> WatchTriggerModel:
    return WatchTriggerModel(kind="pr", project="vogt", number=42, **extra)


# -- the acceptance scenario -------------------------------------------------


def test_a_worker_hibernates_until_its_pr_merges_and_wakes_with_the_outcome(
    wired: AppContext, engine: Engine
) -> None:
    ses_id, engine_id = _started(wired)
    result = hibernate_session(
        wired,
        HibernateSessionParams(
            id=ses_id, wake_on=_pr_trigger(on=["merged"]), reason=WHY
        ),
    )
    assert engine_id in engine.hibernated
    assert result.session.activity == "hibernated"
    [armed] = list_watches(wired, ListWatchesParams()).watches
    assert armed.state == "active"
    assert armed.session == engine_id

    _seed(wired, [_pull(wired, "open")])
    assert evaluate_watches(wired).fired == []
    assert engine_id in engine.hibernated, "nothing fired: it stays asleep"

    _seed(wired, [_pull(wired, "merged")])
    done = evaluate_watches(wired)

    assert done.fired == [armed.id]
    assert engine_id in engine.live, "woken by the delivery"
    [notice] = _typed(engine, engine_id)
    assert notice.startswith("[vogt] watch fired: PR #42 in vogt -> MERGED")
    [fired] = list_watches(wired, ListWatchesParams()).watches
    assert fired.state == "fired"
    assert fired.delivered is True
    assert fired.outcome is not None
    assert fired.outcome["url"] == "https://github.com/acme/vogt/pull/42"
    # Once: a later pass types nothing more.
    assert evaluate_watches(wired).fired == []
    assert len(_typed(engine, engine_id)) == 1


def test_a_watch_survives_a_restart_of_the_core(
    wired: AppContext, engine: Engine
) -> None:
    ses_id, engine_id = _started(wired)
    hibernate_session(
        wired, HibernateSessionParams(id=ses_id, wake_on=_pr_trigger(), reason=WHY)
    )
    _seed(wired, [_pull(wired, "closed")])

    # A new process: a fresh context over the same database, nothing carried
    # over in memory but the engine it talks to.
    restarted = build_context(
        config=wired.config,
        principal=wired.principal,
        clock=StepClock(),
        engine=wired.engine,
    )
    done = evaluate_watches(restarted)

    assert len(done.fired) == 1
    assert engine_id in engine.live
    assert _typed(engine, engine_id)[0].endswith("-> CLOSED (Watch triggers)")


def test_a_hibernate_that_fails_disarms_its_watch(
    wired: AppContext, engine: Engine
) -> None:
    ses_id, _engine_id = _started(wired)
    engine.refuse_hibernate = "mid-turn"
    with pytest.raises(Exception, match="mid-turn"):
        hibernate_session(
            wired, HibernateSessionParams(id=ses_id, wake_on=_pr_trigger(), reason=WHY)
        )
    [watch] = list_watches(wired, ListWatchesParams()).watches
    assert watch.state == "cancelled"


# -- each trigger kind -------------------------------------------------------


def test_ci_on_a_pr_waits_for_the_required_checks_only(
    wired: AppContext, engine: Engine
) -> None:
    ses_id, engine_id = _started(wired)
    create_watch(
        wired,
        CreateWatchParams(
            trigger=WatchTriggerModel(kind="ci", project="vogt", pr=42, checks=["ci"]),
            session=ses_id,
            note="then hand off",
            reason=WHY,
        ),
    )
    _seed(
        wired,
        [_pull(wired, "open"), _check(wired, "ci", None), _check(wired, "e2e", None)],
    )
    assert evaluate_watches(wired).fired == [], "the required check is running"

    _seed(wired, [_check(wired, "ci", "success"), _check(wired, "e2e", None)])
    assert len(evaluate_watches(wired).fired) == 1, "e2e is not required"
    [notice] = _typed(engine, engine_id)
    assert "CI on PR #42 in vogt -> CI PASSED @ c0ffee000000; 1 check(s)" in notice
    assert notice.endswith(". then hand off")


def test_ci_on_a_sha_reports_the_failing_checks(
    wired: AppContext, engine: Engine
) -> None:
    ses_id, engine_id = _started(wired)
    create_watch(
        wired,
        CreateWatchParams(
            trigger=WatchTriggerModel(
                kind="ci", project="vogt", sha=HEAD[:12], checks=["ci", "lint"]
            ),
            session=ses_id,
            reason=WHY,
        ),
    )
    _seed(wired, [_check(wired, "ci", "failure"), _check(wired, "lint", "success")])
    assert len(evaluate_watches(wired).fired) == 1
    [notice] = _typed(engine, engine_id)
    assert "CI FAILED" in notice
    assert "failing: ci" in notice
    assert "https://github.com/acme/vogt/actions/runs/ci" in notice


def test_a_work_item_reaching_a_state_fires(wired: AppContext, engine: Engine) -> None:
    ses_id, engine_id = _started(wired)
    native_work_item(wired, title="Blocker", project="vogt")  # WI-1
    create_watch(
        wired,
        CreateWatchParams(
            trigger=WatchTriggerModel(
                kind="work_item", work_item="WI-1", states=["in_progress"]
            ),
            session=ses_id,
            reason=WHY,
        ),
    )
    assert evaluate_watches(wired).fired == []
    transition_work(
        wired, TransitionWorkParams(ref="WI-1", to_state="in_progress", reason=WHY)
    )
    assert len(evaluate_watches(wired).fired) == 1
    assert _typed(engine, engine_id) == [
        "[vogt] watch fired: WI-1 state -> IN_PROGRESS"
    ]


def test_another_session_ending_fires(wired: AppContext, engine: Engine) -> None:
    overseer, overseer_engine = _started(wired)
    worker, worker_engine = _started(wired)
    create_watch(
        wired,
        CreateWatchParams(
            trigger=WatchTriggerModel(kind="session", session=worker),
            session=overseer,
            reason=WHY,
        ),
    )
    assert evaluate_watches(wired).fired == []
    hibernate_session(wired, HibernateSessionParams(id=worker, reason=WHY))
    assert len(evaluate_watches(wired).fired) == 1
    [notice] = _typed(engine, overseer_engine)
    assert notice == f"[vogt] watch fired: session {worker} -> HIBERNATED"
    assert _typed(engine, worker_engine) == []


def test_a_session_cannot_wait_for_itself(wired: AppContext) -> None:
    ses_id, _ = _started(wired)
    with pytest.raises(InvalidRequest, match="itself"):
        create_watch(
            wired,
            CreateWatchParams(
                trigger=WatchTriggerModel(kind="session", session=ses_id),
                session=ses_id,
                reason=WHY,
            ),
        )


def test_a_trigger_names_the_fields_its_kind_needs() -> None:
    with pytest.raises(ValueError, match="project and number"):
        WatchTriggerModel(kind="pr", project="vogt")
    with pytest.raises(ValueError, match="exactly one of sha or pr"):
        WatchTriggerModel(kind="ci", project="vogt", sha=HEAD, pr=1, checks=["ci"])
    with pytest.raises(ValueError, match="needs checks"):
        WatchTriggerModel(kind="ci", project="vogt", sha=HEAD)


# -- lifecycle ---------------------------------------------------------------


def test_cancel_expiry_and_a_refused_delivery(
    wired: AppContext, engine: Engine
) -> None:
    ses_id, engine_id = _started(wired)
    cancelled = create_watch(
        wired, CreateWatchParams(trigger=_pr_trigger(), session=ses_id, reason=WHY)
    ).watch
    cancel_watch(wired, CancelWatchParams(id=cancelled.id, reason=WHY))
    lapsing = create_watch(
        wired,
        CreateWatchParams(
            trigger=_pr_trigger(), session=ses_id, ttl_seconds=60, reason=WHY
        ),
    ).watch
    stuck = create_watch(
        wired, CreateWatchParams(trigger=_pr_trigger(), session=ses_id, reason=WHY)
    ).watch
    hibernate_session(wired, HibernateSessionParams(id=ses_id, reason=WHY))
    engine.refuse_wake = "superseded"
    _seed(wired, [_pull(wired, "merged")])

    later = dataclasses.replace(wired, clock=lambda: wired.clock() + timedelta(hours=1))
    first = evaluate_watches(later)
    assert first.expired == [lapsing.id]
    assert first.retrying == [stuck.id]
    # Backoff: a pass before the retry is due neither retries nor blocks.
    assert evaluate_watches(later).retrying == []
    for attempt in range(1, MAX_ATTEMPTS):
        # Past each doubling delay (1, 2, 4, 8 min after the hour).
        offset = timedelta(hours=1, minutes=2**attempt * 2)
        evaluate_watches(
            dataclasses.replace(wired, clock=lambda o=offset: wired.clock() + o)
        )

    by_id = {w.id: w for w in list_watches(wired, ListWatchesParams()).watches}
    assert by_id[cancelled.id].state == "cancelled"
    assert by_id[lapsing.id].state == "expired"
    assert by_id[stuck.id].state == "failed"
    assert by_id[stuck.id].delivered is False
    assert by_id[stuck.id].attempts == MAX_ATTEMPTS
    assert "superseded" in (by_id[stuck.id].last_error or "")
    assert engine_id in engine.hibernated, "a refused wake is never forced"


def test_only_the_owner_or_the_woken_session_cancels(wired: AppContext) -> None:
    ses_id, _ = _started(wired)
    watch = create_watch(
        wired, CreateWatchParams(trigger=_pr_trigger(), session=ses_id, reason=WHY)
    ).watch
    from vogt.core.principal import Principal

    stranger = dataclasses.replace(
        wired,
        principal=Principal(
            identity_ref="agent:engine:somebody-else", kind="agent", display_name="x"
        ),
    )
    with pytest.raises(GrantRefused):
        cancel_watch(stranger, CancelWatchParams(id=watch.id, reason=WHY))


def test_the_watch_schedule_runs_a_pass(wired: AppContext, engine: Engine) -> None:
    ses_id, engine_id = _started(wired)
    create_watch(
        wired, CreateWatchParams(trigger=_pr_trigger(), session=ses_id, reason=WHY)
    )
    _seed(wired, [_pull(wired, "merged")])
    schedule = WatchSchedule(lambda: wired, interval_seconds=60)
    asyncio.run(schedule.run_once())
    assert schedule.completed == 1
    assert len(_typed(engine, engine_id)) == 1


# -- the pure rules ----------------------------------------------------------


def _obs(name: str, conclusion: str | None, revision: str = HEAD) -> Observation:
    from datetime import UTC, datetime

    return Observation(
        id=f"obs-{name}",
        kind="ci.check",
        subject_key=f"ci:{revision}:{name}",
        payload={"revision": revision, "check": name, "conclusion": conclusion},
        content_digest="d",
        project_id="prj_1",
        sweep_id="swp",
        collector="forge-checks",
        observed_at=datetime(2026, 10, 20, tzinfo=UTC),
    )


def test_ci_outcome_needs_every_required_check_observed() -> None:
    assert rules.ci_outcome([], HEAD) is None
    assert rules.ci_outcome([_obs("ci", "success")], HEAD, ["ci", "e2e"]) is None
    other = [_obs("ci", "success", revision="d" * 40)]
    assert rules.ci_outcome(other, HEAD) is None
    cancelled = rules.ci_outcome([_obs("ci", "cancelled")], HEAD)
    assert cancelled is not None and cancelled["state"] == "cancelled"


def test_the_notice_cannot_carry_a_keystroke() -> None:
    text = rules.notice_text(
        "pr",
        {"number": 1, "project": "vogt"},
        {"state": "merged", "title": "evil\x1b[2J\rtitle"},
        "",
    )
    assert "\x1b" not in text and "\r" not in text
