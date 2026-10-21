"""`engine/deploy/lsp-guard.sh` and `lsp-gc.sh`: language-server hardening (WI-889).

The guard runs project code (build.rs, proc macros, Gradle import) only for a
trusted root and never passes engine or vault credentials to a server; the GC
drops jdtls data dirs whose root is gone. These drive the real scripts.
"""

from __future__ import annotations

import os
import shutil
import subprocess
from pathlib import Path

import pytest

DEPLOY = Path(__file__).resolve().parents[1] / "engine" / "deploy"
GUARD = DEPLOY / "lsp-guard.sh"
GC = DEPLOY / "lsp-gc.sh"

pytestmark = pytest.mark.skipif(
    not GUARD.is_file() or shutil.which("bash") is None,
    reason="needs engine/deploy (absent in the core-alone job) and bash",
)


def _env(home: Path, **extra: str) -> dict[str, str]:
    return {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "HOME": str(home), **extra}


def _guard(
    args: list[str], env: dict[str, str], check: bool = True
) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["bash", str(GUARD), *args],
        env=env,
        capture_output=True,
        text=True,
        check=check,
    )


def test_untrusted_root_is_static(tmp_path: Path) -> None:
    out = _guard(["--check", str(tmp_path)], _env(tmp_path), check=False)
    assert out.returncode == 3
    assert out.stdout.strip() == "static"


def test_root_inside_trusted_roots_is_trusted(tmp_path: Path) -> None:
    root = tmp_path / "proj" / "crate"
    env = _env(tmp_path, VOGT_LSP_TRUSTED_ROOTS=f"/nowhere:{tmp_path / 'proj'}")
    out = _guard(["--check", str(root)], env, check=False)
    assert (out.returncode, out.stdout.strip()) == (0, "trusted")


def test_sibling_prefix_is_not_trusted(tmp_path: Path) -> None:
    env = _env(tmp_path, VOGT_LSP_TRUSTED_ROOTS=str(tmp_path / "proj"))
    out = _guard(["--check", str(tmp_path / "proj-evil")], env, check=False)
    assert out.returncode == 3


def test_opt_in_env_trusts(tmp_path: Path) -> None:
    out = _guard(["--check", str(tmp_path)], _env(tmp_path, VOGT_LSP_TRUST="opt-in"))
    assert out.stdout.strip() == "trusted"


def test_static_run_strips_secrets_and_disables_code_execution(tmp_path: Path) -> None:
    env = _env(
        tmp_path,
        INFISICAL_CLIENT_SECRET="s",
        INFISICAL_CLIENT_ID="i",
        ENGINE_TOKEN="t",
        AGENT_AUTH_MODE="identity",
        GH_TOKEN="g",
        OPENAI_API_KEY="k",
        KEEP_ME="yes",
    )
    out = _guard(["--root", str(tmp_path), "--", "env"], env)
    seen = dict(line.split("=", 1) for line in out.stdout.splitlines() if "=" in line)
    for name in (
        "INFISICAL_CLIENT_SECRET",
        "INFISICAL_CLIENT_ID",
        "ENGINE_TOKEN",
        "AGENT_AUTH_MODE",
        "GH_TOKEN",
        "OPENAI_API_KEY",
    ):
        assert name not in seen
    assert seen["KEEP_ME"] == "yes"
    assert seen["VOGT_LSP_MODE"] == "static"
    conf = Path(seen["XDG_CONFIG_HOME"]) / "rust-analyzer" / "rust-analyzer.toml"
    text = conf.read_text()
    assert "checkOnSave = false" in text
    assert "[cargo.buildScripts]\nenable = false" in text
    assert "[procMacro]\nenable = false" in text


def test_trusted_run_keeps_user_config_but_still_strips_secrets(tmp_path: Path) -> None:
    env = _env(tmp_path, VOGT_LSP_TRUST="trusted", INFISICAL_CLIENT_SECRET="s")
    out = _guard(["--root", str(tmp_path), "--", "env"], env)
    assert "VOGT_LSP_MODE=trusted" in out.stdout
    assert "XDG_CONFIG_HOME" not in out.stdout
    assert "INFISICAL" not in out.stdout


def test_command_runs_in_root(tmp_path: Path) -> None:
    out = _guard(["--root", str(tmp_path), "--", "pwd"], _env(tmp_path))
    assert out.stdout.strip() == str(tmp_path.resolve())


def test_gc_removes_only_dirs_whose_root_is_gone(tmp_path: Path) -> None:
    store = tmp_path / "jdtls"
    live_root = tmp_path / "live"
    live_root.mkdir()
    for name, root in (("live", live_root), ("dead", tmp_path / "gone")):
        (store / name).mkdir(parents=True)
        (store / name / ".vogt-root").write_text(f"{root}\n")
    (store / "unknown").mkdir()

    dry = subprocess.run(
        ["bash", str(GC), "--dry-run", str(store)],
        env=_env(tmp_path),
        capture_output=True,
        text=True,
        check=True,
    )
    assert "dead" in dry.stdout
    assert (store / "dead").is_dir()

    subprocess.run(["bash", str(GC), str(store)], env=_env(tmp_path), check=True)
    assert sorted(p.name for p in store.iterdir()) == ["live", "unknown"]


def test_gc_without_store_is_a_no_op(tmp_path: Path) -> None:
    subprocess.run(["bash", str(GC)], env=_env(tmp_path), check=True)
