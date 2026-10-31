"""Compact tool text for the write operations a driver calls in bulk (WI-1525).

A driver that starts, hibernates, binds and transitions dozens of sessions and
items keeps every result in its context and re-reads it on each later call.
The full SessionSummary or WorkItem is ~2-4 KB; what the driver goes on with
is the digest row or the item's ref, state and title. So the MCP *text* of
these operations is that digest unless the call passes `verbose: true`.
structuredContent stays the full result, so MCP, REST and the CLI agree.
"""

from __future__ import annotations

from collections.abc import Callable
from typing import Any

_SESSION_KEYS = ("id", "name", "work_item", "activity")
_WORK_KEYS = ("ref", "state", "title", "updated_at")


def _session(payload: dict[str, Any]) -> dict[str, Any]:
    """The digest row the service built, else the summary's identity."""
    digest = payload.get("digest")
    if not isinstance(digest, dict):
        session = payload.get("session") or {}
        digest = {key: session.get(key) for key in _SESSION_KEYS}
    out: dict[str, Any] = {"digest": digest}
    for key in ("previous_work_item", "project_mismatch", "engine_label"):
        if key in payload:
            out[key] = payload[key]
    return out


def _work(payload: dict[str, Any]) -> dict[str, Any]:
    item = payload.get("item") or {}
    return {key: item.get(key) for key in _WORK_KEYS}


COMPACTORS: dict[str, Callable[[dict[str, Any]], dict[str, Any]]] = {
    "session.bind_work": _session,
    "session.hibernate": _session,
    "session.start": _session,
    "session.stop": _session,
    "session.wake": _session,
    "work.create": _work,
    "work.transition": _work,
}


def text_payload(
    operation: str, arguments: dict[str, Any], payload: dict[str, Any]
) -> dict[str, Any]:
    """What the tool text carries: compact unless `verbose` was asked for."""
    compactor = COMPACTORS.get(operation)
    if compactor is None or arguments.get("verbose") is True:
        return payload
    return compactor(payload)
