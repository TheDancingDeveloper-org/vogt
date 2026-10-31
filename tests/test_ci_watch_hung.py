"""Hung CI runs on bound branches (WI-1334): a run still queued or in
progress past its threshold is an Inbox `ci.run_hung` entry and one session
notice, and it goes away when the run concludes."""

from __future__ import annotations

import dataclasses
from datetime import UTC, datetime, timedelta

from vogt.adapters.engine import EngineClient
from vogt.application.context import AppContext
from vogt.application.models import BindBranchParams, StartSessionParams
from vogt.application.services import bind_branch, start_session
from vogt.application.services.ci_watch import (
    CI_RUN_HUNG_EVENT,
    announce_concluded,
)
from vogt.collectors.base import Finding, finding
from vogt.core.ci_alerts import HungThresholds, branch_ci, hung_run
from vogt.core.entities import Observation, Project

from tests.conftest import native_work_item
from tests.test_ci_watch import (
    WHY,
    _checks_collector,
    _ci_entries,
    _Forges,
    _InputEngine,
    _project,
    _run,
    _seed,
    _sweep,
    wired,  # noqa: F401  (fixture)
)

NOW = datetime(2026, 10, 31, 4, 0, tzinfo=UTC)
LIMITS = HungThresholds(queued=timedelta(minutes=30), running=timedelta(minutes=45))


def _stamp(minutes_ago: int, now: datetime = NOW) -> str:
    return (now - timedelta(minutes=minutes_ago)).isoformat().replace("+00:00", "Z")


def _obs(payload: dict[str, object]) -> Observation:
    base: dict[str, object] = {
        "revision": "b" * 40,
        "check": "ci",
        "branch": "wi-1",
        "status": "in_progress",
        "conclusion": None,
        "run_id": 77,
        "updated_at": _stamp(1),
    }
    return Observation(
        id="obs-1",
        kind="ci.check",
        subject_key="ci:acme/app@b:ci@wi-1",
        payload={**base, **payload},
        content_digest="d",
        project_id="prj_1",
        sweep_id="swp",
        collector="forge-checks",
        observed_at=NOW,
    )


# -- pure ---------------------------------------------------------------------


def test_a_run_in_progress_for_50_min_is_hung_and_30_is_not() -> None:
    hung = hung_run(_obs({"run_started_at": _stamp(50)}), NOW, LIMITS)
    assert hung is not None
    assert hung.phase == "running" and hung.elapsed_minutes == 50
    assert hung.since_field == "run_started_at" and hung.run_id == 77
    assert hung_run(_obs({"run_started_at": _stamp(30)}), NOW, LIMITS) is None


def test_queued_and_running_have_separate_thresholds() -> None:
    queued = _obs({"status": "queued", "run_started_at": _stamp(35)})
    assert (hung := hung_run(queued, NOW, LIMITS)) is not None
    assert hung.phase == "queued"
    running = _obs({"status": "in_progress", "run_started_at": _stamp(35)})
    assert hung_run(running, NOW, LIMITS) is None
    off = HungThresholds(queued=None, running=timedelta(minutes=45))
    assert hung_run(queued, NOW, off) is None


def test_a_concluded_run_is_never_hung() -> None:
    done = _obs(
        {"status": "completed", "conclusion": "success", "run_started_at": _stamp(500)}
    )
    assert hung_run(done, NOW, LIMITS) is None


def test_without_run_started_at_it_falls_back_to_updated_at() -> None:
    hung = hung_run(_obs({"updated_at": _stamp(60)}), NOW, LIMITS)
    assert hung is not None and hung.since_field == "updated_at"


def test_branch_ci_lists_hung_runs_only_when_given_now() -> None:
    check = _obs({"run_started_at": _stamp(60)})
    assert branch_ci([check], "wi-1") is not None
    assert branch_ci([check], "wi-1").hung == ()  # type: ignore[union-attr]
    ci = branch_ci([check], "wi-1", now=NOW, hung_after=LIMITS)
    assert ci is not None and [h.run_id for h in ci.hung] == [77]


# -- collector ----------------------------------------------------------------


def test_the_collector_records_run_started_at(
    wired: tuple[AppContext, _Forges],  # noqa: F811
) -> None:
    ctx, forges = wired
    run = _run(5, "ci", "main", None)
    run["run_started_at"] = "2026-08-12T03:00:00Z"
    forges.runs = [run, _run(6, "lint", "main", "success")]
    found = {
        f.payload["check"]: f.payload
        for f in _sweep(ctx, _checks_collector(ctx, forges))
        if f.kind == "ci.check"
    }
    assert found["ci"]["run_started_at"] == "2026-08-12T03:00:00Z"
    assert "run_started_at" not in found["lint"]


# -- Inbox and session --------------------------------------------------------


def _check(
    project: Project,
    conclusion: str | None,
    started: str,
    *,
    status: str = "in_progress",
) -> Finding:
    return finding(
        kind="ci.check",
        subject_key=f"ci:acme/app@{'b' * 40}:ci@wi-1",
        project=project,
        source_url="https://github.com/acme/app/actions/runs/4242",
        payload={
            "revision": "b" * 40,
            "check": "ci",
            "conclusion": conclusion,
            "status": "completed" if conclusion else status,
            "branch": "wi-1",
            "event": "push",
            "updated_at": started,
            "run_started_at": started,
            "run_id": 4242,
            "run_attempt": 1,
            "repo": "acme/app",
        },
    )


def test_a_hung_run_is_one_inbox_row_and_one_notice_until_it_concludes(
    wired: tuple[AppContext, _Forges],  # noqa: F811
) -> None:
    ctx, _forges = wired
    engine = _InputEngine()
    ctx = dataclasses.replace(
        ctx, engine=EngineClient(base_url="http://127.0.0.1:8910", transport=engine)
    )
    native_work_item(ctx, title="Watch me", project="app")  # WI-1
    start_session(ctx, StartSessionParams(work_item="WI-1", reason=WHY))
    bind_branch(ctx, BindBranchParams(ref="WI-1", branch="wi-1", reason=WHY))
    project = _project(ctx)
    now = ctx.clock()

    _seed(ctx, [_check(project, None, _stamp(10, now))])
    assert not [e for e in _ci_entries(ctx) if e.kind == "ci.run_hung"]

    _seed(ctx, [_check(project, None, _stamp(120, now))])
    rows = [e for e in _ci_entries(ctx) if e.kind == "ci.run_hung"]
    assert len(rows) == 1
    row = rows[0]
    assert row.work_item_ref == "WI-1"
    assert row.source_url == "https://github.com/acme/app/actions/runs/4242"
    assert "gh run cancel 4242" in row.summary and "120 min" in row.summary

    announce_concluded(ctx)
    announce_concluded(ctx)
    typed = [body["text"] for _, body in engine.inputs if body.get("text")]
    assert len(typed) == 1
    assert typed[0].startswith("[vogt] CI run 4242 (ci)")
    assert "it may be hung" in typed[0]
    with ctx.declared.read() as view:
        events = [
            e
            for e in view.list_events(after=0, limit=500)
            if e.kind == CI_RUN_HUNG_EVENT
        ]
    assert len(events) == 1 and events[0].summary["sessions_notified"] == 1

    _seed(ctx, [_check(project, "success", _stamp(120, now))])
    assert not [e for e in _ci_entries(ctx) if e.kind == "ci.run_hung"]
