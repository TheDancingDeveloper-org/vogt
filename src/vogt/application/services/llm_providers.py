"""Authenticated management of the engine's secret-free LLM registry."""

from __future__ import annotations

from typing import Any

from vogt.adapters.engine import EngineUnavailable
from vogt.application.context import AppContext
from vogt.application.models import (
    LlmProviderListResult,
    LlmProviderParams,
    LlmProviderRemoveParams,
    LlmProviderRemoveResult,
    LlmProviderRow,
    LlmProviderSetParams,
    LlmProviderSetResult,
)
from vogt.application.writes import WriteOutcome, audited_write
from vogt.core.entities import Actor
from vogt.errors import InvalidRequest
from vogt.storage.interface import WriteTxn

_NO_ENGINE = "no session engine is configured (VOGT_ENGINE_URL is unset)"


def _row(data: dict[str, Any]) -> LlmProviderRow:
    return LlmProviderRow.model_validate(data)


def llm_provider_list(
    ctx: AppContext, params: LlmProviderParams
) -> LlmProviderListResult:
    del params
    if ctx.engine is None:
        return LlmProviderListResult(engine=_NO_ENGINE)
    try:
        payload = ctx.engine.llm_providers()
    except EngineUnavailable as exc:
        return LlmProviderListResult(engine=str(exc))
    return LlmProviderListResult(
        providers=[_row(x) for x in payload.get("providers", [])]
    )


def llm_provider_set(
    ctx: AppContext, params: LlmProviderSetParams
) -> LlmProviderSetResult:
    if ctx.engine is None:
        raise InvalidRequest(_NO_ENGINE)
    body = params.model_dump(exclude={"reason"})
    result = ctx.engine.set_llm_provider(body)
    provider = _row(result["provider"])

    def write(txn: WriteTxn, actor: Actor) -> WriteOutcome[LlmProviderSetResult]:
        del txn, actor
        return WriteOutcome(
            LlmProviderSetResult(provider=provider),
            "llm_provider",
            provider.name,
            {k: v for k, v in body.items() if k != "key"},
            "llm_provider.set",
            {"name": provider.name},
        )

    return audited_write(
        ctx, operation="llm_provider.set", reason=params.reason, body=write
    )


def llm_provider_remove(
    ctx: AppContext, params: LlmProviderRemoveParams
) -> LlmProviderRemoveResult:
    if ctx.engine is None:
        raise InvalidRequest(_NO_ENGINE)
    result = ctx.engine.remove_llm_provider(params.name)

    def write(txn: WriteTxn, actor: Actor) -> WriteOutcome[LlmProviderRemoveResult]:
        del txn, actor
        return WriteOutcome(
            LlmProviderRemoveResult(
                name=params.name, removed=bool(result.get("removed", True))
            ),
            "llm_provider",
            params.name,
            {"name": params.name},
            "llm_provider.remove",
            {"name": params.name},
        )

    return audited_write(
        ctx, operation="llm_provider.remove", reason=params.reason, body=write
    )
