"""session.economics reads a transcript whole and reports what it cost (C16)."""

from __future__ import annotations

import dataclasses
import json
from pathlib import Path

import pytest

from vogt.adapters.engine import EngineClient
from vogt.application.context import AppContext
from vogt.application.models import (
    RegisterProjectParams,
    SessionEconomicsParams,
    StartSessionParams,
)
from vogt.application.services import economics, register_project, start_session
from vogt.errors import NotFound

WHY = "economics test"
ROOT = "/srv/vogt"
ENGINE_ID = "0f8fad5b-d9cb-469f-a165-70867728950e"


def _line(message_id: str, text: str, *, usage: dict[str, int] | None = None) -> str:
    message: dict[str, object] = {
        "id": message_id,
        "role": "assistant",
        "content": [{"type": "text", "text": text}],
    }
    if usage is not None:
        message["usage"] = usage
    return json.dumps(
        {
            "type": "assistant",
            "timestamp": "2026-10-05T00:01:00Z",
            "cwd": ROOT,
            "message": message,
        }
    )


def _write(root: Path, conversation: str, lines: list[str]) -> None:
    from vogt.adapters import transcripts

    directory = root / transcripts.claude_key(ROOT)
    directory.mkdir(parents=True, exist_ok=True)
    (directory / f"{conversation}.jsonl").write_text(
        "\n".join(lines) + "\n", encoding="utf-8"
    )


class Engine:
    def __call__(
        self, url: str, headers: dict[str, str], body: bytes = b"", method: str = "GET"
    ) -> tuple[int, bytes]:
        path = url.split("?", 1)[0].removeprefix("http://127.0.0.1:8910")
        summary = {
            "id": ENGINE_ID,
            "name": "agent",
            "activity": "idle",
            "cwd": ROOT,
            "alive": True,
            "created_at": "2026-10-05T00:00:00Z",
            "command": "vogt-agent-auth run -- claude --session-id " + ENGINE_ID,
        }
        if method == "POST" and path == "/api/sessions":
            return 200, json.dumps(summary).encode()
        if path == f"/api/sessions/{ENGINE_ID}":
            return 200, json.dumps({"summary": summary}).encode()
        return 404, b""


@pytest.fixture
def wired(instance: AppContext, tmp_path: Path) -> AppContext:
    ctx = dataclasses.replace(
        instance,
        engine=EngineClient(base_url="http://127.0.0.1:8910", transport=Engine()),
        config=instance.config.model_copy(
            update={"session_transcript_roots": {"claude": tmp_path / "claude"}}
        ),
    )
    register_project(
        ctx, RegisterProjectParams(name="Vogt", root_path=ROOT, reason=WHY)
    )
    return ctx


def test_economics_meters_recorded_usage(wired: AppContext, tmp_path: Path) -> None:
    started = start_session(
        wired, StartSessionParams(project="vogt", template="claude", reason=WHY)
    )
    _write(
        tmp_path / "claude",
        ENGINE_ID,
        [
            _line("a", "first", usage={"input_tokens": 100, "output_tokens": 10}),
            _line(
                "b",
                "second",
                usage={
                    "input_tokens": 40,
                    "cache_read_input_tokens": 80,
                    "output_tokens": 5,
                },
            ),
            json.dumps({"type": "assistant", "isCompactSummary": True, "message": {}}),
        ],
    )
    result = economics(wired, SessionEconomicsParams(id=started.session.id))
    assert result.agent == "claude" and result.basis == "session-id"
    assert result.turns == 3
    assert result.context_tokens == 120
    assert result.peak_context_tokens == 120
    assert result.compactions == 1
    assert result.usage_measured is True
    assert result.output_tokens == 15
    assert result.cost_usd is None
    assert result.commits is None and result.files_changed is None
    assert result.budget_line == "3 turns, 1 compaction"
    assert result.detail is None


def test_no_transcript_is_an_honest_detail(wired: AppContext) -> None:
    started = start_session(wired, StartSessionParams(project="vogt", reason=WHY))
    result = economics(wired, SessionEconomicsParams(id=started.session.id))
    assert result.turns == 0
    assert result.detail is not None and "no agent transcript" in result.detail
    assert result.cost_usd is None


def test_reading_is_off_with_no_roots(instance: AppContext) -> None:
    ctx = dataclasses.replace(
        instance,
        engine=EngineClient(base_url="http://127.0.0.1:8910", transport=Engine()),
    )
    result = economics(ctx, SessionEconomicsParams(id=ENGINE_ID))
    assert result.detail is not None and "off" in result.detail


def test_an_unknown_session_is_not_found(wired: AppContext) -> None:
    with pytest.raises(NotFound):
        economics(wired, SessionEconomicsParams(id="ses_nope"))
