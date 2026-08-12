"""What one operation is, independently of how it is reached."""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass
from typing import Generic, Literal, TypeVar

from pydantic import BaseModel

from vogt.application.context import AppContext

#: Authorization scopes (FR-S3). Carried from M0 so that every operation
#: declares what it needs; the tokens that grant them arrive at M4, and
#: nothing gates on them before that.
Scope = Literal["read", "work.write", "project.write", "admin", "writeback"]

HttpMethod = Literal["GET", "POST", "PATCH", "DELETE"]

P = TypeVar("P", bound=BaseModel)
R = TypeVar("R", bound=BaseModel)


@dataclass(frozen=True)
class HttpRoute:
    """Where this operation lives on the REST surface."""

    method: HttpMethod
    path: str

    def __post_init__(self) -> None:
        if not self.path.startswith("/"):
            msg = f"route path must start with '/': {self.path}"
            raise ValueError(msg)


@dataclass(frozen=True)
class CliBinding:
    """The CLI command path, e.g. ``("project", "register")``."""

    path: tuple[str, ...]

    def __post_init__(self) -> None:
        if not self.path:
            msg = "CLI binding needs at least one path segment"
            raise ValueError(msg)


@dataclass(frozen=True)
class Operation(Generic[P, R]):
    """One capability of the product, in one place."""

    name: str
    summary: str
    scope: Scope
    mutating: bool
    params_model: type[P]
    result_model: type[R]
    handler: Callable[[AppContext, P], R]
    route: HttpRoute
    cli: CliBinding

    @property
    def mcp_tool_name(self) -> str:
        """MCP tool names use underscores; everything else is identical."""
        return self.name.replace(".", "_")

    def run(self, ctx: AppContext, params: P) -> R:
        return self.handler(ctx, params)

    def run_raw(self, ctx: AppContext, raw: dict[str, object]) -> R:
        """Validate untyped input from a transport, then run."""
        return self.handler(ctx, self.params_model.model_validate(raw))
