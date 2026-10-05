"""What a session overseer should look at first (WI-915).

A driver overseeing many sessions wants one table ordered by who needs it:
a permission dialog that denies itself on a countdown, then an agent that
says it is blocked on a person, then one that finished its turn and waits
for the next instruction, then a turn that has gone quiet for too long, and
only after those the sessions that are simply working or resting. This
module decides that order from facts the engine reported, with the reason
in words — never a bare rank — so the table says *why* a row is at the top.

Pure: the clock is a parameter, and nothing here reads storage or the
engine.
"""

from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime, timedelta
from typing import Literal

Attention = Literal[
    "approval",
    "blocked",
    "waiting",
    "stalled",
    "running",
    "idle",
    "hibernated",
    "exited",
    "unknown",
]

#: Lower first. `stalled` sits above `running` because a turn that has
#: printed nothing for a long time is worth a look before one that is busy.
ORDER: dict[Attention, int] = {
    "approval": 0,
    "blocked": 1,
    "waiting": 2,
    "stalled": 3,
    "running": 4,
    "idle": 5,
    "hibernated": 6,
    "exited": 7,
    "unknown": 8,
}

#: The attention classes a person (or a driver acting for one) must act on.
NEEDS_YOU: frozenset[Attention] = frozenset({"approval", "blocked", "waiting"})


#: Startup gates an agent CLI stops at before any work (WI-917), as words.
GATES: dict[str, str] = {
    "folder-trust": "folder trust",
    "external-imports": "external CLAUDE.md imports",
    "read-outside-cwd": "read outside the working directory",
}


@dataclass(frozen=True)
class Verdict:
    attention: Attention
    reason: str


def classify(
    *,
    activity: str | None,
    alive: bool | None,
    ready: bool | None,
    approval_question: str | None,
    blocker: str | None,
    approval_kind: str | None = None,
    last_output_at: datetime | None,
    now: datetime,
    stall_after: timedelta,
) -> Verdict:
    """Where one session belongs in the oversight table, and why."""
    if activity == "hibernated":
        return Verdict("hibernated", "hibernated to free memory; wake it to continue")
    if activity is None or alive is None:
        return Verdict("unknown", "the engine could not be asked")
    if activity == "stopped":
        return Verdict("exited", "stopped on request")
    if not alive:
        return Verdict("exited", f"its process ended ({activity})")
    if approval_question or activity == "awaiting-approval":
        gate = GATES.get(approval_kind or "")
        if gate and approval_question:
            return Verdict(
                "approval", f"stopped at a startup gate ({gate}): {approval_question}"
            )
        return Verdict(
            "approval",
            f"asking for approval: {approval_question}"
            if approval_question
            else "showing a permission dialog",
        )
    if blocker:
        return Verdict("blocked", f"blocked on a person: {blocker}")
    if activity == "waiting-for-input" or (activity == "idle" and ready):
        return Verdict("waiting", "at its prompt, waiting for the next instruction")
    if activity == "running":
        if last_output_at is not None and now - last_output_at >= stall_after:
            minutes = int((now - last_output_at).total_seconds() // 60)
            return Verdict("stalled", f"running, but nothing printed for {minutes} min")
        return Verdict("running", "working")
    return Verdict("idle", "resting, not at a recognised prompt")
