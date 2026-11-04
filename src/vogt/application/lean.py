"""Lean write results over MCP (WI-1525).

A write confirms itself in a few fields by default; the full record is a
`mode=full` away. This runs on the finished, already-committed result, so it
changes what is returned and never what is written. Only the MCP adapters call
it: REST keeps the full record the GUI renders. The Rust core's
`application::lean` does the same, field for field.
"""

from __future__ import annotations

from typing import Any

#: Session fields a compact result keeps. `id`, `engine_session_id` and `cwd`
#: are what `SessionSummary` requires, so a compact result still reads as one.
_SESSION_KEEP = (
    "linked",
    "id",
    "engine_session_id",
    "cwd",
    "name",
    "project",
    "work_item",
    "activity",
    "alive",
)
_SESSION_OPS = frozenset(
    {"session.start", "session.hibernate", "session.wake", "session.bind_work"}
)
_ITEM_OPS = frozenset({"work.create", "work.update", "work.transition"})


def compact(
    operation: str, arguments: dict[str, Any] | None, payload: dict[str, Any]
) -> dict[str, Any]:
    """`payload` shrunk for `operation`, unless the caller asked for `full`."""
    asked = arguments or {}
    if asked.get("mode") == "full":
        return payload
    if operation in _SESSION_OPS:
        return _session(payload)
    if operation in _ITEM_OPS:
        return _item(payload)
    if operation == "work.comment":
        return _comment(asked, payload)
    return payload


def _empty(value: object) -> bool:
    return value is None or value in ("", [], {})


def _session(payload: dict[str, Any]) -> dict[str, Any]:
    out: dict[str, Any] = {}
    summary = payload.get("session")
    if isinstance(summary, dict):
        out["session"] = {
            key: summary[key] for key in _SESSION_KEEP if not _empty(summary.get(key))
        }
    # Whatever else says something: a bind's previous item and label outcome, a
    # start's landing rule. Nulls and defaults go; the heavy `digest` is
    # `mode=full` only.
    for key, value in payload.items():
        if key in {"session", "digest"} or _empty(value):
            continue
        if key in {"project_mismatch", "woke"} and value is False:
            continue
        out[key] = value
    return out


def _item(payload: dict[str, Any]) -> dict[str, Any]:
    item = payload.get("item")
    if not isinstance(item, dict):
        return payload
    return {
        key: item[key] for key in ("ref", "state", "title", "updated_at") if key in item
    }


def _comment(arguments: dict[str, Any], payload: dict[str, Any]) -> dict[str, Any]:
    out: dict[str, Any] = {}
    if "ref" in arguments:
        out["ref"] = arguments["ref"]
    comment = payload.get("comment")
    if isinstance(comment, dict):
        for key in ("id", "kind", "created_at"):
            if key in comment:
                out["comment_id" if key == "id" else key] = comment[key]
    # `skipped` is the ordinary answer; only a real upstream outcome is news.
    if payload.get("write_back") not in (None, "skipped"):
        out["write_back"] = payload["write_back"]
    return out
