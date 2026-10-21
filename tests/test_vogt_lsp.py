"""`vogt-lsp` and its registration by `vogt-mcp-bootstrap` (WI-887).

The launcher and the Codex MCP bridge are driven through the real script, with
a fake language server on PATH standing in for basedpyright: it answers
initialize, definition, hover, references, documentSymbol and workspace/symbol,
publishes one diagnostic on didOpen, and asks the client for configuration the
way basedpyright does. The bootstrap tests replace `claude` and `codex` with
shims that log their argv, so they are offline and check only what is called.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Any

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
DEPLOY = REPO_ROOT / "engine" / "deploy"
VOGT_LSP = DEPLOY / "vogt-lsp.py"
BOOTSTRAP = DEPLOY / "mcp-bootstrap.sh"
PLUGINS = DEPLOY / "claude-plugins"

pytestmark = pytest.mark.skipif(
    not VOGT_LSP.is_file() or shutil.which("bash") is None,
    reason="needs engine/deploy (absent in the core-alone job) and bash",
)

FAKE_SERVER = r"""#!/usr/bin/env python3
import json, os, sys

def read():
    length = 0
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            sys.exit(0)
        line = line.strip()
        if not line:
            break
        if line.lower().startswith(b"content-length:"):
            length = int(line.split(b":", 1)[1])
    return json.loads(sys.stdin.buffer.read(length))

def send(msg):
    body = json.dumps(msg).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(body) + body)
    sys.stdout.buffer.flush()

with open(os.environ["FAKE_LOG"], "a") as fh:
    fh.write(json.dumps({"cwd": os.getcwd(), "venv": os.environ.get("VIRTUAL_ENV")}) + "\n")

def loc(uri, line, ch):
    return {"uri": uri, "range": {"start": {"line": line, "character": ch}, "end": {"line": line, "character": ch + 3}}}

while True:
    msg = read()
    method, mid = msg.get("method"), msg.get("id")
    params = msg.get("params") or {}
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": 9000, "method": "workspace/configuration",
              "params": {"items": [{"section": "basedpyright.analysis"}]}})
        reply = read()
        with open(os.environ["FAKE_LOG"], "a") as fh:
            fh.write(json.dumps({"configuration": reply.get("result")}) + "\n")
        send({"jsonrpc": "2.0", "id": mid, "result": {"capabilities": {}}})
    elif method == "textDocument/didOpen":
        uri = params["textDocument"]["uri"]
        send({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics", "params": {"uri": uri, "diagnostics": [
            {"range": loc(uri, 1, 4)["range"], "severity": 1, "message": "\"undefined_name\" is not defined"}]}})
    elif method == "textDocument/definition":
        uri = params["textDocument"]["uri"]
        send({"jsonrpc": "2.0", "id": mid, "result": [{"targetUri": uri, "targetRange": loc(uri, 0, 0)["range"],
              "targetSelectionRange": loc(uri, 0, 4)["range"]}]})
    elif method == "textDocument/references":
        uri = params["textDocument"]["uri"]
        assert params["position"] == {"line": 1, "character": 4}, params
        send({"jsonrpc": "2.0", "id": mid, "result": [loc(uri, 0, 4), loc(uri, 1, 4)]})
    elif method == "textDocument/hover":
        send({"jsonrpc": "2.0", "id": mid, "result": {"contents": {"kind": "markdown", "value": "def greet() -> str"}}})
    elif method == "textDocument/documentSymbol":
        uri = params["textDocument"]["uri"]
        send({"jsonrpc": "2.0", "id": mid, "result": [{"name": "Greeter", "kind": 5, "range": loc(uri, 3, 0)["range"],
              "selectionRange": loc(uri, 3, 6)["range"], "children": [
              {"name": "hello", "kind": 6, "detail": "(self)", "range": loc(uri, 4, 4)["range"], "selectionRange": loc(uri, 4, 8)["range"]}]}]})
    elif method == "workspace/symbol":
        send({"jsonrpc": "2.0", "id": mid, "result": [{"name": params["query"], "kind": 12, "containerName": "pkg",
              "location": loc("file:///w/a.py", 2, 0)}]})
    elif method == "shutdown":
        send({"jsonrpc": "2.0", "id": mid, "result": None})
    elif method == "exit":
        sys.exit(0)
    elif mid is not None:
        send({"jsonrpc": "2.0", "id": mid, "result": None})
"""


@pytest.fixture
def project(tmp_path: Path) -> Path:
    root = tmp_path / "proj"
    (root / "pkg").mkdir(parents=True)
    (root / "pyproject.toml").write_text("[project]\nname = 'p'\n")
    (root / "pkg" / "mod.py").write_text(
        "def greet():\n    return greet()\n\nclass Greeter:\n    def hello(self): ...\n"
    )
    return root


@pytest.fixture
def env(tmp_path: Path) -> dict[str, str]:
    bindir = tmp_path / "bin"
    bindir.mkdir()
    server = bindir / "basedpyright-langserver"
    server.write_text(FAKE_SERVER)
    server.chmod(0o755)
    return {
        "PATH": f"{bindir}{os.pathsep}{Path(sys.executable).parent}{os.pathsep}/usr/bin{os.pathsep}/bin",
        "HOME": str(tmp_path),
        "FAKE_LOG": str(tmp_path / "fake.log"),
        "VIRTUAL_ENV": "/somewhere/else/.venv",
        "VOGT_LSP_DIAGNOSTICS_WAIT": "5",
        "VOGT_LSP_REQUEST_TIMEOUT": "10",
    }


def bridge(
    cwd: Path, env: dict[str, str], calls: list[dict[str, Any]], lang: str = "python"
) -> list[dict[str, Any]]:
    messages = [
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {"protocolVersion": "2025-06-18"},
        },
        {"jsonrpc": "2.0", "method": "notifications/initialized"},
        {"jsonrpc": "2.0", "id": 2, "method": "tools/list"},
    ]
    for index, call in enumerate(calls):
        messages.append(
            {"jsonrpc": "2.0", "id": 10 + index, "method": "tools/call", "params": call}
        )
    proc = subprocess.run(
        [sys.executable, str(VOGT_LSP), "mcp", lang],
        input="".join(json.dumps(m) + "\n" for m in messages),
        cwd=cwd,
        env=env,
        capture_output=True,
        text=True,
        timeout=60,
        check=False,
    )
    assert proc.returncode == 0, proc.stderr
    # Every stdout line is a protocol frame: nothing else may be printed there.
    return [json.loads(line) for line in proc.stdout.splitlines()]


def text(reply: dict[str, Any]) -> str:
    return str(reply["result"]["content"][0]["text"])


def test_bridge_lists_the_klaudia_tool_set(project: Path, env: dict[str, str]) -> None:
    replies = bridge(project, env, [])
    assert replies[0]["result"]["serverInfo"]["name"] == "vogt-lsp-python"
    names = [t["name"] for t in replies[1]["result"]["tools"]]
    assert names == [
        "definition",
        "references",
        "hover",
        "document_symbols",
        "workspace_symbol",
        "diagnostics",
    ]


def test_bridge_answers_every_tool_through_the_server(
    project: Path, env: dict[str, str], tmp_path: Path
) -> None:
    replies = bridge(
        project / "pkg",
        env,
        [
            {
                "name": "definition",
                "arguments": {"file": "mod.py", "line": 2, "character": 12},
            },
            {
                "name": "references",
                "arguments": {"file": "mod.py", "line": 2, "character": 5},
            },
            {
                "name": "hover",
                "arguments": {"file": str(project / "pkg" / "mod.py"), "line": 1},
            },
            {"name": "document_symbols", "arguments": {"file": "mod.py"}},
            {"name": "workspace_symbol", "arguments": {"query": "greet"}},
            {"name": "diagnostics", "arguments": {"file": "mod.py"}},
        ],
    )
    by_id = {r["id"]: r for r in replies}
    mod = (project / "pkg" / "mod.py").resolve()
    assert text(by_id[10]) == f"1 definitions:\n{mod}:1:5  def greet():"
    assert text(by_id[11]).splitlines() == [
        "2 references:",
        f"{mod}:1:5  def greet():",
        f"{mod}:2:5  return greet()",
    ]
    assert text(by_id[12]) == "def greet() -> str"
    assert text(by_id[13]).splitlines() == [
        f"[class] Greeter  {mod}:4:7",
        f"[method] hello (self) (in Greeter)  {mod}:5:9",
    ]
    assert text(by_id[14]) == "[function] greet (in pkg)  /w/a.py:3:1"
    assert text(by_id[15]).splitlines() == [
        f"1 problem(s) in {mod}:",
        '  [error] 2:5  "undefined_name" is not defined',
    ]

    log = [
        json.loads(line) for line in (tmp_path / "fake.log").read_text().splitlines()
    ]
    # One server for the root, started at the marker, with the leaked venv gone.
    starts = [entry for entry in log if "cwd" in entry]
    assert starts == [{"cwd": str(project.resolve()), "venv": None}]
    # workspace/configuration is answered from the launcher's settings.
    assert {"configuration": [{"typeCheckingMode": "standard"}]} in log


def test_bridge_reports_bad_input_as_tool_errors(
    project: Path, env: dict[str, str]
) -> None:
    (project / "notes.txt").write_text("x")
    replies = bridge(
        project,
        env,
        [
            {"name": "definition", "arguments": {"file": "pkg/mod.py"}},
            {"name": "hover", "arguments": {"file": "notes.txt", "line": 1}},
            {"name": "diagnostics", "arguments": {"file": "missing.py"}},
        ],
    )
    errors = [r for r in replies if r.get("id", 0) >= 10]
    assert all(r["result"]["isError"] for r in errors)
    assert "1-based line" in text(errors[0])
    assert "not a python file" in text(errors[1])
    assert "no such file" in text(errors[2])


def test_java_is_off_until_opted_in(project: Path, env: dict[str, str]) -> None:
    proc = subprocess.run(
        [sys.executable, str(VOGT_LSP), "check", "java"],
        env=env,
        capture_output=True,
        text=True,
        check=False,
    )
    assert proc.returncode != 0 and "VOGT_LSP_JAVA=1" in proc.stderr
    replies = bridge(
        project,
        env,
        [{"name": "diagnostics", "arguments": {"file": "x.java"}}],
        lang="java",
    )
    assert replies[-1]["result"]["isError"]


def test_stdio_mode_execs_the_server_at_the_root(
    project: Path, env: dict[str, str], tmp_path: Path
) -> None:
    # initialize, then the answer to the server's configuration request, then exit.
    messages = [
        {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}},
        {"jsonrpc": "2.0", "id": 9000, "result": [None]},
        {"jsonrpc": "2.0", "method": "exit"},
    ]
    bodies = [json.dumps(m).encode() for m in messages]
    frames = b"".join(b"Content-Length: %d\r\n\r\n" % len(b) + b for b in bodies)
    proc = subprocess.run(
        [sys.executable, str(VOGT_LSP), "python"],
        cwd=project / "pkg",
        env=env,
        input=frames,
        capture_output=True,
        timeout=30,
        check=False,
    )
    assert b'"capabilities"' in proc.stdout
    start = json.loads((tmp_path / "fake.log").read_text().splitlines()[0])
    assert start == {"cwd": str(project.resolve()), "venv": None}


def test_rust_root_prefers_the_outer_workspace(tmp_path: Path) -> None:
    ws = tmp_path / "ws"
    (ws / "crates" / "a" / "src").mkdir(parents=True)
    (ws / "Cargo.toml").write_text("[workspace]\nmembers = ['crates/a']\n")
    (ws / "crates" / "a" / "Cargo.toml").write_text("[package]\nname = 'a'\n")
    src = ws / "crates" / "a" / "src"
    code = (
        "import importlib.util, sys\n"
        f"spec = importlib.util.spec_from_file_location('vl', {str(VOGT_LSP)!r})\n"
        "m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)\n"
        f"print(m.find_root('rust', __import__('pathlib').Path({str(src)!r})))\n"
    )
    out = subprocess.run(
        [sys.executable, "-c", code], capture_output=True, text=True, check=True
    )
    assert out.stdout.strip() == str(ws.resolve())


def test_plugin_maps_every_language_to_the_launcher() -> None:
    market = json.loads((PLUGINS / ".claude-plugin" / "marketplace.json").read_text())
    assert market["name"] == "vogt"
    assert [p["name"] for p in market["plugins"]] == ["vogt-lsp"]
    lsp = json.loads((PLUGINS / "vogt-lsp" / ".lsp.json").read_text())
    assert {lang: s["args"] for lang, s in lsp.items()} == {
        "python": ["python"],
        "rust": ["rust"],
        "java": ["java"],
    }
    assert all(s["command"] == "/usr/local/bin/vogt-lsp" for s in lsp.values())
    assert lsp["python"]["extensionToLanguage"] == {".py": "python", ".pyi": "python"}
    assert lsp["rust"]["extensionToLanguage"] == {".rs": "rust"}
    assert lsp["java"]["startupTimeout"] == 120000
    # The pinned CLI predates requestTimeout; an unknown key would fail the plugin.
    assert all("requestTimeout" not in s for s in lsp.values())


# --- bootstrap ---------------------------------------------------------------

CLI_SHIM = r"""#!/usr/bin/env bash
printf '%s\n' "$(basename "$0") $*" >> "$SHIM_LOG"
if [[ "$(basename "$0")" == codex && "$1 $2" == "mcp get" ]]; then
    f="$SHIM_STATE/codex-$3"; [[ -f "$f" ]] && cat "$f" && exit 0; exit 1
fi
if [[ "$(basename "$0")" == codex && "$1 $2" == "mcp add" ]]; then
    name="$3"; shift 4; echo "$*" > "$SHIM_STATE/codex-$name"
fi
if [[ "$(basename "$0")" == codex && "$1 $2" == "mcp remove" ]]; then rm -f "$SHIM_STATE/codex-$3"; fi
exit 0
"""

LSP_SHIM = r"""#!/usr/bin/env bash
[[ "$1" == check ]] && [[ " $LSP_AVAILABLE " == *" $2 "* ]]
"""


def run_bootstrap(
    tmp_path: Path, available: str, installed_version: str | None
) -> list[str]:
    bindir = tmp_path / "bin"
    bindir.mkdir(exist_ok=True)
    state = tmp_path / "state"
    state.mkdir(exist_ok=True)
    for name in ("claude", "codex"):
        (bindir / name).write_text(CLI_SHIM)
        (bindir / name).chmod(0o755)
    lsp = bindir / "vogt-lsp"
    lsp.write_text(LSP_SHIM)
    lsp.chmod(0o755)
    claude_dir = tmp_path / ".claude"
    (claude_dir / "plugins").mkdir(parents=True, exist_ok=True)
    if installed_version:
        (claude_dir / "plugins" / "installed_plugins.json").write_text(
            json.dumps(
                {
                    "version": 2,
                    "plugins": {
                        "vogt-lsp@vogt": [
                            {"scope": "user", "version": installed_version}
                        ]
                    },
                }
            )
        )
        (claude_dir / "plugins" / "known_marketplaces.json").write_text('{"vogt": {}}')
        (claude_dir / "settings.json").write_text(
            '{"enabledPlugins": {"vogt-lsp@vogt": true}}'
        )
    log = tmp_path / "shim.log"
    log.write_text("")
    env = {
        "PATH": f"{bindir}{os.pathsep}{Path(sys.executable).parent}{os.pathsep}/usr/bin{os.pathsep}/bin",
        "HOME": str(tmp_path),
        "SHIM_LOG": str(log),
        "SHIM_STATE": str(state),
        "LSP_AVAILABLE": available,
        "VOGT_LSP": str(lsp),
        "VOGT_CLAUDE_PLUGINS": str(PLUGINS),
        "VOGT_MCP_URL": "http://127.0.0.1:1/mcp",
    }
    proc = subprocess.run(
        ["bash", str(BOOTSTRAP)],
        cwd=tmp_path,
        env=env,
        capture_output=True,
        text=True,
        timeout=60,
        check=False,
    )
    assert proc.returncode == 0, proc.stderr
    assert proc.stdout == ""
    return [
        line
        for line in log.read_text().splitlines()
        if "plugin" in line or "vogt-lsp" in line
    ]


@pytest.mark.skipif(
    shutil.which("jq") is None or shutil.which("flock") is None,
    reason="needs jq and flock",
)
def test_bootstrap_installs_the_plugin_and_registers_codex_bridges(
    tmp_path: Path,
) -> None:
    calls = run_bootstrap(tmp_path, "python rust", None)
    assert f"claude plugin marketplace add {PLUGINS}" in calls
    assert "claude plugin install vogt-lsp@vogt --scope user" in calls
    lsp = tmp_path / "bin" / "vogt-lsp"
    assert f"codex mcp add vogt-lsp-python -- {lsp} mcp python" in calls
    assert f"codex mcp add vogt-lsp-rust -- {lsp} mcp rust" in calls
    assert not any("vogt-lsp-java" in c and "add" in c for c in calls)


@pytest.mark.skipif(
    shutil.which("jq") is None or shutil.which("flock") is None,
    reason="needs jq and flock",
)
def test_bootstrap_is_idempotent_and_drops_a_server_that_went_away(
    tmp_path: Path,
) -> None:
    version = json.loads(
        (PLUGINS / "vogt-lsp" / ".claude-plugin" / "plugin.json").read_text()
    )["version"]
    run_bootstrap(tmp_path, "python rust", None)
    calls = run_bootstrap(tmp_path, "python", version)
    assert not any(c.startswith("claude plugin") for c in calls)
    assert not any(c.startswith("codex mcp add") for c in calls)
    assert "codex mcp remove vogt-lsp-rust" in calls

    stale = run_bootstrap(tmp_path, "python", "0.0.1")
    assert "claude plugin update vogt-lsp@vogt --scope user" in stale
