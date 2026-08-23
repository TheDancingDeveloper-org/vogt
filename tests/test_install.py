"""First-run install mode (#292): a door that exists only while no token does.

The security model these tests pin down: install mode is a property of the
token store — active exactly while it holds no rows at all — so the first
token, however issued, closes the unauthenticated bootstrap for good, and
nothing (not even revoking every token) reopens it.
"""

from __future__ import annotations

from collections.abc import Iterator

import pytest
from fastapi.testclient import TestClient

from vogt.adapters.http.server import ServeOptions, build_server
from vogt.application.context import AppContext
from vogt.application.models import (
    CreateActorParams,
    InstallBootstrapParams,
    IssueTokenParams,
    RevokeTokenParams,
)
from vogt.application.services import (
    create_actor,
    install_bootstrap,
    install_status,
    issue_token,
    revoke_token,
)
from vogt.errors import InstallClosed, InvalidRequest

WHY = "install test"


@pytest.fixture
def authed(instance: AppContext) -> Iterator[TestClient]:
    """An authenticated server over a fresh instance — the wizard's world."""
    options = ServeOptions(host="127.0.0.1", port=18099, require_auth=True)
    with TestClient(build_server(options, config=instance.config)) as client:
        yield client


def _issue_any_token(instance: AppContext) -> None:
    create_actor(
        instance,
        CreateActorParams(
            identity_ref="agent:someone",
            kind="agent",
            display_name="Someone",
            reason=WHY,
        ),
    )
    issue_token(
        instance,
        IssueTokenParams(actor="agent:someone", name="t", scopes="read", reason=WHY),
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


def test_bootstrap_closes_install_mode(instance: AppContext) -> None:
    install_bootstrap(instance, InstallBootstrapParams(display_name="Ada"))
    assert install_status(instance).install_mode is False
    with pytest.raises(InstallClosed):
        install_bootstrap(instance, InstallBootstrapParams(display_name="Eve"))


def test_any_token_closes_install_mode(instance: AppContext) -> None:
    """The gate is "zero tokens", not "the wizard has not run"."""
    _issue_any_token(instance)
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
    _issue_any_token(instance)
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
