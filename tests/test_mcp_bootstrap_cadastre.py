"""`vogt-mcp-bootstrap` registers Cadastre as a remote HTTP MCP server.

Cadastre needs no local client: the registration is its URL and an
Authorization header that names `${CADASTRE_HTTP_TOKEN}`, expanded by Claude
Code when it connects. The contract under test is that the stored entry holds
the reference and never the value, that nothing happens without
CADASTRE_MCP_URL, and that a current entry is read from ~/.claude.json
without starting the CLI.

`claude` is replaced by a shim that logs its argv and writes the entry in the
shape the real CLI stores, so the test is offline.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
from pathlib import Path
from typing import Any

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
BOOTSTRAP = REPO_ROOT / "engine" / "deploy" / "mcp-bootstrap.sh"

pytestmark = pytest.mark.skipif(
    not BOOTSTRAP.is_file()
    or shutil.which("bash") is None
    or shutil.which("jq") is None
    or shutil.which("python3") is None,
    reason="needs engine/deploy (absent in the core-alone job), bash, jq and python3",
)

URL = "https://cadastre.example.test/mcp"
TOKEN = "cadastre-token-value-never-stored"

CLAUDE_SHIM = """#!/usr/bin/env python3
import json, os, sys
args = sys.argv[1:]
with open(os.environ["CLAUDE_CALLS"], "a") as fh:
    fh.write(json.dumps(args) + "\\n")
path = os.path.join(os.environ["HOME"], ".claude.json")
data = json.load(open(path)) if os.path.exists(path) else {}
servers = data.setdefault("mcpServers", {})
if args[:2] == ["mcp", "remove"]:
    servers.pop(args[2], None)
elif args[:2] == ["mcp", "add"] and "--transport" in args:
    rest = [a for a in args[2:] if a not in ("--scope", "user", "--transport", "http")]
    header = rest[rest.index("--header") + 1]
    name, url = [a for a in rest if a not in ("--header", header)]
    key, value = header.split(": ", 1)
    servers[name] = {"type": "http", "url": url, "headers": {key: value}}
elif args[:2] == ["mcp", "get"]:
    sys.exit(0 if args[2] in servers else 1)
json.dump(data, open(path, "w"))
"""


def bootstrap(tmp_path: Path, extra: dict[str, str]) -> list[list[str]]:
    bindir = tmp_path / "bin"
    if not bindir.exists():
        bindir.mkdir()
        claude = bindir / "claude"
        claude.write_text(CLAUDE_SHIM, encoding="utf-8")
        claude.chmod(0o755)
    calls = tmp_path / "claude-calls"
    calls.unlink(missing_ok=True)
    proc = subprocess.run(
        ["bash", str(BOOTSTRAP)],
        env={
            "PATH": f"{bindir}{os.pathsep}/usr/bin{os.pathsep}/bin",
            "HOME": str(tmp_path),
            "VOGT_SRC": str(tmp_path / "absent"),
            "CLAUDE_CALLS": str(calls),
            "CADASTRE_HTTP_TOKEN": TOKEN,
            **extra,
        },
        cwd=tmp_path,
        capture_output=True,
        text=True,
        check=False,
    )
    assert proc.returncode == 0, proc.stderr
    assert proc.stdout == "", "stdout is the MCP transport"
    if not calls.exists():
        return []
    lines = calls.read_text(encoding="utf-8").splitlines()
    return [json.loads(line) for line in lines if "cadastre" in line]


def servers(tmp_path: Path) -> dict[str, Any]:
    config = tmp_path / ".claude.json"
    if not config.exists():
        return {}
    loaded: dict[str, Any] = json.loads(config.read_text(encoding="utf-8"))
    return loaded.get("mcpServers", {})


def test_cadastre_is_not_registered_without_a_url(tmp_path: Path) -> None:
    assert bootstrap(tmp_path, {}) == []
    assert "cadastre" not in servers(tmp_path)


def test_cadastre_registers_as_http_with_a_token_reference(tmp_path: Path) -> None:
    calls = bootstrap(tmp_path, {"CADASTRE_MCP_URL": URL})
    assert any(call[:2] == ["mcp", "add"] for call in calls), calls
    entry = servers(tmp_path)["cadastre"]
    assert entry == {
        "type": "http",
        "url": URL,
        "headers": {"Authorization": "Bearer ${CADASTRE_HTTP_TOKEN}"},
    }
    # The value is neither on the CLI's argv nor in the stored config.
    assert TOKEN not in json.dumps(calls)
    assert TOKEN not in (tmp_path / ".claude.json").read_text(encoding="utf-8")

    # A current entry is read from the file: the CLI is not started for it.
    assert bootstrap(tmp_path, {"CADASTRE_MCP_URL": URL}) == []


def test_a_stale_cadastre_entry_is_replaced(tmp_path: Path) -> None:
    stale = {
        "mcpServers": {
            "cadastre": {"type": "stdio", "command": "/usr/local/bin/vogt-cadastre-mcp"}
        }
    }
    (tmp_path / ".claude.json").write_text(json.dumps(stale), encoding="utf-8")
    calls = bootstrap(tmp_path, {"CADASTRE_MCP_URL": URL})
    assert ["mcp", "remove", "cadastre"] in calls
    assert servers(tmp_path)["cadastre"]["type"] == "http"
    assert servers(tmp_path)["cadastre"]["url"] == URL
