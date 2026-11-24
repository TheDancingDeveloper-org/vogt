"""The core's Prometheus listener (WI-1380).

Off unless ``VOGT_METRICS_ADDR`` is set, and on its own port rather than the
API one. What it serves first is the process: resident memory, CPU time, open
file descriptors and GC, each labelled ``component="core"`` so a scrape can
tell the core from the engine without relying on which target it came from.
"""

from __future__ import annotations

import socket
import urllib.request

import pytest

from vogt.adapters.http import metrics
from vogt.errors import InvalidRequest


def _free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


def _scrape(port: int) -> str:
    with urllib.request.urlopen(f"http://127.0.0.1:{port}/metrics") as resp:
        assert resp.status == 200
        return str(resp.read().decode())


def _series(body: str, name: str) -> list[str]:
    """The sample lines of one metric, comments excluded."""
    return [
        line
        for line in body.splitlines()
        if line.startswith(name) and not line.startswith("#")
    ]


def test_nothing_listens_without_the_env() -> None:
    assert metrics.start_from_env({}) is False
    assert metrics._server is None


def test_startup_starts_the_listener_when_asked() -> None:
    port = _free_port()
    try:
        assert metrics.start_from_env({metrics.METRICS_ENV: f"127.0.0.1:{port}"})
        body = _scrape(port)
    finally:
        metrics.stop()
    assert _series(body, "process_resident_memory_bytes")


def test_a_bad_address_is_refused() -> None:
    with pytest.raises(InvalidRequest):
        metrics.parse_addr("9465")
    with pytest.raises(InvalidRequest):
        metrics.parse_addr(":9465")


def test_every_series_is_labelled_as_the_core() -> None:
    port = _free_port()
    with metrics.serve_for_test(f"127.0.0.1:{port}"):
        body = _scrape(port)
    # The names prometheus_client's collectors publish, unchanged, so a panel
    # written against any process exporter reads this one. The label is on the
    # sample itself, not only on target_info.
    for name in (
        "process_resident_memory_bytes",
        "process_cpu_seconds_total",
        "process_open_fds",
        "python_gc_collections_total",
    ):
        lines = _series(body, name)
        assert lines, name
        assert all('component="core"' in line for line in lines), lines


def test_a_scrape_logs_nothing(caplog: pytest.LogCaptureFixture) -> None:
    port = _free_port()
    with metrics.serve_for_test(f"127.0.0.1:{port}"), caplog.at_level("DEBUG"):
        # The one line at start-up is wanted; what must stay quiet is the
        # scrape itself, which a Prometheus server repeats forever.
        caplog.clear()
        _scrape(port)
        _scrape(port)
        assert not caplog.records


def test_starting_twice_does_not_rebind() -> None:
    port = _free_port()
    with metrics.serve_for_test(f"127.0.0.1:{port}"):
        metrics.start(f"127.0.0.1:{port}")
        assert _series(_scrape(port), "process_resident_memory_bytes")
    assert metrics._server is None
