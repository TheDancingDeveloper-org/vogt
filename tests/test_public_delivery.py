"""Contracts for the generic open-source core delivery."""

from __future__ import annotations

import re
import shutil
import subprocess
import tomllib
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
PUBLIC_COMPOSE = REPO_ROOT / "deploy" / "vogt.compose.yml"
BUILD_OVERLAY = REPO_ROOT / "deploy" / "vogt.build.yml"
ENGINE_OVERLAY = REPO_ROOT / "deploy" / "engine.overlay.yml"
STACK_COMPOSE = REPO_ROOT / "deploy" / "stack.compose.yml"
# The two public files that stand a pod in front of a core. They differ in how
# the engine arrives — the overlay builds one, the AIO pulls one that already
# contains the core — and agree on everything a stranger is entitled to assume:
# loopback by default, named volumes, and not one estate name between them.
PUBLIC_DEPLOY_FILES = (ENGINE_OVERLAY, STACK_COMPOSE)
PUBLIC_ENV = REPO_ROOT / "deploy" / ".env.example"
DOCKERFILE = REPO_ROOT / "Dockerfile"


def test_public_delivery_defaults_to_the_current_product_release() -> None:
    """The first pull after publication must name an existing current release.

    Two images carry the version. The stack image is the product — the one
    `stack.compose.yml` and the operator docs name (DEPLOYMENT.md §1.1). The
    core image is the contributor stack's base and the stack image's build
    input, so the contributor files and the image-extension doc name it.
    """
    with (REPO_ROOT / "pyproject.toml").open("rb") as handle:
        version = tomllib.load(handle)["project"]["version"]
    stack = f"ghcr.io/thedancingdeveloper-org/vogt-stack:{version}"
    for path in (
        STACK_COMPOSE,
        REPO_ROOT / "docs" / "DEPLOYMENT.md",
    ):
        assert stack in path.read_text(encoding="utf-8"), (
            f"{path.relative_to(REPO_ROOT)} must name the current release {stack}"
        )
    # The bundled voice sidecar (#565) is versioned with the product: it is
    # published from the same release and named beside the stack image.
    voice = f"ghcr.io/thedancingdeveloper-org/vogt-voice:{version}"
    assert voice in STACK_COMPOSE.read_text(encoding="utf-8"), (
        f"{STACK_COMPOSE.relative_to(REPO_ROOT)} must name the current release {voice}"
    )
    core = f"ghcr.io/thedancingdeveloper-org/vogt:{version}"
    for path in (
        PUBLIC_COMPOSE,
        PUBLIC_ENV,
        ENGINE_OVERLAY,
        REPO_ROOT / "docs" / "CUSTOMISATION.md",
        REPO_ROOT / "docs" / "DEPLOYMENT.md",
    ):
        assert core in path.read_text(encoding="utf-8"), (
            f"{path.relative_to(REPO_ROOT)} must name the current release {core}"
        )


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


def test_public_compose_exposes_the_generic_lifecycle_contract() -> None:
    text = _without_comments(PUBLIC_COMPOSE.read_text(encoding="utf-8"))
    assert "/usr/local/bin/vogt-lifecycle" in text
    assert "VOGT_LIFECYCLE_STATE_DIR" in text
    assert "VOGT_LIFECYCLE_HEALTHCHECK_URL" in text
    assert "health" in text


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
    assert "!vogt-lifecycle.sh" in reincluded


# ── The generic engine overlay (#202) ───────────────────────────────────────
#
# `deploy/engine.overlay.yml` is the public, estate-neutral worked example of
# the two-service deployment: it adds the session engine in front of the core
# with no host paths, no tailnet, and no maintainer integrations. The
# maintainer's own estate overlay is not tracked here (#204) — it is held in
# the operator's private ops repository — so this is the only engine overlay
# the suite asserts. These mirror the base's own contracts — loopback by
# default, an overlay states differences only, and nothing private leaks into
# a file a stranger is meant to run.

# Estate leaks the public overlay must never carry. `sprooty` is deliberately
# absent from this list: `/home/sprooty` is the engine *image's* build-time
# home directory (its `USER`), a container path, not an estate host mount — the
# host-bind test below is what proves no estate *path* is bound in.
ESTATE_MARKERS = (
    "mydevenv2",
    "tailscale",
    "infisical",
    "komodo",
    "cadastre",
    "theclawbay",
    "indexarr",
    "winrarhost",
    "sprooty.com",
    "100.92",  # the estate tailnet
    "/mnt/",  # the estate's host volume root
)


def test_the_engine_overlay_builds_the_engine_and_fronts_the_core() -> None:
    """An overlay states differences only, and this one adds the engine.

    No engine image is published, so the overlay always builds one from
    `engine/Dockerfile`, lifting the published core in via `CORE_IMAGE`; it
    then proxies to the sibling core by service name rather than running one.
    """
    overlay = _without_comments(ENGINE_OVERLAY.read_text(encoding="utf-8"))
    assert "engine:" in overlay
    assert "dockerfile: engine/Dockerfile" in overlay
    assert "CORE_IMAGE:" in overlay
    assert "INSTALL_AI_CLIENTS:" in overlay
    assert "CODEX_VERSION:" in overlay
    assert 'VOGT_CORE_URL: "http://vogt:8000"' in overlay
    assert "VOGT_CORE_TOKEN_FILE:" in overlay
    # The engine's own token is the one required operator value (>=16 chars),
    # exactly as the base requires only VOGT_PUBLIC_URL.
    assert re.search(r"ENGINE_TOKEN:\s*\"\$\{ENGINE_TOKEN:\?", overlay)


@pytest.mark.parametrize("path", PUBLIC_DEPLOY_FILES, ids=lambda p: p.name)
def test_the_public_deploy_files_publish_to_loopback_unless_told_otherwise(
    path: Path,
) -> None:
    """The host interface is an exposure decision, so these files refuse one.

    Like the base, the engine's published port defaults to `127.0.0.1`; the
    engine's own in-container socket is 0.0.0.0 so the published port reaches
    it, which is the one place the two must differ.

    It matters more for the AIO than for the overlay, not less: that image is a
    development pod carrying `sudo`, `sshd` and the agent CLIs, so a default
    that put it on a network interface would be handing out a shell.
    """
    overlay = _without_comments(path.read_text(encoding="utf-8"))
    assert "${ENGINE_BIND:-127.0.0.1}:" in overlay
    assert 'ENGINE_BIND: "0.0.0.0:8910"' in overlay


def test_the_stack_compose_runs_the_published_aio_and_its_own_core() -> None:
    """The AIO is the other half of the parity model: one image, no build.

    Where the overlay builds an engine and proxies to a *sibling* core, this
    runs a published image that already contains both and points the engine at
    its own loopback. That single `VOGT_CORE_URL` is what starts the core —
    the entrypoint derives the core's listen address from the proxy target, so
    the pair cannot drift into a front door aimed at a port nothing serves.

    Loopback is also the containment: NFR-D11 says the engine is the only way
    in, and a non-loopback value here would mean the core is somewhere else
    entirely, not that it is exposed.
    """
    stack = _without_comments(STACK_COMPOSE.read_text(encoding="utf-8"))
    assert "build:" not in stack, (
        "the AIO is the published image; a build directive here would quietly "
        "make it something other than the digest a consumer pinned"
    )
    assert "${VOGT_STACK_IMAGE:-" in stack
    assert 'VOGT_CORE_URL: "http://127.0.0.1:8000"' in stack, (
        "the AIO runs its own core on loopback; any other value stops the "
        "entrypoint starting one (NFR-D11)"
    )
    assert 'VOGT_FRONTED: "true"' in stack
    assert "VOGT_CORE_TOKEN_FILE:" in stack
    assert "VOGT_BOOTSTRAP_CORE_TOKEN_FILE:" in stack
    # The engine's own token is the one required operator value (>=16 chars).
    assert re.search(r"ENGINE_TOKEN:\s*\"\$\{ENGINE_TOKEN:\?", stack)
    # One published port, and it is the engine's. Publishing 8000 would put the
    # core on the network beside the door that exists to be the only way in.
    published = re.findall(r"^      - \"\$\{ENGINE_BIND.*$", stack, re.MULTILINE)
    assert len(published) == 1, f"expected exactly one published port: {published}"
    assert ":8000" not in "".join(published)


def test_the_stack_compose_is_a_base_not_an_overlay() -> None:
    """Layering it onto `vogt.compose.yml` would run two cores.

    The AIO image contains a core and the base compose runs one, so the two
    files are alternatives. This is worth asserting because every other
    deployment file in `deploy/` *is* an overlay, which makes combining them
    the natural guess and a wrong one.

    The stack is the pod plus the bundled `voice` sidecar (#565) — two
    services, not one — but it is still a base: it declares its own image,
    volumes and secrets rather than stating a difference from another file.
    """
    stack = _without_comments(STACK_COMPOSE.read_text(encoding="utf-8"))
    # A base declares what it runs: image, volumes and secrets of its own.
    assert re.search(r"^volumes:\n", stack, re.MULTILINE)
    assert re.search(r"^secrets:\n", stack, re.MULTILINE)
    assert re.search(r"^services:\n", stack, re.MULTILINE)
    # Scoped to the `services:` block: the top-level `volumes:` keys sit at the
    # same indent, so an unscoped search finds the named volumes too.
    block = re.search(r"^services:\n(.*?)(?=^\S)", stack, re.MULTILINE | re.DOTALL)
    assert block, "no services block"
    services = re.findall(r"^  ([a-z][a-z0-9-]*):$", block.group(1), re.MULTILINE)
    assert services == ["vogt", "voice"], (
        "the AIO is the pod plus its bundled voice sidecar (#565); found "
        f"services {services}"
    )


@pytest.mark.parametrize("path", PUBLIC_DEPLOY_FILES, ids=lambda p: p.name)
def test_the_public_deploy_files_carry_no_estate_addresses_or_paths(
    path: Path,
) -> None:
    overlay = _without_comments(path.read_text(encoding="utf-8")).lower()
    for marker in ESTATE_MARKERS:
        assert marker.lower() not in overlay, f"estate marker leaked: {marker}"


@pytest.mark.parametrize("path", PUBLIC_DEPLOY_FILES, ids=lambda p: p.name)
def test_the_public_deploy_files_use_named_volumes_not_host_binds(
    path: Path,
) -> None:
    """A fresh named volume keeps the image's ownership; a host bind would
    arrive root-owned and break the pod, and would tie the file to one host.

    Every volume mount source must be a bare name (a declared named volume),
    never an absolute path, a relative path, a `~` home, or a `${VAR:-/path}`
    whose default is a path.
    """
    overlay = _without_comments(path.read_text(encoding="utf-8"))
    # Isolate each service's `volumes:` list; ports and secrets live under
    # their own keys and are not volume mounts.
    volume_blocks = re.findall(
        r"^    volumes:\n((?:      - .+\n)+)", overlay, re.MULTILINE
    )
    assert volume_blocks, "the overlay should mount a named volume for the pod"
    sources = []
    for block in volume_blocks:
        for line in block.splitlines():
            entry = line.strip().lstrip("- ").strip().strip("\"'")
            source = entry.split(":", 1)[0]
            sources.append(source)
    assert sources, "expected at least one volume mount"
    for source in sources:
        assert not source.startswith(("/", ".", "~", "$")), (
            f"host-path bind mount, not a named volume: {source!r}"
        )
    # The declared named volume must exist under the top-level `volumes:` key.
    assert re.search(r"^volumes:\n(?:  .+\n)*  engine-home:", overlay, re.MULTILINE)
    assert ":/run/vogt/hooks" not in overlay


@pytest.mark.skipif(shutil.which("docker") is None, reason="docker not present")
def test_docker_compose_renders_the_base_and_the_engine_overlay() -> None:
    """Compose itself agrees the two files merge into one valid deployment."""
    result = subprocess.run(
        [
            "docker",
            "compose",
            "-f",
            str(PUBLIC_COMPOSE),
            "-f",
            str(ENGINE_OVERLAY),
            "config",
        ],
        capture_output=True,
        text=True,
        env={
            "PATH": "/usr/bin:/bin",
            "VOGT_PUBLIC_URL": "http://localhost:8080",
            "ENGINE_TOKEN": "0123456789abcdef0123",
        },
    )
    if result.returncode != 0:
        pytest.skip(f"docker compose unavailable: {result.stderr.strip()[:200]}")
    assert "http://vogt:8000" in result.stdout
    assert "127.0.0.1" in result.stdout


# ── The bundled voice sidecar (#565) ─────────────────────────────────────────
#
# Voice used to be two Compose overlays keyed on a specific engine service name
# and pinned to moving third-party tags, absent from the one supported stack.
# It is now a first-party service in `stack.compose.yml`, published from the
# release like the stack image, and reached over the Compose network. These
# pin the properties a stranger is entitled to assume of it.

DEPLOY = REPO_ROOT / "deploy"


def test_the_stack_bundles_voice_wired_to_the_engine() -> None:
    stack = _without_comments(STACK_COMPOSE.read_text(encoding="utf-8"))
    # A service of its own, behind a profile the example env turns on, so it can
    # be switched off without editing this file.
    assert re.search(r"^  voice:\n", stack, re.MULTILINE), "no voice service"
    assert re.search(r"^    profiles:\s*\[\"voice\"\]", stack, re.MULTILINE), (
        "voice must sit behind a profile so COMPOSE_PROFILES can turn it off"
    )
    # A published image, never a build — same rule as the stack image itself.
    voice_block = re.search(
        r"^  voice:\n(.*?)(?=^  [a-z]|^volumes:|^secrets:|\Z)",
        stack,
        re.MULTILINE | re.DOTALL,
    )
    assert voice_block, "no voice service block"
    assert "build:" not in voice_block.group(1), (
        "the sidecar is the published image; a build here would decouple it "
        "from the digest a consumer pinned"
    )
    assert "${VOGT_VOICE_IMAGE:-" in voice_block.group(1)
    # The engine is pointed at it, and told the format the Piper backend serves.
    assert "http://voice:8000/v1" in stack
    assert re.search(
        r'ENGINE_ASSISTANT_TTS_FORMAT:\s*"\$\{ENGINE_ASSISTANT_TTS_FORMAT:-wav\}"',
        stack,
    ), "the bundled Piper backend only speaks wav; the engine must ask for wav"
    # The example env turns the profile on, so a documented `up` runs voice.
    env = (DEPLOY / "stack.env.example").read_text(encoding="utf-8")
    assert re.search(r"^COMPOSE_PROFILES=voice\b", env, re.MULTILINE), (
        "stack.env.example must enable the voice profile so voice is on by default"
    )


def test_the_voice_sidecar_is_internal_only() -> None:
    """Like the core, the sidecar is never published to the host.

    The engine is the only front door (NFR-D11); the sidecar is reached over
    the Compose network and must not open a port on the host.
    """
    stack = _without_comments(STACK_COMPOSE.read_text(encoding="utf-8"))
    match = re.search(
        r"^  voice:\n(.*?)(?=^  [a-z]|^volumes:|^secrets:|\Z)",
        stack,
        re.MULTILINE | re.DOTALL,
    )
    assert match, "no voice service block"
    assert "ports:" not in match.group(1), "the voice sidecar must not publish a port"


def test_no_deploy_file_names_a_mutable_voice_image_tag() -> None:
    """Acceptance (#565): no file under deploy/ names a mutable image tag for voice.

    The old third-party overlay pinned `speaches:latest-cpu` and
    `openedai-speech:latest` by moving tag — the only mutable pins in deploy/.
    Those files are gone; assert they cannot creep back, and that the bundled
    image is pinned to a release version (or a digest), never `latest`/`dev`.
    """
    mutable_markers = ("speaches", "openedai-speech", "latest-cpu")
    for path in sorted(DEPLOY.rglob("*.yml")) + sorted(DEPLOY.rglob("*.example")):
        text = path.read_text(encoding="utf-8")
        for marker in mutable_markers:
            assert marker not in text, (
                f"{path.relative_to(REPO_ROOT)} names the mutable third-party "
                f"voice image marker {marker!r}"
            )
    # The bundled image is a release version or a digest, not a moving alias.
    for path in (STACK_COMPOSE, DEPLOY / "stack.env.example"):
        text = path.read_text(encoding="utf-8")
        for ref in re.findall(r"vogt-voice[:@][^\s\"}]+", text):
            assert not ref.endswith((":latest", ":dev")), (
                f"{path.relative_to(REPO_ROOT)} pins voice to a moving tag: {ref}"
            )
            assert re.search(r"vogt-voice:\d+\.\d+\.\d+", ref) or "@sha256:" in ref, (
                f"{path.relative_to(REPO_ROOT)} must pin voice to a version or "
                f"digest, not {ref}"
            )
