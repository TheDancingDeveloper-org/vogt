"""session.sweep: every session at once, most urgent first (WI-915)."""

from __future__ import annotations

import dataclasses
import json
from typing import Any

import pytest

from vogt.adapters.engine import EngineClient
from vogt.application.context import AppContext
from vogt.application.models import (
    RegisterProjectParams,
    StartSessionParams,
    SweepSessionsParams,
)
from vogt.application.services import register_project, start_session, sweep_sessions

WHY = "sweep test"
ROOT = "/srv/estate/vogt"
UNLINKED = "0b0e5f0c-1111-4222-8333-944455556666"


class Engine:
    def __init__(self) -> None:
        self.rows: dict[str, dict[str, Any]] = {}
        self.counter = 0
        self.sweep_supported = True
        self.sweep_queries: list[str] = []

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
            self.counter += 1
            engine_id = f"00000000-0000-4000-8000-00000000000{self.counter}"
            self.rows[engine_id] = self.row(engine_id)
            return 200, json.dumps(self.rows[engine_id]).encode()
        if method == "GET" and path == "/api/sessions":
            return 200, json.dumps(
                {"sessions": list(self.rows.values()), "hidden_count": 0}
            ).encode()
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
    assert engine.sweep_queries == ["screen_lines=5"]
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
    assert all(row.screen_tail == [] for row in result.rows)
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
