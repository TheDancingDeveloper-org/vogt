"""`deployed.versions`: is each deployment lane at its branch head? (WI-855)

"Is dev at head?" / "did you redeploy?" were asked again and again with no
single answer source. The `deploy-lanes` collector reads each configured
lane's evidence — a receipt its pipeline committed, the running instance's
version endpoint — and how that revision compares with the lane's branch.
This read joins that observation to the declared store: the work items the
undeployed commits name, with their titles and states.

A read path: no forge or network call happens here. A lane configured but
not yet swept is reported `not_collected`, never as "at head".
"""

from __future__ import annotations

import re
from typing import Literal

from vogt.adapters.forge.kinds import KIND_DEPLOY_LANE
from vogt.application.context import AppContext
from vogt.application.models import (
    DeployedCommit,
    DeployedLaneView,
    DeployedVersionsParams,
    DeployedVersionsResult,
    UnpromotedWorkItem,
)
from vogt.application.services import _resolve
from vogt.application.services.views import freshness_of
from vogt.config import DeployLane
from vogt.core.entities import Observation
from vogt.storage.interface import ReadView

_WORK_REF = re.compile(r"\bWI-\d+\b")

LaneStatus = Literal["at_head", "behind", "diverged", "unknown", "not_collected"]


def deployed_versions(
    ctx: AppContext, params: DeployedVersionsParams
) -> DeployedVersionsResult:
    """What each configured lane runs, against its branch head."""
    lanes = list(ctx.config.deploy_lanes)
    configured = len(lanes)
    if not lanes:
        return DeployedVersionsResult(
            lanes=[],
            configured=0,
            detail="deploy_lanes is not configured, so deployed versions are "
            "not collected",
            freshness=freshness_of(ctx),
        )
    with ctx.declared.read() as view:
        if params.project is not None:
            slug = _resolve.project(view, params.project).slug
            lanes = [lane for lane in lanes if lane.project == slug]
        if params.lane is not None:
            lanes = [lane for lane in lanes if lane.name == params.lane]
        observed = {
            observation.subject_key: observation
            for observation in ctx.observed.latest(
                kinds=(KIND_DEPLOY_LANE,), limit=max(len(lanes) * 4, 50)
            )
        }
        views = [
            _view(view, lane, observed.get(f"deploy:{lane.project}/{lane.name}"))
            for lane in lanes
        ]
    return DeployedVersionsResult(
        lanes=views,
        configured=configured,
        detail=None if views else "no configured lane matches the filter",
        freshness=freshness_of(ctx),
    )


def _view(
    view: ReadView, lane: DeployLane, observation: Observation | None
) -> DeployedLaneView:
    if observation is None:
        return DeployedLaneView(
            name=lane.name,
            lane=lane.name,
            project_slug=lane.project,
            branch=lane.branch,
            status="not_collected",
            detail="the deploy-lanes collector has not read this lane yet",
        )
    payload = observation.payload
    receipt = _dict(payload.get("receipt"))
    live = _dict(payload.get("live"))
    compare = _dict(payload.get("compare"))
    commits: list[DeployedCommit] = []
    if compare is not None:
        listed = compare.get("commits")
        for raw in listed if isinstance(listed, list) else []:
            if not isinstance(raw, dict) or not isinstance(raw.get("sha"), str):
                continue
            subject = str(raw.get("subject") or "")
            commits.append(
                DeployedCommit(
                    sha=str(raw["sha"]),
                    subject=subject,
                    work_items=sorted(set(_WORK_REF.findall(subject))),
                )
            )
    refs = sorted({ref for commit in commits for ref in commit.work_items})
    items: list[UnpromotedWorkItem] = []
    for ref in refs:
        item = view.work_item_by_ref(ref)
        items.append(
            UnpromotedWorkItem(
                ref=ref,
                title=None if item is None else item.title,
                state=None if item is None else item.state,
            )
        )
    details = [
        str(value)
        for value in (
            payload.get("live_detail"),
            payload.get("receipt_detail"),
            payload.get("compare_detail"),
        )
        if isinstance(value, str) and value
    ]
    ahead = _int(None if compare is None else compare.get("ahead_by"))
    deployed_from = payload.get("deployed_from")
    return DeployedLaneView(
        name=lane.name,
        lane=lane.name,
        project_slug=lane.project,
        branch=lane.branch,
        status=_status(compare),
        deployed_sha=_text(payload.get("deployed_sha")),
        deployed_from=deployed_from if deployed_from in ("live", "receipt") else None,
        version=None if live is None else _text(live.get("version")),
        source_tag=None if receipt is None else _text(receipt.get("source_tag")),
        receipt_status=None if receipt is None else _text(receipt.get("status")),
        receipt_at=None if receipt is None else _text(receipt.get("timestamp")),
        live_sha=None if live is None else _text(live.get("source_sha")),
        head_sha=None if compare is None else _text(compare.get("head_sha")),
        commits_behind=ahead,
        unpromoted_commits=commits,
        unpromoted_work_items=items,
        truncated=bool(compare is not None and compare.get("truncated")),
        observed_at=observation.observed_at,
        detail="; ".join(details) or None,
    )


def _status(compare: dict[str, object] | None) -> LaneStatus:
    if compare is None:
        return "unknown"
    status = compare.get("status")
    ahead = _int(compare.get("ahead_by"))
    behind = _int(compare.get("behind_by")) or 0
    if status == "diverged" or (ahead and behind):
        return "diverged"
    if status == "identical" or ahead == 0:
        # A deployed revision *ahead* of the branch (behind_by > 0, nothing
        # ahead) is not on it: deployed from somewhere else.
        return "diverged" if behind else "at_head"
    if ahead is not None and ahead > 0:
        return "behind"
    return "unknown"


def _dict(value: object) -> dict[str, object] | None:
    return value if isinstance(value, dict) else None


def _text(value: object) -> str | None:
    return value if isinstance(value, str) and value else None


def _int(value: object) -> int | None:
    return value if isinstance(value, int) and not isinstance(value, bool) else None


__all__ = ["deployed_versions"]
