"""The project-policy hook, driven through the real script.

`engine/deploy/policy-hook.sh` is what the pod entrypoint installs as a
PreToolUse hook, so its contract is the script itself: a command with no
`git` or `gh` word never leaves the box, a missing broker credential allows,
a denial exits 2 with the reason, and an engine that cannot be asked fails
closed for those git and gh commands. `install-policy` adds the hook once.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
HOOK = REPO_ROOT / "engine" / "deploy" / "policy-hook.sh"

pytestmark = pytest.mark.skipif(
    not HOOK.is_file() or shutil.which("bash") is None,
    reason="needs engine/deploy (absent in the core-alone job) and bash",
)


class _Broker:
    """A broker stand-in: records the body and answers what the test says."""

    def __init__(self, status: int, body: bytes) -> None:
        self.status = status
        self.body = body
        self.requests: list[bytes] = []
        self._server = ThreadingHTTPServer(("127.0.0.1", 0), self._handler())
        self._thread = threading.Thread(target=self._server.serve_forever, daemon=True)
        self._thread.start()

    def _handler(self) -> type[BaseHTTPRequestHandler]:
        broker = self

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self) -> None:
                length = int(self.headers.get("Content-Length", "0"))
                broker.requests.append(self.rfile.read(length))
                self.send_response(broker.status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(broker.body)))
                self.end_headers()
                self.wfile.write(broker.body)

            def log_message(self, format: str, *args: object) -> None:
                return

        return Handler

    @property
    def url(self) -> str:
        host, port = self._server.server_address[:2]
        # server_address's host is str or bytes depending on how it was bound.
        return f"http://{host!s}:{port}"

    def close(self) -> None:
        self._server.shutdown()
        self._thread.join(timeout=5)


def run(
    tmp_path: Path, payload: str, env: dict[str, str]
) -> subprocess.CompletedProcess[str]:
    # A scrubbed environment: the hook must not see this session's broker.
    full_env = {
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "HOME": str(tmp_path),
        **env,
    }
    return subprocess.run(
        ["bash", str(HOOK)],
        input=payload,
        env=full_env,
        capture_output=True,
        text=True,
        check=False,
    )


def test_an_allow_passes_and_a_denial_exits_2_with_the_reason(tmp_path: Path) -> None:
    broker = _Broker(200, b'{"decision":"allow"}')
    try:
        env = {"VOGT_ENGINE_BROKER_URL": broker.url, "VOGT_ENGINE_BROKER_TOKEN": "t"}
        allowed = run(
            tmp_path,
            '{"tool_name":"Bash","tool_input":{"command":"git status"}}',
            env,
        )
        assert allowed.returncode == 0, allowed.stderr
        assert broker.requests, "an allow still asks"

        broker.status = 200
        broker.body = b'{"decision":"deny","reason":"matches (?i)grok"}'
        denied = run(
            tmp_path,
            '{"tool_name":"Bash","tool_input":{"command":"git commit -m x"}}',
            env,
        )
        assert denied.returncode == 2
        assert "matches (?i)grok" in denied.stderr
    finally:
        broker.close()


def test_an_unreachable_engine_refuses_a_git_command(tmp_path: Path) -> None:
    refused = run(
        tmp_path,
        '{"tool_name":"Bash","tool_input":{"command":"git push"}}',
        {
            "VOGT_ENGINE_BROKER_URL": "http://127.0.0.1:1",
            "VOGT_ENGINE_BROKER_TOKEN": "t",
        },
    )
    assert refused.returncode == 2
    assert "vogt policy check unavailable" in refused.stderr


def test_a_command_without_git_or_gh_never_asks(tmp_path: Path) -> None:
    broker = _Broker(500, b"")
    try:
        result = run(
            tmp_path,
            '{"tool_name":"Bash","tool_input":{"command":"ls -l"}}',
            {"VOGT_ENGINE_BROKER_URL": broker.url, "VOGT_ENGINE_BROKER_TOKEN": "t"},
        )
        assert result.returncode == 0
        assert broker.requests == []
    finally:
        broker.close()


def test_a_2xx_that_is_not_an_allow_refuses(tmp_path: Path) -> None:
    """Exit 0 only for a decision that is exactly "allow".

    A 2xx body that is not JSON, one whose decision is some other word, an
    empty body, and a box with no python3 to read the body with all refuse a
    git or gh command. A check that cannot be read is not a pass.
    """
    cases = [
        (200, b"not json"),
        (200, b'{"decision":"maybe"}'),
        (204, b""),
    ]
    broker = _Broker(cases[0][0], cases[0][1])
    try:
        env = {"VOGT_ENGINE_BROKER_URL": broker.url, "VOGT_ENGINE_BROKER_TOKEN": "t"}
        for status, body in cases:
            broker.status = status
            broker.body = body
            refused = run(
                tmp_path,
                '{"tool_name":"Bash","tool_input":{"command":"git push"}}',
                env,
            )
            assert refused.returncode == 2, (status, body, refused.stderr)
            assert "unreadable" in refused.stderr
    finally:
        broker.close()

    # The same allow, read without python3, cannot be trusted either.
    no_python = tmp_path / "bin"
    no_python.mkdir()
    for name in ("bash", "curl", "head", "grep", "mktemp", "rm"):
        source = shutil.which(name)
        if source is not None:
            (no_python / name).symlink_to(source)
    # Unreachable is already a refusal; prove the missing interpreter is one
    # too, against a broker that answers allow.
    broker = _Broker(200, b'{"decision":"allow"}')
    try:
        blind = run(
            tmp_path,
            '{"tool_name":"Bash","tool_input":{"command":"gh pr view 1"}}',
            {
                "VOGT_ENGINE_BROKER_URL": broker.url,
                "VOGT_ENGINE_BROKER_TOKEN": "t",
                "PATH": str(no_python),
            },
        )
        assert blind.returncode == 2, blind.stderr
        assert "unreadable" in blind.stderr
    finally:
        broker.close()


def test_no_broker_credential_allows(tmp_path: Path) -> None:
    result = run(
        tmp_path,
        '{"tool_name":"Bash","tool_input":{"command":"gh pr merge 1"}}',
        {},
    )
    assert result.returncode == 0
    assert result.stderr == ""


def test_install_policy_is_idempotent(tmp_path: Path) -> None:
    settings = tmp_path / ".claude" / "settings.json"
    settings.parent.mkdir()
    settings.write_text(
        json.dumps({"hooks": {"SessionStart": [{"hooks": []}]}}),
        encoding="utf-8",
    )
    klaudia = tmp_path / ".klaudia" / "config.toml"
    klaudia.parent.mkdir()
    klaudia.write_text('model = "kept"\n', encoding="utf-8")

    env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "HOME": str(tmp_path)}
    for _ in range(2):
        proc = subprocess.run(
            ["bash", str(HOOK), "install-policy", str(settings), str(klaudia)],
            env=env,
            capture_output=True,
            text=True,
            check=False,
        )
        assert proc.returncode == 0, proc.stderr

    installed = json.loads(settings.read_text(encoding="utf-8"))
    groups = installed["hooks"]["PreToolUse"]
    assert len(groups) == 1
    assert groups[0]["matcher"] == "Bash"
    assert groups[0]["hooks"][0]["command"].endswith("policy-hook.sh")
    assert installed["hooks"]["SessionStart"] == [{"hooks": []}]

    text = klaudia.read_text(encoding="utf-8")
    assert text.startswith('model = "kept"\n')
    assert text.count("# vogt-policy-hook (managed)") == 1
    assert text.count("[[hooks]]") == 1
    assert 'command = "' in text and "policy-hook.sh" in text
