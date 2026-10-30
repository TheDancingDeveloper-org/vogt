"""The cold-wake compaction decision and summary (WI-2037), pure."""

from __future__ import annotations

from datetime import UTC, datetime, timedelta

import pytest

from vogt.core import cold_wake
from vogt.core.entities import Comment, HandoffFields

NOW = datetime(2026, 10, 20, 12, 0, tzinfo=UTC)
TTL = {"anthropic": 300, "deepseek": 300}


def _decide(**overrides: object) -> cold_wake.Decision:
    args: dict[str, object] = {
        "enabled": True,
        "opted_out": False,
        "agent": "claude",
        "model": None,
        "hibernated_at": NOW - timedelta(minutes=30),
        "now": NOW,
        "context_tokens": 150_000,
        "ttl_seconds": TTL,
        "threshold_tokens": 100_000,
    }
    args.update(overrides)
    return cold_wake.decide(**args)  # type: ignore[arg-type]


@pytest.mark.parametrize(
    ("agent", "model", "provider"),
    [
        ("claude", None, "anthropic"),
        ("claude", "claude-opus-5-5", "anthropic"),
        ("klaudia", "deepseek-chat", "deepseek"),
        ("codex", None, "openai"),
        ("codex", "gpt-5.6", "openai"),
        ("opencode", None, None),
    ],
)
def test_provider_of(agent: str, model: str | None, provider: str | None) -> None:
    assert cold_wake.provider_of(agent, model) == provider


@pytest.mark.parametrize(
    ("overrides", "outcome"),
    [
        ({}, "compact"),
        ({"enabled": False}, "disabled"),
        ({"opted_out": True}, "opted-out"),
        ({"agent": "opencode"}, "agent"),
        ({"agent": "codex"}, "provider"),
        ({"hibernated_at": NOW - timedelta(minutes=4)}, "warm"),
        ({"hibernated_at": None}, "warm"),
        ({"context_tokens": None}, "unmeasured"),
        ({"context_tokens": 99_999}, "small"),
        ({"context_tokens": 100_000}, "compact"),
        ({"agent": "klaudia", "model": "deepseek-chat"}, "compact"),
    ],
)
def test_decide(overrides: dict[str, object], outcome: str) -> None:
    assert _decide(**overrides).outcome == outcome


def test_the_decision_audits_its_facts() -> None:
    assert _decide().audit() == {
        "outcome": "compact",
        "provider": "anthropic",
        "ttl_seconds": 300,
        "idle_seconds": 1800,
        "context_tokens": 150_000,
        "threshold_tokens": 100_000,
    }


def test_the_summary_has_every_section_and_says_when_one_is_empty() -> None:
    text = cold_wake.render_summary(
        session_id="ses_1",
        work_item=None,
        work_item_title=None,
        reason=None,
        last_reply=None,
        handoff=None,
        decision=_decide(),
    )
    for heading in (
        "## Task",
        "## State",
        "## Decisions and hand-off",
        "## Files and branch",
        "## Next step",
    ):
        assert heading in text
    assert "(no reply recorded)" in text
    assert "(no hand-off recorded on the work item)" in text


def test_the_summary_carries_the_hand_off_and_caps_the_reply() -> None:
    handoff = Comment(
        id="c1",
        work_item_id="wi1",
        actor_id="a1",
        actor_display_name="worker",
        body="Half done.",
        created_at=NOW,
        kind="handoff",
        handoff=HandoffFields(
            branch="wi-1-x",
            head_sha="abc123",
            done=["engine"],
            remaining=["core"],
            touch_points=["src/x.py"],
        ),
    )
    text = cold_wake.render_summary(
        session_id="ses_1",
        work_item="WI-1",
        work_item_title="Do x",
        reason="implement x",
        last_reply="r" * (cold_wake.REPLY_CAP + 500),
        handoff=handoff,
        decision=_decide(),
    )
    assert "Work item WI-1 — Do x." in text
    assert "[truncated]" in text
    assert "branch: wi-1-x" in text
    assert "touches: src/x.py" in text
    assert "- core" in text
    # A summary is a small fraction of the context it replaces.
    assert len(text) < 16_000
