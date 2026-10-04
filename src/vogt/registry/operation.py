"""What one operation is, independently of how it is reached."""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass
from typing import Any, Generic, Literal, TypeVar

from pydantic import BaseModel, ValidationError

from vogt.application.context import AppContext
from vogt.errors import InvalidParams

#: Authorization scopes. Every operation declares what it needs; the tokens
#: that grant them are issued separately, and nothing gates on them until
#: authentication is switched on.
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
        """Validate untyped input from a transport, then run.

        A validation failure becomes `InvalidParams` naming what to change
        (`describe_invalid`), rather than pydantic's report.
        """
        try:
            params = self.params_model.model_validate(raw)
        except ValidationError as exc:
            raise InvalidParams(describe_invalid(self, exc)) from exc
        return self.handler(ctx, params)


#: What a missing `reason` is told. Every write is audited and the registry
#: refuses to build a write whose reason is optional (ARCHITECTURE.md), so
#: the reason cannot be defaulted — but the refusal can say exactly what is
#: wanted rather than "Field required".
REASON_HINT = (
    "every write is audited and must say why it is being made — pass "
    '`reason` as a short sentence, e.g. reason="tests pass, ready for review"'
)


def describe_invalid(operation: Operation[Any, Any], exc: ValidationError) -> str:
    """A validation failure as an instruction: what is wrong, what is accepted."""
    fields = operation.params_model.model_fields
    problems: list[str] = []
    for error in exc.errors():
        location = ".".join(str(part) for part in error["loc"])
        kind = error["type"]
        if kind == "missing" and location == "reason":
            problems.append(f"missing required parameter 'reason': {REASON_HINT}")
        elif kind == "missing":
            problems.append(f"missing required parameter {location!r}")
        elif kind == "extra_forbidden":
            problems.append(f"unknown parameter {location!r}")
        elif location:
            problems.append(f"{location}: {error['msg']}")
        else:
            problems.append(str(error["msg"]))
    required = [name for name, field in fields.items() if field.is_required()]
    optional = [name for name, field in fields.items() if not field.is_required()]
    accepted = f"required: {', '.join(required) or 'none'}"
    if optional:
        accepted += f"; optional: {', '.join(optional)}"
    return f"{'; '.join(problems)}. {operation.mcp_tool_name} takes {accepted}"
