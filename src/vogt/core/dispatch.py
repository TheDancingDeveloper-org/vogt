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
validated here too, and `check` judges an agent's start against them
(WI-1329). They are enforced only when `dispatch.enforce` is on; until a
person turns it on, a start that breaks one is recorded, not refused.

A profile never carries a role or a permission posture. Both are a person's
decision on each start, and a profile is a default an agent can name, so a
profile that set either would let an agent raise itself by proxy.

Pure: nothing here reads storage, the engine or the clock.
"""

from __future__ import annotations

from collections.abc import Mapping
from typing import Literal, get_args

from pydantic import BaseModel, ConfigDict, Field, ValidationError, field_validator

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
BUDGET_RESTARTS_KEY = "dispatch.budget_restarts_per_item"
ENFORCE_KEY = "dispatch.enforce"
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
    #: Turns by the model the start resolves to, over `budget_turns` (WI-1833):
    #: a small model spends more turns on the same task (rebase and CI loops
    #: on haiku ran out at 120). Keyed by the model as named on the start.
    budget_turns_by_model: dict[str, int] = Field(default_factory=dict)
    budget_wall_clock_minutes: int | None = Field(default=None, gt=0)
    worktree: bool = False
    repo_map: bool = False
    autopilot: bool = True
    #: A hint carried into the brief: the session should read and report,
    #: not change the tree. Advisory; enforcement is the engine policy's.
    read_only: bool = False
    #: The MCP servers a start gets (WI-2740): `worker` keeps the vogt worker
    #: tool subset and cadastre, so the first request stays small.
    mcp_set: Literal["full", "worker"] = "full"

    @field_validator("budget_turns_by_model")
    @classmethod
    def _positive_turns(cls, value: dict[str, int]) -> dict[str, int]:
        _positive(value)
        return value

    def turns_for(self, model: str | None) -> int | None:
        """The turn budget for a start that runs `model`."""
        if model is None:
            return self.budget_turns
        if model in self.budget_turns_by_model:
            return self.budget_turns_by_model[model]
        # A family name matches the model ids that carry it: `haiku` sizes
        # `claude-haiku-4-5-20251001` too. The longest matching key wins.
        family = [
            key
            for key in self.budget_turns_by_model
            if key in model.split("-") or model.startswith(f"{key}-")
        ]
        if family:
            return self.budget_turns_by_model[max(family, key=len)]
        return self.budget_turns


def _positive(value: dict[str, int] | None) -> dict[str, int] | None:
    if value is not None and any(turns <= 0 for turns in value.values()):
        msg = "every turn budget must be greater than 0"
        raise ValueError(msg)
    return value


class ProfileOverride(BaseModel):
    """One profile's fields as a deployment overrides them: every one optional."""

    model_config = ConfigDict(extra="forbid")

    template: str | None = Field(default=None, min_length=1)
    model: str | None = Field(default=None, min_length=1)
    effort: str | None = Field(default=None, min_length=1)
    budget_turns: int | None = Field(default=None, gt=0)
    budget_turns_by_model: dict[str, int] | None = None
    budget_wall_clock_minutes: int | None = Field(default=None, gt=0)
    worktree: bool | None = None
    repo_map: bool | None = None
    autopilot: bool | None = None
    read_only: bool | None = None
    mcp_set: Literal["full", "worker"] | None = None

    @field_validator("budget_turns_by_model")
    @classmethod
    def _positive_turns(cls, value: dict[str, int] | None) -> dict[str, int] | None:
        return _positive(value)


#: The code defaults (design C10). A deployment overrides fields, never has
#: to restate a whole profile.
DEFAULT_PROFILES: dict[Purpose, Profile] = {
    "implement": Profile(
        template="klaudia",
        model="grok-4.7",
        budget_turns=120,
        budget_turns_by_model={"haiku": 200},
        budget_wall_clock_minutes=90,
        worktree=True,
        repo_map=True,
        autopilot=True,
        mcp_set="worker",
    ),
    "implement-careful": Profile(
        template="claude",
        model="opus",
        effort="low",
        budget_turns=150,
        mcp_set="worker",
    ),
    "review": Profile(
        template="claude",
        model="opus",
        effort="low",
        budget_turns=40,
        autopilot=False,
        read_only=True,
        mcp_set="worker",
    ),
    "mechanical": Profile(
        template="claude", model="haiku", budget_turns=60, mcp_set="worker"
    ),
    "investigate": Profile(
        template="claude", model="opus", effort="low", budget_turns=60
    ),
    "design": Profile(template="claude", model="opus", effort="low", budget_turns=60),
}

DEFAULT_MAX_WORKERS_PER_OVERSEER = 5
DEFAULT_ESCALATE_AFTER_FAILED_REVIEWS = 2
DEFAULT_REVIEW_BY_OVERSIGHT_ONLY = True
#: How many times the core restarts an item's worker from its hand-off after
#: a budget stop (WI-1833). `0` turns the restart off; the hand-off is still
#: written.
DEFAULT_BUDGET_RESTARTS_PER_ITEM = 1
#: Off, so a deployment's starts change only once a person turns it on.
DEFAULT_ENFORCE = False


class _IntSetting(BaseModel):
    model_config = ConfigDict(extra="forbid")
    value: int = Field(gt=0)


class _CountSetting(BaseModel):
    model_config = ConfigDict(extra="forbid")
    value: int = Field(ge=0)


class _BoolSetting(BaseModel):
    model_config = ConfigDict(extra="forbid")
    value: bool


#: The scalar settings are stored as `{"value": …}`: a preference value is
#: always a JSON object.
_SCALAR_SHAPES: dict[str, type[BaseModel]] = {
    MAX_WORKERS_KEY: _IntSetting,
    ESCALATE_KEY: _IntSetting,
    REVIEW_BY_OVERSIGHT_KEY: _BoolSetting,
    BUDGET_RESTARTS_KEY: _CountSetting,
    ENFORCE_KEY: _BoolSetting,
}
KEYS: tuple[str, ...] = (PROFILES_KEY, *_SCALAR_SHAPES)


class DispatchSettings(BaseModel):
    """The deployment's dispatch table with every default filled in."""

    model_config = ConfigDict(frozen=True)

    profiles: dict[Purpose, Profile]
    max_workers_per_overseer: int = DEFAULT_MAX_WORKERS_PER_OVERSEER
    escalate_after_failed_reviews: int = DEFAULT_ESCALATE_AFTER_FAILED_REVIEWS
    review_by_oversight_only: bool = DEFAULT_REVIEW_BY_OVERSIGHT_ONLY
    budget_restarts_per_item: int = DEFAULT_BUDGET_RESTARTS_PER_ITEM
    enforce: bool = DEFAULT_ENFORCE


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
        (BUDGET_RESTARTS_KEY, "budget_restarts_per_item"),
        (ENFORCE_KEY, "enforce"),
    ):
        raw = stored.get(key)
        if not raw:
            continue
        try:
            scalars[field] = _SCALAR_SHAPES[key].model_validate(raw).value  # type: ignore[attr-defined]
        except ValidationError:
            continue
    return DispatchSettings(profiles=profiles, **scalars)  # type: ignore[arg-type]


Rule = Literal["review_by_oversight_only", "profile_model", "max_workers_per_overseer"]
CallerRole = Literal["person", "oversight", "worker"]


class Violation(BaseModel):
    """One dispatch rule a start breaks, with the values that broke it."""

    model_config = ConfigDict(frozen=True)

    rule: Rule
    message: str


class Decision(BaseModel):
    """What `check` found: the rules broken, and the purpose to run instead.

    `escalated_to` is set when an `implement` start is resolved as
    `implement-careful` because the item has failed review often enough.
    """

    model_config = ConfigDict(frozen=True)

    violations: tuple[Violation, ...] = ()
    escalated_to: Purpose | None = None
    blockers_count: int = 0


#: The purpose a start that names none is judged as: the default implement
#: profile. Without it an agent could leave the purpose out to run any model
#: and skip the escalation (WI-1329).
DEFAULT_PURPOSE: Purpose = "implement"


def judged_purpose(purpose: Purpose | None) -> Purpose:
    """The purpose the dispatch rules judge a start as."""
    return DEFAULT_PURPOSE if purpose is None else purpose


def escalation(
    purpose: Purpose | None, blockers_count: int, settings: DispatchSettings
) -> Purpose | None:
    """`implement-careful` when an `implement` item has failed review enough.

    A start with no purpose is judged as `implement`.
    """
    if (
        judged_purpose(purpose) == "implement"
        and blockers_count >= settings.escalate_after_failed_reviews
    ):
        return "implement-careful"
    return None


def check(
    *,
    purpose: Purpose | None,
    review: bool,
    model: str | None,
    caller_role: CallerRole | None,
    live_workers: int,
    blockers_count: int,
    settings: DispatchSettings,
) -> Decision:
    """Judge one start against the dispatch rules (design §2.4, WI-1329).

    `purpose` is the purpose the start will run (after any escalation) and
    `model` the model it will run, the caller's or the profile's. A start
    with no purpose is judged as `implement`, and every purpose but review
    must run its profile's model: a review may run another model than the
    author's, and `review.never_models` bounds it instead.
    `caller_role` is the starter's engine role (`person` for a person);
    `live_workers` counts the caller's live started sessions. A rule that
    has nothing to judge (no model, an unknown role) is not broken. The
    project's `review.never_models` is not judged here: that refusal holds
    for every caller and is made before this (WI-1590).
    """
    violations: list[Violation] = []
    if review and caller_role == "worker" and settings.review_by_oversight_only:
        violations.append(
            Violation(
                rule="review_by_oversight_only",
                message=(
                    "review_by_oversight_only=true: a worker session cannot "
                    "start a reviewer; a person or an oversight session does"
                ),
            )
        )
    judged = judged_purpose(purpose)
    if not review and model is not None:
        expected = settings.profiles[judged].model
        if model != expected:
            violations.append(
                Violation(
                    rule="profile_model",
                    message=(
                        f"purpose={judged} runs model {expected!r} by the "
                        f"dispatch profile, not {model!r}"
                    ),
                )
            )
    cap = settings.max_workers_per_overseer
    if caller_role in ("oversight", "worker") and live_workers >= cap:
        violations.append(
            Violation(
                rule="max_workers_per_overseer",
                message=(
                    f"max_workers_per_overseer={cap} reached: this session "
                    f"already has {live_workers} live workers; stop one first"
                ),
            )
        )
    return Decision(
        violations=tuple(violations),
        escalated_to=escalation(purpose, blockers_count, settings),
        blockers_count=blockers_count,
    )


__all__ = [
    "DEFAULT_PROFILES",
    "DEFAULT_PURPOSE",
    "KEYS",
    "KEY_PREFIX",
    "PROFILES_KEY",
    "PURPOSES",
    "Decision",
    "DispatchError",
    "DispatchSettings",
    "Profile",
    "Purpose",
    "Violation",
    "check",
    "escalation",
    "judged_purpose",
    "resolve",
    "validate",
]
