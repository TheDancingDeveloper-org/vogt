"""session.digest reads one engine row and meters nothing (C3)."""

from __future__ import annotations

import dataclasses
import json
from datetime import UTC, datetime
from pathlib import Path

import pytest

from vogt.adapters import transcripts
from vogt.adapters.engine import EngineClient
from vogt.application.context import AppContext
from vogt.application.models import SessionDigestParams
from vogt.application.services import digest
from vogt.errors import NotFound

ENGINE_ID = "0f8fad5b-d9cb-469f-a165-70867728950e"


class Engine:
    """Returns one summary for GET /api/sessions/{ENGINE_ID} and nothing else."""

    def __init__(self, summary: dict[str, object]) -> None:
        self.summary = summary

    def __call__(
        self, url: str, headers: dict[str, str], body: bytes = b"", method: str = "GET"
    ) -> tuple[int, bytes]:
        path = url.split("?", 1)[0].removeprefix("http://127.0.0.1:8910")
        if method == "GET" and path == f"/api/sessions/{ENGINE_ID}":
            return 200, json.dumps({"summary": self.summary}).encode()
        return 404, b""


def _row(**extra: object) -> dict[str, object]:
    summary: dict[str, object] = {
        "id": ENGINE_ID,
        "name": "agent",
        "activity": "idle",
        "cwd": "/srv/vogt",
        "alive": True,
        "created_at": "2026-10-05T00:00:00Z",
        "activity_changed_at": "2026-10-05T00:02:00Z",
        "work_item": "WI-7",
    }
    summary.update(extra)
    return summary


def _wired(instance: AppContext, summary: dict[str, object]) -> AppContext:
    return dataclasses.replace(
        instance,
        engine=EngineClient(
            base_url="http://127.0.0.1:8910", transport=Engine(summary)
        ),
    )


def test_digest_reads_economics_the_row_already_carries(instance: AppContext) -> None:
    ctx = _wired(
        instance,
        _row(
            economics={
                "turns": 96,
                "context_tokens": 122_000,
                "peak_context_tokens": 180_000,
                "compactions": 2,
                "output_tokens": 4000,
                "usage_measured": True,
            }
        ),
    )
    result = digest(ctx, SessionDigestParams(id=ENGINE_ID, context_window=200_000))
    assert result.id == ENGINE_ID
    assert result.name == "agent"
    assert result.work_item == "WI-7"
    assert result.activity == "idle"
    assert result.turns == 96
    assert result.context_tokens == 122_000
    assert result.peak_context_tokens == 180_000
    assert result.compactions == 2
    assert result.output_tokens == 4000
    assert result.usage_measured is True
    assert result.context_pct == 61
    assert result.budget_line == "96 turns, 61% ctx, 2 compactions"
    assert result.updated_at == datetime(2026, 10, 5, 0, 2, tzinfo=UTC)
    assert result.blocked is None
    assert result.last_reply_excerpt is None


def test_absent_economics_stays_null(instance: AppContext) -> None:
    result = digest(_wired(instance, _row()), SessionDigestParams(id=ENGINE_ID))
    assert result.turns is None
    assert result.context_tokens is None
    assert result.peak_context_tokens is None
    assert result.compactions is None
    assert result.output_tokens is None
    assert result.usage_measured is None
    assert result.context_pct is None
    assert result.budget_line is None


def test_no_window_omits_the_percentage(instance: AppContext) -> None:
    ctx = _wired(
        instance,
        _row(economics={"turns": 3, "context_tokens": 120, "compactions": 1}),
    )
    result = digest(ctx, SessionDigestParams(id=ENGINE_ID))
    assert result.context_pct is None
    assert result.budget_line == "3 turns, 1 compaction"
    assert result.usage_measured is None


def test_blocked_report_carries_the_reason_and_the_items(instance: AppContext) -> None:
    ctx = _wired(
        instance,
        _row(
            blocked={
                "reason": "needs the bot token",
                "items": ["create the bot"],
                "since": "2026-10-05T00:03:00Z",
            },
        ),
    )
    result = digest(ctx, SessionDigestParams(id=ENGINE_ID))
    assert result.blocked is not None
    assert result.blocked.blocker == "needs the bot token"
    assert result.blocked.items == ["create the bot"]
    assert result.blocked.since == datetime(2026, 10, 5, 0, 3, tzinfo=UTC)
    assert result.last_reply_excerpt is None


def test_a_transcript_fills_the_excerpt(instance: AppContext, tmp_path: Path) -> None:
    root = tmp_path / "claude"
    directory = root / transcripts.claude_key("/srv/vogt")
    directory.mkdir(parents=True)
    (directory / f"{ENGINE_ID}.jsonl").write_text(
        json.dumps(
            {
                "type": "assistant",
                "timestamp": "2026-10-05T00:04:00Z",
                "cwd": "/srv/vogt",
                "message": {
                    "id": "m1",
                    "role": "assistant",
                    "content": [{"type": "text", "text": "The bot is created."}],
                },
            }
        )
        + "\n",
        encoding="utf-8",
    )
    ctx = _wired(
        instance,
        _row(command=f"claude --session-id {ENGINE_ID}", cwd="/srv/vogt"),
    )
    ctx = dataclasses.replace(
        ctx,
        config=ctx.config.model_copy(
            update={"session_transcript_roots": {"claude": root}}
        ),
    )
    result = digest(ctx, SessionDigestParams(id=ENGINE_ID))
    assert result.last_reply_excerpt == "The bot is created."


def test_an_unknown_session_is_not_found(instance: AppContext) -> None:
    ctx = _wired(instance, _row())
    with pytest.raises(NotFound):
        digest(ctx, SessionDigestParams(id="ses_nope"))
