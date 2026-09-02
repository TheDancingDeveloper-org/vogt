"""The served surface: one port, health, MCP over HTTP, and the gates.

`DEPLOYMENT.md` §1 exists because cadastre's MCP port served only `/mcp`, so
`/health/ready` answered `-32004` and "is it up?" needed an MCP client. The
first three tests here are the ones that would have caught that.
"""

from __future__ import annotations

from collections.abc import Iterator
from pathlib import Path
from typing import Any

import pytest
from fastapi.testclient import TestClient

from vogt.adapters.http.server import ServeOptions, build_server
from vogt.adapters.mcp.stdio import SUPPORTED_PROTOCOL_VERSIONS
from vogt.application.context import AppContext
from vogt.application.models import CreateActorParams, IssueTokenParams
from vogt.application.services import create_actor, issue_token
from vogt.errors import InvalidRequest

WHY = "serve test"


@pytest.fixture
def secrets(instance: AppContext) -> dict[str, str]:
    """One token per scope shape the tests need."""
    create_actor(
        instance,
        CreateActorParams(
            identity_ref="agent:reader",
            kind="agent",
            display_name="Reader",
            reason=WHY,
        ),
    )
    create_actor(
        instance,
        CreateActorParams(
            identity_ref="agent:writer",
            kind="agent",
            display_name="Writer",
            reason=WHY,
        ),
    )
    create_actor(
        instance,
        CreateActorParams(
            identity_ref="agent:admin",
            kind="agent",
            display_name="Admin",
            reason=WHY,
        ),
    )
    return {
        "read": issue_token(
            instance,
            IssueTokenParams(actor="agent:reader", name="r", scopes="read", reason=WHY),
        ).secret,
        "write": issue_token(
            instance,
            IssueTokenParams(
                actor="agent:writer",
                name="w",
                scopes="work.write,project.write",
                reason=WHY,
            ),
        ).secret,
        "admin": issue_token(
            instance,
            IssueTokenParams(actor="agent:admin", name="a", scopes="admin", reason=WHY),
        ).secret,
    }


def _client(instance: AppContext, **overrides: object) -> Iterator[TestClient]:
    options = ServeOptions(
        host="127.0.0.1",
        port=18099,
        **overrides,  # type: ignore[arg-type]
    )
    app = build_server(options, config=instance.config)
    with TestClient(app) as client:
        yield client


@pytest.fixture
def authed(instance: AppContext) -> Iterator[TestClient]:
    yield from _client(instance, require_auth=True)


@pytest.fixture
def open_client(instance: AppContext) -> Iterator[TestClient]:
    yield from _client(instance, require_auth=False)


# -- probes are plain HTTP, always (FR-A7) --------------------------------


def test_health_and_version_need_no_credential(authed: TestClient) -> None:
    """The cadastre failure this prevents: probes that need an MCP client."""
    assert authed.get("/health/live").json()["status"] == "ok"
    assert authed.get("/version").json()["name"] == "vogt"

    ready = authed.get("/health/ready")
    assert ready.status_code == 200
    assert ready.json()["status"] == "ready"
    assert ready.json()["declared_schema_version"] > 0


def test_readiness_reports_why_it_is_not_ready(tmp_path: Path) -> None:
    """A red probe should say what is wrong, not just be red."""
    from vogt.config import VogtConfig

    empty = VogtConfig(data_dir=tmp_path / "nothing-here")
    options = ServeOptions(host="127.0.0.1", port=18099, require_auth=False)
    with TestClient(build_server(options, config=empty)) as client:
        response = client.get("/health/ready")
    assert response.status_code == 503
    assert response.json()["status"] == "not_ready"
    assert "vogt init" in (response.json()["detail"] or "")


def test_connection_info_is_generated_by_the_server(authed: TestClient) -> None:
    """DEPLOYMENT §4.3: one document, from the running server."""
    info = authed.get("/connection-info").json()
    assert info["api_path"] == "/api"
    assert info["mcp_path"] == "/mcp"
    assert info["health_path"] == "/health/ready"
    assert info["supported_mcp_protocol_versions"]
    assert info["authentication"] == "bearer token"


def test_everything_answers_on_the_one_port(authed: TestClient) -> None:
    """NFR-D1: one process, one port, path-routed."""
    for path in ("/health/live", "/version", "/connection-info", "/openapi.json"):
        assert authed.get(path).status_code == 200, path


# -- authentication --------------------------------------------------------


def test_the_api_refuses_an_unauthenticated_caller(authed: TestClient) -> None:
    assert authed.get("/api/status").status_code == 401


def test_a_read_token_reads(authed: TestClient, secrets: dict[str, str]) -> None:
    response = authed.get(
        "/api/status", headers={"Authorization": f"Bearer {secrets['read']}"}
    )
    assert response.status_code == 200


def test_a_read_token_cannot_write(authed: TestClient, secrets: dict[str, str]) -> None:
    response = authed.post(
        "/api/projects",
        json={"name": "Nope", "root_path": "/srv/nope", "reason": WHY},
        headers={"Authorization": f"Bearer {secrets['read']}"},
    )
    assert response.status_code == 403
    assert response.json()["error"]["code"] == "forbidden"


def test_a_write_token_writes(authed: TestClient, secrets: dict[str, str]) -> None:
    response = authed.post(
        "/api/projects",
        json={"name": "Allowed", "root_path": "/srv/allowed", "reason": WHY},
        headers={"Authorization": f"Bearer {secrets['write']}"},
    )
    assert response.status_code == 200


def test_the_write_is_attributed_to_the_token_holder(
    instance: AppContext, authed: TestClient, secrets: dict[str, str]
) -> None:
    """FR-S2: provenance comes from the credential, not from the request."""
    authed.post(
        "/api/projects",
        json={"name": "Attributed", "root_path": "/srv/a", "reason": WHY},
        headers={"Authorization": f"Bearer {secrets['write']}"},
    )
    with instance.declared.read() as view:
        record = view.list_audit(limit=1)[0]
    assert record.operation == "project.register"
    assert record.actor_identity_ref == "agent:writer"


def test_a_garbage_token_is_401_not_500(authed: TestClient) -> None:
    response = authed.get("/api/status", headers={"Authorization": "Bearer vogt_nope"})
    assert response.status_code == 401


def test_the_loopback_surface_needs_no_token(open_client: TestClient) -> None:
    assert open_client.get("/api/status").status_code == 200
    assert open_client.get("/connection-info").json()["authentication"] == (
        "none (loopback)"
    )


# -- MCP over the same port (FR-A5) ---------------------------------------


def _rpc(
    client: TestClient, method: str, token: str | None = None, **params: object
) -> dict[str, Any]:
    headers = {"Authorization": f"Bearer {token}"} if token else {}
    response = client.post(
        "/mcp",
        json={"jsonrpc": "2.0", "id": 1, "method": method, "params": params},
        headers=headers,
    )
    parsed: dict[str, Any] = response.json()
    return parsed


def test_mcp_initialize_over_http(authed: TestClient, secrets: dict[str, str]) -> None:
    result = _rpc(authed, "initialize", secrets["read"])["result"]
    assert result["serverInfo"]["name"] == "vogt"
    assert result["protocolVersion"]


def test_an_unknown_protocol_version_negotiates_down(
    authed: TestClient, secrets: dict[str, str]
) -> None:
    """FR-A6 as revised (r8), on HTTP as well as stdio.

    This is the transport Claude Code actually uses through the bridge, and
    the one that refused 2025-11-25 in production.
    """
    response = _rpc(authed, "initialize", secrets["read"], protocolVersion="2999-01-01")
    assert "error" not in response, response
    assert response["result"]["protocolVersion"] == SUPPORTED_PROTOCOL_VERSIONS[0]


def test_ungranted_tools_are_invisible_not_erroring(
    authed: TestClient, secrets: dict[str, str]
) -> None:
    """FR-S4: an agent's tool list is exactly what it can do."""

    def tools(token: str) -> set[str]:
        listed = _rpc(authed, "tools/list", token)["result"]["tools"]
        return {entry["name"] for entry in listed}

    reader = tools(secrets["read"])
    writer = tools(secrets["write"])

    assert "backlog" in reader
    assert "work_create" not in reader, "a read token does not see write tools"
    assert "work_create" in writer
    assert "token_issue" not in writer, "nor does a writer see admin tools"


def test_calling_an_ungranted_tool_is_refused_and_recorded(
    instance: AppContext, authed: TestClient, secrets: dict[str, str]
) -> None:
    """Checked at tools/call as well as tools/list — both gates, every time."""
    response = _rpc(
        authed,
        "tools/call",
        secrets["read"],
        name="work_create",
        arguments={"kind": "bug", "title": "x", "reason": WHY},
    )
    assert response["result"]["isError"] is True
    assert "forbidden" in response["result"]["content"][0]["text"]

    with instance.declared.read() as view:
        denials = view.list_auth_decisions(decision="deny", limit=10)
    assert any(d.transport == "mcp-http" for d in denials)


def test_local_only_tools_are_not_callable_over_http(
    instance: AppContext, authed: TestClient, secrets: dict[str, str]
) -> None:
    """LOCAL_ONLY operations are excluded from tools/list AND tools/call.

    `restore`/`backup`/`serve`/… turn an admin API token into arbitrary-path
    filesystem read/write on the host; the remote MCP transport must refuse
    them the same way REST and tools/list already do — as unknown tools,
    before authorization or dispatch.
    """
    for tool in ("restore", "backup", "serve", "import", "init", "migrate"):
        response = _rpc(
            authed,
            "tools/call",
            secrets["admin"],
            name=tool,
            arguments={},
        )
        assert "error" in response, f"{tool} must not dispatch over HTTP"
        assert response["error"]["code"] == -32601, tool  # METHOD_NOT_FOUND
        assert "result" not in response, tool


def test_a_granted_tool_call_works_over_http(
    authed: TestClient, secrets: dict[str, str]
) -> None:
    response = _rpc(
        authed,
        "tools/call",
        secrets["write"],
        name="work_create",
        arguments={"kind": "bug", "title": "Filed over HTTP", "reason": WHY},
    )
    assert response["result"]["isError"] is False
    assert response["result"]["structuredContent"]["item"]["ref"] == "WI-1"


# -- serve options ---------------------------------------------------------


def test_a_listen_address_is_required(instance: AppContext) -> None:
    """NFR-D2: no default may encode exposure."""
    with pytest.raises(InvalidRequest, match="no default"):
        ServeOptions(host="  ", port=1234).validate()


def test_tls_needs_both_halves(tmp_path: Path) -> None:
    cert = tmp_path / "cert.pem"
    cert.write_text("x", encoding="utf-8")
    with pytest.raises(InvalidRequest, match="together or not at all"):
        ServeOptions(host="127.0.0.1", port=1, tls_cert=cert).validate()


def test_a_missing_certificate_is_named(tmp_path: Path) -> None:
    with pytest.raises(InvalidRequest, match="no such file"):
        ServeOptions(
            host="127.0.0.1",
            port=1,
            tls_cert=tmp_path / "absent.pem",
            tls_key=tmp_path / "absent.key",
        ).validate()


def test_the_scheme_follows_the_certificate(tmp_path: Path) -> None:
    assert ServeOptions(host="127.0.0.1", port=1).scheme == "http"
    cert = tmp_path / "c.pem"
    key = tmp_path / "k.pem"
    for path in (cert, key):
        path.write_text("x", encoding="utf-8")
    options = ServeOptions(host="127.0.0.1", port=1, tls_cert=cert, tls_key=key)
    options.validate()
    assert options.scheme == "https"
