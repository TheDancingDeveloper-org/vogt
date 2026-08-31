"""Delivery contracts for the browser-only public demo.

The behavioral half lives in Vitest and Playwright.  These checks hold the
packaging and isolation boundary in the same Python suite that already guards
Vogt's other delivery artefacts, so a workflow or Compose edit cannot quietly
turn the static showcase into another engine deployment.
"""

from __future__ import annotations

import re
import shutil
import subprocess
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
WEB_SRC = REPO_ROOT / "web" / "src"
DEMO_SERVER = REPO_ROOT / "engine" / "deploy" / "demo-server.mjs"
MOBILE_DEMO = REPO_ROOT / "web" / "src" / "demo" / "mobile-showcase.html"
DEMO_OVERLAY = REPO_ROOT / "deploy" / "demo.overlay.yml"
MOBILE_DEMO_OVERLAY = REPO_ROOT / "deploy" / "mobile-demo.overlay.yml"
ENGINE_DOCKERFILE = REPO_ROOT / "engine" / "Dockerfile"
BUILD_WORKFLOW = REPO_ROOT / ".github" / "workflows" / "build.yml"


def _code(path: Path) -> str:
    """Return TypeScript with comments removed for token checks."""
    text = re.sub(r"/\*.*?\*/", "", path.read_text(encoding="utf-8"), flags=re.S)
    return "\n".join(line.split("//", 1)[0] for line in text.splitlines())


# The `core alone` CI job runs `rm -rf engine web mobile` before pytest to prove
# the core carries no hidden dependency on them (ci.yml). These delivery checks
# read those very trees, so they belong to the full-tree runs — the `test
# (py3.x)` jobs — and skip, rather than error, when the tree they inspect is
# absent. Same shape as the `docker`-conditional check below.
requires_engine = pytest.mark.skipif(
    not (DEMO_SERVER.exists() and ENGINE_DOCKERFILE.exists()),
    reason="engine tree absent (the `core alone` job deletes it)",
)
requires_web = pytest.mark.skipif(
    not WEB_SRC.exists(),
    reason="web tree absent (the `core alone` job deletes it)",
)


@requires_web
def test_every_browser_transport_enters_through_the_runtime_seam() -> None:
    """A new bare network primitive would bypass deterministic demo state."""
    offenders: list[str] = []
    for path in sorted(WEB_SRC.glob("*.ts")) + sorted(WEB_SRC.glob("*.tsx")):
        if path.name == "runtimeTransport.ts":
            continue
        text = _code(path)
        for primitive in (
            r"\bfetch\s*\(",
            r"\bnew\s+WebSocket\s*\(",
            r"\bEventSource\s*\(",
        ):
            if re.search(primitive, text):
                offenders.append(f"{path.name}: {primitive}")
    assert not offenders, f"browser I/O bypasses runtimeTransport: {offenders}"


@requires_engine
def test_demo_image_branches_from_the_normal_web_build() -> None:
    text = ENGINE_DOCKERFILE.read_text(encoding="utf-8")
    assert "FROM web-build AS demo-web" in text
    assert "pnpm demo:augment" in text
    demo = text.split("FROM web-build AS demo-web", 1)[1].split("# ─── Stage 2:", 1)[0]
    assert "FROM ${NODE_IMAGE} AS demo-runtime" in demo
    assert "chmod 0444 /app/demo-server.mjs" in demo
    # The demo server uses only node builtins, so the bundled npm CLI is removed
    # — its transitive deps are what the fatal Trivy gate flags (#454).
    assert "rm -rf /usr/local/lib/node_modules/npm" in demo
    assert "COPY --from=server-build" not in demo
    assert "COPY --from=core" not in demo
    assert "vogt-engine" not in demo
    assert "vogt-core" not in demo


@requires_engine
def test_demo_origin_has_no_process_proxy_or_write_implementation() -> None:
    text = DEMO_SERVER.read_text(encoding="utf-8")
    for forbidden in (
        "node:child_process",
        "node:net",
        "node:tls",
        "spawn(",
        "exec(",
        "WebSocket",
        "http-proxy",
        "createProxyServer",
    ):
        assert forbidden not in text
    assert 'pathname.startsWith("/api/")' in text
    assert 'req.method !== "GET" && req.method !== "HEAD"' in text
    assert "read-only static origin" in text
    assert '"mobile-demo.html"' in text
    assert 'process.env.DEMO_ROOT_DOCUMENT ?? "index.html"' in text
    assert 'new Set(["index.html", "mobile-demo.html"])' in text


@requires_web
def test_mobile_demo_embeds_the_real_demo_pwa() -> None:
    text = MOBILE_DEMO.read_text(encoding="utf-8")
    assert 'src="/index.html#/sessions"' in text
    assert 'href="/index.html#/assistant"' in text
    assert 'target="vogt-mobile"' in text
    assert "screenshot recreation" in text
    assert "Capacitor" in text
    assert "<script" not in text


def test_demo_compose_requires_a_digest_and_drops_runtime_privilege() -> None:
    text = DEMO_OVERLAY.read_text(encoding="utf-8")
    assert "VOGT_DEMO_IMAGE:?" in text
    assert "@sha256:" in text
    assert "${VOGT_DEMO_BIND_IP:-127.0.0.1}" in text
    assert "${VOGT_DEMO_PORT:-8912}" in text
    for required in ("read_only: true", "no-new-privileges:true", "cap_drop:", "- ALL"):
        assert required in text
    assert not re.search(r"^\s+volumes:\s*$", text, re.MULTILINE)
    assert not re.search(r"^\s+(environment|env_file|secrets):\s*$", text, re.MULTILINE)


def test_mobile_demo_overlay_changes_only_the_root_document() -> None:
    text = MOBILE_DEMO_OVERLAY.read_text(encoding="utf-8")
    executable = "\n".join(
        line for line in text.splitlines() if not line.lstrip().startswith("#")
    )
    assert executable.strip() == (
        "services:\n"
        "  vogt-demo:\n"
        "    environment:\n"
        "      DEMO_ROOT_DOCUMENT: mobile-demo.html"
    )
    for forbidden in ("image:", "ports:", "volumes:", "secrets:", "command:"):
        assert forbidden not in executable


def test_demo_publication_is_dev_only_scanned_signed_and_never_deploys() -> None:
    text = BUILD_WORKFLOW.read_text(encoding="utf-8")
    job = text.split("  demo-image:", 1)[1].split("\n  # The dev-pod", 1)[0]
    assert "if: github.ref == 'refs/heads/dev'" in job
    assert job.count("target: demo-runtime") == 2
    scan = job.split("      - name: scan the static demo image", 1)[1].split(
        "\n      - uses:", 1
    )[0]
    assert "ghcr.io/aquasecurity/trivy@sha256:" in scan
    assert re.search(r"trivy@sha256:[0-9a-f]{64}", scan)
    assert "--exit-code 1" in scan
    assert "continue-on-error" not in scan
    assert "sbom: true" in job
    assert 'cosign sign --yes "${DEMO_IMAGE}@${DIGEST}"' in job
    assert "Nothing has been deployed" in job
    assert "mobile-demo.html" in job
    assert "--env DEMO_ROOT_DOCUMENT=mobile-demo.html" in job
    assert "get index.html" in job
    lowered = job.lower()
    assert "ssh " not in lowered
    assert "docker compose up" not in lowered
    assert "deploystack" not in lowered


@pytest.mark.skipif(shutil.which("docker") is None, reason="docker not present")
def test_demo_compose_renders_as_a_static_read_only_service() -> None:
    result = subprocess.run(
        ["docker", "compose", "-f", str(DEMO_OVERLAY), "config"],
        capture_output=True,
        text=True,
        env={
            "PATH": "/usr/bin:/bin",
            "VOGT_DEMO_IMAGE": (
                "ghcr.io/thedancingdeveloper-org/vogt-demo@sha256:" + "a" * 64
            ),
        },
    )
    if result.returncode != 0:
        pytest.skip(f"docker compose unavailable: {result.stderr.strip()[:200]}")
    assert "read_only: true" in result.stdout
    assert "host_ip: 127.0.0.1" in result.stdout
    assert 'published: "8912"' in result.stdout
    assert "target: 8910" in result.stdout
    assert "cap_drop:" in result.stdout


@pytest.mark.skipif(shutil.which("docker") is None, reason="docker not present")
def test_mobile_demo_overlay_preserves_the_hardened_digest_pinned_service() -> None:
    result = subprocess.run(
        [
            "docker",
            "compose",
            "-f",
            str(DEMO_OVERLAY),
            "-f",
            str(MOBILE_DEMO_OVERLAY),
            "config",
        ],
        capture_output=True,
        text=True,
        env={
            "PATH": "/usr/bin:/bin",
            "VOGT_DEMO_IMAGE": (
                "ghcr.io/thedancingdeveloper-org/vogt-demo@sha256:" + "b" * 64
            ),
            "VOGT_DEMO_PORT": "8913",
        },
    )
    if result.returncode != 0:
        pytest.skip(f"docker compose unavailable: {result.stderr.strip()[:200]}")
    assert "DEMO_ROOT_DOCUMENT: mobile-demo.html" in result.stdout
    assert "read_only: true" in result.stdout
    assert 'published: "8913"' in result.stdout
    assert "cap_drop:" in result.stdout
