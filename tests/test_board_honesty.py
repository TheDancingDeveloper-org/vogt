"""The Board is honest about what it is not showing (#187, repurposed by #183).

`list_board` draws declared cards plus linked projects' upstream items, so a
small Board beside a large Backlog silently read as "the estate has two
things". These tests pin the honesty numbers: the Board result carries the
same candidate population the Backlog considers, so a surface can say how
much it is leaving out rather than letting the small number stand for the
whole.

Repurposed for #183 (the forge-less guarantee withdrawal): these tests
originally asserted that a native declared item on an *unlinked* project
counted as a Board card and a backlog candidate — the forge-less work layer
being real. That guarantee is withdrawn: an unlinked project's native rows
are excluded from the curated surfaces (link or publish migrates them
upstream), and the honesty rule now covers the exclusion too — it must be
*counted* (`excluded_unlinked`), never silently dropped. The declared-vs-
candidates banner stays real: observed markers still outnumber declared
cards, and the denominators must still agree with the Backlog's own.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from vogt.application.context import AppContext
from vogt.application.models import (
    BacklogParams,
    BoardCellParams,
    BoardListParams,
    RegisterProjectParams,
    SweepParams,
)
from vogt.application.services import (
    backlog,
    list_board,
    register_project,
    sweep,
)

from tests.conftest import native_work_item

WHY = "board honesty test"


def _board(instance: AppContext, **over: object) -> BoardListParams:
    return BoardListParams(
        cells=[
            BoardCellParams(lane_key="", state="open"),
            BoardCellParams(lane_key="", state="in_progress"),
        ],
        **over,  # type: ignore[arg-type]
    )


@pytest.fixture
def estate(instance: AppContext, tmp_path: Path) -> AppContext:
    """One native item on an unlinked project, and two unadopted markers."""
    project = tmp_path / "fixture"
    project.mkdir()
    (project / "notes.md").write_text(
        "TODO(vogt): promoted chore\nFIXME(vogt): promoted defect\n",
        encoding="utf-8",
    )
    register_project(
        instance,
        RegisterProjectParams(name="Fixture", root_path=str(project), reason=WHY),
    )
    native_work_item(instance, kind="feature", title="declared one", project="fixture")
    sweep(instance, SweepParams(offline_only=True, reason=WHY))
    return instance


def test_board_reports_the_candidate_population(estate: AppContext) -> None:
    result = list_board(estate, _board(estate))

    # No cards on the Board: the one declared feature lives on an unlinked
    # project and is withdrawn from the surfaces (#183) — counted, not
    # dropped. The two promoted markers remain the outstanding population.
    assert result.total == 0
    assert result.declared_total == 0
    assert result.excluded_unlinked == 1
    assert result.backlog_candidates == 2
    assert result.backlog_candidates > result.declared_total


def test_candidate_count_matches_the_backlog_for_the_same_scope(
    estate: AppContext,
) -> None:
    board = list_board(estate, _board(estate))
    ranked = backlog(estate, BacklogParams(limit=200))

    # The honesty number is the Backlog's own denominator, not a second idea
    # of one — and since #183 that includes agreeing on what was excluded.
    assert board.backlog_candidates == ranked.total_considered
    assert board.declared_total == ranked.declared
    assert ranked.excluded_unlinked == 1


def test_candidate_count_honours_the_project_filter(estate: AppContext) -> None:
    # A second project with no work of its own. Unlinked, so its scoped Board
    # is the #183 CTA answer: empty cells and the machine-readable marker.
    register_project(
        estate,
        RegisterProjectParams(name="Empty", root_path="/srv/empty", reason=WHY),
    )
    scoped = list_board(estate, _board(estate, project="empty"))
    assert scoped.link_state == "unlinked"
    assert scoped.backlog_candidates == 0
    assert scoped.declared_total == 0
    assert all(cell.items == [] for cell in scoped.cells)

    glob = list_board(estate, _board(estate))
    assert glob.link_state is None, "the global Board is not a project scope"
    assert glob.backlog_candidates == 2


def test_a_board_with_no_observations_counts_its_exclusions(
    instance: AppContext,
) -> None:
    # Repurposed (#183): this used to pin "no observations means candidates
    # equal declared". The native item on the unlinked project is now
    # excluded from both sides of that equation — what must hold instead is
    # that the exclusion is visible, so a zero Board cannot pass for an
    # empty estate.
    register_project(
        instance,
        RegisterProjectParams(name="Bare", root_path="/srv/bare", reason=WHY),
    )
    native_work_item(instance, kind="feature", title="only one", project="bare")
    result = list_board(instance, _board(instance))
    assert result.backlog_candidates == result.declared_total == 0
    assert result.excluded_unlinked == 1
