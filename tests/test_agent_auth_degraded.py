"""A session launch does not depend on the secrets manager (WI-1812).

On 2026-10-18 Infisical was down after a reboot, the engine's start path
blocked on `infisical login`, and vogt-prod restarted in a loop. A launch
(`vogt-agent-auth run`/`shell`) now bounds each secrets-manager call, and when
it cannot get the credentials it starts the command anyway, without them: it
warns in the session, exports `AGENT_AUTH_UNAVAILABLE` beside an accurate
`AGENT_AUTH_GRANTED`, drops the identity as always, and reports the launch to
the engine as `degraded`. Nothing is cached, so once the secrets manager is
back the next launch gets its credentials.
"""

from __future__ import annotations

import json
import os
import shutil
import signal
import socket
import subprocess
import threading
import time
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any, ClassVar

import pytest

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "engine" / "deploy" / "agent-auth.sh"
ENTRYPOINT = ROOT / "engine" / "deploy" / "entrypoint.sh"

pytestmark = [
    pytest.mark.skipif(
        not SCRIPT.exists(), reason="the engine tree is absent (core-only run)"
    ),
    pytest.mark.skipif(
        shutil.which("jq") is None
        or shutil.which("curl") is None
        or shutil.which("timeout") is None,
        reason="the launch uses curl, jq and timeout",
    ),
]


class _Fake(BaseHTTPRequestHandler):
    """Infisical's bulk read and the engine's launch-report route, together."""

    reports: ClassVar[list[dict[str, Any]]] = []
    # The API answers the login but a project read hangs: Infisical up, its
    # Redis or Postgres down.
    reads_hang: ClassVar[bool] = False

    def do_GET(self) -> None:
        if _Fake.reads_hang:
            time.sleep(8)
            return
        body = json.dumps(
            {"secrets": [{"secretKey": "ALPHA", "secretValue": "alpha-value"}]}
        ).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self) -> None:
        length = int(self.headers.get("Content-Length") or 0)
        _Fake.reports.append(json.loads(self.rfile.read(length)))
        self.send_response(204)
        self.end_headers()

    def log_message(self, *args: object) -> None:
        del args


@pytest.fixture
def server() -> Any:
    _Fake.reports = []
    _Fake.reads_hang = False
    srv = ThreadingHTTPServer(("127.0.0.1", 0), _Fake)
    thread = threading.Thread(target=srv.serve_forever, daemon=True)
    thread.start()
    yield f"http://127.0.0.1:{srv.server_port}"
    srv.shutdown()


def _bin(tmp_path: Path, *, infisical_up: bool) -> Path:
    bindir = tmp_path / "bin"
    bindir.mkdir(exist_ok=True)
    cli = bindir / "infisical"
    # Down: login hangs, as it did against an Infisical with no Redis.
    login = "printf tok-123" if infisical_up else "sleep 30"
    # Every CLI call is recorded; a per-secret read (`secrets get`) hangs too.
    cli.write_text(
        f'#!/bin/sh\necho "$1" >> {tmp_path}/cli-calls\n'
        f'[ "$1" = login ] && exec {login}\nexec sleep 30\n',
        encoding="utf-8",
    )
    cli.chmod(0o755)
    boot = bindir / "vogt-mcp-bootstrap"
    boot.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    boot.chmod(0o755)
    return bindir


def _launch(
    server: str,
    tmp_path: Path,
    *,
    infisical_up: bool,
    identity: bool = True,
    manifest: str = "ALPHA proj-a ALPHA\nLATER proj-a LATER ondemand",
) -> subprocess.CompletedProcess[str]:
    bindir = _bin(tmp_path, infisical_up=infisical_up)
    env = {
        "PATH": f"{bindir}:/usr/bin:/bin",
        "HOME": str(tmp_path),
        "TMPDIR": str(tmp_path),
        "VOGT_MCP_BOOTSTRAP": str(bindir / "vogt-mcp-bootstrap"),
        "ENGINE_AGENT_AUTH_TIMEOUT": "1",
        "ENGINE_AGENT_AUTH_SECRETS": manifest,
        "ENGINE_AGENT_AUTH_LAUNCH_BUDGET": "3",
        "VOGT_ENGINE_BROKER_URL": server,
        "VOGT_ENGINE_BROKER_TOKEN": "broker-tok",
    }
    if identity:
        env |= {
            "INFISICAL_API_URL": f"{server}/api",
            "INFISICAL_CLIENT_ID": "id",
            "INFISICAL_CLIENT_SECRET": "secret",
        }
    return subprocess.run(
        ["bash", str(SCRIPT), "run", "--", "env"],
        env=env,
        capture_output=True,
        text=True,
        check=False,
        timeout=30,
    )


def _env(out: str) -> dict[str, str]:
    return dict(line.split("=", 1) for line in out.splitlines() if "=" in line)


def test_infisical_down_the_session_still_launches_without_credentials(
    server: str, tmp_path: Path
) -> None:
    started = time.monotonic()
    proc = _launch(server, tmp_path, infisical_up=False)
    took = time.monotonic() - started

    assert proc.returncode == 0, proc.stderr
    assert took < 10, f"a hung login must be cut off by the timeout, took {took:.1f}s"
    env = _env(proc.stdout)
    assert "ALPHA" not in env
    assert env["AGENT_AUTH_GRANTED"] == "", "the breadcrumb names only what is held"
    assert env["AGENT_AUTH_ONDEMAND"] == "LATER"
    assert "timed out" in env["AGENT_AUTH_UNAVAILABLE"]
    assert "INFISICAL_CLIENT_SECRET" not in env, "the identity is dropped as always"
    assert "agent auth unavailable" in proc.stderr
    assert [r["outcome"] for r in _Fake.reports] == ["degraded"]
    assert "timed out" in _Fake.reports[0]["error"]


def test_hung_reads_are_not_retried_secret_by_secret(
    server: str, tmp_path: Path
) -> None:
    _Fake.reads_hang = True
    manifest = "\n".join(
        [f"A{i} proj-a A{i}" for i in range(8)]
        + [f"B{i} proj-b B{i}" for i in range(8)]
    )
    started = time.monotonic()
    proc = _launch(server, tmp_path, infisical_up=True, manifest=manifest)
    took = time.monotonic() - started

    assert proc.returncode == 0, proc.stderr
    # One budget for the whole launch, not 10 s per secret (WI-1812 review).
    assert took < 8, f"16 hung reads must share one budget, took {took:.1f}s"
    calls = (tmp_path / "cli-calls").read_text(encoding="utf-8").split()
    assert calls == ["login"], "no per-secret CLI fallback after an unanswered read"
    env = _env(proc.stdout)
    assert env["AGENT_AUTH_GRANTED"] == ""
    assert "did not answer" in env["AGENT_AUTH_UNAVAILABLE"]
    assert [r["outcome"] for r in _Fake.reports] == ["degraded"]


def test_no_identity_at_all_still_launches(server: str, tmp_path: Path) -> None:
    proc = _launch(server, tmp_path, infisical_up=False, identity=False)
    assert proc.returncode == 0, proc.stderr
    assert "no secrets-manager identity" in _env(proc.stdout)["AGENT_AUTH_UNAVAILABLE"]


def test_once_infisical_recovers_the_next_launch_gets_credentials(
    server: str, tmp_path: Path
) -> None:
    down = _launch(server, tmp_path, infisical_up=False)
    assert "AGENT_AUTH_UNAVAILABLE" in _env(down.stdout)

    up = _launch(server, tmp_path, infisical_up=True)
    assert up.returncode == 0, up.stderr
    env = _env(up.stdout)
    assert env["ALPHA"] == "alpha-value"
    assert env["AGENT_AUTH_GRANTED"] == "ALPHA"
    assert "AGENT_AUTH_UNAVAILABLE" not in env
    assert "agent auth unavailable" not in up.stderr
    assert [r["outcome"] for r in _Fake.reports] == ["degraded", "ok"]


ENGINE_BIN = ROOT / "engine" / "target" / "debug" / "vogt-engine"


def _free_port() -> int:
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return int(sock.getsockname()[1])


@pytest.mark.skipif(
    not ENGINE_BIN.exists(),
    reason="needs the engine built (cargo build -p vogt-engine-server)",
)
def test_the_engine_is_ready_while_agent_auth_hangs(tmp_path: Path) -> None:
    """Run the real entrypoint and the real engine with agent auth required
    and the helper hanging, as against an Infisical with no Redis: the engine
    must still bind and pass /readyz in its normal time."""
    helper = tmp_path / "hung-helper"
    helper.write_text("#!/bin/sh\nexec sleep 300\n", encoding="utf-8")
    helper.chmod(0o755)
    (tmp_path / "Working").mkdir()
    port = _free_port()
    env = {
        "PATH": "/usr/bin:/bin",
        "HOME": str(tmp_path),
        "VOGT_ENGINE_BIN": str(ENGINE_BIN),
        "ENGINE_TOKEN": "t" * 40,
        "ENGINE_BIND": f"127.0.0.1:{port}",
        "ENGINE_AGENT_AUTH_HELPER": str(helper),
        "ENGINE_AGENT_AUTH_REQUIRED": "1",
        "ENGINE_AGENT_POLICY_HOOK": "0",
        "ENGINE_AGENT_CONVERSATION_HOOK": "0",
        "ENGINE_CARGO_CACHE_PERSIST": "0",
    }
    log = (tmp_path / "entrypoint.log").open("w")
    proc = subprocess.Popen(
        ["bash", str(ENTRYPOINT)],
        env=env,
        stdout=log,
        stderr=subprocess.STDOUT,
        start_new_session=True,
    )
    try:
        started = time.monotonic()
        ready = None
        while time.monotonic() - started < 20 and proc.poll() is None:
            try:
                with urllib.request.urlopen(
                    f"http://127.0.0.1:{port}/readyz", timeout=1
                ) as response:
                    ready = response.status
                    break
            except (OSError, urllib.error.URLError):
                time.sleep(0.2)
        output = (tmp_path / "entrypoint.log").read_text(encoding="utf-8")
        assert proc.poll() is None, f"the container exited:\n{output}"
        assert ready == 200, f"/readyz not ready while agent auth hangs:\n{output}"
        assert "the engine does not wait for it" in output
    finally:
        os.killpg(proc.pid, signal.SIGKILL)
        proc.wait()
        log.close()


def test_the_entrypoint_has_no_exit_on_agent_auth() -> None:
    text = ENTRYPOINT.read_text(encoding="utf-8")
    end = text.index('if [[ "${START_SWAY')
    block = text[text.index('agent_auth_required="') : end]
    assert "exit 1" not in block, "agent auth must never stop the engine starting"
