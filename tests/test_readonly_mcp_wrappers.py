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
            "crw": {"type": "http", "url": "http://100.92.54.45:18190/mcp"},
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
    config.write_text("[other]\nkept = true\n", encoding="utf-8")
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
        subprocess.Popen(
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
    assert tomllib.loads(text)["features"]["daemon_auto_start"] is False

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
    duplicate = "[mcp_servers.github-ro]\n[mcp_servers.github-ro]\n"
    config.write_text(duplicate, encoding="utf-8")
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
    assert config.read_text(encoding="utf-8") == duplicate


def _codex_bootstrap(
    tmp_path: Path, extra: dict[str, str]
) -> subprocess.CompletedProcess[str]:
    bindir = tmp_path / "bin"
    bindir.mkdir(exist_ok=True)
    codex = bindir / "codex"
    if not codex.exists():
        codex.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
        codex.chmod(0o755)
    return subprocess.run(
        ["bash", str(BOOTSTRAP)],
        env={
            "PATH": f"{bindir}{os.pathsep}/usr/bin{os.pathsep}/bin",
            "HOME": str(tmp_path),
            "CODEX_HOME": str(tmp_path / "codex"),
            "VOGT_SRC": str(tmp_path / "absent"),
            **extra,
        },
        cwd=tmp_path,
        capture_output=True,
        text=True,
        check=False,
    )


@pytest.mark.skipif(
    shutil.which("python3") is None or shutil.which("flock") is None,
    reason="the Codex config rewrite needs python3 and flock",
)
def test_daemon_flag_merges_into_an_existing_features_table(tmp_path: Path) -> None:
    """WI-1142: a client that skips --no-daemon still must not start the
    app-server daemon. An existing [features] table used to hide the key."""
    import tomllib

    codex_home = tmp_path / "codex"
    config = codex_home / "config.toml"
    config.parent.mkdir()
    config.write_text(
        "[features]\napprovals = true\n\n[other]\nkept = true\n",
        encoding="utf-8",
    )
    proc = _codex_bootstrap(tmp_path, {})
    assert proc.returncode == 0, proc.stderr
    parsed = tomllib.loads(config.read_text(encoding="utf-8"))
    assert parsed["features"] == {
        "approvals": True,
        "daemon_auto_start": False,
        "memories": True,
    }
    assert parsed["other"] == {"kept": True}
    assert "mcp_servers" not in parsed

    # A key an operator set to true is forced off: the pin depends on it.
    config.write_text(
        "daemon_auto_start = true\n\n[features]\napprovals = true\n",
        encoding="utf-8",
    )
    again = _codex_bootstrap(tmp_path, {})
    assert again.returncode == 0, again.stderr
    parsed = tomllib.loads(config.read_text(encoding="utf-8"))
    assert parsed["features"] == {
        "approvals": True,
        "daemon_auto_start": False,
        "memories": True,
    }
    assert parsed["daemon_auto_start"] is True

    # Already off: the LiteLLM provider was written by the run above, and a
    # later bootstrap leaves the file byte-for-byte.
    settled = config.read_text(encoding="utf-8")
    assert "model_providers.litellm" in settled
    third = _codex_bootstrap(tmp_path, {})
    assert third.returncode == 0, third.stderr
    assert config.read_text(encoding="utf-8") == settled


@pytest.mark.skipif(
    shutil.which("python3") is None or shutil.which("flock") is None,
    reason="the Codex config rewrite needs python3 and flock",
)
def test_codex_litellm_provider_is_the_default_and_stores_no_secret(
    tmp_path: Path,
) -> None:
    """WI-1147: Codex sessions default to the estate router. The session key
    is named, never written, and an operator's other provider survives."""
    import tomllib

    codex_home = tmp_path / "codex"
    config = codex_home / "config.toml"

    proc = _codex_bootstrap(tmp_path, {})
    assert proc.returncode == 0, proc.stderr
    parsed = tomllib.loads(config.read_text(encoding="utf-8"))
    assert parsed["model"] == "grok"
    assert parsed["model_provider"] == "litellm"
    litellm = parsed["model_providers"]["litellm"]
    assert litellm["base_url"] == "http://100.92.54.45:4000/v1"
    assert litellm["wire_api"] == "chat"
    assert litellm["env_key"] == "HOMELAB_LITELLM_SESSION_KEY"
    # web_search is not a setting this Codex fork recognises; it is not written.
    assert "web_search" not in litellm
    assert "experimental_bearer_token" not in litellm
    # grok is not in Codex's bundled catalog, so bootstrap hands it one:
    # apply_patch freeform, and no tool_mode (code mode stays off).
    catalog_path = codex_home / "vogt-model-catalog.json"
    assert parsed["model_catalog_json"] == str(catalog_path)
    catalog = json.loads(catalog_path.read_text(encoding="utf-8"))
    # Groups a session-key completion answered (WI-1592). Not or-deepseek
    # (blocked), not bedrock (absent), not the DLab models that 400/401/503,
    # and not qwen3-coder / qwen3-8-27b / gpt-oss-120b, which fail inside
    # LiteLLM (408 / hang; re-adding them is WI-1631).
    # The fork sorts by priority ascending and defaults to the first visible one.
    by_priority = sorted(catalog["models"], key=lambda model: model["priority"])
    assert [model["slug"] for model in by_priority] == [
        "grok",
        "code",
        "small",
        "reasoning",
        "vision",
        "or-deepseek-flash",
        "or-gemini-flash",
        "or-glm-5-3",
        "dlab-deepseek-v4-pro",
        "dlab-deepseek-v4-1-flash",
        "dlab-grok-4-5",
        "dlab-grok-4-6",
        "dlab-grok-4-7",
        "dlab-gemini-3-8-flash",
        "dlab-glm-5-3",
        "dlab-glm-5-3-flash",
        "dlab-glm-5-3-flashx",
        "dlab-kimi-k3",
        "premium",
    ]
    assert by_priority[0]["slug"] == "grok"
    assert by_priority[0]["visibility"] == "list"
    assert by_priority[-1]["slug"] == "premium"
    assert by_priority[-1]["visibility"] == "hide"
    model = by_priority[0]
    assert all(
        row["apply_patch_tool_type"] == "freeform" and "tool_mode" not in row
        for row in catalog["models"]
    )
    # ModelsResponse rejects a missing availability_nux (no serde default) and a
    # model with neither base_instructions nor model_messages.instructions_template.
    assert model["availability_nux"] is None
    template = model["model_messages"]["instructions_template"]
    assert isinstance(template, str) and template.strip()
    clawbay = parsed["model_providers"]["theclawbay"]
    assert clawbay["base_url"] == "https://api.theclawbay.com/backend-api/codex"
    assert clawbay["wire_api"] == "responses"
    assert "experimental_bearer_token" not in clawbay
    assert "HOMELAB_LITELLM" not in config.read_text(encoding="utf-8").replace(
        'env_key = "HOMELAB_LITELLM_SESSION_KEY"', ""
    )

    # A theclawbay default, including the old pinned model id, moves to the router.
    config.write_text(
        'model = "grok-4.7"\nmodel_provider = "theclawbay"\n\n'
        "[model_providers.theclawbay]\n"
        'base_url = "https://api.theclawbay.com/backend-api/codex"\n'
        'experimental_bearer_token = "operator-token"\n',
        encoding="utf-8",
    )
    moved = _codex_bootstrap(tmp_path, {})
    assert moved.returncode == 0, moved.stderr
    text = config.read_text(encoding="utf-8")
    parsed = tomllib.loads(text)
    assert parsed["model"] == "grok"
    assert parsed["model_provider"] == "litellm"
    assert parsed["model_providers"]["theclawbay"]["experimental_bearer_token"] == (
        "operator-token"
    )
    assert text.count("[model_providers.theclawbay]") == 1

    # A model the router does not serve, left over from theclawbay, moves to
    # grok; a catalog group stays (WI-1640).
    config.write_text(
        'model = "gpt-6-astra"\nmodel_provider = "litellm"\n', encoding="utf-8"
    )
    stale_run = _codex_bootstrap(tmp_path, {})
    assert stale_run.returncode == 0, stale_run.stderr
    assert tomllib.loads(config.read_text(encoding="utf-8"))["model"] == "grok"
    config.write_text('model = "code"\nmodel_provider = "litellm"\n', encoding="utf-8")
    group_run = _codex_bootstrap(tmp_path, {})
    assert group_run.returncode == 0, group_run.stderr
    assert tomllib.loads(config.read_text(encoding="utf-8"))["model"] == "code"

    # Some other provider and model are the operator's choice.
    config.write_text(
        'model = "gpt-5.6"\nmodel_provider = "dlab"\n\n[other]\nkept = true\n',
        encoding="utf-8",
    )
    kept = _codex_bootstrap(tmp_path, {})
    assert kept.returncode == 0, kept.stderr
    parsed = tomllib.loads(config.read_text(encoding="utf-8"))
    assert parsed["model"] == "gpt-5.6"
    assert parsed["model_provider"] == "dlab"
    assert parsed["other"] == {"kept": True}
    assert "litellm" in parsed["model_providers"]
    # No catalog was configured, so grok's is written. One the operator
    # already chose is not replaced, and the file is not written for it.
    assert parsed["model_catalog_json"] == str(codex_home / "vogt-model-catalog.json")
    chosen = codex_home / "operator-catalog.json"
    config.write_text(
        f'model = "gpt-5.6"\nmodel_provider = "dlab"\n'
        f'model_catalog_json = "{chosen}"\n',
        encoding="utf-8",
    )
    (codex_home / "vogt-model-catalog.json").unlink()
    foreign = _codex_bootstrap(tmp_path, {})
    assert foreign.returncode == 0, foreign.stderr
    parsed = tomllib.loads(config.read_text(encoding="utf-8"))
    assert parsed["model_catalog_json"] == str(chosen)
    assert not (codex_home / "vogt-model-catalog.json").exists()

    # Invalid TOML is not rewritten.
    duplicate = "[model_providers.litellm]\n[model_providers.litellm]\n"
    config.write_text(duplicate, encoding="utf-8")
    broken = _codex_bootstrap(tmp_path, {})
    assert broken.returncode == 0, broken.stderr
    assert "not valid TOML" in broken.stderr
    assert config.read_text(encoding="utf-8") == duplicate


@pytest.mark.skipif(
    shutil.which("python3") is None or shutil.which("flock") is None,
    reason="the Codex config rewrite needs python3 and flock",
)
def test_codex_gets_the_browser_and_the_isolated_subagent(tmp_path: Path) -> None:
    """WI-1358: a Codex session has no browser of its own and its spawn_agent
    cannot take a working directory. The bootstrap registers the two stdio
    servers the klaudia binary serves, and points the sub-agent at the real
    codex binary rather than the full-access wrapper on PATH."""
    import tomllib

    bindir = tmp_path / "bin"
    bindir.mkdir()
    for name in ("codex", "klaudia"):
        path = bindir / name
        path.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
        path.chmod(0o755)
    proc = _codex_bootstrap(tmp_path, {})
    assert proc.returncode == 0, proc.stderr
    assert proc.stdout == "", "stdout is the MCP transport"

    config = tmp_path / "codex" / "config.toml"
    servers = _codex_tables(config.read_text(encoding="utf-8"))
    assert servers["browser"]["command"] == str(bindir / "klaudia")
    assert servers["browser"]["args"] == ["mcp", "browser"]
    assert servers["browser"]["tool_timeout_sec"] == 60
    sub = servers["codex-subagent"]
    assert sub["command"] == str(bindir / "klaudia")
    assert sub["args"] == [
        "mcp",
        "codex-subagent",
        "--codex",
        "/usr/local/libexec/codex-real",
        "--root",
        str(tmp_path),
    ]
    # The registration is a command and arguments, never a credential.
    assert "env" not in sub and "bearer_token" not in sub
    # The child codex exec needs the provider key, passed by name (WI-1639).
    assert sub["env_vars"] == ["HOMELAB_LITELLM_SESSION_KEY"]

    # A second run replaces the tables in place.
    again = _codex_bootstrap(tmp_path, {})
    assert again.returncode == 0, again.stderr
    settled = tomllib.loads(config.read_text(encoding="utf-8"))["mcp_servers"]
    assert settled["browser"] == servers["browser"]
    assert config.read_text(encoding="utf-8").count("[mcp_servers.browser]") == 1

    # No klaudia binary, no tables: nothing is written that cannot be run.
    bindir.joinpath("klaudia").unlink()
    config.write_text("[other]\nkept = true\n", encoding="utf-8")
    absent = _codex_bootstrap(tmp_path, {})
    assert absent.returncode == 0, absent.stderr
    parsed = tomllib.loads(config.read_text(encoding="utf-8"))
    assert "mcp_servers" not in parsed
    assert parsed["other"] == {"kept": True}


@pytest.mark.skipif(
    shutil.which("python3") is None or shutil.which("flock") is None,
    reason="the Codex config rewrite needs python3 and flock",
)
def test_driven_session_bundle_merges_and_raises_the_schema_cap(
    tmp_path: Path,
) -> None:
    """WI-1149: a driven Codex reads CLAUDE.md, titles itself by agent state,
    remembers, and accepts the vogt and cadastre tool schemas."""
    import tomllib

    workspace = tmp_path / "workspace"
    (workspace / ".claude" / "skills").mkdir(parents=True)
    (tmp_path / ".claude" / "skills").mkdir(parents=True)

    config = tmp_path / "codex" / "config.toml"
    config.parent.mkdir()
    config.write_text(
        'model = "grok"\n\n'
        "[features]\ndaemon_auto_start = false\n\n"
        '[tui]\nstatus_line = ["model"]\n\n'
        '[mcp_servers.vogt]\ncommand = "/usr/local/bin/vogt-mcp"\n'
        "tool_input_schema_max_bytes = 1000\n\n"
        '[mcp_servers.cadastre]\ncommand = "/usr/local/bin/cadastre-mcp"\n\n'
        '[mcp_servers.other]\ncommand = "/bin/other"\n',
        encoding="utf-8",
    )
    proc = subprocess.run(
        ["bash", str(BOOTSTRAP)],
        env={
            "PATH": f"/usr/bin{os.pathsep}/bin",
            "HOME": str(tmp_path),
            "CODEX_HOME": str(tmp_path / "codex"),
            "VOGT_SRC": str(tmp_path / "absent"),
        },
        cwd=workspace,
        capture_output=True,
        text=True,
        check=False,
    )
    # No `codex` on PATH: the bundle is for a client that is present.
    assert "driven-session" not in proc.stderr

    bindir = tmp_path / "bin"
    bindir.mkdir()
    shim = bindir / "codex"
    shim.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    shim.chmod(0o755)
    proc = subprocess.run(
        ["bash", str(BOOTSTRAP)],
        env={
            "PATH": f"{bindir}{os.pathsep}/usr/bin{os.pathsep}/bin",
            "HOME": str(tmp_path),
            "CODEX_HOME": str(tmp_path / "codex"),
            "VOGT_SRC": str(tmp_path / "absent"),
        },
        cwd=workspace,
        capture_output=True,
        text=True,
        check=False,
    )
    assert proc.returncode == 0, proc.stderr
    assert proc.stdout == ""
    parsed = tomllib.loads(config.read_text(encoding="utf-8"))
    assert parsed["project_doc_fallback_filenames"] == ["CLAUDE.md"]
    assert parsed["features"]["memories"] is True
    assert parsed["features"]["daemon_auto_start"] is False
    assert parsed["tui"]["animations"] is False
    assert parsed["tui"]["terminal_title"] == ["agent-state"]
    assert parsed["tui"]["status_line"] == ["model"]
    assert parsed["mcp_servers"]["vogt"]["tool_input_schema_max_bytes"] == 200_000
    assert parsed["mcp_servers"]["cadastre"]["tool_input_schema_max_bytes"] == 200_000
    assert "tool_input_schema_max_bytes" not in parsed["mcp_servers"]["other"]
    link = tmp_path / ".agents" / "skills"
    assert link.is_symlink()
    assert os.path.realpath(link) == os.path.realpath(workspace / ".claude" / "skills")

    # A cap an operator set higher is not lowered, and a skills path that
    # already exists is not replaced.
    config.write_text(
        "[mcp_servers.vogt]\ntool_input_schema_max_bytes = 900000\n\n"
        '[mcp_servers.cadastre]\ncommand = "/usr/local/bin/cadastre-mcp"\n',
        encoding="utf-8",
    )
    link.unlink()
    link.mkdir()
    again = subprocess.run(
        ["bash", str(BOOTSTRAP)],
        env={
            "PATH": f"{bindir}{os.pathsep}/usr/bin{os.pathsep}/bin",
            "HOME": str(tmp_path),
            "CODEX_HOME": str(tmp_path / "codex"),
            "VOGT_SRC": str(tmp_path / "absent"),
        },
        cwd=tmp_path,
        capture_output=True,
        text=True,
        check=False,
    )
    assert again.returncode == 0, again.stderr
    parsed = tomllib.loads(config.read_text(encoding="utf-8"))
    assert parsed["mcp_servers"]["vogt"]["tool_input_schema_max_bytes"] == 900_000
    # A kept cap is re-appended; it still has to stand off from the next header.
    rewritten = config.read_text(encoding="utf-8")
    assert "tool_input_schema_max_bytes = 900000\n\n[mcp_servers.cadastre]" in rewritten
    assert link.is_dir() and not link.is_symlink()
    # No workspace skills here, so the link would have been the home one.
    assert not (tmp_path / ".agents" / "skills-unused").exists()


@pytest.mark.skipif(
    shutil.which("python3") is None or shutil.which("flock") is None,
    reason="the Codex config rewrite needs python3 and flock",
)
def test_driven_session_pre_approves_only_the_trusted_servers(tmp_path: Path) -> None:
    """WI-1634: a driven Codex runs `-a on-request`, so an MCP call stops at a
    prompt only a person may answer. The estate's trusted servers are
    pre-approved; komodo and anything unnamed keep the gate, an explicit mode
    is kept, and a second run changes nothing."""
    import tomllib

    trusted = (
        "vogt",
        "cadastre",
        "codex-subagent",
        "browser",
        "github-ro",
        "forgejo-ro",
        "grafana-ro",
    )
    config = tmp_path / "codex" / "config.toml"
    config.parent.mkdir()
    tables = "".join(
        f'[mcp_servers.{name}]\ncommand = "/bin/{name}"\n\n' for name in trusted
    )
    config.write_text(
        tables
        + '[mcp_servers.komodo]\ncommand = "/bin/komodo"\n\n'
        + '[mcp_servers.other]\ncommand = "/bin/other"\n\n'
        + '[mcp_servers.vogt.tools.board_list]\napproval_mode = "approve"\n',
        encoding="utf-8",
    )
    proc = _codex_bootstrap(tmp_path, {})
    assert proc.returncode == 0, proc.stderr
    first = config.read_text(encoding="utf-8")
    servers = tomllib.loads(first)["mcp_servers"]
    for name in trusted:
        assert servers[name]["default_tools_approval_mode"] == "approve", name
    assert "default_tools_approval_mode" not in servers["komodo"]
    assert "default_tools_approval_mode" not in servers["other"]
    assert servers["vogt"]["tool_input_schema_max_bytes"] == 200_000
    assert servers["vogt"]["tools"]["board_list"] == {"approval_mode": "approve"}

    again = _codex_bootstrap(tmp_path, {})
    assert again.returncode == 0, again.stderr
    assert config.read_text(encoding="utf-8") == first

    # An operator's explicit mode is theirs; an unregistered server is not made.
    config.write_text(
        '[mcp_servers.vogt]\ncommand = "/bin/vogt"\n'
        'default_tools_approval_mode = "prompt"\n',
        encoding="utf-8",
    )
    kept = _codex_bootstrap(tmp_path, {})
    assert kept.returncode == 0, kept.stderr
    servers = tomllib.loads(config.read_text(encoding="utf-8"))["mcp_servers"]
    assert servers["vogt"]["default_tools_approval_mode"] == "prompt"
    assert set(servers) == {"vogt"}


@pytest.mark.skipif(
    shutil.which("python3") is None or shutil.which("flock") is None,
    reason="the Codex config rewrite needs python3 and flock",
)
def test_owned_codex_tables_carry_the_approval_and_settle(tmp_path: Path) -> None:
    """WI-1634: the read-only, browser and sub-agent writers replace their own
    tables whole, so they write the approval row themselves; otherwise every
    start would drop it and the bundle would add it back."""
    import tomllib

    bindir = tmp_path / "bin"
    bindir.mkdir()
    for name in ("codex", "klaudia", "github-mcp-server"):
        path = bindir / name
        path.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
        path.chmod(0o755)
    wrapper = tmp_path / "readonly-mcp"
    wrapper.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    wrapper.chmod(0o755)
    extra = {"GITHUB_MCP_TOKEN": "x", "VOGT_READONLY_MCP_WRAPPER": str(wrapper)}
    proc = _codex_bootstrap(tmp_path, extra)
    assert proc.returncode == 0, proc.stderr
    config = tmp_path / "codex" / "config.toml"
    first = config.read_text(encoding="utf-8")
    servers = tomllib.loads(first)["mcp_servers"]
    for name in ("browser", "codex-subagent", "github-ro"):
        assert servers[name]["default_tools_approval_mode"] == "approve", name
    again = _codex_bootstrap(tmp_path, extra)
    assert again.returncode == 0, again.stderr
    assert config.read_text(encoding="utf-8") == first


def _shims(tmp_path: Path, names: tuple[str, ...]) -> Path:
    bindir = tmp_path / "bin"
    bindir.mkdir(exist_ok=True)
    for name in names:
        path = bindir / name
        path.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
        path.chmod(0o755)
    return bindir


@pytest.mark.skipif(
    shutil.which("python3") is None or shutil.which("flock") is None,
    reason="the Codex config rewrite needs python3 and flock",
)
@pytest.mark.parametrize("server", ("github-ro", "forgejo-ro", "grafana-ro"))
def test_readonly_writer_keeps_an_explicit_approval_mode(
    tmp_path: Path, server: str
) -> None:
    """WI-1634: the read-only writer replaces its table whole, so it must read an
    operator's mode first. Otherwise a "prompt" becomes "approve" on the next
    start, and only the bundle path would keep it."""
    import tomllib

    binaries = {
        "github-ro": "github-mcp-server",
        "forgejo-ro": "gitea-mcp",
        "grafana-ro": "mcp-grafana",
    }
    _shims(tmp_path, ("codex", binaries[server]))
    wrapper = tmp_path / "readonly-mcp"
    wrapper.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    wrapper.chmod(0o755)
    config = tmp_path / "codex" / "config.toml"
    config.parent.mkdir()
    config.write_text(
        f"[mcp_servers.{server}]\n"
        f'command = "{wrapper}"\n'
        'default_tools_approval_mode = "prompt"\n'
        f'\n[mcp_servers.{server}.tools.read]\napproval_mode = "prompt"\n',
        encoding="utf-8",
    )
    env = {
        "github-ro": {"GITHUB_MCP_TOKEN": "x"},
        "forgejo-ro": {"GITEA_HOST": "http://git", "GITEA_MCP_TOKEN": "x"},
        "grafana-ro": {
            "GRAFANA_URL": "http://grafana",
            "GRAFANA_SERVICE_ACCOUNT_TOKEN": "x",
        },
    }
    proc = _codex_bootstrap(
        tmp_path, {"VOGT_READONLY_MCP_WRAPPER": str(wrapper), **env[server]}
    )
    assert proc.returncode == 0, proc.stderr
    servers = tomllib.loads(config.read_text(encoding="utf-8"))["mcp_servers"]
    assert servers[server]["default_tools_approval_mode"] == "prompt"
    assert servers[server]["command"] == str(wrapper)


@pytest.mark.skipif(
    shutil.which("python3") is None or shutil.which("flock") is None,
    reason="the Codex config rewrite needs python3 and flock",
)
@pytest.mark.parametrize("server", ("browser", "codex-subagent"))
def test_klaudia_tools_writer_keeps_an_explicit_approval_mode(
    tmp_path: Path, server: str
) -> None:
    """WI-1634: the browser and sub-agent writer likewise replaces its tables, so
    an operator's mode on either one has to survive the rewrite."""
    import tomllib

    _shims(tmp_path, ("codex", "klaudia"))
    config = tmp_path / "codex" / "config.toml"
    config.parent.mkdir()
    config.write_text(
        f"[mcp_servers.{server}]\n"
        'command = "klaudia"\n'
        'default_tools_approval_mode = "prompt"\n',
        encoding="utf-8",
    )
    proc = _codex_bootstrap(tmp_path, {})
    assert proc.returncode == 0, proc.stderr
    servers = tomllib.loads(config.read_text(encoding="utf-8"))["mcp_servers"]
    assert servers[server]["default_tools_approval_mode"] == "prompt"
    assert servers["browser"]["command"].endswith("/klaudia")
    again = _codex_bootstrap(tmp_path, {})
    assert again.returncode == 0, again.stderr


@pytest.mark.skipif(
    shutil.which("python3") is None or shutil.which("flock") is None,
    reason="the Codex config rewrite needs python3 and flock",
)
def test_auto_compact_limit_written_when_unset_and_never_overridden(
    tmp_path: Path,
) -> None:
    """WI-1575: Vogt owns Codex's auto-compact limit, but an operator's
    value wins, and a second run changes nothing."""
    import tomllib

    config = tmp_path / "codex" / "config.toml"
    config.parent.mkdir()
    config.write_text('model = "grok"\n\n[other]\nkept = true\n', encoding="utf-8")
    proc = _codex_bootstrap(tmp_path, {})
    assert proc.returncode == 0, proc.stderr
    first = config.read_text(encoding="utf-8")
    parsed = tomllib.loads(first)
    assert parsed["model_auto_compact_token_limit"] == 200000
    assert parsed["model_auto_compact_token_limit_scope"] == "total"
    assert parsed["other"] == {"kept": True}
    again = _codex_bootstrap(tmp_path, {})
    assert again.returncode == 0, again.stderr
    assert config.read_text(encoding="utf-8") == first

    config.write_text("model_auto_compact_token_limit = 150000\n", encoding="utf-8")
    proc = _codex_bootstrap(tmp_path, {"ENGINE_CODEX_AUTO_COMPACT_TOKENS": "99"})
    assert proc.returncode == 0, proc.stderr
    parsed = tomllib.loads(config.read_text(encoding="utf-8"))
    assert parsed["model_auto_compact_token_limit"] == 150000
    assert parsed["model_auto_compact_token_limit_scope"] == "total"


@pytest.mark.skipif(
    shutil.which("python3") is None or shutil.which("flock") is None,
    reason="the Codex config rewrite needs python3 and flock",
)
def test_codex_catalog_lists_the_groups_that_answer_and_keeps_operator_entries(
    tmp_path: Path,
) -> None:
    """WI-2740: the catalog is what the session key reaches, swack-haiku with
    Claude's window, and an entry an operator added survives a rewrite."""
    import threading
    import tomllib
    from http.server import BaseHTTPRequestHandler, HTTPServer

    class Router(BaseHTTPRequestHandler):
        def log_message(self, *args: object) -> None:
            pass

        def _send(self, status: int, body: object) -> None:
            data = json.dumps(body).encode()
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        def do_GET(self) -> None:
            rows = [
                {
                    "model_name": "swack-haiku",
                    "model_info": {"max_input_tokens": 1000000},
                },
                {"model_name": "gpt-5.4", "model_info": {"max_input_tokens": 1050000}},
                {"model_name": "broken", "model_info": {}},
            ]
            self._send(200, {"data": rows})

        def do_POST(self) -> None:
            length = int(self.headers.get("Content-Length", "0"))
            model = json.loads(self.rfile.read(length))["model"]
            self._send(500 if model == "broken" else 200, {"choices": []})

    server = HTTPServer(("127.0.0.1", 0), Router)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    codex_home = tmp_path / "codex"
    codex_home.mkdir()
    catalog_path = codex_home / "vogt-model-catalog.json"
    operator = {"slug": "my-model", "description": "added by hand", "priority": 99}
    catalog_path.write_text(json.dumps({"models": [operator]}), encoding="utf-8")
    env = {
        "HOMELAB_LITELLM_SESSION_KEY": "test-key",
        "VOGT_LITELLM_BASE_URL": f"http://127.0.0.1:{server.server_address[1]}/v1",
    }
    try:
        proc = _codex_bootstrap(tmp_path, env)
    finally:
        server.shutdown()
    assert proc.returncode == 0, proc.stderr
    catalog = json.loads(catalog_path.read_text(encoding="utf-8"))
    slugs = [row["slug"] for row in catalog["models"]]
    assert slugs == ["gpt-5.4", "swack-haiku", "my-model"]
    windows = {row["slug"]: row.get("context_window") for row in catalog["models"]}
    assert windows["swack-haiku"] == 200000
    assert windows["gpt-5.4"] == 1050000
    assert catalog["models"][-1] == operator
    parsed = tomllib.loads((codex_home / "config.toml").read_text(encoding="utf-8"))
    assert parsed["model"] == "gpt-5.4", "the default is a group that answers"
    assert "test-key" not in (codex_home / "config.toml").read_text(encoding="utf-8")

    # The answers are cached: a second run with the router gone is the same.
    again = _codex_bootstrap(tmp_path, env)
    assert again.returncode == 0, again.stderr
    assert json.loads(catalog_path.read_text(encoding="utf-8")) == catalog


@pytest.mark.skipif(shutil.which("jq") is None, reason="the fast path reads with jq")
@pytest.mark.parametrize("url", ["http://crw.test:1/mcp", ""])
def test_bootstrap_registers_crw_as_one_http_server(tmp_path: Path, url: str) -> None:
    """WI-2742: every Claude session gets crw as a single HTTP entry, and an
    empty VOGT_CRW_MCP_URL registers nothing."""
    bindir = tmp_path / "bin"
    bindir.mkdir()
    calls = tmp_path / "claude-calls"
    claude = bindir / "claude"
    claude.write_text(f'#!/bin/sh\necho "$*" >> {calls}\nexit 0\n', encoding="utf-8")
    claude.chmod(0o755)
    (tmp_path / ".claude.json").write_text('{"mcpServers": {}}', encoding="utf-8")
    proc = subprocess.run(
        ["bash", str(BOOTSTRAP)],
        env={
            "PATH": f"{bindir}{os.pathsep}/usr/bin{os.pathsep}/bin",
            "HOME": str(tmp_path),
            "VOGT_SRC": str(tmp_path / "absent"),
            "VOGT_CRW_MCP_URL": url,
        },
        cwd=tmp_path,
        capture_output=True,
        text=True,
        check=False,
    )
    assert proc.returncode == 0, proc.stderr
    lines = calls.read_text(encoding="utf-8").splitlines() if calls.exists() else []
    adds = [line for line in lines if line.startswith("mcp add") and " crw " in line]
    if url:
        assert adds == [f"mcp add --scope user --transport http crw {url}"]
    else:
        assert adds == []
