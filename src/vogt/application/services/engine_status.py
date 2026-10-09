"""The session engine's operational report, on the three surfaces.

The engine answers `GET /api/status` for the GUI's Settings panel: its build,
how many sessions, push subscriptions and GUI processes it holds, its history
archive and agent-task storage, and event subscribers that fell behind. This
puts that same report on CLI, REST and MCP, so an agent diagnosing a
deployment reads what the person reads.

No engine, or one that does not answer, is a stated answer rather than an
error, as `agent_cli.list` says it.
"""

from __future__ import annotations

from typing import Any

from vogt.adapters.engine import EngineUnavailable
from vogt.application.context import AppContext
from vogt.application.models import (
    EngineAgentTaskStatus,
    EngineHistoryStatus,
    EngineLag,
    EngineStatusParams,
    EngineStatusResult,
)

_NO_ENGINE = "no session engine is configured (VOGT_ENGINE_URL is unset)"


def engine_status(ctx: AppContext, params: EngineStatusParams) -> EngineStatusResult:
    """What the engine reports about itself right now."""
    del params
    if ctx.engine is None:
        return EngineStatusResult(engine=_NO_ENGINE)
    try:
        payload = ctx.engine.operational_status()
    except EngineUnavailable as exc:
        return EngineStatusResult(engine=str(exc))
    storage = _object(payload.get("storage"))
    broker = _object(payload.get("auth_broker"))
    history = payload.get("history")
    tasks = payload.get("agent_tasks")
    return EngineStatusResult(
        version=_text(payload.get("product_version") or payload.get("version")),
        source_ref=_text(payload.get("source_ref")),
        source_sha=_text(payload.get("source_sha")),
        release_url=_text(payload.get("release_url")),
        session_count=_count(payload.get("session_count")),
        push_subscription_count=_count(payload.get("push_subscription_count")),
        gui_process_count=_count(payload.get("gui_process_count")),
        gui_stream_configured=_flag(payload.get("gui_stream_configured")),
        fcm_enabled=_flag(payload.get("fcm_enabled")),
        auto_agent_auth=_flag(broker.get("auto_agent_auth")),
        state_dir=_text(storage.get("state_dir")),
        workspace_root=_text(storage.get("workspace_root")),
        history=EngineHistoryStatus.model_validate(history)
        if isinstance(history, dict)
        else None,
        agent_tasks=EngineAgentTaskStatus.model_validate(tasks)
        if isinstance(tasks, dict)
        else None,
        event_lag={
            str(name): EngineLag.model_validate(lag)
            for name, lag in _object(payload.get("event_lag")).items()
            if isinstance(lag, dict)
        },
    )


def _object(value: object) -> dict[str, Any]:
    return value if isinstance(value, dict) else {}


def _text(value: object) -> str | None:
    return value if isinstance(value, str) and value else None


def _count(value: object) -> int | None:
    return value if isinstance(value, int) and not isinstance(value, bool) else None


def _flag(value: object) -> bool | None:
    return value if isinstance(value, bool) else None
