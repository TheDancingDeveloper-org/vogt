"""`engine/deploy/cargo-cache.sh`: Cargo's caches survive a redeploy (WI-1256).

The pod's CARGO_HOME is in the image layer, so the entrypoint moves its
`registry/` and `git/` onto the home volume and links them back. These tests
drive the real script against temporary directories standing in for the
image's CARGO_HOME and the persisted home.
"""

from __future__ import annotations

import os
import shutil
import subprocess
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
SCRIPT = REPO_ROOT / "engine" / "deploy" / "cargo-cache.sh"

pytestmark = pytest.mark.skipif(
    not SCRIPT.is_file() or shutil.which("bash") is None,
    reason="needs engine/deploy (absent in the core-alone job) and bash",
)


def _run(cargo_home: Path, home: Path) -> subprocess.CompletedProcess[str]:
    env = {
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "CARGO_HOME": str(cargo_home),
        "HOME": str(home),
    }
    return subprocess.run(
        ["bash", str(SCRIPT)], env=env, capture_output=True, text=True, check=True
    )


def _image_cargo_home(root: Path) -> Path:
    cargo = root / "opt" / "cargo"
    (cargo / "bin").mkdir(parents=True)
    (cargo / "bin" / "cargo").write_text("binary")
    (cargo / "registry" / "cache").mkdir(parents=True)
    (cargo / "registry" / "cache" / "baked.crate").write_text("from the image")
    (cargo / "git" / "db").mkdir(parents=True)
    return cargo


def test_first_boot_seeds_the_store_and_links_back(tmp_path: Path) -> None:
    cargo = _image_cargo_home(tmp_path)
    home = tmp_path / "home"
    home.mkdir()

    _run(cargo, home)

    store = home / ".cache" / "vogt-cargo"
    for sub in ("registry", "git"):
        assert (cargo / sub).is_symlink()
        assert (cargo / sub).resolve() == (store / sub).resolve()
    baked = store / "registry" / "cache" / "baked.crate"
    assert baked.read_text() == "from the image"
    assert (cargo / "bin" / "cargo").read_text() == "binary"


def test_a_redeploy_keeps_what_was_downloaded(tmp_path: Path) -> None:
    home = tmp_path / "home"
    home.mkdir()
    first = _image_cargo_home(tmp_path / "first")
    _run(first, home)
    (first / "registry" / "cache" / "voicepipe.crate").write_text("fetched later")

    # A redeploy is a fresh image layer: CARGO_HOME holds the baked copy again.
    second = _image_cargo_home(tmp_path / "second")
    _run(second, home)

    assert (second / "registry").is_symlink()
    cache = second / "registry" / "cache"
    assert (cache / "voicepipe.crate").read_text() == "fetched later"


def test_a_restart_of_the_same_container_is_a_no_op(tmp_path: Path) -> None:
    cargo = _image_cargo_home(tmp_path)
    home = tmp_path / "home"
    home.mkdir()
    _run(cargo, home)
    target = (cargo / "registry").readlink()

    _run(cargo, home)

    assert (cargo / "registry").readlink() == target


def test_a_cargo_home_under_home_is_left_alone(tmp_path: Path) -> None:
    home = tmp_path / "home"
    cargo = _image_cargo_home(home)

    result = _run(cargo, home)

    assert not (cargo / "registry").is_symlink()
    assert not (home / ".cache" / "vogt-cargo").exists()
    assert result.stdout == ""


def test_an_interrupted_first_copy_is_redone(tmp_path: Path) -> None:
    cargo = _image_cargo_home(tmp_path)
    home = tmp_path / "home"
    partial = home / ".cache" / "vogt-cargo" / "registry.partial"
    (partial / "half").mkdir(parents=True)

    _run(cargo, home)

    registry = home / ".cache" / "vogt-cargo" / "registry"
    assert (registry / "cache" / "baked.crate").is_file()
    assert not (registry / "registry").exists()
    assert not partial.exists()
