"""Check a session's completion claim against git and the forge (WI-1835).

A worker that prints `AUTOPILOT: DONE`, or moves its item to review or done,
is claiming something: that it committed, pushed, opened or merged a pull
request. Workers sometimes claim work they did not do, for example a PR "opened
and merged" that had merged the day before the session started. This module
compares the claim with facts read elsewhere and names each mismatch in words.

Pure: the facts and the clock are parameters, and nothing here reads git,
the forge or storage.
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field
from datetime import datetime

#: `PR #193`, `pull request #193`, `#193`, `pull/193`. A bare `#n` counts:
#: workers write it that way, and a number that names no observed PR is
#: ignored rather than flagged.
_PR_NUMBER = re.compile(r"(?:\bpull/|#)(\d{1,7})\b")
#: The claim that something landed: "merged", "was merged", "landed".
_MERGE_CLAIM = re.compile(r"\b(?:merged|landed)\b", re.IGNORECASE)
_FAILING_CHECKS = frozenset({"failure", "failed", "error", "red"})


@dataclass(frozen=True)
class PullFact:
    """One pull request as the forge was last observed to report it."""

    number: int
    state: str
    merged_at: datetime | None = None
    merged_by: str | None = None
    checks: str | None = None
    head_ref: str | None = None


@dataclass(frozen=True)
class ClaimFacts:
    """What was checked. A `None` field means it could not be read, and that
    fact is left unchecked rather than counted as a mismatch."""

    claim_text: str | None
    session_started_at: datetime | None
    branch: str | None = None
    head: str | None = None
    pushed: bool | None = None
    new_commits: int | None = None
    pulls: tuple[PullFact, ...] = field(default_factory=tuple)


def mentioned_pulls(text: str | None) -> frozenset[int]:
    """The PR numbers a claim names."""
    if not text:
        return frozenset()
    return frozenset(int(match) for match in _PR_NUMBER.findall(text))


def claims_merge(text: str | None) -> bool:
    return bool(text and _MERGE_CLAIM.search(text))


def mismatches(facts: ClaimFacts) -> list[str]:
    """Each way the claim and the facts disagree, in words. Empty when they
    agree or when nothing could be checked."""
    found: list[str] = []
    named = mentioned_pulls(facts.claim_text)
    merge = claims_merge(facts.claim_text)
    for pull in facts.pulls:
        if pull.number not in named:
            # Not named, but from this branch or bound to the item: a PR that
            # merged before the session started, from a session that made no
            # commits, is the same false claim made without saying it.
            if facts.new_commits == 0 and _merged_before_start(
                pull, facts.session_started_at
            ):
                found.append(
                    f"PR #{pull.number} merged at {_at(pull)}, before this "
                    "session started, and the session made no commits"
                )
            continue
        if merge and pull.state != "merged":
            found.append(f"claimed merged; PR #{pull.number} is {pull.state}")
        if _merged_before_start(pull, facts.session_started_at):
            assert facts.session_started_at is not None
            found.append(
                f"claimed PR #{pull.number}; it merged {_at(pull)}, before this "
                f"session started ({facts.session_started_at.isoformat()})"
            )
        if (pull.checks or "").lower() in _FAILING_CHECKS:
            found.append(f"PR #{pull.number} CI is {pull.checks}")
    if facts.new_commits == 0:
        on = f" on {facts.branch}" if facts.branch else ""
        found.append(f"no new commits from this session{on}")
    if facts.pushed is False and facts.new_commits != 0:
        head = (facts.head or "")[:12]
        found.append(f"branch head {head} is not pushed".replace("  ", " "))
    return found


def _merged_before_start(pull: PullFact, started: datetime | None) -> bool:
    return (
        pull.state == "merged"
        and pull.merged_at is not None
        and started is not None
        and pull.merged_at < started
    )


def _at(pull: PullFact) -> str:
    by = f" by {pull.merged_by}" if pull.merged_by else ""
    when = pull.merged_at.isoformat() if pull.merged_at else "an unknown time"
    return f"{when}{by}"
