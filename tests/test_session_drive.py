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
    SessionInputParams,
    SessionScreenParams,
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
    session_input,
    session_screen,
    session_wait,
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


def test_input_accepts_an_unlinked_engine_uuid(
    wired: AppContext, engine: Engine
) -> None:
    result = session_input(
        wired, SessionInputParams(id=UNLINKED, text="hi", reason=WHY)
    )
    assert result.linked is False
    assert engine.inputs() == [(UNLINKED, {"text": "hi", "submit": False})]


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
    rows = list_sessions(wired, ListSessionsParams()).sessions
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
        wired, SessionWaitParams(id=ses_id, until="any_change", timeout_s=30)
    )
    assert engine.wait_queries[-1] == "until=any-change&timeout_s=30"
    assert waited.engine_session_id == engine_id
    assert waited.outcome == "awaiting-approval"
    assert waited.matched is False
    assert waited.waited_ms == 1234
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
        r for r in list_sessions(wired, ListSessionsParams()).sessions if r.id == ses_id
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
    assert DRIVING_OTHER_SESSIONS in create["body"]["prompt"]


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
