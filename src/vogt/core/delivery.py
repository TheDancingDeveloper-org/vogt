"""Did typed input land? The acknowledgement `session.input` gives (WI-918).

Writing bytes to a PTY always "succeeds"; what a driver needs to know is
what the agent did with them. Judged from what the engine reported before
the input and from a few quick reads of the screen after it — evidence, not
a promise — and said with the reason, so a driver that disagrees can see
what the judgement rested on.

Pure: observations are passed in.
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from typing import Literal

Delivery = Literal["typed", "delivered", "queued", "unconfirmed"]

#: Claude Code's hint under its input box while messages wait behind a turn
#: ("Press up to edit queued messages"), and Codex's queued-message line.
QUEUED_HINT = re.compile(r"(?i)queued messages?|message(?:s)? queued")


@dataclass(frozen=True)
class Observation:
    """One read of the session after the input: its activity and screen."""

    activity: str | None
    lines: tuple[str, ...]


@dataclass(frozen=True)
class Verdict:
    delivery: Delivery
    evidence: str


def judge(
    *,
    submitted: bool,
    before: str | None,
    after: list[Observation],
) -> Verdict:
    """What became of the input, from the activity before it and the reads
    after it, in order."""
    if not submitted:
        return Verdict(
            "typed",
            "no Enter was pressed: the text is in the input, not sent",
        )
    for seen in after:
        if any(QUEUED_HINT.search(line) for line in seen.lines):
            return Verdict(
                "queued",
                "the agent shows a queued-message hint: it will take the "
                "input when its current turn ends",
            )
    if before == "running":
        return Verdict(
            "queued",
            "a turn was already running when it was sent; agent CLIs queue "
            "input behind the running turn",
        )
    for seen in after:
        if seen.activity == "running":
            return Verdict("delivered", "the agent started a turn after it")
        if seen.activity == "awaiting-approval":
            return Verdict(
                "delivered",
                "the agent took it and is now asking for approval",
            )
    return Verdict(
        "unconfirmed",
        "sent, but no turn started and no queued hint showed while Vogt "
        "watched; read session_screen",
    )
