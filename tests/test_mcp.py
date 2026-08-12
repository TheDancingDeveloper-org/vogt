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


def test_identity_is_never_a_tool_argument(surface: McpSurface) -> None:
    """FR-S2: a caller-supplied principal would let any token forge provenance."""
    for tool in surface.list_tools():
        properties = tool.input_schema.get("properties", {})
        assert "actor" not in properties
        assert "actor_id" not in properties or tool.name == "audit_list"
        assert "principal" not in properties


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
