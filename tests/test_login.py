"""Password logins: the human credential.

What these tests pin down: a person signs in with a username and password
and gets a *session* — a token bound to their own actor, carrying the scopes
on their login, expiring on its own, revoked by `auth.logout`. Authentication
does not know the difference between a session and any other token, which
is the point. A wrong password and an unknown user are one message to the
caller and two reason codes for the operator, and a username that keeps
failing is throttled before its password is even checked.
"""

from __future__ import annotations

from collections.abc import Iterator
from datetime import timedelta

import pytest
from fastapi.testclient import TestClient

from vogt.adapters.http.server import ServeOptions, build_server
from vogt.application.context import AppContext
from vogt.application.models import (
    AuthDecisionListParams,
    CreateUserParams,
    InstallBootstrapParams,
    LoginParams,
    LogoutParams,
    RemoveUserParams,
    SetPasswordParams,
    UserListParams,
    WhoamiParams,
)
from vogt.application.services import (
    create_user,
    install_bootstrap,
    list_auth_decisions,
    list_users,
    login,
    logout,
    remove_user,
    set_password,
    whoami,
)
from vogt.application.services.auth import (
    LOGIN_FAILURE_LIMIT,
    Unauthenticated,
    _throttle,
    authenticate,
)
from vogt.core.auth import (
    AuthDecisionCode,
    hash_password,
    normalise_username,
    verify_password,
)
from vogt.errors import Conflict, InvalidRequest, LoginThrottled, NotFound

WHY = "login test"
PASSWORD = "correct horse battery staple"


@pytest.fixture(autouse=True)
def _fresh_throttle() -> Iterator[None]:
    """The throttle is process-local; a test must not inherit another's failures."""
    _throttle._failures.clear()
    yield
    _throttle._failures.clear()


@pytest.fixture
def ada(instance: AppContext) -> str:
    create_user(
        instance,
        CreateUserParams(
            username="Ada",
            password=PASSWORD,
            display_name="Ada Lovelace",
            scopes="read,work.write",
            reason=WHY,
        ),
    )
    return "ada"


# -- the primitives ----------------------------------------------------------


def test_a_password_verifies_against_its_hash_and_nothing_else_does() -> None:
    stored = hash_password(PASSWORD)
    assert stored.startswith("scrypt$")
    assert verify_password(PASSWORD, stored)
    assert not verify_password(PASSWORD + "x", stored)
    assert not verify_password(PASSWORD, "garbage")


def test_two_hashes_of_one_password_differ() -> None:
    """Salted: an operator cannot tell two people share a password."""
    assert hash_password(PASSWORD) != hash_password(PASSWORD)


def test_a_short_password_is_refused() -> None:
    with pytest.raises(ValueError, match="at least 8"):
        hash_password("short")


def test_usernames_are_folded_and_validated() -> None:
    assert normalise_username("  Ada.Lovelace ") == "ada.lovelace"
    for bad in ("a", "-ada", "ada lovelace", "ada@example", "x" * 65):
        with pytest.raises(ValueError):
            normalise_username(bad)


# -- users -----------------------------------------------------------------


def test_creating_a_user_creates_a_human_actor_and_a_login(
    instance: AppContext, ada: str
) -> None:
    users = list_users(instance, UserListParams()).users
    assert [u.username for u in users] == [ada]
    assert users[0].actor_identity_ref == "human:ada"
    assert users[0].scopes == ["read", "work.write"]
    with instance.declared.read() as view:
        actor = view.actor_by_identity("human:ada")
        assert actor is not None
        assert actor.kind == "human"
        assert actor.display_name == "Ada Lovelace"


def test_a_user_result_never_carries_the_hash(instance: AppContext, ada: str) -> None:
    users = list_users(instance, UserListParams()).users
    assert "password" not in users[0].model_dump_json()
    assert "hash" not in users[0].model_dump_json()


def test_a_duplicate_username_is_a_conflict(instance: AppContext, ada: str) -> None:
    with pytest.raises(Conflict, match="already exists"):
        create_user(
            instance,
            CreateUserParams(username="ADA", password=PASSWORD, reason=WHY),
        )


def test_a_bad_username_or_scope_is_an_invalid_request(instance: AppContext) -> None:
    with pytest.raises(InvalidRequest):
        create_user(
            instance, CreateUserParams(username="a", password=PASSWORD, reason=WHY)
        )
    with pytest.raises(InvalidRequest, match="unknown scope"):
        create_user(
            instance,
            CreateUserParams(
                username="ada", password=PASSWORD, scopes="root", reason=WHY
            ),
        )
    with pytest.raises(InvalidRequest, match="at least 8"):
        create_user(
            instance, CreateUserParams(username="ada", password="short", reason=WHY)
        )


def test_an_agent_cannot_hold_a_password(instance: AppContext) -> None:
    from vogt.application.models import CreateActorParams
    from vogt.application.services import create_actor

    create_actor(
        instance,
        CreateActorParams(
            identity_ref="agent:bot", kind="agent", display_name="bot", reason=WHY
        ),
    )
    with pytest.raises(InvalidRequest, match="agent"):
        create_user(
            instance,
            CreateUserParams(
                username="bot", password=PASSWORD, actor="agent:bot", reason=WHY
            ),
        )


# -- login -----------------------------------------------------------------


def test_a_login_mints_a_session_bound_to_the_person(
    instance: AppContext, ada: str
) -> None:
    result = login(instance, LoginParams(username="Ada", password=PASSWORD))
    assert result.actor.identity_ref == "human:ada"
    assert result.token.kind == "session"
    assert result.token.scopes == ["read", "work.write"]
    assert result.token.expires_at is not None
    assert result.token.expires_at - result.token.created_at == timedelta(
        days=instance.config.session_ttl_days
    )
    caller = authenticate(instance, bearer=result.secret)
    assert caller.principal.identity_ref == "human:ada"
    assert caller.token is not None
    assert caller.token.kind == "session"


def test_the_login_is_audited_to_the_person_without_the_secret(
    instance: AppContext, ada: str
) -> None:
    result = login(instance, LoginParams(username=ada, password=PASSWORD))
    with instance.declared.read() as view:
        rows = view.list_audit(limit=5)
    row = rows[0]
    assert row.operation == "auth.login"
    assert row.actor_identity_ref == "human:ada"
    assert result.secret not in row.model_dump_json()
    decisions = list_auth_decisions(instance, AuthDecisionListParams()).decisions
    assert decisions[0].reason_code == AuthDecisionCode.LOGIN_OK
    assert decisions[0].operation == "auth.login"


def test_a_wrong_password_and_an_unknown_user_read_the_same(
    instance: AppContext, ada: str
) -> None:
    with pytest.raises(Unauthenticated) as wrong:
        login(instance, LoginParams(username=ada, password="not it at all"))
    with pytest.raises(Unauthenticated) as unknown:
        login(instance, LoginParams(username="nobody", password=PASSWORD))
    assert str(wrong.value) == str(unknown.value)
    denials = list_auth_decisions(
        instance, AuthDecisionListParams(decision="deny")
    ).decisions
    assert [d.reason_code for d in denials[:2]] == [
        AuthDecisionCode.BAD_PASSWORD,
        AuthDecisionCode.BAD_PASSWORD,
    ]


def test_a_username_that_keeps_failing_is_throttled(
    instance: AppContext, ada: str
) -> None:
    for _ in range(LOGIN_FAILURE_LIMIT):
        with pytest.raises(Unauthenticated):
            login(instance, LoginParams(username=ada, password="wrong"))
    # Even the right password is refused now, and the refusal is a different
    # answer from "wrong password" so a client backs off rather than retries.
    with pytest.raises(LoginThrottled, match="try again"):
        login(instance, LoginParams(username=ada, password=PASSWORD))
    # Another user is unaffected: the window is per username.
    create_user(
        instance, CreateUserParams(username="grace", password=PASSWORD, reason=WHY)
    )
    grace = login(instance, LoginParams(username="grace", password=PASSWORD))
    assert grace.token.kind == "session"


def test_a_disabled_actor_cannot_log_in(instance: AppContext, ada: str) -> None:
    with instance.declared.write() as txn:
        txn._conn.execute(  # type: ignore[attr-defined]
            "UPDATE actors SET disabled = 1 WHERE identity_ref = 'human:ada'"
        )
    with pytest.raises(Unauthenticated):
        login(instance, LoginParams(username=ada, password=PASSWORD))


# -- logout and whoami ------------------------------------------------------


def _as(instance: AppContext, secret: str) -> AppContext:
    from dataclasses import replace

    caller = authenticate(instance, bearer=secret)
    return replace(instance, principal=caller.principal, token=caller.token)


def test_logout_revokes_the_session_it_arrived_with(
    instance: AppContext, ada: str
) -> None:
    session = login(instance, LoginParams(username=ada, password=PASSWORD))
    other = login(instance, LoginParams(username=ada, password=PASSWORD))
    result = logout(_as(instance, session.secret), LogoutParams(reason="bye"))
    assert result.revoked is True
    assert result.token is not None and result.token.revoked_at is not None
    with pytest.raises(Unauthenticated):
        authenticate(instance, bearer=session.secret)
    # The other device's session is untouched.
    assert authenticate(instance, bearer=other.secret).principal.identity_ref == (
        "human:ada"
    )


def test_logout_with_no_token_behind_it_is_an_honest_no(instance: AppContext) -> None:
    result = logout(instance, LogoutParams(reason="bye"))
    assert result.revoked is False
    assert result.token is None


def test_whoami_reports_the_authenticated_identity_and_effective_scopes(
    instance: AppContext, ada: str
) -> None:
    session = login(instance, LoginParams(username=ada, password=PASSWORD))
    me = whoami(_as(instance, session.secret), WhoamiParams())
    assert me.identity_ref == "human:ada"
    assert me.kind == "human"
    assert me.display_name == "Ada Lovelace"
    assert me.scopes == ["read", "work.write"]
    assert me.token is not None and me.token.id == session.token.id
    local = whoami(instance, WhoamiParams())
    assert local.identity_ref == instance.principal.identity_ref
    assert "admin" in local.scopes
    assert local.token is None


# -- password changes and removal --------------------------------------------


def test_setting_a_password_ends_every_session_and_takes_effect_at_once(
    instance: AppContext, ada: str
) -> None:
    session = login(instance, LoginParams(username=ada, password=PASSWORD))
    set_password(
        instance,
        SetPasswordParams(username=ada, password="a new passphrase", reason=WHY),
    )
    with pytest.raises(Unauthenticated):
        authenticate(instance, bearer=session.secret)
    with pytest.raises(Unauthenticated):
        login(instance, LoginParams(username=ada, password=PASSWORD))
    renewed = login(instance, LoginParams(username=ada, password="a new passphrase"))
    assert renewed.actor.identity_ref == "human:ada"


def test_setting_a_password_can_keep_sessions_and_change_scopes(
    instance: AppContext, ada: str
) -> None:
    session = login(instance, LoginParams(username=ada, password=PASSWORD))
    result = set_password(
        instance,
        SetPasswordParams(
            username=ada,
            password="a new passphrase",
            scopes="admin",
            revoke_sessions=False,
            reason=WHY,
        ),
    )
    assert result.user.scopes == ["admin"]
    assert authenticate(instance, bearer=session.secret).token
    fresh = login(instance, LoginParams(username=ada, password="a new passphrase"))
    assert fresh.token.scopes == ["admin"]


def test_removing_a_user_revokes_sessions_and_keeps_the_actor(
    instance: AppContext, ada: str
) -> None:
    session = login(instance, LoginParams(username=ada, password=PASSWORD))
    result = remove_user(instance, RemoveUserParams(username=ada, reason=WHY))
    assert result.sessions_revoked == 1
    assert list_users(instance, UserListParams()).users == []
    with pytest.raises(Unauthenticated):
        authenticate(instance, bearer=session.secret)
    with instance.declared.read() as view:
        assert view.actor_by_identity("human:ada") is not None
    with pytest.raises(NotFound):
        remove_user(instance, RemoveUserParams(username=ada, reason=WHY))


# -- the install bootstrap with a password -----------------------------------


def test_the_bootstrap_can_create_a_login_and_hands_back_a_session(
    instance: AppContext,
) -> None:
    result = install_bootstrap(
        instance,
        InstallBootstrapParams(display_name="Ada Lovelace", password=PASSWORD),
    )
    assert result.username == "ada-lovelace"
    assert result.token.kind == "session"
    assert result.token.expires_at is not None
    assert result.token.scopes == ["admin"]
    signed_in = login(instance, LoginParams(username="ada-lovelace", password=PASSWORD))
    assert signed_in.actor.id == result.actor.id
    assert signed_in.token.scopes == ["admin"]


def test_the_bootstrap_without_a_password_is_the_headless_shape(
    instance: AppContext,
) -> None:
    result = install_bootstrap(instance, InstallBootstrapParams(display_name="Ada"))
    assert result.username is None
    assert result.token.kind == "api"
    assert result.token.expires_at is None
    assert list_users(instance, UserListParams()).users == []


def test_the_bootstrap_refuses_a_username_without_a_password(
    instance: AppContext,
) -> None:
    with pytest.raises(InvalidRequest, match="password"):
        install_bootstrap(
            instance, InstallBootstrapParams(display_name="Ada", username="ada")
        )


# -- over HTTP -------------------------------------------------------------


@pytest.fixture
def authed(instance: AppContext) -> Iterator[TestClient]:
    options = ServeOptions(host="127.0.0.1", port=18099, require_auth=True)
    with TestClient(build_server(options, config=instance.config)) as client:
        yield client


def test_login_needs_no_credential_and_the_session_it_mints_works(
    authed: TestClient, ada: str
) -> None:
    refused = authed.get("/api/auth/whoami")
    assert refused.status_code == 401
    signed_in = authed.post(
        "/api/auth/login", json={"username": "ada", "password": PASSWORD}
    )
    assert signed_in.status_code == 200, signed_in.text
    secret = signed_in.json()["secret"]
    headers = {"Authorization": f"Bearer {secret}"}
    me = authed.get("/api/auth/whoami", headers=headers)
    assert me.status_code == 200
    assert me.json()["identity_ref"] == "human:ada"
    assert me.json()["scopes"] == ["read", "work.write"]
    out = authed.post("/api/auth/logout", json={"reason": "bye"}, headers=headers)
    assert out.status_code == 200 and out.json()["revoked"] is True
    assert authed.get("/api/auth/whoami", headers=headers).status_code == 401


def test_a_wrong_password_over_http_is_401_and_a_throttle_is_429(
    authed: TestClient, ada: str
) -> None:
    for _ in range(LOGIN_FAILURE_LIMIT):
        res = authed.post("/api/auth/login", json={"username": "ada", "password": "no"})
        assert res.status_code == 401
        assert res.json()["error"]["code"] == "unauthenticated"
    res = authed.post("/api/auth/login", json={"username": "ada", "password": PASSWORD})
    assert res.status_code == 429
    assert res.json()["error"]["code"] == "login_throttled"
