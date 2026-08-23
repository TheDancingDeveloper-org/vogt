"""Assembling a work item's git story — branch, PR, phase — with provenance.

Two collected facts meet here: the branches bound to the item (#283) and the
pull request the `implemented_by` edge points at (#284). Read back together
they answer *where is this in git?* — a branch/PR summary, a derived **phase**
shown beside the workflow state, and the contradictions between the two as
drift. The derivation itself is pure (`core/git_story.py`); this module only
fetches the observations and dresses each fact with its provenance and age, per
the product's first principle that a fact is only as good as its freshness.

Everything here is derived and read-only (FR-O2): nothing is written onto the
item, and the phase never competes with the workflow state the machine owns.
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field
from datetime import datetime

from vogt.application.context import AppContext
from vogt.application.models import (
    GitStoryDriftView,
    WorkItemBranchView,
    WorkItemGitStory,
    WorkItemPullRequestView,
)
from vogt.core.entities import Observation, WorkItem
from vogt.core.git_story import (
    PrState,
    derive_drift,
    derive_phase,
    derive_pr_state,
)
from vogt.core.workflow import TERMINAL_STATES
from vogt.storage.interface import ReadView

_FORGE_NUMBER = re.compile(r"#(\d+)$")

#: Which derived PR state is the *defining* one when more than one PR names the
#: item: a merged PR is the definitive answer (the work shipped), a live one
#: outranks a draft, and a dead (closed, unmerged) one is the last resort.
_STATE_RANK: dict[PrState, int] = {
    "merged": 5,
    "in-review": 4,
    "open": 3,
    "draft": 2,
    "closed": 1,
}


@dataclass(frozen=True)
class GitSignals:
    """The git-activity inputs the ranker reads, computed once per scope (#285).

    A moving item — one with a recently-committed branch or an open PR — should
    rank above idle work of the same priority. Computing these per item during
    a backlog scan would be a query storm; this gathers them for a whole project
    scope in two reads and answers each item by its ref.
    """

    open_pr_refs: frozenset[str] = frozenset()
    open_pr_numbers: frozenset[int] = frozenset()
    branch_age_by_ref: dict[str, int] = field(default_factory=dict)
    branch_age_by_number: dict[int, int] = field(default_factory=dict)

    def for_ref(self, ref: str) -> tuple[bool, int | None]:
        """`(has_open_pr, branch_activity_seconds)` for the item named `ref`."""
        number = _forge_number(ref)
        open_pr = ref in self.open_pr_refs or (
            number is not None and number in self.open_pr_numbers
        )
        age = self.branch_age_by_ref.get(ref)
        if age is None and number is not None:
            age = self.branch_age_by_number.get(number)
        return (open_pr, age)


def build_git_signals(
    ctx: AppContext, *, project_id: str | None, now: datetime
) -> GitSignals:
    """Gather the ranking git signals for a project scope (or the whole estate).

    Two reads: the open pull requests (whose `implements` edges name the items
    they lift), and the observed branches (whose most recent commit dates the
    activity). Both are keyed by subject ref *and* by forge number, so a
    `gh-<n>-…` branch and a `Closes #<n>` PR reach the upstream item the same
    way the branch binding and the backlog collapse do.
    """
    if not ctx.observed.has_evidence_tables():
        return GitSignals()
    open_refs: set[str] = set()
    open_numbers: set[int] = set()
    for obs in ctx.observed.latest(
        kinds=("forge.pull_request",), project_id=project_id, limit=1000
    ):
        state = derive_pr_state(
            _str(obs.payload.get("state")),
            draft=bool(obs.payload.get("draft")),
            review_decision=_str(obs.payload.get("review_state")),
        )
        if state not in ("open", "draft", "in-review"):
            continue
        implements = obs.payload.get("implements")
        if not isinstance(implements, list):
            continue
        for edge in implements:
            if not isinstance(edge, dict):
                continue
            subject = edge.get("subject")
            if isinstance(subject, str):
                open_refs.add(subject)
            number = edge.get("number")
            if isinstance(number, int) and not isinstance(number, bool):
                open_numbers.add(number)

    by_ref: dict[str, int] = {}
    by_number: dict[int, int] = {}
    for obs in ctx.observed.latest(
        kinds=("git.branch",), project_id=project_id, limit=1000
    ):
        age = _age(_parse(obs.payload.get("last_commit_at")), now)
        if age is None:
            continue
        work_ref = obs.payload.get("work_item_ref")
        if isinstance(work_ref, str):
            by_ref[work_ref] = min(age, by_ref.get(work_ref, age))
        number = obs.payload.get("forge_number")
        if isinstance(number, int) and not isinstance(number, bool):
            by_number[number] = min(age, by_number.get(number, age))
    return GitSignals(
        open_pr_refs=frozenset(open_refs),
        open_pr_numbers=frozenset(open_numbers),
        branch_age_by_ref=by_ref,
        branch_age_by_number=by_number,
    )


def git_story_for(
    ctx: AppContext,
    view: ReadView,
    item: WorkItem,
    branches: list[WorkItemBranchView],
) -> WorkItemGitStory | None:
    """The derived git story for `item`, or `None` when there is no evidence.

    Reuses the branch views already assembled for the item (#283) rather than
    re-reading them, finds the pull request that implements it (#284), derives
    the phase and the drift, and stamps each fact with its freshness.
    """
    del view  # kept in the signature for symmetry with the other assemblers
    now = ctx.clock()
    pull = _pull_request_for(ctx, item, now)
    has_branch = len(branches) > 0
    has_active_branch = any(b.source in ("observed", "both") for b in branches)
    pr_state: PrState | None = pull.state if pull is not None else None

    # No git evidence at all is not a story: an item nobody has started in git
    # gets no block rather than a `no_branch` one nobody asked for.
    if not has_branch and pull is None:
        return None

    phase = derive_phase(has_branch=has_branch, pr_state=pr_state)
    item_terminal = item.state in TERMINAL_STATES
    drift = [
        GitStoryDriftView(
            code=code,
            message=message,
            provenance=_drift_provenance(code, pull),
        )
        for code, message in derive_drift(
            item_terminal=item_terminal,
            pr_state=pr_state,
            has_active_branch=has_active_branch,
        )
    ]
    return WorkItemGitStory(
        phase=phase,
        workflow_state=item.state,
        branches=branches,
        pull_request=pull,
        drift=drift,
        # #291's task-run conclusion is an engine-side record not surfaced
        # through the work item, so it does not feed the phase yet.
        task_conclusion_available=False,
    )


def _pull_request_for(
    ctx: AppContext, item: WorkItem, now: datetime
) -> WorkItemPullRequestView | None:
    """The pull request that implements `item`, if one was observed (#284).

    A PR names its target the way the backlog collapse reads it: the observed
    `implements` edges carry the subject keys, keyed the same way the item's
    ref is. A same-repo `gh-<n>-…` branch reaches the upstream item whose
    subject ends `#<n>` through the item's own issue number, exactly as the
    branch binding does. When several PRs name the item, the defining one wins.
    """
    if item.project_id is None or not ctx.observed.has_evidence_tables():
        return None
    forge_number = _forge_number(item.ref)
    best: WorkItemPullRequestView | None = None
    best_key: tuple[int, datetime] | None = None
    for obs in ctx.observed.latest(
        kinds=("forge.pull_request",), project_id=item.project_id, limit=1000
    ):
        provenance = _edge_provenance(obs, item.ref, forge_number)
        if provenance is None:
            continue
        view = _pull_view(obs, provenance=provenance, now=now)
        key = (
            _STATE_RANK.get(view.state, 0),
            view.observed_at or datetime.min.replace(tzinfo=now.tzinfo),
        )
        if best_key is None or key > best_key:
            best, best_key = view, key
    return best


def _edge_provenance(
    obs: Observation, work_ref: str, forge_number: int | None
) -> str | None:
    """Whether — and by what provenance — this PR implements `work_ref`.

    Reads the observed `implements` edges (#284): a subject-key match binds
    directly, and a same-repo edge whose number is the item's own issue number
    binds a `gh-<n>` branch's PR to the upstream item. `None` when the PR names
    other work, never a guess.
    """
    implements = obs.payload.get("implements")
    if not isinstance(implements, list):
        return None
    for edge in implements:
        if not isinstance(edge, dict):
            continue
        if edge.get("subject") == work_ref:
            return _str(edge.get("provenance"))
        if forge_number is not None and edge.get("number") == forge_number:
            return _str(edge.get("provenance"))
    return None


def _pull_view(
    obs: Observation, *, provenance: str | None, now: datetime
) -> WorkItemPullRequestView:
    payload = obs.payload
    review_decision = _str(payload.get("review_state"))
    state = derive_pr_state(
        _str(payload.get("state")),
        draft=bool(payload.get("draft")),
        review_decision=review_decision,
    )
    updated_at = _parse(payload.get("updated_at"))
    return WorkItemPullRequestView(
        number=_int(payload.get("number")) or 0,
        state=state,
        title=_str(payload.get("title")),
        url=obs.source_url,
        draft=bool(payload.get("draft")),
        review_decision=review_decision,
        checks=_str(payload.get("checks")),
        mergeable=_str(payload.get("mergeable")),
        head_ref=_str(payload.get("head_ref")),
        base=_str(payload.get("base")),
        provenance=provenance,
        updated_at=updated_at,
        updated_age_seconds=_age(updated_at, now),
        observed_at=obs.observed_at,
        observed_age_seconds=_age(obs.observed_at, now),
    )


def _drift_provenance(code: str, pull: WorkItemPullRequestView | None) -> str | None:
    """Where a drift finding was read from, for a reader to check it against."""
    if code == "active_branch_done_item":
        return "local checkout (git-local sweep)"
    if pull is not None:
        return f"forge PR #{pull.number}"
    return None


def _age(moment: datetime | None, now: datetime) -> int | None:
    if moment is None or moment.tzinfo is None or now.tzinfo is None:
        return None
    return max(0, int((now - moment).total_seconds()))


def _parse(value: object) -> datetime | None:
    if not isinstance(value, str) or not value:
        return None
    try:
        return datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None


def _str(value: object) -> str | None:
    return value if isinstance(value, str) and value else None


def _int(value: object) -> int | None:
    return value if isinstance(value, int) and not isinstance(value, bool) else None


def _forge_number(ref: str) -> int | None:
    match = _FORGE_NUMBER.search(ref)
    return int(match.group(1)) if match is not None else None


__all__ = ["GitSignals", "build_git_signals", "git_story_for"]
