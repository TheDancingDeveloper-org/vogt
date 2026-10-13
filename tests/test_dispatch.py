"""Dispatch profiles as data (WI-1323): the pure table and its validation."""

from __future__ import annotations

import pytest

from vogt.core import dispatch


def test_the_code_defaults_are_the_design_table() -> None:
    table = dispatch.resolve({})
    implement = table.profiles["implement"]
    assert (implement.template, implement.model) == ("klaudia", "grok-4.7")
    assert (implement.budget_turns, implement.budget_wall_clock_minutes) == (120, 90)
    assert implement.worktree and implement.repo_map and implement.autopilot
    careful = table.profiles["implement-careful"]
    assert (careful.template, careful.model, careful.effort) == (
        "claude",
        "opus",
        "low",
    )
    assert careful.budget_turns == 150
    review = table.profiles["review"]
    assert (review.model, review.effort, review.budget_turns) == ("opus", "low", 40)
    assert not review.autopilot and review.read_only
    assert table.profiles["mechanical"].model == "haiku"
    assert table.profiles["mechanical"].budget_turns == 60
    for name in ("investigate", "design"):
        profile = table.profiles[name]
        assert (profile.template, profile.model, profile.effort) == (
            "claude",
            "opus",
            "low",
        )
        assert profile.budget_turns == 60
    assert set(table.profiles) == set(dispatch.PURPOSES)
    assert table.max_workers_per_overseer == 5
    assert table.escalate_after_failed_reviews == 2
    assert table.review_by_oversight_only is True


def test_an_override_changes_only_the_field_it_names() -> None:
    table = dispatch.resolve({dispatch.PROFILES_KEY: {"implement": {"model": "x"}}})
    implement = table.profiles["implement"]
    assert implement.model == "x"
    assert implement.template == "klaudia"
    assert implement.budget_turns == 120
    assert table.profiles["review"] == dispatch.DEFAULT_PROFILES["review"]


def test_the_scalar_settings_resolve_from_their_value() -> None:
    table = dispatch.resolve(
        {
            dispatch.MAX_WORKERS_KEY: {"value": 3},
            dispatch.ESCALATE_KEY: {"value": 4},
            dispatch.REVIEW_BY_OVERSIGHT_KEY: {"value": False},
        }
    )
    assert table.max_workers_per_overseer == 3
    assert table.escalate_after_failed_reviews == 4
    assert table.review_by_oversight_only is False


def test_a_stale_stored_value_falls_back_to_the_default() -> None:
    table = dispatch.resolve(
        {
            dispatch.PROFILES_KEY: {"implement": {"model": 7}, "gone": {}},
            dispatch.MAX_WORKERS_KEY: {"value": "many"},
        }
    )
    assert table.profiles["implement"].model == "grok-4.7"
    assert table.max_workers_per_overseer == 5


@pytest.mark.parametrize(
    ("key", "value", "said"),
    [
        ("dispatch.profiles", {"deploy": {"model": "x"}}, "unknown purpose"),
        ("dispatch.profiles", {"implement": {"role": "oversight"}}, "role"),
        ("dispatch.profiles", {"review": {"permission_mode": "bypass"}}, "permission"),
        ("dispatch.profiles", {"review": {"budget_turns": 0}}, "budget_turns"),
        ("dispatch.max_workers_per_overseer", {"value": 0}, "value"),
        ("dispatch.max_workers_per_overseer", {"value": "five"}, "value"),
        ("dispatch.review_by_oversight_only", {"value": "maybe"}, "value"),
        ("dispatch.implement", {"model": "x"}, "unknown dispatch setting"),
    ],
)
def test_a_malformed_value_is_refused(
    key: str, value: dict[str, object], said: str
) -> None:
    with pytest.raises(dispatch.DispatchError, match=said):
        dispatch.validate(key, value)


def test_each_key_is_validated_by_its_own_shape() -> None:
    assert dispatch.validate("dispatch.max_workers_per_overseer", {"value": 5}) == {
        "value": 5
    }
    assert dispatch.validate("dispatch.escalate_after_failed_reviews", {"value": 2})
    assert dispatch.validate("dispatch.review_by_oversight_only", {"value": True})
    assert dispatch.validate(
        "dispatch.profiles", {"implement": {"model": "grok-4.8"}}
    ) == {"implement": {"model": "grok-4.8"}}
    assert dispatch.validate("dispatch.profiles", {}) == {}
