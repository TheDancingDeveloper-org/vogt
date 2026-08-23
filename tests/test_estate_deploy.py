"""Estate-only deployment assertions, self-skipping on a public clone.

These assertions are true of *the maintainer's own estate* — its Node B
Tailscale address, its workspace path, its local build-cache registry, the
core-only stack this product retired — and they are meaningful, so they are
kept rather than deleted. They live here, apart from the public-facing suite,
for two reasons:

  * a public contributor runs `uv run pytest` on a clean clone, and the estate
    deploy files (`deploy/personal-vogt.compose.yml`, `deploy/vogt-stack.*`)
    are relocated out of the repository by #204 — so a test that reads one, or
    that hard-codes an estate value, must *skip* on that clone rather than
    fail; and
  * the public suite must not assert the maintainer's estate. Moving the
    estate literals here leaves `tests/test_deploy.py` and `tests/test_core.py`
    estate-free.

Every test skips on the absence of the resource it needs — the estate deploy
file, or (for the retired-stack check) the engine's deploy scripts — so on a
clean clone this whole module reports as skipped, and on the maintainer's
checkout it runs and stays green. The `estate` marker (registered in
`pyproject.toml`) also lets `uv run pytest -m "not estate"` drop them wholesale.
"""

from __future__ import annotations

import re
from pathlib import Path

import pytest

from vogt.core.ids import slugify

pytestmark = pytest.mark.estate

REPO_ROOT = Path(__file__).resolve().parents[1]
WORKFLOWS = REPO_ROOT / ".github" / "workflows"

#: The Node B stack (NFR-D7–D10), kept in the repo for review beside the code
#: it deploys — until #204 relocates it, after which these tests skip.
COMPOSE = REPO_ROOT / "deploy" / "personal-vogt.compose.yml"
#: The merged stack and its env template (NFR-D11, NFR-D12, NFR-C6).
STACK_COMPOSE = REPO_ROOT / "deploy" / "vogt-stack.compose.yml"
STACK_ENV = REPO_ROOT / "deploy" / "vogt-stack.env.example"

ENGINE_PORT = "8910"
CORE_PORT = "8911"

#: The engine's deploy scripts, which a core-only checkout does not carry.
MCP_BOOTSTRAP = REPO_ROOT / "engine" / "deploy" / "mcp-bootstrap.sh"
VOGT_MCP_WRAPPER = REPO_ROOT / "engine" / "deploy" / "vogt-mcp-auth.sh"

#: The estate's local-registry on Node B (#129). `dind-daemon.json` on the
#: workers trusts it over HTTP; it is local to the runners, unlike GHCR.
LOCAL_REGISTRY_CACHE = "192.168.1.75:5500/vogt-buildcache"
#: The core-only stack this product replaces, retired by `DEPLOYMENT.md` §9.5.
RETIRED_CORE_STACK = "winrarhost.tailc7d3c.ts.net:18094"


def _without_comments(text: str) -> str:
    """Strip comments before asserting on content (see `test_deploy.py`)."""
    return "\n".join(
        line for line in text.splitlines() if not line.lstrip().startswith("#")
    )


def _needs(*paths: Path) -> pytest.MarkDecorator:
    """Skip when any named estate deploy file is absent (a public clone / #204)."""
    missing = [p.name for p in paths if not p.is_file()]
    return pytest.mark.skipif(
        bool(missing),
        reason=(
            f"estate deploy file(s) absent ({', '.join(missing) or '—'}); "
            "relocated out of the repository by #204, so the public suite skips "
            "this estate assertion"
        ),
    )


#: The engine tree is deleted in NFR-Q6's core-alone job, and absent from any
#: core-only checkout; the retired-stack check reads the engine's deploy scripts.
needs_engine = pytest.mark.skipif(
    not MCP_BOOTSTRAP.is_file(),
    reason=(
        "the merged tree carries the engine's deploy scripts; "
        "a core-only checkout does not"
    ),
)


@pytest.fixture(scope="module")
def compose() -> str:
    if not COMPOSE.is_file():
        pytest.skip(
            f"estate deploy file {COMPOSE.name} is absent (relocated by #204); "
            "the public suite skips this estate assertion"
        )
    return _without_comments(COMPOSE.read_text(encoding="utf-8"))


@pytest.fixture(scope="module")
def stack() -> str:
    if not STACK_COMPOSE.is_file():
        pytest.skip(
            f"estate deploy file {STACK_COMPOSE.name} is absent (relocated by "
            "#204); the public suite skips this estate assertion"
        )
    return _without_comments(STACK_COMPOSE.read_text(encoding="utf-8"))


# ── The Node B stack: deploy/personal-vogt.compose.yml (NFR-D2, D8, D9) ────


def test_allocation_values_carry_defaults(compose: str) -> None:
    """NFR-D2 revised: gating these produces broken deploys, not safety."""
    for variable in (
        "VOGT_PORT",
        "VOGT_BIND_IP",
        "VOGT_TLS_DIR",
        "VOGT_AUTH_DIR",
        "VOGT_WORKSPACE_DIR",
    ):
        assert f"${{{variable}:-" in compose, (
            f"{variable} must carry a concrete default; `${{{variable}:?}}` is "
            "what cost cadastre every deploy after #42"
        )
    assert ":?}" not in compose, "no required-value gates on allocation values"


def test_the_published_port_binds_the_tailscale_address(compose: str) -> None:
    """NFR-D8: never 0.0.0.0 on the host side."""
    published = re.findall(r'^\s+- "\$\{VOGT_BIND_IP[^"]+"', compose, re.MULTILINE)
    assert published, "the compose file publishes a port"
    for entry in published:
        assert "100.92.54.45" in entry, "defaults to Node B's Tailscale address"
        assert not entry.startswith('      - "0.0.0.0'), "never the wildcard"


def test_state_is_a_named_volume_and_never_a_relative_bind(compose: str) -> None:
    """Komodo clones stack directories to a fresh path on every deploy."""
    assert "vogt-komodo-data:/var/lib/vogt" in compose
    assert " - ./" not in compose, (
        "a relative bind mount silently points at a new empty directory"
    )


def test_the_container_is_hardened(compose: str) -> None:
    """NFR-D9."""
    for required in (
        'user: "${VOGT_UID:-1000}:0"',
        "read_only: true",
        "no-new-privileges:true",
        "cap_drop",
        "tmpfs:",
    ):
        assert required in compose, required


def test_the_image_is_digest_pinned(compose: str) -> None:
    """NFR-PO4: a tag is a lookup convenience; a digest is reproducible."""
    assert "@sha256:" in compose
    assert not re.search(r"image:\s+\S+:latest", compose), "never alias-tracking"


def test_the_healthcheck_is_plain_http(compose: str) -> None:
    """DEPLOYMENT §4.4: nothing outside a real MCP client pins a version."""
    assert "/health/ready" in compose
    assert "initialize" not in compose
    assert "protocolVersion" not in compose
    assert "Authorization" not in compose, "a probe needs no credential"


def test_the_compose_owns_the_uid(compose: str) -> None:
    """Overridable at deploy time, with a working default (NFR-D2 revised).

    Defaulted rather than `:?`-gated: gating allocation values is what broke
    every cadastre deploy after #42. A uid that is wrong shows up the first
    time a collector reads nothing; a uid that is unset stops the deploy.
    """
    user = re.search(r'user: "\$\{(\w+):-(\d+)\}:(\d+)"', compose)
    assert user, "the compose must set `user:` from an overridable variable"
    assert user.group(3) == "0", "gid stays 0; only the uid varies by host"


def test_the_workspace_is_writable(compose: str) -> None:
    """`project create` scaffolds a skeleton on disk (FR-G11).

    Collectors only read, so `:ro` looks right — until you notice it removes
    exactly one operation, as a runtime permission error rather than a clear
    refusal. Splitting capability by topology is what this product is built
    not to do: a mount that makes `project create` work from a laptop and
    fail on the server is the parity rule broken by infrastructure.

    Write access is bounded by the mechanisms meant for it — the
    `project.write` scope, double-gated writes, `serve --read-only`, and the
    audit log — not by the filesystem.
    """
    mounts = [
        line.strip() for line in compose.splitlines() if "VOGT_WORKSPACE_DIR" in line
    ]
    assert mounts, "the estate has to be mounted or no source collector sees it"
    for mount in mounts:
        assert not mount.endswith(':ro"'), (
            f"the estate mount is read-only, which breaks FR-G11: {mount}"
        )


def test_the_workspace_path_matches_where_projects_are_registered(
    compose: str,
) -> None:
    """A project's `root_path` is stored absolute and read verbatim (FR-P5).

    If this container sees the tree anywhere other than the path a project was
    registered under, every source collector finds nothing — and "nothing
    found" renders as an empty view, not as "could not look". Mounting at the
    identical path is what keeps that from being silent.
    """
    assert ":/home/sprooty/Working" in compose, (
        "the estate must appear at the same absolute path it was registered "
        "under, or collectors silently report nothing"
    )


# ── The merged stack: deploy/vogt-stack.compose.yml (NFR-D11, D12) ─────────


def test_the_merged_stack_pins_a_published_digest(stack: str) -> None:
    """NFR-PO4, and the placeholder that outlived its TODO.

    A digest of zeros is not a pin — it is a deploy that fails at pull time,
    which is the good case, and a line that reads as pinned to anyone
    skimming, which is the bad one.
    """
    pins = re.findall(r"image:\s+(\S+)", stack)
    assert pins, "the merged stack names an image"
    for pin in pins:
        assert "@sha256:" in pin, f"digest-pinned, never alias-tracking: {pin}"
        digest = pin.split("@sha256:")[1]
        assert re.fullmatch(r"[0-9a-f]{64}", digest), pin
        assert set(digest) != {"0"}, (
            "this is the placeholder, not a pin: the merged pipeline has "
            "published an image, so this line has a real digest to carry"
        )
    assert "vogt-stack@" in stack, (
        "the merged stack runs the merged image, not the core-only one — "
        "two registries so that pinning the wrong artefact is visible"
    )


def test_the_merged_stack_publishes_the_engine_and_only_the_engine(
    stack: str,
) -> None:
    """NFR-D11: the engine is the front door on the only published port.

    vogt-core is reachable from inside the container and from nowhere else.
    A second published port would not break anything visibly — it would just
    quietly restore the two-front-doors shape the merge exists to end.
    """
    block = re.search(r"^\s+ports:\n((?:\s+- .*\n)+)", stack, re.MULTILINE)
    assert block, "the merged stack publishes a port"
    published = re.findall(r'- "([^"]+)"', block.group(1))
    assert len(published) == 1, f"exactly one published port, got {published}"
    assert published[0].endswith(f":{ENGINE_PORT}"), (
        f"the published port maps to the engine ({ENGINE_PORT}): {published[0]}"
    )
    assert f":{CORE_PORT}:" not in stack, "vogt-core is never published"
    assert f'VOGT_CORE_URL: "http://127.0.0.1:{CORE_PORT}"' in stack, (
        "the front door reaches the core over loopback"
    )


def test_the_merged_stack_takes_its_core_token_from_a_file(stack: str) -> None:
    """FR-S9: a token in the environment is a token in `docker inspect`."""
    assert "VOGT_CORE_TOKEN_FILE:" in stack
    assert not re.search(r"^\s+VOGT_CORE_TOKEN:", stack, re.MULTILINE), (
        "the paired core token is brokered as a file, never as a value"
    )


def test_the_merged_stack_points_the_core_at_the_engine(stack: str) -> None:
    """#157: without VOGT_ENGINE_URL a work item cannot open its own session.

    The merged container runs both halves, so the core reaches the engine over
    loopback on the engine's port. Left unset it is the honest FR-E9 absence —
    which was never meant to apply to the one deployment that runs both halves,
    and is exactly how the work-item session path came to have never run.
    """
    assert f'VOGT_ENGINE_URL: "http://127.0.0.1:{ENGINE_PORT}"' in stack, (
        "the core reaches the engine over loopback to open a work item's session"
    )
    assert "VOGT_ENGINE_TOKEN_FILE:" in stack
    assert not re.search(r"^\s+VOGT_ENGINE_TOKEN:", stack, re.MULTILINE), (
        "the engine token is brokered as a file, never a value (FR-S7)"
    )


def test_two_instances_of_the_merged_stack_can_coexist(stack: str) -> None:
    """NFR-D12 asks for a dev stack, which is by definition the second one.

    Found by deploying: `container_name: vogt` collided with the core-only
    stack's container, Docker refused, and Komodo then reported the new stack
    *running* because it had found a container with that name belonging to
    somebody else. A dashboard saying running about a stack that never
    started is the failure this repository spends its time on, arriving from
    a direction no test had covered.

    Every name a second instance would collide on has to be overridable: the
    container, the volume, and the port and paths §9.3's template already
    sets.
    """
    for setting in ("container_name", "hostname"):
        assert f"{setting}: ${{VOGT_CONTAINER_NAME:-vogt}}" in stack, (
            f"{setting} is fixed; a second instance on the same host cannot "
            "start, and the first symptom is a dashboard reporting it running"
        )
    assert "name: ${VOGT_CORE_VOLUME:-vogt-core-data}" in stack, (
        "the core's volume is named explicitly to escape the compose project "
        "prefix, so it must be overridable or two instances share a database"
    )


# ── The merged stack env template: deploy/vogt-stack.env.example ───────────


@_needs(STACK_COMPOSE, STACK_ENV)
def test_the_env_template_covers_every_value_the_stack_requires() -> None:
    """A required value missing from the template is a failed deploy.

    `${X:?}` in the compose file means the stack refuses to start without it,
    and the template is the only place an operator is told it exists —
    Komodo's environment field is a text box. A value added to the compose and
    not to the template is discovered at deploy time, which is the moment
    `DEPLOYMENT.md` §4.1 records as having cost cadastre every deploy after
    #42.
    """
    compose = STACK_COMPOSE.read_text(encoding="utf-8")
    template = STACK_ENV.read_text(encoding="utf-8")
    required = set(re.findall(r"\$\{([A-Z_]+):\?", compose))
    assert required, "the compose file gates some values as required"
    missing = sorted(name for name in required if name not in template)
    assert not missing, (
        f"{missing} are required by the compose file and named nowhere in the "
        "env template; the stack would refuse to start and the operator would "
        "have had no way to know"
    )


@_needs(STACK_ENV)
def test_the_template_does_not_leave_a_dev_stack_on_production_paths() -> None:
    """The compose defaults are production's, and this template is a dev one.

    `vogt-stack.compose.yml` defaults the port to the one `personal/vogt` is
    serving on and the three bind mounts to the volumes the running MyDevEnv2
    stack owns. Those defaults are right for the file they are in — §4.1 says
    an allocation value carries a default so a deploy cannot fail on an unset
    variable — and wrong for a second instance of the same product, which
    would not stand beside production but on top of it.

    So the template sets them explicitly, and this fails if it stops.
    """
    # Comments stripped, as everywhere else in this file: the template's own
    # header explains the danger by naming the production paths, and a check
    # that cannot tell a rule from a description of one is not a check. That
    # is the same mistake `_without_comments` was written for, made here on
    # its first run.
    template = _without_comments(STACK_ENV.read_text(encoding="utf-8"))
    for name in (
        "VOGT_PORT",
        "VOGT_WORKSPACE_DIR",
        "VOGT_HOME_DIR",
        "VOGT_TAILSCALE_DIR",
        "TAILSCALE_HOSTNAME",
    ):
        assert re.search(rf"^{name}=\S", template, re.MULTILINE), (
            f"{name} must be set explicitly in the template, not left to the "
            "compose default, which points at the running production stack"
        )
    # The estate is deliberately shared (#22): a dev instance with an empty
    # workspace has nothing to collect, nothing to import into and no tree to
    # open a session on, and a private copy would drift away from the trees
    # anyone actually works in. Identity and state are what must stay apart —
    # two pods writing one $HOME overwrite each other's shell history,
    # credentials and agent state, and two hosts sharing a tailnet name
    # resolve to whichever registered last.
    for name in ("VOGT_HOME_DIR", "VOGT_TAILSCALE_DIR"):
        value = re.search(rf"^{name}=(\S+)", template, re.MULTILINE)
        assert value and "/volumes/mydevenv2/" not in value.group(1), (
            f"{name} must be this stack's own; sharing MyDevEnv2's is how two "
            "pods overwrite each other's sessions"
        )


@_needs(STACK_COMPOSE, STACK_ENV)
def test_each_stack_names_the_vogt_token_of_its_own_instance() -> None:
    """A Vogt token belongs to one instance and cannot be shared (#29).

    Tokens are stored hashed in the issuing instance's own database and
    resolved by `token_by_hash`; there is no configuration that accepts a
    supplied value, so an instance cannot be taught a token it did not mint.
    Vogt runs more than one instance deliberately (NFR-D12), which makes "the
    Vogt agent token" one value per instance rather than one value.

    `agent-auth.sh` had the override all along and nothing surfaced it, so
    `vogt-dev` silently inherited the default — production's token — and every
    in-pod agent was refused by an instance that had never issued it. The
    error named the token *file*, which was correct throughout.

    Two halves, because either alone leaves the trap open: the compose must
    pass the variable through (or a stack cannot answer), and this template
    must set it (or the next stack pasted from it inherits production's
    again).
    """
    compose_text = _without_comments(STACK_COMPOSE.read_text(encoding="utf-8"))
    assert "MYDEVENV2_VOGT_SECRET_NAME" in compose_text, (
        "the compose must pass MYDEVENV2_VOGT_SECRET_NAME, or a stack has no "
        "way to name its own instance's token"
    )

    template = _without_comments(STACK_ENV.read_text(encoding="utf-8"))
    named = re.search(r"^MYDEVENV2_VOGT_SECRET_NAME=(\S+)", template, re.MULTILINE)
    assert named, "the template must name this stack's own Vogt token secret"
    assert named.group(1) != "HOMELAB_VOGT_AGENT_TOKEN", (
        "that secret is the production instance's; a second stack using it "
        "presents a token to an instance that never issued it"
    )


# ── The estate's local build-cache registry on Node B (#129, #184) ─────────


@_needs(COMPOSE)
@pytest.mark.parametrize("workflow", ["build.yml", "release.yml", "pod-base.yml"])
def test_image_builds_cache_to_the_node_b_local_registry(workflow: str) -> None:
    """#184: the layer cache lands on Node B's local-registry, never GHCR.

    Every `cache-from`/`cache-to` names the estate's `CACHE_IMAGE` (the local
    registry), and none names GHCR — a WAN round-trip from these runners that
    #55 mistakenly targeted and #129 corrected.

    Estate-gated on the presence of `personal-vogt.compose.yml`: this asserts
    the maintainer's own local-registry IP, which a public fork neither has
    nor should be blocked from removing from its own CI.
    """
    raw = (WORKFLOWS / workflow).read_text("utf-8")
    assert f"CACHE_IMAGE: {LOCAL_REGISTRY_CACHE}" in raw, (
        f"{workflow} must declare the Node B local-registry as CACHE_IMAGE "
        f"({LOCAL_REGISTRY_CACHE})"
    )
    cache_lines = [
        line
        for line in raw.splitlines()
        if "cache-from:" in line or "cache-to:" in line
    ]
    assert cache_lines, f"{workflow} sets a BuildKit layer cache"
    for line in cache_lines:
        assert "${{ env.CACHE_IMAGE }}" in line, (
            f"{workflow}: a cache ref must use the local-registry CACHE_IMAGE, "
            f"not an inline or GHCR ref: {line.strip()}"
        )
        assert "ghcr.io" not in line, (
            f"{workflow}: the layer cache must not land on GHCR (#184): {line.strip()}"
        )


# ── The core-only stack this product retired (DEPLOYMENT.md §9.5) ───────────


@needs_engine
def test_no_vogt_registration_names_the_stack_this_product_replaces() -> None:
    """The core-only stack is retired by `DEPLOYMENT.md` §9.5.

    A default naming a specific deployment keeps working right up until that
    deployment is turned off, and then fails as a handshake error inside an
    agent — the furthest possible place from the file that caused it.
    """
    for path in (MCP_BOOTSTRAP, VOGT_MCP_WRAPPER):
        body = _without_comments(path.read_text(encoding="utf-8"))
        assert RETIRED_CORE_STACK not in body, (
            f"{path.name} still defaults a Vogt endpoint to the core-only "
            "stack this merge replaces"
        )
    assert "http://127.0.0.1:8910" in _without_comments(
        VOGT_MCP_WRAPPER.read_text(encoding="utf-8")
    ), "the fallback is the front door on loopback (NFR-D11)"


# ── The estate hostname as a slugify example (moved from test_core.py) ──────


@_needs(COMPOSE)
def test_slugify_handles_the_estate_hostname() -> None:
    """`slugify` strips parentheses and lower-cases spaces to hyphens.

    The maintainer's Node B host name exercises exactly that shape. The
    public suite keeps the same coverage with a generic parenthesised
    example (`tests/test_core.py`); this pins the estate's own value.
    """
    assert slugify("Node B (winrarhost)") == "node-b-winrarhost"
