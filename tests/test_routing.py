"""Model routes (WI-1260): ordered candidates, breakers, sensitivity."""

from __future__ import annotations

import pytest

from vogt.core import dispatch, routing


def test_every_purpose_has_a_shipped_route() -> None:
    table = dispatch.resolve({})
    assert set(table.routes) == set(dispatch.PURPOSES)
    for purpose in ("review", "design", "investigate", "implement-careful"):
        route = table.routes[purpose]
        assert route.sensitive
        assert all(c.tier == "subscription" for c in route.candidates)


def test_pick_skips_open_breakers_in_order() -> None:
    route = routing.DEFAULT_ROUTES["implement"]
    assert routing.pick(route).candidate == route.candidates[0]
    chosen = routing.pick(route, {"klaudia:grok-4.7"})
    assert chosen.candidate is not None
    assert chosen.candidate.key == "klaudia:code"
    assert chosen.skipped == ("klaudia:grok-4.7",)
    gone = routing.pick(route, {c.key for c in route.candidates})
    assert gone.candidate is None
    assert len(gone.skipped) == len(route.candidates)


def test_sensitive_pick_drops_community_capacity() -> None:
    route = routing.Route(
        candidates=(
            routing.Candidate(template="klaudia", model="kimi-free", tier="community"),
            routing.Candidate(template="claude", model="opus", tier="subscription"),
        )
    )
    assert routing.pick(route).candidate.tier == "community"  # type: ignore[union-attr]
    chosen = routing.pick(route, sensitive=True)
    assert chosen.candidate is not None and chosen.candidate.model == "opus"


def test_a_sensitive_route_refuses_community_candidates() -> None:
    value = {
        "review": {
            "sensitive": True,
            "candidates": [
                {"template": "klaudia", "model": "kimi-free", "tier": "community"}
            ],
        }
    }
    with pytest.raises(dispatch.DispatchError, match="community"):
        dispatch.validate(dispatch.ROUTES_KEY, value)


def test_routes_validate_and_resolve_over_defaults() -> None:
    value = {
        "mechanical": {
            "candidates": [
                {"template": "claude", "model": "haiku", "tier": "subscription"}
            ]
        }
    }
    stored = dispatch.validate(dispatch.ROUTES_KEY, value)
    table = dispatch.resolve({dispatch.ROUTES_KEY: stored})
    assert [c.key for c in table.routes["mechanical"].candidates] == ["claude:haiku"]
    assert table.routes["implement"] == routing.DEFAULT_ROUTES["implement"]
    assert dispatch.validate(dispatch.ROUTES_KEY, {}) == {}


@pytest.mark.parametrize(
    ("value", "match"),
    [
        ({"nope": {"candidates": []}}, "unknown task class"),
        ({"implement": {"candidates": []}}, "invalid route"),
        (
            {
                "implement": {
                    "candidates": [
                        {"template": "claude", "model": "opus", "tier": "paid"}
                    ]
                }
            },
            "invalid route",
        ),
        (
            {
                "implement": {
                    "candidates": [
                        {"template": "claude", "model": "opus", "tier": "subscription"},
                        {"template": "claude", "model": "opus", "tier": "subscription"},
                    ]
                }
            },
            "at most once",
        ),
    ],
)
def test_bad_routes_are_refused(value: dict[str, object], match: str) -> None:
    with pytest.raises(dispatch.DispatchError, match=match):
        dispatch.validate(dispatch.ROUTES_KEY, value)


def test_an_unparseable_stored_route_falls_back() -> None:
    table = dispatch.resolve({dispatch.ROUTES_KEY: {"implement": {"candidates": 3}}})
    assert table.routes["implement"] == routing.DEFAULT_ROUTES["implement"]
