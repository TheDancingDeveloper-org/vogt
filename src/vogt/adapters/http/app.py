"""The REST surface, generated from the operation registry.

Routes are not written here — they are derived, one per registered operation,
which is what makes "nothing is GUI-only" (FR-A1) structurally true rather
than a review convention. The GUI at M6 consumes exactly this surface.

Each endpoint is built with a real signature (`params: TheModel`), so FastAPI
documents request bodies and query parameters, validates repeated query
values into lists, and emits a complete OpenAPI document (FR-A4) — rather
than the registry having to describe itself twice.

`serve` and the health endpoints (FR-A7) arrive with the service stage at M4;
this adapter is the application it will serve.
"""

from __future__ import annotations

import inspect
from collections.abc import Callable
from typing import Annotated, Any

from fastapi import FastAPI, Query, Request
from fastapi.exceptions import RequestValidationError
from fastapi.responses import JSONResponse
from pydantic import BaseModel

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

    @app.exception_handler(RequestValidationError)
    async def _invalid_arguments(
        request: Request, exc: RequestValidationError
    ) -> JSONResponse:
        """Keep the error envelope identical to every other failure.

        FastAPI's default 422 body is shaped differently from the errors this
        application raises. One envelope means a client — or a parity test —
        handles failures the same way whatever produced them.
        """
        del request
        return JSONResponse(
            status_code=422,
            content={
                "error": {
                    "code": "invalid_arguments",
                    "message": "request does not match the operation's parameters",
                    "detail": _jsonable_errors(exc),
                }
            },
        )

    return app


def _jsonable_errors(exc: RequestValidationError) -> list[dict[str, Any]]:
    errors: list[dict[str, Any]] = []
    for error in exc.errors():
        errors.append(
            {
                "loc": [str(part) for part in error.get("loc", ())],
                "msg": str(error.get("msg", "")),
                "type": str(error.get("type", "")),
            }
        )
    return errors


def _endpoint_for(
    operation: Operation[Any, Any], factory: ContextFactory
) -> Callable[..., Any]:
    """Build one route handler with the signature FastAPI needs.

    The signature is assembled rather than written because the parameter type
    differs per operation. Reads take their model as query parameters, writes
    take it as a JSON body — the same model either way, so the CLI flag, the
    query key and the body field always share a name.
    """
    params_model = operation.params_model
    is_read = operation.route.method == "GET"
    annotation: Any = Annotated[params_model, Query()] if is_read else params_model

    async def endpoint(params: BaseModel) -> BaseModel:
        result: BaseModel = operation.run(factory(), params)
        return result

    endpoint.__name__ = operation.name.replace(".", "_")
    endpoint.__doc__ = operation.summary
    endpoint.__annotations__ = {"params": annotation, "return": operation.result_model}
    endpoint.__signature__ = inspect.Signature(  # type: ignore[attr-defined]
        [
            inspect.Parameter(
                "params",
                inspect.Parameter.POSITIONAL_OR_KEYWORD,
                annotation=annotation,
            )
        ],
        return_annotation=operation.result_model,
    )
    return endpoint
