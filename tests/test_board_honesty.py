"""The Board is honest about what it is not showing (#187).

`list_board` reads declared work only, so a Board with two cards beside a
Backlog of many silently read as "the estate has two things". These tests pin
the interim fix: the Board result carries the same candidate population the
Backlog considers, so a surface can say how much it is leaving out rather than
letting the small number stand for the whole.
"""

from __future__ import annotations

from pathlib import Path

import pytest

from vogt.application.context import AppContext
from vogt.application.models import (
    BacklogParams,
    BoardCellParams,
    BoardListParams,
    CreateWorkParams,
    RegisterProjectParams,
    SweepParams,
)
from vogt.application.services import (
    backlog,
    create_work,
    list_board,
    register_project,
    sweep,
)

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
    """One declared item, and two observed markers nobody has adopted."""
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
    create_work(
        instance,
        CreateWorkParams(
            kind="feature", title="declared one", project="fixture", reason=WHY
        ),
    )
    sweep(instance, SweepParams(offline_only=True, reason=WHY))
    return instance


def test_board_reports_the_candidate_population(estate: AppContext) -> None:
    result = list_board(estate, _board(estate))

    # One card on the Board, three things in the outstanding population: the
    # declared feature plus the two promoted markers.
    assert result.total == 1
    assert result.declared_total == 1
    assert result.backlog_candidates == 3
    assert result.backlog_candidates > result.declared_total


def test_candidate_count_matches_the_backlog_for_the_same_scope(
    estate: AppContext,
) -> None:
    board = list_board(estate, _board(estate))
    ranked = backlog(estate, BacklogParams(limit=200))

    # The honesty number is the Backlog's own denominator, not a second idea
    # of one.
    assert board.backlog_candidates == ranked.total_considered
    assert board.declared_total == ranked.declared


def test_candidate_count_honours_the_project_filter(estate: AppContext) -> None:
    # A second project with no work of its own: its scoped Board considers
    # nothing, even though the global Board considers three things.
    register_project(
        estate,
        RegisterProjectParams(name="Empty", root_path="/srv/empty", reason=WHY),
    )
    scoped = list_board(estate, _board(estate, project="empty"))
    assert scoped.backlog_candidates == 0
    assert scoped.declared_total == 0

    glob = list_board(estate, _board(estate))
    assert glob.backlog_candidates == 3


def test_a_board_with_no_observations_is_candidate_equal(instance: AppContext) -> None:
    # No sweep has run, so the candidate population is exactly the declared
    # one and no honesty banner is warranted.
    register_project(
        instance,
        RegisterProjectParams(name="Bare", root_path="/srv/bare", reason=WHY),
    )
    create_work(
        instance,
        CreateWorkParams(kind="feature", title="only one", project="bare", reason=WHY),
    )
    result = list_board(instance, _board(instance))
    assert result.backlog_candidates == result.declared_total == 1
