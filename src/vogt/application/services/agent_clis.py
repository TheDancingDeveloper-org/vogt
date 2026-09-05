"""Runtime-pinned agent CLIs, as the engine reports and moves them (#590).

Two operations, both thin: the engine owns the mechanism (the versioned
prefixes under its runtime root and the one installer that flips `current`),
and Vogt's part is to put the report and the move on the three surfaces every
other operation has — CLI, REST and MCP — and to audit the move. An agent in a
session can therefore say which CLI version it is running, and an operator
can change it with a reason attached, from wherever they already are.

No engine configured is a stated answer for the read and a refusal for the
write: there is nothing to report on and nothing to move.
"""

from __future__ import annotations

from typing import Any

from vogt.adapters.engine import EngineUnavailable
from vogt.application.context import AppContext
from vogt.application.models import (
    AgentCliListParams,
    AgentCliListResult,
    AgentCliRow,
    AgentCliUpdateParams,
    AgentCliUpdateResult,
)
from vogt.application.writes import WriteOutcome, audited_write
from vogt.core.entities import Actor
from vogt.errors import InvalidRequest
from vogt.storage.interface import WriteTxn

AGENT_CLI_UPDATED_EVENT = "agent_cli.updated"
_NO_ENGINE = "no session engine is configured (VOGT_ENGINE_URL is unset)"


def _rows(payload: dict[str, Any]) -> list[AgentCliRow]:
    rows = payload.get("tools")
    if not isinstance(rows, list):
        return []
    return [AgentCliRow.model_validate(row) for row in rows if isinstance(row, dict)]


def agent_cli_list(ctx: AppContext, params: AgentCliListParams) -> AgentCliListResult:
    """What each agent CLI in the pod is: active, baked, and newest upstream."""
    if ctx.engine is None:
        return AgentCliListResult(engine=_NO_ENGINE)
    try:
        payload = ctx.engine.agent_clis(upstream=params.upstream)
    except EngineUnavailable as exc:
        return AgentCliListResult(engine=str(exc))
    return AgentCliListResult(
        tools=_rows(payload),
        installer_present=bool(payload.get("installer_present", False)),
    )


def agent_cli_update(
    ctx: AppContext, params: AgentCliUpdateParams
) -> AgentCliUpdateResult:
    """Make `version` of `tool` the one new sessions run, and audit the move.

    The engine is asked first and its refusals are raised as they are — a
    malformed version, an unknown tool, an install that failed — because a
    write that lands an audit row for a change that did not happen would be
    the record lying. Only a move the engine confirms is written down.
    """
    engine = ctx.engine
    if engine is None:
        raise InvalidRequest(_NO_ENGINE)
    payload = engine.update_agent_cli(params.tool, params.version)
    rows = _rows(payload)
    moved = next((row for row in rows if row.tool == params.tool), None)

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[AgentCliUpdateResult]:
        del txn, actor
        result = AgentCliUpdateResult(
            tool=params.tool,
            requested=params.version,
            active_version=moved.active_version if moved else None,
            source=moved.source if moved else None,
            tools=rows,
        )
        return WriteOutcome(
            result=result,
            entity_kind="agent_cli",
            entity_id=params.tool,
            payload={
                "tool": params.tool,
                "requested": params.version,
                "active_version": result.active_version,
                "source": result.source,
            },
            event_kind=AGENT_CLI_UPDATED_EVENT,
            summary={
                "tool": params.tool,
                "requested": params.version,
                "active_version": result.active_version,
            },
        )

    return audited_write(
        ctx, operation="agent_cli.update", reason=params.reason, body=body
    )


__all__ = ["AGENT_CLI_UPDATED_EVENT", "agent_cli_list", "agent_cli_update"]
