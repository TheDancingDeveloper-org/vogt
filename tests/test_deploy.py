"""The shipped deployment artefacts obey the rules that broke cadastre.

`DEPLOYMENT.md` §4.1 records the incident these assertions exist for: the
port, TLS path and token path were `${X:?}`-gated required values, `verify`
and `publish` went green, and the deploy step went red because three values
were never set. The gate was protecting against nothing — the ports bind a
Tailscale address, so choosing them is an allocation inside the tailnet.
"""

from __future__ import annotations

import re
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
COMPOSE = REPO_ROOT / "deploy" / "personal-vogt.compose.yml"
DOCKERFILE = REPO_ROOT / "Dockerfile"
WORKFLOWS = REPO_ROOT / ".github" / "workflows"


def _without_comments(text: str) -> str:
    """Strip comments before asserting on content.

    The first version of these tests failed on the comment that *explains*
    the `${X:?}` rule — the same mistake the marker collector made when it
    read Vogt's own documentation about markers and filed it as work. A
    checker that cannot tell a rule from a description of one is not a
    checker.
    """
    return "\n".join(
        line for line in text.splitlines() if not line.lstrip().startswith("#")
    )


@pytest.fixture(scope="module")
def compose() -> str:
    return _without_comments(COMPOSE.read_text(encoding="utf-8"))


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


def test_the_image_runs_unprivileged() -> None:
    text = _without_comments(DOCKERFILE.read_text(encoding="utf-8"))
    user = re.search(r"USER (\d+):(\d+)", text)
    assert user, "the image must declare a numeric USER"
    assert user.group(1) != "0", "never root"
    assert "USER root" not in text


def test_the_image_runs_as_any_uid_without_a_rebuild() -> None:
    """The uid is a deployment value; the image must not decide it.

    Which uid is right depends on who owns the files being observed, which
    only the host knows. Needing a release to change it would be a defect —
    a fatal one for anyone self-hosting this, who is not us.

    The obstacle is Docker rather than policy: a fresh named volume is seeded
    from the ownership of the data directory *in the image*, so a directory
    owned by one specific uid is unwritable by every deployer who is not that
    uid. It breaks on volume recreation, which usually means during a
    restore. Group 0 plus group-write is what makes an arbitrary `user:` work.
    """
    text = _without_comments(DOCKERFILE.read_text(encoding="utf-8"))
    assert "chown root:0 /var/lib/vogt" in text, (
        "the data directory must be group-0 owned, or a fresh named volume is "
        "unwritable by any uid but the one baked in"
    )
    assert re.search(r"chmod 0?77[07] /var/lib/vogt", text), (
        "the data directory must be group-writable"
    )
    assert re.search(r"USER \d+:0", text), "the image runs with gid 0"


def test_the_compose_owns_the_uid(compose: str) -> None:
    """Overridable at deploy time, with a working default (NFR-D2 revised).

    Defaulted rather than `:?`-gated: gating allocation values is what broke
    every cadastre deploy after #42. A uid that is wrong shows up the first
    time a collector reads nothing; a uid that is unset stops the deploy.
    """
    user = re.search(r'user: "\$\{(\w+):-(\d+)\}:(\d+)"', compose)
    assert user, "the compose must set `user:` from an overridable variable"
    assert user.group(3) == "0", "gid stays 0; only the uid varies by host"


def test_the_image_has_no_default_listen_address() -> None:
    """NFR-D2: the image must not silently bind anything."""
    text = _without_comments(DOCKERFILE.read_text(encoding="utf-8"))
    assert "0.0.0.0" not in text
    assert 'CMD ["--help"]' in text


def test_every_workflow_job_names_a_self_hosted_runner() -> None:
    """NFR-C4, checked here so a new workflow cannot quietly opt out."""
    for path in sorted(WORKFLOWS.glob("*.yml")):
        for number, line in enumerate(path.read_text("utf-8").splitlines(), start=1):
            if not re.match(r"^\s*runs-on\s*:", line):
                continue
            assert "self-hosted" in line, f"{path.name}:{number}: {line.strip()}"
            assert "${{" not in line, (
                f"{path.name}:{number}: dynamic runner selection is rejected"
            )


def test_the_release_workflow_is_tag_only_and_signs_a_digest() -> None:
    """NFR-C3 (r5), NFR-C5, NFR-D10.

    A *release* is tag-triggered: merging must never assign a version, move
    `latest`, or ship a wheel. Commit images are a different act and live in
    `build.yml`.
    """
    raw = (WORKFLOWS / "release.yml").read_text(encoding="utf-8")
    text = _without_comments(raw)
    assert "tags:" in text
    assert "branches:" not in text, "a push to main must never cut a release"
    assert "id-token: write" in raw, "keyless signing needs the OIDC identity"
    assert 'cosign sign --yes "${IMAGE}@${DIGEST}"' in raw, (
        "sign the digest, never a tag: a tag can be moved after signing"
    )


def test_commit_images_carry_no_version_and_do_not_move_latest() -> None:
    """NFR-C3 (r5): a build is deployable without being a release.

    The rule this replaces made a version bump the price of a hotfix, and
    produced three semver releases in one afternoon that marked nothing. What
    it protected is asserted here instead: a commit image is identified by its
    sha alone, so merging cannot claim a version or move an alias that other
    people follow.
    """
    raw = (WORKFLOWS / "build.yml").read_text(encoding="utf-8")
    text = _without_comments(raw)
    assert "branches:" in text and "main" in text

    assert "type=sha" in text, "commit images are identified by their commit"
    assert "type=semver" not in text, "a build must not assign a version"
    assert "latest=false" in text, "a build must not move `latest`"
    assert "uv build" not in text, "the wheel belongs to a release"


def test_a_commit_image_is_signed_like_a_release() -> None:
    """An unsigned artefact that can reach production is the wider hole.

    Commit images are the ones that actually get deployed between releases,
    so relaxing signing for them would invert the guarantee: strongest on the
    artefact that ships least often.
    """
    raw = (WORKFLOWS / "build.yml").read_text(encoding="utf-8")
    assert "id-token: write" in raw
    assert 'cosign sign --yes "${IMAGE}@${DIGEST}"' in raw


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


def test_the_base_image_is_pinned_by_digest() -> None:
    """A floating base tag makes the pinned image a pin of nothing.

    `DEPLOYMENT.md` §2.2 requires Vogt's published image to be digest-pinned
    in the ops repo. If the base it is assembled from can change under a tag,
    that pin describes a build input that is not fixed. It is also the
    `update_automation_gap` Vogt reports on other people's repositories
    (FR-D6) — hard to justify raising for others and not for itself.
    """
    dockerfile = _without_comments(
        (COMPOSE.parent.parent / "Dockerfile").read_text(encoding="utf-8")
    )
    froms = [
        line.strip() for line in dockerfile.splitlines() if line.startswith("FROM ")
    ]
    assert froms, "the Dockerfile builds from something"
    for line in froms:
        assert "@sha256:" in line, f"base image is not digest-pinned: {line}"


def test_both_build_stages_use_the_same_base() -> None:
    """A digest bump that lands on one stage and not the other would build
    the runtime from a different image than the one the venv was resolved
    against — the kind of skew that only shows up at runtime."""
    dockerfile = _without_comments(
        (COMPOSE.parent.parent / "Dockerfile").read_text(encoding="utf-8")
    )
    digests = re.findall(r"FROM \S+@(sha256:[0-9a-f]{64})", dockerfile)
    assert len(digests) >= 2
    assert len(set(digests)) == 1, f"stages disagree on the base image: {set(digests)}"


# ── The merged stack (NFR-D11, NFR-D12, NFR-C6) ───────────────────────────
#
# Until this section existed, no test in the repository read
# `deploy/vogt-stack.compose.yml` or either `stack-image` job — which is how
# the compose file kept a placeholder digest of zeros through four stages
# while the documentation around it described a pinned image. The artefact
# these assert on has now been built, smoke-tested, signed and published by
# CI, so every claim below is about something that exists.

STACK_COMPOSE = REPO_ROOT / "deploy" / "vogt-stack.compose.yml"
ENGINE_PORT = "8910"
CORE_PORT = "8911"


@pytest.fixture(scope="module")
def stack() -> str:
    return _without_comments(STACK_COMPOSE.read_text(encoding="utf-8"))


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


@pytest.mark.parametrize("workflow", ["build.yml", "release.yml"])
def test_the_merged_image_is_built_from_the_engine_dockerfile(workflow: str) -> None:
    """The context is the repository, not `engine/`.

    `engine/Dockerfile` reaches for `web/dist`, `pyproject.toml` and `src/`
    as well as the crates, so a job that sets `context: engine` builds an
    engine and calls it a stack.
    """
    raw = (WORKFLOWS / workflow).read_text(encoding="utf-8")
    assert "stack-image:" in raw, f"{workflow} builds the merged image"
    job = raw[raw.index("  stack-image:") :]
    assert "file: engine/Dockerfile" in job, workflow
    assert re.search(r"^\s+context: \.$", job, re.MULTILINE), (
        f"{workflow}: the repository root is the build context"
    )


@pytest.mark.parametrize("workflow", ["build.yml", "release.yml"])
def test_the_pwa_is_built_before_the_merged_image(workflow: str) -> None:
    """`rust-embed` reads `web/dist/` at compile time.

    An image built without this step compiles, starts, serves a placeholder
    where the product's front end should be, and passes every check that
    asks whether it is running.
    """
    job = (WORKFLOWS / workflow).read_text(encoding="utf-8")
    job = job[job.index("  stack-image:") :]
    assert job.index("pnpm build") < job.index("file: engine/Dockerfile"), (
        f"{workflow}: the bundle is a build input, not a later artefact"
    )


@pytest.mark.parametrize("workflow", ["build.yml", "release.yml"])
def test_both_halves_run_before_the_merged_image_is_pushed(workflow: str) -> None:
    """The failure this catches is a stack missing the half nobody looked at.

    Both entrypoints are exercised in the candidate image, and the push step
    comes after — so an image that builds and cannot run one of its two
    products never reaches the registry.
    """
    job = (WORKFLOWS / workflow).read_text(encoding="utf-8")
    job = job[job.index("  stack-image:") :]
    smoke_vogt = job.index("--entrypoint vogt")
    smoke_engine = job.index("--entrypoint mydevenv2-server")
    push = job.index("push: true")
    assert max(smoke_vogt, smoke_engine) < push, (
        f"{workflow}: smoke-test the candidate, then push it"
    )


@pytest.mark.parametrize("name", ["image", "stack-image"])
def test_the_two_streams_are_kept_apart(name: str) -> None:
    """NFR-D12: `dev` images can never be mistaken for commit images.

    `dev` and `dev-<sha>` on one branch, `sha-<commit>` on the other, and no
    tag either can move — so "which build is that?" stays answerable and a
    dev image cannot be picked up by anything following the prod stream.

    Both images, because the requirement says both and the merge added the
    second: a rule kept by the core-only image and dropped by the merged one
    would leave the artefact the merge exists for as the unlabelled stream.
    """
    raw = (WORKFLOWS / "build.yml").read_text(encoding="utf-8")
    start = raw.index(f"\n  {name}:\n")
    rest = raw[start + 1 :]
    end = re.search(r"\n  [a-z][a-z-]*:\n", rest)
    job = rest[: end.start()] if end else rest
    assert "type=sha,prefix=dev-,enable=${{ github.ref == 'refs/heads/dev' }}" in job
    assert "type=raw,value=dev,enable=${{ github.ref == 'refs/heads/dev' }}" in job
    assert "type=sha,enable=${{ github.ref != 'refs/heads/dev' }}" in job
    assert "type=semver" not in job, "a build must not assign a version"
    assert "value=latest" not in job, "a build must not move an alias"


def test_a_tag_can_release_the_merged_image() -> None:
    """NFR-C6's second clause, which was vacuous until `release.yml` had one.

    "A push builds `sha-` images, only a tag releases" was true of the merged
    image in its first half and meaningless in its second: no tag could
    produce one. A release now assigns it a version and signs its digest.
    """
    raw = (WORKFLOWS / "release.yml").read_text(encoding="utf-8")
    job = raw[raw.index("  stack-image:") :]
    assert "type=semver,pattern={{version}}" in job, "a release assigns a version"
    assert 'cosign sign --yes "${STACK_IMAGE}@${DIGEST}"' in job, (
        "sign the digest, never a tag: a tag can be moved after signing"
    )


# ── What a session's agent is told about where Vogt is (FR-E5) ────────────

MCP_BOOTSTRAP = REPO_ROOT / "engine" / "deploy" / "mcp-bootstrap.sh"
VOGT_MCP_WRAPPER = REPO_ROOT / "engine" / "deploy" / "vogt-mcp-auth.sh"
RETIRED_CORE_STACK = "winrarhost.tailc7d3c.ts.net:18094"

#: These three read the *engine's* deploy scripts, which a core-only checkout
#: does not have — and NFR-Q6's `core` job proves the core stands alone by
#: deleting `engine/`, `web/` and `mobile/` and running this suite. It found
#: these on their first run, which is the job working: a merged repository
#: makes it very easy to write a core test that quietly needs the engine.
needs_engine = pytest.mark.skipif(
    not MCP_BOOTSTRAP.is_file(),
    reason=(
        "the merged tree carries the engine's deploy scripts; "
        "a core-only checkout does not"
    ),
)


@needs_engine
def test_a_sessions_own_endpoint_is_what_its_agent_is_registered_against() -> None:
    """FR-E5, and a failure that was silent in both directions.

    `vogt session start` exports `VOGT_URL` — the deployment this session
    belongs to. The bootstrap that registers the MCP clients read only its
    own `VOGT_MCP_URL`, so every session's agent was pointed at whatever the
    script's default said, and the session's answer was discarded.
    """
    script = MCP_BOOTSTRAP.read_text(encoding="utf-8")
    body = _without_comments(script)
    assert "${VOGT_MCP_URL:-}" in body, "an explicit override still wins"
    assert '_vogt_endpoint="${VOGT_URL%/}/mcp"' in body, (
        "the session's own endpoint is the second thing consulted, before "
        "any default: it is the only value here that knows which deployment "
        "this session belongs to"
    )
    assert '--url "$VOGT_ENDPOINT"' in body, (
        "and the registration that pins a URL pins that one"
    )


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


@needs_engine
def test_the_opencode_registration_does_not_freeze_an_endpoint() -> None:
    """It is written once and reused by every later session.

    Pinning `VOGT_URL` there overrode the session's own value — the exact
    lesson the cadastre `:18081 -> :18092` move taught, which this file
    already records in a comment for Codex and had not applied here.
    """
    body = _without_comments(MCP_BOOTSTRAP.read_text(encoding="utf-8"))
    opencode = body[body.index("install_vogt_opencode()") :]
    opencode = opencode[: opencode.index("\n}")]
    assert "--env" not in opencode, (
        "the wrapper reads VOGT_URL at spawn; a registration that pins one "
        "freezes whichever deployment was current when it was written"
    )


def test_a_push_run_is_never_cancelled_by_the_next_push() -> None:
    """NFR-C1's path gating makes cancellation lose coverage permanently.

    A push is classified by `before..sha`, so each commit is checked by
    exactly one run and no later run looks at it again. Cancel that run and
    those files are not checked later — they are checked never.

    Found the way these things are found: `tests/test_deploy.py` changed in
    one push, the run was cancelled by the next push, the following run's
    range began after it, and a lint error reached `dev` through the gap.
    """
    raw = (WORKFLOWS / "ci.yml").read_text(encoding="utf-8")
    assert "cancel-in-progress: ${{ github.event_name == 'pull_request' }}" in raw, (
        "a branch push must run to completion; only a superseded pull request "
        "may be cancelled, because its runs classify against the merge base "
        "and a later run covers everything an earlier one would have"
    )


def test_the_local_check_runs_what_ci_runs() -> None:
    """`scripts/check.sh` is a floor under CI, not a second opinion.

    It exists because remembering four commands per half and being right
    about all of them every time is not a strategy — `ruff check` and
    `ruff format --check` feel like one command and are two, and that is how
    a red build reached `dev`.

    Asserted one way only. Every command the script runs must be a command CI
    runs, so a green run locally means something; the reverse is not required,
    because a job may legitimately do more than a developer needs before
    pushing.
    """
    # Comments stripped, because this file's own header explains the
    # `ruff check` / `ruff format` slip by naming both — so a guard that
    # searched the raw text would find "ruff format" in the sentence about
    # forgetting to run it. That is the mistake `_without_comments` exists
    # for, made again three hundred lines below it, and caught by mutating
    # the script rather than by reading the test.
    script = _without_comments(
        (REPO_ROOT / "scripts" / "check.sh").read_text(encoding="utf-8")
    )
    ci = (WORKFLOWS / "ci.yml").read_text(encoding="utf-8")
    required = [
        ("ruff check", "ruff check ."),
        ("ruff format", "ruff format --check ."),
        ("mypy", "mypy"),
        ("pytest", "pytest"),
        ("cargo fmt", "--all"),
        ("cargo clippy", "--workspace --all-targets -- -D warnings"),
        ("cargo test", "--workspace"),
        ("pnpm typecheck", "pnpm typecheck"),
        ("pnpm test", "pnpm test"),
    ]
    for label, fragment in required:
        assert label in script, f"{label} is missing from scripts/check.sh"
        assert fragment in ci, (
            f"scripts/check.sh runs {label}, and `ci.yml` no longer contains "
            f"{fragment!r} — one of the two has moved and they must not drift"
        )
