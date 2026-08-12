"""The transport-parity harness (FR-A3).

For every registered operation this drives the CLI, the REST surface and the
MCP surface against three identical instances and asserts that they return
the same answer and leave the same audit trail. Parity is *tested*, not
intended (DESIGN §2).

Three staleness checks run alongside it, all of which fail in **both**
directions:

1. An operation must appear on exactly the surfaces `transports_for` claims —
   present where expected, absent where excluded.
2. Every exclusion must name a registered operation.
3. Every non-excluded operation must have a scenario here, so a new operation
   cannot be added without being driven on all three surfaces.
"""

from __future__ import annotations

import json
from typing import Any

import pytest
from fastapi.testclient import TestClient

from vogt.adapters.cli.main import EXIT_OK, build_parser, run
from vogt.adapters.http.app import API_PREFIX, build_app
from vogt.adapters.mcp.surface import McpSurface
from vogt.application.context import AppContext, build_context
from vogt.application.models import InitParams
from vogt.application.services import init_instance
from vogt.config import VogtConfig
from vogt.registry import HTTP_ONLY, LOCAL_ONLY, default_registry
from vogt.registry.operation import Operation

from tests.conftest import TEST_PRINCIPAL, SequentialIds, StepClock

#: One invocation per operation. Ordered by the registry, so all three
#: transports see the same instance history.
SCENARIOS: dict[str, dict[str, Any]] = {
    "status": {},
    "project.register": {
        "name": "Parity Project",
        "root_path": "/srv/parity",
        "reason": "parity harness: exercise the write path on every transport",
    },
    "project.list": {},
    "events.list": {},
    "audit.list": {},
}

#: Values that legitimately differ between two runs of the same sequence.
VOLATILE_KEYS = frozenset(
    {
        "id",
        "instance_id",
        "entity_id",
        "actor_id",
        "audit_id",
        "txn_id",
        "created_at",
        "updated_at",
        "compliance_checked_at",
        "at",
        "data_dir",
        "payload_digest",
    }
)


def normalise(value: Any) -> Any:
    """Blank out values that cannot match across independent instances."""
    if isinstance(value, dict):
        return {
            key: "<volatile>" if key in VOLATILE_KEYS else normalise(item)
            for key, item in value.items()
        }
    if isinstance(value, list):
        return [normalise(item) for item in value]
    return value


def _fresh_context(tmp_path_factory: pytest.TempPathFactory, label: str) -> AppContext:
    directory = tmp_path_factory.mktemp(label)
    context = build_context(
        config=VogtConfig(data_dir=directory / "instance"),
        principal=TEST_PRINCIPAL,
        clock=StepClock(),
        id_factory=SequentialIds(),
    )
    init_instance(context, InitParams())
    return context


def _argv_for(operation: Operation[Any, Any], params: dict[str, Any]) -> list[str]:
    argv = [*operation.cli.path]
    for key, value in params.items():
        flag = f"--{key.replace('_', '-')}"
        if isinstance(value, bool):
            argv.append(flag if value else f"--no-{key.replace('_', '-')}")
        else:
            argv += [flag, str(value)]
    return argv


def _via_cli(context: AppContext, name: str, params: dict[str, Any]) -> Any:
    registry = default_registry()
    operation = registry.get(name)
    result = run(
        ["--json", *_argv_for(operation, params)],
        registry=registry,
        context=context,
    )
    assert result.exit_code == EXIT_OK, result.stderr
    return json.loads(result.stdout)


def _via_http(context: AppContext, name: str, params: dict[str, Any]) -> Any:
    registry = default_registry()
    operation = registry.get(name)
    client = TestClient(build_app(registry=registry, context_factory=lambda: context))
    url = f"{API_PREFIX}{operation.route.path}"
    if operation.route.method == "GET":
        response = client.get(url, params=params)
    else:
        response = client.post(url, json=params)
    assert response.status_code == 200, response.text
    return response.json()


def _via_mcp(context: AppContext, name: str, params: dict[str, Any]) -> Any:
    registry = default_registry()
    surface = McpSurface(registry=registry, context_factory=lambda: context)
    return surface.call_tool(registry.get(name).mcp_tool_name, params)


def _audit_trail(context: AppContext) -> list[dict[str, Any]]:
    with context.declared.read() as view:
        return [
            normalise(record.model_dump(mode="json"))
            for record in view.list_audit(limit=100)
        ]


@pytest.fixture(scope="module")
def parity_results(
    tmp_path_factory: pytest.TempPathFactory,
) -> dict[str, dict[str, Any]]:
    """Run every scenario on every transport, against three fresh instances."""
    registry = default_registry()
    contexts = {
        "cli": _fresh_context(tmp_path_factory, "cli"),
        "http": _fresh_context(tmp_path_factory, "http"),
        "mcp": _fresh_context(tmp_path_factory, "mcp"),
    }
    drivers = {"cli": _via_cli, "http": _via_http, "mcp": _via_mcp}
    results: dict[str, dict[str, Any]] = {name: {} for name in SCENARIOS}

    for operation in registry:
        if operation.name not in SCENARIOS:
            continue
        params = SCENARIOS[operation.name]
        for transport, context in contexts.items():
            results[operation.name][transport] = drivers[transport](
                context, operation.name, dict(params)
            )
    results["__audit__"] = {
        transport: _audit_trail(context) for transport, context in contexts.items()
    }
    return results


# -- the matrix ------------------------------------------------------------


@pytest.mark.parametrize("name", sorted(SCENARIOS))
def test_transports_return_the_same_answer(
    name: str, parity_results: dict[str, dict[str, Any]]
) -> None:
    answers = parity_results[name]
    cli = normalise(answers["cli"])
    assert cli == normalise(answers["http"]), f"{name}: CLI and REST disagree"
    assert cli == normalise(answers["mcp"]), f"{name}: CLI and MCP disagree"


def test_transports_leave_the_same_audit_trail(
    parity_results: dict[str, dict[str, Any]],
) -> None:
    trails = parity_results["__audit__"]
    assert trails["cli"] == trails["http"]
    assert trails["cli"] == trails["mcp"]
    operations = [record["operation"] for record in trails["cli"]]
    assert "project.register" in operations
    assert "instance.init" in operations


# -- staleness, in both directions ----------------------------------------


def test_every_operation_appears_on_exactly_its_expected_surfaces() -> None:
    registry = default_registry()
    parser_text = build_parser(registry).format_help()
    app = build_app(registry=registry, context_factory=build_context)
    http_routes = {
        (getattr(route, "path", ""), method)
        for route in app.routes
        for method in getattr(route, "methods", set())
    }
    mcp_tools = {tool.name for tool in McpSurface(registry=registry).list_tools()}

    for operation in registry:
        expected = registry.transports_for(operation.name)
        route_key = (f"{API_PREFIX}{operation.route.path}", operation.route.method)

        assert (route_key in http_routes) is ("http" in expected), (
            f"{operation.name}: REST presence does not match its exclusion state"
        )
        assert (operation.mcp_tool_name in mcp_tools) is ("mcp" in expected), (
            f"{operation.name}: MCP presence does not match its exclusion state"
        )
        # argparse renders top-level commands in the help text; nested ones
        # are reached through their group, which is enough to show presence.
        assert (operation.cli.path[0] in parser_text) is ("cli" in expected), (
            f"{operation.name}: CLI presence does not match its exclusion state"
        )


def test_exclusion_lists_name_only_registered_operations() -> None:
    registry = default_registry()
    for name in (*LOCAL_ONLY, *HTTP_ONLY):
        assert name in registry, f"stale parity exclusion: {name}"


def test_exclusions_carry_a_justification() -> None:
    for name, reason in (*LOCAL_ONLY.items(), *HTTP_ONLY.items()):
        assert reason.strip(), f"{name} is excluded without saying why"


def test_every_shared_operation_has_a_parity_scenario() -> None:
    registry = default_registry()
    shared = {
        operation.name
        for operation in registry
        if registry.transports_for(operation.name) >= frozenset({"cli", "http", "mcp"})
    }
    assert shared == set(SCENARIOS), (
        "every operation on all three surfaces needs a parity scenario; "
        f"missing: {sorted(shared - set(SCENARIOS))}, "
        f"stale: {sorted(set(SCENARIOS) - shared)}"
    )
