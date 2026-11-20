"""Admin view of the agent-auth manifest and a dry-run of a fetch decision."""

from __future__ import annotations

from typing import Any

from vogt.adapters.engine import EngineUnavailable
from vogt.application.context import AppContext
from vogt.application.models import (
    AgentAuthManifestParams,
    AgentAuthManifestResult,
    AgentAuthManifestRow,
    AgentAuthPolicyCheckParams,
    AgentAuthPolicyCheckResult,
)
from vogt.errors import InvalidRequest

_NO_ENGINE = "no session engine is configured (VOGT_ENGINE_URL is unset)"


def _row(data: dict[str, Any]) -> AgentAuthManifestRow:
    return AgentAuthManifestRow.model_validate(data)


def agent_auth_manifest(
    ctx: AppContext, params: AgentAuthManifestParams
) -> AgentAuthManifestResult:
    del params
    if ctx.engine is None:
        return AgentAuthManifestResult(engine=_NO_ENGINE)
    try:
        payload = ctx.engine.agent_auth_manifest()
    except EngineUnavailable as exc:
        return AgentAuthManifestResult(engine=str(exc))
    return AgentAuthManifestResult(
        entries=[_row(x) for x in payload.get("entries", [])]
    )


def agent_auth_policy_check(
    ctx: AppContext, params: AgentAuthPolicyCheckParams
) -> AgentAuthPolicyCheckResult:
    if ctx.engine is None:
        raise InvalidRequest(_NO_ENGINE)
    payload = ctx.engine.agent_auth_policy_check(params.model_dump(exclude_none=True))
    return AgentAuthPolicyCheckResult.model_validate(payload)
