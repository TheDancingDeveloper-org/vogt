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

Delivery = Literal["typed", "delivered", "queued", "stuck", "unconfirmed"]

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


#: Claude Code's placeholder for a paste it folds: `[Pasted text #1 +4 lines]`.
PASTED = re.compile(r"\[Pasted text #\d+")

#: Claude Code's input box is drawn between two full-width rules of `─`.
_CLAUDE_RULE = re.compile(r"^\s*\u2500{20,}\s*$")

#: Claude Code's prompt glyphs: `❯` now, `>` in the older boxed input.
_CLAUDE_PROMPTS = ("\u276f", ">")


def claude_composer(lines: tuple[str, ...]) -> str | None:
    """The text in Claude Code's input box, or None when none is drawn.

    The box is the last pair of `─` rules on the screen with a prompt line
    (`❯ text`, then any wrapped continuation lines) between them, as
    captured live on 2026-10-18 (WI-1870). An empty string is a box showing
    nothing, or only the "Press up to edit queued messages" hint.
    """
    rules = [i for i, line in enumerate(lines) if _CLAUDE_RULE.match(line)]
    if len(rules) < 2:
        return None
    top, bottom = rules[-2], rules[-1]
    inside = [line.strip() for line in lines[top + 1 : bottom]]
    if not inside or not inside[0].startswith(_CLAUDE_PROMPTS):
        return None
    first = inside[0]
    for glyph in _CLAUDE_PROMPTS:
        first = first.removeprefix(glyph)
    shown = " ".join([first.strip(), *inside[1:]]).strip()
    if QUEUED_HINT.search(shown):
        return ""
    return shown


def composer_text(lines: tuple[str, ...]) -> str | None:
    """What an agent's input box holds — Codex's composer or Claude Code's
    input box — or None when neither is drawn."""
    shown = codex_composer(lines)
    if shown is None:
        shown = claude_composer(lines)
    return shown


def composer_holds(lines: tuple[str, ...], text: str) -> bool:
    """Whether the input box still shows `text`, unsubmitted (WI-1669,
    WI-1870).

    A resumed Codex can swallow the Enter while it reads as working, and a
    Claude Code turn can end with a queued message left in its box; the
    text left in the input is the only sign.
    """
    wanted = " ".join(text.split())
    shown = composer_text(lines)
    if not wanted or not shown:
        return False
    if "\n" in text.strip() and PASTED.search(shown):
        # Claude Code folds a long paste into a placeholder in its box.
        return True
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

    `pending` is the submitted text: while the latest read shows the
    agent off its turn with the text still in its input box, the Enter has
    not landed (WI-1669) — `stuck`, not `queued`, even when a turn was
    running as it was sent (WI-1870).
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
    if (
        pending
        and after
        and after[-1].activity != "running"
        and composer_holds(after[-1].lines, pending)
    ):
        return Verdict(
            "stuck",
            "the text is still in the input box and no turn is running: the "
            "submit did not land; read session_screen",
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


def input_pending(lines: tuple[str, ...]) -> str | None:
    """The text an agent at rest still holds unsent in its input box, or
    None (WI-1870). Only meaningful for a session not running a turn."""
    shown = composer_text(lines)
    return shown or None
