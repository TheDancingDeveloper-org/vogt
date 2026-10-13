"""Prometheus metrics for the Python core (WI-1380).

Served on its own listener, ``VOGT_METRICS_ADDR``, never on the API port: the
API port is the front door and is reachable from wherever the GUI is; a scrape
endpoint belongs on the network the scraper sits on, like every other
exporter. Unset, nothing listens and nothing is imported that would start one.

The first question this answers is overall CPU and memory of the core
process. ``prometheus_client``'s process and platform collectors supply it:
``process_resident_memory_bytes``, ``process_cpu_seconds_total``, open file
descriptors and the GC collection counts. Their stock names are the ones a
scraper already knows, so they are not renamed. What tells the core from the
engine is a ``component="core"`` label on every series, not the scrape target:
a panel that sums both processes can still split them.

Request, database and MCP latency are later slices of the same endpoint.
"""

from __future__ import annotations

import os
import threading
from collections.abc import Iterable, Iterator
from contextlib import contextmanager
from wsgiref.simple_server import WSGIRequestHandler, WSGIServer, make_server

from prometheus_client import (
    CollectorRegistry,
    GCCollector,
    PlatformCollector,
    ProcessCollector,
    make_wsgi_app,
)
from prometheus_client.metrics_core import Metric
from prometheus_client.registry import Collector

from vogt.errors import InvalidRequest
from vogt.observability import logger

log = logger("metrics")

#: What distinguishes this process's series from the engine's in a scrape that
#: sees both. A constant, so it adds one series per metric and no cardinality.
COMPONENT = "core"
COMPONENT_LABEL = "component"

METRICS_ENV = "VOGT_METRICS_ADDR"

#: Suggested bind. 9464 is the engine's; 9466 is the voice sidecar's. The core
#: takes the one between them so the three can be scraped side by side.
DEFAULT_ADDR = "0.0.0.0:9465"

_server: WSGIServer | None = None
_thread: threading.Thread | None = None


class _Labelled:
    """A collector that adds one constant label to everything it wraps.

    ``prometheus_client``'s process and platform collectors take no const
    labels of their own, and ``target_info`` is not a label a query can group
    by, so the label has to be added to the samples after collection.
    """

    def __init__(self, inner: Collector, label: str, value: str) -> None:
        self._inner = inner
        self._label = label
        self._value = value

    def collect(self) -> Iterable[Metric]:
        for metric in self._inner.collect():
            for sample in metric.samples:
                sample.labels[self._label] = self._value
            yield metric


class _QuietHandler(WSGIRequestHandler):
    """A scrape is polled forever; one log line per poll would bury the log."""

    def log_message(self, format: str, *args: object) -> None:
        return


def registry() -> CollectorRegistry:
    """A registry holding only the process and platform collectors.

    Built fresh rather than using the global default registry: the default
    accumulates across a process, and a test that asks twice must see the same
    shape both times rather than a registry that grew between calls.
    """
    reg = CollectorRegistry()
    # These collectors register themselves as a side effect of construction,
    # and GCCollector refuses registry=None, so they land on a registry that is
    # thrown away. What is served is `reg`, which holds the labelled wrappers.
    discarded = CollectorRegistry()
    for collector in (
        ProcessCollector(registry=discarded),
        PlatformCollector(registry=discarded),
        GCCollector(registry=discarded),
    ):
        reg.register(_Labelled(collector, COMPONENT_LABEL, COMPONENT))
    return reg


def parse_addr(value: str) -> tuple[str, int]:
    """``host:port``. A bare port or a missing host is refused, not guessed."""
    host, sep, port = value.rpartition(":")
    if not sep or not host.strip() or not port.isdigit():
        msg = f"{METRICS_ENV}={value!r} is not an address like {DEFAULT_ADDR}"
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
    server = make_server(
        host, port, make_wsgi_app(registry()), handler_class=_QuietHandler
    )
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


def start_from_env(environ: os._Environ[str] | dict[str, str] | None = None) -> bool:
    """Start the listener when ``VOGT_METRICS_ADDR`` is set.

    The one place ``run`` decides. Returns whether a listener was asked for,
    so a test can exercise the decision without standing the API server up.
    """
    source = os.environ if environ is None else environ
    addr = source.get(METRICS_ENV, "").strip()
    if not addr:
        return False
    start(addr)
    return True


@contextmanager
def serve_for_test(addr: str) -> Iterator[None]:
    """Start the listener for the body of a test and always stop it."""
    start(addr)
    try:
        yield
    finally:
        stop()
