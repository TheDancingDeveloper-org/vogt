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
        self.authorizations: list[str] = []
        self._server = ThreadingHTTPServer(("127.0.0.1", 0), self._handler())
        self._thread = threading.Thread(target=self._server.serve_forever, daemon=True)
        self._thread.start()

    def _handler(self) -> type[BaseHTTPRequestHandler]:
        broker = self

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self) -> None:
                length = int(self.headers.get("Content-Length", "0"))
                broker.requests.append(self.rfile.read(length))
                broker.authorizations.append(self.headers.get("Authorization", ""))
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
    tmp_path: Path, payload: str, env: dict[str, str], mode: str | None = None
) -> subprocess.CompletedProcess[str]:
    # A scrubbed environment: the hook must not see this session's broker.
    full_env = {
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "HOME": str(tmp_path),
        **env,
    }
    argv = ["bash", str(HOOK)]
    if mode is not None:
        argv.append(mode)
    return subprocess.run(
        argv,
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


def test_a_workspace_rule_denial_reaches_the_agent(tmp_path: Path) -> None:
    """WI-1326: the discard guard's and the protected-checkout rule's reasons
    come back on stderr with exit 2, like any other policy denial."""
    for reason in (
        "refusing: discard guard: git restore would discard 1 path(s) that had "
        "uncommitted changes before this session started",
        "refusing: protected checkout: git commit writes to a project checkout",
    ):
        broker = _Broker(
            200, json.dumps({"decision": "deny", "reason": reason}).encode()
        )
        try:
            env = {
                "VOGT_ENGINE_BROKER_URL": broker.url,
                "VOGT_ENGINE_BROKER_TOKEN": "t",
            }
            denied = run(
                tmp_path,
                '{"tool_name":"Bash","tool_input":{"command":"git restore f"}}',
                env,
            )
            assert denied.returncode == 2
            assert reason in denied.stderr
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


def test_a_plain_rm_is_asked_so_the_discard_guard_sees_it(tmp_path: Path) -> None:
    """WI-1326: a bare `rm` of a file with uncommitted changes is a discard,
    so the hook asks the engine rather than letting it pass unasked."""
    reason = "refusing: discard guard: rm would discard 1 path(s)"
    broker = _Broker(200, json.dumps({"decision": "deny", "reason": reason}).encode())
    try:
        env = {"VOGT_ENGINE_BROKER_URL": broker.url, "VOGT_ENGINE_BROKER_TOKEN": "t"}
        denied = run(
            tmp_path,
            '{"tool_name":"Bash","tool_input":{"command":"rm README.md"}}',
            env,
        )
        assert broker.requests, "a plain rm must be asked"
        assert denied.returncode == 2
        assert reason in denied.stderr
    finally:
        broker.close()


@pytest.mark.parametrize(
    "command",
    [
        "ls\ngit checkout -- README.md",
        "ls\nrm README.md",
        "ls\tgit checkout -- README.md",
        "ls\r\ngit push",
        "(git checkout -- README.md)",
        'g""it checkout -- README.md',
        "echo x | bash",
    ],
)
def test_an_unreachable_engine_refuses_a_hidden_command(
    tmp_path: Path, command: str
) -> None:
    """WI-1326 review: the filter reads the decoded command, so a newline
    (`\\n` in the JSON) or tab before `git`/`rm` still asks the engine, and an
    engine that cannot be asked refuses it."""
    env = {
        "VOGT_ENGINE_BROKER_URL": "http://127.0.0.1:1",
        "VOGT_ENGINE_BROKER_TOKEN": "t",
    }
    payload = json.dumps({"tool_name": "Bash", "tool_input": {"command": command}})
    refused = run(tmp_path, payload, env)
    assert refused.returncode == 2, command
    assert "vogt policy check unavailable" in refused.stderr
    codex = json.dumps({"tool_name": "exec_command", "tool_input": {"cmd": command}})
    assert run(tmp_path, codex, env, mode="codex").returncode == 2, command


@pytest.mark.parametrize(
    "command",
    [
        "git restore \\\na.txt",
        "rm \\\na.txt",
        "gi\\\nt restore a.txt",
        "r\\\nm a.txt",
        "$'\\x72m' a.txt",
        "$'\\162m' a.txt",
        "R=$(echo cm | tr c r); $R a.txt",
        "X=y ${R}m a.txt",
        ". <(echo x)",
        "source <(echo x)",
        "bash <(echo x)",
        "bash /dev/stdin <<< x",
    ],
)
def test_rereview_bypasses_are_asked(tmp_path: Path, command: str) -> None:
    """WI-1326 re-review: a line continuation, an ANSI-C string, a variable as
    the program, and a sourced or process-substituted script all reach the
    engine, so an engine that cannot be asked refuses them, with a message
    that does not call a read a write."""
    env = {
        "VOGT_ENGINE_BROKER_URL": "http://127.0.0.1:1",
        "VOGT_ENGINE_BROKER_TOKEN": "t",
    }
    payload = json.dumps({"tool_name": "Bash", "tool_input": {"command": command}})
    refused = run(tmp_path, payload, env)
    assert refused.returncode == 2, command
    assert "vogt policy check unavailable" in refused.stderr
    assert "write" not in refused.stderr
    codex = json.dumps({"tool_name": "exec_command", "tool_input": {"cmd": command}})
    assert run(tmp_path, codex, env, mode="codex").returncode == 2, command


@pytest.mark.parametrize(
    "command",
    [
        "R=$(echo cm | tr c r); timeout 5 $R a.txt",
        "R=$(echo cm | tr c r); timeout -s KILL 5 $R a.txt",
        "R=x; nice -n 5 $R a.txt",
        "R=x; nice $R a.txt",
        "R=x; stdbuf -oL $R a.txt",
        "R=x; setsid $R a.txt",
        "R=x; ionice -c 3 $R a.txt",
        "R=x; doas $R a.txt",
        "R=x; chronic $R a.txt",
        "R=x; nohup ${R} a.txt",
        "R=x; timeout 5 `echo $R` a.txt",
        "{rm,a.txt}",
        "{git,restore,a.txt}",
        "{r,}m a.txt",
        "$(printf %s%s r m) a.txt",
        "`printf %s%s r m` a.txt",
    ],
)
def test_round3_wrapped_and_braced_programs_are_asked(
    tmp_path: Path, command: str
) -> None:
    """WI-4001 bypasses 5 and 3: a variable after a wrapper with options or a
    duration (`timeout 5 $R`), and a brace expansion as the program, reach
    the engine; with no engine to ask they are refused."""
    env = {
        "VOGT_ENGINE_BROKER_URL": "http://127.0.0.1:1",
        "VOGT_ENGINE_BROKER_TOKEN": "t",
    }
    payload = json.dumps({"tool_name": "Bash", "tool_input": {"command": command}})
    refused = run(tmp_path, payload, env)
    assert refused.returncode == 2, command
    assert "vogt policy check unavailable" in refused.stderr
    codex = json.dumps({"tool_name": "exec_command", "tool_input": {"cmd": command}})
    assert run(tmp_path, codex, env, mode="codex").returncode == 2, command


@pytest.mark.parametrize(
    "command",
    [
        "env -S'rm a.txt'",
        'env -S"rm a.txt"',
        "env -iS'rm a.txt'",
        "env -Srm\\ a.txt",
        "su -c'rm a.txt'",
        "su -lc'rm a.txt'",
        "runuser -c'unlink a.txt'",
        "script -qc'rm a.txt' /dev/null",
    ],
)
def test_round4_attached_payloads_are_asked(tmp_path: Path, command: str) -> None:
    """WI-4001 round 4, blocker 3: a payload attached to `env -S` or `su
    -c` (`-S'rm a.txt'`) reaches the engine; with no engine it is refused."""
    env = {
        "VOGT_ENGINE_BROKER_URL": "http://127.0.0.1:1",
        "VOGT_ENGINE_BROKER_TOKEN": "t",
    }
    payload = json.dumps({"tool_name": "Bash", "tool_input": {"command": command}})
    refused = run(tmp_path, payload, env)
    assert refused.returncode == 2, command
    assert "vogt policy check unavailable" in refused.stderr
    codex = json.dumps({"tool_name": "exec_command", "tool_input": {"cmd": command}})
    assert run(tmp_path, codex, env, mode="codex").returncode == 2, command


@pytest.mark.parametrize(
    "command",
    [
        "timeout 60 cargo test",
        "nice -n 5 cargo build --release",
        "ls src/{a,b}.rs",
        "echo {a,b}",
        "awk '{print $1}' f.txt",
        "script -q /dev/null",
    ],
)
def test_round3_ordinary_wrapped_commands_are_not_asked(
    tmp_path: Path, command: str
) -> None:
    broker = _Broker(500, b"")
    try:
        env = {"VOGT_ENGINE_BROKER_URL": broker.url, "VOGT_ENGINE_BROKER_TOKEN": "t"}
        payload = json.dumps({"tool_name": "Bash", "tool_input": {"command": command}})
        assert run(tmp_path, payload, env).returncode == 0, command
        assert broker.requests == []
    finally:
        broker.close()


@pytest.mark.parametrize(
    "command",
    ["env RUST_LOG=debug cargo test", "env -i PATH=/usr/bin cargo test"],
)
def test_env_wrapped_commands_are_asked_and_pass_on_allow(
    tmp_path: Path, command: str
) -> None:
    """WI-4046 made `env` an ask-word: the engine's environment guard tells
    `env FOO=bar cmd` from `env`, so the hook asks and an allow passes."""
    broker = _Broker(200, b'{"decision":"allow"}')
    try:
        env = {"VOGT_ENGINE_BROKER_URL": broker.url, "VOGT_ENGINE_BROKER_TOKEN": "t"}
        payload = json.dumps({"tool_name": "Bash", "tool_input": {"command": command}})
        assert run(tmp_path, payload, env).returncode == 0, command
        assert len(broker.requests) == 1, command
        # WI-4061: with the engine unreachable, the offline allowlist still
        # recognises `env NAME=value cmd` as a wrapper that runs a program.
        broker.status = 500
        assert run(tmp_path, payload, env).returncode == 0, command
    finally:
        broker.close()


def test_an_unreachable_engine_allows_a_harmless_shell_prologue(
    tmp_path: Path,
) -> None:
    """WI-4061: an outage must not block every command that merely names an
    ambiguous environment word. These forms print nothing, and the offline
    allowlist recognises them; a dump stays refused (next test)."""
    env = {
        "VOGT_ENGINE_BROKER_URL": "http://127.0.0.1:1",
        "VOGT_ENGINE_BROKER_TOKEN": "t",
    }
    for command in [
        "set -euo pipefail",
        "set -o pipefail",
        "set +x",
        "set -- a b",
        "export FOO=bar && cargo test",
        'export PATH="$HOME/bin:$PATH"',
        "export -n FOO",
        "readonly X=1",
        "local x=1",
        "declare -A map",
        "declare -f",
        "ps aux",
        "ps -ef",
        "env FOO=bar cargo test",
        "env -i PATH=/usr/bin cargo test",
        "env -u FOO cargo test",
        "grep -r env src/",
    ]:
        payload = json.dumps({"tool_name": "Bash", "tool_input": {"command": command}})
        result = run(tmp_path, payload, env)
        assert result.returncode == 0, (command, result.stderr)


def test_an_unreachable_engine_still_refuses_an_environment_dump(
    tmp_path: Path,
) -> None:
    """WI-4061: the outage rule is fail-closed for a real dump and for git;
    the allowlist above only covers harmless prologue forms."""
    env = {
        "VOGT_ENGINE_BROKER_URL": "http://127.0.0.1:1",
        "VOGT_ENGINE_BROKER_TOKEN": "t",
    }
    for command in [
        "env",
        "env | cat",
        "env VAR=1 printenv",
        "printenv",
        "printenv VOGT_HTTP_TOKEN",
        "set",
        "set | cat",
        "export -p",
        "declare -p",
        "declare -x",
        "ps auxe",
        "ps eww",
        "echo $VOGT_HTTP_TOKEN | cat",
        "python3 -c 'import os; print(dict(os.environ))'",
        "perl -e 'print %ENV'",
        "ruby -e 'puts ENV.to_h'",
        "echo x; ruby -e 'p ENV'",
        "jq -n env",
        "env FOO=1 jq -n env",
        'cat <<< "$VOGT_HTTP_TOKEN"',
        "timeout 5 env",
        "nice env",
        "git log --grep=environment",
        # WI-4061 batch 126: the offline /proc check must not be fooled by a
        # quote or a glob now that the engine cannot be asked.
        "cat /proc/self/env*",
        'strings /proc/self/env""iron',
        "cat /proc/self/e?viron",
        "cat /proc/1/envir[o]n",
    ]:
        payload = json.dumps({"tool_name": "Bash", "tool_input": {"command": command}})
        result = run(tmp_path, payload, env)
        assert result.returncode == 2, (command, result.stderr)
        assert "vogt policy check unavailable" in result.stderr, command


@pytest.mark.parametrize(
    "command",
    [
        "cat /proc/self/e?viron",
        "cat /proc/1/envir[o]n",
        'python3 -c \'import os; print(vars(os)["envi"+"ron"])\'',
    ],
)
def test_a_hidden_proc_or_vars_os_read_is_asked(tmp_path: Path, command: str) -> None:
    """WI-4061 batch 126: the prefilter used to let a globbed /proc read and
    a `vars(os)` concatenation through without ever asking the engine, so
    the guard never saw them."""
    broker = _Broker(200, b'{"decision":"allow"}')
    try:
        env = {"VOGT_ENGINE_BROKER_URL": broker.url, "VOGT_ENGINE_BROKER_TOKEN": "t"}
        payload = json.dumps({"tool_name": "Bash", "tool_input": {"command": command}})
        assert run(tmp_path, payload, env).returncode == 0, command
        assert len(broker.requests) == 1, command
    finally:
        broker.close()


@pytest.mark.parametrize(
    "command", ["echo $HOME", "ls -la; cargo test", "printf '%s\\n' x"]
)
def test_an_expansion_that_is_not_the_program_is_not_asked(
    tmp_path: Path, command: str
) -> None:
    broker = _Broker(500, b"")
    try:
        env = {"VOGT_ENGINE_BROKER_URL": broker.url, "VOGT_ENGINE_BROKER_TOKEN": "t"}
        payload = json.dumps({"tool_name": "Bash", "tool_input": {"command": command}})
        assert run(tmp_path, payload, env).returncode == 0, command
        assert broker.requests == []
    finally:
        broker.close()


def test_a_newline_separated_rm_is_asked(tmp_path: Path) -> None:
    reason = "refusing: discard guard: rm would discard 1 path(s)"
    broker = _Broker(200, json.dumps({"decision": "deny", "reason": reason}).encode())
    try:
        env = {"VOGT_ENGINE_BROKER_URL": broker.url, "VOGT_ENGINE_BROKER_TOKEN": "t"}
        payload = json.dumps(
            {"tool_name": "Bash", "tool_input": {"command": "ls\nrm README.md"}}
        )
        denied = run(tmp_path, payload, env)
        assert len(broker.requests) == 1
        sent = json.loads(broker.requests[0])
        assert sent["tool_input"]["command"] == "ls\nrm README.md"
        assert denied.returncode == 2
        assert reason in denied.stderr
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


def test_a_pod_session_asks_with_its_stream_credential(tmp_path: Path) -> None:
    """A pod holds no broker token; the credential file names the session (WI-3243)."""
    broker = _Broker(200, b'{"decision":"deny","reason":"no merges"}')
    try:
        token_file = tmp_path / "session-token"
        token_file.write_text("pod-stream-token\n")
        env = {
            "VOGT_ENGINE_URL": broker.url,
            "VOGT_SESSION_TOKEN_FILE": str(token_file),
        }
        denied = run(
            tmp_path,
            '{"tool_name":"Bash","tool_input":{"command":"gh pr merge 1"}}',
            env,
        )
        assert denied.returncode == 2
        assert "no merges" in denied.stderr
        assert broker.authorizations == ["Bearer pod-stream-token"]

        # A broker token, where there is one, still wins over the file.
        broker.authorizations.clear()
        other = _Broker(200, b'{"decision":"allow"}')
        try:
            allowed = run(
                tmp_path,
                '{"tool_name":"Bash","tool_input":{"command":"git status"}}',
                {
                    **env,
                    "VOGT_ENGINE_BROKER_URL": other.url,
                    "VOGT_ENGINE_BROKER_TOKEN": "broker-token",
                },
            )
            assert allowed.returncode == 0, allowed.stderr
            assert other.authorizations == ["Bearer broker-token"]
            assert broker.authorizations == []
        finally:
            other.close()
    finally:
        broker.close()


def test_codex_exec_command_is_translated_to_the_engine_request(
    tmp_path: Path,
) -> None:
    """Codex sends `cmd`; the engine only reads `tool_input.command`."""
    broker = _Broker(200, b'{"decision":"deny","reason":"no hard resets"}')
    try:
        denied = run(
            tmp_path,
            json.dumps(
                {
                    "session_id": "s",
                    "cwd": "/work/repo",
                    "hook_event_name": "PreToolUse",
                    "tool_name": "Bash",
                    "tool_input": {"cmd": "git reset --hard"},
                }
            ),
            {"VOGT_ENGINE_BROKER_URL": broker.url, "VOGT_ENGINE_BROKER_TOKEN": "t"},
            mode="codex",
        )
        assert denied.returncode == 2, denied.stderr
        assert "no hard resets" in denied.stderr
        sent = json.loads(broker.requests[-1])
        assert sent == {
            "tool_name": "Bash",
            "tool_input": {"command": "git reset --hard"},
            "cwd": "/work/repo",
        }
    finally:
        broker.close()


def test_codex_shell_array_is_one_command(tmp_path: Path) -> None:
    """`shell` sends an argv array; the real command must still be judged."""
    broker = _Broker(200, b'{"decision":"deny","reason":"no pushes"}')
    try:
        denied = run(
            tmp_path,
            json.dumps(
                {
                    "session_id": "s",
                    "cwd": "/work/repo",
                    "tool_name": "Bash",
                    "tool_input": {
                        "command": ["bash", "-lc", "git push origin main"],
                        "workdir": "sub",
                    },
                }
            ),
            {"VOGT_ENGINE_BROKER_URL": broker.url, "VOGT_ENGINE_BROKER_TOKEN": "t"},
            mode="codex",
        )
        assert denied.returncode == 2, denied.stderr
        sent = json.loads(broker.requests[-1])
        assert "git push origin main" in sent["tool_input"]["command"]
        assert sent["cwd"] == "/work/repo/sub"
    finally:
        broker.close()


def test_codex_apply_patch_is_allowed_without_a_request(tmp_path: Path) -> None:
    broker = _Broker(500, b"")
    try:
        result = run(
            tmp_path,
            json.dumps(
                {
                    "session_id": "s",
                    "tool_name": "apply_patch",
                    "tool_input": {"command": "*** Begin Patch\n*** End Patch\n"},
                }
            ),
            {"VOGT_ENGINE_BROKER_URL": broker.url, "VOGT_ENGINE_BROKER_TOKEN": "t"},
            mode="codex",
        )
        assert result.returncode == 0, result.stderr
        assert broker.requests == []
    finally:
        broker.close()


def test_codex_unreadable_payload_fails_closed(tmp_path: Path) -> None:
    """A translator that cannot read the tool call must not become a pass."""
    broker = _Broker(200, b'{"decision":"allow"}')
    try:
        refused = run(
            tmp_path,
            "not json",
            {"VOGT_ENGINE_BROKER_URL": broker.url, "VOGT_ENGINE_BROKER_TOKEN": "t"},
            mode="codex",
        )
        assert refused.returncode == 2
        assert "vogt policy check unavailable" in refused.stderr
        assert broker.requests == []
    finally:
        broker.close()


def test_install_policy_is_idempotent(tmp_path: Path) -> None:
    settings = tmp_path / ".claude" / "settings.json"
    settings.parent.mkdir()
    settings.write_text(
        json.dumps({"hooks": {"SessionStart": [{"hooks": []}]}}),
        encoding="utf-8",
    )
    env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "HOME": str(tmp_path)}
    for _ in range(2):
        proc = subprocess.run(
            ["bash", str(HOOK), "install-policy", str(settings)],
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
