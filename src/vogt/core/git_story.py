"""Deriving a work item's git story — branch, PR, phase — from observed data.

A work item can already answer two collected questions on its own: which
branches are bound to it (#283) and which pull request implements it (#284).
This module is the pure derivation that reads those two facts back as a single
*where is this in git* story: a branch/PR summary, a single derived **phase**
shown beside the workflow state, and the obvious contradictions between the two
surfaced as drift.

Everything here is **derived and read-only** (FR-O2): a phase is computed from
what was observed, never written onto the item, and it never competes with the
workflow state the machine owns. The phase is a second, honest opinion — "the
state says review, but no PR has ever been opened" is exactly the disagreement
this is meant to make visible rather than average away.

Task-run conclusion (#291) is a deliberate omission: it is an engine-side
record not yet surfaced through Vogt's `SessionSummary`, so the phase is derived
from branches and the PR edge alone, and the conclusion input remains a seam
(noted on the story so a reader knows it is *not* being considered).
"""

from __future__ import annotations

from typing import Literal

#: The derived phase ladder, in order. A work item climbs it as git evidence
#: accumulates: nothing → a branch exists → a PR is open → a review is under
#: way → it merged. It is shown *beside* the workflow state, never as it.
GitPhase = Literal[
    "no_branch", "branch_active", "pr_open", "in_review", "merged"
]

#: The derived PR state, richer than the observation's raw open/closed/merged:
#: `draft` and `in-review` are read off the PR's own fields so the phase can
#: tell "opened" from "being reviewed". The *absence* of a PR is Python `None`,
#: not a member here — an existing PR always has one of these states.
PrState = Literal["draft", "open", "in-review", "merged", "closed"]

#: Review-decision values that mean a review is actually under way, as opposed
#: to merely required-but-not-started. Matched case-insensitively. A forge that
#: does not expose a review decision leaves this `None`, which reads as `open`,
#: never as `in-review` — an absent review is not a started one.
REVIEW_DECIDED: frozenset[str] = frozenset(
    {"approved", "changes_requested", "commented", "dismissed"}
)

#: PR states that mean the PR is still live — the ones that contradict a
#: closed/done work item.
_OPEN_PR_STATES: frozenset[str] = frozenset({"draft", "open", "in-review"})

# -- drift codes + messages ------------------------------------------------

DRIFT_CLOSED_ITEM_OPEN_PR = "closed_item_open_pr"
DRIFT_MERGED_PR_OPEN_ITEM = "merged_pr_open_item"
DRIFT_ACTIVE_BRANCH_DONE_ITEM = "active_branch_done_item"


def derive_pr_state(
    raw_state: str | None, *, draft: bool, review_decision: str | None
) -> PrState:
    """Read the PR's collected fields into the richer derived state.

    `merged` and `closed` are lifecycle facts the sync already distinguishes
    (a merged PR shipped, a bare closed one did not). Above them, `draft` and
    `in-review` are derived from the PR's own draft flag and review decision —
    `None` where the forge did not say, which is honestly `open`, not a guess
    that a review has begun.
    """
    normalised = (raw_state or "").strip().lower()
    if normalised == "merged":
        return "merged"
    if normalised == "closed":
        return "closed"
    if draft:
        return "draft"
    if review_decision and review_decision.strip().lower() in REVIEW_DECIDED:
        return "in-review"
    return "open"


def derive_phase(*, has_branch: bool, pr_state: PrState | None) -> GitPhase:
    """The single phase shown beside the workflow state.

    The PR dominates when it is live or shipped: an open PR is `pr_open`, a
    reviewed one `in_review`, a merged one `merged`. With no live PR the story
    falls back to the branch — a bound branch is `branch_active`, and nothing
    at all is `no_branch`. A *closed, unmerged* PR is a dead end, not a phase
    of its own: the item reverts to whatever its branch says.
    """
    if pr_state == "merged":
        return "merged"
    if pr_state == "in-review":
        return "in_review"
    if pr_state in ("open", "draft"):
        return "pr_open"
    return "branch_active" if has_branch else "no_branch"


def derive_drift(
    *, item_terminal: bool, pr_state: PrState | None, has_active_branch: bool
) -> list[tuple[str, str]]:
    """The contradictions between the item and its git evidence (FR-O2).

    Three of them, each an obvious disagreement a person would want flagged
    rather than reconciled:

    - a **closed/done item with a still-open PR** — the work is marked done but
      the change has not landed;
    - a **merged PR under a still-open item** — the change shipped but the item
      never moved to done;
    - an **active branch on a done item** — a checkout is still being committed
      to for work the item says is finished.

    Returned as `(code, message)` pairs, empty when the two agree. Derived and
    read-only: this *reports* the disagreement, it does not resolve it.
    """
    findings: list[tuple[str, str]] = []
    if item_terminal and pr_state in _OPEN_PR_STATES:
        findings.append(
            (
                DRIFT_CLOSED_ITEM_OPEN_PR,
                "the item is closed but its pull request is still open — "
                "the change has not landed",
            )
        )
    if pr_state == "merged" and not item_terminal:
        findings.append(
            (
                DRIFT_MERGED_PR_OPEN_ITEM,
                "the pull request merged but the item is still open — "
                "it never moved to done",
            )
        )
    if item_terminal and has_active_branch:
        findings.append(
            (
                DRIFT_ACTIVE_BRANCH_DONE_ITEM,
                "a branch is still active in a checkout for an item marked done",
            )
        )
    return findings


__all__ = [
    "DRIFT_ACTIVE_BRANCH_DONE_ITEM",
    "DRIFT_CLOSED_ITEM_OPEN_PR",
    "DRIFT_MERGED_PR_OPEN_ITEM",
    "REVIEW_DECIDED",
    "GitPhase",
    "PrState",
    "derive_drift",
    "derive_phase",
    "derive_pr_state",
]
