"""Dispatch profiles: what a session started for a purpose runs with (WI-1323).

A start names *why* it is being made — `implement`, `review`, and so on —
and the profile for that purpose supplies the template, model, effort,
budget, worktree, repo map and autopilot it would otherwise have to restate
every time (design `agent-harness-services.md` §2.4). The table is data:
the code defaults below are the fallback, and a deployment's
`dispatch.profiles` preference overrides any field of any profile without
restating the rest.

The three limits beside it (`max_workers_per_overseer`,
`escalate_after_failed_reviews`, `review_by_oversight_only`) are read and
validated here too. Nothing in this item refuses a start on them; that is
WI-1329's enforcement.

A profile never carries a role or a permission posture. Both are a person's
decision on each start, and a profile is a default an agent can name, so a
profile that set either would let an agent raise itself by proxy.

Pure: nothing here reads storage, the engine or the clock.
"""

from __future__ import annotations

from collections.abc import Mapping
from typing import Literal, get_args

from pydantic import BaseModel, ConfigDict, Field, ValidationError

Purpose = Literal[
    "implement",
    "implement-careful",
    "review",
    "mechanical",
    "investigate",
    "design",
]
PURPOSES: tuple[Purpose, ...] = get_args(Purpose)

#: The preference keys this module owns. Every other `dispatch.*` key is
#: refused, so a typo is said rather than stored and silently ignored.
PROFILES_KEY = "dispatch.profiles"
MAX_WORKERS_KEY = "dispatch.max_workers_per_overseer"
ESCALATE_KEY = "dispatch.escalate_after_failed_reviews"
REVIEW_BY_OVERSIGHT_KEY = "dispatch.review_by_oversight_only"
KEY_PREFIX = "dispatch."


class DispatchError(ValueError):
    """A `dispatch.*` value that is not the shape its key holds."""


class Profile(BaseModel):
    """The start defaults one purpose resolves to."""

    model_config = ConfigDict(extra="forbid", frozen=True)

    template: str
    model: str
    effort: str | None = None
    budget_turns: int | None = Field(default=None, gt=0)
    budget_wall_clock_minutes: int | None = Field(default=None, gt=0)
    worktree: bool = False
    repo_map: bool = False
    autopilot: bool = True
    #: A hint carried into the brief: the session should read and report,
    #: not change the tree. Advisory; enforcement is the engine policy's.
    read_only: bool = False


class ProfileOverride(BaseModel):
    """One profile's fields as a deployment overrides them: every one optional."""

    model_config = ConfigDict(extra="forbid")

    template: str | None = Field(default=None, min_length=1)
    model: str | None = Field(default=None, min_length=1)
    effort: str | None = Field(default=None, min_length=1)
    budget_turns: int | None = Field(default=None, gt=0)
    budget_wall_clock_minutes: int | None = Field(default=None, gt=0)
    worktree: bool | None = None
    repo_map: bool | None = None
    autopilot: bool | None = None
    read_only: bool | None = None


#: The code defaults (design C10). A deployment overrides fields, never has
#: to restate a whole profile.
DEFAULT_PROFILES: dict[Purpose, Profile] = {
    "implement": Profile(
        template="klaudia",
        model="grok-4.7",
        budget_turns=120,
        budget_wall_clock_minutes=90,
        worktree=True,
        repo_map=True,
        autopilot=True,
    ),
    "implement-careful": Profile(
        template="claude", model="opus", effort="low", budget_turns=150
    ),
    "review": Profile(
        template="claude",
        model="opus",
        effort="low",
        budget_turns=40,
        autopilot=False,
        read_only=True,
    ),
    "mechanical": Profile(template="claude", model="haiku", budget_turns=60),
    "investigate": Profile(
        template="claude", model="opus", effort="low", budget_turns=60
    ),
    "design": Profile(template="claude", model="opus", effort="low", budget_turns=60),
}

DEFAULT_MAX_WORKERS_PER_OVERSEER = 5
DEFAULT_ESCALATE_AFTER_FAILED_REVIEWS = 2
DEFAULT_REVIEW_BY_OVERSIGHT_ONLY = True


class _IntSetting(BaseModel):
    model_config = ConfigDict(extra="forbid")
    value: int = Field(gt=0)


class _BoolSetting(BaseModel):
    model_config = ConfigDict(extra="forbid")
    value: bool


#: The scalar settings are stored as `{"value": …}`: a preference value is
#: always a JSON object.
_SCALAR_SHAPES: dict[str, type[BaseModel]] = {
    MAX_WORKERS_KEY: _IntSetting,
    ESCALATE_KEY: _IntSetting,
    REVIEW_BY_OVERSIGHT_KEY: _BoolSetting,
}
KEYS: tuple[str, ...] = (PROFILES_KEY, *_SCALAR_SHAPES)


class DispatchSettings(BaseModel):
    """The deployment's dispatch table with every default filled in."""

    model_config = ConfigDict(frozen=True)

    profiles: dict[Purpose, Profile]
    max_workers_per_overseer: int = DEFAULT_MAX_WORKERS_PER_OVERSEER
    escalate_after_failed_reviews: int = DEFAULT_ESCALATE_AFTER_FAILED_REVIEWS
    review_by_oversight_only: bool = DEFAULT_REVIEW_BY_OVERSIGHT_ONLY


def _problems(error: ValidationError) -> str:
    return "; ".join(
        f"{'.'.join(str(part) for part in item['loc']) or 'value'}: {item['msg']}"
        for item in error.errors()
    )


def validate(key: str, value: Mapping[str, object]) -> dict[str, object]:
    """The canonical stored form of a `dispatch.*` value, or `DispatchError`.

    `{}` clears a key back to the code default and is always accepted.
    """
    if key not in KEYS:
        msg = f"unknown dispatch setting {key!r}; the keys are {', '.join(KEYS)}"
        raise DispatchError(msg)
    if not value:
        return {}
    if key == PROFILES_KEY:
        unknown = sorted(name for name in value if name not in PURPOSES)
        if unknown:
            msg = (
                f"unknown purpose {', '.join(map(repr, unknown))} in "
                f"{PROFILES_KEY}; the purposes are {', '.join(PURPOSES)}"
            )
            raise DispatchError(msg)
        out: dict[str, object] = {}
        for name, fields in value.items():
            try:
                parsed = ProfileOverride.model_validate(fields)
            except ValidationError as error:
                msg = f"invalid {PROFILES_KEY}.{name} — {_problems(error)}"
                raise DispatchError(msg) from None
            out[name] = parsed.model_dump(mode="json", exclude_none=True)
        return out
    try:
        return _SCALAR_SHAPES[key].model_validate(value).model_dump(mode="json")
    except ValidationError as error:
        msg = f"invalid {key} value — {_problems(error)}"
        raise DispatchError(msg) from None


def resolve(stored: Mapping[str, Mapping[str, object]]) -> DispatchSettings:
    """The dispatch table from the stored `dispatch.*` values over the defaults.

    `stored` maps a key to its value; a missing or empty key is its default.
    A stored value that no longer parses (written by an older build) is
    ignored rather than failing every start, the way the Inbox reads its
    saved filter.
    """
    profiles = dict(DEFAULT_PROFILES)
    overrides = stored.get(PROFILES_KEY) or {}
    for name, fields in overrides.items():
        if name not in PURPOSES or not isinstance(fields, Mapping):
            continue
        try:
            override = ProfileOverride.model_validate(fields)
        except ValidationError:
            continue
        purpose: Purpose = name
        merged = profiles[purpose].model_dump() | override.model_dump(exclude_none=True)
        profiles[purpose] = Profile.model_validate(merged)
    scalars: dict[str, object] = {}
    for key, field in (
        (MAX_WORKERS_KEY, "max_workers_per_overseer"),
        (ESCALATE_KEY, "escalate_after_failed_reviews"),
        (REVIEW_BY_OVERSIGHT_KEY, "review_by_oversight_only"),
    ):
        raw = stored.get(key)
        if not raw:
            continue
        try:
            scalars[field] = _SCALAR_SHAPES[key].model_validate(raw).value  # type: ignore[attr-defined]
        except ValidationError:
            continue
    return DispatchSettings(profiles=profiles, **scalars)  # type: ignore[arg-type]


__all__ = [
    "DEFAULT_PROFILES",
    "KEYS",
    "KEY_PREFIX",
    "PROFILES_KEY",
    "PURPOSES",
    "DispatchError",
    "DispatchSettings",
    "Profile",
    "Purpose",
    "resolve",
    "validate",
]
