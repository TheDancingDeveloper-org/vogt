"""What became of typed input (WI-918): the rule, then session.input."""

from __future__ import annotations

import dataclasses
import json
from typing import Any

import pytest

from vogt.adapters.engine import EngineClient
from vogt.application.context import AppContext
from vogt.application.models import (
    RegisterProjectParams,
    SessionInputParams,
    StartSessionParams,
)
from vogt.application.services import register_project, session_input, start_session
from vogt.core.delivery import Observation, judge

WHY = "delivery test"
ROOT = "/srv/estate/vogt"


def test_no_enter_is_typed_not_sent() -> None:
    assert judge(submitted=False, before="idle", after=[]).delivery == "typed"


def test_a_queued_hint_or_a_running_turn_means_queued() -> None:
    hint = Observation("running", ("> next", "  Press up to edit queued messages"))
    assert judge(submitted=True, before="idle", after=[hint]).delivery == "queued"
    assert judge(submitted=True, before="running", after=[]).delivery == "queued"


def test_a_turn_starting_after_it_means_delivered() -> None:
    after = [Observation("idle", ()), Observation("running", ())]
    found = judge(submitted=True, before="waiting-for-input", after=after)
    assert found.delivery == "delivered"
    assert "started a turn" in found.evidence


def test_nothing_seen_is_said_as_unconfirmed() -> None:
    found = judge(submitted=True, before="idle", after=[Observation("idle", ())])
    assert found.delivery == "unconfirmed"
    assert "session_screen" in found.evidence


class Engine:
    """Reports `before` until input arrives, then each of `after` in turn."""

    def __init__(self, before: str, after: list[tuple[str, list[str]]]) -> None:
        self.before = before
        self.after = after
        self.inputs = 0
        self.screens = 0

    def __call__(
        self, url: str, headers: dict[str, str], body: bytes = b"", method: str = "GET"
    ) -> tuple[int, bytes]:
        path = url.split("?", 1)[0].removeprefix("http://127.0.0.1:8910")
        engine_id = "00000000-0000-4000-8000-000000000001"
        summary = {"id": engine_id, "name": "x", "activity": self.before, "cwd": ROOT}
        if method == "POST" and path == "/api/sessions":
            return 200, json.dumps(summary).encode()
        if path.endswith("/input"):
            self.inputs += 1
            return 200, b'{"ok":true}'
        if path.endswith("/screen"):
            activity, lines = self.after[min(self.screens, len(self.after) - 1)]
            self.screens += 1
            screen: dict[str, Any] = {
                "id": engine_id,
                "lines": lines,
                "activity": activity,
                "alive": True,
            }
            return 200, json.dumps(screen).encode()
        if method == "GET" and path == f"/api/sessions/{engine_id}":
            return 200, json.dumps({"summary": summary}).encode()
        return 404, b""


def _wired(instance: AppContext, engine: Engine) -> tuple[AppContext, str]:
    ctx = dataclasses.replace(
        instance,
        engine=EngineClient(base_url="http://127.0.0.1:8910", transport=engine),
    )
    register_project(
        ctx, RegisterProjectParams(name="Vogt", root_path=ROOT, reason=WHY)
    )
    started = start_session(ctx, StartSessionParams(project="vogt", reason=WHY))
    return ctx, started.session.id


@pytest.mark.parametrize(
    ("before", "after", "delivery"),
    [
        (
            "waiting-for-input",
            [("idle", ["> "]), ("running", ["✻ Thinking"])],
            "delivered",
        ),
        (
            "idle",
            [("running", ["> next", "Press up to edit queued messages"])],
            "queued",
        ),
        ("running", [("running", [])], "queued"),
        ("idle", [("idle", ["> "])], "unconfirmed"),
    ],
)
def test_session_input_says_what_became_of_it(
    instance: AppContext,
    before: str,
    after: list[tuple[str, list[str]]],
    delivery: str,
) -> None:
    engine = Engine(before, after)
    ctx, ses_id = _wired(instance, engine)
    result = session_input(
        ctx, SessionInputParams(id=ses_id, text="go on", keys=["enter"], reason=WHY)
    )
    assert result.submitted is True, "Enter as a key is a submit"
    assert result.delivery == delivery
    assert result.delivery_evidence


def test_text_without_enter_is_typed_and_not_watched(instance: AppContext) -> None:
    engine = Engine("idle", [("idle", [])])
    ctx, ses_id = _wired(instance, engine)
    result = session_input(ctx, SessionInputParams(id=ses_id, text="draft", reason=WHY))
    assert result.submitted is False
    assert result.delivery == "typed"
    assert engine.screens == 0
