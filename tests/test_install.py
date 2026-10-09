"""First-run install mode: a door that exists only while no operator does.

The security model these tests pin down: install mode is a property of the
credential store — active exactly while no person holds a token (revoked
included) or a password login — so the first person's credential, however
issued, closes the unauthenticated bootstrap for good, and nothing (not even
revoking every token) reopens it. Tokens bound to agent actors — the stack
secret adopted at init above all — are machinery and never close it (#903).
"""

from __future__ import annotations

from collections.abc import Iterator
from dataclasses import replace
from datetime import UTC, datetime
from pathlib import Path

import pytest
from fastapi.testclient import TestClient

from vogt.adapters.http.server import ServeOptions, build_server
from vogt.application.context import AppContext
from vogt.application.models import (
    CreateActorParams,
    CreateUserParams,
    InitParams,
    InstallBootstrapParams,
    IssueTokenParams,
    RemoveUserParams,
    RevokeTokenParams,
)
from vogt.application.services import (
    create_actor,
    create_user,
    init_instance,
    install_bootstrap,
    install_status,
    issue_token,
    remove_user,
    revoke_token,
)
from vogt.application.services.auth import authenticate
from vogt.application.services.install import install_mode_active
from vogt.errors import InstallClosed, InvalidRequest
from vogt.storage.sqlite.connection import connect
from vogt.storage.sqlite.declared import MIGRATIONS_DIR as DECLARED_MIGRATIONS
from vogt.storage.sqlite.declared import SqliteDeclaredStore
from vogt.storage.sqlite.migrator import Migrator, load_migrations

from tests.conftest import TEST_PRINCIPAL, StepClock

WHY = "install test"
PASSWORD = "correct horse battery"
#: The stack secret a Docker quick start writes to deploy/vogt-core-token.
CORE_SECRET = "vogt_stack_secret_that_is_long_enough_0123456789"


@pytest.fixture
def authed(instance: AppContext) -> Iterator[TestClient]:
    """An authenticated server over a fresh instance — the wizard's world."""
    options = ServeOptions(host="127.0.0.1", port=18099, require_auth=True)
    with TestClient(build_server(options, config=instance.config)) as client:
        yield client


def _issue_token(instance: AppContext, *, kind: str) -> None:
    identity_ref = f"{kind}:someone"
    create_actor(
        instance,
        CreateActorParams(
            identity_ref=identity_ref,
            kind=kind,
            display_name="Someone",
            reason=WHY,
        ),
    )
    issue_token(
        instance,
        IssueTokenParams(actor=identity_ref, name="t", scopes="read", reason=WHY),
    )


# -- the service -----------------------------------------------------------


def test_a_fresh_instance_is_in_install_mode(instance: AppContext) -> None:
    assert install_status(instance).install_mode is True


def test_bootstrap_names_the_operator_and_mints_an_admin_token(
    instance: AppContext,
) -> None:
    result = install_bootstrap(
        instance, InstallBootstrapParams(display_name="Ada Lovelace")
    )
    assert result.actor.identity_ref == "human:ada-lovelace"
    assert result.actor.kind == "human"
    assert result.actor.display_name == "Ada Lovelace"
    assert result.token.scopes == ["admin"]
    assert result.token.actor_id == result.actor.id
    assert result.secret.startswith("vogt_")
    assert result.token.expires_at is None


def test_disabling_the_bootstrap_refuses_it_and_reports_closed(
    instance: AppContext,
) -> None:
    """An operator can refuse the unauthenticated first-run bootstrap.

    A fronted/public deployment that provisions its first credential another
    way sets install_bootstrap_enabled=false; the door is then closed even on
    a fresh, zero-token store — no window for an internet caller to race.
    """
    from dataclasses import replace

    disabled = replace(
        instance,
        config=instance.config.model_copy(update={"install_bootstrap_enabled": False}),
    )
    assert install_status(disabled).install_mode is False
    with pytest.raises(InstallClosed, match="disabled"):
        install_bootstrap(disabled, InstallBootstrapParams(display_name="Ada"))
    # And no token was minted, so the loopback path can still provision one.
    with disabled.declared.read() as view:
        assert not view.list_tokens(include_revoked=True, limit=1)


def test_bootstrap_closes_install_mode(instance: AppContext) -> None:
    install_bootstrap(instance, InstallBootstrapParams(display_name="Ada"))
    assert install_status(instance).install_mode is False
    with pytest.raises(InstallClosed):
        install_bootstrap(instance, InstallBootstrapParams(display_name="Eve"))


def test_a_persons_token_closes_install_mode(instance: AppContext) -> None:
    """The gate is "no person holds a credential", not "the wizard has not
    run": a token minted over loopback for a person closes it too."""
    _issue_token(instance, kind="human")
    assert install_status(instance).install_mode is False
    with pytest.raises(InstallClosed):
        install_bootstrap(instance, InstallBootstrapParams(display_name="Eve"))


def test_an_agents_token_leaves_install_mode_open(instance: AppContext) -> None:
    """An agent's token is not an operator: nobody could sign in with it."""
    _issue_token(instance, kind="agent")
    assert install_status(instance).install_mode is True


def test_a_password_login_closes_install_mode(instance: AppContext) -> None:
    """`vogt user create` — the CLI fallback — is a first operator too."""
    create_user(
        instance,
        CreateUserParams(username="ada", password=PASSWORD, scopes="admin", reason=WHY),
    )
    assert install_status(instance).install_mode is False
    with pytest.raises(InstallClosed):
        install_bootstrap(instance, InstallBootstrapParams(display_name="Eve"))


def test_revoking_every_token_does_not_reopen_install_mode(
    instance: AppContext,
) -> None:
    """A lockout is fixed over loopback, not by reopening the door."""
    result = install_bootstrap(instance, InstallBootstrapParams(display_name="Ada"))
    revoke_token(instance, RevokeTokenParams(id=result.token.id, reason=WHY))
    assert install_status(instance).install_mode is False
    with pytest.raises(InstallClosed):
        install_bootstrap(instance, InstallBootstrapParams(display_name="Mallory"))


def test_the_bootstrap_is_audited_and_attributed_to_the_new_actor(
    instance: AppContext,
) -> None:
    install_bootstrap(instance, InstallBootstrapParams(display_name="Ada"))
    with instance.declared.read() as view:
        operations = {
            record.operation: record.actor_identity_ref
            for record in view.list_audit(limit=10)
        }
    assert operations["install.bootstrap"] == "human:ada"
    assert operations["actor.auto_register"] == "human:ada"


def test_a_failed_bootstrap_leaves_no_actor_behind(instance: AppContext) -> None:
    """The loser of the race rolls back everything, auto-registration included."""
    _issue_token(instance, kind="human")
    with pytest.raises(InstallClosed):
        install_bootstrap(instance, InstallBootstrapParams(display_name="Eve"))
    with instance.declared.read() as view:
        actors = {actor.identity_ref for actor in view.list_actors(limit=100, offset=0)}
    assert "human:eve" not in actors


def test_an_explicit_identity_ref_is_honoured(instance: AppContext) -> None:
    result = install_bootstrap(
        instance,
        InstallBootstrapParams(display_name="Ada", identity_ref="human:ada.l"),
    )
    assert result.actor.identity_ref == "human:ada.l"


def test_an_unsluggable_display_name_is_refused(instance: AppContext) -> None:
    with pytest.raises(InvalidRequest, match="cannot derive an identity"):
        install_bootstrap(instance, InstallBootstrapParams(display_name="???"))
    assert install_status(instance).install_mode is True


# -- the HTTP surface ------------------------------------------------------


def test_install_status_needs_no_credential(authed: TestClient) -> None:
    response = authed.get("/api/install/status")
    assert response.status_code == 200
    assert response.json()["install_mode"] is True


def test_the_bootstrap_issues_a_working_token_over_http(authed: TestClient) -> None:
    """The whole point: browser arrives with nothing, leaves authenticated."""
    response = authed.post(
        "/api/install/bootstrap", json={"display_name": "Ada Lovelace"}
    )
    assert response.status_code == 200
    body = response.json()
    secret = body["secret"]
    assert body["actor"]["identity_ref"] == "human:ada-lovelace"
    assert "only time" in body["warning"]

    authenticated = authed.get(
        "/api/status", headers={"Authorization": f"Bearer {secret}"}
    )
    assert authenticated.status_code == 200
    assert authed.get("/api/install/status").json()["install_mode"] is False


def test_a_closed_bootstrap_refuses_with_a_named_reason(authed: TestClient) -> None:
    first = authed.post("/api/install/bootstrap", json={"display_name": "Ada"})
    assert first.status_code == 200
    second = authed.post("/api/install/bootstrap", json={"display_name": "Eve"})
    assert second.status_code == 409
    assert second.json()["error"]["code"] == "install_closed"


def test_the_bootstrap_validates_its_body(authed: TestClient) -> None:
    response = authed.post("/api/install/bootstrap", json={})
    assert response.status_code == 422
    assert response.json()["error"]["code"] == "invalid_arguments"


# -- #903: the Docker quick start supplies the stack secret first ----------


@pytest.fixture
def quick_start(instance: AppContext, tmp_path: Path) -> AppContext:
    """A fresh instance booted the way the published stack boots it: the
    operator wrote the stack secret before `docker compose up`, so `init`
    adopted it as the core token, and no person exists yet."""
    secret_file = tmp_path / "vogt-core-token"
    secret_file.write_text(CORE_SECRET, encoding="utf-8")
    ctx = replace(
        instance,
        config=instance.config.model_copy(
            update={"bootstrap_core_token_file": secret_file}
        ),
    )
    assert init_instance(ctx, InitParams()).bootstrap_core_token == "adopted"
    return ctx


def test_the_adopted_core_token_leaves_install_mode_open(
    quick_start: AppContext,
) -> None:
    with quick_start.declared.read() as view:
        assert view.list_tokens(include_revoked=True, limit=10), "a token row exists"
        assert view.list_password_credentials() == []
    assert install_status(quick_start).install_mode is True


def test_a_quick_start_reaches_a_first_operator_end_to_end(
    quick_start: AppContext,
) -> None:
    """Over HTTP, as the wizard does it: install mode is on, the bootstrap
    creates a login, the password signs in, and the door then shuts — while
    the core token keeps exactly the identity and scopes it was adopted with."""
    options = ServeOptions(host="127.0.0.1", port=18099, require_auth=True)
    with TestClient(build_server(options, config=quick_start.config)) as client:
        assert client.get("/api/install/status").json() == {"install_mode": True}

        created = client.post(
            "/api/install/bootstrap",
            json={"display_name": "Ada", "username": "ada", "password": PASSWORD},
        )
        assert created.status_code == 200, created.text
        assert created.json()["username"] == "ada"

        signed_in = client.post(
            "/api/auth/login", json={"username": "ada", "password": PASSWORD}
        )
        assert signed_in.status_code == 200, signed_in.text
        session = signed_in.json()["secret"]
        whoami = client.get(
            "/api/auth/whoami", headers={"Authorization": f"Bearer {session}"}
        )
        assert whoami.status_code == 200
        assert whoami.json()["identity_ref"] == "human:ada"

        assert client.get("/api/install/status").json() == {"install_mode": False}
        again = client.post(
            "/api/install/bootstrap",
            json={"display_name": "Eve", "username": "eve", "password": PASSWORD},
        )
        assert again.status_code == 409
        assert again.json()["error"]["code"] == "install_closed"

    core = authenticate(quick_start, bearer=CORE_SECRET)
    assert core.principal.identity_ref == "agent:vogt-engine"
    assert core.principal.kind == "agent"
    assert core.token is not None
    assert sorted(core.token.scopes) == ["project.write", "read", "work.write"]


def test_the_cli_fallback_also_closes_a_quick_start(quick_start: AppContext) -> None:
    """`vogt user create --scopes admin` inside the container is the
    documented alternative to the wizard; it closes the door the same way."""
    create_user(
        quick_start,
        CreateUserParams(username="ada", password=PASSWORD, scopes="admin", reason=WHY),
    )
    assert install_status(quick_start).install_mode is False


# -- the latch: an upgrade, or a removed user, never reopens the door -------

#: The migration that latches install mode closed. Named, not derived from
#: "the newest one", so this keeps testing that upgrade after later ones land.
LATCH_MIGRATION = 20


def _store_before_the_latch(tmp_path: Path) -> Path:
    """A declared store migrated with every shipped migration before 0020,
    the way a v0.7.7 instance's store is."""
    old = tmp_path / "before-latch"
    old.mkdir()
    for migration in load_migrations(DECLARED_MIGRATIONS):
        if migration.number < LATCH_MIGRATION:
            (old / f"{migration.id}.sql").write_text(migration.sql, encoding="utf-8")
    path = tmp_path / "declared.sqlite3"
    conn = connect(path, create=True)
    Migrator(store="declared", directory=old, holder="old/1").migrate(
        conn, now=datetime(2026, 9, 1, tzinfo=UTC)
    )
    conn.close()
    return path


def test_an_upgraded_instance_operated_without_a_person_stays_closed(
    tmp_path: Path,
) -> None:
    """The old README quick start had operators drive everything with the
    engine's break-glass ENGINE_TOKEN, which is no core row: such a store
    holds the adopted stack secret, session and agent tokens and its work —
    and no person's credential. Upgrading it must not open an
    unauthenticated admin bootstrap on its port."""
    path = _store_before_the_latch(tmp_path)
    conn = connect(path, create=False)
    stamp = "2026-09-01T00:00:00+00:00"
    conn.execute(
        "INSERT INTO actors (id, kind, display_name, identity_ref, created_at) "
        "VALUES ('act_engine', 'agent', 'agent:vogt-engine', 'agent:vogt-engine', ?)",
        (stamp,),
    )
    for token_id, name, kind in [
        ("tok_core", "bootstrap-core-token", "api"),
        ("tok_session", "session ses_1", "agent"),
    ]:
        conn.execute(
            "INSERT INTO tokens (id, actor_id, name, token_hash, scopes, kind, "
            "created_at) VALUES (?, 'act_engine', ?, ?, '[\"read\"]', ?, ?)",
            (token_id, name, f"hash-{token_id}", kind, stamp),
        )
    conn.execute(
        "INSERT INTO projects (id, slug, name, root_path, lifecycle_state, "
        "compliance_status, exclusions, trust_state, created_at, updated_at) "
        "VALUES ('prj_1', 'months-of-work', 'Months of work', '/srv/w', 'active', "
        "'not_checked', '[]', 'unverified', ?, ?)",
        (stamp, stamp),
    )
    conn.commit()
    conn.close()

    store = SqliteDeclaredStore(path, clock=StepClock())
    store.bootstrap(TEST_PRINCIPAL)
    assert "0020_install_latch" in store.migrate().applied
    with store.read() as view:
        assert not view.list_password_credentials()
    with store.read() as view:
        assert install_mode_active(view) is False


def test_an_upgraded_store_with_no_tokens_is_still_a_fresh_install(
    tmp_path: Path,
) -> None:
    """The latch copies the old rule exactly: a store that held no token at
    all was in install mode before the upgrade and still is."""
    store = SqliteDeclaredStore(_store_before_the_latch(tmp_path), clock=StepClock())
    store.bootstrap(TEST_PRINCIPAL)
    store.migrate()
    with store.read() as view:
        assert install_mode_active(view) is True


def test_removing_the_only_operator_does_not_reopen_install_mode(
    quick_start: AppContext,
) -> None:
    """A login made with `vogt user create` and never used holds no token;
    removing it (say, to fix a typo in the username) leaves no person
    credential — and the door stays shut regardless."""
    create_user(
        quick_start,
        CreateUserParams(
            username="adda", password=PASSWORD, scopes="admin", reason=WHY
        ),
    )
    remove_user(quick_start, RemoveUserParams(username="adda", reason=WHY))
    with quick_start.declared.read() as view:
        assert view.list_password_credentials() == []
    assert install_status(quick_start).install_mode is False
    with pytest.raises(InstallClosed):
        install_bootstrap(quick_start, InstallBootstrapParams(display_name="Eve"))
