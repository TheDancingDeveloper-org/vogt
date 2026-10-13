"""Prometheus metrics for the Python core (WI-1380).

Served on its own listener, ``VOGT_METRICS_ADDR``, never on the API port: the
API port is the front door and is reachable from wherever the GUI is; a scrape
endpoint belongs on the network the scraper sits on, like every other
exporter. Unset, nothing listens and nothing is imported that would start one.

The first question this answers is overall CPU and memory of the core
process. ``prometheus_client``'s process and platform collectors supply it:
``process_resident_memory_bytes``, ``process_cpu_seconds_total``, open file
descriptors and the GC collection counts. Their stock names are the ones a
scraper already knows, so they are not renamed; ``component="core"`` is what
lets a panel that scrapes both processes tell the core from the engine.

Request, database and MCP latency are later slices of the same endpoint.
"""

from __future__ import annotations

import threading
from collections.abc import Iterator
from contextlib import contextmanager
from wsgiref.simple_server import WSGIServer, make_server

from prometheus_client import (
    CollectorRegistry,
    GCCollector,
    PlatformCollector,
    ProcessCollector,
    make_wsgi_app,
)

from vogt.errors import InvalidRequest
from vogt.observability import logger

log = logger("metrics")

#: What distinguishes this process's series from the engine's in a scrape that
#: sees both. A constant, so it adds one series per metric and no cardinality.
COMPONENT = "core"

METRICS_ENV = "VOGT_METRICS_ADDR"

_server: WSGIServer | None = None
_thread: threading.Thread | None = None


def registry() -> CollectorRegistry:
    """A registry holding only the process and platform collectors.

    Built fresh rather than using the global default registry: the default
    accumulates across a process, and a test that asks twice must see the same
    shape both times rather than a registry that grew between calls.
    """
    reg = CollectorRegistry()
    ProcessCollector(registry=reg)
    PlatformCollector(registry=reg)
    GCCollector(registry=reg)
    reg.set_target_info({"component": COMPONENT})
    return reg


def parse_addr(value: str) -> tuple[str, int]:
    """``host:port``. A bare port or a missing host is refused, not guessed."""
    host, sep, port = value.rpartition(":")
    if not sep or not host.strip() or not port.isdigit():
        msg = f"{METRICS_ENV}={value!r} is not an address like 0.0.0.0:9465"
        raise InvalidRequest(msg)
    return host, int(port)


def start(addr: str) -> None:
    """Listen for ``GET /metrics`` on ``addr``, in a daemon thread.

    Idempotent: a second call while a listener is up changes nothing, so a
    reload that calls it again cannot bind the port twice.
    """
    global _server, _thread
    if _server is not None:
        return
    host, port = parse_addr(addr)
    server = make_server(host, port, make_wsgi_app(registry()))
    thread = threading.Thread(
        target=server.serve_forever, name="vogt-metrics", daemon=True
    )
    thread.start()
    _server = server
    _thread = thread
    log.info(
        "metrics listening",
        extra={"vogt": {"bind": f"{host}:{port}", "component": COMPONENT}},
    )


def stop() -> None:
    """Shut the listener down. A no-op when none was started."""
    global _server, _thread
    server, thread = _server, _thread
    _server = None
    _thread = None
    if server is not None:
        server.shutdown()
        server.server_close()
    if thread is not None:
        thread.join(timeout=5)


@contextmanager
def serve_for_test(addr: str) -> Iterator[None]:
    """Start the listener for the body of a test and always stop it."""
    start(addr)
    try:
        yield
    finally:
        stop()
