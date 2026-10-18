"""What became of typed input (WI-918): the rule, then session.input."""

from __future__ import annotations

import dataclasses
import json
import urllib.parse
from pathlib import Path
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
from vogt.core.delivery import (
    Observation,
    claude_composer,
    codex_composer,
    composer_holds,
    input_pending,
    judge,
    safe_to_resubmit,
)

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


#: Codex screens captured live on 2026-10-16 (WI-1669); the dialog is the
#: one synthetic screen, as its file says.
CODEX = {
    path.stem: json.loads(path.read_text())
    for path in (Path(__file__).parent / "fixtures" / "codex_screens").glob("*.json")
}
#: The text the live captures hold in the composer.
SENT = "reply with the single word two"


def _seen(name: str) -> Observation:
    shot = CODEX[name]
    return Observation(shot["activity"], tuple(shot["lines"]))


def test_the_composer_is_the_prompt_line_above_the_footer() -> None:
    assert codex_composer(_seen("resumed_swallowed").lines) == SENT
    assert codex_composer(_seen("resumed_stuck_ready").lines) == SENT
    assert codex_composer(_seen("resumed_cleared").lines) == "Ask Codex to do anything"
    assert codex_composer(_seen("approval_dialog").lines) is None, "no composer drawn"


def test_a_transcript_copy_of_the_text_is_not_a_held_composer() -> None:
    """After the submit the text is still on screen, as a transcript line."""
    cleared = _seen("resumed_cleared")
    assert "\u203a reply with the single word ok" in cleared.lines
    assert not composer_holds(cleared.lines, "reply with the single word ok")


def test_a_swallowed_enter_reading_as_working_is_unconfirmed() -> None:
    """Live: `running`, yet the text never left the composer."""
    found = judge(
        submitted=True,
        before="waiting-for-input",
        after=[_seen("resumed_swallowed")],
        pending=SENT,
    )
    assert found.delivery == "unconfirmed"
    assert "composer" in found.evidence


def test_a_cleared_composer_with_a_turn_is_delivered() -> None:
    found = judge(
        submitted=True,
        before="waiting-for-input",
        after=[_seen("resumed_swallowed"), _seen("resumed_cleared")],
        pending=SENT,
    )
    assert found.delivery == "delivered"


def test_the_composer_is_ignored_without_pending_text() -> None:
    after = [_seen("resumed_swallowed")]
    assert judge(submitted=True, before="idle", after=after).delivery == "delivered"


def test_another_enter_only_on_a_composer_still_holding_the_text() -> None:
    assert safe_to_resubmit(_seen("resumed_stuck_ready"), SENT)
    assert safe_to_resubmit(_seen("resumed_swallowed"), SENT)
    assert not safe_to_resubmit(_seen("resumed_cleared"), SENT)
    assert not safe_to_resubmit(_seen("approval_dialog"), SENT)
    assert not safe_to_resubmit(Observation("idle", ("> " + SENT, "")), SENT), (
        "no Codex composer drawn"
    )


def test_a_wrapped_composer_still_holds_a_long_text() -> None:
    long = "x" * 120
    lines = (
        "\u203a " + "x" * 70,
        "  " + "x" * 50,
        "",
        "  grok high \u00b7 ~/w",
        "  ? for shortcuts",
    )
    assert composer_holds(lines, long)


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


#: Claude Code screens captured live on 2026-10-18 (WI-1870): a message
#: queued behind a running turn, the box after the turn took it, and the
#: box still holding text at rest — the stuck shape.
CLAUDE = {
    path.stem: json.loads(path.read_text())
    for path in (Path(__file__).parent / "fixtures" / "claude_screens").glob("*.json")
}


def _claude(name: str) -> Observation:
    screen = CLAUDE[name]
    return Observation(screen["activity"], tuple(screen["lines"]))


@pytest.mark.parametrize("name", sorted(CLAUDE))
def test_the_claude_input_box_holds_only_what_is_unsent(name: str) -> None:
    screen = CLAUDE[name]
    seen = _claude(name)
    assert composer_holds(seen.lines, screen["text"]) is screen["held"]
    assert (input_pending(seen.lines) is not None) is screen["held"]


def test_the_claude_box_is_read_between_its_rules() -> None:
    assert claude_composer(_claude("held_idle").lines) == (
        "reply with the single word stuck-ok"
    )
    assert claude_composer(_claude("drained_idle").lines) == ""
    assert claude_composer(_claude("queued_behind_turn").lines) == "", (
        "the queued-messages hint is not text in the box"
    )
    assert claude_composer(("● Done.", "> hello")) is None, "no box drawn"


def test_text_left_in_the_box_at_rest_is_stuck_not_queued() -> None:
    """WI-1870: sent behind a turn, the turn ended, the text never left."""
    found = judge(
        submitted=True,
        before="running",
        after=[_claude("held_idle")],
        pending=CLAUDE["held_idle"]["text"],
    )
    assert found.delivery == "stuck"
    assert "input box" in found.evidence


def test_a_message_queued_behind_a_turn_is_still_queued() -> None:
    found = judge(
        submitted=True,
        before="running",
        after=[_claude("queued_behind_turn")],
        pending=CLAUDE["queued_behind_turn"]["text"],
    )
    assert found.delivery == "queued"


class QueuedEngine:
    """A Claude session running a turn as the input arrives; the turn then
    ends with the text left in the box. Enters after the first clear it
    only once `takes_after` of them have been pressed."""

    ID = "00000000-0000-4000-8000-000000000001"

    def __init__(self, takes_after: int, timeouts: int = 0) -> None:
        self.takes_after = takes_after
        self.timeouts = timeouts
        self.enters = 0
        self.waits = 0
        self.wait_timeouts: list[int] = []

    def _screen(self) -> tuple[str, list[str]]:
        if self.enters == 0:
            return "running", CLAUDE["queued_behind_turn"]["lines"]
        if self.enters - 1 < self.takes_after:
            return "idle", CLAUDE["held_idle"]["lines"]
        return "running", CLAUDE["drained_idle"]["lines"]

    def __call__(
        self, url: str, headers: dict[str, str], body: bytes = b"", method: str = "GET"
    ) -> tuple[int, bytes]:
        path = url.split("?", 1)[0].removeprefix("http://127.0.0.1:8910")
        activity, lines = self._screen()
        summary = {"id": self.ID, "name": "x", "activity": activity, "cwd": ROOT}
        if method == "POST" and path == "/api/sessions":
            return 200, json.dumps(summary).encode()
        if path.endswith("/input"):
            if json.loads(body or b"{}").get("submit"):
                self.enters += 1
            return 200, b'{"ok":true}'
        if path.endswith("/wait"):
            self.waits += 1
            query = urllib.parse.parse_qs(url.split("?", 1)[1])
            self.wait_timeouts.append(int(query["timeout_s"][0]))
            if self.waits <= self.timeouts:
                return 200, json.dumps({"outcome": "timeout"}).encode()
            return 200, json.dumps({"outcome": "ready", "matched": True}).encode()
        if path.endswith("/screen"):
            screen = {"id": self.ID, "lines": lines, "activity": activity}
            return 200, json.dumps({**screen, "alive": True}).encode()
        if method == "GET" and path == f"/api/sessions/{self.ID}":
            return 200, json.dumps({"summary": summary}).encode()
        return 404, b""


def _wired_queued(instance: AppContext, engine: QueuedEngine) -> tuple[AppContext, str]:
    ctx = dataclasses.replace(
        instance,
        engine=EngineClient(base_url="http://127.0.0.1:8910", transport=engine),
    )
    register_project(
        ctx, RegisterProjectParams(name="Vogt", root_path=ROOT, reason=WHY)
    )
    started = start_session(ctx, StartSessionParams(project="vogt", reason=WHY))
    return ctx, started.session.id


def test_a_queued_input_left_in_the_box_gets_an_enter(instance: AppContext) -> None:
    """WI-1870: once the turn ends with the text still in the box, the
    follow-up presses Enter and the agent takes it."""
    engine = QueuedEngine(takes_after=1)
    ctx, ses_id = _wired_queued(instance, engine)
    result = session_input(
        ctx,
        SessionInputParams(id=ses_id, text=CLAUDE["held_idle"]["text"], reason=WHY),
    )
    assert result.delivery == "queued"
    assert engine.waits == 1, "it waited for the turn to end"
    assert engine.enters == 2, "the submit, then one follow-up Enter"


def test_a_queued_input_the_cli_took_gets_no_extra_enter(
    instance: AppContext,
) -> None:
    engine = QueuedEngine(takes_after=0)
    ctx, ses_id = _wired_queued(instance, engine)
    session_input(
        ctx, SessionInputParams(id=ses_id, text=CLAUDE["held_idle"]["text"], reason=WHY)
    )
    assert engine.enters == 1, "the box was clear: no second Enter"


def test_a_queued_input_that_will_not_submit_is_stuck(instance: AppContext) -> None:
    from vogt.application.services import sessions

    engine = QueuedEngine(takes_after=99)
    _wired_queued(instance, engine)
    client = EngineClient(base_url="http://127.0.0.1:8910", transport=engine)
    engine.enters = 1  # the submit landed behind the turn; it has ended
    outcome = sessions._submit_queued(
        client, QueuedEngine.ID, "x", CLAUDE["held_idle"]["text"], person=False
    )
    assert outcome == "stuck"
    assert engine.enters == 1 + sessions._QUEUED_MAX_ENTERS, "bounded Enters"


#: Claude Code's prompt glyph.
PROMPT = "\u276f"

MULTI = "first line of the body\nsecond line\n\nthird, after a blank"


def _box(*inside: str) -> tuple[str, ...]:
    rule = "─" * 80
    return ("● Done.", "", rule, *inside, rule, "  ⏵⏵ auto mode on")


def test_a_multi_line_body_left_in_the_box_is_held() -> None:
    """WI-1870: a multi-line body sits in the box over several lines, or
    folded into Claude Code's paste placeholder."""
    wrapped = _box(
        f"{PROMPT} first line of the body",
        "  second line",
        "",
        "  third, after a blank",
    )
    assert composer_holds(wrapped, MULTI)
    folded = _box(f"{PROMPT} [Pasted text #1 +3 lines]")
    assert composer_holds(folded, MULTI)
    assert not composer_holds(folded, "one line"), "a placeholder is not one line"
    assert not composer_holds(_box(PROMPT), MULTI)


def test_a_multi_line_body_stuck_at_rest_is_stuck() -> None:
    held = Observation("idle", _box(f"{PROMPT} [Pasted text #1 +3 lines]"))
    found = judge(submitted=True, before="running", after=[held], pending=MULTI)
    assert found.delivery == "stuck"


def test_a_stuck_multi_line_body_gets_a_follow_up_enter(instance: AppContext) -> None:
    engine = QueuedEngine(takes_after=1)
    held = list(_box(f"{PROMPT} [Pasted text #1 +3 lines]"))
    saved = CLAUDE["held_idle"]["lines"]
    CLAUDE["held_idle"]["lines"] = held
    try:
        ctx, ses_id = _wired_queued(instance, engine)
        result = session_input(
            ctx, SessionInputParams(id=ses_id, text=MULTI, reason=WHY)
        )
    finally:
        CLAUDE["held_idle"]["lines"] = saved
    assert result.delivery == "queued"
    assert engine.enters == 2


def test_a_long_turn_is_waited_out_in_waits_the_engine_accepts(
    instance: AppContext,
) -> None:
    """Review of #1112: the engine refuses a wait over 600 s, so a turn
    longer than one wait is followed in chunks, each within the cap."""
    from vogt.application.services import sessions

    engine = QueuedEngine(takes_after=1, timeouts=3)
    ctx, ses_id = _wired_queued(instance, engine)
    session_input(
        ctx, SessionInputParams(id=ses_id, text=CLAUDE["held_idle"]["text"], reason=WHY)
    )
    assert engine.waits == 4, "three timed-out waits, then ready"
    assert max(engine.wait_timeouts) <= 600
    assert sessions._ENGINE_WAIT_MAX_S <= 600
    assert engine.enters == 2, "the follow-up Enter still came"


def test_a_turn_longer_than_the_follow_up_gets_no_enter(instance: AppContext) -> None:
    from vogt.application.services import sessions

    engine = QueuedEngine(takes_after=1, timeouts=1000)
    ctx, ses_id = _wired_queued(instance, engine)
    session_input(
        ctx, SessionInputParams(id=ses_id, text=CLAUDE["held_idle"]["text"], reason=WHY)
    )
    assert engine.waits * sessions._ENGINE_WAIT_MAX_S >= sessions._QUEUED_FOLLOW_S
    assert all(t <= 600 for t in engine.wait_timeouts)
    assert engine.enters == 1


def test_the_follow_up_thread_logs_what_it_raises(
    caplog: pytest.LogCaptureFixture,
) -> None:
    from vogt.application.services import sessions

    class Broken:
        def wait_session(self, *_args: object, **_kwargs: object) -> None:
            msg = "boom"
            raise RuntimeError(msg)

    sessions._submit_queued_logged(Broken(), "e", "x", "text", person=False)  # type: ignore[arg-type]
    assert "following queued input to x failed" in caplog.text


def test_a_greyed_suggestion_is_not_input_pending() -> None:
    """Review of #1112: Claude's suggestion after the prompt marker."""
    box = _box(f'{PROMPT} Try "fix lint errors"')
    assert claude_composer(box) == ""
    assert input_pending(box) is None
    assert input_pending(_box(f"{PROMPT} fix lint errors")) == "fix lint errors"
