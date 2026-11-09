"""`vogt-mcp-bootstrap` must never abort a launch for a client it cannot set up.

It runs from `vogt-agent-auth run` on every launch under `set -euo pipefail`,
for whichever agent the session is starting (or for a plain, non-agent
payload). On vogt-dev the engine container has no `XDG_DATA_HOME`, and
`~/.local/share` there is a root-owned parent the kubelet made for the state
mount, so opencode's own `mkdir ~/.local/share/opencode` returned EACCES and
killed a Codex session in about half a second. The contract: report on stderr,
skip that client, and still exit 0.

The clients are shell shims on a PATH with no real `codex`, `claude` or
`opencode`, so the test is offline and does not touch the caller's own config.
"""

from __future__ import annotations

import os
import shutil
import subprocess
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
BOOTSTRAP = REPO_ROOT / "engine" / "deploy" / "mcp-bootstrap.sh"

pytestmark = pytest.mark.skipif(
    not BOOTSTRAP.is_file() or shutil.which("bash") is None,
    reason="needs engine/deploy (absent in the core-alone job) and bash",
)


def _shim(bindir: Path, name: str, body: str) -> None:
    path = bindir / name
    path.write_text(f"#!/bin/sh\n{body}", encoding="utf-8")
    path.chmod(0o755)


def _run(tmp_path: Path) -> subprocess.CompletedProcess[str]:
    home = tmp_path / "home"
    home.mkdir(exist_ok=True)
    env = {
        "PATH": os.pathsep.join([str(tmp_path / "bin"), "/usr/bin", "/bin"]),
        "HOME": str(home),
        "VOGT_SRC": str(tmp_path / "absent"),
        "CALLS": str(tmp_path / "calls"),
    }
    return subprocess.run(
        ["bash", str(BOOTSTRAP)],
        env=env,
        cwd=tmp_path,
        capture_output=True,
        text=True,
        check=False,
    )


def test_an_unwritable_opencode_data_dir_does_not_abort_the_launch(
    tmp_path: Path,
) -> None:
    home = tmp_path / "home"
    (home / ".local").mkdir(parents=True)
    # A file, not a directory: `mkdir -p` under it fails for any uid, root
    # included, so this reproduces the EACCES shape without relying on mode
    # bits that root ignores.
    (home / ".local" / "share").write_text("not a directory", encoding="utf-8")
    bindir = tmp_path / "bin"
    bindir.mkdir()
    _shim(
        bindir,
        "opencode",
        'printf "%s\\n" "$*" >> "$CALLS"\n'
        'mkdir -p "$HOME/.local/share/opencode" || exit 1\nexit 0\n',
    )

    proc = _run(tmp_path)

    assert proc.returncode == 0, proc.stderr
    assert proc.stdout == "", "stdout is the MCP transport"
    assert "opencode data directory" in proc.stderr
    assert "skipping" in proc.stderr
    assert not (tmp_path / "calls").exists(), "opencode was started anyway"


def test_an_unwritable_codex_config_dir_is_skipped(tmp_path: Path) -> None:
    bindir = tmp_path / "bin"
    bindir.mkdir()
    _shim(bindir, "codex", "exit 0\n")
    home = tmp_path / "home"
    home.mkdir()
    (home / ".codex").write_text("not a directory", encoding="utf-8")

    proc = _run(tmp_path)

    assert proc.returncode == 0, proc.stderr
    assert proc.stdout == ""
    assert "Codex config directory is not writable" in proc.stderr
    assert "Vogt MCP client registrations written" in proc.stderr


def test_a_failing_client_is_a_warning_not_a_launch_failure(
    tmp_path: Path,
) -> None:
    bindir = tmp_path / "bin"
    bindir.mkdir()
    _shim(
        bindir,
        "opencode",
        'mkdir -p "$HOME/.local/share/opencode" "$HOME/.config/opencode"\nexit 7\n',
    )
    _shim(bindir, "codex", "exit 9\n")

    proc = _run(tmp_path)

    assert proc.returncode == 0, proc.stderr
    assert proc.stdout == ""
    assert "Codex Vogt registration failed; continuing" in proc.stderr
    assert "opencode registration of vogt failed" in proc.stderr
    # The run reached its last step instead of stopping at the first failure.
    assert "Vogt MCP client registrations written" in proc.stderr


def test_a_writable_home_still_registers_opencode(tmp_path: Path) -> None:
    bindir = tmp_path / "bin"
    bindir.mkdir()
    _shim(bindir, "opencode", 'printf "%s\\n" "$*" >> "$CALLS"\nexit 0\n')

    proc = _run(tmp_path)

    assert proc.returncode == 0, proc.stderr
    calls = (tmp_path / "calls").read_text(encoding="utf-8").splitlines()
    assert calls == ["mcp add vogt -- /usr/local/bin/vogt-mcp"], calls
    assert "not writable" not in proc.stderr
