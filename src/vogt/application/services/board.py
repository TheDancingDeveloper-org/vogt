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
from vogt.application.services import _resolve
from vogt.core.clock import from_iso
from vogt.core.digest import digest_of
from vogt.core.entities import WorkItem
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
        work_filter = _work_filter(view, params)
        fingerprint = _fingerprint(params)
        revision = view.current_revision()
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
            high_water = view.board_high_water(work_filter)
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

        queries = tuple(
            _cell_query(cell, fingerprint, revision, high_water)
            for cell in params.cells
        )
        counts = view.board_counts(
            work_filter, lane_mode=params.lane_mode, high_water=high_water
        )
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
    for cell in params.cells:
        key = (cell.lane_key, cell.state)
        fetched = pages[key]
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
    return BoardListResult(
        cells=cells,
        column_totals=column_totals,
        lane_totals=lane_totals,
        total=sum(counts.values()),
        snapshot=snapshot,
        snapshot_at=snapshot_at,
        revision=revision,
    )


def _work_filter(view: ReadView, params: BoardListParams) -> WorkFilter:
    return WorkFilter(
        project_id=(
            None
            if params.project is None
            else _resolve.project(view, params.project).id
        ),
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
