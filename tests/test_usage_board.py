"""The usage board (WI-2040): reported limits per account, key spend, alerts."""

from __future__ import annotations

import json
import os
from dataclasses import replace
from datetime import UTC, datetime, timedelta
from pathlib import Path

import pytest

from vogt.application.context import AppContext
from vogt.application.models import UsageBoardParams
from vogt.application.services import usage as usage_service
from vogt.application.services.usage import usage_board_read
from vogt.config import UsageAccount, UsageKey
from vogt.core import usage_board
from vogt.core.usage_board import RoutingRule

NOW = datetime(2026, 10, 20, 12, 0, tzinfo=UTC)


def _codex_line(at: datetime, limits: object) -> str:
    return json.dumps(
        {
            "timestamp": at.isoformat(),
            "type": "event_msg",
            "payload": {"type": "token_count", "info": None, "rate_limits": limits},
        }
    )


CODEX_LIMITS = {
    "limit_id": "codex",
    "primary": {"used_percent": 42.0, "window_minutes": 300, "resets_at": 1760968800},
    "secondary": {
        "used_percent": 100.0,
        "window_minutes": 10080,
        "resets_at": 1761300000,
    },
    "plan_type": "pro",
    "rate_limit_reached_type": "secondary",
}


def test_codex_reading_takes_the_newest_rate_limits() -> None:
    older = {"primary": {"used_percent": 1.0, "window_minutes": 300}}
    text = "\n".join(
        [
            _codex_line(NOW - timedelta(minutes=5), older),
            _codex_line(NOW, CODEX_LIMITS),
            "not json",
        ]
    )
    reading = usage_board.codex_reading(text)
    assert [w.name for w in reading.windows] == ["primary", "secondary"]
    assert reading.windows[0].used_percent == 42.0
    assert reading.windows[0].resets_at == datetime.fromtimestamp(1760968800, UTC)
    assert reading.plan == "pro" and reading.limit_reached == "secondary"
    assert reading.limited_at == NOW
    assert usage_board.is_limited(reading, now=NOW)


def test_codex_null_windows_report_nothing_limited() -> None:
    limits = {"primary": None, "secondary": None, "rate_limit_reached_type": None}
    reading = usage_board.codex_reading(_codex_line(NOW, limits))
    assert reading.windows == () and reading.limited_at is None
    assert not usage_board.is_limited(reading, now=NOW)


def _claude_entry(at: datetime, **fields: object) -> str:
    return json.dumps({"timestamp": at.isoformat(), **fields})


def test_claude_reading_reads_a_limit_hit_with_its_epoch_reset() -> None:
    reset = NOW + timedelta(hours=2)
    said = f"Claude AI usage limit reached|{int(reset.timestamp())}"
    text = "\n".join(
        [
            _claude_entry(
                NOW - timedelta(hours=1),
                message={"usage": {"input_tokens": 100, "output_tokens": 50}},
            ),
            _claude_entry(
                NOW - timedelta(hours=9),
                message={"usage": {"input_tokens": 999}},
            ),
            _claude_entry(
                NOW,
                isApiErrorMessage=True,
                message={"content": [{"type": "text", "text": said}]},
            ),
        ]
    )
    reading = usage_board.claude_reading([text], now=NOW)
    assert reading.tokens_5h == 150
    assert reading.windows[0].resets_at == reset
    assert reading.limited_at == NOW
    assert usage_board.is_limited(reading, now=NOW)


def test_claude_reading_keeps_the_words_of_a_worded_reset() -> None:
    said = "You've hit your limit · resets 3pm (Europe/London)"
    text = _claude_entry(
        NOW, isApiErrorMessage=True, message={"content": [{"text": said}]}
    )
    reading = usage_board.claude_reading([text], now=NOW)
    assert reading.windows[0].resets_text == "3pm (Europe/London)"


def test_a_claude_limit_whose_reset_passed_no_longer_binds() -> None:
    past = int((NOW - timedelta(minutes=1)).timestamp())
    text = _claude_entry(
        NOW - timedelta(hours=3),
        isApiErrorMessage=True,
        message={"content": [{"text": f"Claude AI usage limit reached|{past}"}]},
    )
    reading = usage_board.claude_reading([text], now=NOW)
    assert reading.limited_at is not None
    assert not usage_board.is_limited(reading, now=NOW)


def test_key_spend_alerts_on_a_daily_key_over_threshold() -> None:
    spend = usage_board.key_spend(
        {
            "key": "sk-hash",
            "info": {
                "spend": 31.5,
                "max_budget": 50,
                "budget_duration": "1d",
                "budget_reset_at": "2026-10-21T00:00:00Z",
                "key_alias": "dlab",
            },
        },
        alert_usd=30,
    )
    assert spend.daily and spend.alert and spend.detail is None
    assert spend.alias == "dlab" and spend.max_budget_usd == 50
    assert spend.budget_reset_at == datetime(2026, 10, 21, tzinfo=UTC)


def test_key_spend_without_a_day_window_says_it_is_a_total() -> None:
    spend = usage_board.key_spend({"info": {"spend": 2.0}}, alert_usd=None)
    assert not spend.daily and not spend.alert
    assert spend.detail is not None and "total" in spend.detail


def test_fallback_respects_the_routing_rule() -> None:
    rules = [
        RoutingRule("primary"),
        RoutingRule("acct2", sessions="msp-agent*"),
        RoutingRule("spare"),
    ]
    limited = {"spare": True}
    assert usage_board.fallback_accounts("msp-agent-7", "primary", rules, limited) == [
        "acct2"
    ]
    assert usage_board.fallback_accounts("vogt-wi-1", "primary", rules, limited) == []
    assert usage_board.fallback_accounts("msp-agent-7", "acct2", rules, {}) == [
        "primary",
        "spare",
    ]


# -- the operation ---------------------------------------------------------


def _with(ctx: AppContext, **update: object) -> AppContext:
    return replace(ctx, config=ctx.config.model_copy(update=update), clock=lambda: NOW)


def _touch(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)
    stamp = datetime.now(UTC).timestamp()
    os.utime(path, (stamp, stamp))


def test_usage_board_is_not_configured_by_default(instance: AppContext) -> None:
    result = usage_board_read(_with(instance), UsageBoardParams())
    assert result.accounts == [] and result.keys == [] and result.alerts == []
    assert result.detail is not None and "not configured" in result.detail


def test_usage_board_reads_accounts_and_alerts_on_key_spend(
    instance: AppContext, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    codex = tmp_path / "codex"
    _touch(
        codex / "sessions/2026/10/20/rollout-x.jsonl", _codex_line(NOW, CODEX_LIMITS)
    )
    claude = tmp_path / "claude-acct2"
    (claude / "projects").mkdir(parents=True)
    seen: list[tuple[str, str]] = []

    def fetch(url: str, key: str) -> tuple[int, bytes]:
        seen.append((url, key))
        body = {"info": {"spend": 30.0, "budget_duration": "1d"}}
        return 200, json.dumps(body).encode()

    monkeypatch.setattr(usage_service, "key_info_fetch", fetch)
    monkeypatch.setenv("TEST_DLAB_KEY", "sk-secret")
    ctx = _with(
        instance,
        usage_accounts=(
            UsageAccount(name="codex", agent="codex", dir=codex),
            UsageAccount(name="acct2", agent="claude", dir=claude),
        ),
        usage_litellm_url="http://litellm.test",
        usage_litellm_keys=(
            UsageKey(name="dlab", key_env="TEST_DLAB_KEY"),
            UsageKey(name="missing", key_env="TEST_UNSET_KEY_2040"),
        ),
        usage_daily_spend_alert_usd=25.0,
    )
    result = usage_board_read(ctx, UsageBoardParams())

    codex_view, claude_view = result.accounts
    assert codex_view.limited and codex_view.plan == "pro"
    assert [w.name for w in codex_view.windows] == ["primary", "secondary"]
    assert not claude_view.limited and claude_view.detail is not None

    dlab, missing = result.keys
    assert dlab.spend_usd == 30.0 and dlab.daily and dlab.alert
    assert missing.detail == "TEST_UNSET_KEY_2040 is not set"
    assert seen == [("http://litellm.test", "sk-secret")]
    assert [a.key for a in result.alerts] == ["dlab"]
    assert "today" in result.alerts[0].message
    # The key itself never appears in the result.
    assert "sk-secret" not in result.model_dump_json()


def test_a_key_threshold_overrides_the_default(
    instance: AppContext, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(
        usage_service,
        "key_info_fetch",
        lambda _u, _k: (200, b'{"info": {"spend": 30.0, "budget_duration": "24h"}}'),
    )
    monkeypatch.setenv("TEST_DS_KEY", "sk-x")
    ctx = _with(
        instance,
        usage_litellm_url="http://litellm.test",
        usage_litellm_keys=(
            UsageKey(name="deepseek", key_env="TEST_DS_KEY", daily_alert_usd=100),
        ),
        usage_daily_spend_alert_usd=5.0,
    )
    result = usage_board_read(ctx, UsageBoardParams())
    assert result.keys[0].alert_usd == 100 and result.alerts == []


def test_an_unreachable_litellm_says_why(
    instance: AppContext, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setattr(usage_service, "key_info_fetch", lambda _u, _k: (401, b"{}"))
    monkeypatch.setenv("TEST_DS_KEY", "sk-x")
    ctx = _with(
        instance,
        usage_litellm_url="http://litellm.test",
        usage_litellm_keys=(UsageKey(name="deepseek", key_env="TEST_DS_KEY"),),
    )
    key = usage_board_read(ctx, UsageBoardParams()).keys[0]
    assert key.spend_usd is None and key.detail == "LiteLLM /key/info answered 401"
