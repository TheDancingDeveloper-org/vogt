"""One structured line per request, with a duration on it (#139, NFR-OB1).

Written as a **raw ASGI middleware** rather than a Starlette
`BaseHTTPMiddleware`, and that is not a style preference: this application
serves `/mcp` as a long-lived SSE stream, and `BaseHTTPMiddleware` buffers a
response through a queue on a second task, which breaks streaming and loses
the context variables set by the handler it wraps. This wraps `send`, touches
nothing, and stays out of the way of the bytes.

Two durations are recorded because they answer different questions. `ttfb_ms`
is time to the response *starting* — the number that says an endpoint is
slow, and the only meaningful one for a stream that stays open for an hour.
`duration_ms` is the whole exchange, which is what a reader expects an access
log to say. The slow-request warning is judged on the first, so an SSE
subscription does not report itself as a pathological request every time a
client disconnects.
"""

from __future__ import annotations

from dataclasses import dataclass
from time import perf_counter
from typing import Any

from starlette.types import ASGIApp, Message, Receive, Scope, Send

from vogt.observability import (
    REQUEST_ID_HEADER,
    accepted_request_id,
    bind_request_id,
    current_actor,
    logger,
    new_request_id,
    reset_request_actor,
    reset_request_id,
)

log = logger("http")


@dataclass(frozen=True)
class AccessLogSettings:
    """What the operator decided about the access log."""

    #: Off makes this middleware a request-id middleware and nothing else:
    #: the correlation header is still honoured and echoed, because a
    #: deployment that turns the access log down still has to be able to
    #: follow one request across the two runtimes.
    enabled: bool = True
    #: Above this, a request is logged at WARNING instead of INFO, so
    #: pathological endpoints surface without anybody trawling.
    slow_request_ms: int = 1000
    #: Paths logged at DEBUG rather than INFO — health and readiness probes.
    #: Suppressed rather than dropped: a neighbouring container on the same
    #: host produced three thousand consecutive lines that were 100%
    #: `/healthz` and no application output at all, which is the failure mode
    #: this exists to avoid in both directions.
    quiet_paths: tuple[str, ...] = ()


class RequestLogMiddleware:
    """Assign a request id, then say what happened and how long it took."""

    def __init__(self, app: ASGIApp, *, settings: AccessLogSettings) -> None:
        self.app = app
        self.settings = settings

    async def __call__(self, scope: Scope, receive: Receive, send: Send) -> None:
        if scope["type"] != "http":
            await self.app(scope, receive, send)
            return

        request_id = (
            accepted_request_id(_header(scope, REQUEST_ID_HEADER)) or new_request_id()
        )
        token = bind_request_id(request_id)
        reset_request_actor()

        started = perf_counter()
        state: dict[str, Any] = {"status": 0, "bytes": 0, "ttfb": None, "logged": False}

        async def send_wrapper(message: Message) -> None:
            if message["type"] == "http.response.start":
                state["status"] = int(message["status"])
                state["ttfb"] = perf_counter() - started
                # The caller gets the id back whether or not it sent one, so
                # a slow answer can be quoted in a bug report and found in
                # the log.
                headers = list(message.get("headers") or [])
                headers.append((REQUEST_ID_HEADER.encode(), request_id.encode()))
                message["headers"] = headers
            elif message["type"] == "http.response.body":
                state["bytes"] += len(message.get("body") or b"")
            await send(message)

        try:
            await self.app(scope, receive, send_wrapper)
        except Exception as error:
            self._write(scope, state, started, failure=error)
            raise
        else:
            self._write(scope, state, started)
        finally:
            reset_request_id(token)
            reset_request_actor()

    def _write(
        self,
        scope: Scope,
        state: dict[str, Any],
        started: float,
        *,
        failure: BaseException | None = None,
    ) -> None:
        if not self.settings.enabled or state["logged"]:
            return
        state["logged"] = True

        duration_ms = round((perf_counter() - started) * 1000, 1)
        ttfb = state["ttfb"]
        ttfb_ms = duration_ms if ttfb is None else round(ttfb * 1000, 1)
        path = str(scope.get("path", ""))
        status = int(state["status"]) or (500 if failure is not None else 0)

        fields: dict[str, Any] = {
            "method": scope.get("method", ""),
            "path": path,
            "status": status,
            "duration_ms": duration_ms,
            "ttfb_ms": ttfb_ms,
            "bytes": state["bytes"],
        }
        query = scope.get("query_string") or b""
        if query:
            # The evidence in #138 was `?limit=1` appearing 2,643 times; a
            # path alone could not have said that. Vogt takes no credential
            # in a query parameter, so there is nothing here to redact.
            fields["query"] = query.decode("latin-1")
        client = scope.get("client")
        if client:
            fields["client"] = client[0]
        actor = current_actor()
        if actor:
            fields["actor"] = actor

        if failure is not None:
            log.error("request failed", extra={"vogt": fields}, exc_info=failure)
            return
        if status >= 500 or ttfb_ms >= self.settings.slow_request_ms:
            log.warning("slow or failed request", extra={"vogt": fields})
            return
        if path in self.settings.quiet_paths:
            log.debug("request", extra={"vogt": fields})
            return
        log.info("request", extra={"vogt": fields})


def _header(scope: Scope, name: str) -> str | None:
    wanted = name.encode()
    for key, value in scope.get("headers") or []:
        if bytes(key).lower() == wanted:
            return bytes(value).decode("latin-1")
    return None
