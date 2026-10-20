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
    "done",
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
    "done": 3,
    "stalled": 4,
    "running": 5,
    "idle": 6,
    "hibernated": 7,
    "exited": 8,
    "unknown": 9,
}

#: The attention classes a person (or a driver acting for one) must act on.
NEEDS_YOU: frozenset[Attention] = frozenset({"approval", "blocked", "waiting"})


#: Startup gates an agent CLI stops at before any work (WI-917), as words.
GATES: dict[str, str] = {
    "folder-trust": "folder trust",
    "external-imports": "external CLAUDE.md imports",
    "read-outside-cwd": "read outside the working directory",
}


#: Turns a first turn takes: read the brief, then say what it is (WI-1854).
FIRST_TURN_TURNS = 3

#: The reason a session idle since its first turn is shown with.
AWAITING_TASK = (
    "awaiting task: idle at its prompt since its first turn, no work done; "
    "give it the task with session_input"
)


#: The reason a session at rest with unsent text in its input box is shown
#: with (WI-1870).
INPUT_PENDING = (
    "input pending: at rest with text typed into its input box but not "
    "submitted; read session_screen, then press enter with session_input"
)

#: Codex echoed input after a model-API error but started no turn (WI-2027).
WEDGED = (
    "wedged: Codex shows a model-API error and echoed input since, but "
    "started no turn; restart it keeping its worktree and conversation with "
    "session_hibernate, then session_wake"
)


#: The reason an `AUTOPILOT: DONE` with no result report is shown with, when
#: `autopilot_done_requires_result_report` is on (WI-2039).
DONE_WITHOUT_REPORT = (
    "done without a result report: it printed AUTOPILOT: DONE but posted no "
    "session_report kind=result this turn; ask it for one"
)


def result_reported(
    *,
    report_kind: str | None,
    report_at: datetime | None,
    turn_started_at: datetime | None,
) -> bool:
    """Whether the session's latest report is a result made this turn.

    A result from an earlier turn says what that turn did, not this one, so
    it does not count once a later turn has begun. With no turn start known,
    any result report counts.
    """
    if report_kind != "result" or report_at is None:
        return False
    return turn_started_at is None or report_at >= turn_started_at


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
    done_at: datetime | None = None,
    turns: int | None = None,
) -> Verdict:
    """Where one session belongs in the oversight table, and why.

    `turns` is the engine's metered turn count, None when it has none. A
    session resting after no more than `FIRST_TURN_TURNS` of them did its
    first turn and nothing else: it was never given work (WI-1854).
    """
    if activity == "hibernated":
        return Verdict("hibernated", "hibernated to free memory; wake it to continue")
    if activity is None or alive is None:
        return Verdict("unknown", "the engine could not be asked")
    if activity == "stopped":
        return Verdict("exited", "stopped on request")
    if not alive:
        return Verdict("exited", f"its process ended ({activity})")
    if approval_question or activity == "awaiting-approval":
        if approval_kind == "question" and approval_question:
            # WI-1871: an agent's question menu (AskUserQuestion) waits on an
            # answer just as a permission dialog does.
            return Verdict(
                "approval",
                f"asking a question (answer with session_answer): {approval_question}",
            )
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
        if done_at is not None:
            return Verdict("done", "said it is done (AUTOPILOT: DONE), at its prompt")
        if _awaiting_task(turns):
            return Verdict("waiting", AWAITING_TASK)
        return Verdict("waiting", "at its prompt, waiting for the next instruction")
    if activity == "running":
        if last_output_at is not None and now - last_output_at >= stall_after:
            minutes = int((now - last_output_at).total_seconds() // 60)
            return Verdict("stalled", f"running, but nothing printed for {minutes} min")
        return Verdict("running", "working")
    if activity == "idle" and done_at is None and _awaiting_task(turns):
        return Verdict("waiting", AWAITING_TASK)
    return Verdict("idle", "resting, not at a recognised prompt")


def _awaiting_task(turns: int | None) -> bool:
    return turns is not None and 0 < turns <= FIRST_TURN_TURNS
