"""Claude Code account profiles: `claude-profile` and the bootstrap it runs.

A profile is a `CLAUDE_CONFIG_DIR` with its own login. Two things have to hold
for a second account to be usable in a session that was set up for the first:

- `mcp-bootstrap.sh` must decide "already registered" from *that profile's*
  config, not `~/.claude.json`. It used to read `$HOME/.claude.json` for its
  fast path, so the main account's registration made it skip the profile.
- `claude-profile prepare` must leave a profile that has the shared settings
  linked in, its own login left alone, and the MCP servers registered.

The `claude` CLI is a shim that records `mcp add`, so the tests are offline.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
DEPLOY = REPO_ROOT / "engine" / "deploy"
BOOTSTRAP = DEPLOY / "mcp-bootstrap.sh"
PROFILE = DEPLOY / "claude-profile.sh"

pytestmark = pytest.mark.skipif(
    not (BOOTSTRAP.is_file() and PROFILE.is_file())
    or shutil.which("bash") is None
    or shutil.which("jq") is None,
    reason="needs engine/deploy (absent in the core-alone job), bash and jq",
)

WRAPPER = "/usr/local/bin/vogt-mcp"

# `mcp get` fails (nothing registered, as far as the CLI can tell); `mcp add`
# writes the registration into the config directory the CLI was given, the way
# the real one does, and logs the call.
CLAUDE_SHIM = """#!/usr/bin/env python3
import json, os, sys
args = sys.argv[1:]
if args[:2] == ["mcp", "get"]:
    sys.exit(1)
if args[:2] == ["mcp", "add"]:
    name = args[args.index("--scope") + 2]
    config = os.path.join(
        os.environ.get("CLAUDE_CONFIG_DIR") or os.environ["HOME"], ".claude.json"
    )
    try:
        data = json.load(open(config))
    except FileNotFoundError:
        data = {}
    data.setdefault("mcpServers", {})[name] = {
        "command": args[args.index("--") + 1]
    }
    json.dump(data, open(config, "w"))
    with open(os.environ["SHIM_LOG"], "a") as log:
        log.write(json.dumps({"config": config, "name": name}) + "\\n")
sys.exit(0)
"""


@pytest.fixture
def sandbox(tmp_path: Path) -> dict[str, str]:
    home = tmp_path / "home"
    home.mkdir()
    bindir = tmp_path / "bin"
    bindir.mkdir()
    claude = bindir / "claude"
    claude.write_text(CLAUDE_SHIM, encoding="utf-8")
    claude.chmod(0o755)
    # The bootstrap as the image installs it: executable, on PATH, under its
    # installed name, which is what `claude-profile` looks for.
    installed = bindir / "vogt-mcp-bootstrap"
    shutil.copy(BOOTSTRAP, installed)
    installed.chmod(0o755)
    # The main account already has Vogt registered, which is the situation a
    # second account is set up in.
    (home / ".claude.json").write_text(
        json.dumps({"mcpServers": {"vogt": {"command": WRAPPER}}}), encoding="utf-8"
    )
    return {
        "PATH": f"{bindir}{os.pathsep}/usr/bin{os.pathsep}/bin",
        "HOME": str(home),
        "SHIM_LOG": str(tmp_path / "shim.log"),
        "VOGT_SRC": str(tmp_path / "no-vogt-checkout"),
        "CLAUDE_ACCOUNTS_ROOT": str(home / ".claude-accounts"),
        "CLAUDE_PROFILE_SHARE_FROM": str(home / ".claude"),
    }


def run(
    script: Path, args: list[str], env: dict[str, str]
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["bash", str(script), *args],
        env=env,
        capture_output=True,
        text=True,
        check=False,
    )


def registrations(env: dict[str, str]) -> list[dict[str, str]]:
    log = Path(env["SHIM_LOG"])
    if not log.exists():
        return []
    return [json.loads(line) for line in log.read_text().splitlines()]


def test_bootstrap_registers_vogt_in_the_profile_not_just_the_main_config(
    sandbox: dict[str, str], tmp_path: Path
) -> None:
    profile = tmp_path / "acct2"
    profile.mkdir()
    proc = run(BOOTSTRAP, [], {**sandbox, "CLAUDE_CONFIG_DIR": str(profile)})
    assert proc.returncode == 0, proc.stderr
    assert registrations(sandbox) == [
        {"config": str(profile / ".claude.json"), "name": "vogt"}
    ]
    # The main account's config is left exactly as it was.
    main = json.loads((Path(sandbox["HOME"]) / ".claude.json").read_text())
    assert main == {"mcpServers": {"vogt": {"command": WRAPPER}}}


def test_bootstrap_still_skips_when_the_profile_already_runs_the_wrapper(
    sandbox: dict[str, str], tmp_path: Path
) -> None:
    profile = tmp_path / "acct2"
    profile.mkdir()
    (profile / ".claude.json").write_text(
        json.dumps({"mcpServers": {"vogt": {"command": WRAPPER}}}), encoding="utf-8"
    )
    proc = run(BOOTSTRAP, [], {**sandbox, "CLAUDE_CONFIG_DIR": str(profile)})
    assert proc.returncode == 0, proc.stderr
    assert registrations(sandbox) == []


def test_bootstrap_without_a_profile_still_uses_the_home_config(
    sandbox: dict[str, str],
) -> None:
    proc = run(BOOTSTRAP, [], sandbox)
    assert proc.returncode == 0, proc.stderr
    # Already registered in ~/.claude.json: nothing to do.
    assert registrations(sandbox) == []


def test_prepare_builds_a_profile_with_shared_settings_and_its_own_login(
    sandbox: dict[str, str],
) -> None:
    home = Path(sandbox["HOME"])
    share = home / ".claude"
    (share / "skills").mkdir(parents=True)
    (share / "settings.json").write_text("{}", encoding="utf-8")
    # The main account's login and history must never reach a profile.
    (share / ".credentials.json").write_text("secret", encoding="utf-8")
    (share / "projects").mkdir()

    proc = run(PROFILE, ["prepare", "acct2"], sandbox)
    assert proc.returncode == 0, proc.stderr
    profile = home / ".claude-accounts" / "acct2"

    assert (profile / "settings.json").is_symlink()
    assert (profile / "skills").is_symlink()
    assert not (profile / ".credentials.json").exists()
    assert not (profile / "projects").exists()
    assert oct(profile.stat().st_mode & 0o777) == "0o700"
    assert "login: none yet" in proc.stdout
    assert "ENGINE_AGENT_CLAUDE_ACCOUNTS" in proc.stdout

    config = json.loads((profile / ".claude.json").read_text())
    assert config["autoModeEnvSetup"] == {"dismissed": True}
    assert config["mcpServers"]["vogt"] == {"command": WRAPPER}


def test_prepare_is_idempotent_and_keeps_a_profiles_own_settings(
    sandbox: dict[str, str],
) -> None:
    home = Path(sandbox["HOME"])
    (home / ".claude").mkdir()
    (home / ".claude" / "settings.json").write_text("{}", encoding="utf-8")
    assert run(PROFILE, ["prepare", "acct2"], sandbox).returncode == 0
    profile = home / ".claude-accounts" / "acct2"
    # The profile diverges on purpose and logs in.
    (profile / "settings.json").unlink()
    (profile / "settings.json").write_text('{"own": true}', encoding="utf-8")
    (profile / ".credentials.json").write_text("login", encoding="utf-8")

    proc = run(PROFILE, ["prepare", "acct2"], sandbox)
    assert proc.returncode == 0, proc.stderr
    assert (profile / "settings.json").read_text() == '{"own": true}'
    assert not (profile / "settings.json").is_symlink()
    assert (profile / ".credentials.json").read_text() == "login"
    assert "login: present" in proc.stdout
    # Registered the first time, found there the second.
    assert [r["name"] for r in registrations(sandbox)] == ["vogt"]


@pytest.mark.parametrize("bad", ["", "Acct", "../x", "a b", "-x", "x" * 33])
def test_prepare_refuses_a_bad_profile_name(sandbox: dict[str, str], bad: str) -> None:
    proc = run(PROFILE, ["prepare", bad], sandbox)
    assert proc.returncode != 0
    assert not (Path(sandbox["HOME"]) / ".claude-accounts").exists()


def test_path_env_and_list(sandbox: dict[str, str]) -> None:
    root = Path(sandbox["HOME"]) / ".claude-accounts"
    assert run(PROFILE, ["path", "acct2"], sandbox).stdout.strip() == str(
        root / "acct2"
    )
    assert run(PROFILE, ["env", "acct2"], sandbox).stdout.strip() == (
        f"export CLAUDE_CONFIG_DIR={root / 'acct2'}"
    )
    assert run(PROFILE, ["prepare", "acct2", "--no-mcp"], sandbox).returncode == 0
    assert run(PROFILE, ["list"], sandbox).stdout.split("\t") == [
        "acct2",
        str(root / "acct2"),
        "no-login\n",
    ]
    assert registrations(sandbox) == []
