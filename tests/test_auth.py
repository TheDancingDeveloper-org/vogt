"""Tokens, scopes, and the two gates a write passes (FR-S2–S5, S7)."""

from __future__ import annotations

from datetime import timedelta

import pytest

from vogt.application.context import AppContext
from vogt.application.models import (
    AuthDecisionListParams,
    CreateActorParams,
    IssueTokenParams,
    ListTokensParams,
    RevokeTokenParams,
)
from vogt.application.services import (
    create_actor,
    issue_token,
    list_auth_decisions,
    list_tokens,
    revoke_token,
)
from vogt.application.services.auth import (
    Forbidden,
    Unauthenticated,
    authenticate,
    authorize,
    local,
)
from vogt.core.auth import Grant, hash_token, issue, matches, parse_scopes
from vogt.errors import Conflict, InvalidRequest, NotFound

WHY = "auth test"


@pytest.fixture
def agent(instance: AppContext) -> AppContext:
    create_actor(
        instance,
        CreateActorParams(
            identity_ref="agent:claude-code",
            kind="agent",
            display_name="Claude Code",
            reason=WHY,
        ),
    )
    return instance


def _token(ctx: AppContext, scopes: str = "read", **kwargs: object) -> str:
    result = issue_token(
        ctx,
        IssueTokenParams(
            actor="agent:claude-code",
            name="test",
            scopes=scopes,
            reason=WHY,
            **kwargs,  # type: ignore[arg-type]
        ),
    )
    return result.secret


# -- the primitives --------------------------------------------------------


def test_a_secret_verifies_against_its_hash() -> None:
    credential = issue(("read",))
    assert matches(credential.secret, credential.token_hash)
    assert not matches("vogt_wrong", credential.token_hash)


def test_the_secret_is_not_derivable_from_the_hash() -> None:
    credential = issue(("read",))
    assert credential.secret not in credential.token_hash
    assert hash_token(credential.secret) == credential.token_hash


def test_admin_implies_everything_and_nothing_else_does() -> None:
    admin = Grant(scopes=frozenset({"admin"}))
    assert admin.allows("work.write", mutating=True)[0]
    assert admin.allows("writeback", mutating=True)[0]

    worker = Grant(scopes=frozenset({"work.write"}))
    assert worker.allows("work.write", mutating=True)[0]
    assert worker.allows("read", mutating=False)[0]
    assert not worker.allows("project.write", mutating=True)[0], (
        "filing a bug and registering a project are different powers"
    )


def test_unknown_scopes_are_refused() -> None:
    with pytest.raises(ValueError, match="unknown scope"):
        parse_scopes("read,wat")
    with pytest.raises(ValueError, match="at least one scope"):
        parse_scopes("  ")


# -- issuing ---------------------------------------------------------------


def test_a_token_is_shown_once_and_never_again(agent: AppContext) -> None:
    result = issue_token(
        agent,
        IssueTokenParams(
            actor="agent:claude-code", name="cc", scopes="read", reason=WHY
        ),
    )
    assert result.secret.startswith("vogt_")
    assert "only time" in result.warning

    listed = list_tokens(agent, ListTokensParams())
    assert len(listed.tokens) == 1
    assert result.secret not in listed.tokens[0].model_dump_json()


def test_the_secret_never_reaches_the_audit_trail(agent: AppContext) -> None:
    """An audit row containing the credential is a leak with a timestamp."""
    secret = _token(agent)
    with agent.declared.read() as view:
        record = view.list_audit(limit=1)[0]
    assert record.operation == "token.issue"
    assert secret not in record.model_dump_json()


def test_issuing_for_an_unknown_actor_says_so(instance: AppContext) -> None:
    with pytest.raises(NotFound, match="no actor with identity"):
        issue_token(
            instance,
            IssueTokenParams(actor="agent:ghost", name="x", reason=WHY),
        )


def test_an_unknown_scope_is_an_invalid_request(agent: AppContext) -> None:
    with pytest.raises(InvalidRequest, match="unknown scope"):
        issue_token(
            agent,
            IssueTokenParams(
                actor="agent:claude-code", name="x", scopes="superuser", reason=WHY
            ),
        )


# -- authenticating --------------------------------------------------------


def test_a_valid_token_resolves_to_its_actor(agent: AppContext) -> None:
    secret = _token(agent, "work.write")
    caller = authenticate(agent, bearer=secret)
    assert caller.principal.identity_ref == "agent:claude-code"
    assert caller.principal.kind == "agent"
    assert caller.grant.allows("work.write", mutating=True)[0]


def test_no_credential_is_refused(agent: AppContext) -> None:
    with pytest.raises(Unauthenticated, match="no bearer token"):
        authenticate(agent, bearer=None)


@pytest.mark.parametrize("bearer", ["vogt_nonsense", "", "   "])
def test_an_invalid_token_is_refused_without_explaining_why(
    agent: AppContext, bearer: str
) -> None:
    """The holder learns nothing; the operator sees the reason in the log."""
    with pytest.raises(Unauthenticated, match="not valid"):
        authenticate(agent, bearer=bearer)


def test_a_revoked_token_stops_working(agent: AppContext) -> None:
    secret = _token(agent)
    token_id = list_tokens(agent, ListTokensParams()).tokens[0].id
    revoke_token(agent, RevokeTokenParams(id=token_id, reason="rotated"))

    with pytest.raises(Unauthenticated):
        authenticate(agent, bearer=secret)
    assert list_tokens(agent, ListTokensParams()).tokens == []
    assert list_tokens(agent, ListTokensParams(include_revoked=True)).tokens


def test_revoking_twice_is_a_conflict(agent: AppContext) -> None:
    _token(agent)
    token_id = list_tokens(agent, ListTokensParams()).tokens[0].id
    revoke_token(agent, RevokeTokenParams(id=token_id, reason="once"))
    with pytest.raises(Conflict, match="already revoked"):
        revoke_token(agent, RevokeTokenParams(id=token_id, reason="twice"))


def test_an_expired_token_stops_working(agent: AppContext) -> None:
    secret = _token(agent, "read", expires_in_days=1)
    authenticate(agent, bearer=secret)  # still fine now

    future = agent.clock() + timedelta(days=2)
    aged = AppContext(
        config=agent.config,
        declared=agent.declared,
        observed=agent.observed,
        principal=agent.principal,
        clock=lambda: future,
        id_factory=agent.id_factory,
    )
    with pytest.raises(Unauthenticated):
        authenticate(aged, bearer=secret)


def test_using_a_token_records_when(agent: AppContext) -> None:
    secret = _token(agent)
    assert list_tokens(agent, ListTokensParams()).tokens[0].last_used_at is None
    authenticate(agent, bearer=secret)
    assert list_tokens(agent, ListTokensParams()).tokens[0].last_used_at is not None


def test_last_used_write_is_debounced(agent: AppContext) -> None:
    """#526: the last-used write is skipped within the debounce window.

    The first use records it; a second use moments later must not open another
    write transaction — the stored instant stays put, but no write is paid.
    """
    from vogt.application.services.auth import _TOUCH_DEBOUNCE

    secret = _token(agent)
    authenticate(agent, bearer=secret)
    first = list_tokens(agent, ListTokensParams()).tokens[0].last_used_at
    assert first is not None
    # Same context/clock (advances by a step, well under the 5-minute window).
    authenticate(agent, bearer=secret)
    second = list_tokens(agent, ListTokensParams()).tokens[0].last_used_at
    assert second == first, "a repeat use within the window does not re-touch"
    assert _TOUCH_DEBOUNCE.total_seconds() > 0


def test_prune_auth_decisions_caps_allows_and_keeps_denies_longer(
    agent: AppContext,
) -> None:
    """#526: allows are pruned at the horizon, denies kept longer."""
    from datetime import UTC, datetime

    from vogt.core.entities import AuthDecision

    def row(decision: str, at: datetime) -> None:
        agent.declared.record_auth_decision(
            AuthDecision(
                id=agent.id_factory("aut"),
                at=at,
                decision=decision,  # type: ignore[arg-type]
                reason_code="ok",
                operation="work.list",
                scope="read",
                actor_id=None,
                token_id=None,
                identity_ref="agent:x",
                transport="http",
            )
        )

    old = datetime(2020, 1, 1, tzinfo=UTC)
    recent = datetime(2020, 6, 1, tzinfo=UTC)
    row("allow", old)
    row("deny", old)
    row("allow", recent)
    before = len(list_auth_decisions(agent, AuthDecisionListParams()).decisions)
    removed = agent.declared.prune_auth_decisions(
        allow_before=datetime(2020, 3, 1, tzinfo=UTC),
        deny_before=datetime(2019, 1, 1, tzinfo=UTC),
    )
    assert removed == 1  # only the old allow
    after = list_auth_decisions(agent, AuthDecisionListParams()).decisions
    assert len(after) == before - 1
    assert all(not (d.decision == "allow" and d.at == old) for d in after), (
        "the old allow is gone"
    )
    assert any(d.decision == "deny" for d in after), "denies are kept longer"


# -- the two gates ---------------------------------------------------------


def test_a_missing_scope_is_refused_and_says_what_is_needed(
    agent: AppContext,
) -> None:
    caller = authenticate(agent, bearer=_token(agent, "read"))
    with pytest.raises(Forbidden, match=r"requires the 'work\.write' scope"):
        authorize(
            agent,
            caller,
            operation="work.create",
            scope="work.write",
            mutating=True,
            transport="http",
        )


def test_the_writeback_scope_gates_arming_write_back(agent: AppContext) -> None:
    """FR-S11, in both directions, because one of them is the whole point.

    `writeback` gated nothing until r13: it parsed, it implied `read`, it was
    issuable, and no operation asked for it — so a token issued with it in good
    faith could only read, and the control it appeared to grant did not exist.
    It now gates `forge.writeback`, the operation that arms a project's
    upstream pushing.

    The negative half is what makes this worth writing. A test that only
    checked the grant would pass with the scope back on `project.write` and
    `writeback` meaningless again, which is the state this replaces.
    """
    armed = authenticate(agent, bearer=_token(agent, "writeback"))
    authorize(
        agent,
        armed,
        operation="forge.writeback",
        scope="writeback",
        mutating=True,
        transport="http",
    )

    projects_only = authenticate(agent, bearer=_token(agent, "project.write"))
    with pytest.raises(Forbidden, match=r"requires the 'writeback' scope"):
        authorize(
            agent,
            projects_only,
            operation="forge.writeback",
            scope="writeback",
            mutating=True,
            transport="http",
        )


def test_every_scope_this_instance_issues_gates_something(agent: AppContext) -> None:
    """A scope that grants nothing is worse than one that does not exist.

    It is issued in good faith, appears on a token listing, and silently
    confers only `read`. `writeback` was in exactly that state from M5 to r13,
    and nothing could have caught it: every parse test passed, because parsing
    was all it did.
    """
    del agent
    from vogt.core.auth import ALL_SCOPES
    from vogt.registry import default_registry

    gated = {operation.scope for operation in default_registry()}
    ungated = [scope for scope in ALL_SCOPES if scope not in gated]
    assert not ungated, (
        f"{ungated} gate no operation, so a token holding one is granted "
        "nothing it does not already have"
    )


def test_a_read_only_server_refuses_every_write(agent: AppContext) -> None:
    """The first gate: no token can grant a write the process refuses."""
    caller = authenticate(agent, bearer=_token(agent, "admin"), writes_enabled=False)
    with pytest.raises(Forbidden, match="started read-only"):
        authorize(
            agent,
            caller,
            operation="work.create",
            scope="work.write",
            mutating=True,
            transport="http",
        )
    authorize(
        agent,
        caller,
        operation="backlog",
        scope="read",
        mutating=False,
        transport="http",
    )


def test_both_allow_and_deny_are_recorded(agent: AppContext) -> None:
    """FR-S5. The denials are the interesting half."""
    caller = authenticate(agent, bearer=_token(agent, "read"))
    authorize(
        agent,
        caller,
        operation="backlog",
        scope="read",
        mutating=False,
        transport="http",
    )
    with pytest.raises(Forbidden):
        authorize(
            agent,
            caller,
            operation="work.create",
            scope="work.write",
            mutating=True,
            transport="http",
        )

    allows = list_auth_decisions(agent, AuthDecisionListParams(decision="allow"))
    denies = list_auth_decisions(agent, AuthDecisionListParams(decision="deny"))
    assert any(d.operation == "backlog" for d in allows.decisions)
    assert any(d.operation == "work.create" for d in denies.decisions)
    assert denies.decisions[0].reason_code == "missing_scope"
    assert denies.decisions[0].identity_ref == "agent:claude-code"


def test_a_rejected_token_is_recorded_too(agent: AppContext) -> None:
    with pytest.raises(Unauthenticated):
        authenticate(agent, bearer="vogt_not_a_token")
    denies = list_auth_decisions(agent, AuthDecisionListParams(decision="deny"))
    assert denies.decisions[0].reason_code == "unknown_token"


def test_the_local_path_is_unauthenticated_and_says_so(instance: AppContext) -> None:
    """DEPLOYMENT §3: loopback has no authentication by design."""
    caller = local(instance)
    assert caller.principal.identity_ref == "local:test-user"
    assert caller.token is None
    assert caller.grant.allows("admin", mutating=True)[0]


def test_issuing_a_token_for_another_actor_still_audits_the_caller(
    agent: AppContext,
) -> None:
    """FR-S2: naming a subject is not the same as claiming to be one.

    `token.issue --actor agent:claude-code` binds the token to that agent.
    The audit row still records who *did the issuing*, which is the property
    that makes provenance mean anything.
    """
    issue_token(
        agent,
        IssueTokenParams(actor="agent:claude-code", name="for-the-agent", reason=WHY),
    )
    with agent.declared.read() as view:
        record = view.list_audit(limit=1)[0]
    assert record.operation == "token.issue"
    assert record.actor_identity_ref == "local:test-user", (
        "the acting principal, not the actor the token was issued for"
    )
