"""Group 11 — economics and usage reads."""

from __future__ import annotations

from ..harness import Case, Check, Context, ToolError, failed, passed, skipped


def usage_board(ctx: Context) -> Check:
    try:
        payload = ctx.client.tool("usage_board")
    except ToolError as exc:
        text = str(exc).lower()
        if "not configured" in text:
            return skipped(f"the usage board is not configured (B2): {exc}")
        return failed(f"usage_board refused: {exc}")
    accounts = payload.get("accounts") or []
    keys = payload.get("keys") or []
    detail = payload.get("detail")
    if "not configured" in str(detail).lower():
        return skipped(f"the usage board is not configured (B2): {detail}")
    return passed(
        f"usage_board returned {len(accounts)} account(s) and {len(keys)} key row(s)"
    )


def session_economics(ctx: Context) -> Check:
    ctx.require_sessions()
    identifier = ctx.state.get("sessions.codex")
    if not isinstance(identifier, str):
        return skipped("no Codex session was started")
    payload = ctx.client.tool("session_economics", {"id": identifier})
    if not payload.get("id"):
        return failed(f"session_economics returned no id: {payload!r}")
    return passed(
        f"session_economics turns={payload.get('turns')} "
        f"context_tokens={payload.get('context_tokens')} "
        f"usage_measured={payload.get('usage_measured')}"
    )


CASES = [
    Case("E1", "budget", "B2", "high", "the usage board read returns", usage_board),
    Case(
        "E2",
        "budget",
        "B1",
        "high",
        "session economics reads for a live session",
        session_economics,
    ),
]
