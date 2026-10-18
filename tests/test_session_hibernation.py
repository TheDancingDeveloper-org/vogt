"""Hibernating and waking sessions (WI-912).

The engine is stood in for by a transport that models the one piece of state
these operations are about: whether a session is live or hibernated. What the
tests assert is what the core owns: which token the woken process holds (a
new one, for the same actor, with the old one revoked), that typing into a
hibernated session wakes it before a byte is sent, and that reading or
waiting never wakes anything.
"""

from __future__ import annotations

import dataclasses
import json
from pathlib import Path
from typing import Any

import pytest

from vogt.adapters.engine import EngineClient
from vogt.application.context import AppContext
from vogt.application.models import (
    HibernateSessionParams,
    KeepSessionAwakeParams,
    ListAuditParams,
    ListEventsParams,
    ListSessionsParams,
    RegisterProjectParams,
    ReportBlockedParams,
    SessionInputParams,
    SessionScreenParams,
    SessionWaitParams,
    SetSessionRoleParams,
    StartSessionParams,
    WakeSessionParams,
)
from vogt.application.services import (
    hibernate_session,
    keep_session_awake,
    list_audit,
    list_events,
    list_sessions,
    register_project,
    report_blocked,
    session_input,
    session_screen,
    session_wait,
    set_session_role,
    start_session,
    wake_session,
)
from vogt.core.auth import hash_token
from vogt.core.principal import Principal
from vogt.errors import Conflict, RoleRefused

WHY = "hibernation test"
ROOT = "/srv/estate/vogt"
UNLINKED = "0b0e5f0c-1111-4222-8333-944455556666"
HIBERNATED_SCREEN = ["> what next?", ""]
#: Codex screens captured live on 2026-10-16 (WI-1669); see each file.
CODEX = {
    path.stem: json.loads(path.read_text())
    for path in (Path(__file__).parent / "fixtures" / "codex_screens").glob("*.json")
}
#: The text the live captures hold in the composer.
CODEX_TEXT = "reply with the single word two"


class Engine:
    """Live and hibernated sessions, and every request made of them."""

    def __init__(self) -> None:
        self.sent: list[dict[str, Any]] = []
        self.live: set[str] = set()
        self.hibernated: set[str] = set()
        self.keep_awake: set[str] = set()
        #: Sessions nominated as oversight (WI-957).
        self.oversight: set[str] = set()
        self.counter = 0
        #: The outcome a wait answers with for a live session.
        self.wait_outcome = "ready"
        #: The agent a session reports, keyed by engine id. Default claude.
        self.agent: dict[str, str] = {}
        #: Screen reads after a wake answer `idle` this many times before
        #: `running`, so a submit that did not start a turn is visible
        #: (WI-1669).
        self.idle_reads_after_wake = 0
        self._idle_reads_left = 0
        #: A Codex fake (WI-1669): after a wake its first `swallow_enters`
        #: Enters leave the text in the composer, as a resumed Codex did
        #: live; `dialog` draws an approval dialog instead of the composer.
        self.swallow_enters = 0
        self._swallow_left = 0
        self.composer = "empty"
        self.dialog = False
        #: When set, a hibernate is refused (409) with this reason.
        self.refuse_hibernate: str | None = None
        #: When set, a wake is refused (409) with this reason and the session
        #: stays hibernated — the engine's handoff guard.
        self.refuse_wake: str | None = None
        #: The successor recorded by the last hibernate that named one.
        self.superseded_by: dict[str, str] = {}
        self.blocked: dict[str, dict[str, Any]] = {}

    def summary(self, engine_id: str) -> dict[str, Any]:
        asleep = engine_id in self.hibernated
        return {
            "id": engine_id,
            "name": "x",
            "activity": "hibernated" if asleep else "idle",
            "alive": not asleep,
            "cwd": ROOT,
            "created_at": "2026-10-05T00:00:00Z",
            "blocked": self.blocked.get(engine_id),
            "conversation": {
                "agent": self.agent.get(engine_id, "claude"),
                "id": engine_id,
            },
            "keep_awake": engine_id in self.keep_awake,
            **({"role": "oversight"} if engine_id in self.oversight else {}),
            **(
                {
                    "hibernation": {
                        "at": "2026-10-05T01:00:00Z",
                        "trigger": "manual",
                        "reason": "idle",
                        "resumable": True,
                        **(
                            {"superseded_by": self.superseded_by[engine_id]}
                            if engine_id in self.superseded_by
                            else {}
                        ),
                    }
                }
                if asleep
                else {}
            ),
        }

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
            if payload.get("role") == "oversight":
                self.oversight.add(engine_id)
                self.keep_awake.add(engine_id)
            return 200, json.dumps(self.summary(engine_id)).encode()
        if method == "GET" and path == "/api/sessions":
            ids = sorted(self.live | self.hibernated)
            return 200, json.dumps([self.summary(i) for i in ids]).encode()
        parts = path.split("/")
        if len(parts) < 4 or parts[2] != "sessions":
            return 404, b""
        engine_id = parts[3]
        verb = parts[4] if len(parts) > 4 else ""
        if engine_id not in self.live | self.hibernated:
            return 404, b""
        asleep = engine_id in self.hibernated
        conflict = 409, json.dumps({"error": f"session {engine_id} is hibernated"})
        if method == "GET" and verb == "":
            return 200, json.dumps({"summary": self.summary(engine_id)}).encode()
        if method == "POST" and verb == "hibernate":
            if self.refuse_hibernate is not None:
                return 409, json.dumps({"error": self.refuse_hibernate}).encode()
            successor = payload.get("superseded_by")
            if successor:
                self.superseded_by[engine_id] = successor
            self.live.discard(engine_id)
            self.hibernated.add(engine_id)
            return 200, json.dumps(self.summary(engine_id)).encode()
        if method == "POST" and verb == "wake":
            if self.refuse_wake is not None:
                return 409, json.dumps({"error": self.refuse_wake}).encode()
            self.hibernated.discard(engine_id)
            self.live.add(engine_id)
            self._idle_reads_left = self.idle_reads_after_wake
            self._swallow_left = self.swallow_enters
            self.composer = "empty"
            return 200, json.dumps(self.summary(engine_id)).encode()
        if method == "POST" and verb == "blocked":
            if payload.get("blocked"):
                self.blocked[engine_id] = {
                    "reason": payload["reason"],
                    "items": payload.get("items", []),
                    "since": "2026-10-18T09:00:00Z",
                }
            else:
                self.blocked.pop(engine_id, None)
            return 200, json.dumps(self.summary(engine_id)).encode()
        if method == "POST" and verb == "keep-awake":
            if payload.get("keep_awake"):
                self.keep_awake.add(engine_id)
            else:
                self.keep_awake.discard(engine_id)
            return 200, json.dumps(self.summary(engine_id)).encode()
        if method == "POST" and verb == "role":
            if payload.get("role") == "oversight":
                self.oversight.add(engine_id)
                self.keep_awake.add(engine_id)
            elif engine_id in self.oversight:
                # Removing oversight lifts the pin it set (WI-1091).
                self.oversight.discard(engine_id)
                self.keep_awake.discard(engine_id)
            return 200, json.dumps(self.summary(engine_id)).encode()
        if method == "POST" and verb == "input":
            if asleep:
                return conflict[0], conflict[1].encode()
            if payload.get("text"):
                self.composer = "held"
            if payload.get("submit") and self.composer == "held" and not self.dialog:
                if self._swallow_left > 0:
                    self._swallow_left -= 1
                else:
                    self.composer = "taken"
            return 200, b'{"ok":true}'
        if method == "GET" and verb == "screen":
            if not asleep and self._idle_reads_left > 0:
                self._idle_reads_left -= 1
                activity = "idle"
            elif asleep:
                activity = "hibernated"
            else:
                activity = "running"
            lines = HIBERNATED_SCREEN if asleep else ["> ", ""]
            if not asleep and self.agent.get(engine_id) == "codex":
                if self.dialog:
                    shot = CODEX["approval_dialog"]
                elif self.composer == "held":
                    shot = CODEX["resumed_stuck_ready"]
                else:
                    shot = CODEX["resumed_cleared"]
                activity, lines = shot["activity"], shot["lines"]
            return 200, json.dumps(
                {
                    "id": engine_id,
                    "cols": 80,
                    "rows": 2,
                    "lines": lines,
                    "cursor": {"row": 0, "col": 2},
                    "activity": activity,
                    "alive": not asleep,
                    "ready": not asleep,
                }
            ).encode()
        if method == "GET" and verb == "wait":
            if asleep:
                return conflict[0], conflict[1].encode()
            return 200, json.dumps(
                {
                    "outcome": self.wait_outcome,
                    "matched": self.wait_outcome == "ready",
                    "waited_ms": 10,
                    "screen": {
                        "id": engine_id,
                        "lines": ["> "],
                        "activity": "idle",
                        "alive": True,
                        "ready": self.wait_outcome == "ready",
                    },
                }
            ).encode()
        if method == "POST" and verb == "kill":
            self.live.discard(engine_id)
            self.hibernated.discard(engine_id)
            return 200, b'{"ok":true}'
        return 404, b""

    def calls(self, engine_id: str) -> list[str]:
        """`METHOD verb` for every request about one session, in order."""
        out = []
        for row in self.sent:
            parts = row["path"].split("/")
            if len(parts) >= 4 and parts[3] == engine_id:
                out.append(f"{row['method']} {parts[4] if len(parts) > 4 else ''}")
        return out

    def last_wake_env(self) -> dict[str, str]:
        wakes = [
            r
            for r in self.sent
            if r["method"] == "POST" and r["path"].endswith("/wake")
        ]
        return dict(wakes[-1]["body"].get("env", []))

    def start_env(self) -> dict[str, str]:
        starts = [
            r
            for r in self.sent
            if r["method"] == "POST" and r["path"] == "/api/sessions"
        ]
        return dict(starts[-1]["body"]["env"])


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
    result = start_session(
        ctx, StartSessionParams(project="vogt", template="claude", reason=WHY)
    )
    return result.session.id, result.session.engine_session_id


def _token_revoked(ctx: AppContext, secret: str) -> bool:
    with ctx.declared.read() as view:
        token = view.token_by_hash(hash_token(secret))
    assert token is not None
    return token.revoked_at is not None


def test_hibernating_revokes_the_token_and_reports_the_hibernation(
    wired: AppContext, engine: Engine
) -> None:
    ses_id, engine_id = _started(wired)
    first = engine.start_env()["VOGT_HTTP_TOKEN"]

    result = hibernate_session(wired, HibernateSessionParams(id=ses_id, reason=WHY))

    assert engine_id in engine.hibernated
    hibernate = next(r for r in engine.sent if r["path"].endswith("/hibernate"))
    assert hibernate["body"] == {"allow_shell": False, "reason": WHY}
    assert result.session.activity == "hibernated"
    assert result.session.alive is False
    assert result.session.hibernation is not None
    assert result.session.hibernation.trigger == "manual"
    assert result.session.conversation_id == engine_id
    assert _token_revoked(wired, first), "nothing runs to hold it while asleep"
    audit = list_audit(wired, ListAuditParams(limit=5)).records
    assert audit[0].operation == "session.hibernate"


def test_waking_mints_a_new_token_for_the_same_actor(
    wired: AppContext, engine: Engine
) -> None:
    ses_id, engine_id = _started(wired)
    first = engine.start_env()["VOGT_HTTP_TOKEN"]
    hibernate_session(wired, HibernateSessionParams(id=ses_id, reason=WHY))

    result = wake_session(wired, WakeSessionParams(id=ses_id, reason=WHY))

    assert engine_id in engine.live
    env = engine.last_wake_env()
    second = env["VOGT_HTTP_TOKEN"]
    assert second != first
    assert env["VOGT_SESSION_ID"] == ses_id
    with wired.declared.read() as view:
        token = view.token_by_hash(hash_token(second))
    assert token is not None and token.revoked_at is None
    assert token.actor_identity_ref == f"agent:session:{ses_id}", (
        "the attribution is unchanged: the same actor, a new credential"
    )
    assert _token_revoked(wired, first)
    assert result.session.alive is True
    assert result.session.id == ses_id
    audit = list_audit(wired, ListAuditParams(limit=5)).records
    assert audit[0].operation == "session.wake"
    # The secret is in neither the audit trail nor anything it summarises.
    assert second not in json.dumps([row.model_dump(mode="json") for row in audit])


def test_waking_a_live_session_changes_nothing(
    wired: AppContext, engine: Engine
) -> None:
    ses_id, engine_id = _started(wired)
    result = wake_session(wired, WakeSessionParams(id=ses_id, reason=WHY))
    assert "POST wake" not in engine.calls(engine_id)
    assert result.session.alive is True


def test_waking_clears_the_blocked_report_it_slept_with(
    wired: AppContext, engine: Engine
) -> None:
    """WI-1834: whoever wakes a session is acting on it; the report goes, and
    the audit keeps it."""
    ses_id, engine_id = _started(wired)
    report_blocked(
        wired, ReportBlockedParams(id=ses_id, blocker="waiting on go-ahead", reason=WHY)
    )
    hibernate_session(wired, HibernateSessionParams(id=ses_id, reason=WHY))

    result = wake_session(wired, WakeSessionParams(id=ses_id, reason=WHY))

    assert engine_id not in engine.blocked
    assert result.session.alive is True
    assert result.session.blocked is None
    cleared = list_audit(
        wired, ListAuditParams(entity_id=ses_id, operation="session.report_unblocked")
    ).records
    assert [row.reason for row in cleared] == [f"cleared by wake: {WHY}"]


def test_typing_into_a_hibernated_session_wakes_it_first(
    wired: AppContext, engine: Engine
) -> None:
    ses_id, engine_id = _started(wired)
    hibernate_session(wired, HibernateSessionParams(id=ses_id, reason=WHY))
    engine.sent.clear()

    result = session_input(
        wired,
        SessionInputParams(
            id=ses_id, text="go on", submit=True, confirm=False, reason=WHY
        ),
    )

    assert result.woke is True
    assert engine.calls(engine_id) == [
        "POST input",  # refused: hibernated
        "GET ",  # is it hibernated?
        "GET ",  # _wake reads it again
        "POST wake",
        "GET wait",  # until ready
        "POST input",
        "POST input",
        "GET ",  # the digest row reads the session once more
    ]


def _submits(engine: Engine) -> list[dict[str, Any]]:
    return [
        row
        for row in engine.sent
        if row["path"].endswith("/input") and row["body"].get("submit")
    ]


def test_a_delivered_submit_is_not_pressed_again(
    wired: AppContext, engine: Engine, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The retry exists for a swallowed Enter, not for one that landed."""
    import vogt.application.services.sessions as sessions

    monkeypatch.setattr(sessions, "_CONFIRM_READS", 2)
    monkeypatch.setattr(sessions, "_CONFIRM_PAUSE_S", 0)
    ses_id, engine_id = _started(wired)
    engine.agent[engine_id] = "codex"
    hibernate_session(wired, HibernateSessionParams(id=ses_id, reason=WHY))
    engine.idle_reads_after_wake = 0
    engine.sent.clear()

    result = session_input(
        wired,
        SessionInputParams(
            id=ses_id, text="go on", submit=True, confirm=True, reason=WHY
        ),
    )

    assert len(_submits(engine)) == 1
    assert result.delivery == "delivered"
    assert result.woke is True


def test_a_non_codex_session_never_gets_the_retry(
    wired: AppContext, engine: Engine, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Claude stays on one Enter even when the confirm reads stay idle.

    A second Enter there can accept a dialog the screen parser missed.
    """
    import vogt.application.services.sessions as sessions

    monkeypatch.setattr(sessions, "_CONFIRM_READS", 2)
    monkeypatch.setattr(sessions, "_CONFIRM_PAUSE_S", 0)
    ses_id, engine_id = _started(wired)
    assert engine.agent.get(engine_id, "claude") == "claude"
    hibernate_session(wired, HibernateSessionParams(id=ses_id, reason=WHY))
    engine.idle_reads_after_wake = 2
    engine.sent.clear()

    result = session_input(
        wired,
        SessionInputParams(
            id=ses_id, text="go on", submit=True, confirm=True, reason=WHY
        ),
    )

    assert len(_submits(engine)) == 1
    assert result.delivery == "unconfirmed"
    assert result.woke is True


def _send_to_resumed_codex(
    wired: AppContext,
    engine: Engine,
    monkeypatch: pytest.MonkeyPatch,
    *,
    swallow: int = 0,
    dialog: bool = False,
) -> Any:
    """Hibernate a Codex session, then type into it with Enter (WI-1669)."""
    import vogt.application.services.sessions as sessions

    monkeypatch.setattr(sessions, "_CONFIRM_READS", 2)
    monkeypatch.setattr(sessions, "_CONFIRM_PAUSE_S", 0)
    ses_id, engine_id = _started(wired)
    engine.agent[engine_id] = "codex"
    hibernate_session(wired, HibernateSessionParams(id=ses_id, reason=WHY))
    engine.swallow_enters = swallow
    engine.dialog = dialog
    engine.sent.clear()
    return session_input(
        wired,
        SessionInputParams(
            id=ses_id, text=CODEX_TEXT, submit=True, confirm=True, reason=WHY
        ),
    )


def test_a_swallowed_enter_is_pressed_again_until_the_turn_starts(
    wired: AppContext, engine: Engine, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A resumed Codex swallowed the Enter and read as working (WI-1669).

    Captured live: the text stayed in the composer until a later Enter.
    """
    result = _send_to_resumed_codex(wired, engine, monkeypatch, swallow=1)
    assert len(_submits(engine)) == 2
    assert result.delivery == "delivered"
    assert result.woke is True


def test_the_enters_stop_as_soon_as_the_composer_clears(
    wired: AppContext, engine: Engine, monkeypatch: pytest.MonkeyPatch
) -> None:
    result = _send_to_resumed_codex(wired, engine, monkeypatch, swallow=2)
    assert len(_submits(engine)) == 3
    assert result.delivery == "delivered"


def test_the_enters_are_bounded(
    wired: AppContext, engine: Engine, monkeypatch: pytest.MonkeyPatch
) -> None:
    """A composer that never clears gets three Enters in all, not more."""
    result = _send_to_resumed_codex(wired, engine, monkeypatch, swallow=10)
    assert len(_submits(engine)) == 3
    assert result.delivery == "unconfirmed"
    assert "composer" in result.delivery_evidence


def test_no_enter_is_pressed_again_on_a_dialog(
    wired: AppContext, engine: Engine, monkeypatch: pytest.MonkeyPatch
) -> None:
    """An approval dialog's highlighted choice starts with the composer's
    glyph; another Enter there would accept it."""
    result = _send_to_resumed_codex(wired, engine, monkeypatch, dialog=True)
    assert len(_submits(engine)) == 1
    assert result.delivery == "delivered", "the agent is asking for approval"


def test_a_woken_session_that_is_not_ready_is_not_typed_into(
    wired: AppContext, engine: Engine
) -> None:
    ses_id, engine_id = _started(wired)
    hibernate_session(wired, HibernateSessionParams(id=ses_id, reason=WHY))
    engine.wait_outcome = "awaiting-approval"
    engine.sent.clear()

    with pytest.raises(Conflict, match="not ready for input"):
        session_input(wired, SessionInputParams(id=ses_id, text="go", reason=WHY))

    assert engine.calls(engine_id).count("POST input") == 1, "only the refused one"
    assert engine_id in engine.live, "it was woken; only the typing was held back"


def test_reading_and_waiting_never_wake_a_session(
    wired: AppContext, engine: Engine
) -> None:
    ses_id, engine_id = _started(wired)
    hibernate_session(wired, HibernateSessionParams(id=ses_id, reason=WHY))

    screen = session_screen(wired, SessionScreenParams(id=ses_id))
    waited = session_wait(
        wired, SessionWaitParams(id=ses_id, timeout_s=30, screen=True)
    )

    assert screen.activity == "hibernated"
    assert screen.lines == HIBERNATED_SCREEN
    assert waited.outcome == "hibernated"
    assert waited.matched is False
    assert waited.waited_ms == 0
    assert waited.screen is not None and waited.screen.lines == HIBERNATED_SCREEN
    assert "POST wake" not in engine.calls(engine_id)
    assert engine_id in engine.hibernated


def _live_tokens(ctx: AppContext, actor_ref: str) -> int:
    with ctx.declared.read() as view:
        actor = view.actor_by_identity(actor_ref)
        assert actor is not None
        return sum(
            1
            for token in view.tokens_for_actor(actor.id, include_revoked=True)
            if token.revoked_at is None
        )


def test_a_refused_wake_reaches_session_input_and_leaves_no_token(
    wired: AppContext, engine: Engine
) -> None:
    """WI-1251: the engine refuses to wake a session whose work was handed on.
    session_input never forces, so the refusal is the answer, the session stays
    asleep, and the credential minted for the wake is never stored."""
    ses_id, engine_id = _started(wired)
    hibernate_session(wired, HibernateSessionParams(id=ses_id, reason=WHY))
    actor = f"agent:session:{ses_id}"
    before = _live_tokens(wired, actor)
    engine.refuse_wake = f"session {engine_id} was superseded by {UNLINKED}"

    with pytest.raises(Conflict, match="superseded"):
        session_input(wired, SessionInputParams(id=ses_id, text="go on", reason=WHY))

    assert engine_id in engine.hibernated
    assert _live_tokens(wired, actor) == before
    wakes = [r for r in engine.sent if r["path"].endswith("/wake")]
    assert wakes[-1]["body"].get("force") is not True


def test_a_forced_wake_is_sent_and_succeeds(wired: AppContext, engine: Engine) -> None:
    ses_id, engine_id = _started(wired)
    hibernate_session(wired, HibernateSessionParams(id=ses_id, reason=WHY))
    person = dataclasses.replace(
        wired,
        principal=Principal(
            identity_ref="local:someone", kind="human", display_name="someone"
        ),
    )

    result = wake_session(person, WakeSessionParams(id=ses_id, force=True, reason=WHY))

    wakes = [r for r in engine.sent if r["path"].endswith("/wake")]
    assert wakes[-1]["body"]["force"] is True
    assert engine_id in engine.live
    assert result.session.alive is True
    recorded = [
        e
        for e in list_events(wired, ListEventsParams()).events
        if e.kind == "session.woken"
    ]
    assert recorded[-1].summary["forced"] is True


def test_an_agent_may_not_force_a_wake(wired: AppContext, engine: Engine) -> None:
    """force overrides the handoff guard, so a worker agent is refused before
    the engine is asked. An oversight session may."""
    ses_id, _ = _started(wired)
    hibernate_session(wired, HibernateSessionParams(id=ses_id, reason=WHY))
    sent = len(engine.sent)
    as_worker = dataclasses.replace(
        wired,
        principal=Principal(
            identity_ref="agent:session:ses_worker",
            kind="agent",
            display_name="worker",
        ),
    )

    with pytest.raises(RoleRefused, match="oversight"):
        wake_session(as_worker, WakeSessionParams(id=ses_id, force=True, reason=WHY))

    assert len(engine.sent) == sent, "the refusal is the core's, not the engine's"

    overseer, overseer_engine = _started(wired)
    engine.oversight.add(overseer_engine)
    as_overseer = dataclasses.replace(
        wired,
        principal=Principal(
            identity_ref=f"agent:session:{overseer}",
            kind="agent",
            display_name="overseer",
        ),
    )
    woken = wake_session(
        as_overseer, WakeSessionParams(id=ses_id, force=True, reason=WHY)
    )
    assert woken.session.alive is True


def test_a_refused_wake_names_the_successor_session(
    wired: AppContext, engine: Engine
) -> None:
    """The engine refuses naming the successor's uuid; the core says its ses_ id
    when one is linked, and leaves the uuid when none is."""
    first, first_engine = _started(wired)
    second, second_engine = _started(wired)
    hibernate_session(wired, HibernateSessionParams(id=first, reason=WHY))
    engine.refuse_wake = (
        f"session {first_engine} was superseded by {second_engine}; "
        "send to the successor instead"
    )

    with pytest.raises(Conflict, match=second):
        wake_session(wired, WakeSessionParams(id=first, reason=WHY))

    engine.refuse_wake = (
        f"session {first_engine} was superseded by {UNLINKED}; "
        "send to the successor instead"
    )
    with pytest.raises(Conflict, match=UNLINKED):
        wake_session(wired, WakeSessionParams(id=first, reason=WHY))


def test_hibernate_names_the_successor_by_its_engine_id(
    wired: AppContext, engine: Engine
) -> None:
    """superseded_by is a ses_… id; the engine is told the UUID it knows."""
    first, _ = _started(wired)
    second, second_engine = _started(wired)

    result = hibernate_session(
        wired,
        HibernateSessionParams(id=first, superseded_by=second, reason=WHY),
    )

    hibernate = next(
        r for r in reversed(engine.sent) if r["path"].endswith("/hibernate")
    )
    assert hibernate["body"]["superseded_by"] == second_engine
    assert result.session.hibernation is not None
    assert result.session.hibernation.superseded_by == second_engine
    recorded = [
        e
        for e in list_events(wired, ListEventsParams()).events
        if e.kind == "session.hibernated"
    ]
    assert recorded[-1].summary["superseded_by"] == second_engine
    ses_id, _ = _started(wired)
    engine.refuse_hibernate = "the engine does not know an agent conversation"
    with pytest.raises(Conflict, match="agent conversation"):
        hibernate_session(wired, HibernateSessionParams(id=ses_id, reason=WHY))


def test_hibernated_sessions_are_listed_linked_or_not(
    wired: AppContext, engine: Engine
) -> None:
    ses_id, _ = _started(wired)
    hibernate_session(wired, HibernateSessionParams(id=ses_id, reason=WHY))
    engine.hibernated.add(UNLINKED)

    rows = {
        row.id: row
        for row in list_sessions(wired, ListSessionsParams(mode="full")).sessions
    }

    assert rows[ses_id].activity == "hibernated"
    assert rows[ses_id].hibernation is not None
    assert UNLINKED in rows, "asleep is not stopped: listed without include_stopped"
    assert rows[UNLINKED].linked is False
    assert rows[UNLINKED].hibernation is not None


def test_an_unlinked_session_wakes_without_a_token(
    wired: AppContext, engine: Engine
) -> None:
    engine.hibernated.add(UNLINKED)
    result = wake_session(wired, WakeSessionParams(id=UNLINKED, reason=WHY))
    assert result.session.linked is False
    assert engine.last_wake_env() == {}, "Vogt holds no credential for it"


def test_keep_awake_is_passed_through_and_audited(
    wired: AppContext, engine: Engine
) -> None:
    ses_id, engine_id = _started(wired)
    result = keep_session_awake(
        wired, KeepSessionAwakeParams(id=ses_id, keep_awake=True, reason=WHY)
    )
    assert engine_id in engine.keep_awake
    assert result.session.keep_awake is True
    audit = list_audit(wired, ListAuditParams(limit=5)).records
    assert audit[0].operation == "session.keep_awake"


def test_an_oversight_session_is_nominated_at_start_and_pinned(
    wired: AppContext, engine: Engine
) -> None:
    """WI-957: role=oversight reaches the engine, which pins it awake; the
    start's audit row names the role, so the overseer is findable later."""
    result = start_session(
        wired,
        StartSessionParams(
            project="vogt", template="claude", role="oversight", reason=WHY
        ),
    )
    engine_id = result.session.engine_session_id
    starts = [r for r in engine.sent if r["path"] == "/api/sessions"]
    assert starts[-1]["body"]["role"] == "oversight"
    assert result.session.role == "oversight"
    assert result.session.keep_awake is True
    started = [
        e
        for e in list_events(wired, ListEventsParams()).events
        if e.kind == "session.started"
    ]
    assert started[-1].summary["role"] == "oversight"

    # A worker is the request this client has always made.
    _started(wired)
    starts = [r for r in engine.sent if r["path"] == "/api/sessions"]
    assert "role" not in starts[-1]["body"]
    rows = {
        row.engine_session_id: row
        for row in list_sessions(wired, ListSessionsParams(mode="full")).sessions
    }
    assert rows[engine_id].role == "oversight"
    assert {row.role for row in rows.values()} == {"oversight", "worker"}


def test_an_agent_cannot_start_an_oversight_session(
    wired: AppContext, engine: Engine
) -> None:
    """WI-1255: an agent that could start an overseer would get its standing
    by proxy — start one, then drive its terminal. Refused before the engine
    is asked; a worker start by the same agent still works, and the engine's
    own credential (relaying the break-glass operator) may start one."""
    agent = dataclasses.replace(
        wired,
        principal=Principal(
            identity_ref="agent:session:ses_worker", kind="agent", display_name="w"
        ),
    )
    sent_before = len(engine.sent)
    with pytest.raises(RoleRefused, match="only a person starts an oversight"):
        start_session(
            agent,
            StartSessionParams(
                project="vogt", template="claude", role="oversight", reason=WHY
            ),
        )
    assert [r for r in engine.sent[sent_before:] if r["path"] == "/api/sessions"] == []

    worker = start_session(
        agent, StartSessionParams(project="vogt", template="claude", reason=WHY)
    )
    assert worker.session.role == "worker"

    core = dataclasses.replace(
        wired,
        principal=Principal(
            identity_ref=wired.config.bootstrap_core_token_actor,
            kind="agent",
            display_name="engine",
        ),
    )
    overseer = start_session(
        core,
        StartSessionParams(
            project="vogt", template="claude", role="oversight", reason=WHY
        ),
    )
    assert overseer.session.role == "oversight"


def test_set_role_refusal_no_longer_suggests_starting_an_overseer(
    wired: AppContext,
) -> None:
    ses_id, _ = _started(wired)
    agent = dataclasses.replace(
        wired,
        principal=Principal(
            identity_ref="agent:session:ses_worker", kind="agent", display_name="w"
        ),
    )
    with pytest.raises(RoleRefused) as refused:
        set_session_role(
            agent, SetSessionRoleParams(id=ses_id, role="oversight", reason=WHY)
        )
    assert "role=oversight" not in str(refused.value)


def test_set_role_nominates_any_session_and_is_audited(
    wired: AppContext, engine: Engine
) -> None:
    ses_id, engine_id = _started(wired)
    result = set_session_role(
        wired, SetSessionRoleParams(id=ses_id, role="oversight", reason=WHY)
    )
    assert engine_id in engine.oversight
    assert result.session.role == "oversight"
    assert result.session.keep_awake is True
    audit = list_audit(wired, ListAuditParams(limit=5)).records
    assert audit[0].operation == "session.set_role"

    # A session the GUI started (no link in the core) can be nominated too:
    # the role lives with the engine.
    engine.live.add(UNLINKED)
    unlinked = set_session_role(
        wired, SetSessionRoleParams(id=UNLINKED, role="oversight", reason=WHY)
    )
    assert unlinked.session.linked is False
    assert unlinked.session.role == "oversight"

    back = set_session_role(
        wired, SetSessionRoleParams(id=ses_id, role="worker", reason=WHY)
    )
    assert back.session.role == "worker"
    assert back.session.keep_awake is False, "removing oversight lifts its pin"
