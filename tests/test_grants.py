"""Person-approved grants to a live session (WI-973).

Each test names the invariant from `docs/design/oversight-grants.md` it holds
up. The engine is a stand-in that records what it was sent, because the
property under test is *what reaches the engine, and when* — a grant must
reach it only after a person approved it, and never for an agent's say-so.
"""

from __future__ import annotations

import dataclasses
import json
from datetime import timedelta
from typing import Any

import pytest

from vogt.adapters.engine import EngineClient
from vogt.application.context import AppContext
from vogt.application.models import (
    DecideGrantParams,
    InboxListParams,
    ListAuditParams,
    ListGrantsParams,
    RequestGrantParams,
    RevokeGrantParams,
    SetSessionRoleParams,
)
from vogt.application.services import (
    decide_grant,
    list_audit,
    list_grants,
    list_inbox,
    request_grant,
    revoke_grant,
    set_session_role,
)
from vogt.application.services import grants as grants_service
from vogt.application.services.grants import default_var
from vogt.core.principal import Principal
from vogt.errors import Conflict, GrantRefused, InvalidRequest, RoleRefused

WHY = "grant test"
SECRET = "100.109.218.11_SSH"


class GrantEngine:
    """Sessions with a role, and the grants the engine was handed."""

    def __init__(self) -> None:
        self.sessions: dict[str, dict[str, Any]] = {
            "eng-worker": {"alive": True, "role": "worker"},
            "eng-other": {"alive": True, "role": "worker"},
            "eng-overseer": {"alive": True, "role": "oversight"},
            "eng-gone": {"alive": False, "role": "worker"},
        }
        self.applied: list[tuple[str, dict[str, Any]]] = []
        self.revoked: list[tuple[str, str]] = []
        #: Projects the engine refuses, as it refuses one not open to grants.
        self.closed_projects: set[str] = set()

    def summary(self, engine_id: str) -> dict[str, Any]:
        state = self.sessions[engine_id]
        return {
            "id": engine_id,
            "name": engine_id,
            "activity": "idle" if state["alive"] else "exited",
            "alive": state["alive"],
            "exit_code": None if state["alive"] else 0,
            "role": state["role"],
            "cwd": "/srv",
        }

    def __call__(
        self, url: str, headers: dict[str, str], body: bytes = b"", method: str = "GET"
    ) -> tuple[int, bytes]:
        del headers
        payload = json.loads(body.decode()) if body else {}
        path = url.split("?", 1)[0].split("8910", 1)[1]
        parts = path.strip("/").split("/")
        if method == "GET" and path == "/api/sessions":
            return 200, json.dumps([self.summary(i) for i in self.sessions]).encode()
        if parts[:2] == ["api", "sessions"] and len(parts) >= 3:
            engine_id = parts[2]
            if engine_id not in self.sessions:
                return 404, b""
            if method == "GET" and len(parts) == 3:
                return 200, json.dumps({"summary": self.summary(engine_id)}).encode()
            if method == "POST" and parts[3:] == ["grants"]:
                if payload["project_id"] in self.closed_projects:
                    return 403, json.dumps(
                        {"error": "forbidden: project is not open to grants"}
                    ).encode()
                self.applied.append((engine_id, payload))
                return 200, json.dumps(payload).encode()
            if method == "DELETE" and parts[3:4] == ["grants"]:
                self.revoked.append((engine_id, parts[4]))
                return 200, json.dumps({"revoked": True}).encode()
        return 404, b""


@pytest.fixture
def engine() -> GrantEngine:
    return GrantEngine()


@pytest.fixture
def ctx(instance: AppContext, engine: GrantEngine) -> AppContext:
    return dataclasses.replace(
        instance,
        engine=EngineClient(base_url="http://127.0.0.1:8910", transport=engine),
    )


def as_agent(ctx: AppContext, engine_id: str) -> AppContext:
    """The context of an agent running in engine session `engine_id`."""
    return dataclasses.replace(
        ctx,
        principal=Principal(
            identity_ref=f"agent:engine:{engine_id}",
            kind="agent",
            display_name=engine_id,
        ),
    )


def ask(ctx: AppContext, target: str = "eng-worker", **extra: Any) -> str:
    fields: dict[str, Any] = {
        "target": target,
        "secret_name": SECRET,
        "project_id": "infra",
        "reason": "the worker needs the emulator key",
        **extra,
    }
    return request_grant(ctx, RequestGrantParams(**fields)).grant.id


# -- invariant 1: a person decides, an agent cannot -------------------------


def test_an_agent_cannot_decide_a_grant_even_the_overseer(
    ctx: AppContext, engine: GrantEngine
) -> None:
    overseer = as_agent(ctx, "eng-overseer")
    grant_id = ask(overseer)
    for agent in (overseer, as_agent(ctx, "eng-worker")):
        with pytest.raises(GrantRefused, match="only a person decides"):
            decide_grant(
                agent, DecideGrantParams(id=grant_id, decision="approve", reason=WHY)
            )
    assert engine.applied == [], "nothing reached the engine"
    (row,) = list_grants(ctx, ListGrantsParams()).grants
    assert row.state == "pending"


def test_a_person_approves_and_the_engine_holds_it_before_the_record_says_so(
    ctx: AppContext, engine: GrantEngine
) -> None:
    grant_id = ask(as_agent(ctx, "eng-overseer"), uses="ttl", ttl_seconds=600)
    decided = decide_grant(
        ctx, DecideGrantParams(id=grant_id, decision="approve", reason=WHY)
    ).grant
    assert decided.state == "approved"
    assert decided.decided_by == ctx.principal.identity_ref
    assert decided.expires_at is not None
    assert decided.expires_at - decided.decided_at == timedelta(seconds=600)  # type: ignore[operator]
    ((target, sent),) = engine.applied
    assert target == "eng-worker"
    assert sent == {
        "grant_id": grant_id,
        "var": default_var(SECRET),
        "project_id": "infra",
        "secret_name": SECRET,
        "uses": "ttl",
        "expires_at": sent["expires_at"],
        # The purpose the person approved rides to the session (F5).
        "reason": "the worker needs the emulator key",
    }
    assert "value" not in json.dumps(sent).lower()
    # Decided once: a second decision is a conflict, not a second grant.
    with pytest.raises(Conflict):
        decide_grant(ctx, DecideGrantParams(id=grant_id, decision="deny", reason=WHY))


def test_an_engine_refusal_leaves_the_request_pending_with_its_reason(
    ctx: AppContext, engine: GrantEngine
) -> None:
    engine.closed_projects.add("infra")
    grant_id = ask(as_agent(ctx, "eng-overseer"))
    with pytest.raises(GrantRefused, match="not open to grants"):
        decide_grant(
            ctx, DecideGrantParams(id=grant_id, decision="approve", reason=WHY)
        )
    (row,) = list_grants(ctx, ListGrantsParams()).grants
    assert row.state == "pending"
    denied = decide_grant(
        ctx, DecideGrantParams(id=grant_id, decision="deny", reason=WHY)
    ).grant
    assert denied.state == "denied"
    assert engine.applied == []


# -- invariant 3: no laundering ----------------------------------------------


def test_only_an_overseer_asks_on_another_sessions_behalf(
    ctx: AppContext, engine: GrantEngine
) -> None:
    with pytest.raises(GrantRefused, match="only an oversight session"):
        ask(as_agent(ctx, "eng-other"), target="eng-worker")
    # Its own session is fine; so is an overseer for a worker, and a person.
    ask(as_agent(ctx, "eng-worker"), target="eng-worker")
    ask(as_agent(ctx, "eng-overseer"), target="eng-worker")
    ask(ctx, target="eng-other")
    # An agent that is not a session at all (the pod's token) cannot ask.
    pod = dataclasses.replace(
        ctx,
        principal=Principal(
            identity_ref="agent:pod:vogt", kind="agent", display_name="pod"
        ),
    )
    with pytest.raises(GrantRefused, match="not a session"):
        ask(pod)
    assert engine.applied == []


def test_the_requester_is_the_principal_not_a_parameter(ctx: AppContext) -> None:
    ask(as_agent(ctx, "eng-overseer"))
    (row,) = list_grants(ctx, ListGrantsParams()).grants
    assert row.requested_by == "agent:engine:eng-overseer"


def test_an_agent_cannot_make_itself_an_overseer(ctx: AppContext) -> None:
    """The overseer rule would be self-service if a session could set its own
    role; the role is a person's nomination (F3)."""
    for who in ("eng-other", "eng-worker"):
        with pytest.raises(RoleRefused, match="only a person nominates"):
            set_session_role(
                as_agent(ctx, who),
                SetSessionRoleParams(id=who, role="oversight", reason=WHY),
            )
    with pytest.raises(GrantRefused, match="only an oversight session"):
        ask(as_agent(ctx, "eng-other"), target="eng-worker")


def test_an_agent_lists_only_its_own_sessions_grants(ctx: AppContext) -> None:
    """Which secrets another session was granted is not every agent's to read
    (F8): an agent sees its own session's grants, a person sees all."""
    mine = ask(as_agent(ctx, "eng-worker"), target="eng-worker")
    ask(ctx, target="eng-other")
    assert len(list_grants(ctx, ListGrantsParams()).grants) == 2
    seen = list_grants(as_agent(ctx, "eng-worker"), ListGrantsParams()).grants
    assert [g.id for g in seen] == [mine]
    other = list_grants(
        as_agent(ctx, "eng-worker"), ListGrantsParams(target="eng-other")
    ).grants
    assert other == []
    pod = dataclasses.replace(
        ctx,
        principal=Principal(
            identity_ref="agent:pod:vogt", kind="agent", display_name="pod"
        ),
    )
    assert list_grants(pod, ListGrantsParams()).grants == []


def test_a_failed_approval_takes_the_grant_back_unless_another_person_approved(
    ctx: AppContext, engine: GrantEngine, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The engine never holds a grant the record does not stand behind (F6):
    any failure after the apply revokes it — except another person's approval
    of the same grant landing first, which stands."""
    grant_id = ask(as_agent(ctx, "eng-overseer"))
    real = grants_service._record_decision

    def broken(*args: Any, **kwargs: Any) -> Any:
        raise RuntimeError("the record could not be written")

    monkeypatch.setattr(grants_service, "_record_decision", broken)
    with pytest.raises(RuntimeError):
        decide_grant(
            ctx, DecideGrantParams(id=grant_id, decision="approve", reason=WHY)
        )
    assert engine.revoked == [("eng-worker", grant_id)]
    (row,) = list_grants(ctx, ListGrantsParams()).grants
    assert row.state == "pending"

    # Two people approve at once: the second's record is a conflict, but the
    # first's approval stands at the engine.
    engine.revoked.clear()
    calls = {"n": 0}

    def raced(*args: Any, **kwargs: Any) -> Any:
        calls["n"] += 1
        if calls["n"] == 1:
            real(*args, **kwargs)  # the other person's write lands first
            raise Conflict(f"grant {grant_id} is approved, not pending")
        return real(*args, **kwargs)

    monkeypatch.setattr(grants_service, "_record_decision", raced)
    with pytest.raises(Conflict):
        decide_grant(
            ctx, DecideGrantParams(id=grant_id, decision="approve", reason=WHY)
        )
    assert engine.revoked == [], "the other person's approval was not taken back"
    (row,) = list_grants(ctx, ListGrantsParams()).grants
    assert row.state == "approved"


# -- invariant 4: least privilege ---------------------------------------------


def test_a_grant_is_one_named_credential_for_a_live_session(ctx: AppContext) -> None:
    with pytest.raises(InvalidRequest, match="capability grants are not available"):
        ask(ctx, kind="capability", capability="bypass")
    with pytest.raises(InvalidRequest, match="names secret_name and project_id"):
        ask(ctx, project_id=None)
    with pytest.raises(InvalidRequest, match="plain names"):
        ask(ctx, secret_name="--all")
    with pytest.raises(InvalidRequest, match="environment variable"):
        ask(ctx, var="not a var")
    with pytest.raises(Conflict, match="not running"):
        ask(ctx, target="eng-gone")
    assert default_var(SECRET) == "GRANT_100_109_218_11_SSH"


# -- invariants 5 and 6: time-boxed and revocable -----------------------------


def test_an_approved_grant_reads_expired_after_its_ttl(ctx: AppContext) -> None:
    grant_id = ask(ctx, ttl_seconds=60)
    decide_grant(ctx, DecideGrantParams(id=grant_id, decision="approve", reason=WHY))
    later = ctx.clock() + timedelta(minutes=2)
    after = dataclasses.replace(ctx, clock=lambda: later)
    (row,) = list_grants(after, ListGrantsParams(state="expired")).grants
    assert row.id == grant_id
    assert list_grants(after, ListGrantsParams(state="approved")).grants == []


def test_revoking_reaches_the_engine_and_only_a_person_or_the_asker_may(
    ctx: AppContext, engine: GrantEngine
) -> None:
    overseer = as_agent(ctx, "eng-overseer")
    grant_id = ask(overseer, uses="ttl")
    decide_grant(ctx, DecideGrantParams(id=grant_id, decision="approve", reason=WHY))
    with pytest.raises(GrantRefused, match="only a person, or the session that asked"):
        revoke_grant(
            as_agent(ctx, "eng-worker"), RevokeGrantParams(id=grant_id, reason=WHY)
        )
    revoked = revoke_grant(overseer, RevokeGrantParams(id=grant_id, reason=WHY)).grant
    assert revoked.state == "revoked"
    assert engine.revoked == [("eng-worker", grant_id)]
    # A pending one is withdrawn without troubling the engine.
    pending = ask(ctx)
    revoke_grant(ctx, RevokeGrantParams(id=pending, reason=WHY))
    assert engine.revoked == [("eng-worker", grant_id)]


# -- invariant 8: audited by name ----------------------------------------------


def test_every_step_is_audited_by_name(ctx: AppContext) -> None:
    grant_id = ask(as_agent(ctx, "eng-overseer"))
    decide_grant(ctx, DecideGrantParams(id=grant_id, decision="approve", reason=WHY))
    revoke_grant(ctx, RevokeGrantParams(id=grant_id, reason=WHY))
    operations = [
        row.operation
        for row in list_audit(
            ctx, ListAuditParams(entity_id=grant_id, limit=10)
        ).records
    ]
    assert sorted(operations) == [
        "session.grant_decide",
        "session.grant_request",
        "session.grant_revoke",
    ]


# -- the Inbox -------------------------------------------------------------------


def test_a_pending_grant_is_an_inbox_entry_until_it_is_decided(ctx: AppContext) -> None:
    grant_id = ask(as_agent(ctx, "eng-overseer"))
    entries = [
        e
        for e in list_inbox(ctx, InboxListParams()).entries
        if e.kind == "session.grant_request"
    ]
    assert len(entries) == 1
    entry = entries[0]
    assert entry.action is not None and entry.action.kind == "grant"
    assert entry.action.grant_id == grant_id
    assert SECRET in entry.title
    assert "agent:engine:eng-overseer" in entry.summary
    assert "infra" in entry.summary
    # The approver sees the target as they know it, and who is asking for
    # whom (F4): the names come from the engine, not the request.
    assert "for session eng-worker" in entry.title
    assert "for session eng-worker (worker)" in entry.summary
    assert entry.actor_login == "agent:engine:eng-overseer"
    assert entry.actor_kind == "bot"
    assert entry.evidence_snapshot is not None
    assert entry.evidence_snapshot["target_role"] == "worker"
    assert entry.evidence_snapshot["requester_is_target"] is False
    own = ask(as_agent(ctx, "eng-other"), target="eng-other")
    (self_entry,) = [
        e
        for e in list_inbox(ctx, InboxListParams()).entries
        if e.kind == "session.grant_request"
        and e.action is not None
        and e.action.grant_id == own
    ]
    assert "asks for" in self_entry.summary and "for itself" in self_entry.summary
    assert self_entry.evidence_snapshot is not None
    assert self_entry.evidence_snapshot["requester_is_target"] is True
    decide_grant(ctx, DecideGrantParams(id=own, decision="deny", reason=WHY))
    decide_grant(ctx, DecideGrantParams(id=grant_id, decision="deny", reason=WHY))
    assert not [
        e
        for e in list_inbox(ctx, InboxListParams()).entries
        if e.kind == "session.grant_request"
    ]
