"""Unlinked projects have no work surface — a link/publish CTA instead (#183).

These are the repurposed forge-less honesty tests. Before #183 the suite
asserted that an unlinked project's native backlog was *real* — items ranked,
boards drew them, `work.list` served them — because "the forge-less layer
stays real" was a guarantee. That guarantee is withdrawn (REQUIREMENTS §7.3,
via #178 decision 10): what these tests pin now is the CTA contract that
replaced it —

- project-scoped `backlog` / `work.list` / `board.list` on an unlinked
  project answer with **empty items and a machine-readable
  `link_state: "unlinked"` marker**, not an error and not the native rows;
- the **global** Backlog and Board exclude unlinked projects' native items,
  with the exclusion counted (`excluded_unlinked`) so a small answer cannot
  pass for a small estate;
- the typed `NotLinked` write refusal (#181) stays as the write-plane half
  (pinned in `test_upstream_truth`); native rows stay reachable by ref, and
  a link or publish migrates them (`test_native_migration`).
"""

from __future__ import annotations

from vogt.application.context import AppContext
from vogt.application.models import (
    BacklogParams,
    BoardCellParams,
    BoardListParams,
    BugsParams,
    GetWorkParams,
    ListWorkParams,
    RegisterProjectParams,
)
from vogt.application.services import (
    backlog,
    bugs,
    get_work,
    list_board,
    list_work,
    register_project,
)

from tests.conftest import native_work_item

WHY = "unlinked surface test"


def _estate(instance: AppContext) -> AppContext:
    """An unlinked project holding two open native items and a finished one."""
    register_project(
        instance,
        RegisterProjectParams(name="Folder", root_path="/srv/folder", reason=WHY),
    )
    native_work_item(instance, kind="bug", title="native bug", project="folder")
    native_work_item(instance, kind="feature", title="native feat", project="folder")
    native_work_item(
        instance, kind="chore", title="finished", project="folder", state="done"
    )
    return instance


def test_scoped_work_list_answers_with_the_marker(instance: AppContext) -> None:
    estate = _estate(instance)
    result = list_work(estate, ListWorkParams(project="folder"))
    assert result.items == [] and result.total == 0
    assert result.link_state == "unlinked", "the machine-readable CTA marker"


def test_scoped_backlog_answers_with_the_marker_and_the_migration_count(
    instance: AppContext,
) -> None:
    estate = _estate(instance)
    result = backlog(estate, BacklogParams(project="folder"))
    assert result.items == [] and result.total_considered == 0
    assert result.link_state == "unlinked"
    assert result.excluded_unlinked == 2, (
        "the CTA can say what a link or publish would migrate: the two open "
        "items, not the finished one"
    )


def test_scoped_bugs_answers_with_the_marker(instance: AppContext) -> None:
    estate = _estate(instance)
    result = bugs(estate, BugsParams(project="folder"))
    assert result.items == [] and result.link_state == "unlinked"


def test_scoped_board_answers_with_the_marker(instance: AppContext) -> None:
    estate = _estate(instance)
    result = list_board(
        estate,
        BoardListParams(
            project="folder",
            cells=[
                BoardCellParams(lane_key="", state="open"),
                BoardCellParams(lane_key="", state="in_progress"),
            ],
        ),
    )
    assert result.link_state == "unlinked"
    assert all(cell.items == [] and cell.total == 0 for cell in result.cells)
    assert result.total == 0
    assert result.excluded_unlinked == 2
    assert result.snapshot, "the result contract still holds — a real snapshot"


def test_the_global_views_exclude_and_count(instance: AppContext) -> None:
    estate = _estate(instance)
    ranked = backlog(estate, BacklogParams(limit=100))
    assert all(entry.project_slug != "folder" for entry in ranked.items)
    assert ranked.link_state is None, "the global view is not a project scope"
    assert ranked.excluded_unlinked == 2, "excluded, visibly — never dropped"

    board = list_board(
        estate,
        BoardListParams(cells=[BoardCellParams(lane_key="", state="open")]),
    )
    assert board.total == 0
    assert board.excluded_unlinked >= 2

    # The raw query surface stays complete: `work.list` global is how the
    # native rows remain reachable in bulk, alongside per-ref reads.
    raw = list_work(estate, ListWorkParams())
    assert {item.title for item in raw.items} == {"native bug", "native feat"}
    assert raw.link_state is None


def test_native_rows_stay_reachable_by_ref(instance: AppContext) -> None:
    estate = _estate(instance)
    held = get_work(estate, GetWorkParams(ref="WI-1"))
    assert held.item.title == "native bug", (
        "the withdrawal is a surface change, not data loss"
    )
