"""Observed-first: what shows up, what does not, and why (FR-W4, W5, W10, W11)."""

from __future__ import annotations

from pathlib import Path

import pytest

from vogt.application.context import AppContext
from vogt.application.models import (
    AdoptParams,
    BacklogParams,
    BugsParams,
    CreateWorkParams,
    ListSuppressionsParams,
    ObservationsParams,
    PruneParams,
    RegisterProjectParams,
    RevokeSuppressionParams,
    SuppressParams,
    SweepParams,
    WhyParams,
)
from vogt.application.services import (
    adopt,
    backlog,
    bugs,
    create_work,
    list_suppressions,
    observations,
    prune,
    register_project,
    revoke_suppression,
    suppress,
    sweep,
    why,
)

WHY = "observed-first test"


@pytest.fixture
def swept(instance: AppContext, tmp_path: Path) -> AppContext:
    """An instance with one registered project that has been collected."""
    project = tmp_path / "fixture"
    project.mkdir()
    (project / "notes.md").write_text(
        "TODO(vogt): promoted chore\n"
        "FIXME(vogt): promoted defect\n"
        "TODO: unpromoted note\n",
        encoding="utf-8",
    )
    register_project(
        instance,
        RegisterProjectParams(name="Fixture", root_path=str(project), reason=WHY),
    )
    sweep(instance, SweepParams(offline_only=True, reason=WHY))
    return instance


def _refs(result: object) -> list[str]:
    return [entry.ref for entry in result.items]  # type: ignore[attr-defined]


# -- what appears ----------------------------------------------------------


def test_promoted_markers_appear_without_being_declared(swept: AppContext) -> None:
    """FR-W4: collected work is visible immediately."""
    ranked = backlog(swept, BacklogParams(limit=50))
    observed = [entry for entry in ranked.items if entry.origin == "observed"]
    assert len(observed) == 2, "both promoted markers, and only those"
    assert all(entry.state == "observed" for entry in observed)


def test_unpromoted_markers_stay_out_of_ranked_views(swept: AppContext) -> None:
    """FR-W11: still observed, still queryable — it just is not work."""
    ranked = backlog(swept, BacklogParams(limit=50))
    assert not any("unpromoted" in entry.title for entry in ranked.items)

    everything = observations(swept, ObservationsParams(kind="marker", limit=50))
    assert any(
        "unpromoted" in str(o.payload.get("text", "")) for o in everything.observations
    )


def test_a_fixme_reads_as_a_bug_and_a_todo_does_not(swept: AppContext) -> None:
    found = bugs(swept, BugsParams(limit=50))
    titles = [entry.title for entry in found.items]
    assert any("FIXME" in title for title in titles)
    assert not any("TODO" in title for title in titles)


def test_declared_and_observed_are_ranked_together(swept: AppContext) -> None:
    """One list, one set of weights — otherwise the ordering means nothing."""
    create_work(
        swept,
        CreateWorkParams(
            kind="bug", title="Declared and urgent", priority="p0", reason=WHY
        ),
    )
    ranked = backlog(swept, BacklogParams(limit=50))
    assert ranked.items[0].origin == "declared", "p0 outranks a p3 marker"
    assert {entry.origin for entry in ranked.items} == {"declared", "observed"}
    assert ranked.declared == 1
    assert ranked.observed == 2


def test_observed_entries_carry_trust_and_the_answer_carries_freshness(
    swept: AppContext,
) -> None:
    """FR-R4 and FR-V4 on the same answer."""
    ranked = backlog(swept, BacklogParams(limit=50))
    observed = next(e for e in ranked.items if e.origin == "observed")
    assert observed.trust_state == "verified", "just swept, so within the horizon"
    assert ranked.freshness.status == "fresh"
    assert ranked.freshness.oldest_relevant_sweep is not None
    assert set(ranked.freshness.collectors) >= {"source-markers"}


def test_why_explains_an_observed_subject_too(swept: AppContext) -> None:
    ranked = backlog(swept, BacklogParams(limit=50))
    observed = next(e for e in ranked.items if e.origin == "observed")
    explanation = why(swept, WhyParams(ref=observed.ref))
    # Staleness is a function of wall-clock age, so two calls a moment
    # apart differ in the fourth decimal place. The tolerance is about
    # the clock, not about the score being approximate.
    assert explanation.total == pytest.approx(observed.score, abs=0.01)
    assert next(c.input for c in explanation.contributions) == "priority"


def test_freshness_says_never_swept_before_any_sweep(instance: AppContext) -> None:
    assert backlog(instance, BacklogParams()).freshness.status == "never_swept"


# -- suppression -----------------------------------------------------------


def test_suppressing_removes_from_views_but_not_from_evidence(
    swept: AppContext,
) -> None:
    """FR-W10: the decision hides a subject; it does not delete evidence."""
    ranked = backlog(swept, BacklogParams(limit=50))
    target = next(e for e in ranked.items if e.origin == "observed")

    suppress(swept, SuppressParams(subject=target.ref, reason="known and accepted"))

    after = backlog(swept, BacklogParams(limit=50))
    assert target.ref not in _refs(after)
    assert after.suppressed == 1

    still_there = observations(
        swept, ObservationsParams(subject_key=target.ref, latest_only=False)
    )
    assert still_there.observations, "the evidence is still queryable"


def test_a_suppression_survives_re_observation(swept: AppContext) -> None:
    """The whole reason suppression lives in the declared store."""
    target = next(
        e
        for e in backlog(swept, BacklogParams(limit=50)).items
        if e.origin == "observed"
    )
    suppress(swept, SuppressParams(subject=target.ref, reason="noise"))
    sweep(swept, SweepParams(offline_only=True, reason="look again"))

    assert target.ref not in _refs(backlog(swept, BacklogParams(limit=50)))


def test_a_pattern_suppresses_a_whole_family(swept: AppContext) -> None:
    suppress(
        swept,
        SuppressParams(
            subject="mark:fixture/notes.md*", pattern=True, reason="that file is noise"
        ),
    )
    ranked = backlog(swept, BacklogParams(limit=50))
    assert not [e for e in ranked.items if e.origin == "observed"]
    assert ranked.suppressed == 2


def test_suppressing_is_audited_with_its_reason(swept: AppContext) -> None:
    suppress(swept, SuppressParams(subject="mark:x", reason="because I said so"))
    with swept.declared.read() as view:
        record = view.list_audit(limit=1)[0]
    assert record.operation == "suppress"
    assert record.reason == "because I said so"


def test_a_revoked_suppression_returns_the_subject(swept: AppContext) -> None:
    target = next(
        e
        for e in backlog(swept, BacklogParams(limit=50)).items
        if e.origin == "observed"
    )
    created = suppress(swept, SuppressParams(subject=target.ref, reason="noise"))
    assert target.ref not in _refs(backlog(swept, BacklogParams(limit=50)))

    revoke_suppression(
        swept,
        RevokeSuppressionParams(id=created.suppression.id, reason="it was real"),
    )
    assert target.ref in _refs(backlog(swept, BacklogParams(limit=50)))

    listed = list_suppressions(swept, ListSuppressionsParams(include_revoked=True))
    assert listed.suppressions[0].revoked_at is not None
    assert list_suppressions(swept, ListSuppressionsParams()).suppressions == []


def test_suppressing_the_same_subject_twice_is_a_conflict(swept: AppContext) -> None:
    from vogt.errors import Conflict

    suppress(swept, SuppressParams(subject="mark:x", reason="once"))
    with pytest.raises(Conflict, match="already suppressed"):
        suppress(swept, SuppressParams(subject="mark:x", reason="twice"))


# -- adoption --------------------------------------------------------------


def test_adopting_creates_declared_work_linked_to_its_origin(
    swept: AppContext,
) -> None:
    """FR-W5: promotion with a maintained link back to the evidence."""
    target = next(
        e
        for e in backlog(swept, BacklogParams(limit=50)).items
        if e.origin == "observed" and "FIXME" in e.title
    )
    result = adopt(swept, AdoptParams(subject=target.ref, reason="taking this on"))

    assert result.item.origin == "adopted"
    assert result.inferred_kind == "bug"
    assert result.subject_key in result.item.body
    with swept.declared.read() as view:
        assert view.work_item_by_subject(target.ref) is not None


def test_an_adopted_subject_is_not_listed_twice(swept: AppContext) -> None:
    """Adoption makes a subject real; it does not give it a duplicate."""
    before = backlog(swept, BacklogParams(limit=50))
    target = next(e for e in before.items if e.origin == "observed")
    adopt(swept, AdoptParams(subject=target.ref, reason="mine now"))

    after = backlog(swept, BacklogParams(limit=50))
    assert len(after.items) == len(before.items)
    assert target.ref not in _refs(after)
    assert after.declared == 1


def test_adoption_overrides_the_inferred_guess(swept: AppContext) -> None:
    target = next(
        e
        for e in backlog(swept, BacklogParams(limit=50)).items
        if e.origin == "observed" and "TODO" in e.title
    )
    result = adopt(
        swept,
        AdoptParams(subject=target.ref, kind="feature", priority="p0", reason="mine"),
    )
    assert result.inferred_kind == "chore", "what Vogt guessed is still reported"
    assert result.item.kind == "feature", "what the human said is what is stored"
    assert result.item.priority == "p0"


def test_adopting_twice_is_a_conflict(swept: AppContext) -> None:
    from vogt.errors import Conflict

    target = next(
        e
        for e in backlog(swept, BacklogParams(limit=50)).items
        if e.origin == "observed"
    )
    adopt(swept, AdoptParams(subject=target.ref, reason="first"))
    with pytest.raises(Conflict, match="already adopted"):
        adopt(swept, AdoptParams(subject=target.ref, reason="second"))


def test_adopting_something_unobserved_says_so(swept: AppContext) -> None:
    from vogt.errors import NotFound

    with pytest.raises(NotFound, match="no observed subject"):
        adopt(swept, AdoptParams(subject="mark:nope#L1", reason="try"))


# -- retention -------------------------------------------------------------


def test_retention_keeps_the_newest_observation_however_old(
    swept: AppContext,
) -> None:
    """NFR-I5 rule 1: digest dedup means the newest row can be very old."""
    before = swept.observed.counts()["observations"]
    result = prune(swept, PruneParams(reason="routine"))

    assert result.removed == 0
    assert result.kept_latest >= 0
    assert swept.observed.counts()["observations"] == before


def test_pruning_publishes_an_event_and_rebuilds_the_projection(
    swept: AppContext,
) -> None:
    prune(swept, PruneParams(reason="routine"))
    with swept.declared.read() as view:
        kinds = [event.kind for event in view.list_events(after=0, limit=50)]
    assert "observations.pruned" in kinds
    assert swept.observed.counts()["subjects"] > 0


def test_pruning_is_audited_with_the_reason_it_was_given(swept: AppContext) -> None:
    """FR-S1: the one effect that cannot be re-derived by running it again.

    A sweep deliberately writes no audit row — it runs every fifteen minutes on
    a schedule with nobody to name. Deletion is the opposite case on both
    counts, and until r14 it was treated the same way: the reason went into an
    event summary and never reached `audit list`.
    """
    why = "Retention pass before the quarterly backup."
    prune(swept, PruneParams(reason=why))

    with swept.declared.read() as view:
        rows = [
            record
            for record in view.list_audit(limit=100)
            if record.operation == "observations.prune"
        ]
    assert [record.reason for record in rows] == [why]
