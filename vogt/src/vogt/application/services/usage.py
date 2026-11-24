"""`usage.board`: every provider account's reported limits and every LiteLLM
key's spend, in one view (WI-2040).

Read on request: each account's newest transcripts (`usage_accounts`) and
each key's `/key/info` (`usage_litellm_keys`). Nothing is stored. A source
that cannot be read says why in its row's `detail`, so "unreadable" never
reads as "unlimited".
"""

from __future__ import annotations

import os
from datetime import datetime

from vogt.adapters import usage as sources
from vogt.application.context import AppContext
from vogt.application.models import (
    UsageAccountView,
    UsageAlert,
    UsageBoardParams,
    UsageBoardResult,
    UsageKeyView,
    UsageWindowView,
)
from vogt.config import UsageAccount, UsageKey
from vogt.core import usage_board

#: Swapped by tests so no request leaves the process.
key_info_fetch: sources.KeyInfoFetch = sources.fetch_key_info


def usage_board_read(ctx: AppContext, params: UsageBoardParams) -> UsageBoardResult:
    """What each configured account and key reports now."""
    del params
    now = ctx.clock()
    config = ctx.config
    accounts = [_account(account, now) for account in config.usage_accounts]
    keys = [_key(ctx, key) for key in config.usage_litellm_keys]
    alerts = [
        UsageAlert(
            key=key.name,
            spend_usd=key.spend_usd,
            threshold_usd=key.alert_usd,
            message=(
                f"{key.name} spent ${key.spend_usd:.2f} today, at or over "
                f"its ${key.alert_usd:.2f} day alert"
            ),
        )
        for key in keys
        if key.alert and key.spend_usd is not None and key.alert_usd is not None
    ]
    detail = None
    if not accounts and not keys:
        detail = (
            "usage_accounts and usage_litellm_keys are not configured, so no "
            "usage is read"
        )
    return UsageBoardResult(
        accounts=accounts,
        keys=keys,
        alerts=alerts,
        generated_at=now,
        detail=detail,
    )


def _account(account: UsageAccount, now: datetime) -> UsageAccountView:
    tails = sources.transcript_tails(account.agent, account.dir)
    if account.agent == "codex":
        # Newest first: the first rollout that reported limits is the newest.
        reading = usage_board.AccountReading()
        for tail in tails:
            reading = usage_board.codex_reading(tail)
            if reading.observed_at is not None:
                break
    else:
        reading = usage_board.claude_reading(tails, now=now)
    detail = None
    if not tails:
        detail = f"no transcript under {account.dir} in the last week"
    elif not reading.windows and reading.limited_at is None:
        detail = (
            "no limit reported"
            if account.agent == "claude"
            else "no rollout reported rate limits"
        )
    return UsageAccountView(
        name=account.name,
        agent=account.agent,
        limited=usage_board.is_limited(reading, now=now),
        windows=[
            UsageWindowView(
                name=w.name,
                used_percent=w.used_percent,
                window_minutes=w.window_minutes,
                resets_at=w.resets_at,
                resets_text=w.resets_text,
            )
            for w in reading.windows
        ],
        limited_at=reading.limited_at,
        limit_reached=reading.limit_reached,
        plan=reading.plan,
        tokens_5h=reading.tokens_5h,
        observed_at=reading.observed_at,
        detail=detail,
    )


def _key(ctx: AppContext, key: UsageKey) -> UsageKeyView:
    threshold = (
        key.daily_alert_usd
        if key.daily_alert_usd is not None
        else ctx.config.usage_daily_spend_alert_usd
    )
    url = ctx.config.usage_litellm_url
    if url is None:
        return UsageKeyView(
            name=key.name, alert_usd=threshold, detail="usage_litellm_url is not set"
        )
    secret = os.environ.get(key.key_env, "")
    if not secret:
        return UsageKeyView(
            name=key.name, alert_usd=threshold, detail=f"{key.key_env} is not set"
        )
    info, why = sources.key_info(url, secret, key_info_fetch)
    if info is None:
        return UsageKeyView(name=key.name, alert_usd=threshold, detail=why)
    spend = usage_board.key_spend(info, alert_usd=threshold)
    return UsageKeyView(
        name=key.name,
        alias=spend.alias,
        spend_usd=spend.spend_usd,
        daily=spend.daily,
        max_budget_usd=spend.max_budget_usd,
        budget_duration=spend.budget_duration,
        budget_reset_at=spend.budget_reset_at,
        alert_usd=spend.alert_usd,
        alert=spend.alert,
        detail=spend.detail,
    )


__all__ = ["usage_board_read"]
