"""session.report: a session's own typed report (WI-2039)."""

from __future__ import annotations

import dataclasses
from datetime import timedelta

import pytest

from vogt.adapters.engine import EngineClient
from vogt.application.context import AppContext
from vogt.application.models import (
    CommentParams,
    CreateWorkParams,
    GetWorkParams,
    InboxListParams,
    ListEventsParams,
    RegisterProjectParams,
    SessionDigestParams,
    SessionReportParams,
    StartSessionParams,
    StopSessionParams,
    SweepSessionsParams,
)
from vogt.application.services import (
    comment_work,
    create_work,
    digest,
    get_work,
    list_events,
    list_inbox,
    register_project,
    report_session,
    start_session,
    stop_session,
    sweep_sessions,
)
from vogt.core.entities import HandoffFields
from vogt.core.oversight import DONE_WITHOUT_REPORT, result_reported
from vogt.core.principal import Principal
from vogt.errors import InvalidRequest, ReportRefused

from tests.test_session_sweep import ROOT, Engine

WHY = "report test"


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
        ctx, RegisterProjectParams(name="Vogt", root_path=ROOT, reason=WHY)
    )
    create_work(
        ctx,
        CreateWorkParams(
            kind="feature", title="Reports", project="vogt", local_only=True, reason=WHY
        ),
    )
    return ctx


def _start(ctx: AppContext) -> tuple[str, str]:
    result = start_session(ctx, StartSessionParams(work_item="WI-1", reason=WHY))
    return result.session.id, result.session.engine_session_id


def _as_session(ctx: AppContext, session_id: str) -> AppContext:
    return dataclasses.replace(
        ctx,
        principal=Principal(
            identity_ref=f"agent:session:{session_id}", kind="agent", display_name="w"
        ),
    )


def _result(**over: object) -> SessionReportParams:
    fields: dict[str, object] = {
        "kind": "result",
        "summary": "pushed wi-1-reports, gates green",
        "branch": "wi-1-reports",
        "sha": "0123456789abcdef0123",
        "pr": "#163",
        "checks": ["pytest: 12 passed", "ruff: clean"],
        "next_step": "review",
        "reason": WHY,
    }
    fields.update(over)
    return SessionReportParams.model_validate(fields)


def test_a_session_reports_for_itself_onto_its_bound_item(wired: AppContext) -> None:
    session_id, engine_id = _start(wired)

    result = report_session(_as_session(wired, session_id), _result())

    report = result.report
    assert report.kind == "result"
    assert report.session_id == engine_id
    assert report.work_item == "WI-1"
    assert report.pr == "#163"
    assert report.checks == ["pytest: 12 passed", "ruff: clean"]
    item = get_work(wired, GetWorkParams(ref="WI-1"))
    bodies = [comment.body for comment in item.comments]
    assert any(
        body.startswith("Session report (result): pushed wi-1-reports")
        and "pr: #163" in body
        for body in bodies
    )


def test_a_session_may_not_report_as_another(wired: AppContext) -> None:
    worker, _ = _start(wired)
    _other, other_engine = _start(wired)

    with pytest.raises(ReportRefused):
        report_session(_as_session(wired, worker), _result(id=other_engine))


def test_a_caller_with_no_session_must_name_one(wired: AppContext) -> None:
    with pytest.raises(InvalidRequest, match="give `id`"):
        report_session(wired, _result())


def test_the_latest_report_rides_on_the_sweep_digest_instead_of_the_excerpt(
    wired: AppContext, engine: Engine
) -> None:
    session_id, engine_id = _start(wired)
    agent = _as_session(wired, session_id)
    report_session(agent, _result(kind="progress", summary="tests written"))
    report_session(agent, _result())

    full = sweep_sessions(wired, SweepSessionsParams(mode="full", screen_lines=0))
    [row] = full.rows
    assert row.session is not None
    assert row.session.latest_report is not None
    assert row.session.latest_report.kind == "result"

    table = sweep_sessions(wired, SweepSessionsParams(mode="digest"))
    [line] = table.rows
    assert line.digest is not None
    assert line.digest.report is not None
    assert line.digest.report.startswith("result: pushed wi-1-reports")
    assert "pr=#163" in line.digest.report
    assert line.digest.last_reply_excerpt is None

    one = digest(wired, SessionDigestParams(id=engine_id))
    assert one.latest_report is not None
    assert one.latest_report.summary == "pushed wi-1-reports, gates green"


def test_the_inbox_shows_the_newest_report_per_session(wired: AppContext) -> None:
    session_id, engine_id = _start(wired)
    agent = _as_session(wired, session_id)
    report_session(agent, _result(kind="progress", summary="halfway"))
    report_session(agent, _result(kind="question", summary="which base?"))

    entries = [
        entry
        for entry in list_inbox(wired, InboxListParams()).entries
        if entry.kind.startswith("session.report.")
    ]

    assert [entry.kind for entry in entries] == ["session.report.question"]
    assert entries[0].session_id == engine_id
    assert entries[0].work_item_ref == "WI-1"
    assert "which base?" in entries[0].summary


def _done(engine: Engine, engine_id: str) -> None:
    engine.rows[engine_id].update(
        activity="waiting-for-input",
        turn_started_at="2026-10-18T08:00:00Z",
        done_at="2026-10-18T09:30:00Z",
    )


def test_done_without_a_result_report_is_waiting_when_the_gate_is_on(
    wired: AppContext, engine: Engine
) -> None:
    _session, engine_id = _start(wired)
    _done(engine, engine_id)
    gated = dataclasses.replace(
        wired,
        config=wired.config.model_copy(
            update={"autopilot_done_requires_result_report": True}
        ),
    )

    [row] = sweep_sessions(gated, SweepSessionsParams(mode="full")).rows
    assert row.attention == "waiting"
    assert row.attention_reason == DONE_WITHOUT_REPORT

    # Off by default: the same session reads done.
    [row] = sweep_sessions(wired, SweepSessionsParams(mode="full")).rows
    assert row.attention == "done"


def test_a_result_this_turn_satisfies_the_gate_and_an_older_one_does_not() -> None:
    from datetime import UTC, datetime

    turn = datetime(2026, 10, 18, 8, tzinfo=UTC)
    after = datetime(2026, 10, 18, 9, tzinfo=UTC)
    before = datetime(2026, 10, 18, 7, tzinfo=UTC)
    assert result_reported(report_kind="result", report_at=after, turn_started_at=turn)
    assert not result_reported(
        report_kind="result", report_at=before, turn_started_at=turn
    )
    assert not result_reported(
        report_kind="progress", report_at=after, turn_started_at=turn
    )
    assert result_reported(report_kind="result", report_at=after, turn_started_at=None)


def test_credentials_in_a_report_are_redacted_everywhere(wired: AppContext) -> None:
    session_id, _ = _start(wired)
    token = "ghp_" + "A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8"
    result = report_session(
        _as_session(wired, session_id),
        _result(
            summary=f"pushed with {token}",
            blockers=[f"GITHUB_TOKEN={token} was denied"],
        ),
    )

    stored = result.report.model_dump_json()
    assert token not in stored
    assert result.report.sha == "0123456789abcdef0123"
    item = get_work(wired, GetWorkParams(ref="WI-1"))
    assert all(token not in comment.body for comment in item.comments)
    inbox = list_inbox(wired, InboxListParams()).entries
    assert all(token not in entry.model_dump_json() for entry in inbox)


def test_a_stopped_or_old_sessions_report_leaves_the_inbox(
    wired: AppContext,
) -> None:
    session_id, _ = _start(wired)
    report_session(_as_session(wired, session_id), _result())

    def kinds(ctx: AppContext) -> list[str]:
        return [
            e.kind
            for e in list_inbox(ctx, InboxListParams()).entries
            if e.kind.startswith("session.report.")
        ]

    assert kinds(wired) == ["session.report.result"]
    later = dataclasses.replace(wired, clock=lambda: wired.clock() + timedelta(days=8))
    assert kinds(later) == []

    stop_session(wired, StopSessionParams(id=session_id, reason=WHY))
    assert kinds(wired) == []


# -- an unbound autopilot worker gets a chore (WI-1338) ---------------


def _unbound(ctx: AppContext, **over: object) -> StartSessionParams:
    fields: dict[str, object] = {
        "project": "vogt",
        "autopilot": True,
        "task": "Tidy the release notes\nand nothing else",
        "reason": WHY,
    }
    fields.update(over)
    return StartSessionParams.model_validate(fields)


def test_an_unbound_autopilot_worker_gets_a_local_chore_it_can_hand_off_on(
    wired: AppContext, engine: Engine
) -> None:
    result = start_session(wired, _unbound(wired))

    ref = result.session.work_item
    assert ref == "WI-2"
    item = get_work(wired, GetWorkParams(ref=ref)).item
    assert item.kind == "chore"
    assert item.priority == "p3"
    assert item.title == "Session: Tidy the release notes"
    assert item.body.startswith("Tidy the release notes\nand nothing else")
    assert item.project_slug == "vogt"
    created = [
        e
        for e in list_events(wired, ListEventsParams()).events
        if e.kind == "work.created" and e.entity_id == item.id
    ]
    assert created
    comment_work(
        _as_session(wired, result.session.id),
        CommentParams(
            ref=ref,
            body="done",
            kind="handoff",
            handoff=HandoffFields(done=["notes"]),
            reason=WHY,
        ),
    )


def test_the_chore_is_titled_from_the_session_name(wired: AppContext) -> None:
    result = start_session(wired, _unbound(wired, name="notes-pass"))
    item = get_work(wired, GetWorkParams(ref=result.session.work_item or "")).item
    assert item.title == "Session: notes-pass"


@pytest.mark.parametrize(
    "over",
    [
        {"autopilot": False},
        {"purpose": "review", "autopilot": True},
        {"purpose": "investigate", "autopilot": True},
    ],
)
def test_no_chore_without_autopilot_or_for_a_reading_purpose(
    wired: AppContext, over: dict[str, object]
) -> None:
    result = start_session(wired, _unbound(wired, **over))
    assert result.session.work_item is None


def test_no_chore_when_an_item_is_named(wired: AppContext) -> None:
    result = start_session(wired, _unbound(wired, project=None, work_item="WI-1"))
    assert result.session.work_item == "WI-1"
    with pytest.raises(Exception):  # noqa: B017 - only WI-1 exists
        get_work(wired, GetWorkParams(ref="WI-2"))


def test_a_resume_reuses_the_open_chore(wired: AppContext) -> None:
    first = start_session(wired, _unbound(wired))
    again = start_session(wired, _unbound(wired, resume="conv-1"))
    assert again.session.work_item == first.session.work_item


def test_a_result_report_closes_the_chore(wired: AppContext) -> None:
    result = start_session(wired, _unbound(wired))
    report_session(_as_session(wired, result.session.id), _result())
    item = get_work(wired, GetWorkParams(ref=result.session.work_item or "")).item
    assert item.state == "done"


def test_a_result_report_leaves_a_filed_item_open(wired: AppContext) -> None:
    session_id, _ = _start(wired)
    report_session(_as_session(wired, session_id), _result())
    assert get_work(wired, GetWorkParams(ref="WI-1")).item.state != "done"


def test_a_progress_report_leaves_the_chore_open(wired: AppContext) -> None:
    result = start_session(wired, _unbound(wired))
    report_session(_as_session(wired, result.session.id), _result(kind="progress"))
    item = get_work(wired, GetWorkParams(ref=result.session.work_item or "")).item
    assert item.state != "done"
