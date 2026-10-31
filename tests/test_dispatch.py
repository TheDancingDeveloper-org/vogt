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


def test_the_turn_budget_is_sized_by_the_model_it_runs() -> None:
    implement = dispatch.resolve({}).profiles["implement"]
    assert implement.turns_for("haiku") == 200
    assert implement.turns_for("claude-haiku-4-5-20251001") == 200
    assert implement.turns_for("sonnet-haikuish") == 120
    assert implement.turns_for("grok-4.7") == 120
    assert implement.turns_for(None) == 120


def test_a_model_turn_override_merges_and_is_validated() -> None:
    table = dispatch.resolve(
        {dispatch.PROFILES_KEY: {"review": {"budget_turns_by_model": {"haiku": 30}}}}
    )
    assert table.profiles["review"].turns_for("haiku") == 30
    assert table.profiles["review"].turns_for("opus") == 40
    with pytest.raises(dispatch.DispatchError):
        dispatch.validate(
            dispatch.PROFILES_KEY, {"review": {"budget_turns_by_model": {"haiku": 0}}}
        )


def test_the_budget_restart_cap_defaults_to_one_and_can_be_off() -> None:
    assert dispatch.resolve({}).budget_restarts_per_item == 1
    off = dispatch.resolve({dispatch.BUDGET_RESTARTS_KEY: {"value": 0}})
    assert off.budget_restarts_per_item == 0
    assert dispatch.validate(dispatch.BUDGET_RESTARTS_KEY, {"value": 0}) == {"value": 0}


# -- check (WI-1329) ------------------------------------------------------

_SETTINGS = dispatch.resolve({})


def _check(**overrides: object) -> dispatch.Decision:
    args: dict[str, object] = {
        "purpose": None,
        "review": False,
        "model": None,
        "caller_role": "oversight",
        "live_workers": 0,
        "blockers_count": 0,
        "settings": _SETTINGS,
    } | overrides
    return dispatch.check(**args)  # type: ignore[arg-type]


def _rules(decision: dispatch.Decision) -> list[str]:
    return [v.rule for v in decision.violations]


def test_enforcement_is_off_by_default() -> None:
    assert _SETTINGS.enforce is False
    assert dispatch.resolve({dispatch.ENFORCE_KEY: {"value": True}}).enforce


def test_a_worker_starting_a_review_breaks_review_by_oversight_only() -> None:
    assert _rules(_check(review=True, caller_role="worker")) == [
        "review_by_oversight_only"
    ]
    assert _rules(_check(review=True, caller_role="oversight")) == []
    assert _rules(_check(review=True, caller_role="person")) == []
    off = dispatch.resolve({dispatch.REVIEW_BY_OVERSIGHT_KEY: {"value": False}})
    assert _rules(_check(review=True, caller_role="worker", settings=off)) == []


@pytest.mark.parametrize(
    "purpose", ["implement", "implement-careful", "mechanical", "investigate"]
)
def test_a_model_other_than_the_profiles_is_named(purpose: str) -> None:
    decision = _check(purpose=purpose, model="something-else")
    [violation] = decision.violations
    assert violation.rule == "profile_model"
    assert "something-else" in violation.message
    profile_model = _SETTINGS.profiles[purpose].model  # type: ignore[index]
    assert _rules(_check(purpose=purpose, model=profile_model)) == []


def test_the_model_rule_follows_a_profile_override() -> None:
    settings = dispatch.resolve(
        {dispatch.PROFILES_KEY: {"implement": {"model": "grok-4.8"}}}
    )
    assert (
        _rules(_check(purpose="implement", model="grok-4.8", settings=settings)) == []
    )
    assert _rules(_check(purpose="implement", model="grok-4.7", settings=settings)) == [
        "profile_model"
    ]


def test_the_worker_cap_refuses_the_sixth() -> None:
    assert _rules(_check(live_workers=4)) == []
    [violation] = _check(live_workers=5).violations
    assert violation.message.startswith("max_workers_per_overseer=5 reached")
    assert _rules(_check(live_workers=9, caller_role="person")) == []


def test_failed_reviews_escalate_implement() -> None:
    assert _check(purpose="implement", blockers_count=1).escalated_to is None
    assert (
        _check(purpose="implement", blockers_count=2).escalated_to
        == "implement-careful"
    )
    assert _check(purpose="mechanical", blockers_count=5).escalated_to is None


def test_no_purpose_is_judged_as_implement() -> None:
    [violation] = _check(purpose=None, model="something-else").violations
    assert violation.rule == "profile_model"
    assert "purpose=implement" in violation.message
    implement_model = _SETTINGS.profiles["implement"].model
    assert _rules(_check(purpose=None, model=implement_model)) == []


def test_a_review_is_not_held_to_the_profile_model() -> None:
    assert _rules(_check(purpose="review", review=True, model="other")) == []


def test_failed_reviews_escalate_a_start_with_no_purpose() -> None:
    assert _check(purpose=None, blockers_count=2).escalated_to == "implement-careful"
    assert dispatch.escalation(None, 2, _SETTINGS) == "implement-careful"
    assert dispatch.escalation(None, 1, _SETTINGS) is None
