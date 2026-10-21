"""Capability grants: a person-approved posture for a live session (WI-979).

Milestone 2 of `docs/design/oversight-grants.md`. The core records the
request and the decision exactly as for a credential; what reaches the
engine is the capability, which the engine applies by restarting the
session into it (tested in the engine's integration suite).
"""

from __future__ import annotations

import dataclasses
from typing import Any

import pytest

from vogt.adapters.engine.client import EngineClient
from vogt.application.context import AppContext
from vogt.application.models import (
    DecideGrantParams,
    InboxListParams,
    RequestGrantParams,
    RevokeGrantParams,
)
from vogt.application.services import (
    decide_grant,
    list_inbox,
    request_grant,
    revoke_grant,
)
from vogt.errors import Conflict, GrantRefused, InvalidRequest

from tests.test_grants import WHY, GrantEngine, as_agent


@pytest.fixture
def engine() -> GrantEngine:
    return GrantEngine()


@pytest.fixture
def ctx(instance: AppContext, engine: GrantEngine) -> AppContext:
    return dataclasses.replace(
        instance,
        engine=EngineClient(base_url="http://127.0.0.1:8910", transport=engine),
    )


def ask_capability(
    ctx: AppContext,
    capability: str = "bypass",
    target: str = "eng-worker",
    **extra: Any,
) -> str:
    return request_grant(
        ctx,
        RequestGrantParams(
            target=target,
            kind="capability",
            capability=capability,
            reason="the worker runs the e2e suite unattended",
            **extra,
        ),
    ).grant.id


def test_a_person_approves_a_posture_and_the_engine_is_sent_only_that(
    ctx: AppContext, engine: GrantEngine
) -> None:
    grant_id = ask_capability(as_agent(ctx, "eng-overseer"), ttl_seconds=900)
    decided = decide_grant(
        ctx, DecideGrantParams(id=grant_id, decision="approve", reason=WHY)
    ).grant
    assert decided.state == "approved"
    assert decided.kind == "capability" and decided.capability == "bypass"
    assert decided.uses == "ttl", "a posture stands until it expires or is revoked"
    ((target, sent),) = engine.applied
    assert target == "eng-worker"
    assert sent == {
        "grant_id": grant_id,
        "capability": "bypass",
        "uses": "ttl",
        "expires_at": sent["expires_at"],
        "reason": "the worker runs the e2e suite unattended",
    }
    revoke_grant(ctx, RevokeGrantParams(id=grant_id, reason=WHY))
    assert engine.revoked == [("eng-worker", grant_id)]


def test_an_agent_cannot_approve_a_posture(
    ctx: AppContext, engine: GrantEngine
) -> None:
    grant_id = ask_capability(as_agent(ctx, "eng-overseer"))
    with pytest.raises(GrantRefused, match="only a person decides"):
        decide_grant(
            as_agent(ctx, "eng-overseer"),
            DecideGrantParams(id=grant_id, decision="approve", reason=WHY),
        )
    assert engine.applied == []


def test_a_capability_request_is_checked(ctx: AppContext, engine: GrantEngine) -> None:
    with pytest.raises(InvalidRequest, match="capability must be one of"):
        ask_capability(ctx, capability="root")
    with pytest.raises(InvalidRequest, match="not a secret"):
        ask_capability(ctx, secret_name="X", project_id="infra")
    with pytest.raises(Conflict, match="not running"):
        ask_capability(ctx, target="eng-gone")
    engine.sessions["eng-worker"]["permission_mode"] = "accept-edits"
    with pytest.raises(Conflict, match="already runs in accept-edits"):
        ask_capability(ctx, capability="accept_edits")
    with pytest.raises(GrantRefused, match="only an oversight session"):
        ask_capability(as_agent(ctx, "eng-other"))
    # For itself, any session may ask.
    assert ask_capability(as_agent(ctx, "eng-other"), target="eng-other")


def test_a_pending_posture_is_an_inbox_entry_naming_it(ctx: AppContext) -> None:
    grant_id = ask_capability(as_agent(ctx, "eng-overseer"))
    (entry,) = [
        e
        for e in list_inbox(ctx, InboxListParams()).entries
        if e.kind == "session.grant_request"
    ]
    assert entry.action is not None and entry.action.grant_id == grant_id
    assert entry.title == "Grant request: bypass for session eng-worker"
    assert "the bypass permission posture" in entry.summary
    assert "restarts into it once idle" in entry.summary
    assert entry.evidence_snapshot is not None
    assert entry.evidence_snapshot["capability"] == "bypass"
