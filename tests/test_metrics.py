"""The core's Prometheus listener (WI-1380).

Off unless ``VOGT_METRICS_ADDR`` is set, and on its own port rather than the
API one. What it serves first is the process: resident memory, CPU time, open
file descriptors and GC, labelled so a scrape can tell the core from the
engine.
"""

from __future__ import annotations

import os
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


def test_nothing_listens_without_the_env(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv(metrics.METRICS_ENV, raising=False)
    assert os.environ.get(metrics.METRICS_ENV) is None
    assert metrics._server is None


def test_a_bad_address_is_refused() -> None:
    with pytest.raises(InvalidRequest):
        metrics.parse_addr("9465")
    with pytest.raises(InvalidRequest):
        metrics.parse_addr(":9465")


def test_metrics_serves_process_collectors() -> None:
    port = _free_port()
    with metrics.serve_for_test(f"127.0.0.1:{port}"):
        body = _scrape(port)
    # The names prometheus_client's collectors publish, unchanged, so a panel
    # written against any process exporter reads this one.
    for name in (
        "process_resident_memory_bytes",
        "process_cpu_seconds_total",
        "process_open_fds",
        "python_gc_collections_total",
    ):
        assert f"\n{name} " in f"\n{body}" or f"\n{name}{{" in f"\n{body}"
    # And labelled as the core, not the engine.
    assert 'component="core"' in body


def test_starting_twice_does_not_rebind() -> None:
    port = _free_port()
    with metrics.serve_for_test(f"127.0.0.1:{port}"):
        metrics.start(f"127.0.0.1:{port}")
        assert "process_resident_memory_bytes" in _scrape(port)
    assert metrics._server is None
