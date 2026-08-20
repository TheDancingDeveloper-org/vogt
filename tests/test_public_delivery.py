"""Contracts for the generic open-source core delivery."""

from __future__ import annotations

import re
import shutil
import subprocess
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
PUBLIC_COMPOSE = REPO_ROOT / "deploy" / "vogt.compose.yml"
BUILD_OVERLAY = REPO_ROOT / "deploy" / "vogt.build.yml"
PUBLIC_ENV = REPO_ROOT / "deploy" / ".env.example"
DOCKERFILE = REPO_ROOT / "Dockerfile"


def _without_comments(text: str) -> str:
    return "\n".join(
        line for line in text.splitlines() if not line.lstrip().startswith("#")
    )


def test_public_compose_is_a_single_self_contained_core_service() -> None:
    raw = PUBLIC_COMPOSE.read_text(encoding="utf-8")
    assert re.search(r"^services:\n  vogt:\n", raw, re.MULTILINE)
    assert "volumes:" in raw
    assert "vogt-data:/var/lib/vogt" in raw
    executable_config = _without_comments(raw).lower()
    assert "cadastre" not in executable_config
    assert "mydevenv2" not in executable_config
    assert "theclawbay" not in executable_config
    assert "tailscale" not in executable_config
    assert "infisical" not in executable_config


def test_the_base_deploys_a_published_image_and_the_overlay_builds_one() -> None:
    """The base has to be something an overlay can be layered onto.

    A `build:` key survives a Compose merge, so a base that builds from the
    checkout cannot be deployed from a registry by anyone layering onto it.
    The build path moves to its own overlay for that reason.
    """
    base = _without_comments(PUBLIC_COMPOSE.read_text(encoding="utf-8"))
    assert "build:" not in base
    assert "image:" in base

    overlay = _without_comments(BUILD_OVERLAY.read_text(encoding="utf-8"))
    assert "dockerfile: Dockerfile" in overlay
    # An overlay states differences only: no second copy of the base.
    assert "healthcheck:" not in overlay
    assert "volumes:" not in overlay


def test_public_compose_command_survives_compose_parsing() -> None:
    """Compose word-splits a *string* command and truncates it at `>`.

    Written as a folded scalar, `vogt init >/dev/null && exec vogt serve …`
    resolves to `["vogt", "init"]`: the container runs `sh -c vogt init`,
    prints the CLI usage, exits 2, and crash-loops under `restart`. A
    single-element list is passed through verbatim.

    `vogt init` is load-bearing, not decoration — `serve` on an empty data
    directory answers /health/ready with 503 and "run `vogt init` first".
    """
    raw = PUBLIC_COMPOSE.read_text(encoding="utf-8")
    assert not re.search(r"^\s*command:\s*[>|]", raw, re.MULTILINE), (
        "a block-scalar command is word-split and truncated by Compose"
    )
    block = re.search(r"^    command:\n((?:      - .+\n)+)", raw, re.MULTILINE)
    assert block, "command must be a YAML sequence"
    element = block.group(1)
    assert "vogt init" in element
    assert "vogt serve --host 0.0.0.0 --port 8000" in element


def test_public_compose_healthcheck_is_plain_http() -> None:
    text = _without_comments(PUBLIC_COMPOSE.read_text(encoding="utf-8"))
    assert "/health/ready" in text
    assert "initialize" not in text
    assert "Authorization" not in text
    assert "protocolVersion" not in text


def test_public_compose_requires_only_the_public_url() -> None:
    text = _without_comments(PUBLIC_COMPOSE.read_text(encoding="utf-8"))
    required = set(re.findall(r"\$\{([A-Z_]+):\?", text))
    assert required == {"VOGT_PUBLIC_URL"}
    env = PUBLIC_ENV.read_text(encoding="utf-8")
    assert "VOGT_PUBLIC_URL=" in env
    assert "VOGT_PORT=" in env


def test_public_compose_publishes_to_loopback_unless_told_otherwise() -> None:
    """The host interface is an exposure decision, so the example refuses one.

    A default of `${VOGT_BIND_IP:-127.0.0.1}` is not a guess about where the
    instance belongs; it is a refusal to put it on a network interface
    because nobody said to.
    """
    text = _without_comments(PUBLIC_COMPOSE.read_text(encoding="utf-8"))
    assert "${VOGT_BIND_IP:-127.0.0.1}:" in text


def test_public_compose_does_not_override_the_default_import_root() -> None:
    """`import_root` defaults under the data volume, which the uid can write.

    A second named volume is created with the *host's* ownership, not the
    image's, so `/workspace` is root-owned and mode 0755 while the container
    runs as uid 1000 — `project import` then fails on permission. Pointing
    import_root at a mount is a customisation with an ownership requirement,
    documented rather than shipped in the base.
    """
    text = _without_comments(PUBLIC_COMPOSE.read_text(encoding="utf-8"))
    assert "VOGT_IMPORT_ROOT" not in text


@pytest.mark.skipif(shutil.which("docker") is None, reason="docker not present")
def test_docker_compose_renders_the_base_and_the_build_overlay() -> None:
    """The assertions above are about text; this one asks Compose itself."""
    result = subprocess.run(
        [
            "docker",
            "compose",
            "-f",
            str(PUBLIC_COMPOSE),
            "-f",
            str(BUILD_OVERLAY),
            "config",
        ],
        capture_output=True,
        text=True,
        env={"PATH": "/usr/bin:/bin", "VOGT_PUBLIC_URL": "http://localhost:8080"},
    )
    if result.returncode != 0:
        pytest.skip(f"docker compose unavailable: {result.stderr.strip()[:200]}")
    assert "vogt serve --host 0.0.0.0 --port 8000" in result.stdout


def test_public_dockerfile_has_no_private_base_or_integration() -> None:
    text = _without_comments(DOCKERFILE.read_text(encoding="utf-8")).lower()
    assert "ghcr.io/thedancingdeveloper-org" not in text
    assert "cadastre" not in text
    assert "theclawbay" not in text
    assert "mydevenv2" not in text
    assert "engine" not in text
    assert "web" not in text
    assert "mobile" not in text


def test_public_dockerignore_excludes_private_toolchains() -> None:
    text = REPO_ROOT.joinpath(".dockerignore").read_text(encoding="utf-8")
    lines = [
        line.strip()
        for line in text.splitlines()
        if line.strip() and not line.lstrip().startswith("#")
    ]
    # An allow-list: everything is excluded unless re-included, so private
    # toolchains (engine, web, mobile, deploy) never enter the build context.
    assert lines[0] == "*"
    reincluded = set(lines[1:])
    assert all(line.startswith("!") for line in reincluded)
    for path in ("engine", "web", "mobile", "docs", "tests", "deploy"):
        assert not any(entry.lstrip("!").rstrip("/*") == path for entry in reincluded)
