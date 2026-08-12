"""The REST surface, generated from the operation registry.

Routes are not written here — they are derived, one per registered operation,
which is what makes "nothing is GUI-only" (FR-A1) structurally true rather
than a review convention. The GUI at M6 consumes exactly this surface.

At M0 this adapter exists to be driven by the parity harness; `serve` and the
health endpoints (FR-A7) arrive with the service stage at M4.
"""

from __future__ import annotations

from collections.abc import Callable
from typing import Any

from fastapi import FastAPI, Request
from fastapi.responses import JSONResponse
from pydantic import BaseModel, ValidationError

from vogt import __version__
from vogt.application.context import AppContext, build_context
from vogt.errors import VogtError
from vogt.registry import OperationRegistry, default_registry
from vogt.registry.operation import Operation

API_PREFIX = "/api"

ContextFactory = Callable[[], AppContext]


def build_app(
    *,
    registry: OperationRegistry | None = None,
    context_factory: ContextFactory | None = None,
) -> FastAPI:
    """Build the FastAPI application for one instance."""
    active_registry = registry if registry is not None else default_registry()
    factory: ContextFactory = (
        context_factory if context_factory is not None else build_context
    )

    app = FastAPI(
        title="Vogt",
        version=__version__,
        description=(
            "Every capability is available here, on the CLI, and over MCP — "
            "all three generated from one operation registry."
        ),
    )

    for operation in active_registry.for_transport("http"):
        app.add_api_route(
            f"{API_PREFIX}{operation.route.path}",
            _endpoint_for(operation, factory),
            methods=[operation.route.method],
            name=operation.name,
            summary=operation.summary,
            response_model=operation.result_model,
            operation_id=operation.name.replace(".", "_"),
        )

    @app.exception_handler(VogtError)
    async def _vogt_error(request: Request, exc: VogtError) -> JSONResponse:
        del request
        return JSONResponse(
            status_code=exc.http_status,
            content={"error": {"code": exc.code, "message": str(exc)}},
        )

    return app


def _endpoint_for(
    operation: Operation[Any, Any], factory: ContextFactory
) -> Callable[[Request], Any]:
    """Build one route handler.

    Arguments are read from the query string for reads and the JSON body for
    writes, then validated by the operation's own parameter model — the same
    model the CLI builds its flags from.
    """

    async def endpoint(request: Request) -> BaseModel:
        raw: dict[str, Any]
        if operation.route.method == "GET":
            raw = dict(request.query_params)
        else:
            body = await request.body()
            raw = {} if not body else await request.json()
        try:
            params = operation.params_model.model_validate(raw)
        except ValidationError as exc:
            raise RequestInvalid(exc) from exc
        result: BaseModel = operation.run(factory(), params)
        return result

    endpoint.__name__ = operation.name.replace(".", "_")
    return endpoint


class RequestInvalid(VogtError):
    """The request body or query string did not match the parameter model."""

    code = "invalid_arguments"
    http_status = 422

    def __init__(self, error: ValidationError) -> None:
        super().__init__(str(error))
