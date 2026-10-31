"""`work.get` delivery and the agent-only verification gate on done (WI-1330)."""

from __future__ import annotations

import dataclasses
from datetime import UTC, datetime

import pytest

from vogt.application.context import AppContext
from vogt.application.models import (
    CommentParams,
    GetWorkParams,
    RegisterProjectParams,
    TransitionWorkParams,
    UpdateProjectParams,
    WorkItemBranchView,
)
from vogt.application.services import (
    comment_work,
    get_work,
    register_project,
    transition_work,
    update_project,
)
from vogt.application.services.work import _landed_of
from vogt.core.entities import (
    AgentPolicy,
    Comment,
    HandoffFields,
    VerificationFields,
    VerificationPolicy,
)
from vogt.core.principal import Principal
from vogt.errors import PersonRequired

from tests.conftest import native_work_item
from tests.test_sessions import ROOT

WHY = "delivery test"


def _agent(ctx: AppContext) -> AppContext:
    return dataclasses.replace(
        ctx,
        principal=Principal(
            identity_ref="agent:session:ses_x", kind="agent", display_name="ses_x"
        ),
    )


def _item(ctx: AppContext, *, required: bool, kind: str = "feature") -> str:
    register_project(
        ctx, RegisterProjectParams(name="Vogt", root_path=ROOT, reason=WHY)
    )
    update_project(
        ctx,
        UpdateProjectParams(
            slug="vogt",
            agent_policy=AgentPolicy(
                verification=VerificationPolicy(required_for_done=required)
            ),
            reason=WHY,
        ),
    )
    ref = native_work_item(ctx, title="Gate", kind=kind, project="vogt").ref
    transition_work(
        ctx, TransitionWorkParams(ref=ref, to_state="review", walk=True, reason=WHY)
    )
    return ref


def _verify(ctx: AppContext, ref: str, lane: str = "dev") -> None:
    comment_work(
        ctx,
        CommentParams(
            ref=ref,
            body="checked",
            kind="verification",
            verification=VerificationFields(lane=lane, what="it works", evidence="200"),
            reason=WHY,
        ),
    )


def _done(ctx: AppContext, ref: str) -> str:
    return transition_work(
        ctx, TransitionWorkParams(ref=ref, to_state="done", reason=WHY)
    ).item.state


def test_an_agent_is_refused_without_a_verification(instance: AppContext) -> None:
    ref = _item(instance, required=True)
    with pytest.raises(PersonRequired, match="work_comment kind=verification"):
        _done(_agent(instance), ref)
    with pytest.raises(PersonRequired):
        transition_work(
            _agent(instance),
            TransitionWorkParams(ref=ref, to_state="done", walk=True, reason=WHY),
        )


def test_an_agent_closes_once_verified(instance: AppContext) -> None:
    ref = _item(instance, required=True)
    _verify(instance, ref)
    assert _done(_agent(instance), ref) == "done"


def test_a_person_is_never_refused(instance: AppContext) -> None:
    ref = _item(instance, required=True)
    assert _done(instance, ref) == "done"


def test_no_gate_when_the_policy_is_off_or_the_kind_is_exempt(
    instance: AppContext,
) -> None:
    ref = _item(instance, required=False)
    assert _done(_agent(instance), ref) == "done"
    chore = native_work_item(instance, title="Chore", kind="chore", project="vogt")
    transition_work(
        instance,
        TransitionWorkParams(ref=chore.ref, to_state="review", walk=True, reason=WHY),
    )
    update_project(
        instance,
        UpdateProjectParams(
            slug="vogt",
            agent_policy=AgentPolicy(
                verification=VerificationPolicy(required_for_done=True)
            ),
            reason=WHY,
        ),
    )
    assert _done(_agent(instance), chore.ref) == "done"


def test_work_get_shows_delivery(instance: AppContext) -> None:
    ref = _item(instance, required=True)
    comment_work(
        instance,
        CommentParams(
            ref=ref,
            body="handed off",
            kind="handoff",
            handoff=HandoffFields(branch="wi-1-gate", head_sha="abc"),
            reason=WHY,
        ),
    )
    _verify(instance, ref)
    delivery = get_work(instance, GetWorkParams(ref=ref)).delivery
    assert delivery is not None
    assert delivery.landed is None  # the branch has not been observed
    assert delivery.verified is not None
    assert delivery.verified.lane == "dev"
    assert delivery.verified.by == instance.principal.identity_ref


def _handoff(branch: str) -> Comment:
    return Comment(
        id="c1",
        work_item_id="w1",
        actor_id="a1",
        actor_display_name="worker",
        body="done",
        created_at=datetime(2026, 10, 30, tzinfo=UTC),
        kind="handoff",
        handoff=HandoffFields(branch=branch),
    )


def test_landed_needs_the_branch_observed_with_nothing_ahead() -> None:
    merged = WorkItemBranchView(
        name="wi-1", source="both", tip="f00", ahead=0, behind=3, default_branch="main"
    )
    landed = _landed_of([_handoff("wi-1")], [merged])
    assert landed is not None
    assert landed.sha == "f00"
    assert "main" in landed.evidence
    open_branch = merged.model_copy(update={"ahead": 2})
    assert _landed_of([_handoff("wi-1")], [open_branch]) is None
    assert _landed_of([_handoff("wi-1")], []) is None
