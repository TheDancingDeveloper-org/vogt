"""Ranked views, and the explanation behind them (FR-V1–V3)."""

from __future__ import annotations

from collections.abc import Sequence
from datetime import datetime

from vogt.application.context import AppContext
from vogt.application.models import (
    BacklogParams,
    BacklogResult,
    BugsParams,
    ContributionView,
    NotCollected,
    RankedItem,
    WhyParams,
    WhyResult,
)
from vogt.application.services import _resolve
from vogt.core.entities import WorkItem
from vogt.core.ranking import PENDING_INPUTS, RankingInputs, rank, score_item
from vogt.core.workflow import TERMINAL_STATES
from vogt.storage.interface import ReadView, WorkFilter

#: Ranked views read a wide slice and score it in the application layer,
#: because the score is a documented function of several inputs rather than
#: something SQL should be asked to express. The cap keeps that honest: at
#: the NFR-S1 envelope this is the query the M2 benchmark fixture watches.
RANKING_CANDIDATE_LIMIT = 1000


def rank_items(
    view: ReadView, items: list[WorkItem], *, now: datetime
) -> list[RankedItem]:
    """Score and order a set of work items."""
    if not items:
        return []
    fan_out = view.blocking_fan_out([item.id for item in items])

    weights: dict[str, int] = {}
    for item in items:
        if item.initiative_id and item.initiative_id not in weights:
            initiative = view.initiative_by_id(item.initiative_id)
            weights[item.initiative_id] = 0 if initiative is None else initiative.weight

    scores = [
        score_item(
            item,
            RankingInputs(
                now=now,
                blocking_fan_out=fan_out.get(item.id, 0),
                initiative_weight=(
                    weights.get(item.initiative_id, 0) if item.initiative_id else 0
                ),
                is_terminal=item.state in TERMINAL_STATES,
            ),
        )
        for item in items
    ]
    by_id = {item.id: item for item in items}
    return [
        RankedItem(item=by_id[score.work_item_id], score=score.total)
        for score in rank(scores)
    ]


def _filter_from(
    view: ReadView,
    *,
    project: str | None,
    kinds: Sequence[str] | None,
    priorities: Sequence[str] | None,
    assignee: str | None,
    initiative: str | None,
    label: str | None,
    trust_states: Sequence[str] | None = None,
) -> WorkFilter:
    return WorkFilter(
        project_id=None if project is None else _resolve.project(view, project).id,
        kinds=tuple(kinds or ()),
        priorities=tuple(priorities or ()),
        assignee_actor_id=(
            None if assignee is None else _resolve.actor(view, assignee).id
        ),
        initiative_id=(
            None if initiative is None else _resolve.initiative(view, initiative).id
        ),
        label=label,
        trust_states=tuple(trust_states or ()),
        exclude_terminal=True,
        limit=RANKING_CANDIDATE_LIMIT,
    )


_FRESHNESS = NotCollected(
    detail="Nothing has been swept: collectors and their coverage records "
    "arrive at M2, and this field carries the oldest relevant sweep from then "
    "on (FR-V4)."
)


def backlog(ctx: AppContext, params: BacklogParams) -> BacklogResult:
    """The ranked backlog, globally or for one project (FR-V1, FR-V2)."""
    with ctx.declared.read() as view:
        work_filter = _filter_from(
            view,
            project=params.project,
            kinds=params.kinds,
            priorities=params.priorities,
            assignee=params.assignee,
            initiative=params.initiative,
            label=params.label,
            trust_states=params.trust_states,
        )
        items = view.list_work_items(work_filter)
        ranked = rank_items(view, items, now=ctx.clock())
        return BacklogResult(
            items=ranked[: params.limit],
            total_considered=len(items),
            scope=params.project or "global",
            freshness=_FRESHNESS,
        )


def bugs(ctx: AppContext, params: BugsParams) -> BacklogResult:
    """Open bugs across every project, ranked the same way (FR-V1)."""
    with ctx.declared.read() as view:
        work_filter = _filter_from(
            view,
            project=params.project,
            kinds=["bug"],
            priorities=params.priorities,
            assignee=params.assignee,
            initiative=None,
            label=params.label,
        )
        items = view.list_work_items(work_filter)
        ranked = rank_items(view, items, now=ctx.clock())
        return BacklogResult(
            items=ranked[: params.limit],
            total_considered=len(items),
            scope=params.project or "global",
            freshness=_FRESHNESS,
        )


def why(ctx: AppContext, params: WhyParams) -> WhyResult:
    """Per-input score contributions for one item (FR-V3).

    Answerable for any item, including finished ones — the terminal penalty
    is itself a contribution, so "why is this not in my backlog" gets an
    answer rather than an empty result.
    """
    with ctx.declared.read() as view:
        item = _resolve.work_item(view, params.ref)
        fan_out = view.blocking_fan_out([item.id])
        weight = 0
        if item.initiative_id:
            initiative = view.initiative_by_id(item.initiative_id)
            weight = 0 if initiative is None else initiative.weight
        score = score_item(
            item,
            RankingInputs(
                now=ctx.clock(),
                blocking_fan_out=fan_out.get(item.id, 0),
                initiative_weight=weight,
                is_terminal=item.state in TERMINAL_STATES,
            ),
        )
        return WhyResult(
            ref=item.ref,
            title=item.title,
            total=score.total,
            contributions=[
                ContributionView(
                    input=entry.input,
                    detail=entry.detail,
                    value=entry.value,
                    weight=entry.weight,
                    contribution=entry.contribution,
                )
                for entry in score.contributions
            ],
            inputs_not_yet_available=dict(PENDING_INPUTS),
        )
