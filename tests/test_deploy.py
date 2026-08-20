"""The shipped deployment artefacts obey the rules that broke cadastre.

`DEPLOYMENT.md` §4.1 records the incident these assertions exist for: the
port, TLS path and token path were `${X:?}`-gated required values, `verify`
and `publish` went green, and the deploy step went red because three values
were never set. The gate was protecting against nothing — the ports bind a
Tailscale address, so choosing them is an allocation inside the tailnet.
"""

from __future__ import annotations

import json
import re
import subprocess
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


def test_the_image_carries_git() -> None:
    """git is a runtime dependency of import and of the git-local collector.

    v0.2.0 shipped without it. Both builds were green: nothing in the test
    suite runs the image, and every git call in the product is a subprocess
    that fails at the point of use. Production therefore ran for days with
    `project.import` unable to run at all, and with `git-local` recording a
    clean checkout it had never read (#19, #20, #21).

    This assertion is the cheap half. The half that actually proves it is
    `docker run --entrypoint git "$CANDIDATE" --version` in `build.yml` and
    `release.yml`, because a package can be named here and still not be in
    the artefact — which is what NFR-Q7 now requires.
    """
    text = _without_comments(DOCKERFILE.read_text(encoding="utf-8"))
    runtime = text.split("AS runtime", 1)
    assert len(runtime) == 2, "the Dockerfile must have a named runtime stage"
    assert re.search(r"apt-get install[^\n]*\bgit\b", runtime[1]), (
        "the runtime stage must install git; the build stage having it is "
        "not the same thing, and is what v0.2.0 relied on"
    )


def test_both_image_smoke_tests_run_git() -> None:
    """NFR-Q7: the proof is running the artefact, so it is checked like code.

    Reading a Dockerfile cannot catch a build that drops a package. Running
    the binary in the built image can, and both publishing paths must do it —
    a release that skips the check is exactly how this reached production.
    """
    for workflow in ("build.yml", "release.yml"):
        text = (WORKFLOWS / workflow).read_text(encoding="utf-8")
        assert 'docker run --rm --entrypoint git "$CANDIDATE" --version' in text, (
            f"{workflow} must ask the built image for git, not the Dockerfile"
        )


def test_the_dev_image_build_turns_the_ai_clients_on() -> None:
    """A build arg nothing overrides is just a default (#23).

    `engine/Dockerfile` gates `claude`, `codex` and Flutter behind args that
    default to false, and pointed at the engine's Woodpecker pipeline —
    `build-and-push-dev` — as the build that turned them on. That pipeline
    never ran in this repository and its vendored copy has since been
    deleted, so for a while nothing turned them on at all and `vogt-dev` ran
    with neither client while every build stayed green.

    `build.yml` owns the arg now, which is why this reads `build.yml`. The
    test outlives the file it was written about because the property was never
    "a pipeline sets this" — it was "something that actually runs does".

    Both build steps must pass them — the candidate as well as the push, or
    the image that gets smoke-tested is not the image that gets published.

    `theclawbay` joined the probed set after WI-17, by a different route to the
    same place: it was installed by no image at all, living only in a persisted
    `$HOME`, so binding `vogt-dev` to a different home volume took it away and
    — again — nothing was red. #23 was a build arg nothing asserted; WI-17 was
    a tool nothing ran. The loop answers both.

    Asserted as a set rather than as the literal loop line, because the exact
    string broke the first time a fourth tool was added, which is a test
    failing for the wrong reason: the property is *which tools are proven*, not
    how they are spelled in a `for`. Removing one still fails, which is the
    part worth keeping.
    """
    text = (WORKFLOWS / "build.yml").read_text(encoding="utf-8")
    on_dev = r"=\$\{\{ github\.ref == 'refs/heads/dev' \}\}"
    for arg in ("INSTALL_AI_CLIENTS", "INSTALL_FLUTTER"):
        wired = re.findall(arg + on_dev, text)
        assert len(wired) == 2, (
            f"{arg} must be passed to both the candidate and the pushed "
            f"build of engine/Dockerfile; found {len(wired)}"
        )
    loop = re.search(r"for tool in ([^;]+); do", text)
    assert loop, (
        "the dev image's smoke test must loop over the tools the image carries "
        "and run each one (NFR-Q7)"
    )
    probed = set(loop.group(1).split())
    owed = {"claude", "codex", "flutter", "theclawbay"}
    assert owed <= probed, (
        "the dev image's smoke test must ask the image for the clients "
        "(NFR-Q7); a build arg nothing asserts is how #23 went unnoticed, and "
        f"a tool nothing runs is how WI-17 did. Missing: {sorted(owed - probed)}"
    )


def test_the_image_has_no_default_listen_address() -> None:
    """NFR-D2: the image must not silently bind anything."""
    text = _without_comments(DOCKERFILE.read_text(encoding="utf-8"))
    assert "0.0.0.0" not in text
    assert 'CMD ["--help"]' in text


def _workflow_files() -> list[Path]:
    """Both extensions. A gate a `.yaml` file can walk past is not a gate."""
    return sorted([*WORKFLOWS.glob("*.yml"), *WORKFLOWS.glob("*.yaml")])


def test_every_workflow_job_names_a_self_hosted_runner() -> None:
    """NFR-C4, checked here so a new workflow cannot quietly opt out.

    r20 considered a trust split — fork validation on GitHub-hosted runners,
    publication on the pool — and rejected it: where a job runs is not what
    makes an image generic, and every job that builds or publishes one never
    left the pool. The exposure a public repository creates is fork-submitted
    code executing on a tailnet-connected worker, and that is closed by
    requiring approval for fork pull request workflows, which is a
    prerequisite of NFR-O1's milestone rather than a property of `runs-on`.
    """
    for path in _workflow_files():
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
AGENT_AUTH = REPO_ROOT / "engine" / "deploy" / "agent-auth.sh"
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
def test_the_bootstrap_writes_nothing_to_the_protocol_stream() -> None:
    """#28: stdout is the MCP transport, so a banner there is a bad frame.

    `mydevenv2-agent-auth run` invokes this bootstrap on every launch, and two
    of the things it launches are stdio MCP servers. `tests/test_bridge.py`
    already forbids this of the bridge — "a diagnostic on stdout corrupts
    framing and looks like a client bug" — and the bridge obeys it. The
    launcher above it did not, so the rule held exactly where it was tested
    and broke in the layer that wraps it.

    Asserted on every `printf`/`echo`, not just the one that was wrong: the
    next diagnostic added here is the next bad frame, and it would be found
    the same way this one was — by a client, later, looking like something
    else.
    """
    lines = MCP_BOOTSTRAP.read_text(encoding="utf-8").splitlines()
    offenders: list[str] = []
    for index, line in enumerate(lines):
        if not re.match(r"\s*(printf|echo)\b", line):
            continue
        # A redirect may sit on a continuation line, which is how the four
        # diagnostics here are written.
        window = " ".join(lines[index : index + 3])
        if ">&2" not in window and ">/dev/null" not in window:
            offenders.append(line.strip())
    assert not offenders, (
        "these write to stdout, which for a stdio MCP server is the protocol "
        f"stream: {offenders}"
    )


CADASTRE_MCP_WRAPPER = REPO_ROOT / "engine" / "deploy" / "cadastre-mcp-auth.sh"


def _wrapper_sandbox(tmp_path: Path, wrapper: Path, bridge: str) -> tuple[Path, Path]:
    """Stand a wrapper up far enough to run it and watch its stdout.

    Two things are replaced and nothing else. `mydevenv2-agent-auth` becomes a
    stub that does the one thing on this path that matters — invoke the
    bootstrap, then `exec` the command it was handed — because the real one
    brokers Infisical secrets and a test cannot have those. The bridge itself
    becomes a stub that answers one frame, because what is under test is the
    launcher, not the remote.

    The bootstrap is the real script, the wrapper is the real script, and the
    only edit to either is the `/usr/local/bin` prefix the wrapper `exec`s
    through, which is an absolute path a test cannot otherwise intercept.
    """
    bin_dir = tmp_path / "bin"
    bin_dir.mkdir()

    def _install(name: str, body: str) -> Path:
        path = bin_dir / name
        path.write_text(body, encoding="utf-8")
        path.chmod(0o755)
        return path

    _install(
        "mydevenv2-agent-auth",
        "#!/usr/bin/env bash\n"
        "set -euo pipefail\n"
        '[[ "${1:-}" == "run" ]] || exit 2\n'
        "shift\n"
        '[[ "${1:-}" == "--" ]] && shift\n'
        # `bash <path>`, not the path alone: the Dockerfile is what makes
        # these scripts executable, and the checkout they are read from here
        # has not been through it.
        f'bash "{MCP_BOOTSTRAP}"\n'
        'exec "$@"\n',
    )
    # A stdio bridge: read the request, answer it, say the rest on stderr.
    _install(
        bridge,
        "#!/usr/bin/env bash\n"
        "set -euo pipefail\n"
        "read -r _line\n"
        "printf 'bridge: connected\\n' >&2\n"
        'printf \'{"jsonrpc":"2.0","id":1,"result":{}}\\n\'\n',
    )

    local = tmp_path / "wrapper.sh"
    local.write_text(
        wrapper.read_text(encoding="utf-8").replace("/usr/local/bin", str(bin_dir)),
        encoding="utf-8",
    )
    local.chmod(0o755)
    return local, bin_dir


@needs_engine
@pytest.mark.parametrize(
    ("wrapper", "bridge"),
    [
        (VOGT_MCP_WRAPPER, "vogt-mcp-remote"),
        (CADASTRE_MCP_WRAPPER, "cadastre-mcp-remote"),
    ],
    ids=["vogt", "cadastre"],
)
def test_nothing_but_protocol_reaches_a_wrappers_stdout(
    tmp_path: Path, wrapper: Path, bridge: str
) -> None:
    """#28, asserted by running it: every stdout line must parse as JSON.

    The same assertion `tests/test_bridge.py` already makes about the bridge
    — "a diagnostic on stdout corrupts framing and looks like a client bug" —
    made here about the thing that launches it. The static check above reads
    the bootstrap's own `printf`s; this one reads fd 1 of the whole wrapper
    path, so a diagnostic introduced anywhere along it is caught, including
    in a script this test never names.

    Whether a client survives a stray line is a matter of how defensively it
    skips what it cannot parse, which is what made the original report
    intermittent across clients rather than absent.
    """
    script, bin_dir = _wrapper_sandbox(tmp_path, wrapper, bridge)
    home = tmp_path / "home"
    home.mkdir()
    environment = {
        "PATH": f"{bin_dir}:/usr/bin:/bin",
        "HOME": str(home),
        "TMPDIR": str(tmp_path),
        # An absent checkout is a supported state and reports on stderr; it
        # keeps the bootstrap from trying to pip-install anything.
        "MYDEVENV2_CADASTRE_SRC": str(tmp_path / "absent"),
        "MYDEVENV2_VOGT_SRC": str(tmp_path / "absent"),
    }
    completed = subprocess.run(
        [str(script)],
        input='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}\n',
        capture_output=True,
        text=True,
        env=environment,
        timeout=60,
        check=False,
    )
    assert completed.returncode == 0, completed.stderr
    frames = [line for line in completed.stdout.splitlines() if line.strip()]
    assert frames, "the bridge's own answer should still be there"
    for frame in frames:
        try:
            message = json.loads(frame)
        except json.JSONDecodeError:  # pragma: no cover - the failure this guards
            pytest.fail(
                f"{wrapper.name} put a non-JSON line on the protocol stream, "
                f"which is what a client sees as a framing error: {frame!r}"
            )
        assert message["jsonrpc"] == "2.0"
    # Not merely silent: an operator still gets told, one stream over. The
    # wording itself is asserted below, by the test that owns it (#30); all
    # this one cares about is that moving the banner off stdout did not
    # silence it.
    assert "registrations written" in completed.stderr


@needs_engine
def test_the_access_check_probes_vogt_and_not_only_cadastre() -> None:
    """#30: a check that skips one service converts unknown into assurance.

    `agent-auth check` probed seven services and named Vogt only in the
    bootstrap's banner — which reports that registrations were *written*, not
    that anything answers. It was green while Vogt was completely unusable
    from the pod, and the first evidence arrived later from a client (#29).

    It is also the only thing that runs in the pod holding the credential a
    client will use, against the endpoint that client will use, so it is the
    one place that can catch a per-instance token mismatch.
    """
    body = _without_comments(AGENT_AUTH.read_text(encoding="utf-8"))
    assert 'probe_mcp "Vogt MCP"' in body, (
        "the check must probe Vogt and name it, as it does Cadastre"
    )
    assert '"method":"initialize"' in body, "probed the way a client connects"
    # Absent is not broken: an instance may legitimately not be deployed, and
    # agent auth has to keep working for git/gh regardless. The three states
    # must stay distinguishable (FR-O4's rule, one layer out).
    assert "skip: Vogt MCP" in body, (
        "an unconfigured instance must read as 'not configured', never as a "
        "failure and never as success"
    )
    # A configured-but-broken instance must be told apart from a working one,
    # and the credential a client reads must exist as well as be loaded.
    assert '[[ -s "${VOGT_TOKEN_FILE:-}" ]]' in body, (
        "a loaded token that never reached its file leaves every registered "
        "client broken while this check is green — and the rejection they "
        "eventually see names that file"
    )


@needs_engine
def test_the_vogt_probe_reads_the_answer_and_not_only_the_status() -> None:
    """#29 reached its client as a JSON-RPC error, which rides on a 200.

    `curl -f` was the first shape of this probe. It collapses every refusal
    into exit 22 and discards the body, so the check could say only that
    something went wrong — while the interesting half of the answer, the
    reason the server gave, went to /dev/null. And an MCP server that refuses
    a *handshake* may still answer HTTP 200: `-32001` is carried in the body,
    which is exactly how the outage in #29 was finally seen. A probe that
    stopped at the status code would have called that green, which is the
    same false assurance #30 is about, one layer in.
    """
    body = _without_comments(AGENT_AUTH.read_text(encoding="utf-8"))
    assert "%{http_code}" in body, (
        "capture the status rather than letting -f throw the response away"
    )
    assert "*'\"error\"'*" in body, (
        "a JSON-RPC error is carried on a 200; the body has to be read"
    )
    assert "$status" in body and "$detail" in body, (
        "the failure must report what the server actually said, not only that "
        "it said no"
    )
    assert 'probe_mcp "Cadastre MCP"' in body
    assert 'probe_mcp "Vogt MCP"' in body, (
        "both endpoints must use the same JSON-RPC-aware probe"
    )


@needs_engine
@pytest.mark.parametrize(
    ("status", "body", "curl_error", "curl_exit", "expected_exit", "message"),
    [
        ("200", '{"jsonrpc":"2.0","id":1,"result":{}}', "", 0, 0, "ok: Test MCP"),
        (
            "200",
            '{"jsonrpc":"2.0","id":1,"error":{"code":-32001}}',
            "",
            0,
            1,
            "refused the handshake",
        ),
        (
            "403",
            '{"detail":"wrong token"}',
            "",
            0,
            1,
            "rejected TEST_TOKEN at https://mcp.invalid (HTTP 403)",
        ),
        ("000", "", "connection refused", 7, 1, "is unreachable"),
    ],
    ids=["success", "json-rpc-error", "http-rejection", "unreachable"],
)
def test_the_shared_mcp_probe_classifies_transport_and_json_rpc_outcomes(
    tmp_path: Path,
    status: str,
    body: str,
    curl_error: str,
    curl_exit: int,
    expected_exit: int,
    message: str,
) -> None:
    """#35: both MCP checks trust the handshake, not merely HTTP 200."""
    response_file = tmp_path / "response"
    error_file = tmp_path / "error"
    script = f"""
source {AGENT_AUTH!s}
curl() {{
    local output_file="" error_target=""
    while (($#)); do
        case "$1" in
            -o) output_file="$2"; shift 2 ;;
            -w) shift 2 ;;
            *) shift ;;
        esac
    done
    printf '%s' "$PROBE_BODY" >"$output_file"
    printf '%s' "$PROBE_ERROR" >&2
    printf '%s' "$PROBE_STATUS"
    return "$PROBE_EXIT"
}}
probe_mcp "Test MCP" "https://mcp.invalid" "secret" "TEST_TOKEN" \\
    {response_file!s} {error_file!s} ""
"""
    completed = subprocess.run(
        ["bash", "-c", script],
        capture_output=True,
        text=True,
        env={
            "PATH": "/usr/bin:/bin",
            "PROBE_STATUS": status,
            "PROBE_BODY": body,
            "PROBE_ERROR": curl_error,
            "PROBE_EXIT": str(curl_exit),
        },
        check=False,
    )

    assert completed.returncode == expected_exit
    assert message in completed.stdout + completed.stderr


@needs_engine
def test_the_bootstrap_banner_does_not_claim_readiness() -> None:
    """#30: the banner said 'ready' about work that only wrote config.

    It reports that MCP client registrations were *written* — nothing in this
    script contacts either endpoint. Read as readiness it is the reason a
    completely unusable Vogt looked fine from the pod: the banner said ready,
    the check below it was green because it never probed Vogt, and the first
    evidence came later from a client (#29). Registration and reachability are
    different claims and this one may only make the first.
    """
    body = _without_comments(MCP_BOOTSTRAP.read_text(encoding="utf-8"))
    assert "registrations are ready" not in body, (
        "writing a registration is not evidence that anything answers"
    )
    assert "registrations written" in body, "say what was actually done"


@needs_engine
def test_no_credential_is_fetched_in_a_way_that_hides_a_failure() -> None:
    """`export X="$(cmd)"` takes `export`'s exit status, which is always 0.

    `agent-auth.sh` runs under `set -euo pipefail`, so it reads as though a
    failed `get_secret` would stop it. It does not: the status tested is
    `export`'s, the substitution's failure is discarded, and the variable is
    left as the empty string. Every service is then called with an empty
    credential and answers 401 — which looks like a revoked token, a long way
    from the secret store that was actually unavailable.

    Five secrets were fetched that way with no emptiness check either. The two
    GitHub ones already used assign-then-guard-then-export; this asserts all
    of them do.
    """
    offenders: list[str] = []
    for path in sorted((REPO_ROOT / "engine" / "deploy").glob("*.sh")):
        for line in _without_comments(path.read_text(encoding="utf-8")).splitlines():
            if re.match(r'\s*export\s+\w+="\$\(', line):
                offenders.append(f"{path.name}: {line.strip()}")
    assert not offenders, (
        f"assign, check, then export — this form discards the failure: {offenders}"
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


def test_no_two_pushed_commits_share_a_concurrency_group() -> None:
    """NFR-C1's path gating makes a lost run lose coverage permanently.

    A push is classified by `before..sha`, so each commit is checked by
    exactly one run and no later run looks at it again. Lose that run and
    those files are not checked later — they are checked never.

    **`cancel-in-progress: false` is not enough, and believing it was cost a
    second escape.** That setting governs runs which are *in progress*; a run
    still **pending** is cancelled whenever a newer run joins its group,
    unconditionally. On a single self-hosted runner nearly every run is
    pending for a while, so a `ruff format` failure slipped through exactly
    as the previous failure had, while the workflow carried a comment saying
    the hole was shut.

    Keying a push by its commit is what closes it: no two pushed commits
    share a group, so none can supersede another. A pull request still
    cancels its own superseded runs, which loses nothing — those classify
    against the merge base.
    """
    raw = (WORKFLOWS / "ci.yml").read_text(encoding="utf-8")
    group = re.search(r"^  group: (.+)$", raw, re.MULTILINE)
    assert group, "ci.yml declares a concurrency group"
    assert "github.sha" in group.group(1), (
        f"pushes must be keyed by commit, not by ref: {group.group(1)!r}. Two "
        "commits in one group means the older run can be cancelled while "
        "pending, and its files are then checked by nothing, ever"
    )
    assert "pull_request.number" in group.group(1), (
        "a pull request still groups by PR, so its superseded runs can be "
        "cancelled — they classify against the merge base and lose nothing"
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


def test_every_gate_nfr_c6_names_is_in_the_pipeline() -> None:
    """NFR-C6 lists what CI shall run for the merged product.

    The requirement names seven things. Until this, the only test that read
    `ci.yml` checked its `runs-on:` lines — so the pipeline could have lost
    `cargo clippy` or the APK build entirely and nothing here would have
    noticed, which is a strange gap in a repository whose whole argument is
    that an unrun check and a passing check look identical.

    This asserts presence, not the gating: §6.2's NFR-C6 row records that each
    half runs only when its own paths changed, deliberately and with the
    argument written in the workflow. A step that is present and skipped is a
    decision; a step that is absent is an accident.
    """
    ci = (WORKFLOWS / "ci.yml").read_text(encoding="utf-8")
    for gate, fragment in (
        ("cargo fmt", "cargo fmt --all -- --check"),
        ("cargo clippy", "cargo clippy --workspace --all-targets -- -D warnings"),
        ("cargo test", "cargo test --workspace"),
        ("pnpm typecheck", "pnpm typecheck"),
        ("pnpm test", "pnpm test"),
        ("the APK build", "./gradlew assembleDebug"),
        ("pytest", "uv run --no-sync pytest"),
    ):
        assert fragment in ci, f"NFR-C6 names {gate}; `ci.yml` no longer runs it"


ENTRYPOINT = REPO_ROOT / "engine" / "deploy" / "entrypoint.sh"


@needs_engine
def test_only_a_loopback_core_is_started_by_this_container() -> None:
    """NFR-D11: vogt-core binds loopback and is never published.

    The rule is enforced where the process is started, and §6.2a recorded
    that no test read the file. It also recorded that a non-loopback URL is
    declined *silently*, and that is not true — reading the script to write
    this found it announces the case on stderr, deliberately, because "no
    core started" and "core started elsewhere" look identical in a process
    list. The row is corrected rather than the script.
    """
    script = ENTRYPOINT.read_text(encoding="utf-8")
    body = _without_comments(script)

    # The loopback set, exactly. A hostname that resolves to 127.0.0.1 is not
    # in it, and should not be: this is about what the container starts, and
    # the answer must not depend on a resolver.
    assert "127.0.0.1|localhost|'[::1]')" in body, (
        "the loopback set is the case arm; widening it widens what this "
        "container will start on a published interface"
    )

    # Not silent, in either direction.
    assert "proxying to a core this container does not run" in body, (
        "a non-loopback URL is a legitimate topology and has to say so — "
        "otherwise it is indistinguishable from no core at all"
    )
    assert "refusing to start" in body and "exit 78" in body, (
        "a core URL with no port can never be reached, and a front door that "
        "cannot reach its core should not come up claiming to be one"
    )


@needs_engine
def test_the_engine_is_the_only_published_port_in_the_merged_image() -> None:
    """NFR-D11 again, from the image rather than the compose file.

    `test_the_merged_stack_publishes_the_engine_and_only_the_engine` asserts
    the deployment; this asserts what the image itself offers, so a compose
    file written by somebody else cannot expose the core by accident.
    """
    dockerfile = _without_comments(
        (REPO_ROOT / "engine" / "Dockerfile").read_text(encoding="utf-8")
    )
    exposed = re.findall(r"^EXPOSE\s+(.+)$", dockerfile, re.MULTILINE)
    ports = {port for line in exposed for port in line.split()}
    assert ports <= {ENGINE_PORT}, (
        f"the merged image exposes {sorted(ports)}; only the engine's "
        f"{ENGINE_PORT} may be published (NFR-D11)"
    )


def test_the_gate_fails_on_anything_that_is_not_success_or_skipped() -> None:
    """NFR-Q6: both suites pass in the merged repository.

    The halves are path-gated (NFR-C1), so on most changes one of them is
    skipped — and "both passed" for a change that touched both is true only
    because one job aggregates the results and refuses anything that is
    neither a pass nor a deliberate skip.

    The case worth pinning is `cancelled`. A gate written as
    `if result == "failure": fail` treats a cancelled job as a pass, and this
    repository has already been bitten once today by a cancelled run being
    indistinguishable from a run that had nothing to do (§6.3 finding 19).
    Here the default arm fails, so only `success` and `skipped` get through.
    """
    ci = (WORKFLOWS / "ci.yml").read_text(encoding="utf-8")
    gate = ci[ci.index("\n  ci:\n") :]

    for half in ("python:", "core-alone:", "engine:", "android:"):
        assert half in gate, f"the gate no longer weighs {half.rstrip(':')}"

    # The shape: a success arm, a skipped arm, and a catch-all that fails.
    assert "success)" in gate and "skipped)" in gate, gate[:400]
    catch_all = gate.index("*)")
    assert "failed=1" in gate[catch_all : catch_all + 200], (
        "the default arm must fail — a gate that only checks for `failure` "
        "passes a cancelled job, and a cancelled job checked nothing"
    )
    assert 'if [ "$failed" -ne 0 ]' in gate and "exit 1" in gate


def test_a_workflow_may_cancel_only_when_a_later_run_covers_the_same_ground() -> None:
    """The rule that tells `docs.yml` apart from `ci.yml`.

    Both cancel superseded runs on a branch and only one of them can afford
    to. `docs.yml` checks the whole tree every time, so a later run covers
    everything a superseded one would have. `ci.yml` classifies a push by
    `before..sha` — each commit is examined by exactly one run, and a lost
    run is coverage lost permanently.

    Asserted because the two files look the same at the point where they
    differ, and the difference cost two escaped failures before it was
    understood (§6.3 finding 19).
    """
    docs = (WORKFLOWS / "docs.yml").read_text(encoding="utf-8")
    assert "whole tree" in docs, (
        "docs.yml cancels superseded runs; the comment saying why that is "
        "safe here is the thing that stops it being copied into a workflow "
        "where it is not"
    )
    assert "before..sha" not in _without_comments(docs), (
        "if docs.yml ever classifies by diff range, cancelling its runs loses "
        "coverage the way ci.yml's did"
    )


STACK_ENV = REPO_ROOT / "deploy" / "vogt-stack.env.example"


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


def test_a_release_apk_is_signed_or_the_job_stops() -> None:
    """NFR-C6's signed APK, and the failure it must not produce.

    Gradle produces a release APK with *no* signature when the signing config
    is absent — silently, because an unsigned release build is a legitimate
    thing to want. Publishing that under a release tag is the failure this
    job exists against, so it refuses to start without a keystore and
    verifies the artefact afterwards rather than trusting the build.

    The keystore is the one in the retired forge (§9.6). A new key is a
    different app identity and cannot upgrade an existing install, which is
    why the job names the missing secrets instead of generating one.
    """
    release = (WORKFLOWS / "release.yml").read_text(encoding="utf-8")
    assert "\n  android:" in release, "release.yml builds the signed APK"
    job = release[release.index("\n  android:") :]

    assert "MYDEVENV2_ANDROID_KEYSTORE_B64" in job, "the keystore is a secret"
    assert "exit 1" in job, "a missing keystore stops the job"
    assert "assembleRelease" in job, "a release build, not a debug one"
    assert "apksigner" in job and "verify" in job, (
        "the signature is verified after the build; Gradle's silence about a "
        "missing signing config is the whole risk"
    )
    assert "rm -f" in job, (
        "the keystore is removed from the runner whether the build succeeded "
        "or not — a shared self-hosted runner is a shared filesystem"
    )
    # `ci.yml` keeps building the *debug* APK on every push, and must not
    # start signing: a build proves the shell assembles, and signing is what
    # makes an artefact somebody installs. The pipeline this replaced
    # conflated them.
    ci = (WORKFLOWS / "ci.yml").read_text(encoding="utf-8")
    assert "assembleRelease" not in ci, (
        "signing belongs to a release, not to every push (NFR-C3's rule "
        "applied to the APK)"
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


ENGINE_DOCKERFILE = WORKFLOWS.parent.parent / "engine" / "Dockerfile"


@pytest.mark.skipif(
    not ENGINE_DOCKERFILE.exists(),
    reason="engine/ is deleted in the core-alone job, which is the point of it",
)
def test_the_engine_pins_every_npm_global_it_installs() -> None:
    """A `latest` global install is somebody else's publish deciding this image.

    `@anthropic-ai/claude-code@2.1.237` shipped `bin/claude.exe` as a shell
    stub rather than the ELF launcher, and turned a green build red with no
    change on our side. Unpinned also means two builds of the same commit are
    different images, which defeats a commit-identified `dev-<sha>` (NFR-C3).
    """
    text = ENGINE_DOCKERFILE.read_text("utf-8")
    installs = [
        line for line in text.splitlines() if "pkgs=" in line and "$pkgs" in line
    ]
    assert installs, "the npm global install shape changed; update this test"
    for line in installs:
        for token in line.split():
            if token.startswith(("@openai/", "@anthropic-ai/", "theclawbay")):
                assert "@${" in token or re.search(r"@\d", token), (
                    f"unpinned npm global: {token}"
                )


def test_latest_moves_only_on_a_semver_tag() -> None:
    """NFR-C3: a commit build never moves `latest`; a release does.

    `build.yml` already said `latest=false` outright. `release.yml` said
    nothing and inherited `latest=auto` from metadata-action's defaults —
    the right behaviour, but living in another project's source, where a
    change to that default would silently move this repository's `latest`.
    Both halves are now stated here.
    """
    release = (WORKFLOWS / "release.yml").read_text("utf-8")
    build = (WORKFLOWS / "build.yml").read_text("utf-8")
    assert release.count("latest=auto") >= 2, "both released images must state it"
    assert "latest=false" in build
    assert "latest=true" not in release, (
        "`latest=true` would tag a non-semver ref as latest"
    )


@pytest.mark.skipif(
    not ENGINE_DOCKERFILE.exists(),
    reason="engine/ is deleted in the core-alone job, which is the point of it",
)
def test_the_engine_takes_its_core_from_the_published_image() -> None:
    """#143: the private image contains the public one, verifiably.

    It used to run a second `uv sync` of the same source, so "the private path
    is the public path plus configuration" was a claim about two builds
    agreeing that nobody could check. Now it is a digest — and `CORE_IMAGE`
    must have no default, because a floating tag would silently decouple the
    two halves of a commit-identified build (NFR-C3).
    """
    text = ENGINE_DOCKERFILE.read_text("utf-8")
    assert "FROM ${CORE_IMAGE} AS core" in text
    assert "COPY --from=core /opt/vogt /opt/vogt" in text
    assert "AS core-build" not in text, "the engine must not build a second core"
    assert not re.search(r"^ARG CORE_IMAGE=", text, re.MULTILINE), (
        "CORE_IMAGE must not default; the caller pins the digest"
    )

    for name in ("build.yml", "release.yml"):
        wf = (WORKFLOWS / name).read_text("utf-8")
        assert "CORE_IMAGE=${{ env.IMAGE }}@${{ needs.image.outputs.digest }}" in wf, (
            f"{name} must pass the digest its own core job just pushed"
        )


def test_the_public_image_is_relocatable() -> None:
    """The derive above only works if `/opt/vogt` carries its interpreter.

    A venv links `bin/python` to whatever created it, so one built against the
    base image's system Python is a directory of scripts whose interpreter does
    not exist once copied into the engine's Ubuntu runtime — and the error
    names the script, not the missing interpreter.
    """
    text = (WORKFLOWS.parent.parent / "Dockerfile").read_text("utf-8")
    assert "UV_PYTHON_PREFERENCE=only-managed" in text
    assert "UV_PYTHON_INSTALL_DIR=/opt/vogt/python" in text
    assert "UV_PYTHON=3.13" in text, "pin the interpreter; unset means newest"
    assert "COPY --from=build --chown=root:root /opt/vogt /opt/vogt" in text
