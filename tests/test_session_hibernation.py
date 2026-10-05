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
from typing import Any

import pytest

from vogt.adapters.engine import EngineClient
from vogt.application.context import AppContext
from vogt.application.models import (
    HibernateSessionParams,
    KeepSessionAwakeParams,
    ListAuditParams,
    ListSessionsParams,
    RegisterProjectParams,
    SessionInputParams,
    SessionScreenParams,
    SessionWaitParams,
    StartSessionParams,
    WakeSessionParams,
)
from vogt.application.services import (
    hibernate_session,
    keep_session_awake,
    list_audit,
    list_sessions,
    register_project,
    session_input,
    session_screen,
    session_wait,
    start_session,
    wake_session,
)
from vogt.core.auth import hash_token
from vogt.errors import Conflict

WHY = "hibernation test"
ROOT = "/srv/estate/vogt"
UNLINKED = "0b0e5f0c-1111-4222-8333-944455556666"
HIBERNATED_SCREEN = ["> what next?", ""]


class Engine:
    """Live and hibernated sessions, and every request made of them."""

    def __init__(self) -> None:
        self.sent: list[dict[str, Any]] = []
        self.live: set[str] = set()
        self.hibernated: set[str] = set()
        self.keep_awake: set[str] = set()
        self.counter = 0
        #: The outcome a wait answers with for a live session.
        self.wait_outcome = "ready"
        #: When set, a hibernate is refused (409) with this reason.
        self.refuse_hibernate: str | None = None

    def summary(self, engine_id: str) -> dict[str, Any]:
        asleep = engine_id in self.hibernated
        return {
            "id": engine_id,
            "name": "x",
            "activity": "hibernated" if asleep else "idle",
            "alive": not asleep,
            "cwd": ROOT,
            "created_at": "2026-10-05T00:00:00Z",
            "conversation": {"agent": "claude", "id": engine_id},
            "keep_awake": engine_id in self.keep_awake,
            **(
                {
                    "hibernation": {
                        "at": "2026-10-05T01:00:00Z",
                        "trigger": "manual",
                        "reason": "idle",
                        "resumable": True,
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
            self.live.discard(engine_id)
            self.hibernated.add(engine_id)
            return 200, json.dumps(self.summary(engine_id)).encode()
        if method == "POST" and verb == "wake":
            self.hibernated.discard(engine_id)
            self.live.add(engine_id)
            return 200, json.dumps(self.summary(engine_id)).encode()
        if method == "POST" and verb == "keep-awake":
            if payload.get("keep_awake"):
                self.keep_awake.add(engine_id)
            else:
                self.keep_awake.discard(engine_id)
            return 200, json.dumps(self.summary(engine_id)).encode()
        if method == "POST" and verb == "input":
            if asleep:
                return conflict[0], conflict[1].encode()
            return 200, b'{"ok":true}'
        if method == "GET" and verb == "screen":
            return 200, json.dumps(
                {
                    "id": engine_id,
                    "cols": 80,
                    "rows": 2,
                    "lines": HIBERNATED_SCREEN if asleep else ["> ", ""],
                    "cursor": {"row": 0, "col": 2},
                    "activity": "hibernated" if asleep else "idle",
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
    ]


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
    waited = session_wait(wired, SessionWaitParams(id=ses_id, timeout_s=30))

    assert screen.activity == "hibernated"
    assert screen.lines == HIBERNATED_SCREEN
    assert waited.outcome == "hibernated"
    assert waited.matched is False
    assert waited.waited_ms == 0
    assert waited.screen.lines == HIBERNATED_SCREEN
    assert "POST wake" not in engine.calls(engine_id)
    assert engine_id in engine.hibernated


def test_a_refused_hibernation_says_why(wired: AppContext, engine: Engine) -> None:
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

    rows = {row.id: row for row in list_sessions(wired, ListSessionsParams()).sessions}

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
