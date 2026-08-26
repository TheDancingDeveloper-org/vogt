"""The REST surface, generated from the operation registry.

Routes are not written here — they are derived, one per registered operation,
which is what makes "nothing is client-only" (FR-A1) structurally true rather
than a review convention. Every front end — the PWA the engine ships, the CLI,
an MCP client — consumes exactly this surface.

Each endpoint is built with a real signature (`params: TheModel`), so FastAPI
documents request bodies and query parameters, validates repeated query
values into lists, and emits a complete OpenAPI document (FR-A4) — rather
than the registry having to describe itself twice.

`serve` and the health endpoints (FR-A7) arrive with the service stage at M4;
this adapter is the application it will serve. The core serves the API and
nothing else: the product's front end is the Solid PWA, which the engine
serves from `web/`, so "nothing is client-only" stays structurally true.
"""

from __future__ import annotations

import inspect
from collections.abc import Callable
from dataclasses import replace
from typing import Annotated, Any

from fastapi import FastAPI, Query, Request, Response
from fastapi.exceptions import RequestValidationError
from fastapi.responses import JSONResponse
from pydantic import BaseModel
from starlette.types import Lifespan

from vogt import __version__
from vogt.adapters.http.etag import (
    etag_for_revision,
    if_none_match_matches,
    is_conditional_read,
)
from vogt.application.context import AppContext, build_context
from vogt.application.identity import identity_from_headers
from vogt.application.services.auth import Authenticated, authorize
from vogt.errors import VogtError
from vogt.registry import OperationRegistry, default_registry
from vogt.registry.operation import Operation

API_PREFIX = "/api"

ContextFactory = Callable[[], AppContext]
#: Resolves a request to a context and an authenticated caller. Injected
#: by `serve`; absent on the unauthenticated loopback surface.
RequestResolver = Callable[[Request], "tuple[AppContext, Authenticated]"]


def build_app(
    *,
    registry: OperationRegistry | None = None,
    context_factory: ContextFactory | None = None,
    authorize_request: RequestResolver | None = None,
    writes_enabled: bool = True,
    lifespan: Lifespan[Any] | None = None,
) -> FastAPI:
    """Build the FastAPI application for one instance.

    `authorize_request` is how `serve` injects authentication. When it is
    absent the surface is unauthenticated — the loopback topology, where the
    caller already has the data directory (`DEPLOYMENT.md` §3). When it is
    present, every route resolves a principal and checks its scope before
    the handler runs, and the decision is recorded either way (FR-S5).
    """
    active_registry = registry if registry is not None else default_registry()
    factory: ContextFactory = (
        context_factory if context_factory is not None else build_context
    )

    app = FastAPI(
        title="Vogt",
        version=__version__,
        # `serve` passes one, to run the collector schedule (FR-L3) for
        # exactly as long as the listener is up. Absent everywhere else: a
        # library caller building the app should not acquire a background
        # writer by doing so.
        lifespan=lifespan,
        description=(
            "Every capability is available here, on the CLI, and over MCP — "
            "all three generated from one operation registry."
        ),
    )

    for operation in active_registry.for_transport("http"):
        app.add_api_route(
            f"{API_PREFIX}{operation.route.path}",
            _endpoint_for(
                operation,
                factory,
                authorize_request=authorize_request,
                writes_enabled=writes_enabled,
            ),
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


def _with_public_identity(ctx: AppContext, request: Request) -> AppContext:
    """Let a front door say where this request arrived (FR-A8, MERGE §5.3).

    `connect` renders an address, and behind a door the address is the door's.
    The context's own identity is the base, so an instance that is not
    configured as `fronted` — or one whose door said nothing — is returned
    untouched rather than rebuilt.
    """
    identity = identity_from_headers(
        ctx.config, request.headers, base=ctx.public_identity
    )
    if identity == ctx.public_identity:
        return ctx
    return replace(ctx, public_identity=identity)


def _endpoint_for(
    operation: Operation[Any, Any],
    factory: ContextFactory,
    *,
    authorize_request: RequestResolver | None = None,
    writes_enabled: bool = True,
) -> Callable[..., Any]:
    """Build one route handler with the signature FastAPI needs.

    The signature is assembled rather than written because the parameter type
    differs per operation. Reads take their model as query parameters, writes
    take it as a JSON body — the same model either way, so the CLI flag, the
    query key and the body field always share a name.

    `Request` is always in the signature, even unauthenticated: one shape is
    easier to reason about than two, and FastAPI injects it for free.
    """
    del writes_enabled  # the grant carries it; kept for call-site clarity
    params_model = operation.params_model
    is_read = operation.route.method == "GET"
    annotation: Any = Annotated[params_model, Query()] if is_read else params_model

    async def endpoint(
        params: BaseModel, request: Request, response: Response
    ) -> BaseModel | Response:
        if authorize_request is None:
            ctx = factory()
        else:
            ctx, caller = authorize_request(request)
            # The second gate, recorded either way (FR-S4, FR-S5).
            authorize(
                ctx,
                caller,
                operation=operation.name,
                scope=operation.scope,
                mutating=operation.mutating,
                transport="http",
            )
        if is_conditional_read(operation.name, method=operation.route.method):
            # This is intentionally after authentication and authorization:
            # an old validator must never turn a refused request into a 304.
            with ctx.declared.read() as view:
                etag = etag_for_revision(view.current_revision())
            response.headers["ETag"] = etag
            if if_none_match_matches(request.headers.get("if-none-match"), etag):
                return Response(status_code=304, headers={"ETag": etag})
        # Resolved here, not in the factory: only a request carries what the
        # front door stated, and only the adapter should read a request
        # (`identity.py`). A no-op unless this instance is configured as
        # fronted and a door actually said something.
        result: BaseModel = operation.run(_with_public_identity(ctx, request), params)
        return result

    endpoint.__name__ = operation.name.replace(".", "_")
    endpoint.__doc__ = operation.summary
    endpoint.__annotations__ = {
        "params": annotation,
        "request": Request,
        "response": Response,
        "return": operation.result_model,
    }
    endpoint.__signature__ = inspect.Signature(  # type: ignore[attr-defined]
        [
            inspect.Parameter(
                "params", inspect.Parameter.POSITIONAL_OR_KEYWORD, annotation=annotation
            ),
            inspect.Parameter(
                "request", inspect.Parameter.POSITIONAL_OR_KEYWORD, annotation=Request
            ),
            inspect.Parameter(
                "response", inspect.Parameter.POSITIONAL_OR_KEYWORD, annotation=Response
            ),
        ],
        return_annotation=operation.result_model,
    )
    return endpoint
