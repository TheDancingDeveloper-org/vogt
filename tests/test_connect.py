"""Connecting a client (FR-A8).

The requirement this covers did not exist until r7, and its absence is the
reason nothing failed while the product had no client story at all. Three
separate items each recorded a piece of it as minor — NFR-D3 "vacuously
satisfied, Vogt ships no client-setup script", `DEPLOYMENT.md` §4.3's
un-built generator, and NFR-PO4's unpublished wheel — and no requirement
owned the whole.
"""

from __future__ import annotations

import json
from pathlib import Path

import pytest
from fastapi.testclient import TestClient

from vogt.adapters.http.app import build_app
from vogt.adapters.http.health import ServerInfo, add_health_routes
from vogt.application.context import AppContext, build_context
from vogt.application.models import ConnectParams, InitParams
from vogt.application.services import connect, init_instance
from vogt.application.services.connect import NOT_CONFIGURED
from vogt.config import VogtConfig
from vogt.registry import default_registry

from tests.conftest import TEST_PRINCIPAL, SequentialIds, StepClock

URL = "https://host.tailnet.ts.net:18094"


def _instance(data_dir: Path, url: str | None) -> AppContext:
    context = build_context(
        config=VogtConfig(data_dir=data_dir, public_url=url, sqlite_synchronous="off"),
        principal=TEST_PRINCIPAL,
        clock=StepClock(),
        id_factory=SequentialIds(),
    )
    init_instance(context, InitParams())
    return context


@pytest.fixture
def configured(data_dir: Path) -> AppContext:
    return _instance(data_dir, URL)


@pytest.fixture
def unconfigured(data_dir: Path) -> AppContext:
    return _instance(data_dir, None)


# -- the fact a client needs first ----------------------------------------


def test_connect_states_where_the_instance_is(configured: AppContext) -> None:
    result = connect(configured, ConnectParams())
    assert result.url == URL
    assert result.mcp_url == f"{URL}/mcp"
    assert result.detail is None


def test_an_unconfigured_instance_says_so_rather_than_guessing(
    unconfigured: AppContext,
) -> None:
    """The whole reason `public_url` is configuration and not inference.

    A server binds a container port and is published somewhere else. Any URL
    it invented would be wrong in exactly the deployment this field exists
    for, and from a client a wrong URL is indistinguishable from an
    unreachable one.
    """
    result = connect(unconfigured, ConnectParams())
    assert result.url is None
    assert result.mcp_url is None
    assert result.detail == NOT_CONFIGURED
    assert "public_url" in result.detail


def test_a_trailing_slash_does_not_become_a_double_slash(data_dir: Path) -> None:
    context = _instance(data_dir, f"{URL}/")
    assert connect(context, ConnectParams()).mcp_url == f"{URL}/mcp"


# -- the recommended path installs nothing --------------------------------


def test_the_http_client_needs_no_vogt_code(configured: AppContext) -> None:
    """Which is the entire strategic argument for preferring it.

    A client holding a copy of Vogt's code has a version that can skew
    (FR-A6 exists because of it) and a second place to upgrade. Streamable
    HTTP has neither.
    """
    result = connect(configured, ConnectParams(client="http"))
    assert result.requires_install is False

    config = json.loads(result.configuration)
    assert config["mcpServers"]["vogt"]["url"] == f"{URL}/mcp"
    assert config["mcpServers"]["vogt"]["type"] == "http"


def test_the_bridge_is_honest_about_its_cost(configured: AppContext) -> None:
    result = connect(configured, ConnectParams(client="bridge"))
    assert result.requires_install is True

    config = json.loads(result.configuration)["mcpServers"]["vogt"]
    assert config["command"] == "vogt-mcp-remote"
    assert config["env"]["VOGT_URL"] == URL


def test_no_configuration_ever_carries_a_token_inline(
    configured: AppContext,
) -> None:
    """FR-S7 at the place a config example would leak one.

    The HTTP config references an environment variable and the bridge
    references a token *file* path. Neither embeds a credential, because a
    generated snippet is the thing people paste unchanged.
    """
    for client in ("http", "bridge"):
        for fmt in ("json", "markdown"):
            rendered = connect(
                configured, ConnectParams(client=client, format=fmt)
            ).configuration
            assert "VOGT_TOKEN" in rendered or "token" in rendered.lower()
            # A URL with credentials in it is the specific shape to refuse.
            assert "@" not in rendered.split("://")[-1].split("/")[0]


# -- the document DEPLOYMENT.md §4.3 asked for ----------------------------


def test_markdown_renders_the_connection_document(configured: AppContext) -> None:
    rendered = connect(configured, ConnectParams(format="markdown")).configuration
    assert rendered.startswith("# Connecting to Vogt")
    assert f"{URL}/mcp" in rendered
    assert "2025-06-18" in rendered


def test_the_document_is_generated_not_committed() -> None:
    """§4.3's failure mode was a URL living in seven places.

    A `CONNECTING.md` in the repository is an eighth copy, and it drifts from
    the instance it claims to describe. The operation renders one on demand
    from the running server instead, so there is nothing to go stale.
    """
    assert not Path("CONNECTING.md").exists()
    assert not Path("docs/CONNECTING.md").exists()


# -- reachable everywhere, like everything else (FR-A1) -------------------


def test_connect_is_on_every_surface() -> None:
    registry = default_registry()
    assert registry.transports_for("connect") >= frozenset({"cli", "http", "mcp"})
    assert not registry.get("connect").mutating


def test_connect_answers_over_rest(configured: AppContext) -> None:
    client = TestClient(
        build_app(registry=default_registry(), context_factory=lambda: configured)
    )
    response = client.get("/api/connect", params={"client": "http"})
    assert response.status_code == 200
    assert response.json()["mcp_url"] == f"{URL}/mcp"


def _probe_client(context: AppContext) -> TestClient:
    """A server with the plain-HTTP probes mounted, as `serve` builds it."""
    app = build_app(registry=default_registry(), context_factory=lambda: context)
    add_health_routes(
        app,
        context_factory=lambda: context,
        info=ServerInfo(writes_enabled=True, auth_required=True),
        api_prefix="/api",
        mcp_path="/mcp",
    )
    return TestClient(app)


def test_connection_info_reports_the_url_unauthenticated(
    configured: AppContext,
) -> None:
    """FR-A7's probe and FR-A8's operation must not disagree.

    Two endpoints reporting the same fact from different sources is how the
    `:18081` incident started; both read `public_url`.
    """
    body = _probe_client(configured).get("/connection-info").json()
    assert body["url"] == URL
    assert body["mcp_path"] == "/mcp"


def test_connection_info_omits_a_url_it_does_not_have(
    unconfigured: AppContext,
) -> None:
    body = _probe_client(unconfigured).get("/connection-info").json()
    assert body["url"] is None
