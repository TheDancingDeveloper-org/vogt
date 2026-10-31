"""session.sweep: every session at once, most urgent first (WI-915)."""

from __future__ import annotations

import dataclasses
import json
from typing import Any

import pytest

from vogt.adapters.engine import EngineClient
from vogt.application.context import AppContext
from vogt.application.models import (
    ListAuditParams,
    RegisterProjectParams,
    ReportBlockedParams,
    SessionInputParams,
    StartSessionParams,
    SweepSessionsParams,
)
from vogt.application.services import (
    list_audit,
    register_project,
    report_blocked,
    session_input,
    start_session,
    sweep_sessions,
)

WHY = "sweep test"
ROOT = "/srv/estate/vogt"
UNLINKED = "0b0e5f0c-1111-4222-8333-944455556666"


class Engine:
    def __init__(self) -> None:
        self.rows: dict[str, dict[str, Any]] = {}
        self.counter = 0
        self.sweep_supported = True
        self.refuse_starts = False
        self.sweep_queries: list[str] = []
        self.inputs: list[tuple[str, dict[str, Any]]] = []

    def row(self, engine_id: str, **over: Any) -> dict[str, Any]:
        base = {
            "id": engine_id,
            "name": engine_id,
            "activity": "running",
            "alive": True,
            "cwd": ROOT,
            "created_at": "2026-10-05T00:00:00Z",
            "last_output_at": "2026-10-05T00:00:00Z",
        }
        base.update(over)
        return base

    def __call__(
        self, url: str, headers: dict[str, str], body: bytes = b"", method: str = "GET"
    ) -> tuple[int, bytes]:
        path, _, query = url.removeprefix("http://127.0.0.1:8910").partition("?")
        if method == "POST" and path == "/api/sessions":
            if self.refuse_starts:
                return 409, b'{"error": "refused: no capacity"}'
            self.counter += 1
            engine_id = f"00000000-0000-4000-8000-00000000000{self.counter}"
            self.rows[engine_id] = self.row(engine_id)
            return 200, json.dumps(self.rows[engine_id]).encode()
        if method == "GET" and path == "/api/sessions":
            return 200, json.dumps(
                {"sessions": list(self.rows.values()), "hidden_count": 0}
            ).encode()
        if path.startswith("/api/sessions/") and path.count("/") >= 3:
            parts = path.split("/")
            row = self.rows.get(parts[3])
            tail = parts[4] if len(parts) > 4 else ""
            if row is not None and method == "GET" and not tail:
                return 200, json.dumps(row).encode()
            if row is not None and method == "POST" and tail == "blocked":
                req = json.loads(body)
                row["blocked"] = (
                    {
                        "reason": req["reason"],
                        "items": req["items"],
                        "since": "2026-10-18T09:00:00Z",
                    }
                    if req["blocked"]
                    else None
                )
                return 200, json.dumps(row).encode()
            if row is not None and method == "POST" and tail == "input":
                self.inputs.append((parts[3], json.loads(body)))
                return 200, b"{}"
        if method == "GET" and path == "/api/sessions/sweep":
            if not self.sweep_supported:
                return 404, b""
            self.sweep_queries.append(query)
            return 200, json.dumps(
                [
                    {
                        "summary": row,
                        "screen_tail": [f"tail of {row['id']}", "> "],
                        "ready": row["activity"] == "waiting-for-input",
                    }
                    for row in self.rows.values()
                    if row["alive"] or row["activity"] == "hibernated"
                ]
            ).encode()
        return 404, b""


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
    return ctx


def _start(ctx: AppContext) -> tuple[str, str]:
    result = start_session(ctx, StartSessionParams(project="vogt", reason=WHY))
    return result.session.id, result.session.engine_session_id


def test_one_call_returns_every_session_most_urgent_first(
    wired: AppContext, engine: Engine
) -> None:
    busy, busy_engine = _start(wired)
    asking, asking_engine = _start(wired)
    done, done_engine = _start(wired)
    engine.rows[asking_engine].update(
        activity="awaiting-approval",
        approval={
            "question": "Do you want to proceed?",
            "command_excerpt": "rm -rf build",
            "detected_at": "2026-10-05T00:00:00Z",
        },
    )
    engine.rows[done_engine].update(activity="waiting-for-input")
    engine.rows[UNLINKED] = engine.row(UNLINKED, activity="hibernated", alive=False)

    result = sweep_sessions(wired, SweepSessionsParams(mode="full", screen_lines=5))

    sessions = [row.session for row in result.rows]
    assert all(session is not None for session in sessions)
    assert [session.id for session in sessions if session is not None] == [
        asking,
        done,
        busy,
        UNLINKED,
    ]
    assert [row.attention for row in result.rows] == [
        "approval",
        "waiting",
        "running",
        "hibernated",
    ]
    assert "Do you want to proceed?" in result.rows[0].attention_reason
    assert result.rows[0].screen_tail == [f"tail of {asking_engine}", "> "]
    third = result.rows[3].session
    assert third is not None and third.linked is False
    assert result.counts["total"] == 4
    assert result.counts["needs_you"] == 2
    # The sweep reads at least 8 lines to find an input box (WI-1870).
    assert engine.sweep_queries == ["screen_lines=8"]
    assert busy_engine in {
        session.engine_session_id for session in sessions if session is not None
    }


def test_a_stopped_or_vanished_session_is_not_in_the_table(
    wired: AppContext, engine: Engine
) -> None:
    _, gone = _start(wired)
    _, ended = _start(wired)
    del engine.rows[gone]
    engine.rows[ended].update(activity="errored", alive=False, exit_code=1)
    assert sweep_sessions(wired, SweepSessionsParams(mode="full")).rows == []


def test_an_older_engine_still_gets_the_table_without_screens(
    wired: AppContext, engine: Engine
) -> None:
    ses_id, _ = _start(wired)
    engine.sweep_supported = False
    result = sweep_sessions(wired, SweepSessionsParams(mode="full"))
    only = result.rows[0].session
    assert only is not None and only.id == ses_id
    assert result.rows[0].screen_tail == []


def test_no_engine_is_said_not_rendered_as_nothing_running(
    instance: AppContext,
) -> None:
    ctx = dataclasses.replace(instance, engine=None)
    result = sweep_sessions(ctx, SweepSessionsParams())
    assert result.rows == []
    assert result.engine is not None and "VOGT_ENGINE_URL" in result.engine


def test_resources_ride_on_the_row_and_list_can_put_the_heaviest_first(
    wired: AppContext, engine: Engine
) -> None:
    from vogt.application.models import ListSessionsParams
    from vogt.application.services import list_sessions

    light, light_engine = _start(wired)
    heavy, heavy_engine = _start(wired)
    unmeasured, _ = _start(wired)
    sample = {"cpu_pct": 12.5, "processes": 6, "sampled_at": "2026-10-05T00:00:00Z"}
    engine.rows[light_engine]["resources"] = {**sample, "rss_bytes": 300 << 20}
    engine.rows[heavy_engine]["resources"] = {
        **sample,
        "rss_bytes": 50 << 30,
        "over_threshold": True,
    }

    rows = list_sessions(wired, ListSessionsParams(mode="full", order="rss")).sessions
    assert [row.id for row in rows] == [heavy, light, unmeasured]
    assert rows[0].resources is not None
    assert rows[0].resources.rss_bytes == 50 << 30
    assert rows[0].resources.over_threshold is True
    assert rows[2].resources is None

    swept = sweep_sessions(wired, SweepSessionsParams(mode="full"))
    by_id = {
        row.session.id: row.session for row in swept.rows if row.session is not None
    }
    light_resources = by_id[light].resources
    assert light_resources is not None
    assert light_resources.processes == 6


def test_digest_drops_hibernated_keeps_counts_and_fits_fifteen_k(
    wired: AppContext, engine: Engine
) -> None:
    """C1: the default sweep is a digest. Counts include the rows the filter drops."""
    started = [_start(wired) for _ in range(3)]
    live_ids = [session_id for session_id, _engine_id in started]
    _, hibernated_engine = _start(wired)
    engine.rows[hibernated_engine].update(activity="hibernated", alive=False)

    result = sweep_sessions(wired, SweepSessionsParams())

    assert result.counts["total"] == 4
    assert result.counts["hibernated"] == 1
    assert len(result.rows) == 3
    assert {row.digest.id for row in result.rows if row.digest} == set(live_ids)
    assert all(row.session is None for row in result.rows)
    assert all(row.screen_tail is None for row in result.rows)
    assert all(row.ready is None for row in result.rows)
    payload = result.model_dump_json(exclude_none=True)
    assert len(payload) < 15_000
    for row in result.rows:
        assert row.digest is not None
        assert row.digest.over_budget() is False
        # No economics reported yet, so the digest carries no budget line.
        assert row.digest.budget is None

    metered, metered_engine = started[0]
    engine.rows[metered_engine]["economics"] = {"turns": 7, "compactions": 2}
    again = sweep_sessions(wired, SweepSessionsParams())
    by_id = {row.digest.id: row.digest for row in again.rows if row.digest}
    assert by_id[metered].budget == "7 turns, 2 compactions"
    assert all(digest.budget is None for key, digest in by_id.items() if key != metered)


def test_digest_keeps_the_full_blocked_reason(
    wired: AppContext, engine: Engine
) -> None:
    """WI-1670: the head stays, and the guard's full reason rides along."""
    blocked, blocked_engine = _start(wired)
    reason = "the commit guard refuses git add: " + "x" * 200
    engine.rows[blocked_engine].update(
        activity="waiting-for-input",
        blocked={"reason": reason, "items": [], "since": "2026-10-05T00:00:00Z"},
    )
    result = sweep_sessions(wired, SweepSessionsParams())
    row = next(r for r in result.rows if r.digest and r.digest.id == blocked)
    assert row.digest is not None
    assert row.digest.blocked_reason == reason[:120]
    assert row.digest.blocked_detail == reason

    short, short_engine = _start(wired)
    engine.rows[short_engine].update(
        activity="waiting-for-input",
        blocked={
            "reason": "short reason",
            "items": [],
            "since": "2026-10-05T00:00:00Z",
        },
    )
    again = sweep_sessions(wired, SweepSessionsParams())
    row = next(r for r in again.rows if r.digest and r.digest.id == short)
    assert row.digest is not None
    assert row.digest.blocked_reason == "short reason"
    assert row.digest.blocked_detail is None


def test_digest_attention_needs_you_and_limit(
    wired: AppContext, engine: Engine
) -> None:
    asking, asking_engine = _start(wired)
    _start(wired)
    engine.rows[asking_engine].update(
        activity="awaiting-approval",
        approval={"question": "Proceed?", "detected_at": "2026-10-05T00:00:00Z"},
    )
    result = sweep_sessions(
        wired, SweepSessionsParams(attention=["needs_you"], limit=1)
    )
    assert len(result.rows) == 1
    assert result.rows[0].digest is not None
    assert result.rows[0].digest.id == asking
    assert result.counts["total"] == 2
    assert result.counts["needs_you"] == 1


def test_input_clears_a_blocked_report_and_needs_you_drops_it(
    wired: AppContext, engine: Engine
) -> None:
    """WI-1834 acceptance: report blocked, inject input, no longer needs you."""
    worker, worker_engine = _start(wired)
    engine.rows[worker_engine].update(activity="idle")
    report_blocked(
        wired,
        ReportBlockedParams(id=worker, blocker="Infisical returns 500", reason=WHY),
    )
    before = sweep_sessions(wired, SweepSessionsParams(mode="full", screen_lines=0))
    assert before.counts["needs_you"] == 1
    assert before.rows[0].attention == "blocked"
    assert before.rows[0].blocked_since is not None
    assert before.rows[0].blocked_age_s is not None

    session_input(
        wired,
        SessionInputParams(
            id=worker, text="Infisical is fixed, re-run", confirm=False, reason=WHY
        ),
    )

    assert engine.inputs
    assert engine.rows[worker_engine]["blocked"] is None
    after = sweep_sessions(wired, SweepSessionsParams(mode="full", screen_lines=0))
    assert after.counts["needs_you"] == 0
    assert after.rows[0].attention != "blocked"
    assert after.rows[0].blocked_since is None
    # The report it replaced stays in the audit.
    audit = list_audit(
        wired, ListAuditParams(entity_id=worker, operation="session.report_unblocked")
    )
    assert [r.reason for r in audit.records] == [f"cleared by input: {WHY}"]


def test_a_draft_does_not_clear_a_blocked_report(
    wired: AppContext, engine: Engine
) -> None:
    worker, worker_engine = _start(wired)
    report_blocked(
        wired, ReportBlockedParams(id=worker, blocker="needs a go-ahead", reason=WHY)
    )
    session_input(
        wired, SessionInputParams(id=worker, text="half", draft=True, reason=WHY)
    )
    assert engine.rows[worker_engine]["blocked"] is not None


def test_a_report_older_than_the_current_turn_is_stale(
    wired: AppContext, engine: Engine
) -> None:
    """Typed into straight from the terminal, or nudged on: the session went
    back to work after reporting, so the report is stale, not needs-you."""
    _worker, worker_engine = _start(wired)
    _held, held_engine = _start(wired)
    engine.rows[worker_engine].update(
        activity="idle",
        turn_started_at="2026-10-18T10:00:00Z",
        blocked={"reason": "ready: #197", "since": "2026-10-18T09:00:00Z"},
    )
    engine.rows[held_engine].update(
        activity="idle",
        turn_started_at="2026-10-18T08:00:00Z",
        blocked={"reason": "waiting on go-ahead", "since": "2026-10-18T09:00:00Z"},
    )

    result = sweep_sessions(wired, SweepSessionsParams(mode="full", screen_lines=0))

    rows = {row.session.engine_session_id: row for row in result.rows if row.session}
    assert result.counts["needs_you"] == 1
    assert rows[held_engine].attention == "blocked"
    assert not rows[held_engine].blocked_stale
    assert rows[worker_engine].attention != "blocked"
    assert rows[worker_engine].blocked_stale
    assert rows[worker_engine].blocked_since is not None


def test_a_done_declaration_after_the_report_makes_it_stale(
    wired: AppContext, engine: Engine
) -> None:
    """AUTOPILOT: DONE after the report, even in the same turn, ages it out."""
    _worker, worker_engine = _start(wired)
    engine.rows[worker_engine].update(
        activity="idle",
        turn_started_at="2026-10-18T08:00:00Z",
        done_at="2026-10-18T09:30:00Z",
        blocked={"reason": "waiting on go-ahead", "since": "2026-10-18T09:00:00Z"},
    )

    result = sweep_sessions(wired, SweepSessionsParams(mode="full", screen_lines=0))

    assert result.counts["needs_you"] == 0
    assert result.rows[0].blocked_stale
    assert result.rows[0].attention != "blocked"


def test_a_question_menu_is_needs_you_with_its_options_in_the_digest(
    wired: AppContext, engine: Engine
) -> None:
    """WI-1871: the engine reads an AskUserQuestion menu as kind=question."""
    _, asking_engine = _start(wired)
    engine.rows[asking_engine].update(
        activity="awaiting-approval",
        approval={
            "question": "Which approach should the sweep fix take?",
            "command_excerpt": "☐ Approach\nWhich approach should the sweep fix take?",
            "kind": "question",
            "options": [
                {"number": 1, "label": "Narrow fix", "selected": True},
                {"number": 2, "label": "Full rewrite"},
            ],
            "detected_at": "2026-10-18T00:00:00Z",
        },
    )

    result = sweep_sessions(wired, SweepSessionsParams(attention=["needs_you"]))

    assert result.counts["needs_you"] == 1
    (row,) = result.rows
    assert row.attention == "approval"
    assert row.attention_reason.startswith("asking a question")
    assert row.digest is not None
    assert row.digest.question == "Which approach should the sweep fix take?"
    assert row.digest.question_kind == "question"
    assert row.digest.options == ["Narrow fix", "Full rewrite"]


def test_a_bypass_lost_on_resume_is_said_on_the_sweep_and_the_list(
    wired: AppContext, engine: Engine
) -> None:
    """WI-2252: a session woken without the bypass it showed before a restart
    carries the engine's flag on its list row, and its sweep row says so."""
    from vogt.application.models import ListSessionsParams
    from vogt.application.services import list_sessions

    lost, lost_engine = _start(wired)
    kept, _kept_engine = _start(wired)
    engine.rows[lost_engine].update(posture_lost_on_resume=True)

    rows = {
        row.id: row
        for row in list_sessions(wired, ListSessionsParams(mode="full")).sessions
    }
    assert rows[lost].posture_lost_on_resume is True
    assert rows[kept].posture_lost_on_resume is False

    result = sweep_sessions(wired, SweepSessionsParams(mode="full", screen_lines=0))
    reasons = {
        row.session.id: row.attention_reason
        for row in result.rows
        if row.session is not None
    }
    assert "posture lost on resume" in reasons[lost]
    assert "posture lost on resume" not in reasons[kept]


def test_digest_drops_operator_sessions_and_empty_fields_wi1525(
    wired: AppContext, engine: Engine
) -> None:
    """WI-1525: a session Vogt never linked is the operator's own, off the
    default digest; `exclude_roles=[]` brings it back, and full mode (the
    GUI's) never drops it. A digest row carries no empty `screen_tail` and no
    row-level `ready` beside `digest.ready`, and stays under 300 characters."""
    session_id, _engine_id = _start(wired)
    engine.rows[UNLINKED] = engine.row(UNLINKED)

    default = sweep_sessions(wired, SweepSessionsParams())
    assert [row.digest.id for row in default.rows if row.digest] == [session_id]
    assert default.counts["total"] == 1
    row = default.rows[0]
    assert row.screen_tail is None and row.ready is None
    assert len(row.model_dump_json(exclude_none=True)) <= 300

    every = sweep_sessions(wired, SweepSessionsParams(exclude_roles=[]))
    assert {row.digest.id for row in every.rows if row.digest} == {
        session_id,
        UNLINKED,
    }
    full = sweep_sessions(wired, SweepSessionsParams(mode="full"))
    assert len(full.rows) == 2
    assert all(isinstance(row.screen_tail, list) for row in full.rows)

    workers = sweep_sessions(wired, SweepSessionsParams(exclude_roles=["worker"]))
    assert [row.digest.id for row in workers.rows if row.digest] == [UNLINKED]


def test_supervised_by_keeps_only_sessions_that_starter_started_wi1525(
    wired: AppContext, engine: Engine
) -> None:
    _start(wired)
    swept = sweep_sessions(
        wired, SweepSessionsParams(supervised_by="11111111-2222-4333-8444-555566667777")
    )
    assert swept.rows == []
    assert swept.counts["total"] == 0


def test_mcp_write_results_are_compact_unless_verbose_wi1525(
    wired: AppContext, engine: Engine
) -> None:
    """WI-1525: start/hibernate/wake/stop/bind tool text is the digest row,
    under 600 characters; `verbose: true` keeps the full result."""
    from vogt.adapters.mcp.compact import COMPACTORS, text_payload
    from vogt.registry.registry import default_registry

    started = start_session(
        wired, StartSessionParams(project="vogt", reason=WHY)
    ).model_dump(mode="json")
    for operation in (
        "session.start",
        "session.hibernate",
        "session.wake",
        "session.stop",
    ):
        compact = text_payload(operation, {}, started)
        assert set(compact) == {"digest"}
        assert compact["digest"]["id"] == started["session"]["id"]
        assert len(json.dumps(compact, separators=(",", ":"))) <= 600
        assert text_payload(operation, {"verbose": True}, started) == started
    stopped = {**started, "digest": None}
    assert (
        text_payload("session.stop", {}, stopped)["digest"]["id"]
        == (started["session"]["id"])
    )
    bound = {
        "session": started["session"],
        "previous_work_item": None,
        "project_mismatch": False,
        "engine_label": "written",
    }
    compact = text_payload("session.bind_work", {}, bound)
    assert compact["engine_label"] == "written"
    assert compact["digest"]["id"] == started["session"]["id"]
    assert "session" not in compact

    registry = default_registry()
    for operation in COMPACTORS:
        schema = registry.get(operation).params_model.model_json_schema()
        assert "verbose" in schema["properties"], operation


def test_since_cursor_returns_only_changed_rows_wi1525(
    wired: AppContext, engine: Engine
) -> None:
    """WI-1525: each sweep returns a cursor; passing it as `since` returns only
    rows whose state changed, with the rest counted as `unchanged` and dropped
    sessions in `gone`. Counts still cover every session."""
    first_id, first_engine = _start(wired)
    second_id, second_engine = _start(wired)

    base = sweep_sessions(wired, SweepSessionsParams())
    assert base.cursor and base.cursor_reset is None and base.unchanged is None
    assert len(base.rows) == 2

    quiet = sweep_sessions(wired, SweepSessionsParams(since=base.cursor))
    assert quiet.rows == []
    assert quiet.unchanged == 2 and quiet.gone == []
    assert quiet.counts["total"] == 2

    engine.rows[first_engine]["activity"] = "waiting-for-input"
    engine.rows[second_engine]["alive"] = False
    moved = sweep_sessions(wired, SweepSessionsParams(since=quiet.cursor))
    assert [row.digest.id for row in moved.rows if row.digest] == [first_id]
    assert moved.unchanged == 0
    assert moved.gone == [second_engine]
    assert second_id not in moved.model_dump_json()

    full = sweep_sessions(wired, SweepSessionsParams(mode="full", since=moved.cursor))
    assert full.rows == [] and full.unchanged == 1


@pytest.mark.parametrize(
    "cursor",
    ["garbage", "sw1.!!!", "sw1." + "A" * 12],
)
def test_an_invalid_cursor_returns_every_row_flagged_wi1525(
    wired: AppContext, engine: Engine, cursor: str
) -> None:
    _start(wired)
    swept = sweep_sessions(wired, SweepSessionsParams(since=cursor))
    assert swept.cursor_reset is True
    assert len(swept.rows) == 1
    assert swept.cursor and swept.cursor != cursor


def test_an_expired_cursor_returns_every_row_flagged_wi1525(
    wired: AppContext, engine: Engine
) -> None:
    from datetime import timedelta

    from vogt.application.services.sessions import _encode_cursor

    _start(wired)
    old = _encode_cursor(wired.clock() - timedelta(days=2), {})
    swept = sweep_sessions(wired, SweepSessionsParams(since=old))
    assert swept.cursor_reset is True and len(swept.rows) == 1
