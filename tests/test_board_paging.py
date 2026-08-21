"""Bounded, snapshot-stable Board pages (NFR-S5)."""

from __future__ import annotations

from datetime import UTC, datetime, timedelta

import pytest

from vogt.application.context import AppContext
from vogt.application.models import (
    BoardCellParams,
    BoardListParams,
    CreateWorkParams,
    RegisterProjectParams,
)
from vogt.application.services import create_work, list_board, register_project
from vogt.core.entities import WorkItem
from vogt.errors import InvalidCursor

from tests.conftest import native_work_item

WHY = "board paging test"


def _cell(
    state: str, *, lane_key: str = "", cursor: str | None = None
) -> BoardCellParams:
    return BoardCellParams(lane_key=lane_key, state=state, cursor=cursor)


def test_cells_page_independently_with_exact_totals(instance: AppContext) -> None:
    register_project(
        instance,
        RegisterProjectParams(name="Alpha", root_path="/srv/alpha", reason=WHY),
    )
    for index in range(7):
        native_work_item(
            instance, kind="feature", title=f"open {index}", project="alpha"
        )
    # Straight to `in_progress`: `work.transition` refuses on an unlinked
    # project since #181, and what this test pages is the Board SQL, not the
    # state machine.
    moving = native_work_item(
        instance,
        kind="feature",
        title="moving",
        project="alpha",
        state="in_progress",
    )

    first = list_board(
        instance,
        BoardListParams(
            lane_mode="project",
            cells=[
                _cell("open", lane_key="alpha"),
                _cell("in_progress", lane_key="alpha"),
            ],
            page_size=3,
        ),
    )
    opened, progressing = first.cells
    assert opened.total == 7
    assert len(opened.items) == 3
    assert opened.next_cursor is not None
    assert progressing.total == 1
    assert [item.ref for item in progressing.items] == [moving.ref]
    assert progressing.next_cursor is None
    assert first.column_totals == {"open": 7, "in_progress": 1}
    assert first.lane_totals == {"alpha": 8}
    assert first.total == 8

    second = list_board(
        instance,
        BoardListParams(
            lane_mode="project",
            cells=[_cell("open", lane_key="alpha", cursor=opened.next_cursor)],
            page_size=3,
            snapshot=first.snapshot,
        ),
    ).cells[0]
    third = list_board(
        instance,
        BoardListParams(
            lane_mode="project",
            cells=[_cell("open", lane_key="alpha", cursor=second.next_cursor)],
            page_size=3,
            snapshot=first.snapshot,
        ),
    ).cells[0]
    refs = [item.ref for item in opened.items + second.items + third.items]
    assert len(refs) == 7
    assert len(set(refs)) == 7
    assert second.total == third.total == 7
    assert third.next_cursor is None


def test_a_cursor_cannot_move_between_cells_or_filters(instance: AppContext) -> None:
    create_work(instance, CreateWorkParams(kind="feature", title="one", reason=WHY))
    first = list_board(
        instance,
        BoardListParams(cells=[_cell("open")], page_size=1),
    )
    # One row has no continuation, so construct a two-row snapshot.
    create_work(instance, CreateWorkParams(kind="feature", title="two", reason=WHY))
    first = list_board(
        instance,
        BoardListParams(cells=[_cell("open")], page_size=1),
    )
    cursor = first.cells[0].next_cursor
    assert cursor is not None

    with pytest.raises(InvalidCursor, match="another cell"):
        list_board(
            instance,
            BoardListParams(
                cells=[_cell("in_progress", cursor=cursor)],
                page_size=1,
                snapshot=first.snapshot,
            ),
        )
    with pytest.raises(InvalidCursor, match="another filter"):
        list_board(
            instance,
            BoardListParams(
                kinds=["bug"],
                cells=[_cell("open", cursor=cursor)],
                page_size=1,
                snapshot=first.snapshot,
            ),
        )


def test_a_declared_write_makes_the_snapshot_explicitly_stale(
    instance: AppContext,
) -> None:
    create_work(instance, CreateWorkParams(kind="feature", title="one", reason=WHY))
    create_work(instance, CreateWorkParams(kind="feature", title="two", reason=WHY))
    first = list_board(instance, BoardListParams(cells=[_cell("open")], page_size=1))
    create_work(instance, CreateWorkParams(kind="feature", title="later", reason=WHY))

    with pytest.raises(InvalidCursor, match="stale"):
        list_board(
            instance,
            BoardListParams(
                cells=[_cell("open", cursor=first.cells[0].next_cursor)],
                page_size=1,
                snapshot=first.snapshot,
            ),
        )


def test_every_row_beyond_the_old_two_thousand_cap_is_reachable(
    instance: AppContext,
) -> None:
    """The scale fixture is one transaction; paging, not setup, is under test."""
    start = datetime(2026, 8, 1, tzinfo=UTC)
    with instance.declared.write() as txn:
        for index in range(2005):
            moment = start + timedelta(microseconds=index)
            txn.insert_work_item(
                WorkItem(
                    id=f"wrk_scale_{index:04d}",
                    ref=txn.next_work_ref(),
                    kind="feature",
                    title=f"scale item {index:04d}",
                    state="open",
                    priority="p2",
                    origin="created",
                    trust_state="unverified",
                    created_at=moment,
                    updated_at=moment,
                )
            )

    request = BoardListParams(cells=[_cell("open")], page_size=100)
    seen: list[str] = []
    snapshot: str | None = None
    while True:
        page = list_board(instance, request)
        cell = page.cells[0]
        seen.extend(item.ref for item in cell.items)
        snapshot = page.snapshot
        if cell.next_cursor is None:
            break
        request = BoardListParams(
            cells=[_cell("open", cursor=cell.next_cursor)],
            page_size=100,
            snapshot=snapshot,
        )

    assert len(seen) == 2005
    assert len(set(seen)) == 2005
    assert seen[-1] == "WI-2005"
