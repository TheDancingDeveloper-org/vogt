"""The transport-neutral operation registry.

Every operation is defined once — name, scope, mutating flag, argument
schema, HTTP route, CLI path — and the CLI commands, FastAPI routes and MCP
tool list are generated from that definition. Cadastre's largest source of
MCP duplication was the same tool signatures hand-mirrored across a server, a
remote bridge and a registry; here there is one definition and the surfaces
are derived (DESIGN §4.1).
"""

from __future__ import annotations

from vogt.registry.operation import CliBinding, HttpRoute, Operation, Scope
from vogt.registry.registry import (
    HTTP_ONLY,
    LOCAL_ONLY,
    OperationRegistry,
    default_registry,
)

__all__ = [
    "HTTP_ONLY",
    "LOCAL_ONLY",
    "CliBinding",
    "HttpRoute",
    "Operation",
    "OperationRegistry",
    "Scope",
    "default_registry",
]
