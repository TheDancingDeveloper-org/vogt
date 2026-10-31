"""The pure delivery projection: landed, deployed per lane, verified (WI-1330)."""

from __future__ import annotations

from datetime import UTC, datetime

from vogt.core.delivery_state import (
    Landed,
    LaneView,
    VerificationRecord,
    needs_verification,
    project,
)
from vogt.core.entities import VerificationFields, VerificationPolicy

SHA = "abc1234"
LANDED = Landed(sha=SHA, branch="wi-7", evidence="branch wi-7 is on main")


def _lane(status: str, **fields: object) -> LaneView:
    return LaneView.model_validate({"name": "dev", "status": status, **fields})


def _state(lane: LaneView) -> str:
    return project(LANDED, [lane], [], []).deployed[0].state


def _record(day: int, lane: str) -> VerificationRecord:
    return VerificationRecord(
        comment_id=f"c{day}",
        at=datetime(2026, 10, day, tzinfo=UTC),
        fields=VerificationFields(lane=lane, what="it works", evidence=f"day {day}"),
    )


def test_landed_only() -> None:
    delivery = project(LANDED, [], [], [])
    assert delivery.landed == LANDED
    assert delivery.deployed == []
    assert delivery.verified is None


def test_nothing_landed_answers_no_lane() -> None:
    assert project(None, [_lane("at_head")], [], []).deployed == []


def test_a_lane_at_head_is_deployed() -> None:
    assert _state(_lane("at_head", deployed_sha="f00")) == "deployed"


def test_a_lane_behind_without_the_sha_is_deployed() -> None:
    lane = _lane("behind", commits_behind=2, unpromoted_shas=["111", "222"])
    assert _state(lane) == "deployed"


def test_a_lane_behind_with_the_sha_is_not_deployed() -> None:
    lane = _lane("behind", commits_behind=2, unpromoted_shas=["111", SHA + "ffff"])
    deployed = project(LANDED, [lane], [], []).deployed[0]
    assert deployed.state == "not_deployed"
    assert SHA in deployed.evidence


def test_a_truncated_list_reads_unknown() -> None:
    lane = _lane("behind", commits_behind=300, unpromoted_shas=["111"])
    assert _state(lane) == "unknown"


def test_diverged_is_not_deployed_and_the_rest_unknown() -> None:
    assert _state(_lane("diverged")) == "not_deployed"
    assert _state(_lane("unknown")) == "unknown"
    assert _state(_lane("not_collected")) == "unknown"


def test_verified_is_the_newest_on_an_allowed_lane() -> None:
    records = [_record(1, "dev"), _record(3, "staging"), _record(2, "dev")]
    verified = project(None, [], records, ["dev"]).verified
    assert verified is not None
    assert verified.comment_id == "c2"
    newest = project(None, [], records, []).verified
    assert newest is not None
    assert newest.comment_id == "c3"
    assert project(None, [], records, ["prod"]).verified is None


def test_only_features_and_bugs_need_verification_under_the_policy() -> None:
    on = VerificationPolicy(required_for_done=True)
    assert needs_verification("feature", on)
    assert needs_verification("bug", on)
    assert not needs_verification("chore", on)
    assert not needs_verification("question", on)
    assert not needs_verification("feature", VerificationPolicy())
    assert not needs_verification("feature", None)
