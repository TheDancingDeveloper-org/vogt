"""`vogt-agent-auth with VAR[=ENVNAME] -- CMD` (WI-1343).

The command runs with broker-fetched secrets in its environment, so `gh` can
read a granted forge token as `GH_TOKEN`. The value never reaches the child's
argv or a log line, a refused VAR runs nothing, and `--` is required.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any

import pytest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "engine" / "deploy" / "agent-auth.sh"
SECRET = "ghp_fakeSecretValue1343"

pytestmark = [
    pytest.mark.skipif(
        not SCRIPT.exists(), reason="the engine tree is absent (core-only run)"
    ),
    pytest.mark.skipif(shutil.which("curl") is None, reason="fetch uses curl"),
]


class _Broker(BaseHTTPRequestHandler):
    def do_POST(self) -> None:
        var = self.path.rsplit("/", 1)[-1]
        if self.headers.get("Authorization") != "Bearer broker-token":
            self._reply(401, b"bad token")
        elif var == "GRANT_X":
            self._reply(200, SECRET.encode())
        elif var == "OTHER":
            self._reply(200, b"other-value")
        else:
            self._reply(403, f"{var} is not in the manifest and has no grant".encode())

    def _reply(self, code: int, body: bytes) -> None:
        self.send_response(code)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *args: object) -> None:
        pass


@pytest.fixture(scope="module")
def broker() -> Any:
    srv = ThreadingHTTPServer(("127.0.0.1", 0), _Broker)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    yield f"http://127.0.0.1:{srv.server_address[1]}"
    srv.shutdown()


def _run(broker: str, *args: str) -> subprocess.CompletedProcess[str]:
    env = {
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "HOME": os.environ.get("HOME", "/tmp"),
        "VOGT_ENGINE_BROKER_URL": broker,
        "VOGT_ENGINE_BROKER_TOKEN": "broker-token",
    }
    return subprocess.run(
        ["bash", str(SCRIPT), "with", *args],
        env=env,
        capture_output=True,
        text=True,
        timeout=30,
        check=False,
    )


def test_maps_a_grant_into_the_child_environment(broker: str) -> None:
    res = _run(broker, "GRANT_X=GH_TOKEN", "OTHER", "--", "env")
    assert res.returncode == 0, res.stderr
    lines = res.stdout.splitlines()
    assert f"GH_TOKEN={SECRET}" in lines
    assert "OTHER=other-value" in lines
    assert not any(line.startswith("GRANT_X=") for line in lines)
    assert "GH_TOKEN" not in os.environ or os.environ["GH_TOKEN"] != SECRET
    # stderr names the VARs and the command's first word, never a value.
    assert "GRANT_X=GH_TOKEN OTHER -- env" in res.stderr
    assert SECRET not in res.stderr


def test_the_value_is_not_in_the_childs_argv(broker: str) -> None:
    res = _run(broker, "GRANT_X=GH_TOKEN", "--", "cat", "/proc/self/cmdline")
    assert res.returncode == 0, res.stderr
    assert res.stdout.split("\0") == ["cat", "/proc/self/cmdline", ""]
    assert SECRET not in res.stdout + res.stderr


def test_a_refused_var_runs_nothing(broker: str, tmp_path: Path) -> None:
    marker = tmp_path / "ran"
    res = _run(broker, "GRANT_X", "NOPE", "--", "touch", str(marker))
    assert res.returncode == 1
    assert "NOPE is not in the manifest and has no grant" in res.stderr
    assert not marker.exists()
    assert SECRET not in res.stderr


def test_double_dash_is_required(broker: str, tmp_path: Path) -> None:
    marker = tmp_path / "ran"
    res = _run(broker, "GRANT_X", "touch", str(marker))
    assert res.returncode == 1
    assert "requires --" in res.stderr
    assert not marker.exists()


@pytest.mark.parametrize(
    "args", [["--", "env"], ["GRANT_X", "--"], ["1BAD", "--", "env"]]
)
def test_rejects_malformed_invocations(broker: str, args: list[str]) -> None:
    res = _run(broker, *args)
    assert res.returncode == 1
    assert res.stdout == ""
