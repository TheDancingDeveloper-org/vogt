"""Model routes: the ordered candidates a task class may run on (WI-1260).

A dispatch profile names one template and model per purpose. A route says
what to fall back to when that choice is unavailable: an ordered list of
candidates, each a driver template, a model, and the capacity *tier* it
spends. `pick` walks the list and skips every candidate whose circuit
breaker is open, so oversight stops re-deriving the choice by hand.

Tiers, cheapest first in the shipped routes:

- `router` — a group name on the estate LiteLLM proxy (`code`, `small`,
  `reasoning`). The proxy does its own ordered fallback, cooldowns and
  provider budgets; Vogt sees one candidate.
- `community` — a shared, public free key. Best effort, and never for
  sensitive work: a route marked `sensitive` refuses it at validation, and
  `pick(..., sensitive=True)` skips it too.
- `subscription` — the Claude drivers on the operator's subscription.

Keys never appear here: a candidate names a template and a model, and the
template's own environment carries the credential.

Pure: nothing here reads storage, the engine, the network or the clock.
"""

from __future__ import annotations

from collections.abc import Collection, Mapping
from typing import Literal

from pydantic import BaseModel, ConfigDict, Field, ValidationError, model_validator

Tier = Literal["router", "community", "subscription"]


class RouteError(ValueError):
    """A route value that is not the shape the routes key holds."""


class Candidate(BaseModel):
    """One driver and model a route may start."""

    model_config = ConfigDict(extra="forbid", frozen=True)

    template: str = Field(min_length=1)
    model: str = Field(min_length=1)
    tier: Tier
    effort: str | None = Field(default=None, min_length=1)

    @property
    def key(self) -> str:
        """The breaker key: one per template and model."""
        return f"{self.template}:{self.model}"


class Route(BaseModel):
    """A task class's candidates, tried in order."""

    model_config = ConfigDict(extra="forbid", frozen=True)

    candidates: tuple[Candidate, ...] = Field(min_length=1)
    #: Security, prod and oversight work: no community capacity, ever.
    sensitive: bool = False

    @model_validator(mode="after")
    def _no_community_when_sensitive(self) -> Route:
        if self.sensitive and any(c.tier == "community" for c in self.candidates):
            msg = "a sensitive route cannot list a community-tier candidate"
            raise ValueError(msg)
        keys = [c.key for c in self.candidates]
        if len(set(keys)) != len(keys):
            msg = "a route lists each template:model at most once"
            raise ValueError(msg)
        return self


def _claude(model: str, effort: str | None = "low") -> Candidate:
    return Candidate(template="claude", model=model, tier="subscription", effort=effort)


def _klaudia(group: str) -> Candidate:
    return Candidate(template="klaudia", model=group, tier="router")


#: The shipped routes, keyed by dispatch purpose. Each first candidate
#: matches the purpose's default profile model family; the rest are its
#: fallbacks. Review, design, investigation and careful work stay on the
#: subscription (operator rule: no grok or community capacity there).
DEFAULT_ROUTES: dict[str, Route] = {
    "implement": Route(
        candidates=(
            Candidate(template="klaudia", model="grok-4.7", tier="router"),
            _klaudia("code"),
            _claude("haiku", effort=None),
        )
    ),
    "implement-careful": Route(candidates=(_claude("opus"),), sensitive=True),
    "review": Route(candidates=(_claude("opus"),), sensitive=True),
    "mechanical": Route(candidates=(_klaudia("small"), _claude("haiku", effort=None))),
    "investigate": Route(candidates=(_claude("opus"),), sensitive=True),
    "design": Route(candidates=(_claude("opus"),), sensitive=True),
}


def validate(
    value: Mapping[str, object], classes: Collection[str]
) -> dict[str, object]:
    """The canonical stored form of a routes value, or `RouteError`.

    `value` maps a class (one of `classes`) to a whole route; a deployment
    replaces a class's route rather than patching single candidates, since
    order is the point. `{}` clears back to the shipped routes.
    """
    unknown = sorted(name for name in value if name not in classes)
    if unknown:
        msg = (
            f"unknown task class {', '.join(map(repr, unknown))}; "
            f"the classes are {', '.join(classes)}"
        )
        raise RouteError(msg)
    out: dict[str, object] = {}
    for name, fields in value.items():
        try:
            route = Route.model_validate(fields)
        except ValidationError as error:
            problems = "; ".join(
                f"{'.'.join(str(p) for p in item['loc']) or 'value'}: {item['msg']}"
                for item in error.errors()
            )
            msg = f"invalid route for {name} — {problems}"
            raise RouteError(msg) from None
        out[name] = route.model_dump(mode="json", exclude_none=True)
    return out


def resolve(stored: Mapping[str, object] | None) -> dict[str, Route]:
    """The shipped routes with each stored class's route over its default.

    A stored route that no longer parses is ignored, not fatal.
    """
    routes = dict(DEFAULT_ROUTES)
    for name, fields in (stored or {}).items():
        if name not in routes:
            continue
        try:
            routes[name] = Route.model_validate(fields)
        except ValidationError:
            continue
    return routes


class Pick(BaseModel):
    """What `pick` chose, and what it passed over on the way."""

    model_config = ConfigDict(frozen=True)

    candidate: Candidate | None
    skipped: tuple[str, ...] = ()


def pick(
    route: Route, open_breakers: Collection[str] = (), *, sensitive: bool = False
) -> Pick:
    """The first candidate whose breaker is closed.

    `open_breakers` holds candidate keys (`template:model`) currently failing.
    `sensitive` also drops community capacity from a route that was not
    itself marked sensitive. `candidate` is `None` when nothing is left; the
    caller decides whether that refuses the start or queues it.
    """
    skipped: list[str] = []
    for candidate in route.candidates:
        if candidate.key in open_breakers or (
            sensitive and candidate.tier == "community"
        ):
            skipped.append(candidate.key)
            continue
        return Pick(candidate=candidate, skipped=tuple(skipped))
    return Pick(candidate=None, skipped=tuple(skipped))


__all__ = [
    "DEFAULT_ROUTES",
    "Candidate",
    "Pick",
    "Route",
    "RouteError",
    "Tier",
    "pick",
    "resolve",
    "validate",
]
