"""The registry itself, plus the two parity exclusion lists.

FR-A3: parity exclusions are **explicit named lists** that fail when stale in
either direction — an entry naming an operation that no longer exists is as
much a bug as an operation quietly missing from a surface. Both directions
are asserted in `tests/test_parity.py`.
"""

from __future__ import annotations

from collections.abc import Iterator, Mapping
from typing import Any, Literal

from vogt.registry.operation import Operation

Transport = Literal["cli", "http", "mcp"]
ALL_TRANSPORTS: frozenset[Transport] = frozenset({"cli", "http", "mcp"})

#: Operations that exist only where the data directory is (DESIGN §4).
#: Each entry carries its justification, so "why is this excluded" is
#: answerable without archaeology.
LOCAL_ONLY: Mapping[str, str] = {
    "init": (
        "Creates the instance in a local data directory. A running server "
        "already has one, so there is no meaningful remote semantics."
    ),
    "migrate": (
        "Brings the schema of the local data directory forward. Over HTTP it "
        "would be a request asking the server to change the ground it is "
        "standing on mid-flight; the server already migrates at startup "
        "(NFR-I3), so the remote case is served by restarting it."
    ),
    "serve": (
        "Takes over this process to listen on a port. A running server being "
        "asked over its own API to start another one is not a meaningful "
        "request; restarting a service is the supervisor's job."
    ),
    "backup": (
        "Writes to a path on the machine holding the data directory. Over "
        "HTTP the path would name a filesystem the caller cannot see."
    ),
    "restore": (
        "Replaces the live stores from a path on the server's filesystem, "
        "and every client's view of the instance changes underneath them. "
        "That is an operator action taken at the machine, not a request."
    ),
    "import": (
        "Reads a file from the machine holding the data directory, for the "
        "same reason as backup."
    ),
    "mcp.stdio": (
        "Takes over this process's stdin and stdout to speak MCP. That is "
        "meaningful only where the data directory is; a remote client uses "
        "the streamable-HTTP transport at /mcp, which arrives at M4. "
        "Offering it as a REST route would mean a server hijacking its own "
        "stdout."
    ),
}

#: Operations that exist only over HTTP. Empty at M0, and live: an entry
#: naming an unregistered operation fails the parity test.
HTTP_ONLY: Mapping[str, str] = {}


class RegistryError(Exception):
    """The registry is internally inconsistent — always a programming error."""


class OperationRegistry:
    """An ordered collection of operations, validated on build."""

    def __init__(self, operations: list[Operation[Any, Any]]) -> None:
        self._operations: dict[str, Operation[Any, Any]] = {}
        for operation in operations:
            if operation.name in self._operations:
                msg = f"duplicate operation name: {operation.name}"
                raise RegistryError(msg)
            self._operations[operation.name] = operation
        self._validate()

    def __iter__(self) -> Iterator[Operation[Any, Any]]:
        return iter(self._operations.values())

    def __len__(self) -> int:
        return len(self._operations)

    def __contains__(self, name: object) -> bool:
        return name in self._operations

    @property
    def names(self) -> tuple[str, ...]:
        return tuple(self._operations)

    def get(self, name: str) -> Operation[Any, Any]:
        try:
            return self._operations[name]
        except KeyError as exc:
            msg = f"unknown operation: {name}"
            raise RegistryError(msg) from exc

    def by_mcp_tool(self, tool_name: str) -> Operation[Any, Any]:
        for operation in self:
            if operation.mcp_tool_name == tool_name:
                return operation
        msg = f"unknown MCP tool: {tool_name}"
        raise RegistryError(msg)

    def transports_for(self, name: str) -> frozenset[Transport]:
        """Which surfaces this operation is expected to appear on."""
        if name in LOCAL_ONLY:
            return frozenset({"cli"})
        if name in HTTP_ONLY:
            return frozenset({"http"})
        return ALL_TRANSPORTS

    def for_transport(self, transport: Transport) -> list[Operation[Any, Any]]:
        return [op for op in self if transport in self.transports_for(op.name)]

    # -- validation --------------------------------------------------------

    def _validate(self) -> None:
        self._validate_unique_bindings()
        self._validate_reasons()
        self._validate_exclusions()

    def _validate_unique_bindings(self) -> None:
        routes: dict[tuple[str, str], str] = {}
        cli_paths: dict[tuple[str, ...], str] = {}
        tools: dict[str, str] = {}
        for operation in self:
            route_key = (operation.route.method, operation.route.path)
            if route_key in routes:
                msg = (
                    f"route {route_key[0]} {route_key[1]} is claimed by both "
                    f"{routes[route_key]} and {operation.name}"
                )
                raise RegistryError(msg)
            routes[route_key] = operation.name

            if operation.cli.path in cli_paths:
                msg = (
                    f"CLI path {' '.join(operation.cli.path)} is claimed by both "
                    f"{cli_paths[operation.cli.path]} and {operation.name}"
                )
                raise RegistryError(msg)
            cli_paths[operation.cli.path] = operation.name

            if operation.mcp_tool_name in tools:
                msg = (
                    f"MCP tool {operation.mcp_tool_name} is claimed by both "
                    f"{tools[operation.mcp_tool_name]} and {operation.name}"
                )
                raise RegistryError(msg)
            tools[operation.mcp_tool_name] = operation.name

        self._validate_cli_tree(cli_paths)

    def _validate_cli_tree(self, cli_paths: dict[tuple[str, ...], str]) -> None:
        """A CLI command cannot be both a leaf and a group.

        argparse refuses to register `observations` as a subcommand and then
        reuse that name for `observations prune` — but it refuses at
        parser-build time, which is a long way from the definition that
        caused it. Catching it here names both operations.
        """
        for path, owner in sorted(cli_paths.items()):
            for other, other_owner in sorted(cli_paths.items()):
                if other != path and other[: len(path)] == path:
                    msg = (
                        f"CLI path {' '.join(path)!r} ({owner}) is a prefix of "
                        f"{' '.join(other)!r} ({other_owner}); a command "
                        "cannot be both a leaf and a group"
                    )
                    raise RegistryError(msg)

    def _validate_reasons(self) -> None:
        """Every mutating operation must take a required, non-empty reason.

        FR-S1 makes `reason` part of every audit row, and DESIGN §4.1 makes
        it the *only* caller-supplied audit field. Checking it here means a
        future write cannot be added without one — the rule is enforced at
        the definition site, not by review.
        """
        for operation in self:
            if not operation.mutating:
                continue
            field = operation.params_model.model_fields.get("reason")
            if field is None:
                msg = (
                    f"{operation.name} is mutating but its parameters have no "
                    "reason field (FR-S1)"
                )
                raise RegistryError(msg)
            if not field.is_required():
                msg = (
                    f"{operation.name} has an optional reason; a defaulted "
                    "reason explains nothing (FR-S1)"
                )
                raise RegistryError(msg)

    def _validate_exclusions(self) -> None:
        for name in (*LOCAL_ONLY, *HTTP_ONLY):
            if name not in self._operations:
                msg = (
                    f"parity exclusion names {name!r}, which is not a registered "
                    "operation — the list is stale (FR-A3)"
                )
                raise RegistryError(msg)
        overlap = set(LOCAL_ONLY) & set(HTTP_ONLY)
        if overlap:
            msg = f"operations excluded twice: {sorted(overlap)}"
            raise RegistryError(msg)


def default_registry() -> OperationRegistry:
    """Build the registry every adapter uses."""
    from vogt.registry.operations import build_operations

    return OperationRegistry(build_operations())
