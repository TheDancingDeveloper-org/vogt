"""Where a work item's change has got to: landed, deployed, verified (WI-1330).

Pure: everything here is computed from facts the caller already read — the
landed change, the deploy-lane views `deployed.versions` builds from the
collector's observations, and the item's `verification` comments. No git or
forge call happens here (design §2.5).

`core/delivery.py` is input delivery to sessions; this is unrelated.
"""

from __future__ import annotations

from collections.abc import Sequence
from datetime import datetime
from typing import Literal

from pydantic import BaseModel, ConfigDict

from vogt.core.entities import VerificationFields, VerificationPolicy

LaneStatus = Literal["at_head", "behind", "diverged", "unknown", "not_collected"]

#: The kinds whose close can need a verification record. A chore or a
#: question delivers no behaviour a check could exercise, so it never does.
VERIFIED_KINDS = frozenset({"feature", "bug"})

VERIFICATION_REQUIRED_MESSAGE = (
    "post a verification comment (work_comment kind=verification), "
    "or ask a person to close it"
)


class _Model(BaseModel):
    model_config = ConfigDict(extra="forbid", frozen=True)


class Landed(_Model):
    """The change the item's latest hand-off names, and why it counts as landed."""

    sha: str
    branch: str | None = None
    #: What showed it landed, e.g. "branch wi-7 tip abc123 is 0 behind main".
    evidence: str


class LaneView(_Model):
    """The parts of one `deployed.versions` lane the projection reads."""

    name: str
    status: LaneStatus
    commits_behind: int | None = None
    unpromoted_shas: list[str] = []
    deployed_sha: str | None = None
    receipt_at: str | None = None


class LaneDeployed(_Model):
    """One lane's answer: does it run the landed change?"""

    lane: str
    state: Literal["deployed", "not_deployed", "unknown"]
    #: The lane evidence the answer was read from.
    evidence: str


class Verified(_Model):
    """The verification comment that counts, with who posted it and where."""

    comment_id: str
    lane: str
    what: str
    evidence: str
    by: str | None = None
    head_sha: str | None = None
    at: datetime


class VerificationRecord(_Model):
    """A `verification` comment, flattened for the projection."""

    comment_id: str
    at: datetime
    fields: VerificationFields


class Delivery(_Model):
    landed: Landed | None = None
    deployed: list[LaneDeployed] = []
    verified: Verified | None = None


def project(
    landed: Landed | None,
    lanes: Sequence[LaneView],
    verifications: Sequence[VerificationRecord],
    allowed_lanes: Sequence[str],
) -> Delivery:
    """Project the three delivery facts. No landed change → no lane answers."""
    deployed = [] if landed is None else [_lane(landed.sha, lane) for lane in lanes]
    return Delivery(
        landed=landed,
        deployed=deployed,
        verified=_verified(verifications, allowed_lanes),
    )


def _lane(sha: str, lane: LaneView) -> LaneDeployed:
    if lane.status == "at_head":
        return LaneDeployed(
            lane=lane.name,
            state="deployed",
            evidence=f"lane {lane.name} is at its branch head"
            + (f" ({lane.deployed_sha})" if lane.deployed_sha else ""),
        )
    if lane.status != "behind":
        state: Literal["not_deployed", "unknown"] = (
            "not_deployed" if lane.status == "diverged" else "unknown"
        )
        return LaneDeployed(
            lane=lane.name, state=state, evidence=f"lane {lane.name} is {lane.status}"
        )
    listed = len(lane.unpromoted_shas)
    if lane.commits_behind is None or lane.commits_behind > listed:
        return LaneDeployed(
            lane=lane.name,
            state="unknown",
            evidence=f"lane {lane.name} is behind and its commit list is "
            "truncated, so the change may be among the unlisted ones",
        )
    if _sha_in(sha, lane.unpromoted_shas):
        return LaneDeployed(
            lane=lane.name,
            state="not_deployed",
            evidence=f"{sha} is among lane {lane.name}'s "
            f"{lane.commits_behind} unpromoted commits",
        )
    return LaneDeployed(
        lane=lane.name,
        state="deployed",
        evidence=f"lane {lane.name} is {lane.commits_behind} behind and "
        f"{sha} is not among the unpromoted commits",
    )


def _sha_in(sha: str, shas: Sequence[str]) -> bool:
    # A hand-off may name an abbreviated sha; match on the common prefix.
    return any(s.startswith(sha) or sha.startswith(s) for s in shas if s and sha)


def _verified(
    records: Sequence[VerificationRecord], allowed_lanes: Sequence[str]
) -> Verified | None:
    allowed = set(allowed_lanes)
    for record in sorted(records, key=lambda r: r.at, reverse=True):
        if allowed and record.fields.lane not in allowed:
            continue
        fields = record.fields
        return Verified(
            comment_id=record.comment_id,
            lane=fields.lane,
            what=fields.what,
            evidence=fields.evidence,
            by=fields.by,
            head_sha=fields.head_sha,
            at=record.at,
        )
    return None


def needs_verification(kind: str, policy: VerificationPolicy | None) -> bool:
    """Whether closing an item of `kind` needs a verification record.

    Only when the project's policy says `required_for_done`, and only for
    `feature` and `bug` items (`VERIFIED_KINDS`).
    """
    return bool(policy and policy.required_for_done and kind in VERIFIED_KINDS)
