"""The oversight attention order (WI-915): pure, with the clock passed in."""

from __future__ import annotations

from datetime import UTC, datetime, timedelta

import pytest

from vogt.core.oversight import ORDER, classify

NOW = datetime(2026, 10, 5, 12, 0, tzinfo=UTC)


def verdict(**over: object) -> tuple[str, str]:
    facts: dict[str, object] = {
        "activity": "idle",
        "alive": True,
        "ready": False,
        "approval_question": None,
        "blocker": None,
        "last_output_at": NOW,
        "now": NOW,
        "stall_after": timedelta(minutes=10),
    }
    facts.update(over)
    found = classify(**facts)  # type: ignore[arg-type]
    return found.attention, found.reason


@pytest.mark.parametrize(
    ("facts", "attention"),
    [
        ({"activity": "awaiting-approval", "approval_question": "Run rm?"}, "approval"),
        ({"activity": "running", "blocker": "needs a token"}, "blocked"),
        ({"activity": "waiting-for-input"}, "waiting"),
        ({"activity": "idle", "ready": True}, "waiting"),
        ({"activity": "idle"}, "idle"),
        ({"activity": "running"}, "running"),
        (
            {"activity": "running", "last_output_at": NOW - timedelta(minutes=25)},
            "stalled",
        ),
        ({"activity": "hibernated", "alive": False}, "hibernated"),
        ({"activity": "errored", "alive": False}, "exited"),
        ({"activity": None, "alive": None}, "unknown"),
    ],
)
def test_each_session_lands_where_a_driver_should_look(
    facts: dict[str, object], attention: str
) -> None:
    assert verdict(**facts)[0] == attention


def test_a_dialog_outranks_a_blocked_report_and_says_what_it_asks() -> None:
    attention, reason = verdict(
        activity="awaiting-approval", approval_question="Run rm?", blocker="x"
    )
    assert attention == "approval"
    assert "Run rm?" in reason
    assert verdict(activity="running", last_output_at=NOW - timedelta(minutes=25))[
        1
    ] == ("running, but nothing printed for 25 min")


def test_the_order_puts_what_needs_a_person_first() -> None:
    assert sorted(ORDER, key=ORDER.__getitem__)[:4] == [
        "approval",
        "blocked",
        "waiting",
        "stalled",
    ]


def test_a_startup_gate_is_named_as_one() -> None:
    attention, reason = verdict(
        activity="awaiting-approval",
        approval_question="Is this a project you created or one you trust?",
        approval_kind="folder-trust",
    )
    assert attention == "approval"
    assert reason.startswith("stopped at a startup gate (folder trust)")
