"""Driving a session: typing into it, reading its screen, either id form.

The engine is stood in for by a transport that records every request, so the
tests can assert the bytes that reached the PTY and their order — the thing
`session.input` exists for — and that a `ses_…` id reached the engine as the
engine's own id rather than as a string it cannot parse.
"""

from __future__ import annotations

import dataclasses
import json
from typing import Any

import pytest

from vogt.adapters.engine import EngineClient, EngineUnavailable
from vogt.application.context import AppContext
from vogt.application.models import (
    InboxListParams,
    ListSessionsParams,
    LogTailParams,
    RegisterProjectParams,
    ReportBlockedParams,
    ReportUnblockedParams,
    SessionChangesParams,
    SessionInputParams,
    SessionScreenParams,
    SessionWaitAnyParams,
    SessionWaitParams,
    StartSessionParams,
    StopSessionParams,
)
from vogt.application.services import (
    list_inbox,
    list_sessions,
    log_tail,
    register_project,
    report_blocked,
    report_unblocked,
    session_changes,
    session_input,
    session_screen,
    session_wait,
    session_wait_any,
    start_session,
    stop_session,
)
from vogt.application.services._brief import DRIVING_OTHER_SESSIONS
from vogt.core.principal import Principal
from vogt.errors import InvalidRequest, MissingReason, NotFound

WHY = "drive test"
ROOT = "/srv/estate/vogt"
UNLINKED = "0b0e5f0c-1111-4222-8333-944455556666"


class Engine:
    """Answers the session routes `session.input`/`.screen`/`.stop` use."""

    def __init__(self) -> None:
        self.sent: list[dict[str, Any]] = []
        self.live: set[str] = {UNLINKED}
        self.counter = 0
        #: False models an engine that predates `GET /api/sessions/{id}/screen`.
        self.screen_supported = True
        #: Extra fields the engine adds to every screen and listed session.
        self.extra: dict[str, Any] = {}
        #: The query strings the screen route was asked with.
        self.screen_queries: list[str] = []
        self.wait_queries: list[str] = []
        self.wait_supported = True
        self.changes_queries: list[str] = []
        self.changes_supported = True
        self.wait_any_queries: list[str] = []
        self.wait_any_supported = True
        #: The engine id a canned change names as linked, when a test sets it.
        self.linked: str | None = None
        #: The blocked report the engine holds (one for the whole stand-in).
        self.blocked: dict[str, Any] | None = None
        #: Bodies sent to POST /answer.
        self.answers: list[dict[str, Any]] = []
        #: A permission prompt is showing: the engine refuses an answer or
        #: input that does not say a person is behind it (WI-983).
        self.permission_prompt = False

    def __call__(
        self,
        url: str,
        headers: dict[str, str],
        body: bytes = b"",
        method: str = "GET",
    ) -> tuple[int, bytes]:
        payload = json.loads(body.decode("utf-8")) if body else {}
        path = url.split("?", 1)[0].removeprefix("http://127.0.0.1:8910")
        self.sent.append({"path": path, "method": method, "body": payload})
        if method == "POST" and path == "/api/sessions":
            self.counter += 1
            engine_id = f"00000000-0000-4000-8000-00000000000{self.counter}"
            self.live.add(engine_id)
            return 200, json.dumps(
                {"id": engine_id, "name": "x", "activity": "running", "cwd": ROOT}
            ).encode()
        if method == "GET" and path == "/api/sessions":
            return 200, json.dumps(
                [
                    {
                        "id": engine_id,
                        "name": "x",
                        "activity": self.extra.get("activity", "running"),
                        "cwd": ROOT,
                        "created_at": "2026-01-03T00:00:00Z",
                        "alive": True,
                        "blocked": self.blocked,
                        **self.extra,
                    }
                    for engine_id in sorted(self.live)
                ]
            ).encode()
        parts = path.split("/")  # ['', 'api', 'sessions', id, verb?]
        if method == "GET" and path == "/api/sessions/changes":
            if not self.changes_supported:
                return 404, b""
            self.changes_queries.append(url.partition("?")[2])
            return 200, json.dumps(
                {
                    "changes": [
                        {
                            "seq": 7,
                            "at": "2026-10-11T00:00:00Z",
                            "session": self.linked or UNLINKED,
                            "kind": "ready",
                            "summary": "ready",
                        },
                        {
                            "seq": 8,
                            "at": "2026-10-11T00:00:01Z",
                            "session": UNLINKED,
                            "kind": "blocked",
                            "summary": "blocked: needs a token",
                        },
                    ],
                    "next_cursor": 8,
                    "truncated": False,
                }
            ).encode()
        if method == "GET" and path == "/api/sessions/wait-any":
            if not self.wait_any_supported:
                return 404, b""
            self.wait_any_queries.append(url.partition("?")[2])
            query = url.partition("?")[2]
            if "until=escalated" in query:
                return 200, json.dumps(
                    {
                        "outcome": "changed",
                        "change": {
                            "seq": 9,
                            "at": "2026-10-11T00:00:02Z",
                            "session": UNLINKED,
                            "kind": "escalated",
                            "summary": "grant request: asks for GH_TOKEN",
                            "escalation": {
                                "needs": "person",
                                "boundary": "credential",
                                "ask": "asks for GH_TOKEN",
                                "key": f"{UNLINKED}:credential:grant:grt_1",
                                "resolve": ["grant"],
                            },
                        },
                        "next_cursor": 9,
                    }
                ).encode()
            if "cursor=now" in query or (
                "timeout" in query and "until=exited" in query
            ):
                return 200, json.dumps(
                    {"outcome": "timeout", "next_cursor": 8}
                ).encode()
            return 200, json.dumps(
                {
                    "outcome": "changed",
                    "change": {
                        "seq": 8,
                        "at": "2026-10-11T00:00:01Z",
                        "session": UNLINKED,
                        "kind": "blocked",
                        "summary": "blocked: needs a token",
                    },
                    "next_cursor": 8,
                }
            ).encode()
        if len(parts) >= 4 and parts[2] == "sessions":
            engine_id = parts[3]
            verb = parts[4] if len(parts) > 4 else ""
            if engine_id not in self.live:
                return 404, b""
            if (
                method == "POST"
                and verb in ("answer", "input")
                and self.permission_prompt
                and payload.get("person") is not True
                # The fake engine lets an overseer answer; the real check of
                # its role is the engine's (`oversight_answer`, WI-2028).
                and not payload.get("overseer")
            ):
                if verb == "answer":
                    self.answers.append(payload)
                return 403, json.dumps(
                    {
                        "error": "forbidden: person required: session is showing a "
                        "permission prompt and only a person answers one"
                    }
                ).encode()
            if method == "POST" and verb == "answer" and self.permission_prompt:
                self.answers.append(payload)
                return 200, json.dumps(
                    {
                        "question": "Do you want to proceed?",
                        "kind": "permission",
                        "chosen": {"number": 2, "label": "No", "selected": True},
                        "dismissed": True,
                    }
                ).encode()
            if method == "POST" and verb == "answer":
                self.answers.append(payload)
                if payload.get("expect_question") == "stale?":
                    return 409, json.dumps(
                        {"error": "the dialog on screen now asks 'x'"}
                    ).encode()
                return 200, json.dumps(
                    {
                        "question": "Is this a project you trust?",
                        "kind": "folder-trust",
                        "chosen": {
                            "number": payload.get("option") or 1,
                            "label": "Yes, I trust this folder",
                            "selected": True,
                        },
                        "dismissed": True,
                    }
                ).encode()
            if method == "POST" and verb == "input":
                return 200, b'{"ok":true}'
            if method == "POST" and verb == "blocked":
                self.blocked = (
                    {
                        "reason": payload.get("reason"),
                        "items": payload.get("items", []),
                        "since": "2026-10-05T00:00:00Z",
                        "needs": payload.get("needs", "person"),
                        "boundary": payload.get("boundary", "other"),
                        "key": f"{engine_id}:{payload.get('boundary', 'other')}:"
                        f"{abs(hash(payload.get('reason'))) % 10**8:08d}",
                    }
                    if payload.get("blocked")
                    else None
                )
                return 200, json.dumps(
                    {
                        "id": engine_id,
                        "name": "x",
                        "activity": "idle",
                        "cwd": ROOT,
                        "blocked": self.blocked,
                    }
                ).encode()
            if method == "GET" and verb == "wait":
                if not self.wait_supported:
                    return 404, b""
                self.wait_queries.append(url.partition("?")[2])
                return 200, json.dumps(
                    {
                        "outcome": "awaiting-approval",
                        "matched": False,
                        "waited_ms": 1234,
                        "screen": {
                            "id": engine_id,
                            "cols": 80,
                            "rows": 1,
                            "lines": ["Do you want to proceed?"],
                            "cursor": {"row": 0, "col": 0},
                            "activity": "awaiting-approval",
                            "alive": True,
                            "ready": False,
                            **APPROVAL,
                        },
                    }
                ).encode()
            if method == "POST" and verb == "kill":
                self.live.discard(engine_id)
                return 200, b'{"ok":true}'
            if method == "GET" and verb == "screen":
                if not self.screen_supported:
                    return 404, b""
                self.screen_queries.append(url.partition("?")[2])
                return 200, json.dumps(
                    {
                        "id": engine_id,
                        "cols": 80,
                        "rows": 2,
                        "lines": ["$ make test", "ok"],
                        "cursor": {"row": 1, "col": 2},
                        "title": "bash",
                        "activity": "idle",
                        "alive": True,
                        "ready": True,
                        **self.extra,
                    }
                ).encode()
            if method == "GET" and verb == "":
                return 200, json.dumps(
                    {
                        "summary": {
                            "id": engine_id,
                            "name": "gui",
                            "activity": "running",
                            "cwd": ROOT,
                            "created_at": "2026-01-03T00:00:00Z",
                            **self.extra,
                        }
                    }
                ).encode()
        if (
            method == "GET"
            and path.startswith("/api/history/")
            and path.endswith("/log")
        ):
            engine_id = path.split("/")[3]
            return 200, json.dumps(
                {"session_id": engine_id, "text": "tail", "bytes": 4}
            ).encode()
        return 404, b""

    def inputs(self) -> list[tuple[str, dict[str, Any]]]:
        """What was typed, where; `person` (WI-983) has its own tests."""
        return [
            (
                row["path"].split("/")[3],
                {k: v for k, v in row["body"].items() if k != "person"},
            )
            for row in self.sent
            if row["method"] == "POST" and row["path"].endswith("/input")
        ]

    def persons(self) -> list[bool | None]:
        """The `person` each input said, in order."""
        return [
            row["body"].get("person")
            for row in self.sent
            if row["method"] == "POST" and row["path"].endswith("/input")
        ]


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


def _started(ctx: AppContext) -> tuple[str, str]:
    result = start_session(ctx, StartSessionParams(project="vogt", reason=WHY))
    return result.session.id, result.session.engine_session_id


# -- session.input -----------------------------------------------------------


def test_input_sends_text_then_keys_then_submit_in_order(
    wired: AppContext, engine: Engine
) -> None:
    ses_id, engine_id = _started(wired)
    result = session_input(
        wired,
        SessionInputParams(
            id=ses_id, text="ls", keys=["esc", "up", "ctrl-c"], submit=True, reason=WHY
        ),
    )
    assert engine.inputs() == [
        (engine_id, {"text": "ls", "submit": False}),
        (engine_id, {"text": "\x1b", "submit": False}),
        (engine_id, {"text": "\x1b[A", "submit": False}),
        (engine_id, {"text": "\x03", "submit": False}),
        (engine_id, {"text": "", "submit": True}),
    ]
    assert result.engine_session_id == engine_id
    assert result.linked is True
    assert result.bytes == 2
    assert result.submitted is True


@pytest.mark.parametrize(
    ("key", "sequence"),
    [
        ("enter", "\r"),
        ("tab", "\t"),
        ("down", "\x1b[B"),
        ("right", "\x1b[C"),
        ("left", "\x1b[D"),
        ("ctrl-d", "\x04"),
        ("backspace", "\x7f"),
    ],
)
def test_each_named_key_maps_to_its_terminal_bytes(
    wired: AppContext, engine: Engine, key: str, sequence: str
) -> None:
    session_input(
        wired,
        SessionInputParams.model_validate(
            {"id": UNLINKED, "keys": [key], "reason": WHY}
        ),
    )
    assert engine.inputs() == [(UNLINKED, {"text": sequence, "submit": False})]


def test_an_unknown_key_is_refused_by_the_model() -> None:
    with pytest.raises(ValueError, match="keys"):
        SessionInputParams.model_validate(
            {"id": UNLINKED, "keys": ["f13"], "reason": WHY}
        )


def test_text_submits_by_default(wired: AppContext, engine: Engine) -> None:
    result = session_input(
        wired, SessionInputParams(id=UNLINKED, text="hi", reason=WHY)
    )
    assert engine.inputs() == [
        (UNLINKED, {"text": "hi", "submit": False}),
        (UNLINKED, {"text": "", "submit": True}),
    ]
    assert result.submitted is True
    assert result.delivery != "typed"


def test_keys_alone_press_only_the_keys(wired: AppContext, engine: Engine) -> None:
    result = session_input(
        wired,
        SessionInputParams.model_validate(
            {"id": UNLINKED, "keys": ["esc"], "reason": WHY}
        ),
    )
    assert engine.inputs() == [(UNLINKED, {"text": "\x1b", "submit": False})]
    assert result.submitted is False


def test_text_and_enter_key_press_enter_once(wired: AppContext, engine: Engine) -> None:
    result = session_input(
        wired,
        SessionInputParams(id=UNLINKED, text="go on", keys=["enter"], reason=WHY),
    )
    assert engine.inputs() == [
        (UNLINKED, {"text": "go on", "submit": False}),
        (UNLINKED, {"text": "\r", "submit": False}),
    ]
    assert result.submitted is True


def test_text_and_a_non_enter_key_still_submit(
    wired: AppContext, engine: Engine
) -> None:
    session_input(
        wired,
        SessionInputParams(id=UNLINKED, text="ls", keys=["tab"], reason=WHY),
    )
    assert engine.inputs() == [
        (UNLINKED, {"text": "ls", "submit": False}),
        (UNLINKED, {"text": "\t", "submit": False}),
        (UNLINKED, {"text": "", "submit": True}),
    ]


def test_draft_types_without_pressing_enter(wired: AppContext, engine: Engine) -> None:
    result = session_input(
        wired,
        SessionInputParams(id=UNLINKED, text="hi", draft=True, reason=WHY),
    )
    assert engine.inputs() == [(UNLINKED, {"text": "hi", "submit": False})]
    assert result.submitted is False
    assert result.delivery == "typed"
    assert "no Enter" in result.delivery_evidence


def test_explicit_submit_false_does_not_press_enter(
    wired: AppContext, engine: Engine
) -> None:
    result = session_input(
        wired,
        SessionInputParams(id=UNLINKED, text="hi", submit=False, reason=WHY),
    )
    assert engine.inputs() == [(UNLINKED, {"text": "hi", "submit": False})]
    assert result.submitted is False
    assert result.delivery == "typed"


def test_draft_wins_over_submit(wired: AppContext, engine: Engine) -> None:
    result = session_input(
        wired,
        SessionInputParams(id=UNLINKED, text="hi", submit=True, draft=True, reason=WHY),
    )
    assert engine.inputs() == [(UNLINKED, {"text": "hi", "submit": False})]
    assert result.submitted is False
    assert result.delivery == "typed"


def test_input_accepts_an_unlinked_engine_uuid(
    wired: AppContext, engine: Engine
) -> None:
    result = session_input(
        wired, SessionInputParams(id=UNLINKED, text="hi", reason=WHY)
    )
    assert result.linked is False
    assert engine.inputs() == [
        (UNLINKED, {"text": "hi", "submit": False}),
        (UNLINKED, {"text": "", "submit": True}),
    ]


def test_input_accepts_the_engine_uuid_of_a_linked_session(
    wired: AppContext, engine: Engine
) -> None:
    ses_id, engine_id = _started(wired)
    result = session_input(
        wired, SessionInputParams(id=engine_id, submit=True, reason=WHY)
    )
    assert result.linked is True
    with wired.declared.read() as view:
        audit = view.list_audit(limit=10)
    assert audit[0].operation == "session.input"
    assert audit[0].entity_id == ses_id, "a linked session is audited by its ses_ id"


def test_input_is_audited_without_the_text(wired: AppContext, engine: Engine) -> None:
    secret = "hunter2-correct-horse"
    session_input(
        wired,
        SessionInputParams(id=UNLINKED, text=secret, keys=["enter"], reason=WHY),
    )
    with wired.declared.read() as view:
        audit = view.list_audit(limit=10)
        events = view.list_events(after=0, limit=50)
    row = next(one for one in audit if one.operation == "session.input")
    assert row.entity_id == UNLINKED
    assert engine.inputs() == [
        (UNLINKED, {"text": secret, "submit": False}),
        (UNLINKED, {"text": "\r", "submit": False}),
    ], "Enter as a key is the submit: no second, blind Enter after it"
    assert row.reason == WHY
    event = next(one for one in events if one.kind == "session.input")
    assert event.summary["bytes"] == len(secret)
    assert event.summary["keys"] == ["enter"]
    written = json.dumps([one.model_dump(mode="json") for one in (*audit, *events)])
    assert secret not in written


def test_input_needs_a_reason(wired: AppContext, engine: Engine) -> None:
    with pytest.raises(MissingReason):
        # The shape adapters already refuse, built directly: the service must
        # still refuse it before anything reaches the terminal.
        session_input(
            wired,
            SessionInputParams.model_construct(
                id=UNLINKED, text="x", keys=None, submit=False, reason=" \t"
            ),
        )
    assert engine.inputs() == []


def test_input_with_nothing_to_send_is_refused(wired: AppContext) -> None:
    with pytest.raises(InvalidRequest, match="nothing to send"):
        session_input(wired, SessionInputParams(id=UNLINKED, reason=WHY))


def test_input_over_the_engine_cap_is_refused_before_sending(
    wired: AppContext, engine: Engine
) -> None:
    with pytest.raises(InvalidRequest, match="65536"):
        session_input(
            wired, SessionInputParams(id=UNLINKED, text="é" * 40000, reason=WHY)
        )
    assert engine.inputs() == []


def test_input_to_a_session_the_engine_lacks_is_not_found(wired: AppContext) -> None:
    with pytest.raises(NotFound):
        session_input(
            wired,
            SessionInputParams(
                id="99999999-0000-4000-8000-000000000000", text="x", reason=WHY
            ),
        )


def test_an_unknown_ses_id_is_not_found_without_asking_the_engine(
    wired: AppContext, engine: Engine
) -> None:
    with pytest.raises(NotFound, match="ses_nope"):
        session_input(wired, SessionInputParams(id="ses_nope", text="x", reason=WHY))
    assert engine.inputs() == []


def test_input_with_no_engine_says_so(instance: AppContext) -> None:
    with pytest.raises(EngineUnavailable, match="VOGT_ENGINE_URL"):
        session_input(instance, SessionInputParams(id=UNLINKED, text="x", reason=WHY))


# -- session.screen ----------------------------------------------------------


def test_screen_maps_the_engine_contract(wired: AppContext) -> None:
    ses_id, engine_id = _started(wired)
    screen = session_screen(wired, SessionScreenParams(id=ses_id))
    assert screen.id == ses_id
    assert screen.engine_session_id == engine_id
    assert (screen.cols, screen.rows) == (80, 2)
    assert screen.lines == ["$ make test", "ok"]
    assert screen.cursor is not None
    assert (screen.cursor.row, screen.cursor.col) == (1, 2)
    assert screen.title == "bash"
    assert screen.activity == "idle"
    assert screen.alive is True
    assert screen.ready is True


APPROVAL = {
    "activity": "awaiting-approval",
    "ready": False,
    "turn_started_at": "2026-10-05T00:00:00Z",
    "last_output_at": "2026-10-05T00:17:00Z",
    "approval": {
        "question": "Do you want to proceed?",
        "command_excerpt": "Bash command\nsh -c 'docker rm -f x'",
        "deadline_seconds": 61,
        "deadline_at": "2026-10-05T00:18:00Z",
        "detected_at": "2026-10-05T00:16:30Z",
    },
}


def test_screen_carries_the_approval_turn_timing_and_scrollback(
    wired: AppContext, engine: Engine
) -> None:
    """WI-877 / WI-875: a permission dialog and the turn's timing reach the
    caller, and `scrollback_lines` is passed to the engine."""
    engine.extra = {**APPROVAL, "scrollback": ["older", "lines"]}
    ses_id, _ = _started(wired)
    screen = session_screen(wired, SessionScreenParams(id=ses_id, scrollback_lines=40))
    assert engine.screen_queries[-1] == "scrollback_lines=40"
    assert screen.activity == "awaiting-approval"
    assert screen.ready is False
    assert screen.scrollback == ["older", "lines"]
    assert screen.approval is not None
    assert screen.approval.question == "Do you want to proceed?"
    assert "docker rm -f x" in screen.approval.command_excerpt
    assert screen.approval.deadline_seconds == 61
    assert screen.turn_started_at is not None
    assert screen.last_output_at is not None
    assert screen.last_output_at > screen.turn_started_at
    # No scrollback asked for: no query sent, so an older engine is unaffected.
    session_screen(wired, SessionScreenParams(id=ses_id))
    assert engine.screen_queries[-1] == ""


def test_session_list_and_inbox_show_a_permission_dialog(
    wired: AppContext, engine: Engine
) -> None:
    engine.extra = dict(APPROVAL)
    _started(wired)
    rows = list_sessions(wired, ListSessionsParams(mode="full")).sessions
    assert rows and all(row.activity == "awaiting-approval" for row in rows)
    approval = rows[0].approval
    assert approval is not None and approval.deadline_seconds == 61
    assert rows[0].turn_started_at is not None
    entries = list_inbox(wired, InboxListParams()).entries
    asking = [e for e in entries if e.kind == "session.attention"]
    assert asking, "a permission dialog is an Inbox entry"
    assert all("asking for approval" in e.title for e in asking)
    assert "Auto-deny in 61s" in asking[0].summary
    assert "docker rm -f x" in asking[0].summary


def test_wait_passes_until_and_timeout_and_returns_why(
    wired: AppContext, engine: Engine
) -> None:
    """WI-874: one call waits on the engine and says why it stopped."""
    ses_id, engine_id = _started(wired)
    waited = session_wait(
        wired,
        SessionWaitParams(id=ses_id, until="any_change", timeout_s=30, screen=True),
    )
    assert engine.wait_queries[-1] == "until=any-change&timeout_s=30"
    assert waited.engine_session_id == engine_id
    assert waited.outcome == "awaiting-approval"
    assert waited.matched is False
    assert waited.waited_ms == 1234
    assert waited.screen is not None
    assert waited.screen.approval is not None
    assert waited.screen.ready is False


def test_wait_on_an_engine_without_the_route_says_so(
    wired: AppContext, engine: Engine
) -> None:
    engine.wait_supported = False
    with pytest.raises(EngineUnavailable, match="does not support wait yet"):
        session_wait(wired, SessionWaitParams(id=UNLINKED))


def test_wait_bounds_its_timeout() -> None:
    with pytest.raises(ValueError):
        SessionWaitParams(id=UNLINKED, timeout_s=601)


def test_changes_maps_a_linked_session_and_keeps_an_unlinked_uuid(
    wired: AppContext, engine: Engine
) -> None:
    """WI-1246: a delta names Vogt's id when the engine session is linked."""
    ses_id, engine_id = _started(wired)
    engine.linked = engine_id
    page = session_changes(wired, SessionChangesParams(since=3, ids=[ses_id], limit=10))
    assert engine.changes_queries[-1] == f"since=3&limit=10&ids={engine_id}"
    assert page.next_cursor == 8 and page.truncated is False
    linked, unlinked = page.changes
    assert linked.session == ses_id and linked.kind == "ready" and linked.seq == 7
    assert unlinked.session == UNLINKED and unlinked.kind == "blocked"
    assert unlinked.summary == "blocked: needs a token"


def test_changes_on_an_engine_without_the_route_says_so(
    wired: AppContext, engine: Engine
) -> None:
    engine.changes_supported = False
    with pytest.raises(EngineUnavailable, match="does not support changes yet"):
        session_changes(wired, SessionChangesParams())


def test_changes_rejects_an_unknown_session(wired: AppContext) -> None:
    with pytest.raises(NotFound):
        session_changes(wired, SessionChangesParams(ids=["ses_nope"]))


def test_wait_any_passes_its_query_and_returns_the_mapped_change(
    wired: AppContext, engine: Engine
) -> None:
    ses_id, _engine_id = _started(wired)
    waited = session_wait_any(
        wired,
        SessionWaitAnyParams(
            ids=[ses_id, UNLINKED], until=["blocked"], cursor=4, timeout_s=30
        ),
    )
    query = engine.wait_any_queries[-1]
    assert "timeout_s=30" in query and "until=blocked" in query and "cursor=4" in query
    assert UNLINKED in query
    assert waited.outcome == "changed"
    assert waited.change is not None and waited.change.session == UNLINKED
    assert waited.next_cursor == 8


def test_wait_any_until_escalated_returns_the_escalation_payload(
    wired: AppContext, engine: Engine
) -> None:
    waited = session_wait_any(
        wired, SessionWaitAnyParams(until=["escalated"], timeout_s=30)
    )
    assert "until=escalated" in engine.wait_any_queries[-1]
    assert waited.change is not None and waited.change.kind == "escalated"
    esc = waited.change.escalation
    assert esc is not None
    assert (esc.needs, esc.boundary, esc.resolve) == ("person", "credential", ["grant"])
    assert esc.key == f"{UNLINKED}:credential:grant:grt_1"
    assert esc.ask == "asks for GH_TOKEN"


def test_a_change_without_an_escalation_maps_to_none(
    wired: AppContext, engine: Engine
) -> None:
    waited = session_wait_any(wired, SessionWaitAnyParams(until=["blocked"]))
    assert waited.change is not None and waited.change.escalation is None


def test_wait_any_timeout_has_no_change(wired: AppContext, engine: Engine) -> None:
    waited = session_wait_any(
        wired, SessionWaitAnyParams(until=["exited"], timeout_s=5)
    )
    assert waited.outcome == "timeout" and waited.change is None


def test_wait_any_on_an_engine_without_the_route_says_so(
    wired: AppContext, engine: Engine
) -> None:
    engine.wait_any_supported = False
    with pytest.raises(EngineUnavailable, match="does not support wait-any yet"):
        session_wait_any(wired, SessionWaitAnyParams())


def test_wait_any_bounds_its_timeout() -> None:
    with pytest.raises(ValueError):
        SessionWaitAnyParams(timeout_s=601)


def test_wait_any_cursor_now_is_sent_as_now(wired: AppContext, engine: Engine) -> None:
    waited = session_wait_any(wired, SessionWaitAnyParams(cursor="now", timeout_s=5))
    assert "cursor=now" in engine.wait_any_queries[-1]
    assert waited.outcome == "timeout"


def test_wait_any_rejects_a_cursor_that_is_neither_a_seq_nor_now() -> None:
    with pytest.raises(ValueError, match="cursor"):
        SessionWaitAnyParams(cursor="yesterday")


def test_a_session_reports_itself_blocked_without_naming_itself(
    wired: AppContext, engine: Engine
) -> None:
    """WI-873: from inside a session Vogt started, the token names it."""
    ses_id, engine_id = _started(wired)
    own = dataclasses.replace(
        wired,
        principal=Principal(
            identity_ref=f"agent:session:{ses_id}",
            kind="agent",
            display_name=f"Session {ses_id}",
        ),
    )
    result = report_blocked(
        own,
        ReportBlockedParams(
            blocker="needs the Telegram bot token",
            items=["create the bot", "  ", "store the token"],
            reason="cannot finish MVP1 without it",
        ),
    )
    assert result.id == ses_id and result.engine_session_id == engine_id
    assert result.blocked is not None
    assert result.blocked.items == ["create the bot", "store the token"]
    sent = [row for row in engine.sent if row["path"].endswith("/blocked")][-1]
    assert sent["body"]["blocked"] is True
    # It is on the list and in the Inbox, and the write is audited with it.
    row = next(
        r
        for r in list_sessions(wired, ListSessionsParams(mode="full")).sessions
        if r.id == ses_id
    )
    assert row.blocked is not None
    assert row.blocked.blocker == "needs the Telegram bot token"
    entries = list_inbox(wired, InboxListParams()).entries
    blocked = [e for e in entries if e.kind == "session.blocked"]
    assert blocked and "blocked on you" in blocked[0].title
    assert "create the bot" in blocked[0].summary
    with wired.declared.read() as view:
        audit = view.list_audit(limit=20)
    assert any(a.operation == "session.report_blocked" for a in audit)
    # And cleared.
    cleared = report_unblocked(own, ReportUnblockedParams(reason="token stored"))
    assert cleared.blocked is None
    assert not [
        e
        for e in list_inbox(wired, InboxListParams()).entries
        if e.kind == "session.blocked"
    ]


def test_a_typed_blocked_report_carries_needs_boundary_and_a_content_key(
    wired: AppContext, engine: Engine
) -> None:
    """WI-996 E0: the report says who can resolve it and what was hit; the
    Inbox keys it on content, so the same ask re-reported is one entry."""
    ses_id, engine_id = _started(wired)
    params = ReportBlockedParams(
        id=ses_id,
        blocker="gh pr merge was denied by auto mode",
        needs="overseer",
        boundary="denied_action",
        reason="cannot land without it",
    )
    result = report_blocked(wired, params)
    sent = [row for row in engine.sent if row["path"].endswith("/blocked")][-1]
    assert sent["body"]["needs"] == "overseer"
    assert sent["body"]["boundary"] == "denied_action"
    assert result.blocked is not None
    assert result.blocked.needs == "overseer"
    assert result.blocked.boundary == "denied_action"
    assert result.blocked.key and result.blocked.key.startswith(
        f"{engine_id}:denied_action:"
    )
    first = [
        e.entry_key
        for e in list_inbox(wired, InboxListParams()).entries
        if e.kind == "session.blocked"
    ]
    assert f"agent:session:{engine_id}:blocked:{result.blocked.key}" in first
    titles = [
        e.title
        for e in list_inbox(wired, InboxListParams()).entries
        if e.kind == "session.blocked"
    ]
    assert titles and titles[0].endswith("is blocked on its overseer")
    report_blocked(wired, params)
    again = [
        e.entry_key
        for e in list_inbox(wired, InboxListParams()).entries
        if e.kind == "session.blocked"
    ]
    assert again == first


def test_an_untyped_blocked_report_needs_a_person(
    wired: AppContext, engine: Engine
) -> None:
    ses_id, _ = _started(wired)
    result = report_blocked(
        wired, ReportBlockedParams(id=ses_id, blocker="x", reason="old caller")
    )
    assert result.blocked is not None
    assert (result.blocked.needs, result.blocked.boundary) == ("person", "other")


def test_a_bad_needs_is_refused() -> None:
    with pytest.raises(ValueError):
        ReportBlockedParams(blocker="x", needs="anyone", reason="testing")  # type: ignore[arg-type]


def test_a_caller_that_is_not_a_session_must_name_one(wired: AppContext) -> None:
    with pytest.raises(InvalidRequest, match="give `id`"):
        report_blocked(
            wired, ReportBlockedParams(blocker="x", reason="testing the refusal")
        )


def test_screen_on_an_engine_without_the_route_says_so(
    wired: AppContext, engine: Engine
) -> None:
    engine.screen_supported = False
    with pytest.raises(EngineUnavailable, match="does not support screen yet"):
        session_screen(wired, SessionScreenParams(id=UNLINKED))


def test_screen_of_an_unknown_session_is_not_found(wired: AppContext) -> None:
    with pytest.raises(NotFound):
        session_screen(
            wired, SessionScreenParams(id="99999999-0000-4000-8000-000000000000")
        )


# -- either id form (WI-829) -------------------------------------------------


def test_log_tail_resolves_a_ses_id_to_the_engine_uuid(
    wired: AppContext, engine: Engine
) -> None:
    ses_id, engine_id = _started(wired)
    tail = log_tail(wired, LogTailParams(id=ses_id))
    assert tail.session_id == engine_id
    asked = [row["path"] for row in engine.sent if row["path"].endswith("/log")]
    assert asked == [f"/api/history/{engine_id}/log"]


def test_log_tail_of_an_unknown_ses_id_is_not_found(wired: AppContext) -> None:
    with pytest.raises(NotFound):
        log_tail(wired, LogTailParams(id="ses_missing"))


def test_stop_accepts_the_engine_uuid_of_a_linked_session(
    wired: AppContext, engine: Engine
) -> None:
    ses_id, engine_id = _started(wired)
    stopped = stop_session(wired, StopSessionParams(id=engine_id, reason=WHY))
    assert stopped.session.id == ses_id
    assert stopped.session.stopped_at is not None
    assert engine_id not in engine.live


def test_stop_kills_an_unlinked_session_and_audits_it(
    wired: AppContext, engine: Engine
) -> None:
    stopped = stop_session(wired, StopSessionParams(id=UNLINKED, reason=WHY))
    assert stopped.session.linked is False
    assert stopped.session.alive is False
    assert UNLINKED not in engine.live
    with wired.declared.read() as view:
        audit = view.list_audit(limit=10)
    assert audit[0].operation == "session.stop"
    assert audit[0].entity_id == UNLINKED


def test_stopping_an_unlinked_session_the_engine_lacks_is_not_found(
    wired: AppContext,
) -> None:
    with pytest.raises(NotFound):
        stop_session(
            wired,
            StopSessionParams(id="99999999-0000-4000-8000-000000000000", reason=WHY),
        )


# -- how a session learns to reach the others --------------------------------


def test_a_session_is_told_where_the_engine_is(
    wired: AppContext, engine: Engine
) -> None:
    _started(wired)
    create = next(
        row
        for row in engine.sent
        if row["method"] == "POST" and row["path"] == "/api/sessions"
    )
    env = dict(create["body"]["env"])
    assert env["VOGT_ENGINE_URL"] == "http://127.0.0.1:8910"
    # The driving instructions moved to the companion; the brief only names it.
    assert ".guide.md" in create["body"]["prompt"]
    assert DRIVING_OTHER_SESSIONS in create["body"]["companion"]


# -- session.answer (WI-917) ------------------------------------------------


def test_answer_chooses_by_option_and_is_audited(
    wired: AppContext, engine: Engine
) -> None:
    from vogt.application.models import AnswerSessionParams, ListAuditParams
    from vogt.application.services import answer_session, list_audit

    ses_id, engine_id = _started(wired)
    result = answer_session(
        wired,
        AnswerSessionParams(
            id=ses_id,
            option=1,
            expect_question="Is this a project you trust?",
            reason=WHY,
        ),
    )
    assert engine.answers == [
        {"option": 1, "expect_question": "Is this a project you trust?", "person": True}
    ]
    assert result.kind == "folder-trust"
    assert result.chosen.number == 1
    assert result.dismissed is True
    assert result.engine_session_id == engine_id
    row = list_audit(wired, ListAuditParams(limit=1)).records[0]
    assert row.operation == "session.answer"


def test_answer_needs_exactly_one_of_option_and_label(wired: AppContext) -> None:
    from vogt.application.models import AnswerSessionParams
    from vogt.application.services import answer_session

    ses_id, _ = _started(wired)
    for params in (
        AnswerSessionParams(id=ses_id, reason=WHY),
        AnswerSessionParams(id=ses_id, option=1, label="yes", reason=WHY),
    ):
        with pytest.raises(InvalidRequest, match="exactly one"):
            answer_session(wired, params)


def test_a_stale_answer_is_refused_with_the_engines_reason(
    wired: AppContext, engine: Engine
) -> None:
    from vogt.application.models import AnswerSessionParams
    from vogt.application.services import answer_session
    from vogt.errors import Conflict

    ses_id, _ = _started(wired)
    with pytest.raises(Conflict, match="now asks"):
        answer_session(
            wired,
            AnswerSessionParams(
                id=ses_id, label="yes", expect_question="stale?", reason=WHY
            ),
        )


def test_the_approval_carries_its_kind_and_menu(
    wired: AppContext, engine: Engine
) -> None:
    ses_id, _ = _started(wired)
    engine.extra = {
        "activity": "awaiting-approval",
        "approval": {
            "question": "Allow external CLAUDE.md file imports?",
            "command_excerpt": "",
            "detected_at": "2026-10-05T00:00:00Z",
            "kind": "external-imports",
            "options": [
                {"number": 1, "label": "Yes, allow external imports", "selected": True},
                {"number": 2, "label": "No, disable external imports"},
            ],
        },
    }
    screen = session_screen(wired, SessionScreenParams(id=ses_id))
    assert screen.approval is not None
    assert screen.approval.kind == "external-imports"
    assert [o.number for o in screen.approval.options] == [1, 2]
    assert screen.approval.options[0].selected is True


# -- only a person answers a permission prompt (WI-983) ---------------------

AGENTS = (
    "agent:session:ses_01OVERSEER",
    "agent:engine:3f2a",
    "agent:pod:vogt-dev",
    "agent:vogt-sessions",
)


def _as(ctx: AppContext, identity_ref: str, kind: str = "agent") -> AppContext:
    return dataclasses.replace(
        ctx,
        principal=Principal(
            identity_ref=identity_ref,
            kind=kind,  # type: ignore[arg-type]
            display_name=identity_ref,
        ),
    )


@pytest.mark.parametrize("identity_ref", AGENTS)
def test_an_agent_cannot_answer_a_permission_prompt(
    wired: AppContext, engine: Engine, identity_ref: str
) -> None:
    from vogt.application.models import AnswerSessionParams
    from vogt.application.services import answer_session
    from vogt.errors import PersonRequired

    ses_id, _ = _started(wired)
    engine.permission_prompt = True
    agent = _as(wired, identity_ref)
    with pytest.raises(PersonRequired, match="only a person"):
        answer_session(agent, AnswerSessionParams(id=ses_id, option=1, reason=WHY))
    assert engine.answers[-1]["person"] is False
    # Typing into the dialog is the same answer by other means.
    with pytest.raises(PersonRequired):
        session_input(
            agent, SessionInputParams(id=ses_id, text="1", submit=True, reason=WHY)
        )
    assert engine.persons() == [False]
    with wired.declared.read() as view:
        audited = [row.operation for row in view.list_audit(limit=50)]
    assert "session.answer" not in audited
    assert "session.input" not in audited


def test_a_refused_answer_comes_back_escalated_with_its_keys(
    wired: AppContext, engine: Engine
) -> None:
    """WI-996: the refusal names the Inbox entry a person answers and the
    dialog's content key, `outcome: escalated`, on both routes."""
    from vogt.application.models import AnswerSessionParams
    from vogt.application.services import answer_session
    from vogt.errors import PersonRequired

    ses_id, engine_id = _started(wired)
    engine.permission_prompt = True
    engine.extra = {
        "activity": "awaiting-approval",
        "activity_changed_at": "2026-10-31T01:02:03Z",
        "approval": {"question": "Do you want to proceed?", "options": []},
    }
    agent = _as(wired, AGENTS[0])
    with pytest.raises(PersonRequired, match="escalated to a person") as answered:
        answer_session(agent, AnswerSessionParams(id=ses_id, option=1, reason=WHY))
    details = answered.value.details or {}
    assert details["outcome"] == "escalated"
    assert details["needs"] == "person"
    assert details["inbox_entry_key"] == (
        f"agent:session:{engine_id}:awaiting-approval:2026-10-31T01:02:03Z"
    )
    assert details["escalation_key"].startswith(f"{engine_id}:permission_prompt:")
    with pytest.raises(PersonRequired) as typed:
        session_input(
            agent, SessionInputParams(id=ses_id, text="1", submit=True, reason=WHY)
        )
    # The same prompt is one escalation, whichever route met it.
    assert typed.value.details == details


def test_a_refused_answer_is_shown_to_the_person_as_a_recommendation(
    wired: AppContext, engine: Engine
) -> None:
    """WI-996 E0b: an agent's refused pick is recorded under the dialog's
    escalation key and shown on the Inbox entry beside the person's buttons,
    by the engine's label, never chosen. A pick no option matches records
    nothing, and a recommendation for another question is not shown."""
    from vogt.application.models import AnswerSessionParams
    from vogt.application.services import answer_session
    from vogt.errors import PersonRequired

    ses_id, engine_id = _started(wired)
    engine.permission_prompt = True
    options = [
        {"number": 1, "label": "Yes", "selected": True},
        {"number": 2, "label": "No, and tell Claude", "selected": False},
    ]
    engine.extra = {
        "activity": "awaiting-approval",
        "activity_changed_at": "2026-10-31T01:02:03Z",
        "approval": {"question": "Do you want to proceed?", "options": options},
    }
    agent = _as(wired, AGENTS[0])
    with pytest.raises(PersonRequired) as unmatched:
        answer_session(agent, AnswerSessionParams(id=ses_id, label="maybe", reason=WHY))
    assert "recommendation" not in (unmatched.value.details or {})
    with pytest.raises(PersonRequired, match="as a recommendation") as refused:
        answer_session(
            agent,
            AnswerSessionParams(id=ses_id, label="no, AND tell claude", reason=WHY),
        )
    details = refused.value.details or {}
    assert details["recommendation"] == "recorded"

    entry = next(
        e
        for e in list_inbox(wired, InboxListParams()).entries
        if e.entry_key == details["inbox_entry_key"]
    )
    shown = (entry.evidence_snapshot or {})["recommendation"]
    assert isinstance(shown, dict)
    assert shown["option"] == 2
    assert shown["label"] == "No, and tell Claude"
    assert shown["by"] == AGENTS[0]
    assert shown["escalation_key"] == details["escalation_key"]
    assert f"{AGENTS[0]} recommends 2." in entry.summary

    # The next dialog asks something else: the old pick is not carried over.
    engine.extra = {
        "activity": "awaiting-approval",
        "activity_changed_at": "2026-10-31T01:05:00Z",
        "approval": {"question": "Run rm -rf build?", "options": options},
    }
    entry = next(
        e
        for e in list_inbox(wired, InboxListParams()).entries
        if e.kind == "session.attention" and e.session_id == engine_id
    )
    assert entry.evidence_snapshot is None
    assert "recommends" not in entry.summary


def test_an_oversight_session_answers_a_permission_prompt_and_is_audited(
    wired: AppContext, engine: Engine
) -> None:
    """An autonomous fleet has no person at the prompt: the session the engine
    marks `oversight` answers for its workers, audited as oversight. It is
    relayed by its engine id, not as a person, so the engine checks its role
    and audits it with the command (WI-1831, WI-2028)."""
    from vogt.application.models import AnswerSessionParams
    from vogt.application.services import answer_session

    ses_id, _ = _started(wired)
    engine.permission_prompt = True
    engine.extra = {"role": "oversight"}
    overseer = _as(wired, f"agent:engine:{UNLINKED}")
    result = answer_session(
        overseer, AnswerSessionParams(id=ses_id, option=1, reason=WHY)
    )
    assert result.kind == "permission"
    assert engine.answers[-1]["person"] is False
    assert engine.answers[-1]["overseer"] == UNLINKED
    with wired.declared.read() as view:
        event = next(
            e
            for e in view.list_events(after=0, limit=100)
            if e.kind == "session.answered"
        )
    assert event.summary["person"] is False
    assert event.summary["oversight"] is True


def test_an_oversight_session_answers_a_permission_prompt_with_keys(
    wired: AppContext, engine: Engine
) -> None:
    """Keys typed into a worker's permission prompt are an answer too: an
    oversight session's are relayed with its engine id and audited as
    oversight (WI-2028); the same keys from a plain worker are refused
    above."""
    ses_id, _ = _started(wired)
    engine.permission_prompt = True
    engine.extra = {"role": "oversight"}
    overseer = _as(wired, f"agent:engine:{UNLINKED}")
    session_input(overseer, SessionInputParams(id=ses_id, keys=["enter"], reason=WHY))
    assert [
        row["body"].get("overseer")
        for row in engine.sent
        if row["method"] == "POST" and row["path"].endswith("/input")
    ] == [UNLINKED]
    assert engine.persons() == [False]
    with wired.declared.read() as view:
        row = next(
            r for r in view.list_audit(limit=50) if r.operation == "session.input"
        )
    assert row.actor_identity_ref == f"agent:engine:{UNLINKED}"


def test_a_person_answers_a_permission_prompt_and_is_audited(
    wired: AppContext, engine: Engine
) -> None:
    from vogt.application.models import AnswerSessionParams
    from vogt.application.services import answer_session

    ses_id, _ = _started(wired)
    engine.permission_prompt = True
    person = _as(wired, "human:ada", kind="human")
    result = answer_session(
        person, AnswerSessionParams(id=ses_id, label="no", reason=WHY)
    )
    assert result.kind == "permission"
    assert engine.answers[-1]["person"] is True
    with wired.declared.read() as view:
        row = next(
            r for r in view.list_audit(limit=50) if r.operation == "session.answer"
        )
        event = next(
            e
            for e in view.list_events(after=0, limit=100)
            if e.kind == "session.answered"
        )
    assert row.actor_identity_ref == "human:ada"
    assert row.reason == WHY
    assert event.summary["kind"] == "permission"
    assert event.summary["question"] == "Do you want to proceed?"
    assert event.summary["option"] == 2
    assert event.summary["label"] == "No"
    assert event.summary["person"] is True


def test_the_engines_own_credential_is_not_a_person_even_when_bound_to_one(
    wired: AppContext, engine: Engine, tmp_path: Any
) -> None:
    """The stack secret first issued by hand is bound to a person (the
    estate); presented, it is the engine — or any process that read it."""
    from vogt.application.models import AnswerSessionParams, IssueTokenParams
    from vogt.application.services import answer_session
    from vogt.application.services.auth import authenticate, issue_token
    from vogt.errors import PersonRequired

    ses_id, _ = _started(wired)
    secret = issue_token(
        wired,
        IssueTokenParams(
            actor=wired.principal.identity_ref,
            name="stack secret",
            scopes="admin",
            reason=WHY,
        ),
    ).secret
    secret_file = tmp_path / "vogt-core-token"
    secret_file.write_text(secret + "\n", encoding="utf-8")
    ctx = dataclasses.replace(
        wired,
        config=wired.config.model_copy(
            update={"bootstrap_core_token_file": secret_file}
        ),
    )
    who = authenticate(ctx, bearer=secret)
    as_engine = dataclasses.replace(ctx, principal=who.principal, token=who.token)
    assert as_engine.principal.kind == "human"
    engine.permission_prompt = True
    with pytest.raises(PersonRequired):
        answer_session(as_engine, AnswerSessionParams(id=ses_id, option=1, reason=WHY))
    assert engine.answers[-1]["person"] is False


def test_an_overseer_still_drives_a_session_with_no_permission_prompt(
    wired: AppContext, engine: Engine
) -> None:
    """Ordinary input and startup-gate answers from an agent are unchanged:
    the engine, which reads the screen, is what tells them apart."""
    from vogt.application.models import AnswerSessionParams
    from vogt.application.services import answer_session

    ses_id, engine_id = _started(wired)
    overseer = _as(wired, AGENTS[0])
    typed = session_input(
        overseer,
        SessionInputParams(id=ses_id, text="carry on", submit=True, reason=WHY),
    )
    assert typed.submitted is True
    assert engine.inputs() == [
        (engine_id, {"text": "carry on", "submit": False}),
        (engine_id, {"text": "", "submit": True}),
    ]
    assert engine.persons() == [False, False]
    gate = answer_session(
        overseer, AnswerSessionParams(id=ses_id, option=1, reason=WHY)
    )
    assert gate.kind == "folder-trust"
    with wired.declared.read() as view:
        rows = [r for r in view.list_audit(limit=50) if r.operation == "session.answer"]
    assert rows[0].actor_identity_ref == AGENTS[0]


def test_wait_any_blocks_at_most_the_cap_and_says_still_waiting(
    wired: AppContext, engine: Engine
) -> None:
    """WI-1574: Codex cuts a tool call at 300 s, so one wait blocks 280 s."""
    waited = session_wait_any(
        wired, SessionWaitAnyParams(until=["exited"], timeout_s=600)
    )
    assert "timeout_s=280" in engine.wait_any_queries[-1]
    assert waited.outcome == "timeout" and waited.still_waiting is True
    assert waited.next_cursor == 8


def test_wait_any_under_the_cap_is_not_still_waiting(
    wired: AppContext, engine: Engine
) -> None:
    waited = session_wait_any(
        wired, SessionWaitAnyParams(until=["exited"], timeout_s=5)
    )
    assert waited.still_waiting is False


def test_wait_blocks_at_most_the_cap(wired: AppContext, engine: Engine) -> None:
    ses_id, _ = _started(wired)
    waited = session_wait(
        wired, SessionWaitParams(id=ses_id, until="any_change", timeout_s=600)
    )
    assert engine.wait_queries[-1] == "until=any-change&timeout_s=280"
    # The fake answers a match, not a timeout: nothing is still waiting.
    assert waited.still_waiting is False


def test_an_escalated_refusal_carries_its_keys_on_the_mcp_result() -> None:
    """WI-996: the keys reach a model as structuredContent, not only text."""
    from vogt.adapters.mcp.stdio import _tool_error

    plain = _tool_error("person_required", "no")
    assert "structuredContent" not in plain
    keyed = _tool_error("person_required", "no", {"outcome": "escalated"})
    assert keyed["isError"] is True
    assert keyed["structuredContent"]["error"] == {
        "code": "person_required",
        "message": "no",
        "outcome": "escalated",
    }
