"""`vogt-prod`: a dev session's MCP server for the prod Vogt (WI-4289).

`vogt-mcp-bootstrap` registers `vogt-prod` only while VOGT_PROD_MCP_URL is set
and the session can get the prod token (in its environment, in a file, or
declared `ondemand` in the agent-auth manifest), and every registration runs
`vogt-prod-mcp`, which resolves the token at spawn. The contract under test:
registered when set, absent (and removed) when not, and the token never on
any argv, in any stored config, or on stdout/stderr.

The agent CLIs, the bridge and the broker are shims on PATH, so the tests are
offline and exercise only the scripts.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import tomllib
from pathlib import Path
from typing import Any

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
DEPLOY = REPO_ROOT / "engine" / "deploy"
BOOTSTRAP = DEPLOY / "mcp-bootstrap.sh"
WRAPPER = DEPLOY / "vogt-prod-mcp.sh"

pytestmark = pytest.mark.skipif(
    not (BOOTSTRAP.is_file() and WRAPPER.is_file())
    or shutil.which("bash") is None
    or shutil.which("jq") is None
    or shutil.which("flock") is None
    or shutil.which("python3") is None,
    reason="needs engine/deploy (absent in core-alone), bash, jq, flock, python3",
)

URL = "https://vogt-prod.example.test/mcp"
TOKEN = "prod-token-value-never-stored"
INSTALLED = "/usr/local/bin/vogt-prod-mcp"

# Logs argv and keeps ~/.claude.json in the shape the real CLI stores.
CLAUDE_SHIM = """#!/usr/bin/env python3
import json, os, sys
args = sys.argv[1:]
with open(os.environ["CALLS"], "a") as fh:
    fh.write(json.dumps(["claude", *args]) + "\\n")
path = os.path.join(os.environ["HOME"], ".claude.json")
data = json.load(open(path)) if os.path.exists(path) else {}
servers = data.setdefault("mcpServers", {})
if args[:2] == ["mcp", "remove"]:
    servers.pop([a for a in args[2:] if not a.startswith("-") and a != "user"][0], None)
elif args[:2] == ["mcp", "add"] and "--" in args:
    split = args.index("--")
    name = [a for a in args[2:split] if not a.startswith("-") and a != "user"][0]
    command, rest = args[split + 1], args[split + 2:]
    servers[name] = {"type": "stdio", "command": command, "args": rest}
elif args[:2] == ["mcp", "get"]:
    entry = servers.get(args[2])
    if entry is None:
        sys.exit(1)
    print(json.dumps(entry))
json.dump(data, open(path, "w"))
"""

# Codex's table is written by the bootstrap itself; `mcp get` reads it back.
CODEX_SHIM = """#!/usr/bin/env python3
import json, os, sys, tomllib
args = sys.argv[1:]
with open(os.environ["CALLS"], "a") as fh:
    fh.write(json.dumps(["codex", *args]) + "\\n")
if args[:2] == ["mcp", "get"]:
    path = os.path.join(os.environ["HOME"], ".codex", "config.toml")
    try:
        servers = tomllib.load(open(path, "rb")).get("mcp_servers", {})
    except FileNotFoundError:
        servers = {}
    if args[2] not in servers:
        sys.exit(1)
    print(json.dumps(servers[args[2]]))
"""

# Records argv and environment, for the bridge and the broker.
RECORD_SHIM = """#!/usr/bin/env python3
import json, os, sys
with open(os.environ["SHIM_OUT"], "w") as fh:
    json.dump({"argv": sys.argv, "env": dict(os.environ)}, fh)
"""

BROKER_SHIM = """#!/usr/bin/env python3
import json, os, sys
with open(os.environ["BROKER_OUT"], "w") as fh:
    json.dump(sys.argv, fh)
if sys.argv[1:] != ["fetch", "VOGT_PROD_TOKEN"]:
    sys.exit(1)
sys.stdout.write(os.environ["BROKER_VALUE"] + "\\n")
"""


def write_shim(bindir: Path, name: str, body: str) -> Path:
    path = bindir / name
    path.write_text(body, encoding="utf-8")
    path.chmod(0o755)
    return path


def bootstrap(tmp_path: Path, extra: dict[str, str]) -> tuple[list[list[str]], str]:
    bindir = tmp_path / "bin"
    if not bindir.exists():
        bindir.mkdir()
        write_shim(bindir, "claude", CLAUDE_SHIM)
        write_shim(bindir, "codex", CODEX_SHIM)
    calls = tmp_path / "calls"
    calls.unlink(missing_ok=True)
    proc = subprocess.run(
        ["bash", str(BOOTSTRAP)],
        env={
            "PATH": f"{bindir}{os.pathsep}/usr/bin{os.pathsep}/bin",
            "HOME": str(tmp_path),
            "VOGT_SRC": str(tmp_path / "absent"),
            "CALLS": str(calls),
            **extra,
        },
        cwd=tmp_path,
        capture_output=True,
        text=True,
        check=False,
    )
    assert proc.returncode == 0, proc.stderr
    assert proc.stdout == "", "stdout is the MCP transport"
    assert TOKEN not in proc.stderr
    lines = calls.read_text(encoding="utf-8").splitlines() if calls.exists() else []
    return [json.loads(line) for line in lines], proc.stderr


def claude_servers(tmp_path: Path) -> dict[str, Any]:
    config = tmp_path / ".claude.json"
    if not config.exists():
        return {}
    servers: dict[str, Any] = json.loads(config.read_text(encoding="utf-8"))[
        "mcpServers"
    ]
    return servers


def codex_servers(tmp_path: Path) -> dict[str, Any]:
    config = tmp_path / ".codex" / "config.toml"
    if not config.exists():
        return {}
    with config.open("rb") as fh:
        servers: dict[str, Any] = tomllib.load(fh).get("mcp_servers", {})
    return servers


def assert_no_token_stored(tmp_path: Path, calls: list[list[str]]) -> None:
    assert TOKEN not in json.dumps(calls)
    for path in (tmp_path / ".claude.json", tmp_path / ".codex" / "config.toml"):
        if path.exists():
            assert TOKEN not in path.read_text(encoding="utf-8")
            assert URL not in path.read_text(encoding="utf-8")


def test_not_registered_without_a_url(tmp_path: Path) -> None:
    calls, _ = bootstrap(tmp_path, {"VOGT_PROD_TOKEN": TOKEN})
    assert not any("vogt-prod" in call and "add" in call for call in calls)
    assert "vogt-prod" not in claude_servers(tmp_path)
    assert "vogt-prod" not in codex_servers(tmp_path)


def test_not_registered_without_a_token(tmp_path: Path) -> None:
    bootstrap(
        tmp_path, {"VOGT_PROD_MCP_URL": URL, "AGENT_AUTH_ONDEMAND": "OTHER_TOKEN"}
    )
    assert "vogt-prod" not in claude_servers(tmp_path)
    assert "vogt-prod" not in codex_servers(tmp_path)


@pytest.mark.parametrize(
    "source",
    [
        {"VOGT_PROD_TOKEN": TOKEN},
        {"VOGT_PROD_TOKEN_FILE": "/run/secrets/prod-token"},
        {"AGENT_AUTH_ONDEMAND": "GH_TOKEN VOGT_PROD_TOKEN"},
    ],
    ids=["env", "file", "ondemand"],
)
def test_registered_with_the_wrapper_and_no_token(
    tmp_path: Path, source: dict[str, str]
) -> None:
    calls, _ = bootstrap(tmp_path, {"VOGT_PROD_MCP_URL": URL, **source})
    assert claude_servers(tmp_path)["vogt-prod"] == {
        "type": "stdio",
        "command": INSTALLED,
        "args": [],
    }
    codex = codex_servers(tmp_path)["vogt-prod"]
    assert codex["command"] == INSTALLED
    assert codex["args"] == []
    for var in ("VOGT_PROD_MCP_URL", "VOGT_PROD_TOKEN", "VOGT_PROD_TOKEN_FILE"):
        assert var in codex["env_vars"]
    assert "VOGT_SESSION_TOKEN_FILE" in codex["env_vars"], "the broker needs it"
    assert_no_token_stored(tmp_path, calls)

    # A current Claude entry is read from the file: no second add.
    calls, _ = bootstrap(tmp_path, {"VOGT_PROD_MCP_URL": URL, **source})
    assert [
        "claude",
        "mcp",
        "add",
        "--scope",
        "user",
        "vogt-prod",
        "--",
        INSTALLED,
    ] not in calls


def test_removed_when_the_token_goes(tmp_path: Path) -> None:
    bootstrap(tmp_path, {"VOGT_PROD_MCP_URL": URL, "VOGT_PROD_TOKEN": TOKEN})
    assert "vogt-prod" in claude_servers(tmp_path)
    calls, _ = bootstrap(tmp_path, {"VOGT_PROD_MCP_URL": URL})
    assert "vogt-prod" not in claude_servers(tmp_path)
    assert ["codex", "mcp", "remove", "vogt-prod"] in calls


def test_a_hand_registered_vogt_prod_is_left_alone(tmp_path: Path) -> None:
    mine = {"type": "http", "url": "https://elsewhere.example.test/mcp"}
    (tmp_path / ".claude.json").write_text(
        json.dumps({"mcpServers": {"vogt-prod": mine}}), encoding="utf-8"
    )
    bootstrap(tmp_path, {})
    assert claude_servers(tmp_path)["vogt-prod"] == mine


# --- the wrapper ---------------------------------------------------------


def run_wrapper(
    tmp_path: Path, env: dict[str, str], broker_value: str = ""
) -> tuple[subprocess.CompletedProcess[str], dict[str, Any] | None, list[str] | None]:
    bindir = tmp_path / "wbin"
    bindir.mkdir(exist_ok=True)
    write_shim(bindir, "vogt-mcp-remote", RECORD_SHIM)
    broker = write_shim(bindir, "vogt-agent-auth", BROKER_SHIM)
    out = tmp_path / "bridge.json"
    broker_out = tmp_path / "broker.json"
    for path in (out, broker_out):
        path.unlink(missing_ok=True)
    proc = subprocess.run(
        ["bash", str(WRAPPER)],
        env={
            "PATH": f"{bindir}{os.pathsep}/usr/bin{os.pathsep}/bin",
            "HOME": str(tmp_path),
            "SHIM_OUT": str(out),
            "BROKER_OUT": str(broker_out),
            "BROKER_VALUE": broker_value,
            "VOGT_AGENT_AUTH": str(broker),
            # The local deployment's own session credential, which must not leak
            # to prod nor win over the prod token.
            "VOGT_SESSION_ID": "ses_local",
            "VOGT_HTTP_TOKEN": "local-dev-token",
            "VOGT_TOKEN_FILE": str(tmp_path / "local-token"),
            "VOGT_ENGINE_SESSION_ID": "local-engine-uuid",
            "VOGT_URL": "http://127.0.0.1:8910",
            **env,
        },
        capture_output=True,
        text=True,
        check=False,
    )
    bridge = json.loads(out.read_text(encoding="utf-8")) if out.exists() else None
    broker_argv = (
        json.loads(broker_out.read_text(encoding="utf-8"))
        if broker_out.exists()
        else None
    )
    return proc, bridge, broker_argv


def assert_bridged_to_prod(proc: subprocess.CompletedProcess[str], bridge: Any) -> None:
    assert proc.returncode == 0, proc.stderr
    assert proc.stdout == ""
    assert TOKEN not in proc.stderr
    assert bridge is not None
    assert TOKEN not in json.dumps(bridge["argv"])
    env = bridge["env"]
    assert env["VOGT_URL"] == "https://vogt-prod.example.test"
    assert env["VOGT_HTTP_TOKEN"] == TOKEN
    for gone in (
        "VOGT_SESSION_ID",
        "VOGT_TOKEN_FILE",
        "VOGT_ENGINE_SESSION_ID",
        "VOGT_PROD_TOKEN",
        "VOGT_PROD_TOKEN_FILE",
    ):
        assert gone not in env, gone


def test_wrapper_uses_the_token_from_the_environment(tmp_path: Path) -> None:
    proc, bridge, broker = run_wrapper(
        tmp_path, {"VOGT_PROD_MCP_URL": URL, "VOGT_PROD_TOKEN": TOKEN}
    )
    assert_bridged_to_prod(proc, bridge)
    assert broker is None


def test_wrapper_reads_the_token_file(tmp_path: Path) -> None:
    token_file = tmp_path / "prod-token"
    token_file.write_text(TOKEN + "\n", encoding="utf-8")
    proc, bridge, broker = run_wrapper(
        tmp_path,
        {"VOGT_PROD_MCP_URL": URL + "/", "VOGT_PROD_TOKEN_FILE": str(token_file)},
    )
    assert_bridged_to_prod(proc, bridge)
    assert broker is None


def test_wrapper_fetches_an_ondemand_token_from_the_broker(tmp_path: Path) -> None:
    proc, bridge, broker = run_wrapper(
        tmp_path, {"VOGT_PROD_MCP_URL": URL}, broker_value=TOKEN
    )
    assert_bridged_to_prod(proc, bridge)
    assert broker is not None and broker[1:] == ["fetch", "VOGT_PROD_TOKEN"]


def test_wrapper_refuses_without_a_url(tmp_path: Path) -> None:
    proc, bridge, _ = run_wrapper(tmp_path, {"VOGT_PROD_TOKEN": TOKEN})
    assert proc.returncode == 64
    assert proc.stdout == ""
    assert "VOGT_PROD_MCP_URL" in proc.stderr
    assert bridge is None


def test_wrapper_refuses_when_the_broker_has_nothing(tmp_path: Path) -> None:
    proc, bridge, _ = run_wrapper(tmp_path, {"VOGT_PROD_MCP_URL": URL}, broker_value="")
    assert proc.returncode == 64
    assert proc.stdout == ""
    assert bridge is None
