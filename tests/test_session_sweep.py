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
            return 200, json.dumps(list(self.rows.values())).encode()
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

    result = sweep_sessions(wired, SweepSessionsParams(screen_lines=5))

    assert [row.session.id for row in result.rows] == [asking, done, busy, UNLINKED]
    assert [row.attention for row in result.rows] == [
        "approval",
        "waiting",
        "running",
        "hibernated",
    ]
    assert "Do you want to proceed?" in result.rows[0].attention_reason
    assert result.rows[0].screen_tail == [f"tail of {asking_engine}", "> "]
    assert result.rows[3].session.linked is False
    assert result.counts["total"] == 4
    assert result.counts["needs_you"] == 2
    assert engine.sweep_queries == ["screen_lines=5"]
    assert busy_engine in {row.session.engine_session_id for row in result.rows}


def test_a_stopped_or_vanished_session_is_not_in_the_table(
    wired: AppContext, engine: Engine
) -> None:
    _, gone = _start(wired)
    _, ended = _start(wired)
    del engine.rows[gone]
    engine.rows[ended].update(activity="errored", alive=False, exit_code=1)
    assert sweep_sessions(wired, SweepSessionsParams()).rows == []


def test_an_older_engine_still_gets_the_table_without_screens(
    wired: AppContext, engine: Engine
) -> None:
    ses_id, _ = _start(wired)
    engine.sweep_supported = False
    result = sweep_sessions(wired, SweepSessionsParams())
    assert [row.session.id for row in result.rows] == [ses_id]
    assert result.rows[0].screen_tail == []


def test_no_engine_is_said_not_rendered_as_nothing_running(
    instance: AppContext,
) -> None:
    ctx = dataclasses.replace(instance, engine=None)
    result = sweep_sessions(ctx, SweepSessionsParams())
    assert result.rows == []
    assert result.engine is not None and "VOGT_ENGINE_URL" in result.engine
