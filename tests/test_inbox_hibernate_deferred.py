"""WI-4274: a deferred idle hibernate shows one Inbox entry, gone when cleared."""

from __future__ import annotations

import dataclasses
import json
from typing import Any

from vogt.adapters.engine import EngineClient
from vogt.application.context import AppContext
from vogt.application.models import InboxListParams
from vogt.application.services.inbox import list_inbox

SID = "00000000-0000-4000-8000-000000000001"


def _ctx(instance: AppContext, row: dict[str, Any]) -> AppContext:
    def transport(
        url: str, headers: dict[str, str], body: bytes = b"", method: str = "GET"
    ) -> tuple[int, bytes]:
        if url.endswith("/api/sessions"):
            return 200, json.dumps({"sessions": [row], "hidden_count": 0}).encode()
        return 404, b""

    return dataclasses.replace(
        instance,
        engine=EngineClient(base_url="http://127.0.0.1:8910", transport=transport),
    )


def _entries(ctx: AppContext) -> list[Any]:
    result = list_inbox(ctx, InboxListParams())
    return [e for e in result.entries if e.kind == "session.hibernate_deferred"]


def test_deferred_session_has_one_entry_that_clears(instance: AppContext) -> None:
    row: dict[str, Any] = {
        "id": SID,
        "name": "worker",
        "activity": "idle",
        "alive": True,
        "cwd": "/srv/x",
        "hibernate_deferred": {
            "reason": "save failed",
            "failures": 2,
            "at": "2026-11-15T01:00:00Z",
        },
    }
    ctx = _ctx(instance, row)
    entries = _entries(ctx)
    assert len(entries) == 1
    assert "save failed" in entries[0].summary
    assert "backoff" in entries[0].summary
    assert "force=true" in entries[0].summary
    assert entries[0].session_id == SID

    del row["hibernate_deferred"]
    assert _entries(ctx) == []


def test_paused_deferral_says_so(instance: AppContext) -> None:
    row = {
        "id": SID,
        "name": "w",
        "activity": "idle",
        "alive": True,
        "cwd": "/srv/x",
        "hibernate_deferred": {"reason": "r", "failures": 3, "paused": True},
    }
    (entry,) = _entries(_ctx(instance, row))
    assert "paused" in entry.summary
