"""State machines, and rejections that name the rule they violated.

FR-W2 asks for two things that are easy to get half-right: transitions
governed by a *configurable* per-kind machine, and invalid transitions
rejected **with the violated rule named**. A bare "invalid transition" tells
an agent nothing it can act on; `transition.not_allowed: bug has no
open -> done edge (allowed: in_progress, blocked, wont_do)` tells it exactly
what to do next.

The definitions live in the `workflow_defs` table, seeded with the defaults
below, so changing a workflow is configuration rather than a release.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Literal

from vogt.core.entities import LifecycleState, WorkKind
from vogt.errors import VogtError

OPEN = "open"
IN_PROGRESS = "in_progress"
REVIEW = "review"
DONE = "done"
BLOCKED = "blocked"
WONT_DO = "wont_do"

#: States that mean the work is finished, one way or another.
TERMINAL_STATES = frozenset({DONE, WONT_DO})

#: The shipped default machine (DESIGN §3.3): open -> in_progress -> review
#: -> done, plus blocked and wont_do reachable from the live states. `done`
#: and `wont_do` reopen to `open` — work comes back, and a machine that
#: cannot express that gets worked around with duplicate items.
DEFAULT_TRANSITIONS: dict[str, list[str]] = {
    OPEN: [IN_PROGRESS, BLOCKED, WONT_DO],
    IN_PROGRESS: [REVIEW, BLOCKED, OPEN, WONT_DO],
    REVIEW: [DONE, IN_PROGRESS, BLOCKED, WONT_DO],
    BLOCKED: [OPEN, IN_PROGRESS, WONT_DO],
    DONE: [OPEN],
    WONT_DO: [OPEN],
}

DEFAULT_INITIAL_STATE = OPEN


class TransitionRejected(VogtError):
    """A transition was refused, naming the rule that refused it."""

    code = "transition_rejected"
    http_status = 409

    def __init__(self, rule: str, message: str) -> None:
        super().__init__(f"{rule}: {message}")
        self.rule = rule


@dataclass(frozen=True)
class Workflow:
    """One kind's state machine."""

    kind: WorkKind
    initial_state: str
    transitions: dict[str, list[str]]

    @property
    def states(self) -> tuple[str, ...]:
        seen: dict[str, None] = {}
        for source, targets in self.transitions.items():
            seen.setdefault(source, None)
            for target in targets:
                seen.setdefault(target, None)
        return tuple(seen)

    def allowed_from(self, state: str) -> tuple[str, ...]:
        return tuple(self.transitions.get(state, ()))

    def check(self, *, from_state: str, to_state: str) -> None:
        """Raise `TransitionRejected` unless the edge exists."""
        if from_state == to_state:
            raise TransitionRejected(
                "transition.no_op",
                f"item is already in state {to_state!r}",
            )
        if to_state not in self.states:
            raise TransitionRejected(
                "transition.unknown_state",
                f"{self.kind} has no state {to_state!r} "
                f"(states: {', '.join(sorted(self.states))})",
            )
        allowed = self.allowed_from(from_state)
        if to_state not in allowed:
            raise TransitionRejected(
                "transition.not_allowed",
                f"{self.kind} has no {from_state} -> {to_state} edge "
                f"(allowed from {from_state}: {', '.join(allowed) or 'nothing'})",
            )

    def to_definition(self) -> dict[str, object]:
        return {"initial_state": self.initial_state, "transitions": self.transitions}

    @classmethod
    def from_definition(cls, kind: WorkKind, definition: dict[str, object]) -> Workflow:
        raw_transitions = definition.get("transitions")
        if not isinstance(raw_transitions, dict):
            msg = f"workflow definition for {kind} has no transitions map"
            raise ValueError(msg)
        transitions = {
            str(source): [str(target) for target in targets]
            for source, targets in raw_transitions.items()
            if isinstance(targets, list)
        }
        initial = definition.get("initial_state", DEFAULT_INITIAL_STATE)
        return cls(kind=kind, initial_state=str(initial), transitions=transitions)


def default_workflow(kind: WorkKind) -> Workflow:
    """The machine every kind starts with.

    All four kinds share one shape today. They are stored per kind anyway,
    because the first time a `question` needs `answered` instead of `review`
    the alternative is a migration and a special case.
    """
    return Workflow(
        kind=kind,
        initial_state=DEFAULT_INITIAL_STATE,
        transitions={
            source: list(targets) for source, targets in DEFAULT_TRANSITIONS.items()
        },
    )


def check_completion_allowed(*, to_state: str, blockers: list[tuple[str, str]]) -> None:
    """`depends_on` blocks completion (FR-W8, DESIGN §3.1).

    Not an enforcement of compliance, trust or drift — those may never gate an
    operation (FR-G13). This is the declared meaning of the edge the user
    themselves created, and the rejection names both the rule and the items,
    so the caller can act without a second query.
    """
    if to_state != DONE or not blockers:
        return
    listed = ", ".join(f"{ref} ({state})" for ref, state in blockers)
    raise TransitionRejected(
        "transition.blocked_by_dependency",
        f"cannot complete while {len(blockers)} depends_on target(s) are "
        f"unfinished: {listed}",
    )


# -- project lifecycle (FR-P4) --------------------------------------------

LIFECYCLE_TRANSITIONS: dict[LifecycleState, list[LifecycleState]] = {
    "incubating": ["active", "archived"],
    "active": ["maintenance", "archived", "incubating"],
    "maintenance": ["active", "archived"],
    # Archived is not a grave: a project can come back, and the alternative
    # is people registering a second project for the same repository.
    "archived": ["active", "maintenance"],
}

LifecycleRule = Literal["lifecycle.no_op", "lifecycle.not_allowed"]


def check_lifecycle_transition(
    *, from_state: LifecycleState, to_state: LifecycleState
) -> None:
    """Raise `TransitionRejected` unless the lifecycle edge exists."""
    if from_state == to_state:
        raise TransitionRejected("lifecycle.no_op", f"project is already {to_state!r}")
    allowed = LIFECYCLE_TRANSITIONS.get(from_state, [])
    if to_state not in allowed:
        raise TransitionRejected(
            "lifecycle.not_allowed",
            f"no {from_state} -> {to_state} edge "
            f"(allowed from {from_state}: {', '.join(allowed) or 'nothing'})",
        )
