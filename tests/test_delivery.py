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
from vogt.core.delivery import Observation, composer_holds, judge

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


#: A resumed Codex after an Enter it swallowed: the status line reads as
#: working, but the text is still after the composer's prompt (WI-1669).
CODEX_RESUMED_HELD = (
    "\u203a fix the flaky retry test",
    "",
    "\u2022 Ran cargo test -p vogt-engine",
    "  \u2514 ok",
    "",
    "\u2022 Working (2s \u2022 esc to interrupt)",
    "",
    "\u203a review the WI-1537 branch and report blockers",
    "",
    "  gpt-5-codex high \u00b7 92% context left",
)

#: The same screen once the submit landed: the text moved into the
#: transcript and the composer shows only its placeholder.
CODEX_RESUMED_CLEARED = (
    "\u203a fix the flaky retry test",
    "",
    "\u203a review the WI-1537 branch and report blockers",
    "",
    "\u2022 Working (1s \u2022 esc to interrupt)",
    "",
    "\u203a Ask Codex to do anything",
    "",
    "  gpt-5-codex high \u00b7 92% context left",
)

SENT = "review the WI-1537 branch and report blockers"


def test_a_codex_composer_still_holding_the_text_is_unconfirmed() -> None:
    held = [Observation("running", CODEX_RESUMED_HELD)]
    found = judge(submitted=True, before="waiting-for-input", after=held, pending=SENT)
    assert found.delivery == "unconfirmed"
    assert "composer" in found.evidence


def test_a_cleared_codex_composer_with_a_turn_is_delivered() -> None:
    cleared = [Observation("running", CODEX_RESUMED_CLEARED)]
    found = judge(
        submitted=True, before="waiting-for-input", after=cleared, pending=SENT
    )
    assert found.delivery == "delivered"


def test_only_the_latest_read_decides_the_composer() -> None:
    after = [
        Observation("running", CODEX_RESUMED_HELD),
        Observation("running", CODEX_RESUMED_CLEARED),
    ]
    found = judge(submitted=True, before="idle", after=after, pending=SENT)
    assert found.delivery == "delivered"


def test_the_composer_is_ignored_without_pending_text() -> None:
    held = [Observation("running", CODEX_RESUMED_HELD)]
    assert judge(submitted=True, before="idle", after=held).delivery == "delivered"


def test_composer_holds_reads_the_last_prompt_line() -> None:
    assert composer_holds(CODEX_RESUMED_HELD, SENT)
    assert not composer_holds(CODEX_RESUMED_CLEARED, SENT)
    assert not composer_holds(CODEX_RESUMED_HELD, "")
    assert not composer_holds(("> " + SENT,), SENT), "Claude's prompt is not Codex's"
    long = "x" * 120
    wrapped = ("\u203a " + "x" * 70, "  " + "x" * 50)
    assert composer_holds(wrapped, long)


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
    assert engine.inputs == 2, "the text and the Enter key, no second Enter"
    assert result.delivery == delivery
    assert result.delivery_evidence


def test_text_without_enter_is_typed_and_not_watched(instance: AppContext) -> None:
    engine = Engine("idle", [("idle", [])])
    ctx, ses_id = _wired(instance, engine)
    result = session_input(
        ctx, SessionInputParams(id=ses_id, text="draft", draft=True, reason=WHY)
    )
    assert result.submitted is False
    assert result.delivery == "typed"
    assert engine.screens == 0
