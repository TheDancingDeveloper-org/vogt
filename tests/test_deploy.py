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
