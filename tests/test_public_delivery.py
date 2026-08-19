"""Contracts for the generic open-source core delivery."""

from __future__ import annotations

import re
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
PUBLIC_COMPOSE = REPO_ROOT / "deploy" / "vogt.compose.yml"
PUBLIC_ENV = REPO_ROOT / "deploy" / ".env.example"
DOCKERFILE = REPO_ROOT / "Dockerfile"


def _without_comments(text: str) -> str:
    return "\n".join(
        line for line in text.splitlines() if not line.lstrip().startswith("#")
    )


def test_public_compose_is_a_single_self_contained_core_service() -> None:
    raw = PUBLIC_COMPOSE.read_text(encoding="utf-8")
    assert re.search(r"^services:\n  vogt:\n", raw, re.MULTILINE)
    assert "dockerfile: Dockerfile" in raw
    assert "volumes:" in raw
    assert "vogt-data:/var/lib/vogt" in raw
    assert "vogt-workspace:/workspace" in raw
    executable_config = _without_comments(raw).lower()
    assert "cadastre" not in executable_config
    assert "mydevenv2" not in executable_config
    assert "theclawbay" not in executable_config
    assert "tailscale" not in executable_config
    assert "infisical" not in executable_config


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
