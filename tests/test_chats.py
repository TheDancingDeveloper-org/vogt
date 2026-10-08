"""Quick chats (WI-1097) through the core: what reaches the engine, and who
may answer a chat's approval.

The engine is stood in for by a transport that records each request, so the
tests assert the request the core makes — the relayed actor, the `person`
flag, the wait — rather than the engine's own behaviour (that is
`engine/server/tests/integration.rs`).
"""

from __future__ import annotations

import dataclasses
import json
from typing import Any

import pytest

from vogt.adapters.engine import EngineClient, EngineUnavailable
from vogt.application.context import AppContext
from vogt.application.models import (
    ChatArchiveParams,
    ChatCreateParams,
    ChatDecideParams,
    ChatGetParams,
    ChatListParams,
    ChatSendParams,
)
from vogt.application.services import (
    chat_archive,
    chat_create,
    chat_decide,
    chat_get,
    chat_list,
    chat_send,
)
from vogt.core.principal import Principal
from vogt.errors import InvalidRequest, NotFound, PersonRequired

WHY = "chat test"
CHAT = "0c0c0c0c-0000-4000-8000-000000000001"
SUMMARY = {
    "id": CHAT,
    "title": "Sydney",
    "driver": "klaudia",
    "model": "grok-4.7",
    "creator": "human:ada",
    "created_at": "2026-10-08T00:00:00Z",
    "updated_at": "2026-10-08T00:00:00Z",
    "archived": False,
    "state": "idle",
    "live": True,
    "message_count": 2,
}


class Engine:
    def __init__(self) -> None:
        self.sent: list[dict[str, Any]] = []
        self.chats_on = True

    def __call__(
        self, url: str, headers: dict[str, str], body: bytes = b"", method: str = "GET"
    ) -> tuple[int, bytes]:
        payload = json.loads(body.decode("utf-8")) if body else {}
        path, _, query = url.removeprefix("http://127.0.0.1:8910").partition("?")
        self.sent.append(
            {"path": path, "query": query, "method": method, "body": payload}
        )
        if not self.chats_on:
            return 404, b""
        if "/approvals/" in path:
            if not payload.get("person"):
                return 403, json.dumps(
                    {"error": "forbidden: person required: only a person answers"}
                ).encode()
            return 200, json.dumps(
                {
                    "id": "a1",
                    "tool_name": "Bash",
                    "summary": "make test",
                    "source": "gate",
                    "status": "allowed",
                    "requested_at": "2026-10-08T00:00:00Z",
                    "expires_at": "2026-10-08T00:10:00Z",
                }
            ).encode()
        if method == "GET" and path == "/api/chats":
            return 200, json.dumps([SUMMARY]).encode()
        entries = [
            {"seq": 1, "at": "t", "kind": "user", "text": "is bedrock in sydney?"},
            {"seq": 2, "at": "t", "kind": "assistant", "text": "yes"},
        ]
        if method == "GET":
            return 200, json.dumps(
                {**SUMMARY, "entries": entries, "approvals": []}
            ).encode()
        if path.endswith("/messages") or path == "/api/chats":
            return 200, json.dumps(
                {"chat": SUMMARY, "entries": entries, "finished": True}
            ).encode()
        return 200, json.dumps({**SUMMARY, **payload}).encode()


@pytest.fixture
def engine() -> Engine:
    return Engine()


@pytest.fixture
def wired(instance: AppContext, engine: Engine) -> AppContext:
    return dataclasses.replace(
        instance,
        engine=EngineClient(base_url="http://127.0.0.1:8910", transport=engine),
    )


def _as(ctx: AppContext, identity_ref: str, kind: str) -> AppContext:
    return dataclasses.replace(
        ctx,
        principal=Principal(
            identity_ref=identity_ref,
            kind=kind,  # type: ignore[arg-type]
            display_name=identity_ref,
        ),
    )


def _audited(ctx: AppContext) -> list[str]:
    with ctx.declared.read() as view:
        return [row.operation for row in view.list_audit(limit=50)]


def test_create_relays_the_caller_as_creator_and_is_audited(
    wired: AppContext, engine: Engine
) -> None:
    ada = _as(wired, "human:ada", "human")
    result = chat_create(
        ada, ChatCreateParams(message="is bedrock in sydney?", reason=WHY)
    )
    assert result.chat.id == CHAT
    assert engine.sent[0]["body"]["creator"] == "human:ada"
    assert engine.sent[0]["body"]["message"] == "is bedrock in sydney?"
    assert "chat.create" in _audited(wired)


def test_send_waits_for_the_reply_and_audits_bytes_not_text(
    wired: AppContext, engine: Engine
) -> None:
    result = chat_send(wired, ChatSendParams(id=CHAT, text="secret-ish", reason=WHY))
    assert result.finished is True
    assert [e.text for e in result.entries] == ["is bedrock in sydney?", "yes"]
    assert engine.sent[-1]["body"]["wait_secs"] == 60
    with wired.declared.read() as view:
        row = next(r for r in view.list_audit(limit=50) if r.operation == "chat.send")
    assert "secret-ish" not in row.model_dump_json()


@pytest.mark.parametrize(
    ("identity_ref", "kind"),
    [("agent:session:ses_1", "agent"), ("agent:engine:3f2a", "agent")],
)
def test_an_agent_cannot_answer_a_chats_approval(
    wired: AppContext, engine: Engine, identity_ref: str, kind: str
) -> None:
    agent = _as(wired, identity_ref, kind)
    with pytest.raises(PersonRequired):
        chat_decide(
            agent, ChatDecideParams(id=CHAT, approval_id="a1", allow=True, reason=WHY)
        )
    assert engine.sent[-1]["body"]["person"] is False
    assert "chat.decide" not in _audited(wired)


def test_a_person_answers_a_chats_approval(wired: AppContext, engine: Engine) -> None:
    ada = _as(wired, "human:ada", "human")
    approval = chat_decide(
        ada, ChatDecideParams(id=CHAT, approval_id="a1", allow=True, reason=WHY)
    )
    assert approval.status == "allowed"
    assert engine.sent[-1]["body"]["person"] is True
    assert "chat.decide" in _audited(wired)


def test_a_chat_id_is_a_uuid_before_it_becomes_a_path(wired: AppContext) -> None:
    with pytest.raises(InvalidRequest, match="not a chat id"):
        chat_get(wired, ChatGetParams(id="../sessions"))


def test_chats_off_is_said_rather_than_an_empty_success(
    wired: AppContext, engine: Engine
) -> None:
    engine.chats_on = False
    listed = chat_list(wired, ChatListParams())
    assert listed.available is False
    assert listed.chats == []
    with pytest.raises(NotFound, match="chats turned off"):
        chat_archive(wired, ChatArchiveParams(id=CHAT, reason=WHY))


def test_list_passes_the_search_through(wired: AppContext, engine: Engine) -> None:
    listed = chat_list(wired, ChatListParams(q="bedrock", archived="all"))
    assert listed.available is True
    assert listed.chats[0].title == "Sydney"
    assert "q=bedrock" in engine.sent[-1]["query"]
    assert "archived=all" in engine.sent[-1]["query"]


def test_no_engine_is_unavailable(instance: AppContext) -> None:
    with pytest.raises(EngineUnavailable):
        chat_list(dataclasses.replace(instance, engine=None), ChatListParams())
