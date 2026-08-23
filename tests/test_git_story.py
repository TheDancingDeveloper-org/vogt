"""The derived git story on a work item — branch / PR / phase + drift (#285).

With branches observed (#283) and the PR edge observed (#284), a work item can
answer "where is this in git?" without anything new being written. This suite
covers that derivation end to end:

- the **phase** ladder (no-branch → active → PR open → in review → merged),
  each transition and the closed-PR fallback, as a pure function;
- the **PR view** read from the observation, carrying provenance and freshness;
- the **`why` inputs** — an open PR and a recently-committed branch — changing
  the score the way the weights promise;
- the **drift** contradictions surfaced (and the agreeing cases left alone).

Everything asserted here is derived and read-only: no test writes a phase, and
the workflow state is never touched by it.
"""

from __future__ import annotations

from datetime import UTC, datetime, timedelta
from pathlib import Path

import pytest

from vogt.application.context import AppContext
from vogt.application.models import (
    GetWorkParams,
    RegisterProjectParams,
    WhyParams,
)
from vogt.application.services import (
    get_work,
    register_project,
    why,
)
from vogt.core.git_story import (
    DRIFT_ACTIVE_BRANCH_DONE_ITEM,
    DRIFT_CLOSED_ITEM_OPEN_PR,
    DRIFT_MERGED_PR_OPEN_ITEM,
    derive_drift,
    derive_phase,
    derive_pr_state,
)
from vogt.core.observed import Rankable
from vogt.core.ranking import (
    BRANCH_ACTIVITY_POINTS,
    OPEN_PR_POINTS,
    RankingInputs,
    score_item,
)
from vogt.storage.observed_types import PendingObservation

from tests.conftest import native_work_item

WHY = "git-story test"


# -- the phase ladder, pure (test gate: each transition) -------------------


def test_no_evidence_is_no_branch() -> None:
    assert derive_phase(has_branch=False, pr_state=None) == "no_branch"


def test_a_bound_branch_with_no_pr_is_branch_active() -> None:
    assert derive_phase(has_branch=True, pr_state=None) == "branch_active"


@pytest.mark.parametrize("state", ["open", "draft"])
def test_an_open_pr_is_pr_open(state: str) -> None:
    assert derive_phase(has_branch=True, pr_state=state) == "pr_open"  # type: ignore[arg-type]


def test_a_reviewed_pr_is_in_review() -> None:
    assert derive_phase(has_branch=True, pr_state="in-review") == "in_review"


def test_a_merged_pr_is_merged_even_without_a_live_branch() -> None:
    assert derive_phase(has_branch=False, pr_state="merged") == "merged"


def test_a_closed_unmerged_pr_falls_back_to_the_branch() -> None:
    # A dead PR is not a phase of its own: the item reverts to what the branch
    # says, active if one is still bound and no-branch otherwise.
    assert derive_phase(has_branch=True, pr_state="closed") == "branch_active"
    assert derive_phase(has_branch=False, pr_state="closed") == "no_branch"


# -- deriving the PR state from the observation's fields -------------------


def test_pr_state_reads_merged_closed_draft_review_and_open() -> None:
    assert derive_pr_state("merged", draft=False, review_decision=None) == "merged"
    assert derive_pr_state("closed", draft=False, review_decision=None) == "closed"
    assert derive_pr_state("open", draft=True, review_decision=None) == "draft"
    assert (
        derive_pr_state("open", draft=False, review_decision="approved") == "in-review"
    )
    assert (
        derive_pr_state("open", draft=False, review_decision="changes_requested")
        == "in-review"
    )
    # A required-but-not-started review is not a started one.
    assert (
        derive_pr_state("open", draft=False, review_decision="review_required")
        == "open"
    )
    assert derive_pr_state("open", draft=False, review_decision=None) == "open"


# -- drift, pure (test gate: each contradiction, and the agreeing cases) ---


def test_closed_item_with_an_open_pr_is_drift() -> None:
    codes = _codes(
        derive_drift(item_terminal=True, pr_state="open", has_active_branch=False)
    )
    assert codes == [DRIFT_CLOSED_ITEM_OPEN_PR]


def test_merged_pr_under_an_open_item_is_drift() -> None:
    codes = _codes(
        derive_drift(item_terminal=False, pr_state="merged", has_active_branch=False)
    )
    assert codes == [DRIFT_MERGED_PR_OPEN_ITEM]


def test_an_active_branch_on_a_done_item_is_drift() -> None:
    codes = _codes(
        derive_drift(item_terminal=True, pr_state=None, has_active_branch=True)
    )
    assert codes == [DRIFT_ACTIVE_BRANCH_DONE_ITEM]


def test_the_agreeing_cases_are_not_drift() -> None:
    # Open item, open PR — the normal in-flight case.
    assert (
        derive_drift(item_terminal=False, pr_state="open", has_active_branch=True) == []
    )
    # Done item, merged PR, no live branch — the normal finished case.
    assert (
        derive_drift(item_terminal=True, pr_state="merged", has_active_branch=False)
        == []
    )
    # Nothing at all.
    assert (
        derive_drift(item_terminal=False, pr_state=None, has_active_branch=False) == []
    )


def _codes(findings: list[tuple[str, str]]) -> list[str]:
    return [code for code, _ in findings]


# -- the why inputs change the ranking (test gate) -------------------------


def _rankable() -> Rankable:
    return Rankable(
        id="WI-1",
        ref="WI-1",
        priority="p2",
        updated_at=datetime(2026, 8, 20, tzinfo=UTC),
        trust_state="verified",
        state="in_progress",
    )


def test_an_open_pr_raises_the_score() -> None:
    now = datetime(2026, 8, 22, tzinfo=UTC)
    base = score_item(_rankable(), RankingInputs(now=now)).total
    moved = score_item(_rankable(), RankingInputs(now=now, open_pr=True)).total
    assert moved == pytest.approx(base + OPEN_PR_POINTS)


def test_a_freshly_active_branch_raises_the_score_more_than_a_stale_one() -> None:
    now = datetime(2026, 8, 22, tzinfo=UTC)
    base = score_item(_rankable(), RankingInputs(now=now)).total
    fresh = score_item(
        _rankable(), RankingInputs(now=now, branch_activity_seconds=0)
    ).total
    week = score_item(
        _rankable(),
        RankingInputs(now=now, branch_activity_seconds=7 * 86_400),
    ).total
    outside = score_item(
        _rankable(),
        RankingInputs(now=now, branch_activity_seconds=30 * 86_400),
    ).total
    assert fresh == pytest.approx(base + BRANCH_ACTIVITY_POINTS)
    assert base < week < fresh
    # Past the window the branch contributes nothing.
    assert outside == pytest.approx(base)


def test_the_why_explanation_names_the_new_inputs(
    instance: AppContext,
    tmp_path: Path,
) -> None:
    project_id = _project(instance, tmp_path)
    item = native_work_item(instance, title="Moving", project="proj")
    _seed_branch(instance, project_id, "wi-1", work_ref=item.ref, age_days=0.1)
    _seed_pr(instance, project_id, number=5, implements=item.ref, state="open")

    result = why(instance, WhyParams(ref=item.ref))
    by_input = {c.input: c for c in result.contributions}
    assert by_input["open_pr"].contribution == pytest.approx(OPEN_PR_POINTS)
    assert by_input["branch_activity"].contribution > 0
    # A comparable item with no git activity scores lower.
    idle = native_work_item(instance, title="Idle", project="proj")
    idle_score = why(instance, WhyParams(ref=idle.ref)).total
    assert result.total > idle_score


# -- the git block on work.get, with provenance and age --------------------


def test_work_get_carries_the_git_block_with_phase_pr_and_freshness(
    instance: AppContext,
    tmp_path: Path,
) -> None:
    project_id = _project(instance, tmp_path)
    item = native_work_item(instance, title="In flight", project="proj")
    _seed_branch(instance, project_id, "wi-1", work_ref=item.ref, age_days=0.05)
    _seed_pr(
        instance,
        project_id,
        number=7,
        implements=item.ref,
        state="open",
        checks="success",
        url="https://github.com/acme/app/pull/7",
    )

    git = get_work(instance, GetWorkParams(ref=item.ref)).git
    assert git is not None
    assert git.phase == "pr_open"
    assert git.workflow_state == item.state
    # The branch is present with a derived age (provenance + freshness).
    assert git.branches and git.branches[0].last_commit_age_seconds is not None
    # The PR view carries its state, checks, provenance and freshness.
    pr = git.pull_request
    assert pr is not None
    assert pr.number == 7
    assert pr.state == "open"
    assert pr.checks == "success"
    assert pr.url == "https://github.com/acme/app/pull/7"
    assert pr.provenance == "from PR body"
    assert pr.observed_age_seconds is not None
    # #291's task conclusion is a seam, not folded in.
    assert git.task_conclusion_available is False


def test_a_reviewed_pr_reads_in_review_phase(
    instance: AppContext,
    tmp_path: Path,
) -> None:
    project_id = _project(instance, tmp_path)
    item = native_work_item(instance, title="Under review", project="proj")
    _seed_pr(
        instance,
        project_id,
        number=8,
        implements=item.ref,
        state="open",
        review_state="changes_requested",
    )
    git = get_work(instance, GetWorkParams(ref=item.ref)).git
    assert git is not None and git.phase == "in_review"
    assert git.pull_request is not None and git.pull_request.state == "in-review"


def test_an_item_with_no_git_evidence_has_no_git_block(
    instance: AppContext,
    tmp_path: Path,
) -> None:
    _project(instance, tmp_path)
    item = native_work_item(instance, title="Nothing yet", project="proj")
    assert get_work(instance, GetWorkParams(ref=item.ref)).git is None


# -- the drift contradictions, end to end ----------------------------------


def test_a_merged_pr_under_an_open_item_surfaces_as_git_drift(
    instance: AppContext,
    tmp_path: Path,
) -> None:
    project_id = _project(instance, tmp_path)
    item = native_work_item(instance, title="Shipped but open", project="proj")
    _seed_pr(instance, project_id, number=9, implements=item.ref, state="merged")

    git = get_work(instance, GetWorkParams(ref=item.ref)).git
    assert git is not None and git.phase == "merged"
    assert [d.code for d in git.drift] == [DRIFT_MERGED_PR_OPEN_ITEM]
    assert git.drift[0].provenance == "forge PR #9"


def test_a_closed_item_with_an_open_pr_surfaces_as_git_drift(
    instance: AppContext,
    tmp_path: Path,
) -> None:
    project_id = _project(instance, tmp_path)
    item = native_work_item(
        instance, title="Closed too early", project="proj", state="done"
    )
    _seed_branch(instance, project_id, "wi-1", work_ref=item.ref, age_days=0.1)
    _seed_pr(instance, project_id, number=10, implements=item.ref, state="open")

    git = get_work(instance, GetWorkParams(ref=item.ref)).git
    assert git is not None
    codes = {d.code for d in git.drift}
    # Both the open-PR-on-a-closed-item and the active-branch-on-a-done-item
    # contradictions fire here.
    assert DRIFT_CLOSED_ITEM_OPEN_PR in codes
    assert DRIFT_ACTIVE_BRANCH_DONE_ITEM in codes


def test_an_open_item_with_an_open_pr_has_no_drift(
    instance: AppContext,
    tmp_path: Path,
) -> None:
    project_id = _project(instance, tmp_path)
    item = native_work_item(instance, title="Healthy in-flight", project="proj")
    _seed_branch(instance, project_id, "wi-1", work_ref=item.ref, age_days=0.1)
    _seed_pr(instance, project_id, number=11, implements=item.ref, state="open")
    git = get_work(instance, GetWorkParams(ref=item.ref)).git
    assert git is not None and git.drift == []


# -- helpers ---------------------------------------------------------------


def _project(ctx: AppContext, tmp_path: Path) -> str:
    root = tmp_path / "proj"
    root.mkdir()
    register_project(
        ctx, RegisterProjectParams(name="Proj", root_path=str(root), reason=WHY)
    )
    with ctx.declared.read() as view:
        project = view.project_by_slug("proj")
    assert project is not None
    return project.id


def _append(ctx: AppContext, project_id: str, obs: PendingObservation) -> None:
    now = ctx.clock()
    sweep = ctx.observed.begin_sweep(collector="test", scope=[project_id], at=now)
    ctx.observed.append(sweep.id, [obs], at=now)
    ctx.observed.finish_sweep(
        sweep.id, outcome="ok", stats={"projects": 1, "new": 1, "unchanged": 0}, at=now
    )
    ctx.observed.rebuild_latest()


def _seed_branch(
    ctx: AppContext,
    project_id: str,
    name: str,
    *,
    work_ref: str,
    age_days: float,
) -> None:
    last_commit = ctx.clock() - timedelta(days=age_days)
    _append(
        ctx,
        project_id,
        PendingObservation(
            kind="git.branch",
            subject_key=f"branch:{project_id}:{name}",
            payload={
                "name": name,
                "work_item_ref": work_ref,
                "forge_number": None,
                "tip": "deadbeef",
                "ahead": 1,
                "behind": 0,
                "default_branch": "main",
                "last_commit_at": last_commit.isoformat(),
            },
            content_digest=f"branch-{name}-{age_days}",
            project_id=project_id,
        ),
    )


def _seed_pr(
    ctx: AppContext,
    project_id: str,
    *,
    number: int,
    implements: str,
    state: str,
    review_state: str | None = None,
    checks: str | None = None,
    url: str | None = None,
) -> None:
    _append(
        ctx,
        project_id,
        PendingObservation(
            kind="forge.pull_request",
            subject_key=f"gh:acme/app#{number}",
            payload={
                "number": number,
                "title": f"PR {number}",
                "state": state,
                "draft": False,
                "head_ref": "wi-1",
                "base": "main",
                "review_state": review_state,
                "mergeable": None,
                "checks": checks,
                "implements": [
                    {
                        "subject": implements,
                        "number": number,
                        "provenance": "from PR body",
                    }
                ],
                "updated_at": ctx.clock().isoformat(),
            },
            content_digest=f"pr-{number}-{state}",
            project_id=project_id,
            source_url=url,
        ),
    )
