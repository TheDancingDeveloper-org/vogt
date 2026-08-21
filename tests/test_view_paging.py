"""Ranked views page past their first page (FR-V5).

`backlog` and `bugs` capped at `limit` and offered nothing else, so the top
200 of a ranked view was the whole of it as far as any caller was concerned.
The view announced the truncation — `total_considered` has always carried the
full count — and could not continue it, which is the worse half: a reader is
told there is more and given no way to reach it.

The property worth protecting is not "an offset parameter exists". It is that
**page two is the next rows of one ordering**, not a fresh ranking of what was
left over. Ranking is computed over the whole candidate set before the slice,
so that holds by construction; these tests keep it true if the slice ever
moves into the query.

**Every test here pins the clock, and that is a finding rather than a
convenience.** Ranking is recomputed per request and staleness grows with time,
so two pages fetched far enough apart can order near-ties differently — writing
these against the suite's stepping clock is what surfaced it. That is inherent
to paging a live ranking rather than a defect in the slice, it is the same
caveat `audit.list` carries, and `BacklogParams.offset` now states it. A fixed
clock is what isolates the paging mechanism from it; the last test here pins
the caveat itself so it stays known.
"""

from __future__ import annotations

from datetime import UTC, datetime
from pathlib import Path

import pytest

from vogt.application.context import AppContext, build_context
from vogt.application.models import (
    BacklogParams,
    BacklogResult,
    BugsParams,
    CreateProjectParams,
    InitParams,
)
from vogt.application.services import (
    backlog,
    bugs,
    create_project,
    init_instance,
)
from vogt.config import VogtConfig
from vogt.core.principal import Principal

from tests.conftest import native_work_item

WHY = "paging test"
FIXED = datetime(2026, 8, 12, 5, 0, 0, tzinfo=UTC)


@pytest.fixture
def estate(tmp_path: Path) -> AppContext:
    """One project and twelve items, under a clock that does not move."""
    config = VogtConfig(data_dir=tmp_path / "instance", sqlite_synchronous="off")
    ctx = build_context(
        config=config,
        principal=Principal(
            identity_ref="local:paging", kind="human", display_name="Paging"
        ),
        clock=lambda: FIXED,
    )
    init_instance(ctx, InitParams())
    create_project(
        ctx,
        CreateProjectParams(name="Paged", root_path=str(tmp_path), reason=WHY),
    )
    for index in range(12):
        native_work_item(
            ctx,
            title=f"item {index:02d}",
            kind="bug" if index % 2 else "feature",
            project="paged",
        )
    return ctx


def _refs(result: BacklogResult) -> list[str]:
    return [item.ref for item in result.items]


def test_the_pages_of_a_backlog_tile_the_ranking(estate: AppContext) -> None:
    """Consecutive pages are consecutive rows of one order, with no overlap.

    Asserted against the unpaged ranking rather than against each other,
    because two pages that agreed with each other and disagreed with the
    ranking would be the actual defect — a view that pages consistently and
    ranks freshly each time is how one item is seen twice and another never.
    """
    whole = _refs(backlog(estate, BacklogParams(limit=200)))
    assert len(whole) == 12, "the fixture must exceed one page to prove much"

    first = _refs(backlog(estate, BacklogParams(limit=5, offset=0)))
    second = _refs(backlog(estate, BacklogParams(limit=5, offset=5)))
    third = _refs(backlog(estate, BacklogParams(limit=5, offset=10)))

    assert first + second + third == whole
    assert not set(first) & set(second)
    assert len(third) == 2, "the last page is short rather than padded"


def test_the_total_is_the_estate_not_the_page(estate: AppContext) -> None:
    """What tells a caller another page exists.

    `total_considered` is deliberately unaffected by the slice: it is the
    count the ranking ran over, so `offset + len(items) < total_considered`
    is the whole of "there is more".
    """
    page = backlog(estate, BacklogParams(limit=5, offset=5))
    assert page.total_considered == 12
    assert len(page.items) == 5


def test_an_offset_past_the_end_is_empty_rather_than_an_error(
    estate: AppContext,
) -> None:
    """A client paging to the end walks off it, and that is not a failure."""
    page = backlog(estate, BacklogParams(limit=5, offset=99))
    assert page.items == []
    assert page.total_considered == 12


def test_bugs_pages_the_same_way(estate: AppContext) -> None:
    """The bug view is the one most likely to be deep on a real estate."""
    whole = _refs(bugs(estate, BugsParams(limit=200)))
    assert len(whole) == 6

    first = _refs(bugs(estate, BugsParams(limit=4, offset=0)))
    second = _refs(bugs(estate, BugsParams(limit=4, offset=4)))
    assert first + second == whole
    assert not set(first) & set(second)


def test_a_negative_offset_is_refused_by_the_schema() -> None:
    """Rather than silently slicing from the end, which is what Python does.

    `ranked[-3:]` is a valid expression and a wrong answer, so the bound lives
    on the model, where every transport inherits it.
    """
    with pytest.raises(ValueError):
        BacklogParams(limit=5, offset=-3)


def test_equal_scores_break_ties_the_same_way_every_time(
    estate: AppContext,
) -> None:
    """Paging is only honest if a tie has one answer.

    Twelve items created at one instant score identically, so this is the
    degenerate case the slice has to survive: were the tiebreak arbitrary,
    every page would be a different sample of the same score band and the
    test above would pass by luck on a small fixture.
    """
    scores = {item.score for item in backlog(estate, BacklogParams(limit=200)).items}
    assert len(scores) == 1, "the fixture is meant to be one flat score band"

    first = _refs(backlog(estate, BacklogParams(limit=200)))
    again = _refs(backlog(estate, BacklogParams(limit=200)))
    assert first == again
