"""WI-2833: an explicit `mcp_set="full"` reaches the engine; unset sends nothing."""

from __future__ import annotations

import json
from typing import Any

from vogt.adapters.engine.client import EngineClient


def _spec(**kwargs: Any) -> dict[str, Any]:
    sent: dict[str, Any] = {}

    def transport(
        url: str, headers: dict[str, str], body: bytes = b"", method: str = "GET"
    ) -> tuple[int, bytes]:
        sent.update(json.loads(body))
        return 200, b"{}"

    client = EngineClient(base_url="http://127.0.0.1:8910", transport=transport)
    client.create_session(name="n", cwd="/w", **kwargs)
    return sent


def test_an_explicit_full_set_is_sent() -> None:
    assert _spec(mcp_set="full")["mcp_set"] == "full"


def test_a_worker_set_is_sent() -> None:
    assert _spec(mcp_set="worker")["mcp_set"] == "worker"


def test_an_unset_set_sends_nothing() -> None:
    assert "mcp_set" not in _spec()
