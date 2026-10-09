"""The read-only MCP wrapper and `git-forgejo`, driven through the real scripts.

`engine/deploy/readonly-mcp.sh` is what `vogt-mcp-bootstrap` registers for the
optional GitHub, Grafana and Gitea/Forgejo MCP servers, so the read-only
switches it sets are the contract: a session can neither drop them nor point
the server at a broader credential. `engine/deploy/git-forgejo.sh` exists
because the hand-written `git -c http.extraheader="Authorization: token …"`
split at its spaces in four sessions; it must hand git the header as one
value, scoped to the forge, and never put the token on git's command line.

Each upstream binary (and git) is replaced by a shim on PATH that records its
argv and environment, so the tests are offline and exercise only the
wrappers.
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
DEPLOY = REPO_ROOT / "engine" / "deploy"
READONLY = DEPLOY / "readonly-mcp.sh"
GIT_FORGEJO = DEPLOY / "git-forgejo.sh"
BOOTSTRAP = DEPLOY / "mcp-bootstrap.sh"

pytestmark = pytest.mark.skipif(
    not (READONLY.is_file() and GIT_FORGEJO.is_file()) or shutil.which("bash") is None,
    reason="needs engine/deploy (absent in the core-alone job) and bash",
)

SHIM = """#!/usr/bin/env python3
import json, os, sys
with open(os.environ["SHIM_OUT"], "w") as fh:
    json.dump({"argv": sys.argv, "env": dict(os.environ)}, fh)
"""


def shim_dir(tmp_path: Path, *names: str) -> Path:
    bindir = tmp_path / "bin"
    bindir.mkdir(exist_ok=True)
    for name in names:
        path = bindir / name
        path.write_text(SHIM, encoding="utf-8")
        path.chmod(0o755)
    return bindir


def run(
    script: Path, args: list[str], env: dict[str, str], tmp_path: Path, bindir: Path
) -> tuple[subprocess.CompletedProcess[str], dict[str, Any] | None]:
    out = tmp_path / "shim.json"
    if out.exists():
        out.unlink()
    full_env = {
        "PATH": f"{bindir}{os.pathsep}/usr/bin{os.pathsep}/bin",
        "HOME": str(tmp_path),
        "SHIM_OUT": str(out),
        **env,
    }
    proc = subprocess.run(
        ["bash", str(script), *args],
        env=full_env,
        capture_output=True,
        text=True,
        check=False,
    )
    recorded = json.loads(out.read_text(encoding="utf-8")) if out.exists() else None
    return proc, recorded


def test_github_is_read_only_with_the_pinned_toolsets(tmp_path: Path) -> None:
    bindir = shim_dir(tmp_path, "github-mcp-server")
    proc, rec = run(
        READONLY,
        ["github"],
        {
            "GITHUB_MCP_TOKEN": "ro-token",
            # A session trying to widen the server: both must be overridden.
            "GITHUB_READ_ONLY": "0",
            "GITHUB_TOOLSETS": "all",
            "GITHUB_TOOLS": "create_pull_request",
        },
        tmp_path,
        bindir,
    )
    assert proc.returncode == 0, proc.stderr
    assert rec is not None
    argv, env = rec["argv"], rec["env"]
    assert argv[1:] == ["stdio", "--read-only"]
    assert env["GITHUB_READ_ONLY"] == "1"
    assert env["GITHUB_PERSONAL_ACCESS_TOKEN"] == "ro-token"
    assert env["GITHUB_TOOLSETS"] == (
        "actions,pull_requests,repos,code_security,dependabot"
    )
    assert "GITHUB_TOOLS" not in env
    assert proc.stdout == ""


def test_grafana_disables_writes_and_usage_stats(tmp_path: Path) -> None:
    bindir = shim_dir(tmp_path, "mcp-grafana")
    proc, rec = run(
        READONLY,
        ["grafana"],
        {
            "GRAFANA_URL": "http://grafana.example",
            "GRAFANA_SERVICE_ACCOUNT_TOKEN": "viewer",
            "GRAFANA_API_KEY": "admin-key",
        },
        tmp_path,
        bindir,
    )
    assert proc.returncode == 0, proc.stderr
    assert rec is not None
    assert rec["argv"][1:] == [
        "-t",
        "stdio",
        "--disable-write",
        "--usage-stats=disabled",
    ]
    assert "GRAFANA_API_KEY" not in rec["env"]


def test_gitea_is_read_only(tmp_path: Path) -> None:
    bindir = shim_dir(tmp_path, "gitea-mcp")
    proc, rec = run(
        READONLY,
        ["gitea"],
        {
            "GITEA_HOST": "https://forge.example",
            "GITEA_MCP_TOKEN": "ro",
            "GITEA_READONLY": "false",
        },
        tmp_path,
        bindir,
    )
    assert proc.returncode == 0, proc.stderr
    assert rec is not None
    assert rec["argv"][1:] == ["-t", "stdio", "-r"]
    assert rec["env"]["GITEA_READONLY"] == "true"
    assert rec["env"]["GITEA_ACCESS_TOKEN"] == "ro"


@pytest.mark.parametrize(
    ("server", "binary"),
    [
        ("github", "github-mcp-server"),
        ("grafana", "mcp-grafana"),
        ("gitea", "gitea-mcp"),
    ],
)
def test_missing_token_refuses_without_starting(
    tmp_path: Path, server: str, binary: str
) -> None:
    bindir = shim_dir(tmp_path, binary)
    proc, rec = run(READONLY, [server], {}, tmp_path, bindir)
    assert proc.returncode == 64
    assert rec is None
    assert proc.stdout == ""


def test_git_forgejo_passes_one_scoped_header_outside_argv(tmp_path: Path) -> None:
    bindir = shim_dir(tmp_path, "git")
    token = "tok with spaces"  # would split under the unquoted one-liner
    proc, rec = run(
        GIT_FORGEJO,
        ["push", "origin", "main"],
        {
            "FORGEJO_TOKEN": token,
            "FORGEJO_URL": "https://forge.example",
            "GIT_CONFIG_COUNT": "1",
            "GIT_CONFIG_KEY_0": "user.name",
            "GIT_CONFIG_VALUE_0": "someone",
        },
        tmp_path,
        bindir,
    )
    assert proc.returncode == 0, proc.stderr
    assert rec is not None
    argv, env = rec["argv"], rec["env"]
    assert argv[1:] == ["push", "origin", "main"]
    assert not any(token in arg for arg in argv)
    assert env["GIT_CONFIG_COUNT"] == "2"
    assert env["GIT_CONFIG_KEY_0"] == "user.name"
    assert env["GIT_CONFIG_KEY_1"] == "http.https://forge.example/.extraheader"
    assert env["GIT_CONFIG_VALUE_1"] == f"Authorization: token {token}"
    assert token not in proc.stdout + proc.stderr


@pytest.mark.parametrize(
    "env",
    [
        {"FORGEJO_URL": "https://forge.example"},
        {"FORGEJO_TOKEN": "t"},
        {"FORGEJO_TOKEN": "t", "FORGEJO_URL": "forge.example"},
    ],
)
def test_git_forgejo_refuses_without_token_or_url(
    tmp_path: Path, env: dict[str, str]
) -> None:
    bindir = shim_dir(tmp_path, "git")
    proc, rec = run(GIT_FORGEJO, ["status"], env, tmp_path, bindir)
    assert proc.returncode == 64
    assert rec is None


CLI_SHIM = """#!/usr/bin/env bash
# A stand-in for `claude`/`codex mcp`: records registrations in a JSON file.
set -euo pipefail
db="$HOME/$(basename "$0").json"
[[ -f "$db" ]] || echo '{}' >"$db"
[[ "$1" == mcp ]] || exit 0
shift
op="$1"; shift
python3 - "$db" "$op" "$@" <<'PY'
import json, sys
db, op, *rest = sys.argv[1:]
data = json.load(open(db))
args = [a for a in rest if not a.startswith("--scope") and a not in ("user", "--json")]
if op == "get":
    name = args[0]
    if name not in data:
        sys.exit(1)
    print(json.dumps(data[name]))
elif op == "remove":
    data.pop(args[0], None)
elif op == "add":
    name, cmd = args[0], args[args.index("--") + 1 :] if "--" in args else args[1:]
    data[name] = {"command": cmd}
json.dump(data, open(db, "w"))
PY
"""


@pytest.mark.skipif(
    shutil.which("python3") is None, reason="the CLI stand-in needs python3"
)
def test_bootstrap_registers_only_servers_whose_token_is_present(
    tmp_path: Path,
) -> None:
    bindir = tmp_path / "bin"
    bindir.mkdir()
    (bindir / "claude").write_text(CLI_SHIM, encoding="utf-8")
    (bindir / "claude").chmod(0o755)
    for name in ("github-mcp-server", "mcp-grafana", "gitea-mcp"):
        (bindir / name).write_text("#!/bin/sh\n", encoding="utf-8")
        (bindir / name).chmod(0o755)
    wrapper = bindir / "vogt-readonly-mcp"
    wrapper.write_text("#!/bin/sh\n", encoding="utf-8")
    wrapper.chmod(0o755)
    base_env = {
        "PATH": f"{bindir}{os.pathsep}/usr/bin{os.pathsep}/bin",
        "HOME": str(tmp_path),
        "VOGT_SRC": str(tmp_path / "absent"),
        "VOGT_READONLY_MCP_WRAPPER": str(wrapper),
    }

    def bootstrap(extra: dict[str, str]) -> dict[str, Any]:
        proc = subprocess.run(
            ["bash", str(BOOTSTRAP)],
            env={**base_env, **extra},
            capture_output=True,
            text=True,
            check=False,
        )
        assert proc.returncode == 0, proc.stderr
        assert proc.stdout == "", "stdout is the MCP transport"
        loaded: dict[str, Any] = json.loads(
            (tmp_path / "claude.json").read_text(encoding="utf-8")
        )
        return loaded

    first = bootstrap(
        {"GITHUB_MCP_TOKEN": "x", "GITEA_HOST": "https://f", "GITEA_MCP_TOKEN": "y"}
    )
    assert set(first) >= {"github-ro", "forgejo-ro"}
    assert "grafana-ro" not in first
    assert first["github-ro"] == {"command": [str(wrapper), "github"]}
    # No token value ever reaches the stored registration.
    assert "x" not in json.dumps(first["github-ro"]).replace(str(wrapper), "")

    second = bootstrap({"GITEA_HOST": "https://f", "GITEA_MCP_TOKEN": "y"})
    assert "github-ro" not in second
    assert "forgejo-ro" in second


@pytest.mark.skipif(shutil.which("jq") is None, reason="the fast path reads with jq")
def test_bootstrap_reads_existing_claude_registrations_without_the_cli(
    tmp_path: Path,
) -> None:
    """WI-927: on every session launch the bootstrap used to ask `claude mcp
    get` (node start-up plus a server health check, ~0.7 s each) whether
    each server was registered. When ~/.claude.json already says so, it must
    not start the CLI at all."""
    bindir = tmp_path / "bin"
    bindir.mkdir()
    calls = tmp_path / "claude-calls"
    claude = bindir / "claude"
    claude.write_text(f'#!/bin/sh\necho "$*" >> {calls}\nexit 1\n', encoding="utf-8")
    claude.chmod(0o755)
    for name in ("github-mcp-server", "gitea-mcp"):
        (bindir / name).write_text("#!/bin/sh\n", encoding="utf-8")
        (bindir / name).chmod(0o755)
    wrapper = bindir / "vogt-readonly-mcp"
    wrapper.write_text("#!/bin/sh\n", encoding="utf-8")
    wrapper.chmod(0o755)

    def stdio(command: str, *args: str) -> dict[str, Any]:
        return {"type": "stdio", "command": command, "args": list(args), "env": {}}

    config = {
        "mcpServers": {
            "vogt": stdio("/usr/local/bin/vogt-mcp"),
            "github-ro": stdio(str(wrapper), "github"),
            "forgejo-ro": stdio(str(wrapper), "gitea"),
        }
    }
    (tmp_path / ".claude.json").write_text(json.dumps(config), encoding="utf-8")
    proc = subprocess.run(
        ["bash", str(BOOTSTRAP)],
        env={
            "PATH": f"{bindir}{os.pathsep}/usr/bin{os.pathsep}/bin",
            "HOME": str(tmp_path),
            "VOGT_SRC": str(tmp_path / "absent"),
            "VOGT_READONLY_MCP_WRAPPER": str(wrapper),
            # github and forgejo wanted; grafana not (no token) and absent.
            "GITHUB_MCP_TOKEN": "x",
            "GITEA_HOST": "https://f",
            "GITEA_MCP_TOKEN": "y",
        },
        cwd=tmp_path,
        capture_output=True,
        text=True,
        check=False,
    )
    assert proc.returncode == 0, proc.stderr
    assert not calls.exists(), calls.read_text(encoding="utf-8")

    # A stale command is still reconciled through the CLI.
    config["mcpServers"]["vogt"] = stdio("/old/mydevenv2-vogt-mcp")
    (tmp_path / ".claude.json").write_text(json.dumps(config), encoding="utf-8")
    subprocess.run(
        ["bash", str(BOOTSTRAP)],
        env={
            "PATH": f"{bindir}{os.pathsep}/usr/bin{os.pathsep}/bin",
            "HOME": str(tmp_path),
            "VOGT_SRC": str(tmp_path / "absent"),
            "VOGT_READONLY_MCP_WRAPPER": str(wrapper),
        },
        cwd=tmp_path,
        capture_output=True,
        text=True,
        check=False,
    )
    assert "mcp get vogt" in calls.read_text(encoding="utf-8")


KLAUDIA_MCP = DEPLOY / "klaudia-mcp.sh"


def klaudia_mcp(tmp_path: Path, *args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["bash", str(KLAUDIA_MCP), *args],
        env={
            "PATH": f"/usr/bin{os.pathsep}/bin",
            "HOME": str(tmp_path),
            "KLAUDIA_CONFIG_DIR": str(tmp_path / "kl"),
        },
        capture_output=True,
        text=True,
        check=False,
    )


@pytest.mark.skipif(shutil.which("python3") is None, reason="the helper is python3")
def test_klaudia_mcp_upserts_and_removes_only_its_own_entry(tmp_path: Path) -> None:
    """WI-974: Klaudia has no `mcp add`, so this helper is how its
    `.mcp.json` is written. It must keep everything else in the file, and
    `remove` must spare an entry somebody registered with another command."""
    config = tmp_path / "kl" / ".mcp.json"
    config.parent.mkdir()
    config.write_text(
        json.dumps({"theme": "x", "mcpServers": {"mine": {"command": "/bin/mine"}}}),
        encoding="utf-8",
    )
    first = klaudia_mcp(tmp_path, "set", "github-ro", "/w", "github")
    assert first.returncode == 0, first.stderr
    assert first.stdout == "", "the bootstrap's stdout is an MCP transport"
    data = json.loads(config.read_text(encoding="utf-8"))
    assert data["theme"] == "x"
    assert data["mcpServers"] == {
        "mine": {"command": "/bin/mine"},
        "github-ro": {"command": "/w", "args": ["github"]},
    }

    # Unchanged entry: the file is not rewritten.
    before = config.stat().st_mtime_ns
    assert klaudia_mcp(tmp_path, "set", "github-ro", "/w", "github").returncode == 0
    assert config.stat().st_mtime_ns == before

    assert klaudia_mcp(tmp_path, "remove", "mine", "/w").returncode == 0
    assert klaudia_mcp(tmp_path, "remove", "github-ro", "/w").returncode == 0
    data = json.loads(config.read_text(encoding="utf-8"))
    assert data["mcpServers"] == {"mine": {"command": "/bin/mine"}}

    with_env = klaudia_mcp(tmp_path, "set", "-e", "URL=http://k:1", "kom", "/k")
    assert with_env.returncode == 0, with_env.stderr
    data = json.loads(config.read_text(encoding="utf-8"))
    assert data["mcpServers"]["kom"] == {"command": "/k", "env": {"URL": "http://k:1"}}
    assert klaudia_mcp(tmp_path, "set", "-e", "NOVALUE", "kom", "/k").returncode == 2
    assert klaudia_mcp(tmp_path, "remove", "kom").returncode == 2

    config.write_text("{not json", encoding="utf-8")
    broken = klaudia_mcp(tmp_path, "set", "vogt", "/v")
    assert broken.returncode == 1
    assert config.read_text(encoding="utf-8") == "{not json"


OPENCODE_SHIM = """#!/usr/bin/env python3
import json, os, sys
log = os.path.join(os.environ["HOME"], "opencode-calls.json")
calls = json.load(open(log)) if os.path.exists(log) else []
calls.append(sys.argv[1:])
json.dump(calls, open(log, "w"))
args = sys.argv[1:]
if args[:2] == ["mcp", "add"]:
    cfg_dir = os.path.join(os.environ["HOME"], ".config", "opencode")
    os.makedirs(cfg_dir, exist_ok=True)
    cfg = os.path.join(cfg_dir, "opencode.json")
    data = json.load(open(cfg)) if os.path.exists(cfg) else {}
    data.setdefault("mcp", {})[args[2]] = {
        "type": "local", "command": args[args.index("--") + 1 :]
    }
    json.dump(data, open(cfg, "w"))
"""


@pytest.mark.skipif(
    shutil.which("python3") is None, reason="the CLI stand-ins need python3"
)
def test_bootstrap_registers_vogt_and_readonly_servers_for_klaudia_and_opencode(
    tmp_path: Path,
) -> None:
    """WI-974/WI-975: a Klaudia session had no MCP servers at all, and an
    opencode session none of the read-only ones, because the bootstrap only
    knew Claude Code and Codex for them."""
    bindir = tmp_path / "bin"
    bindir.mkdir()
    (bindir / "opencode").write_text(OPENCODE_SHIM, encoding="utf-8")
    (bindir / "klaudia").write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    for name in ("opencode", "klaudia"):
        (bindir / name).chmod(0o755)
    for name in ("github-mcp-server", "mcp-grafana", "gitea-mcp"):
        (bindir / name).write_text("#!/bin/sh\n", encoding="utf-8")
        (bindir / name).chmod(0o755)
    wrapper = bindir / "vogt-readonly-mcp"
    wrapper.write_text("#!/bin/sh\n", encoding="utf-8")
    wrapper.chmod(0o755)
    base_env = {
        "PATH": f"{bindir}{os.pathsep}/usr/bin{os.pathsep}/bin",
        "HOME": str(tmp_path),
        "VOGT_SRC": str(tmp_path / "absent"),
        "VOGT_READONLY_MCP_WRAPPER": str(wrapper),
        "VOGT_KLAUDIA_MCP": str(KLAUDIA_MCP),
    }
    klaudia_config = tmp_path / ".klaudia" / ".mcp.json"
    opencode_calls = tmp_path / "opencode-calls.json"

    def bootstrap(extra: dict[str, str]) -> None:
        proc = subprocess.run(
            ["bash", str(BOOTSTRAP)],
            env={**base_env, **extra},
            cwd=tmp_path,
            capture_output=True,
            text=True,
            check=False,
        )
        assert proc.returncode == 0, proc.stderr
        assert proc.stdout == "", "stdout is the MCP transport"
        assert "failed" not in proc.stderr, proc.stderr

    tokens = {"GITHUB_MCP_TOKEN": "x"}
    bootstrap(tokens)
    servers = json.loads(klaudia_config.read_text(encoding="utf-8"))["mcpServers"]
    assert servers == {
        "vogt": {"command": "/usr/local/bin/vogt-mcp"},
        "github-ro": {"command": str(wrapper), "args": ["github"]},
    }
    assert ["mcp", "add", "github-ro", "--", str(wrapper), "github"] in json.loads(
        opencode_calls.read_text(encoding="utf-8")
    )

    # Already registered: opencode is not asked again.
    opencode_calls.unlink()
    bootstrap(tokens)
    assert not opencode_calls.exists(), opencode_calls.read_text(encoding="utf-8")

    # The token is gone: Klaudia's entry goes with it.
    bootstrap({})
    servers = json.loads(klaudia_config.read_text(encoding="utf-8"))["mcpServers"]
    assert servers == {"vogt": {"command": "/usr/local/bin/vogt-mcp"}}


def _codex_tables(text: str) -> dict[str, dict[str, Any]]:
    import tomllib

    parsed = tomllib.loads(text)
    servers = parsed["mcp_servers"]
    assert isinstance(servers, dict)
    return servers


@pytest.mark.skipif(
    shutil.which("python3") is None or shutil.which("flock") is None,
    reason="the Codex config rewrite needs python3 and flock",
)
def test_parallel_codex_bootstraps_write_one_table_each(tmp_path: Path) -> None:
    """GitHub #914: unlocked check-then-append raced when several sessions
    started together, leaving duplicate `[mcp_servers.*]` tables. The file
    then no longer parsed, so every later start appended again."""
    import tomllib

    bindir = tmp_path / "bin"
    bindir.mkdir()
    # The rewrite no longer asks `codex mcp` while a server is wanted, but the
    # bootstrap still requires the binary to be present before it touches the file.
    codex = bindir / "codex"
    codex.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    codex.chmod(0o755)
    for name in ("github-mcp-server", "mcp-grafana", "gitea-mcp"):
        (bindir / name).write_text("#!/bin/sh\n", encoding="utf-8")
        (bindir / name).chmod(0o755)
    wrapper = bindir / "vogt-readonly-mcp"
    wrapper.write_text("#!/bin/sh\n", encoding="utf-8")
    wrapper.chmod(0o755)
    codex_home = tmp_path / "codex"
    config = codex_home / "config.toml"
    config.parent.mkdir()
    # An operator's own table must survive the rewrite.
    config.write_text('[other]\nkept = true\n', encoding="utf-8")
    env = {
        "PATH": f"{bindir}{os.pathsep}/usr/bin{os.pathsep}/bin",
        "HOME": str(tmp_path),
        "CODEX_HOME": str(codex_home),
        "VOGT_SRC": str(tmp_path / "absent"),
        "VOGT_READONLY_MCP_WRAPPER": str(wrapper),
        "GITHUB_MCP_TOKEN": "x",
        "VOGT_GITHUB_MCP_TOOLSETS": "repos",
        "GRAFANA_URL": "http://grafana.example",
        "GRAFANA_SERVICE_ACCOUNT_TOKEN": "viewer",
        "GITEA_HOST": "https://forge.example",
        "GITEA_MCP_TOKEN": "y",
    }
    procs = [
        subprocess.Popen(  # noqa: S603 — fixed argv, no shell
            ["bash", str(BOOTSTRAP)],
            env=env,
            cwd=tmp_path,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        for _ in range(8)
    ]
    for proc in procs:
        stdout, stderr = proc.communicate(timeout=60)
        assert proc.returncode == 0, stderr
        assert stdout == "", "stdout is the MCP transport"
        assert "not valid TOML" not in stderr

    text = config.read_text(encoding="utf-8")
    servers = _codex_tables(text)
    assert set(servers) == {"github-ro", "grafana-ro", "forgejo-ro"}
    assert text.count("[mcp_servers.github-ro]") == 1
    assert text.count("[mcp_servers.grafana-ro]") == 1
    assert text.count("[mcp_servers.forgejo-ro]") == 1
    assert servers["github-ro"]["command"] == str(wrapper)
    assert servers["github-ro"]["args"] == ["github"]
    assert "GITHUB_MCP_TOKEN" in servers["github-ro"]["env_vars"]
    assert tomllib.loads(text)["other"] == {"kept": True}

    # A second wave replaces the tables in place instead of appending copies.
    again = subprocess.run(
        ["bash", str(BOOTSTRAP)],
        env=env,
        cwd=tmp_path,
        capture_output=True,
        text=True,
        check=False,
    )
    assert again.returncode == 0, again.stderr
    assert config.read_text(encoding="utf-8").count("[mcp_servers.") == 3

    # A config that does not parse is not appended to.
    config.write_text("[mcp_servers.github-ro]\n[mcp_servers.github-ro]\n", encoding="utf-8")
    broken = subprocess.run(
        ["bash", str(BOOTSTRAP)],
        env=env,
        cwd=tmp_path,
        capture_output=True,
        text=True,
        check=False,
    )
    assert broken.returncode == 0, broken.stderr
    assert "not valid TOML" in broken.stderr
    assert config.read_text(encoding="utf-8") == (
        "[mcp_servers.github-ro]\n[mcp_servers.github-ro]\n"
    )
