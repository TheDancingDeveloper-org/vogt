"""Runtime-pinned agent CLIs on Vogt's surfaces (#590).

The engine owns the mechanism; these pin Vogt's half — the report is passed
through as the engine gave it, an absent or silent engine is a stated answer
for the read and a refusal for the write, the engine's own refusals reach the
caller with their words intact, and a move the engine confirms is audited
while one it refused is not.
"""

from __future__ import annotations

import dataclasses
import json
from typing import Any

import pytest

from vogt.adapters.engine import EngineClient, EngineUnavailable
from vogt.application.context import AppContext
from vogt.application.models import AgentCliListParams, AgentCliUpdateParams
from vogt.application.services import agent_cli_list, agent_cli_update
from vogt.errors import Conflict, InvalidRequest, NotFound

REPORT: dict[str, Any] = {
    "root": "/opt/vogt/agent-clis",
    "installer_present": True,
    "tools": [
        {
            "tool": "claude-code",
            "package": "@anthropic-ai/claude-code",
            "binary": "claude",
            "env_var": "VOGT_CLAUDE_CODE_VERSION",
            "baked_version": "2.1.258",
            "active_version": "2.1.258",
            "source": "image",
            "installed_versions": [],
            "upstream_latest": "2.1.261",
            "update_available": True,
        },
        {
            "tool": "codex",
            "package": "@openai/codex",
            "binary": "codex",
            "env_var": "VOGT_CODEX_VERSION",
            "baked_version": "0.149.1",
            "active_version": None,
            "source": "absent",
            "installed_versions": [],
        },
    ],
}


class StandInEngine:
    """An engine whose agent CLI routes answer a scripted status."""

    def __init__(self) -> None:
        self.sent: list[dict[str, Any]] = []
        self.post_status = 200
        self.post_body: dict[str, Any] | None = None

    def __call__(
        self,
        url: str,
        headers: dict[str, str],
        body: bytes = b"",
        method: str = "GET",
    ) -> tuple[int, bytes]:
        payload = json.loads(body.decode("utf-8")) if body else {}
        self.sent.append({"url": url, "method": method, "body": payload})
        path = url.split("?", 1)[0]
        if method == "GET" and path.endswith("/api/agent-clis"):
            return 200, json.dumps(REPORT).encode()
        if method == "POST" and "/api/agent-clis/" in path:
            if self.post_status != 200:
                return self.post_status, json.dumps(self.post_body or {}).encode()
            moved = json.loads(json.dumps(REPORT))
            moved["tools"][0].update(
                {
                    "active_version": payload["version"],
                    "source": "runtime",
                    "installed_versions": [payload["version"]],
                }
            )
            return 200, json.dumps(moved).encode()
        return 404, b""


@pytest.fixture
def engine() -> StandInEngine:
    return StandInEngine()


@pytest.fixture
def wired(instance: AppContext, engine: StandInEngine) -> AppContext:
    return dataclasses.replace(
        instance,
        engine=EngineClient(base_url="http://127.0.0.1:8910", transport=engine),
    )


def test_the_report_is_passed_through_as_the_engine_gave_it(
    wired: AppContext, engine: StandInEngine
) -> None:
    result = agent_cli_list(wired, AgentCliListParams(upstream=True))
    assert result.engine is None
    assert result.installer_present is True
    assert [row.tool for row in result.tools] == ["claude-code", "codex"]
    claude = result.tools[0]
    assert claude.active_version == "2.1.258"
    assert claude.upstream_latest == "2.1.261"
    assert claude.update_available is True
    assert result.tools[1].source == "absent"
    # The upstream ask is forwarded, not decided here.
    assert engine.sent[0]["url"].endswith("/api/agent-clis?upstream=true")


def test_no_engine_is_a_stated_answer_for_the_read(instance: AppContext) -> None:
    result = agent_cli_list(instance, AgentCliListParams())
    assert result.tools == []
    assert result.engine is not None
    assert "VOGT_ENGINE_URL" in result.engine


def test_a_silent_engine_is_reported_not_raised_on_the_read(
    instance: AppContext,
) -> None:
    def dead(
        url: str, headers: dict[str, str], body: bytes = b"", method: str = "GET"
    ) -> tuple[int, bytes]:
        msg = "the engine is not answering: connection refused"
        raise EngineUnavailable(msg)

    ctx = dataclasses.replace(
        instance, engine=EngineClient(base_url="http://127.0.0.1:8910", transport=dead)
    )
    result = agent_cli_list(ctx, AgentCliListParams())
    assert result.tools == []
    assert result.engine is not None
    assert "not answering" in result.engine


def test_a_confirmed_move_is_audited_with_the_engines_answer(
    wired: AppContext, engine: StandInEngine
) -> None:
    result = agent_cli_update(
        wired,
        AgentCliUpdateParams(
            tool="claude-code", version="2.1.261", reason="a model users want"
        ),
    )
    assert result.tool == "claude-code"
    assert result.requested == "2.1.261"
    assert result.active_version == "2.1.261"
    assert result.source == "runtime"
    assert result.tools[0].installed_versions == ["2.1.261"]
    post = engine.sent[-1]
    assert post["method"] == "POST"
    assert post["url"].endswith("/api/agent-clis/claude-code")
    assert post["body"] == {"version": "2.1.261"}

    with wired.declared.read() as view:
        audit = [
            r for r in view.list_audit(limit=20) if r.operation == "agent_cli.update"
        ]
        assert len(audit) == 1
        assert audit[0].entity_kind == "agent_cli"
        assert audit[0].entity_id == "claude-code"
        assert audit[0].reason == "a model users want"
        events = [
            e
            for e in view.list_events(after=0, limit=50)
            if e.kind == "agent_cli.updated"
        ]
        assert len(events) == 1
        assert events[0].summary["requested"] == "2.1.261"


@pytest.mark.parametrize(
    ("status", "body", "error"),
    [
        (
            400,
            {"error": "bad request: version 'x' is not an exact version"},
            InvalidRequest,
        ),
        (404, {"error": "not found"}, NotFound),
        (
            409,
            {"error": "conflict: claude-code 2.1.999 was not made current"},
            Conflict,
        ),
    ],
)
def test_the_engines_refusals_reach_the_caller_and_nothing_is_audited(
    wired: AppContext,
    engine: StandInEngine,
    status: int,
    body: dict[str, str],
    error: type[Exception],
) -> None:
    engine.post_status = status
    engine.post_body = body
    with pytest.raises(error) as raised:
        agent_cli_update(
            wired,
            AgentCliUpdateParams(
                tool="claude-code", version="2.1.999", reason="try it"
            ),
        )
    if status != 404:
        assert body["error"] in str(raised.value)
    with wired.declared.read() as view:
        assert not [
            r for r in view.list_audit(limit=20) if r.operation == "agent_cli.update"
        ]


def test_a_move_needs_an_engine(instance: AppContext) -> None:
    with pytest.raises(InvalidRequest, match="VOGT_ENGINE_URL"):
        agent_cli_update(
            instance,
            AgentCliUpdateParams(tool="claude-code", version="2.1.261", reason="why"),
        )


def test_a_refused_token_is_named_as_the_capability_it_lacks(
    instance: AppContext,
) -> None:
    def forbidden(
        url: str, headers: dict[str, str], body: bytes = b"", method: str = "GET"
    ) -> tuple[int, bytes]:
        return 403, b'{"error":"forbidden"}'

    ctx = dataclasses.replace(
        instance,
        engine=EngineClient(base_url="http://127.0.0.1:8910", transport=forbidden),
    )
    with pytest.raises(EngineUnavailable, match="agent-clis-write"):
        agent_cli_update(
            ctx, AgentCliUpdateParams(tool="codex", version="0.150.0", reason="why")
        )
