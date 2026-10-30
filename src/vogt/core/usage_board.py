"""Usage board: what each provider account reports about its own limits (WI-2040).

The operator runs several provider accounts at once — Claude subscriptions
(primary and acct2), Codex, and LiteLLM virtual keys in front of DeepSeek and
others — and learnt of a limit or a spend spike only when a session stopped or
a bill arrived. This module is the reading of what each provider already
reports. It takes text and numbers and returns values. It opens no file, asks
no clock, and stores nothing.

What each provider reports, and so what is read:

- **Codex** writes its account's rate limits into every `token_count` event of
  a rollout transcript: a `primary` (5-hour) and `secondary` (weekly) window,
  each with `used_percent`, `window_minutes` and `resets_at` (epoch seconds),
  plus `rate_limit_reached_type` once a limit is hit. The newest event wins.
- **Claude Code** records no limit state until a limit is hit. Then it writes
  an assistant entry with `isApiErrorMessage` whose text names the limit and,
  in one of two shapes, when it resets: `...usage limit reached|<epoch>` or
  `...limit reached · resets 3pm (Europe/London)`. Tokens per window are
  summed from `message.usage` as the account's reported usage.
- **LiteLLM** answers `/key/info` with the key's `spend`, `max_budget`,
  `budget_duration` and `budget_reset_at`. Its `spend` is the spend in the
  current budget window, so a key with a one-day window reports today's
  spend; a key with no window reports its total, and is labelled so.

Account fallback (moving a limited session to another allowed account) is a
follow-up; `fallback_accounts` is its seam: which accounts a session may move
to, by the routing rule, without saying whether it should.
"""

from __future__ import annotations

import fnmatch
import json
import re
from collections.abc import Iterable, Mapping, Sequence
from dataclasses import dataclass
from datetime import UTC, datetime, timedelta

#: Claude Code's legacy limit text carries the reset time as epoch seconds
#: after a pipe: `Claude AI usage limit reached|1760000000`.
_CLAUDE_EPOCH = re.compile(r"limit reached\|(\d{9,11})")
#: The current text names the reset in words: `... · resets 3pm (Europe/London)`.
_CLAUDE_RESETS = re.compile(r"(?i)\bresets\s+(?:at\s+)?([^\n\"]{1,60})")
_CLAUDE_LIMIT = re.compile(r"(?i)(usage limit reached|limit reached|hit your limit)")

#: `budget_duration` values LiteLLM accepts that mean "resets daily".
_DAILY = frozenset({"1d", "24h", "1440m", "86400s"})


@dataclass(frozen=True)
class LimitWindow:
    """One rate-limit window a provider reported for an account."""

    name: str
    used_percent: float | None = None
    window_minutes: int | None = None
    resets_at: datetime | None = None
    #: The provider's own words for the reset when it gave no timestamp.
    resets_text: str | None = None


@dataclass(frozen=True)
class AccountReading:
    """What one account's transcripts say about its limits."""

    windows: tuple[LimitWindow, ...] = ()
    #: When the provider last said this account hit a limit, and which.
    limited_at: datetime | None = None
    limit_reached: str | None = None
    plan: str | None = None
    tokens_5h: int = 0
    observed_at: datetime | None = None


@dataclass(frozen=True)
class KeySpend:
    """One LiteLLM key's reported spend, and whether it crossed its alert."""

    spend_usd: float | None
    max_budget_usd: float | None
    budget_duration: str | None
    budget_reset_at: datetime | None
    #: True when `spend_usd` is today's: the key's budget window is one day.
    daily: bool
    alert_usd: float | None
    alert: bool
    alias: str | None = None
    detail: str | None = None


@dataclass(frozen=True)
class RoutingRule:
    """An account a session may run under: `sessions` is an fnmatch pattern
    on the session name (`msp-agent*`), `*` for any session."""

    account: str
    sessions: str = "*"


def _parse_time(value: object) -> datetime | None:
    if isinstance(value, bool):
        return None
    if isinstance(value, int | float):
        try:
            return datetime.fromtimestamp(float(value), tz=UTC)
        except (OverflowError, OSError, ValueError):
            return None
    if isinstance(value, str) and value:
        try:
            parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
        except ValueError:
            return None
        return parsed if parsed.tzinfo else parsed.replace(tzinfo=UTC)
    return None


def _number(value: object) -> float | None:
    if isinstance(value, bool) or not isinstance(value, int | float):
        return None
    return float(value)


def _lines(text: str) -> Iterable[dict[str, object]]:
    for line in text.splitlines():
        line = line.strip()
        if not line.startswith("{"):
            continue
        try:
            entry = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(entry, dict):
            yield entry


def codex_reading(text: str) -> AccountReading:
    """The newest rate limits a Codex rollout transcript reported."""
    latest: dict[str, object] | None = None
    latest_at: datetime | None = None
    for entry in _lines(text):
        payload = entry.get("payload")
        if not isinstance(payload, dict) or payload.get("type") != "token_count":
            continue
        limits = payload.get("rate_limits")
        if isinstance(limits, dict):
            latest = limits
            latest_at = _parse_time(entry.get("timestamp"))
    if latest is None:
        return AccountReading()
    windows: list[LimitWindow] = []
    for name in ("primary", "secondary"):
        raw = latest.get(name)
        if not isinstance(raw, dict):
            continue
        minutes = _number(raw.get("window_minutes"))
        resets_at = _parse_time(raw.get("resets_at"))
        seconds = _number(raw.get("resets_in_seconds"))
        if resets_at is None and seconds is not None and latest_at is not None:
            resets_at = latest_at + timedelta(seconds=seconds)
        windows.append(
            LimitWindow(
                name=name,
                used_percent=_number(raw.get("used_percent")),
                window_minutes=None if minutes is None else int(minutes),
                resets_at=resets_at,
            )
        )
    reached = latest.get("rate_limit_reached_type")
    plan = latest.get("plan_type")
    return AccountReading(
        windows=tuple(windows),
        limited_at=latest_at if isinstance(reached, str) and reached else None,
        limit_reached=reached if isinstance(reached, str) and reached else None,
        plan=plan if isinstance(plan, str) and plan else None,
        observed_at=latest_at,
    )


def _texts(entry: Mapping[str, object]) -> Iterable[str]:
    message = entry.get("message")
    content = message.get("content") if isinstance(message, dict) else None
    if isinstance(content, str):
        yield content
    elif isinstance(content, list):
        for block in content:
            if isinstance(block, dict) and isinstance(block.get("text"), str):
                yield block["text"]


def claude_reading(texts: Iterable[str], *, now: datetime) -> AccountReading:
    """What one Claude account's transcripts say: the newest limit-hit
    message with its reset, and the tokens the account used in the last five
    hours (the subscription's short window), each message once, cache reads
    excluded. A turn answered after the hit means the limit has lifted."""
    since = now - timedelta(hours=5)
    #: Tokens per message id, last entry wins: Claude repeats one message's
    #: usage across streamed and resumed entries, and a sum would count it
    #: once per entry. Cache reads are left out, as the field says.
    by_message: dict[str, int] = {}
    unnamed = 0
    #: The newest turn that carried usage: an answered turn after a limit hit
    #: means the limit has lifted, whatever the hit said about its reset.
    answered_at: datetime | None = None
    hit_at: datetime | None = None
    hit_text: str | None = None
    newest: datetime | None = None
    for text in texts:
        for entry in _lines(text):
            at = _parse_time(entry.get("timestamp"))
            if at is not None and (newest is None or at > newest):
                newest = at
            message = entry.get("message")
            usage = message.get("usage") if isinstance(message, dict) else None
            if isinstance(usage, dict) and at is not None:
                if answered_at is None or at > answered_at:
                    answered_at = at
                if at >= since:
                    count = sum(
                        value
                        for key in (
                            "input_tokens",
                            "output_tokens",
                            "cache_creation_input_tokens",
                        )
                        if isinstance(value := usage.get(key), int)
                        and not isinstance(value, bool)
                    )
                    message_id = (
                        message.get("id") if isinstance(message, dict) else None
                    )
                    if isinstance(message_id, str) and message_id:
                        by_message[message_id] = count
                    else:
                        unnamed += count
            if not entry.get("isApiErrorMessage") and entry.get("error") != (
                "rate_limit"
            ):
                continue
            for said in _texts(entry):
                if _CLAUDE_LIMIT.search(said) and (
                    hit_at is None or (at is not None and at >= hit_at)
                ):
                    hit_at, hit_text = at, said
    tokens = sum(by_message.values()) + unnamed
    lifted = hit_at is not None and answered_at is not None and answered_at > hit_at
    windows: tuple[LimitWindow, ...] = ()
    if hit_text is not None:
        epoch = _CLAUDE_EPOCH.search(hit_text)
        words = _CLAUDE_RESETS.search(hit_text)
        resets_at = _parse_time(int(epoch.group(1))) if epoch else None
        window = LimitWindow(
            name="limit",
            resets_at=resets_at,
            resets_text=None if resets_at or not words else words.group(1).strip(),
        )
        # A reset already past means the limit has lifted: report the hit,
        # not a window that still binds.
        if not lifted and (resets_at is None or resets_at > now):
            windows = (window,)
    return AccountReading(
        windows=windows,
        limited_at=hit_at,
        limit_reached=None if hit_text is None else hit_text[:160],
        tokens_5h=tokens,
        observed_at=newest,
    )


def key_spend(info: Mapping[str, object], *, alert_usd: float | None) -> KeySpend:
    """A LiteLLM `/key/info` answer, read, with its alert decided.

    The alert fires only for a key with a one-day budget window, whose spend
    is today's, at or above `alert_usd`. A key with no day window raises no
    alert, and its `detail` says its spend is not a day's.
    """
    inner = info.get("info")
    body: Mapping[str, object] = inner if isinstance(inner, dict) else info
    spend = _number(body.get("spend"))
    duration = body.get("budget_duration")
    duration = duration if isinstance(duration, str) and duration else None
    daily = duration in _DAILY
    # A day threshold compared with a total or a month's spend would always
    # fire, so only a key whose spend is a day's can alert.
    over = alert_usd is not None and spend is not None and spend >= alert_usd
    alert = daily and over
    detail = None
    if not daily:
        detail = (
            "spend is the key's total: it has no one-day budget window, so a "
            "day's spend is not reported"
            if duration is None
            else f"spend is for the key's {duration} budget window, not one day"
        ) + ("; no day alert is raised on it" if alert_usd is not None else "")
    alias = body.get("key_alias")
    return KeySpend(
        spend_usd=spend,
        max_budget_usd=_number(body.get("max_budget")),
        budget_duration=duration,
        budget_reset_at=_parse_time(body.get("budget_reset_at")),
        daily=daily,
        alert_usd=alert_usd,
        alert=alert,
        alias=alias if isinstance(alias, str) and alias else None,
        detail=detail,
    )


def is_limited(reading: AccountReading, *, now: datetime) -> bool:
    """Whether an account is limited now: a window at 100%, or a reported hit,
    whose reset is still ahead (or unknown)."""
    for window in reading.windows:
        if window.resets_at is not None and window.resets_at <= now:
            # The window has reset since it was read: what it said is stale.
            continue
        if window.used_percent is not None and window.used_percent >= 100:
            return True
        if window.name == "limit" and (
            window.resets_at is None or window.resets_at > now
        ):
            return True
    return False


def fallback_accounts(
    session_name: str,
    current: str,
    rules: Sequence[RoutingRule],
    limited: Mapping[str, bool],
) -> list[str]:
    """Accounts a session may move to when `current` is limited: every
    account a routing rule allows for this session, other than the current
    one, that is not itself limited. The seam for account fallback; nothing
    moves a session yet."""
    allowed: list[str] = []
    for rule in rules:
        if rule.account == current or rule.account in allowed:
            continue
        if not fnmatch.fnmatchcase(session_name, rule.sessions):
            continue
        if limited.get(rule.account, False):
            continue
        allowed.append(rule.account)
    return allowed
