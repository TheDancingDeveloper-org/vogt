"""Ranking is deterministic, documented, and explainable (FR-V2, FR-V3)."""

from __future__ import annotations

from datetime import UTC, datetime, timedelta

import pytest

from vogt.application.context import AppContext
from vogt.application.models import (
    BacklogParams,
    BugsParams,
    CreateInitiativeParams,
    CreateWorkParams,
    RelateWorkParams,
    TransitionWorkParams,
    UpdateWorkParams,
    WhyParams,
)
from vogt.application.services import (
    backlog,
    bugs,
    create_initiative,
    create_work,
    relate_work,
    transition_work,
    update_work,
    why,
)
from vogt.core.observed import Rankable
from vogt.core.ranking import (
    PRIORITY_POINTS,
    RankingInputs,
    rank,
    score_item,
)

WHY = "ranking test"
NOW = datetime(2026, 8, 12, 12, 0, 0, tzinfo=UTC)


def _item(**overrides: object) -> Rankable:
    """A rankable subject.

    The scorer takes `Rankable` rather than `WorkItem` so that declared work
    and observed subjects go through the same weights — which is the whole
    point of an observed-first backlog.
    """
    base: dict[str, object] = {
        "id": "wrk_1",
        "ref": "WI-1",
        "priority": "p2",
        "state": "open",
        "trust_state": "verified",
        "updated_at": NOW,
    }
    base.update(overrides)
    return Rankable(**base)  # type: ignore[arg-type]


# -- the scoring function --------------------------------------------------


def test_priority_dominates_a_fresh_item() -> None:
    high = score_item(_item(priority="p0"), RankingInputs(now=NOW))
    low = score_item(_item(priority="p4"), RankingInputs(now=NOW))
    assert high.total > low.total
    assert high.total - low.total == PRIORITY_POINTS["p0"] - PRIORITY_POINTS["p4"]


def test_staleness_lifts_an_ignored_item() -> None:
    fresh = score_item(_item(), RankingInputs(now=NOW))
    stale = score_item(
        _item(updated_at=NOW - timedelta(days=30)), RankingInputs(now=NOW)
    )
    assert stale.total > fresh.total


def test_staleness_is_capped_so_it_never_outranks_a_p0() -> None:
    ancient = score_item(
        _item(priority="p3", updated_at=NOW - timedelta(days=3650)),
        RankingInputs(now=NOW),
    )
    fresh_p0 = score_item(_item(priority="p0"), RankingInputs(now=NOW))
    assert fresh_p0.total > ancient.total


def test_blocking_other_work_lifts_an_item() -> None:
    alone = score_item(_item(), RankingInputs(now=NOW))
    blocking = score_item(_item(), RankingInputs(now=NOW, blocking_fan_out=3))
    assert blocking.total > alone.total


def test_initiative_weight_lifts_its_items() -> None:
    plain = score_item(_item(), RankingInputs(now=NOW))
    weighted = score_item(
        _item(has_initiative=True), RankingInputs(now=NOW, initiative_weight=100)
    )
    assert weighted.total > plain.total


def test_trust_only_ever_penalises() -> None:
    verified = score_item(_item(trust_state="verified"), RankingInputs(now=NOW))
    disputed = score_item(_item(trust_state="disputed"), RankingInputs(now=NOW))
    assert disputed.total < verified.total


def test_a_terminal_item_sinks_and_says_why() -> None:
    finished = score_item(_item(state="done"), RankingInputs(now=NOW, is_terminal=True))
    assert finished.total < 0
    assert any(entry.input == "terminal_state" for entry in finished.contributions)


def test_contributions_sum_to_the_total() -> None:
    score = score_item(
        _item(priority="p1", updated_at=NOW - timedelta(days=5)),
        RankingInputs(now=NOW, blocking_fan_out=2, initiative_weight=40),
    )
    assert score.total == pytest.approx(
        sum(entry.contribution for entry in score.contributions)
    )


def test_ordering_is_total_so_two_runs_never_disagree() -> None:
    first = _item(id="a", ref="WI-2")
    second = _item(id="b", ref="WI-1")
    scores = [
        score_item(first, RankingInputs(now=NOW)),
        score_item(second, RankingInputs(now=NOW)),
    ]
    assert [s.ref for s in rank(scores)] == ["WI-1", "WI-2"]
    assert [s.ref for s in rank(list(reversed(scores)))] == ["WI-1", "WI-2"]


# -- through the views -----------------------------------------------------


def test_the_backlog_ranks_by_priority(instance: AppContext) -> None:
    create_work(
        instance, CreateWorkParams(kind="chore", title="Low", priority="p4", reason=WHY)
    )
    create_work(
        instance,
        CreateWorkParams(kind="bug", title="Urgent", priority="p0", reason=WHY),
    )
    result = backlog(instance, BacklogParams())
    assert [entry.title for entry in result.items] == ["Urgent", "Low"]
    assert result.scope == "global"
    assert result.freshness.status == "never_swept", (
        "nothing has been swept, and the answer says so rather than "
        "implying the evidence is current"
    )


def test_finished_work_leaves_the_backlog(instance: AppContext) -> None:
    ref = create_work(
        instance, CreateWorkParams(kind="bug", title="Will finish", reason=WHY)
    ).item.ref
    for state in ("in_progress", "review", "done"):
        transition_work(
            instance, TransitionWorkParams(ref=ref, to_state=state, reason=WHY)
        )
    assert backlog(instance, BacklogParams()).items == []


def test_the_bug_view_shows_only_bugs(instance: AppContext) -> None:
    create_work(instance, CreateWorkParams(kind="bug", title="A bug", reason=WHY))
    create_work(
        instance, CreateWorkParams(kind="feature", title="A feature", reason=WHY)
    )
    result = bugs(instance, BugsParams())
    assert [entry.kind for entry in result.items] == ["bug"]


def test_an_initiative_lifts_its_work(instance: AppContext) -> None:
    create_initiative(
        instance,
        CreateInitiativeParams(title="Big Push", weight=100, reason=WHY),
    )
    create_work(
        instance, CreateWorkParams(kind="chore", title="Unattached", reason=WHY)
    )
    create_work(
        instance,
        CreateWorkParams(
            kind="chore", title="In the push", initiative="big-push", reason=WHY
        ),
    )
    result = backlog(instance, BacklogParams())
    assert result.items[0].title == "In the push"


def test_why_explains_every_input(instance: AppContext) -> None:
    ref = create_work(
        instance, CreateWorkParams(kind="bug", title="Explain me", reason=WHY)
    ).item.ref
    explanation = why(instance, WhyParams(ref=ref))

    assert explanation.ref == ref
    inputs = [entry.input for entry in explanation.contributions]
    assert inputs == [
        "priority",
        "staleness",
        "blocking_fan_out",
        "initiative_weight",
        "trust_penalty",
        "open_pr",
        "branch_activity",
    ]
    assert explanation.total == pytest.approx(
        sum(entry.contribution for entry in explanation.contributions)
    )
    assert "ci_red_boost" in explanation.inputs_not_yet_available


def test_why_matches_the_backlog_score(instance: AppContext) -> None:
    """The explanation has to be of the number the view actually used."""
    ref = create_work(
        instance, CreateWorkParams(kind="bug", title="Consistent", reason=WHY)
    ).item.ref
    ranked = backlog(instance, BacklogParams()).items[0]
    assert ranked.ref == ref
    # Staleness is a function of wall-clock age, so two calls a moment
    # apart differ in the fourth decimal place. The tolerance is about
    # the clock, not about the score being approximate.
    assert why(instance, WhyParams(ref=ref)).total == pytest.approx(
        ranked.score, abs=0.01
    )


def test_why_answers_for_finished_work_too(instance: AppContext) -> None:
    ref = create_work(
        instance, CreateWorkParams(kind="chore", title="Done thing", reason=WHY)
    ).item.ref
    transition_work(
        instance, TransitionWorkParams(ref=ref, to_state="wont_do", reason=WHY)
    )
    explanation = why(instance, WhyParams(ref=ref))
    assert any(
        entry.input == "terminal_state" for entry in explanation.contributions
    ), "asking why a finished item is absent must be answerable"


def test_blocking_fan_out_reaches_the_view(instance: AppContext) -> None:
    blocker = create_work(
        instance, CreateWorkParams(kind="chore", title="Blocker", reason=WHY)
    ).item.ref
    plain = create_work(
        instance, CreateWorkParams(kind="chore", title="Plain", reason=WHY)
    ).item.ref
    for _ in range(2):
        dependent = create_work(
            instance, CreateWorkParams(kind="chore", title="Waiting", reason=WHY)
        ).item.ref
        relate_work(
            instance,
            RelateWorkParams(
                ref=dependent, kind="depends_on", target=blocker, reason=WHY
            ),
        )
    scores = {
        entry.ref: entry.score for entry in backlog(instance, BacklogParams()).items
    }
    assert scores[blocker] > scores[plain]


def test_priority_changes_reorder_the_backlog(instance: AppContext) -> None:
    first = create_work(
        instance, CreateWorkParams(kind="chore", title="First", reason=WHY)
    ).item.ref
    second = create_work(
        instance, CreateWorkParams(kind="chore", title="Second", reason=WHY)
    ).item.ref
    update_work(instance, UpdateWorkParams(ref=second, priority="p0", reason=WHY))
    order = [entry.ref for entry in backlog(instance, BacklogParams()).items]
    assert order == [second, first]
