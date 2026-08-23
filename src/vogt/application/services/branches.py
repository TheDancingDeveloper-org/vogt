"""Assembling a work item's branches — declared and observed, side by side.

The two halves of the branch binding (#283) meet here and nowhere else:

- **Declared** — the branches a Vogt-started session said it would use, held
  on the item's overlay row.
- **Observed** — the branches a `git-local` sweep actually found in the
  checkout, matched to this item by the configured pattern.

They are joined by name but never merged (FR-O2): a branch present on one side
and not the other is drift, and the view says which side it came from so the
surface can show the disagreement rather than average it away. Age is derived
from the observation's stable `last_commit_at` at read time, so it is always
current without the observation changing — and therefore re-recording — every
sweep (NFR-S2).
"""

from __future__ import annotations

import re
from datetime import datetime
from typing import Literal

from vogt.application.context import AppContext
from vogt.application.models import WorkItemBranchView
from vogt.core.entities import Observation, WorkItem
from vogt.storage.interface import ReadView

_FORGE_NUMBER = re.compile(r"#(\d+)$")


def branch_views_for(
    ctx: AppContext, view: ReadView, item: WorkItem
) -> list[WorkItemBranchView]:
    """Every branch bound to `item`, declared and observed, kept separate."""
    overlay = view.work_overlay(item.ref)
    declared = list(overlay.branches) if overlay is not None else []

    observed: dict[str, Observation] = {}
    if item.project_id is not None and ctx.observed.has_evidence_tables():
        forge_number = _forge_number(item.ref)
        for obs in ctx.observed.latest(
            kinds=("git.branch",), project_id=item.project_id, limit=1000
        ):
            name = str(obs.payload.get("name", ""))
            if not name or not _binds(obs, item.ref, forge_number):
                continue
            observed[name] = obs

    now = ctx.clock()
    views: list[WorkItemBranchView] = []
    for name in declared:
        views.append(_view(name, declared=True, obs=observed.get(name), now=now))
    for name, obs in observed.items():
        if name in declared:
            continue
        views.append(_view(name, declared=False, obs=obs, now=now))
    return views


def _binds(obs: Observation, work_ref: str, forge_number: int | None) -> bool:
    """Whether this branch observation is for `work_ref`.

    A vogt ref is matched directly; a forge number matches the item's own issue
    number, which is how a `gh-264-…` branch reaches the upstream item whose
    subject key ends `#264`.
    """
    if obs.payload.get("work_item_ref") == work_ref:
        return True
    return forge_number is not None and obs.payload.get("forge_number") == forge_number


def _view(
    name: str, *, declared: bool, obs: Observation | None, now: datetime
) -> WorkItemBranchView:
    observed = obs is not None
    source: Literal["declared", "observed", "both"]
    if not observed:
        source = "declared"
    elif declared:
        source = "both"
    else:
        source = "observed"
    payload = obs.payload if obs is not None else {}
    last_commit_at = _parse(payload.get("last_commit_at"))
    return WorkItemBranchView(
        name=name,
        source=source,
        drift=source != "both",
        tip=_opt_str(payload.get("tip")),
        ahead=_opt_int(payload.get("ahead")),
        behind=_opt_int(payload.get("behind")),
        default_branch=_opt_str(payload.get("default_branch")),
        last_commit_at=last_commit_at,
        last_commit_age_seconds=_age(last_commit_at, now),
        observed_at=None if obs is None else obs.observed_at,
    )


def _age(last_commit_at: datetime | None, now: datetime) -> int | None:
    if last_commit_at is None or last_commit_at.tzinfo is None or now.tzinfo is None:
        return None
    return max(0, int((now - last_commit_at).total_seconds()))


def _parse(value: object) -> datetime | None:
    if not isinstance(value, str):
        return None
    try:
        return datetime.fromisoformat(value)
    except ValueError:
        return None


def _opt_str(value: object) -> str | None:
    return value if isinstance(value, str) else None


def _opt_int(value: object) -> int | None:
    return value if isinstance(value, int) and not isinstance(value, bool) else None


def _forge_number(ref: str) -> int | None:
    match = _FORGE_NUMBER.search(ref)
    return int(match.group(1)) if match is not None else None


__all__ = ["branch_views_for"]
