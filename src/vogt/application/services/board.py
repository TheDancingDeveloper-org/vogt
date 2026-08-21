"""Bounded, snapshot-stable Board projection (NFR-S5)."""

from __future__ import annotations

import base64
import binascii
import json
from datetime import UTC, datetime
from typing import Any

from vogt.application.context import AppContext
from vogt.application.models import (
    BoardCellParams,
    BoardCellResult,
    BoardListParams,
    BoardListResult,
)
from vogt.application.services import _resolve, upstream
from vogt.application.services.views import candidate_population
from vogt.core.clock import from_iso
from vogt.core.digest import digest_of
from vogt.core.entities import Project, WorkItem
from vogt.errors import InvalidCursor, InvalidRequest
from vogt.storage.interface import BoardCellQuery, ReadView, WorkFilter


def list_board(ctx: AppContext, params: BoardListParams) -> BoardListResult:
    """Return exact totals and bounded pages for a batch of Board cells.

    The declared revision is part of every opaque token. A continuation after
    a write is refused rather than mixing old totals with new rows; the caller
    starts a fresh snapshot, which is the honest live-refresh behaviour.
    """
    _validate_cells(params)
    with ctx.declared.read() as view:
        project_row = (
            None if params.project is None else _resolve.project(view, params.project)
        )
        work_filter = _work_filter(view, params, project_row)
        fingerprint = _fingerprint(params)
        revision = view.current_revision()

        # The upstream-truth half of the Board (#181): linked projects'
        # items live in the observed mirror + overlay, not in `work_items`,
        # so they are gathered here and merged into every count and page —
        # each upstream issue exactly once, because `upstream_items` already
        # excludes adopted subjects and linked projects grow no native rows.
        upstream_entries = _upstream_entries(
            ctx, view, params, project_row, work_filter
        )

        requested_snapshot = (
            _decode_token(params.snapshot, kind="snapshot")
            if params.snapshot is not None
            else None
        )
        if requested_snapshot is not None:
            _validate_snapshot(requested_snapshot, fingerprint, revision)
            high_water = _decode_high_water(requested_snapshot)
            snapshot_at = _decode_snapshot_at(requested_snapshot)
            snapshot = params.snapshot
            assert snapshot is not None
        else:
            high_water = _combined_high_water(
                view.board_high_water(work_filter), upstream_entries
            )
            snapshot_at = ctx.clock()
            snapshot = _encode_token(
                {
                    "kind": "snapshot",
                    "fingerprint": fingerprint,
                    "revision": revision,
                    "snapshot_at": snapshot_at.isoformat(),
                    "high_water": _dump_high_water(high_water),
                }
            )
        upstream_entries = _under_high_water(upstream_entries, high_water)

        queries = tuple(
            _cell_query(cell, fingerprint, revision, high_water)
            for cell in params.cells
        )
        counts = view.board_counts(
            work_filter, lane_mode=params.lane_mode, high_water=high_water
        )
        for (lane, state), extra in _upstream_counts(upstream_entries).items():
            counts[(lane, state)] = counts.get((lane, state), 0) + extra
        pages = view.board_work_items(
            work_filter,
            lane_mode=params.lane_mode,
            cells=queries,
            high_water=high_water,
            # One sentinel row says whether continuation exists without an
            # extra query and is never returned to the client.
            limit=params.page_size + 1,
        )

    cells: list[BoardCellResult] = []
    for cell, query in zip(params.cells, queries, strict=True):
        key = (cell.lane_key, cell.state)
        fetched = _merge_cell(
            pages[key], upstream_entries, query, limit=params.page_size + 1
        )
        shown = fetched[: params.page_size]
        cells.append(
            BoardCellResult(
                lane_key=cell.lane_key,
                state=cell.state,
                items=shown,
                total=counts.get(key, 0),
                next_cursor=(
                    _cell_cursor(
                        fingerprint,
                        revision,
                        high_water,
                        cell.lane_key,
                        shown[-1],
                    )
                    if len(fetched) > params.page_size and shown
                    else None
                ),
            )
        )
    column_totals: dict[str, int] = {}
    lane_totals: dict[str, int] = {}
    for (lane, state), count in counts.items():
        column_totals[state] = column_totals.get(state, 0) + count
        lane_totals[lane] = lane_totals.get(lane, 0) + count
    # The honest denominator (#187): how many things the Backlog would consider
    # for this same scope, and how many of those are declared work. Computed
    # after the declared read closes rather than nested inside it, because
    # `candidate_population` opens its own read to score the observed half.
    backlog_candidates, declared_total = candidate_population(
        ctx,
        project=params.project,
        kinds=params.kinds,
        priorities=params.priorities,
        assignee=params.assignee,
        initiative=params.initiative,
        label=params.label,
    )
    return BoardListResult(
        cells=cells,
        column_totals=column_totals,
        lane_totals=lane_totals,
        total=sum(counts.values()),
        backlog_candidates=backlog_candidates,
        declared_total=declared_total,
        snapshot=snapshot,
        snapshot_at=snapshot_at,
        revision=revision,
    )


def _work_filter(
    view: ReadView, params: BoardListParams, project_row: Project | None
) -> WorkFilter:
    return WorkFilter(
        project_id=None if project_row is None else project_row.id,
        kinds=tuple(params.kinds or ()),
        states=tuple(params.states or ()),
        priorities=tuple(params.priorities or ()),
        assignee_actor_id=(
            None
            if params.assignee is None
            else _resolve.actor(view, params.assignee).id
        ),
        initiative_id=(
            None
            if params.initiative is None
            else _resolve.initiative(view, params.initiative).id
        ),
        label=params.label,
        # Board draws terminal workflow columns when requested; visibility is
        # controlled by the explicit state filter, not by work.list's default.
        exclude_terminal=False,
    )


def _upstream_entries(
    ctx: AppContext,
    view: ReadView,
    params: BoardListParams,
    project_row: Project | None,
    work_filter: WorkFilter,
) -> list[tuple[str, WorkItem]]:
    """`(lane_key, item)` for every upstream-truth item the scope covers.

    Closed issues are included — the Board draws terminal columns when asked
    — and the shared `WorkFilter` narrows them exactly as `_work_where`
    narrows declared rows, so the two halves of a cell mean the same thing.
    """
    entries: list[tuple[str, WorkItem]] = []
    for linked in upstream.linked_projects(view, project_row):
        for item in upstream.upstream_items(ctx, view, linked, include_closed=True):
            if not upstream.matches(item, work_filter):
                continue
            entries.append((_upstream_lane(params.lane_mode, item), item))
    entries.sort(key=lambda entry: (entry[1].created_at, entry[1].ref))
    return entries


def _upstream_lane(lane_mode: str, item: WorkItem) -> str:
    """The lane an upstream item lands in, mirroring `_board_lane_expression`."""
    if lane_mode == "project":
        return item.project_slug or ""
    if lane_mode == "initiative":
        return item.initiative_id or ""
    return ""


def _combined_high_water(
    declared: tuple[datetime, str] | None,
    entries: list[tuple[str, WorkItem]],
) -> tuple[datetime, str] | None:
    """One snapshot bound over both halves of the Board."""
    marks: list[tuple[datetime, str]] = [] if declared is None else [declared]
    if entries:
        marks.append(max((item.created_at, item.ref) for _, item in entries))
    return max(marks) if marks else None


def _under_high_water(
    entries: list[tuple[str, WorkItem]],
    high_water: tuple[datetime, str] | None,
) -> list[tuple[str, WorkItem]]:
    """The same `(created_at, ref) <= high_water` bound the SQL applies."""
    if high_water is None:
        return []
    return [
        (lane, item)
        for lane, item in entries
        if (item.created_at, item.ref) <= high_water
    ]


def _upstream_counts(
    entries: list[tuple[str, WorkItem]],
) -> dict[tuple[str, str], int]:
    counts: dict[tuple[str, str], int] = {}
    for lane, item in entries:
        key = (lane, item.state)
        counts[key] = counts.get(key, 0) + 1
    return counts


def _merge_cell(
    sql_items: list[WorkItem],
    entries: list[tuple[str, WorkItem]],
    query: BoardCellQuery,
    *,
    limit: int,
) -> list[WorkItem]:
    """Merge one cell's declared page with its upstream items, in order.

    Both streams are sorted by `(created_at, ref)` and each is complete up
    to `limit` rows past the cursor, so the first `limit` rows of the merge
    are the first `limit` rows of the union — which is what makes the cell
    cursor continue correctly across the two stores.
    """
    mine = [
        item
        for lane, item in entries
        if lane == query.lane_key and item.state == query.state
    ]
    if query.after_created_at is not None and query.after_ref is not None:
        after = (query.after_created_at, query.after_ref)
        mine = [item for item in mine if (item.created_at, item.ref) > after]
    if not mine:
        return sql_items[:limit]
    merged = sorted([*sql_items, *mine], key=lambda item: (item.created_at, item.ref))
    return merged[:limit]


def _validate_cells(params: BoardListParams) -> None:
    seen: set[tuple[str, str]] = set()
    for cell in params.cells:
        key = (cell.lane_key, cell.state)
        if key in seen:
            raise InvalidRequest(
                f"board.list cell {cell.lane_key!r}/{cell.state!r} is duplicated"
            )
        seen.add(key)
        if params.lane_mode == "none" and cell.lane_key:
            raise InvalidRequest("board.list lane_key must be blank in `none` mode")
        if cell.cursor is not None and params.snapshot is None:
            raise InvalidCursor("a Board cell cursor requires its snapshot token")


def _fingerprint(params: BoardListParams) -> str:
    return digest_of(
        {
            "project": params.project,
            "kinds": sorted(params.kinds or ()),
            "states": sorted(params.states or ()),
            "priorities": sorted(params.priorities or ()),
            "assignee": params.assignee,
            "initiative": params.initiative,
            "label": params.label,
            "lane_mode": params.lane_mode,
        }
    )


def _validate_snapshot(token: dict[str, Any], fingerprint: str, revision: int) -> None:
    if token.get("fingerprint") != fingerprint:
        raise InvalidCursor("Board snapshot belongs to another filter or lane mode")
    if token.get("revision") != revision:
        raise InvalidCursor(
            "Board snapshot is stale because declared work changed; refresh the Board"
        )


def _cell_query(
    cell: BoardCellParams,
    fingerprint: str,
    revision: int,
    high_water: tuple[datetime, str] | None,
) -> BoardCellQuery:
    if cell.cursor is None:
        return BoardCellQuery(lane_key=cell.lane_key, state=cell.state)
    token = _decode_token(cell.cursor, kind="cell")
    if (
        token.get("fingerprint") != fingerprint
        or token.get("revision") != revision
        or token.get("lane_key") != cell.lane_key
        or token.get("state") != cell.state
        or token.get("high_water") != _dump_high_water(high_water)
    ):
        raise InvalidCursor("Board cell cursor belongs to another cell or snapshot")
    try:
        created_at = from_iso(str(token["after_created_at"]))
        ref = token["after_ref"]
        if not isinstance(ref, str):
            raise ValueError
    except (KeyError, TypeError, ValueError):
        raise InvalidCursor("Board cell cursor is malformed") from None
    return BoardCellQuery(
        lane_key=cell.lane_key,
        state=cell.state,
        after_created_at=created_at,
        after_ref=ref,
    )


def _cell_cursor(
    fingerprint: str,
    revision: int,
    high_water: tuple[datetime, str] | None,
    lane_key: str,
    item: WorkItem,
) -> str:
    return _encode_token(
        {
            "kind": "cell",
            "fingerprint": fingerprint,
            "revision": revision,
            "high_water": _dump_high_water(high_water),
            "lane_key": lane_key,
            "state": item.state,
            "after_created_at": item.created_at.isoformat(),
            "after_ref": item.ref,
        }
    )


def _dump_high_water(high_water: tuple[datetime, str] | None) -> list[str] | None:
    if high_water is None:
        return None
    return [high_water[0].isoformat(), high_water[1]]


def _decode_high_water(token: dict[str, Any]) -> tuple[datetime, str] | None:
    raw = token.get("high_water")
    if raw is None:
        return None
    if not isinstance(raw, list) or len(raw) != 2 or not isinstance(raw[1], str):
        raise InvalidCursor("Board snapshot has an invalid high-water mark")
    try:
        return datetime.fromisoformat(str(raw[0])), raw[1]
    except ValueError:
        raise InvalidCursor("Board snapshot has an invalid high-water mark") from None


def _decode_snapshot_at(token: dict[str, Any]) -> datetime:
    try:
        value = datetime.fromisoformat(str(token["snapshot_at"]))
        return value if value.tzinfo is not None else value.replace(tzinfo=UTC)
    except (KeyError, ValueError):
        raise InvalidCursor("Board snapshot has an invalid timestamp") from None


def _encode_token(value: dict[str, object]) -> str:
    raw = json.dumps(value, sort_keys=True, separators=(",", ":")).encode()
    return base64.urlsafe_b64encode(raw).decode().rstrip("=")


def _decode_token(value: str, *, kind: str) -> dict[str, Any]:
    try:
        raw = base64.urlsafe_b64decode(value + "=" * (-len(value) % 4))
        decoded = json.loads(raw)
        if not isinstance(decoded, dict) or decoded.get("kind") != kind:
            raise ValueError
        return decoded
    except (binascii.Error, json.JSONDecodeError, UnicodeDecodeError, ValueError):
        raise InvalidCursor(f"Board {kind} token is malformed") from None
