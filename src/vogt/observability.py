"""Logging: one line per request, one convention for every module.

Vogt had five logging calls and one logger in the whole of `src/`, no
middleware, and no timing anywhere. During the 2026-08-19 `vogt-dev`
incident the logs could not answer the first question anybody asks — *which
request is slow, and how slow* — and the cause had to be inferred from the
frequency distribution of paths across three thousand uvicorn access lines
that carry no timestamp, no duration, and a client port of `:0` (#139). It
was found only because the offending requests were numerous. A single slow
endpoint would have left no trace at all.

So this module owns three things, and everything else in the tree consumes
them:

- **A convention.** Every logger is `vogt.<area>` — obtained through
  `logger()` rather than `logging.getLogger` — so a level can be raised for
  one area without silencing the rest, and so a reader can tell Vogt's own
  output from its dependencies' at a glance.
- **A correlation identifier.** `request_id` is a context variable set once
  per request by the access-log middleware, honouring the `X-Request-Id` the
  front door sends, and attached to *every* record emitted while that request
  is being served — not only the access line. That is what makes a slow
  request traceable across the engine and the core, which are two runtimes
  with two logging stacks and, until now, nothing in common.
- **Two renderings.** Human-readable text by default, and JSON when something
  is going to query it. Both carry the same fields, so a query written
  against one describes the other.

Structured fields travel as `extra={"vogt": {...}}` and are rendered as
`key=value` pairs in text and as top-level keys in JSON. Deliberately stdlib:
`structlog` and friends would be a fourth runtime dependency to render a line
this module renders in forty lines, and the one thing worth having from them
— context propagation — is a `ContextVar` either way.

Output goes to **stderr**, always. `vogt-mcp` speaks JSON-RPC on stdout, and
a log line on that stream is a protocol error rather than a diagnostic.
"""

from __future__ import annotations

import json
import logging
import re
import sys
import uuid
from contextvars import ContextVar, Token
from datetime import UTC, datetime
from typing import Any, Literal, TextIO

#: The header this core reads a correlation id from and echoes back. Lower
#: case because that is how ASGI presents it and how the engine sends it.
REQUEST_ID_HEADER = "x-request-id"

#: The namespace every Vogt logger lives under. One prefix means
#: `VOGT_LOG_LEVEL=debug` is a decision about Vogt and not about urllib3.
LOGGER_NAMESPACE = "vogt"

#: A correlation id is an opaque token from somewhere else, and it ends up in
#: a log line, so it is constrained rather than trusted: what a caller sends
#: is accepted only if it looks like an identifier, and is replaced if not.
_SAFE_REQUEST_ID = re.compile(r"^[A-Za-z0-9._-]{1,64}$")

_request_id: ContextVar[str | None] = ContextVar("vogt_request_id", default=None)
_actor: ContextVar[str | None] = ContextVar("vogt_request_actor", default=None)


def logger(area: str) -> logging.Logger:
    """The logger for one area of the code — `logger("http")`, `logger("sweep")`."""
    return logging.getLogger(f"{LOGGER_NAMESPACE}.{area}")


def new_request_id() -> str:
    """An id for a request that arrived without one."""
    return uuid.uuid4().hex[:16]


def accepted_request_id(raw: str | None) -> str | None:
    """The caller's correlation id, if it is one; `None` otherwise.

    A caller — the engine in front, an agent, a curl — may state the id so a
    request can be followed across processes. It is echoed into every line
    this process writes, so an unbounded or newline-carrying value would be a
    log-injection primitive rather than a diagnostic aid.
    """
    if raw is None:
        return None
    candidate = raw.strip()
    return candidate if _SAFE_REQUEST_ID.match(candidate) else None


def bind_request_id(request_id: str) -> Token[str | None]:
    """Bind an id for the current request. Reset with the returned token."""
    return _request_id.set(request_id)


def reset_request_id(token: Token[str | None]) -> None:
    _request_id.reset(token)


def current_request_id() -> str | None:
    return _request_id.get()


def set_request_actor(identity_ref: str | None) -> None:
    """Record who this request resolved to, once authentication has decided.

    Set from the adapter's resolver rather than derived here: the access log
    is written after the handler has run, and "which actor was hammering
    this endpoint" is the question the request-rate half of #138 needed and
    could not answer.
    """
    _actor.set(identity_ref)


def current_actor() -> str | None:
    return _actor.get()


def reset_request_actor() -> None:
    _actor.set(None)


class _ContextFilter(logging.Filter):
    """Attach the request context to every record, whoever emitted it."""

    def filter(self, record: logging.LogRecord) -> bool:
        record.request_id = _request_id.get()
        record.actor = _actor.get()
        return True


def _fields(record: logging.LogRecord) -> dict[str, Any]:
    raw = getattr(record, "vogt", None)
    return dict(raw) if isinstance(raw, dict) else {}


class TextFormatter(logging.Formatter):
    """`2026-08-19T11:02:03Z INFO vogt.http request method=GET …`.

    A timestamp, because the stock uvicorn line has none and "when" is half
    of every incident question.
    """

    def format(self, record: logging.LogRecord) -> str:
        stamp = datetime.fromtimestamp(record.created, tz=UTC).isoformat(
            timespec="milliseconds"
        )
        head = f"{stamp} {record.levelname:<7} {record.name} {record.getMessage()}"
        parts = _fields(record)
        request_id = getattr(record, "request_id", None)
        if request_id:
            parts = {"request_id": request_id, **parts}
        actor = getattr(record, "actor", None)
        if actor:
            parts = {**parts, "actor": actor}
        rendered = " ".join(f"{key}={_terse(value)}" for key, value in parts.items())
        line = f"{head} {rendered}" if rendered else head
        if record.exc_info:
            line = f"{line}\n{self.formatException(record.exc_info)}"
        return line


class JsonFormatter(logging.Formatter):
    """One JSON object per line, for a log that is queried rather than read."""

    def format(self, record: logging.LogRecord) -> str:
        payload: dict[str, Any] = {
            "ts": datetime.fromtimestamp(record.created, tz=UTC).isoformat(
                timespec="milliseconds"
            ),
            "level": record.levelname.lower(),
            "logger": record.name,
            "message": record.getMessage(),
        }
        request_id = getattr(record, "request_id", None)
        if request_id:
            payload["request_id"] = request_id
        actor = getattr(record, "actor", None)
        if actor:
            payload["actor"] = actor
        payload.update(_fields(record))
        if record.exc_info:
            payload["exception"] = self.formatException(record.exc_info)
        return json.dumps(payload, default=str)


def _terse(value: object) -> str:
    """Render one field value for the text format, quoting only when needed."""
    text = "" if value is None else str(value)
    return f'"{text}"' if (" " in text or text == "") else text


def configure_logging(
    *,
    level: str = "info",
    fmt: Literal["text", "json"] = "text",
    stream: TextIO | None = None,
) -> None:
    """Install Vogt's handler on the root logger. Safe to call twice.

    The root rather than `vogt.*`, because the point of a log is to hold
    everything that happened: uvicorn's startup errors and a dependency's
    warning belong in the same stream, in the same shape, with the same
    request id attached. Vogt's own namespace gets the configured level and
    the noisier libraries are left at whatever they choose.
    """
    handler = logging.StreamHandler(stream if stream is not None else sys.stderr)
    handler.setFormatter(JsonFormatter() if fmt == "json" else TextFormatter())
    handler.addFilter(_ContextFilter())
    handler.set_name("vogt")

    root = logging.getLogger()
    for existing in [h for h in root.handlers if h.get_name() == "vogt"]:
        root.removeHandler(existing)
    root.addHandler(handler)
    root.setLevel(logging.WARNING)
    logging.getLogger(LOGGER_NAMESPACE).setLevel(level.upper())

    # uvicorn's own access line is replaced, not supplemented: it carries no
    # timestamp and no duration, and logs every client port as `:0`, so two
    # connections from one host cannot be told apart. `serve` also passes
    # `access_log=False`; this covers the process that forgets to.
    access = logging.getLogger("uvicorn.access")
    access.handlers.clear()
    access.propagate = False
    for name in ("uvicorn", "uvicorn.error"):
        library = logging.getLogger(name)
        library.handlers.clear()
        library.propagate = True
