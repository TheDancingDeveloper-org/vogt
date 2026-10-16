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

#: Codex's prompt glyph. It starts the composer and every copy of the
#: person's messages in the transcript, and marks the highlighted choice in
#: its dialogs, so a line starting with it proves nothing on its own.
CODEX_PROMPT = "\u203a"

#: Codex's footer under the composer: the model, a " · ", the directory
#: (`  grok high · ~/Working/...`). Captured live on 2026-10-16 (WI-1669).
_CODEX_FOOTER = re.compile(r"^\s{2}\S.* \u00b7 \S")

#: How much of the submitted text a composer has to show to count as still
#: holding it: enough to rule out the placeholder, short enough to survive
#: the composer wrapping a long text.
_COMPOSER_MATCH = 40


def codex_composer(lines: tuple[str, ...]) -> str | None:
    """The text in Codex's composer, or None when no composer is drawn.

    The composer is the prompt line in the block directly above the footer:
    `› text`, its wrapped continuation lines, one blank line, then the
    model · directory line and a hint line. A `›` line anywhere else is a
    transcript copy or a dialog's choice. An empty string is a composer
    showing only its placeholder or nothing.
    """
    rows = list(lines)
    while rows and not rows[-1].strip():
        rows.pop()
    # The upper of the last two lines: the hint line under it can carry a
    # " · " too ("⚠ 1 warning · f2 to view").
    footer = next(
        (
            i
            for i in range(max(len(rows) - 2, 0), len(rows))
            if _CODEX_FOOTER.match(rows[i])
        ),
        None,
    )
    if footer is None:
        return None
    i = footer - 1
    while i >= 0 and not rows[i].strip():
        i -= 1
    block: list[str] = []
    while i >= 0 and rows[i].startswith("  ") and rows[i].strip():
        block.append(rows[i].strip())
        i -= 1
    if i < 0 or not rows[i].startswith(CODEX_PROMPT):
        return None
    block.append(rows[i].removeprefix(CODEX_PROMPT).strip())
    return " ".join(reversed(block)).strip()


def composer_holds(lines: tuple[str, ...], text: str) -> bool:
    """Whether Codex's composer still shows `text`, unsubmitted (WI-1669).

    A resumed Codex can swallow the Enter while it reads as working; the
    text left in the composer is the only sign.
    """
    wanted = " ".join(text.split())
    shown = codex_composer(lines)
    if not wanted or not shown:
        return False
    n = min(len(wanted), _COMPOSER_MATCH)
    return " ".join(shown.split())[:n] == wanted[:n]


def safe_to_resubmit(seen: Observation, text: str) -> bool:
    """Whether one more Enter can only submit `text` (WI-1669).

    Only while the composer is drawn and still holds the text, and nothing
    asks for an answer: an Enter on a dialog or an approval would accept
    whatever it has highlighted.
    """
    if seen.activity in {"awaiting-approval", "blocked"}:
        return False
    return composer_holds(seen.lines, text)


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
