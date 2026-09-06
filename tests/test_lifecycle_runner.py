"""Executable contract tests for deployment-owned lifecycle hooks."""

from __future__ import annotations

import os
import subprocess
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path

RUNNER = Path(__file__).parents[1] / "vogt-lifecycle.sh"


def _hook(path: Path, body: str) -> None:
    path.write_text(f"#!/bin/sh\nset -eu\n{body}\n", encoding="utf-8")
    path.chmod(0o755)


def _run(
    tmp_path: Path,
    *command: str,
    required: str = "false",
    health_url: str | None = None,
) -> subprocess.CompletedProcess[str]:
    env = os.environ.copy()
    env.update(
        {
            "VOGT_HOOK_DIR": str(tmp_path / "hooks"),
            "VOGT_LIFECYCLE_STATE_DIR": str(tmp_path / "state"),
            "VOGT_LIFECYCLE_WORKDIR": str(tmp_path / "work"),
            "VOGT_HOOKS_REQUIRED": required,
        }
    )
    if health_url is not None:
        env["VOGT_LIFECYCLE_HEALTHCHECK_URL"] = health_url
    return subprocess.run(
        ["sh", str(RUNNER), *command],
        cwd=tmp_path / "work",
        env=env,
        text=True,
        capture_output=True,
        check=False,
    )


def test_hooks_are_ordered_and_first_start_is_persisted(tmp_path: Path) -> None:
    (tmp_path / "work").mkdir()
    for phase in ("pre-start.d", "post-start.d"):
        (tmp_path / "hooks" / phase).mkdir(parents=True)
    (tmp_path / "hooks" / "post-health.d").mkdir(parents=True)
    _hook(
        tmp_path / "hooks" / "pre-start.d" / "020-second",
        'printf "second-%s\\n" "$VOGT_LIFECYCLE_FIRST_START" >> order',
    )
    _hook(
        tmp_path / "hooks" / "pre-start.d" / "010-first",
        'printf "first-%s\\n" "$VOGT_LIFECYCLE_FIRST_START" >> order',
    )
    _hook(
        tmp_path / "hooks" / "post-start.d" / "030-post",
        'printf "post-%s\\n" "$VOGT_LIFECYCLE_FIRST_START" >> order',
    )

    first = _run(tmp_path, "sh", "-c", "true")
    assert first.returncode == 0, first.stderr
    assert (tmp_path / "work" / "order").read_text(encoding="utf-8").splitlines() == [
        "first-1",
        "second-1",
        "post-1",
    ]
    assert (tmp_path / "state" / "started").is_file()

    second = _run(tmp_path, "sh", "-c", "true")
    assert second.returncode == 0, second.stderr
    lines = (tmp_path / "work" / "order").read_text(encoding="utf-8").splitlines()
    assert lines[-3:] == [
        "first-0",
        "second-0",
        "post-0",
    ]


def test_required_bundle_and_hook_failure_block_start(tmp_path: Path) -> None:
    (tmp_path / "work").mkdir()
    missing = _run(tmp_path, "sh", "-c", "true", required="true")
    assert missing.returncode == 78
    assert "required hook bundle is missing" in missing.stderr

    (tmp_path / "hooks" / "pre-start.d").mkdir(parents=True)
    _hook(tmp_path / "hooks" / "pre-start.d" / "010-fail", "exit 42")
    failed = _run(tmp_path, "sh", "-c", "true")
    assert failed.returncode == 42
    assert "010-fail" in failed.stderr
    assert not (tmp_path / "state" / "started").exists()


def test_post_health_failure_fails_the_probe(tmp_path: Path) -> None:
    (tmp_path / "work").mkdir()
    phase = tmp_path / "hooks" / "post-health.d"
    phase.mkdir(parents=True)
    _hook(phase / "010-fail", "exit 17")

    class Handler(BaseHTTPRequestHandler):
        def do_GET(self) -> None:
            self.send_response(200)
            self.end_headers()

        def log_message(self, *_args: object) -> None:
            pass

    server = HTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        result = _run(
            tmp_path,
            "health",
            health_url=f"http://127.0.0.1:{server.server_port}",
        )
    finally:
        server.shutdown()
        thread.join()
    assert result.returncode == 17
    assert "010-fail" in result.stderr


def test_health_prefers_python3_when_python_is_absent(tmp_path: Path) -> None:
    """The published images ship `python3`, not a bare `python` alias.

    The healthcheck used to assume `python`, so `vogt-lifecycle health` exited
    127 ("python: not found") in the stack image and the pod read as unhealthy
    forever while the core was serving. This shadows `python` with a stub that
    always fails and leaves `python3` working: the probe must still pass,
    proving the runner resolves `python3` rather than the broken `python`.
    """
    (tmp_path / "work").mkdir()
    # A `python` earlier on PATH that fails if anything calls it, so a probe that
    # succeeds can only have used python3.
    fakebin = tmp_path / "fakebin"
    fakebin.mkdir()
    _hook(fakebin / "python", "echo 'must not be called' >&2; exit 99")

    class Handler(BaseHTTPRequestHandler):
        def do_GET(self) -> None:
            self.send_response(200)
            self.end_headers()

        def log_message(self, *_args: object) -> None:
            pass

    server = HTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        env = os.environ.copy()
        env.update(
            {
                "VOGT_HOOK_DIR": str(tmp_path / "hooks"),
                "VOGT_LIFECYCLE_STATE_DIR": str(tmp_path / "state"),
                "VOGT_LIFECYCLE_WORKDIR": str(tmp_path / "work"),
                "VOGT_HOOKS_REQUIRED": "false",
                "PATH": f"{fakebin}{os.pathsep}{env['PATH']}",
                "VOGT_LIFECYCLE_HEALTHCHECK_URL": f"http://127.0.0.1:{server.server_port}",
            }
        )
        env.pop("VOGT_LIFECYCLE_PYTHON", None)
        result = subprocess.run(
            ["sh", str(RUNNER), "health"],
            cwd=tmp_path / "work",
            env=env,
            text=True,
            capture_output=True,
            check=False,
        )
    finally:
        server.shutdown()
        thread.join()
    assert result.returncode == 0, result.stderr
    assert "must not be called" not in result.stderr


def test_restore_sample_refuses_dirty_checkout(tmp_path: Path) -> None:
    source = tmp_path / "source"
    target = tmp_path / "target"
    source.mkdir()
    target.mkdir()
    (source / "asset.txt").write_text("new", encoding="utf-8")
    subprocess.run(["git", "init", "-q", str(target)], check=True)
    (target / "dirty.txt").write_text("operator change", encoding="utf-8")
    script = Path(__file__).parents[1] / "deploy/lifecycle-hooks/restore-and-verify.sh"
    env = os.environ.copy()
    env.update(
        {
            "VOGT_HOOK_SOURCE_DIR": str(source),
            "VOGT_HOOK_TARGET_DIR": str(target),
        }
    )
    result = subprocess.run(
        ["sh", str(script)], env=env, text=True, capture_output=True, check=False
    )
    assert result.returncode == 73
    assert "dirty checkout" in result.stderr
