"""MCP over streamable HTTP, on the same port as everything else.

The remote transport (FR-A5). It reuses the stdio server's message handling
verbatim — one dispatcher, two transports — because the alternative is the
duplication cadastre ended up with, where the same twenty tool signatures
were hand-mirrored across a server, a bridge and a registry.

Scope filtering happens here (FR-S4): `tools/list` returns exactly what this
caller may invoke, and `tools/call` checks again. Ungranted tools are absent
rather than present-and-refusing.
"""

from __future__ import annotations

import asyncio
from collections.abc import Callable
from typing import Any

from fastapi import APIRouter, FastAPI, Request
from fastapi.responses import JSONResponse

from vogt.adapters.mcp.stdio import (
    INVALID_PARAMS,
    METHOD_NOT_FOUND,
    MessageId,
    negotiate_protocol_version,
)
from vogt.adapters.mcp.surface import McpSurface
from vogt.application.context import AppContext
from vogt.application.services.auth import (
    Authenticated,
    Forbidden,
    authorize,
)
from vogt.errors import VogtError
from vogt.registry import OperationRegistry

MCP_PATH = "/mcp"

Resolver = Callable[[Request], "tuple[AppContext, Authenticated]"]


def add_mcp_route(
    app: FastAPI,
    *,
    registry: OperationRegistry,
    resolve: Resolver,
    path: str = MCP_PATH,
) -> None:
    """Mount the streamable-HTTP MCP transport."""
    router = APIRouter()

    @router.post(path, tags=["mcp"])
    async def mcp(request: Request) -> JSONResponse:
        try:
            message = await request.json()
        except Exception:
            return JSONResponse(
                _error(None, INVALID_PARAMS, "body is not valid JSON"), status_code=200
            )
        if not isinstance(message, dict):
            return JSONResponse(
                _error(None, INVALID_PARAMS, "message must be a JSON object"),
                status_code=200,
            )

        # resolve() sets the request-actor contextvar the access log reads
        # after this handler returns, so it must run on the event loop, not a
        # worker thread (a contextvar mutated in a thread would not propagate
        # back). _handle() — the authorize write and the operation dispatch —
        # is the slow part, so only it is offloaded (#525).
        ctx, caller = resolve(request)
        response = await asyncio.to_thread(
            _handle, message, ctx=ctx, caller=caller, registry=registry
        )
        if response is None:
            # A notification. 202 with no body is the streamable-HTTP shape.
            return JSONResponse(content=None, status_code=202)
        return JSONResponse(response, status_code=200)

    app.include_router(router)


def _handle(
    message: dict[str, Any],
    *,
    ctx: AppContext,
    caller: Authenticated,
    registry: OperationRegistry,
) -> dict[str, Any] | None:
    raw_id = message.get("id")
    message_id: MessageId = raw_id if isinstance(raw_id, str | int) else None
    method = message.get("method")
    params = message.get("params") or {}
    if not isinstance(method, str):
        return _error(message_id, INVALID_PARAMS, "missing method")
    if not isinstance(params, dict):
        return _error(message_id, INVALID_PARAMS, "params must be an object")
    if raw_id is None:
        return None

    if method == "initialize":
        return _result(
            message_id,
            {
                "protocolVersion": negotiate_protocol_version(
                    params.get("protocolVersion")
                ),
                "capabilities": {"tools": {"listChanged": False}},
                "serverInfo": {"name": "vogt", "version": _version()},
            },
        )
    if method == "ping":
        return _result(message_id, {})
    if method == "tools/list":
        surface = McpSurface(registry=registry, context_factory=lambda: ctx)
        visible = [
            tool.to_wire()
            for tool in surface.list_tools()
            if caller.grant.allows(tool.scope, mutating=tool.mutating)[0]
        ]
        return _result(message_id, {"tools": visible})
    if method == "tools/call":
        return _call(message_id, params, ctx=ctx, caller=caller, registry=registry)
    return _error(message_id, METHOD_NOT_FOUND, f"unknown method {method!r}")


def _call(
    message_id: MessageId,
    params: dict[str, Any],
    *,
    ctx: AppContext,
    caller: Authenticated,
    registry: OperationRegistry,
) -> dict[str, Any]:
    name = params.get("name")
    if not isinstance(name, str):
        return _error(message_id, INVALID_PARAMS, "tools/call needs a tool name")
    arguments = params.get("arguments") or {}
    if not isinstance(arguments, dict):
        return _error(message_id, INVALID_PARAMS, "arguments must be an object")

    try:
        operation = registry.by_mcp_tool(name)
    except Exception:
        return _error(message_id, METHOD_NOT_FOUND, f"unknown tool {name!r}")

    # tools/call honors the transport exclusion just as tools/list does
    # (FR-A5/FR-S4). LOCAL_ONLY operations (restore/backup/serve/import/init/
    # migrate/mcp.stdio) are invisible to the remote MCP transport and must
    # not be dispatchable through it — the same invisible-tool rule, applied
    # to the call path rather than only the listing.
    if "mcp" not in registry.transports_for(operation.name):
        return _error(message_id, METHOD_NOT_FOUND, f"unknown tool {name!r}")

    try:
        # The second gate, and it is recorded either way (FR-S4, FR-S5).
        authorize(
            ctx,
            caller,
            operation=operation.name,
            scope=operation.scope,
            mutating=operation.mutating,
            transport="mcp-http",
        )
    except Forbidden as exc:
        return _result(message_id, _tool_error("forbidden", str(exc)))

    try:
        payload = operation.run_raw(ctx, arguments)
    except VogtError as exc:
        return _result(message_id, _tool_error(exc.code, str(exc)))
    except Exception as exc:  # a tool must not kill the session
        return _result(message_id, _tool_error("error", str(exc)))

    body: dict[str, Any] = payload.model_dump(mode="json")
    import json as _json

    return _result(
        message_id,
        {
            "content": [{"type": "text", "text": _json.dumps(body, indent=2)}],
            "structuredContent": body,
            "isError": False,
        },
    )


def _version() -> str:
    from vogt import __version__

    return __version__


def _result(message_id: MessageId, result: dict[str, Any]) -> dict[str, Any]:
    return {"jsonrpc": "2.0", "id": message_id, "result": result}


def _error(
    message_id: MessageId,
    code: int,
    message: str,
    *,
    data: dict[str, Any] | None = None,
) -> dict[str, Any]:
    error: dict[str, Any] = {"code": code, "message": message}
    if data is not None:
        error["data"] = data
    return {"jsonrpc": "2.0", "id": message_id, "error": error}


def _tool_error(code: str, message: str) -> dict[str, Any]:
    return {
        "content": [{"type": "text", "text": f"{code}: {message}"}],
        "isError": True,
    }
