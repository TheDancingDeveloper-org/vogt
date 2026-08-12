"""The MCP surface."""

from __future__ import annotations

import pytest

from vogt.adapters.mcp.surface import McpSurface
from vogt.application.context import AppContext
from vogt.registry.registry import RegistryError


@pytest.fixture
def surface(instance: AppContext) -> McpSurface:
    return McpSurface(context_factory=lambda: instance)


def test_tools_are_generated_from_the_registry(surface: McpSurface) -> None:
    tools = {tool.name: tool for tool in surface.list_tools()}
    assert "project_register" in tools
    assert "init" not in tools, "local-only operations are not MCP tools"

    register = tools["project_register"]
    assert register.mutating is True
    assert register.scope == "project.write"
    assert "reason" in register.input_schema["required"]


#: Parameter names that would mean "act as somebody else". Naming a
#: *subject* is fine and necessary — assigning work to an actor, issuing a
#: token bound to one, filtering an audit query by one. What FR-S2 forbids is
#: a parameter that changes who the *acting* principal is, because that would
#: let any caller forge the provenance on every audit row it writes.
IMPERSONATION_PARAMS = frozenset(
    {"principal", "as_actor", "acting_actor", "on_behalf_of", "caller", "identity"}
)


def test_no_tool_lets_a_caller_choose_who_it_is(surface: McpSurface) -> None:
    """FR-S2: the acting principal comes from authentication, never a field."""
    for tool in surface.list_tools():
        properties = set(tool.input_schema.get("properties", {}))
        forbidden = properties & IMPERSONATION_PARAMS
        assert not forbidden, (
            f"{tool.name} takes {sorted(forbidden)}, which would let a caller "
            "claim to be somebody else"
        )


def test_the_wire_shape_is_an_mcp_tool(surface: McpSurface) -> None:
    wire = surface.list_tools()[0].to_wire()
    assert set(wire) == {"name", "description", "inputSchema"}


def test_calling_a_tool_returns_plain_data(surface: McpSurface) -> None:
    result = surface.call_tool(
        "project_register",
        {
            "name": "Via MCP",
            "root_path": "/srv/mcp",
            "reason": "an agent registered this",
        },
    )
    assert result["project"]["slug"] == "via-mcp"

    events = surface.call_tool("events_list")
    assert events["events"][0]["kind"] == "project.registered"


def test_calling_an_unknown_tool_is_an_error(surface: McpSurface) -> None:
    with pytest.raises(RegistryError, match="unknown MCP tool"):
        surface.call_tool("nope")
