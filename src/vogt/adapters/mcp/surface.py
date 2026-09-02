"""The MCP surface, generated from the operation registry.

MCP is the primary surface (DESIGN §4.1): agents are the expected first
users. What lands at M0 is the part that matters for correctness — the tool
list and the dispatch, derived from the registry so no tool signature is ever
hand-mirrored. The stdio transport arrives at M1 and the streamable-HTTP
transport plus the `vogt-mcp-remote` bridge at M4; all three sit on top of
this surface rather than beside it.

Two rules are already visible in the shape here:

- **Identity is never a tool argument.** `call_tool` takes arguments and a
  context whose principal was already authenticated; there is no parameter a
  caller could use to claim to be someone else (FR-S2).
- **`reason` is a tool argument** on every mutating tool, and the registry
  refuses to build if one is missing.
"""

from __future__ import annotations

import weakref
from collections.abc import Callable
from dataclasses import dataclass
from typing import Any

from vogt.application.context import AppContext, build_context
from vogt.registry import OperationRegistry, default_registry
from vogt.registry.operation import Scope

ContextFactory = Callable[[], AppContext]

#: The wire tool list is a pure function of the registry, which is static per
#: process — but `tools/list` regenerated ~60 pydantic JSON schemas on every
#: call, and MCP clients send it on connect and sometimes repeatedly (#529.5).
#: Memoize it per registry object; a WeakKeyDictionary so a test's throwaway
#: registry is not pinned for the life of the process.
_TOOL_CACHE: "weakref.WeakKeyDictionary[OperationRegistry, list[McpTool]]" = (
    weakref.WeakKeyDictionary()
)


@dataclass(frozen=True)
class McpTool:
    """One entry of `tools/list`."""

    name: str
    description: str
    input_schema: dict[str, Any]
    mutating: bool
    scope: Scope

    def to_wire(self) -> dict[str, Any]:
        """The MCP `Tool` shape, ready for a transport to serialise."""
        return {
            "name": self.name,
            "description": self.description,
            "inputSchema": self.input_schema,
        }


class McpSurface:
    """`tools/list` and `tools/call` over the operation registry."""

    def __init__(
        self,
        *,
        registry: OperationRegistry | None = None,
        context_factory: ContextFactory | None = None,
    ) -> None:
        self._registry = registry if registry is not None else default_registry()
        self._factory: ContextFactory = (
            context_factory if context_factory is not None else build_context
        )

    def list_tools(self) -> list[McpTool]:
        """Every operation reachable over MCP, as a tool.

        Scope filtering — an agent's tool list being exactly what it may do
        (FR-S4) — arrives with tokens at M4. Until then there is no
        authentication to filter by, and pretending otherwise would be a gate
        that gates nothing.

        Memoized per registry (#529.5): the registry is static per process, so
        the ~60 JSON schemas are generated once, not per `tools/list`.
        """
        cached = _TOOL_CACHE.get(self._registry)
        if cached is not None:
            return cached
        tools = [
            McpTool(
                name=operation.mcp_tool_name,
                description=operation.summary,
                input_schema=operation.params_model.model_json_schema(),
                mutating=operation.mutating,
                scope=operation.scope,
            )
            for operation in self._registry.for_transport("mcp")
        ]
        _TOOL_CACHE[self._registry] = tools
        return tools

    def call_tool(
        self, name: str, arguments: dict[str, Any] | None = None
    ) -> dict[str, Any]:
        """Invoke one tool and return its result as plain JSON-able data."""
        operation = self._registry.by_mcp_tool(name)
        result = operation.run_raw(self._factory(), arguments or {})
        payload: dict[str, Any] = result.model_dump(mode="json")
        return payload
