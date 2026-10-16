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

#: Codex's prompt glyph. It marks the composer at the bottom of the screen
#: and each of the person's messages in the transcript above it, so the
#: composer is the last line that starts with it.
CODEX_PROMPT = "\u203a"

#: How much of the submitted text a composer has to show to count as still
#: holding it: enough to rule out a placeholder, short enough to survive the
#: composer wrapping a long text.
_COMPOSER_MATCH = 40


def composer_holds(lines: tuple[str, ...], text: str) -> bool:
    """Whether Codex's composer still shows `text`, unsubmitted (WI-1669).

    A resumed Codex can swallow the Enter while its status line already
    reads as working; the text left after the prompt is the only sign.
    """
    wanted = next((line.strip() for line in text.splitlines() if line.strip()), "")
    if not wanted:
        return False
    composer = next(
        (
            line.strip()
            for line in reversed(lines)
            if line.lstrip().startswith(CODEX_PROMPT)
        ),
        None,
    )
    if composer is None:
        return False
    shown = composer.removeprefix(CODEX_PROMPT).strip()
    if not shown:
        return False
    n = min(len(wanted), _COMPOSER_MATCH)
    return shown[:n] == wanted[:n]


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
    pending: str | None = None,
) -> Verdict:
    """What became of the input, from the activity before it and the reads
    after it, in order.

    `pending` is the submitted text, given for Codex only: while the latest
    read still shows it in the composer the Enter has not landed, whatever
    the activity says (WI-1669).
    """
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
    if pending and after and composer_holds(after[-1].lines, pending):
        return Verdict(
            "unconfirmed",
            "the text is still in the composer after Enter: the submit did "
            "not land; read session_screen",
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
