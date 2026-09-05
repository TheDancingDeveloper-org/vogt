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
import sys
from pathlib import Path
from typing import Any

import pytest

REPO_ROOT = Path(__file__).resolve().parents[1]
DOCKERFILE = REPO_ROOT / "Dockerfile"
WORKFLOWS = REPO_ROOT / ".github" / "workflows"
RECEIPT_VALIDATOR = REPO_ROOT / "scripts" / "validate_deployment_receipt.py"


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
    # The estate's own pods are `dev` *and* `prod`: both run coding sessions,
    # so both need the AI clients and the mobile SDK. `main` is integration and
    # gets neither. This was `dev` alone until prod was stood up and came up
    # without claude, codex, flutter or theclawbay — a pod that cannot run the
    # thing the product exists to run.
    estate = (
        r"=\$\{\{ github\.ref == 'refs/heads/dev' "
        r"\|\| github\.ref == 'refs/heads/prod' \}\}"
    )
    # INSTALL_AI_CLIENTS is still a per-commit build arg on the merged image and
    # must reach both the candidate and the pushed build, or the image that is
    # smoke-tested is not the image that is published.
    wired = re.findall("INSTALL_AI_CLIENTS" + estate, text)
    assert len(wired) == 2, (
        "INSTALL_AI_CLIENTS must be passed to both the candidate and the "
        f"pushed build of engine/Dockerfile, for dev and prod; found {len(wired)}"
    )
    # "A pod with the clients but not the integrations is a third shape nobody
    # chose" was true of `build.yml`, whose refs are the estate's own pods, and
    # it stays the rule here. It is no longer true of the *release*: that shape
    # has since been chosen deliberately and is what the public generic AIO is
    # — clients, no estate integrations, `lean` pod base. See
    # `test_the_release_stack_is_the_generic_shape` below, which pins it.
    # The two files therefore disagree on purpose; this assertion is about
    # `build.yml` alone and must not be generalised to both.
    for arg in ("INSTALL_CADASTRE_MCP", "INSTALL_THECLAWBAY"):
        assert len(re.findall(arg + estate, text)) == 2, (
            f"{arg} must follow the same estate rule as INSTALL_AI_CLIENTS; an "
            "estate pod with the clients but not the integrations is a shape "
            "nobody chose"
        )
    # Flutter is no longer a build arg of the merged image (#184): it is the
    # difference between the two published pod-base variants, selected once for
    # the whole build. The estate refs get `full` (Flutter), `main` gets `lean`.
    assert (
        "variant: ${{ (github.ref == 'refs/heads/dev' || "
        "github.ref == 'refs/heads/prod') && 'full' || 'lean' }}" in text
    ), "the pod variant must be `full` for dev and prod, `lean` for main"
    # The smoke loop must run on every ref that installed them, or the tools
    # are proven on one estate pod and merely hoped for on the other.
    assert "ESTATE_IMAGE: ${{ github.ref == 'refs/heads/dev' || " in text, (
        "the AI-client smoke test must gate on the same estate rule"
    )
    assert "INSTALL_FLUTTER" not in text, (
        "Flutter moved to the pod variant (#184); INSTALL_FLUTTER is no longer "
        "a build arg of the merged image"
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


def test_the_estate_stack_publishes_to_its_own_package() -> None:
    """`vogt-stack` is public, and GHCR visibility is per package, not version.

    Making the generic AIO public published *every* version in the repository,
    including 173 `dev-`/`prod-` images that are the maintainer's own pods —
    tailscale, infisical, the step CLI, the Cadastre MCP bridge, theclawbay.
    No credential was exposed (the build passes none as a build arg and mounts
    none), but a stranger being able to pull and run the estate's pod image is
    not a property anybody chose.

    `build.yml`'s own comment had already named the failure mode for a related
    reason — "one repository with two kinds of tag in it is how somebody
    eventually pins the wrong one" — and visibility turned that from a pinning
    hazard into a disclosure one. So the estate stream publishes to
    `vogt-stack-estate` and the public stream to `vogt-stack`, which makes the
    boundary a package rather than a convention.

    `release.yml` is deliberately not parametrised: a release is always the
    public generic artefact, so its `STACK_IMAGE` must stay unconditional.
    """
    build = (WORKFLOWS / "build.yml").read_text(encoding="utf-8")
    assert "vogt-stack${{" in build, (
        "build.yml's STACK_IMAGE must select the estate package for dev/prod; a "
        "literal `vogt-stack` there publishes estate pods to the public package"
    )
    assert "'-estate' || ''" in build, (
        "the estate suffix must be chosen by ref, not hardcoded either way"
    )
    # The same estate rule the build args follow, so one ref cannot drift from
    # the other and publish a `prod` pod to the public package.
    stack_line = next(
        line for line in build.splitlines() if line.strip().startswith("STACK_IMAGE:")
    )
    for ref in ("refs/heads/dev", "refs/heads/prod"):
        assert ref in stack_line, f"the estate package rule must name {ref}"

    release = (WORKFLOWS / "release.yml").read_text(encoding="utf-8")
    release_line = next(
        line for line in release.splitlines() if line.strip().startswith("STACK_IMAGE:")
    )
    assert "${{" not in release_line, (
        "a release is always the public generic artefact; a conditional "
        "STACK_IMAGE in release.yml could publish one to the estate package"
    )
    assert "-estate" not in release_line


def test_the_deploy_path_resolves_the_package_the_build_published_to() -> None:
    """Splitting the packages splits the deploy path too, or dev deploys wrong.

    `deploy-dev.yml` resolves `<image>:dev-<sha>` and `scripts/deploy_dev.py`
    rewrites the image line in the deployed `estate.overlay.yml`. Both named
    `vogt-stack` while `build.yml` published there. Now that `dev` and `prod`
    publish to `vogt-stack-estate`, a stale name here does not fail loudly — the
    public package has no `dev-<sha>` tag, so it errors at best, and at worst
    resolves something unrelated and repins vogt-dev onto a digest from the
    wrong package.

    `deploy-production.yml` is deliberately excluded: it takes a *release tag*
    and its cosign identity requires `release.yml@refs/tags/v[0-9]`, so it
    deploys the signed public release artefact. That one stays on `vogt-stack`,
    and pointing it at the estate package would be the same bug mirrored.
    """
    dev_wf = (WORKFLOWS / "deploy-dev.yml").read_text(encoding="utf-8")
    assert "vogt-stack-estate" in dev_wf, (
        "deploy-dev.yml must resolve the estate package that build.yml's dev "
        "branch publishes to"
    )
    # Executable lines only: the comment above the change explains what the
    # public package is *not* used for here, and naming it is the point.
    for line in dev_wf.splitlines():
        if line.lstrip().startswith("#"):
            continue
        if "vogt-stack" in line and "vogt-stack-estate" not in line:
            raise AssertionError(
                f"deploy-dev.yml still names the public package: {line.strip()}"
            )

    helper = (REPO_ROOT / "scripts" / "deploy_dev.py").read_text(encoding="utf-8")
    assert 'STACK_IMAGE = "ghcr.io/thedancingdeveloper-org/vogt-stack-estate"' in (
        helper
    ), "scripts/deploy_dev.py must rewrite estate.overlay.yml with the estate package"

    prod_wf = (WORKFLOWS / "deploy-production.yml").read_text(encoding="utf-8")
    assert "vogt-stack-estate" not in prod_wf, (
        "production deploys the signed public release, not an estate build"
    )

    # A new package that no retention policy names accumulates every dev build
    # for ever; the public one is already listed.
    retention = (WORKFLOWS / "ghcr-retention.yml").read_text(encoding="utf-8")
    assert "vogt-stack-estate" in retention, (
        "the estate package must be covered by the GHCR retention policy"
    )


def test_the_release_stack_is_the_generic_shape() -> None:
    """The release stack is the public AIO: agent CLIs, no estate integrations.

    The release used to be the inverse of what it is published for — no
    `INSTALL_AI_CLIENTS` line at all (so `engine/Dockerfile`'s `false` default
    won) and `INSTALL_CADASTRE_MCP=true`. It shipped the maintainer's estate
    MCP to strangers while withholding the two CLIs the product exists to run.
    Nothing was red, because nothing asserted either half.

    So this pins the shape rather than trusting the args to stay written: a
    release carries `claude` and `codex`, and carries neither cadastre nor
    theclawbay. `build.yml` keeps the estate rule and is asserted separately in
    `test_the_dev_image_build_turns_the_ai_clients_on` — the two files differ on
    purpose, which is why neither test is written over both.
    """
    text = (WORKFLOWS / "release.yml").read_text(encoding="utf-8")
    # Both build steps, as ever: the candidate is what the smoke test runs and
    # the push is what consumers pull, and an arg on one but not the other means
    # the tested image is not the published one.
    for arg in ("INSTALL_AI_CLIENTS=true", "INSTALL_CADASTRE_MCP=false"):
        assert text.count(arg) == 2, (
            f"release.yml must pass {arg} to both the candidate and the pushed "
            f"stack build; found {text.count(arg)}"
        )
    assert "INSTALL_CADASTRE_MCP=true" not in text, (
        "a release must not carry the maintainer's estate MCP: it is the "
        "public artifact, and cadastre means nothing outside one estate"
    )
    assert "INSTALL_THECLAWBAY" not in text, (
        "theclawbay is an estate integration; a release leaves it at the "
        "Dockerfile's false default rather than naming it"
    )
    # `vogt-verify-agent-clis` is not this proof and never was: it returns 0
    # when the binaries are absent (it detects a persisted-$HOME copy shadowing
    # the image's, which is a different question). Only running them proves
    # they are there — the #23 lesson, applied to the release stream.
    smoke = text[text.index("- name: both halves run") :]
    smoke = smoke[: smoke.index("- uses:")]
    loop = re.search(r"for tool in ([^;]+); do", smoke)
    assert loop, (
        "the release stack's smoke test must run the CLIs it now ships, not "
        "merely check them for shadowing (NFR-Q7)"
    )
    probed = set(loop.group(1).split())
    owed = {"claude", "codex"}
    assert owed <= probed, (
        "the release image must be asked for the clients it carries; missing: "
        f"{sorted(owed - probed)}"
    )
    # `flutter` and `theclawbay` belong to the `full` pod base and the estate
    # build. Probing for them here would fail a correct release.
    assert not probed & {"flutter", "theclawbay"}, (
        "a release is the `lean` pod base with no estate integrations; probing "
        f"for {sorted(probed & {'flutter', 'theclawbay'})} asks it for what it "
        "deliberately does not carry"
    )


@pytest.mark.parametrize("workflow", ["build.yml", "release.yml"])
def test_no_build_args_block_contains_a_comment(workflow: str) -> None:
    """`#` in a `|` block scalar is content, not a comment.

    `build-args: |` is a literal block, and build-push-action parses it without
    a comment option, so an explanatory note written inside the block reaches
    buildx as a build arg named `#`. `build.yml` carries a comment saying so,
    directly above the block it applies to — and the note was written there
    because the mistake had already been made once. It was then made again
    while adding the release's generic build args, which is the point at which
    a comment stops being the right tool.

    Notes belong above the `build-args:` key, where YAML comments are comments.
    """
    text = (WORKFLOWS / workflow).read_text(encoding="utf-8")
    for block in re.finditer(
        r"^(\s+)build-args: \|\n((?:\1  .*\n|\n)*)", text, re.MULTILINE
    ):
        offending = [
            line for line in block.group(2).splitlines() if line.strip().startswith("#")
        ]
        assert not offending, (
            f"{workflow}: comment inside a build-args block scalar reaches "
            f"buildx as a build arg; move it above the key: {offending}"
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


def test_every_third_party_action_is_pinned_to_a_commit() -> None:
    """A mutable action tag is executable supply-chain input.

    The runner-policy workflow repeats this check in CI; keeping the same
    assertion in the Python suite catches a newly added workflow before it
    reaches GitHub at all. Local reusable workflows are intentionally exempt:
    they are resolved from this checkout and therefore move with the commit
    being tested.
    """
    action = re.compile(r"^\s*(?:-\s*)?uses:\s*([^@\s]+)@([^\s#]+)")
    for path in _workflow_files():
        for number, line in enumerate(path.read_text("utf-8").splitlines(), start=1):
            match = action.search(line)
            if not match or match.group(1).startswith("./"):
                continue
            assert re.fullmatch(r"[0-9a-fA-F]{40}", match.group(2)), (
                f"{path.name}:{number}: third-party action is not SHA-pinned: "
                f"{line.strip()}"
            )


def test_codeql_covers_each_implementation_language() -> None:
    """OSR-08: code scanning follows the actual language boundary."""
    workflow = (WORKFLOWS / "codeql.yml").read_text(encoding="utf-8")
    assert "runs-on: [self-hosted]" in workflow
    assert "security-events: write" in workflow
    assert "build-mode: none" in workflow
    assert "security-extended" in workflow
    assert "- python" in workflow
    assert "- javascript-typescript" in workflow
    assert "- rust" in workflow
    assert re.search(r"actions/setup-node@[0-9a-f]{40}", workflow)
    assert "if: matrix.language == 'javascript-typescript'" in workflow
    assert re.search(r"github/codeql-action/init@[0-9a-f]{40}", workflow)
    assert re.search(r"github/codeql-action/analyze@[0-9a-f]{40}", workflow)


def test_rust_dependency_audit_is_a_fatal_ci_gate() -> None:
    """OSR-01: RustSec findings are triaged and future drift fails CI."""
    workflow = (WORKFLOWS / "ci.yml").read_text(encoding="utf-8")
    step = workflow.split("      - name: audit Rust dependencies", 1)[1].split(
        "\n      - uses:", 1
    )[0]
    assert "cargo install --locked cargo-audit --version 0.22.2" in step
    assert "cargo audit --no-fetch --db" in step
    assert "--deny warnings" in step
    assert "--ignore RUSTSEC-2023-0071" in step
    assert "has no fixed rsa release" in step
    assert "signs ES256" in step
    # The advisory database is cloned anonymously so the runner's injected
    # github.com credentials cannot 401 cargo-audit's own fetch.
    assert "RustSec/advisory-db.git" in step
    assert "GIT_CONFIG_GLOBAL=/dev/null" in step


def test_javascript_dependency_audits_are_fatal_ci_gates() -> None:
    """OSR-01: clean pnpm receipts stay clean after this review."""
    workflow = (WORKFLOWS / "ci.yml").read_text(encoding="utf-8")
    assert "audit the PWA's dependencies" in workflow
    assert "audit the shell's dependencies" in workflow
    assert workflow.count("run: pnpm audit") == 2


def test_buildkit_cache_topology_is_operator_configuration() -> None:
    """OSR-11: public workflows consume an opaque cache endpoint.

    release.yml is the exception (#514): a signed release builds cold and no
    longer imports the shared cache at all, so it references neither the cache
    var nor the registry — but it must still never carry the hardcoded IP.
    """
    for name in ("build.yml", "pod-base.yml"):
        workflow = (WORKFLOWS / name).read_text(encoding="utf-8")
        assert "VOGT_BUILDKIT_CACHE_REGISTRY" in workflow
        assert "192.168.1.75:5500" not in workflow
    release = (WORKFLOWS / "release.yml").read_text(encoding="utf-8")
    assert "192.168.1.75:5500" not in release


def test_ghcr_retention_uses_a_pinned_central_policy() -> None:
    """OSR-01: retention executes reviewed policy, not a moving/dead workflow."""
    workflow = (WORKFLOWS / "ghcr-retention.yml").read_text(encoding="utf-8")
    assert "runs-on: [self-hosted, publish]" in workflow
    assert "repository: TheDancingDeveloper-org/github-policy" in workflow
    assert re.search(r"\n\s+ref: [0-9a-f]{40}\n", workflow)
    assert "@main" not in workflow
    assert "ghcr_retention.py" in workflow
    assert 'if [[ "$APPLY" == true ]]' in workflow
    assert "default: false" in workflow
    assert re.search(r"actions/upload-artifact@[0-9a-f]{40}", workflow)


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


def test_the_base_image_is_pinned_by_digest() -> None:
    """A floating base tag makes the pinned image a pin of nothing.

    `DEPLOYMENT.md` §2.2 requires Vogt's published image to be digest-pinned
    in the ops repo. If the base it is assembled from can change under a tag,
    that pin describes a build input that is not fixed. It is also the
    `update_automation_gap` Vogt reports on other people's repositories
    (FR-D6) — hard to justify raising for others and not for itself.
    """
    dockerfile = _without_comments(DOCKERFILE.read_text(encoding="utf-8"))
    match = re.search(r"^ARG PYTHON_IMAGE=\S+@(sha256:[0-9a-f]{64})$", dockerfile, re.M)
    assert match, "PYTHON_IMAGE must be a digest-pinned public default"
    assert dockerfile.count("FROM ${PYTHON_IMAGE}") == 2


def test_both_build_stages_use_the_same_base() -> None:
    """A digest bump that lands on one stage and not the other would build
    the runtime from a different image than the one the venv was resolved
    against — the kind of skew that only shows up at runtime."""
    dockerfile = _without_comments(DOCKERFILE.read_text(encoding="utf-8"))
    assert dockerfile.count("FROM ${PYTHON_IMAGE}") == 2


# ── The merged stack (NFR-D11, NFR-D12, NFR-C6) ───────────────────────────
#
# Until this section existed, no test in the repository read
# `deploy/vogt-stack.compose.yml` or either `stack-image` job — which is how
# the compose file kept a placeholder digest of zeros through four stages
# while the documentation around it described a pinned image. The artefact
# these assert on has now been built, smoke-tested, signed and published by
# CI, so every claim below is about something that exists.

ENGINE_PORT = "8910"


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


@pytest.mark.skipif(
    not (REPO_ROOT / "engine" / "Dockerfile").is_file(),
    reason="the core-alone job (NFR-Q6) deletes engine/; this reads its Dockerfile",
)
@pytest.mark.parametrize("workflow", ["build.yml", "release.yml"])
def test_the_pwa_is_built_before_the_merged_image(workflow: str) -> None:
    """`rust-embed` reads `web/dist/` at compile time, and it is stage 1.

    Two properties, after #184 moved the build inside the image:

      * the bundle is built by stage 1 of `engine/Dockerfile` (`web-build`),
        which finishes before stage 2 (`server-build`) embeds it; and
      * the runner deliberately does **not** build it. A runner-side
        `pnpm build` produced a `web/dist` that `engine/Dockerfile.dockerignore`
        drops from the context, so it never reached the image — a load-bearing
        looking step that read as protection and did nothing.
    """
    dockerfile = (REPO_ROOT / "engine" / "Dockerfile").read_text("utf-8")
    assert "AS web-build" in dockerfile and "AS server-build" in dockerfile
    assert dockerfile.index("AS web-build") < dockerfile.index("AS server-build"), (
        "the web bundle (stage 1) must be built before the server (stage 2) "
        "that embeds it at compile time"
    )
    assert "COPY --from=web-build /app/web/dist" in dockerfile, (
        "stage 2 must embed the bundle stage 1 built, which is what makes the "
        "ordering load-bearing"
    )

    # Comments stripped: the workflow's own note explains the removed step by
    # name, and a raw search would find `pnpm build` in the sentence about not
    # running it — the mistake `_without_comments` exists for.
    job = _without_comments((WORKFLOWS / workflow).read_text(encoding="utf-8"))
    job = job[job.index("  stack-image:") :]
    assert "pnpm build" not in job, (
        f"{workflow}: the PWA must not be rebuilt on the runner (#184); stage 1 "
        "of engine/Dockerfile builds the bundle the image embeds, and "
        "engine/Dockerfile.dockerignore drops any host-built web/dist"
    )


@pytest.mark.skipif(
    not (REPO_ROOT / "engine" / "Dockerfile").is_file(),
    reason="the core-alone job (NFR-Q6) deletes engine/; this reads its Dockerfile",
)
def test_web_frozen_install_sees_the_pnpm_overrides() -> None:
    """The frozen install in stage 1 needs `web/pnpm-workspace.yaml`.

    pnpm 11 keeps `overrides` in `pnpm-workspace.yaml`, and the lockfile is
    resolved against them. If the selective `COPY` before the frozen install
    omits that file, pnpm sees no overrides while the lockfile encodes them and
    aborts with ERR_PNPM_LOCKFILE_CONFIG_MISMATCH — which broke the merged
    image build even though the `ci` job (installing in the full checkout)
    passed. Guard that the file is copied before the install, not only by the
    later full `COPY web/ ./`.
    """
    dockerfile = (REPO_ROOT / "engine" / "Dockerfile").read_text("utf-8")
    copy_idx = dockerfile.index("COPY web/package.json")
    install_idx = dockerfile.index("pnpm install --frozen-lockfile")
    workspace_idx = dockerfile.index("web/pnpm-workspace.yaml")
    assert copy_idx < install_idx, "the selective COPY must precede the install"
    assert workspace_idx < install_idx, (
        "web/pnpm-workspace.yaml must be copied before `pnpm install "
        "--frozen-lockfile`, or the overrides/lockfile mismatch aborts the build"
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
    smoke_engine = job.index("--entrypoint vogt-engine")
    push = job.index("push: true")
    assert max(smoke_vogt, smoke_engine) < push, (
        f"{workflow}: smoke-test the candidate, then push it"
    )


@pytest.mark.parametrize("name", ["image", "stack-image"])
def test_the_three_streams_are_kept_apart(name: str) -> None:
    """NFR-D12: no image of one stream can be mistaken for another's.

    `dev` + `dev-<sha>`, `sha-<commit>`, `prod-<sha>` — and no tag any of
    them can move, so "which build is that?" stays answerable and an image
    cannot be picked up by something following a different stream.

    Every rule is an **explicit equality** on the ref, and that is the part
    worth pinning. `sha-` was once `!= 'refs/heads/dev'` — "anything that is
    not dev" — which was correct while two branches built and silently wrong
    the moment a third did: `prod` would have been handed a plain `sha-` tag
    and put production images into main's stream. A negation here is a bug
    that only appears when somebody adds a branch, so the test refuses one
    outright rather than checking today's three answers.

    Both images, because the requirement says both and the merge added the
    second: a rule kept by the core-only image and dropped by the merged one
    would leave the artefact the merge exists for as the unlabelled stream.
    """
    raw = (WORKFLOWS / "build.yml").read_text(encoding="utf-8")
    start = raw.index(f"\n  {name}:\n")
    rest = raw[start + 1 :]
    end = re.search(r"\n  [a-z][a-z-]*:\n", rest)
    job = rest[: end.start()] if end else rest
    assert (
        "type=sha,format=long,prefix=dev-,enable=${{ github.ref == 'refs/heads/dev' }}"
        in job
    )
    assert "type=raw,value=dev,enable=${{ github.ref == 'refs/heads/dev' }}" in job
    assert "type=sha,enable=${{ github.ref == 'refs/heads/main' }}" in job
    assert (
        "type=sha,format=long,prefix=prod-,enable=${{ github.ref == "
        "'refs/heads/prod' }}" in job
    )
    assert "github.ref !=" not in job, (
        "tag rules name the ref they belong to; a negation silently captures "
        "the next branch somebody adds"
    )
    assert "type=semver" not in job, "a build must not assign a version"
    assert "value=latest" not in job, "a build must not move an alias"


def test_every_building_branch_has_a_tag_rule() -> None:
    """A branch that builds with no rule of its own publishes nothing, or
    worse, borrows another stream's tag."""
    raw = (WORKFLOWS / "build.yml").read_text(encoding="utf-8")
    on = raw[raw.index("on:") : raw.index("permissions:")]
    branches = re.findall(r"^      - ([a-z][a-z0-9-]*)$", on, re.MULTILINE)
    assert set(branches) == {"main", "dev", "prod"}, branches
    for branch in branches:
        assert f"github.ref == 'refs/heads/{branch}'" in raw, (
            f"{branch} builds but no tag rule names it"
        )


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


def test_the_voice_sidecar_is_built_signed_and_published_generically() -> None:
    """The bundled voice sidecar (#565) is published the way the stack is.

    A signed, digest-pinned image with provenance and an SBOM, from both the
    per-commit build and the release. Unlike `vogt-stack` it has no estate
    variant — the sidecar carries no estate integration — so it always names
    the one public package, from every branch.
    """
    build = (WORKFLOWS / "build.yml").read_text(encoding="utf-8")
    release = (WORKFLOWS / "release.yml").read_text(encoding="utf-8")
    for raw in (build, release):
        assert "VOICE_IMAGE: ghcr.io/thedancingdeveloper-org/vogt-voice\n" in raw, (
            "voice publishes to one public package, never an estate variant"
        )
        assert "-estate" not in raw.split("VOICE_IMAGE:")[1].split("\n")[0]
        job = raw[raw.index("  voice-image:") :]
        # A real artefact gate: built, signed, with provenance and an SBOM.
        assert "provenance: true" in job and "sbom: true" in job
        assert 'cosign sign --yes "${VOICE_IMAGE}@${DIGEST}"' in job
        # The run gate is a full round trip through the baked models, not just
        # that the image assembles — a green build of a mute sidecar is exactly
        # the failure #565 is about.
        assert "/v1/audio/transcriptions" in job
        assert "/v1/audio/speech" in job


# ── The build cache and the pod-toolchain split (#184) ─────────────────────
#
# `dev`'s merged image recompiled everything from scratch on every push:
# ~34 minutes, of which 22 were a dev-pod toolchain no commit touched and the
# rest a Rust build with no layer cache. Two changes address it — a BuildKit
# layer cache on the estate's own local-registry on Node B (not GHCR: the
# runners are on Node B, so the cache stays on the host while GHCR is a WAN
# hop, #129), and the toolchain moved to `engine/Dockerfile.pod`, built once by
# `pod-base.yml` and consumed by digest. These assert the shape so a later edit
# cannot quietly send the cache to GHCR or fold the toolchain back inline.


def test_the_core_and_stack_cache_streams_do_not_collide() -> None:
    """#184: keyed per image and per ref so no stream evicts another.

    core and stack are different layer graphs; dev and main fork on
    INSTALL_AI_CLIENTS. One shared cache tag would have the streams evicting
    each other and none hitting, which is the failure a cache is meant to end.
    """
    raw = (WORKFLOWS / "build.yml").read_text("utf-8")
    assert "${{ env.CACHE_IMAGE }}:core-${{ github.ref_name }}" in raw, (
        "the core image caches under a per-ref core tag"
    )
    assert "${{ env.CACHE_IMAGE }}:stack-${{ github.ref_name }}" in raw, (
        "the merged image caches under a per-ref stack tag, distinct from core"
    )


@pytest.mark.skipif(
    not (REPO_ROOT / "engine" / "Dockerfile").is_file(),
    reason="the core-alone job (NFR-Q6) deletes engine/; this reads Dockerfile.pod",
)
def test_the_dev_pod_toolchain_is_split_into_its_own_base() -> None:
    """#184: the merged image builds FROM a prebuilt pod base, not inline.

    22 of the merged image's 34 build minutes were a toolchain no commit
    touched. It now lives in `engine/Dockerfile.pod`, published by
    `pod-base.yml`, and the merged image's runtime stage is
    `FROM ${POD_BASE_IMAGE}`.
    """
    assert (REPO_ROOT / "engine" / "Dockerfile.pod").is_file(), (
        "the pod toolchain lives in engine/Dockerfile.pod"
    )
    assert (WORKFLOWS / "pod-base.yml").is_file(), (
        "pod-base.yml builds and publishes the pod base"
    )
    engine = (REPO_ROOT / "engine" / "Dockerfile").read_text("utf-8")
    assert "FROM ${POD_BASE_IMAGE}" in engine, (
        "the merged image's runtime stage builds on the prebuilt pod base"
    )
    # Sentinels for the heavy toolchain that must have moved out of the
    # per-commit image and into the base.
    pod = (REPO_ROOT / "engine" / "Dockerfile.pod").read_text("utf-8")
    for tool in ("rustup.rs", "sdkmanager", "sway swaybg"):
        assert tool not in engine, (
            f"{tool!r} is toolchain that belongs in engine/Dockerfile.pod, not "
            "the per-commit merged image (#184)"
        )
        assert tool in pod, f"the pod base must install {tool!r}"


@pytest.mark.parametrize("workflow", ["build.yml", "release.yml"])
def test_both_image_workflows_supply_the_pod_base_by_digest(workflow: str) -> None:
    """#184: pod-base is a prerequisite and its digest is what the build uses.

    A tag would silently decouple the merged image from the toolchain it was
    tested against; the workflow passes the digest `pod-base` resolved, the same
    standard `CORE_IMAGE` is held to (#143).
    """
    raw = (WORKFLOWS / workflow).read_text("utf-8")
    assert "uses: ./.github/workflows/pod-base.yml" in raw, (
        f"{workflow} builds or reuses the pod base as a prerequisite"
    )
    assert "POD_BASE_IMAGE=${{ needs.pod-base.outputs.image }}" in raw, (
        f"{workflow} passes the resolved pod-base digest to engine/Dockerfile"
    )


# ── What a session's agent is told about where Vogt is (FR-E5) ────────────

MCP_BOOTSTRAP = REPO_ROOT / "engine" / "deploy" / "mcp-bootstrap.sh"
VOGT_MCP_WRAPPER = REPO_ROOT / "engine" / "deploy" / "vogt-mcp-auth.sh"
AGENT_AUTH = REPO_ROOT / "engine" / "deploy" / "agent-auth.sh"

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
        # The Cadastre endpoint has no baked default any more (#205): the
        # wrapper skips cleanly without it, so name a dummy so this test
        # exercises the *configured* path it is about. Left registration off
        # (CADASTRE_MCP_ENABLED unset) so the bootstrap stays Vogt-only, as
        # before.
        "CADASTRE_MCP_URL": "https://cadastre.invalid/mcp",
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
@pytest.mark.parametrize(
    ("wrapper", "bridge", "token_env"),
    [
        (VOGT_MCP_WRAPPER, "vogt-mcp-remote", "VOGT_HTTP_TOKEN"),
        (CADASTRE_MCP_WRAPPER, "cadastre-mcp-remote", "CADASTRE_HTTP_TOKEN"),
    ],
    ids=["vogt", "cadastre"],
)
def test_a_token_already_in_the_env_skips_the_broker(
    tmp_path: Path, wrapper: Path, bridge: str, token_env: str
) -> None:
    """#559/#560: a usable credential must not be routed through the broker.

    The reported failure: a coding session holds a working token
    (`VOGT_HTTP_TOKEN` split from its `VOGT_SESSION_ID` in v0.5.1 for Vogt;
    `CADASTRE_HTTP_TOKEN` for Cadastre, which had no skip-path at all) but no
    Infisical creds, so the wrapper's mandatory `mydevenv2-agent-auth` detour
    `die`s (exit 1) and every client sees CONNECTION_CLOSED — despite the token
    connecting fine when the bridge is run directly.

    Modelled by making the broker stub fail the way the real one does without
    Infisical creds: the wrapper must reach the bridge without ever invoking it.
    """
    script, bin_dir = _wrapper_sandbox(tmp_path, wrapper, bridge)
    # Replace the cooperative broker stub with one that fails the way the real
    # `mydevenv2-agent-auth` does when it has no Infisical creds, and records
    # that it was reached at all.
    broker_touched = tmp_path / "broker-was-invoked"
    (bin_dir / "mydevenv2-agent-auth").write_text(
        "#!/usr/bin/env bash\n"
        f'touch "{broker_touched}"\n'
        "printf 'mydevenv2-agent-auth: INFISICAL_API_URL is not set\\n' >&2\n"
        "exit 1\n",
        encoding="utf-8",
    )
    (bin_dir / "mydevenv2-agent-auth").chmod(0o755)

    home = tmp_path / "home"
    home.mkdir()
    environment = {
        "PATH": f"{bin_dir}:/usr/bin:/bin",
        "HOME": str(home),
        "TMPDIR": str(tmp_path),
        "MYDEVENV2_CADASTRE_SRC": str(tmp_path / "absent"),
        "MYDEVENV2_VOGT_SRC": str(tmp_path / "absent"),
        "CADASTRE_MCP_URL": "https://cadastre.invalid/mcp",
        # The one thing this session has and the broker path cannot use: a
        # working token, with no VOGT_SESSION_ID pairing it (the v0.5.1 split).
        token_env: "a-token-that-connects",
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
    assert not broker_touched.exists(), (
        "a usable token was in the env, so the wrapper must run the bridge "
        "directly and never enter the broker that hard-fails without Infisical"
    )
    frames = [line for line in completed.stdout.splitlines() if line.strip()]
    assert frames, "the bridge's own answer should be there — it connected"
    assert json.loads(frames[0])["jsonrpc"] == "2.0"


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


# `agent-auth run --` under stubs: the real launch path. `main run` calls
# `load_agent_environment` and then execs the command, so a `bash -c` that
# dumps its environment reads exactly what the boundary #511/#566 is about
# hands a session. The identity, the login and every secret fetch are stubbed;
# nothing here reaches Infisical.
_AGENT_AUTH_STUB_PRELUDE = f"""
source {AGENT_AUTH!s}
require_command() {{ :; }}
mint_access_token() {{ printf 'tok'; }}
get_secret() {{ printf 'val-%s' "$3"; }}
"""

_AGENT_AUTH_STUB_ENV = {
    "PATH": "/usr/bin:/bin",
    "INFISICAL_API_URL": "https://vault.invalid",
    "INFISICAL_CLIENT_ID": "cid",
    "INFISICAL_CLIENT_SECRET": "csec",
    # No Cadastre, and skip the real mcp-bootstrap binary the launch would run.
    "CADASTRE_MCP_ENABLED": "0",
    "MYDEVENV2_AUTO_CADASTRE_MCP": "0",
}


@needs_engine
def test_the_strip_leaves_a_breadcrumb_for_the_brokered_model() -> None:
    """#566: the strip was right but silent, and silence read as 'missing'.

    #511 drops the machine identity before the launched shell, so an agent's
    own `infisical login` finds INFISICAL_CLIENT_ID empty — and the natural,
    wrong conclusion is that the credential does not exist rather than that it
    was withheld by design. The fix is a names-only breadcrumb that survives
    the strip: `AGENT_AUTH_MODE=brokered` and `AGENT_AUTH_GRANTED`, the names
    (never the values) of the credentials the session was actually granted.

    Asserted across the real exec boundary — a child `bash -c` that prints its
    own environment — because that, not the helper's own shell, is what a
    session inherits.
    """
    # `env`, not a hand-quoted printf: the child's own environment is exactly
    # what a session inherits, and parsing it here avoids a nested-quoting trap.
    script = _AGENT_AUTH_STUB_PRELUDE + "main run -- env"
    completed = subprocess.run(
        ["bash", "-c", script],
        capture_output=True,
        text=True,
        env={
            **_AGENT_AUTH_STUB_ENV,
            "ENGINE_AGENT_AUTH_SECRETS": (
                "GHTOK proj gh-secret\nOTHER proj other optional\n"
            ),
            "ENGINE_AGENT_AUTH_GH_TOKEN_FROM": "GHTOK",
        },
        check=False,
    )
    assert completed.returncode == 0, completed.stderr
    child_env: dict[str, str] = {}
    for line in completed.stdout.splitlines():
        key, _, value = line.partition("=")
        child_env[key] = value
    # The breadcrumb crossed the boundary...
    assert child_env.get("AGENT_AUTH_MODE") == "brokered"
    granted = child_env.get("AGENT_AUTH_GRANTED", "").split()
    # ...and names exactly the credentials the session was granted, GH_TOKEN
    # included, and nothing it was not.
    assert {"GHTOK", "OTHER", "GH_TOKEN"} <= set(granted), granted
    assert "INFISICAL_CLIENT_ID" not in granted
    assert "AGENT_AUTH_MODE" not in granted
    # ...while the identity and the manifest did not: the whole point of #511.
    assert "INFISICAL_CLIENT_ID" not in child_env
    assert "INFISICAL_API_URL" not in child_env, (
        "the secrets-manager URL is dropped too, not signposted"
    )
    assert "ENGINE_AGENT_AUTH_SECRETS" not in child_env
    # ...and the one manifest credential that is aliased for git/gh survived.
    assert child_env.get("GH_TOKEN") == "val-gh-secret"


@needs_engine
def test_a_gh_token_alias_that_names_nothing_fails_the_helper() -> None:
    """#566 note: ENGINE_AGENT_AUTH_GH_TOKEN_FROM naming a non-manifest var.

    The alias only fires while iterating manifest entries, so a name that is
    not among them aliased no GH_TOKEN and said nothing — every later `gh`
    call then failed as if unauthenticated, a long way from the typo. It must
    fail loudly instead, naming the mismatch; under ENGINE_AGENT_AUTH_REQUIRED
    the entrypoint runs this at boot so it surfaces before a session starts.
    """
    script = _AGENT_AUTH_STUB_PRELUDE + "main run -- true"
    completed = subprocess.run(
        ["bash", "-c", script],
        capture_output=True,
        text=True,
        env={
            **_AGENT_AUTH_STUB_ENV,
            "ENGINE_AGENT_AUTH_SECRETS": "GHTOK proj gh-secret\n",
            "ENGINE_AGENT_AUTH_GH_TOKEN_FROM": "TYPO_NOT_IN_MANIFEST",
        },
        check=False,
    )
    assert completed.returncode != 0
    assert "TYPO_NOT_IN_MANIFEST" in completed.stderr
    assert "is not a variable in ENGINE_AGENT_AUTH_SECRETS" in completed.stderr


@needs_engine
def test_the_breadcrumb_is_published_before_the_identity_is_stripped() -> None:
    """Ordering is the whole property: a breadcrumb exported after the `unset`
    would still be there, but one exported before proves the two live in the
    same block and cannot drift apart. And the API URL stays *in* the unset —
    it is dropped, never kept as a signpost (#566 chose names-only)."""
    body = _without_comments(AGENT_AUTH.read_text(encoding="utf-8"))
    assert "export AGENT_AUTH_MODE=brokered" in body
    assert 'export AGENT_AUTH_GRANTED="$AGENT_AUTH_GRANTED_VARS"' in body
    mode_at = body.index("export AGENT_AUTH_MODE=brokered")
    unset_at = body.index("unset INFISICAL_CLIENT_ID")
    assert mode_at < unset_at, "the breadcrumb must be published before the strip"
    assert "INFISICAL_API_URL" in body[unset_at:], (
        "the API URL is dropped, not signposted"
    )


@needs_engine
def test_an_ondemand_entry_is_declared_but_never_exported_at_launch() -> None:
    """#568: `ondemand` is the flag that makes a late fetch mean something.

    Every other manifest entry is already in the session's environment for
    the whole session — `env`, a prompt-injected `printenv`, a crash dump.
    An `ondemand` entry is declared for sessions but not resolved at launch,
    so it exists in the session only at the moment it is asked for, and that
    moment is audited. The session is told the name so it knows what it may
    ask for, and told it apart from what it already holds.
    """
    script = _AGENT_AUTH_STUB_PRELUDE + "main run -- env"
    completed = subprocess.run(
        ["bash", "-c", script],
        capture_output=True,
        text=True,
        env={
            **_AGENT_AUTH_STUB_ENV,
            "ENGINE_AGENT_AUTH_SECRETS": (
                "LAUNCHED proj launched\nLATER proj later ondemand\n"
            ),
        },
        check=False,
    )
    assert completed.returncode == 0, completed.stderr
    child_env = dict(line.partition("=")[::2] for line in completed.stdout.splitlines())
    assert child_env.get("LAUNCHED") == "val-launched"
    assert "LATER" not in child_env, "ondemand is not exported at launch"
    assert child_env.get("AGENT_AUTH_GRANTED", "").split() == ["LAUNCHED"]
    assert child_env.get("AGENT_AUTH_ONDEMAND", "").split() == ["LATER"]


@needs_engine
def test_the_gh_alias_cannot_name_an_ondemand_entry() -> None:
    """git/gh need their identity at launch; an alias to a name that is only
    resolved later would leave GH_TOKEN empty with a green launch."""
    script = _AGENT_AUTH_STUB_PRELUDE + "main run -- true"
    completed = subprocess.run(
        ["bash", "-c", script],
        capture_output=True,
        text=True,
        env={
            **_AGENT_AUTH_STUB_ENV,
            "ENGINE_AGENT_AUTH_SECRETS": "GH proj gh ondemand\n",
            "ENGINE_AGENT_AUTH_GH_TOKEN_FROM": "GH",
        },
        check=False,
    )
    assert completed.returncode != 0
    assert "ondemand entry" in completed.stderr


@needs_engine
def test_get_resolves_exactly_one_manifest_entry_and_prints_only_its_value() -> None:
    """#568, the engine's half: `get VAR` is what the engine runs, holding the
    identity, when a session asks for a manifest secret.

    stdout *is* the value, so it must be the value and nothing else — no
    newline, no banner — and a name the deployment did not declare is refused
    before any identity is touched, naming the manifest.
    """
    env = {
        **_AGENT_AUTH_STUB_ENV,
        "ENGINE_AGENT_AUTH_SECRETS": "A proj alpha\nB proj beta ondemand\n",
    }
    ok = subprocess.run(
        ["bash", "-c", _AGENT_AUTH_STUB_PRELUDE + "main get B"],
        capture_output=True,
        text=True,
        env=env,
        check=False,
    )
    assert ok.returncode == 0, ok.stderr
    assert ok.stdout == "val-beta", "the value, verbatim, and nothing else"

    refused = subprocess.run(
        ["bash", "-c", _AGENT_AUTH_STUB_PRELUDE + "main get NOPE"],
        capture_output=True,
        text=True,
        env=env,
        check=False,
    )
    assert refused.returncode != 0
    assert refused.stdout == "", "a refusal never puts anything on the value stream"
    assert "NOPE is not a variable in ENGINE_AGENT_AUTH_SECRETS" in refused.stderr

    # A refusal for an undeclared name must not need the identity at all: the
    # manifest is checked first, so an unconfigured pod gives the same answer.
    no_identity = {k: v for k, v in env.items() if not k.startswith("INFISICAL_")}
    refused_early = subprocess.run(
        ["bash", "-c", _AGENT_AUTH_STUB_PRELUDE + "main get NOPE"],
        capture_output=True,
        text=True,
        env=no_identity,
        check=False,
    )
    assert "NOPE is not a variable in ENGINE_AGENT_AUTH_SECRETS" in refused_early.stderr


# `fetch` is curl against the engine; stub curl the way the probe tests do,
# recording the invocation so the test can see which URL and bearer went out.
_FETCH_STUB = """
source {auth}
curl() {{
    local output_file=""
    printf '%s\\n' "$@" >"$CURL_ARGS_FILE"
    while (($#)); do
        case "$1" in
            -o) output_file="$2"; shift 2 ;;
            -w) shift 2 ;;
            *) shift ;;
        esac
    done
    printf '%s' "$FETCH_BODY" >"$output_file"
    printf '%s' "$FETCH_STATUS"
}}
main fetch {var}
"""


@needs_engine
@pytest.mark.parametrize(
    ("status", "body", "expected_exit", "expect_stdout", "expect_stderr"),
    [
        ("200", "s3cr3t-value", 0, "s3cr3t-value", ""),
        (
            "403",
            '{"error":"X is not in ENGINE_AGENT_AUTH_SECRETS"}',
            1,
            "",
            "ENGINE_AGENT_AUTH_SECRETS",
        ),
        ("401", "", 1, "", "no longer knows this session"),
        ("502", '{"error":"helper failed"}', 1, "", "helper failed"),
    ],
    ids=["value", "not-in-manifest", "revoked", "upstream-failure"],
)
def test_fetch_asks_the_engine_with_the_sessions_own_broker_token(
    tmp_path: Path,
    status: str,
    body: str,
    expected_exit: int,
    expect_stdout: str,
    expect_stderr: str,
) -> None:
    """#568, the session's half: no identity, only the broker token.

    The value is printed verbatim and nothing else; every refusal repeats the
    engine's own reason, so the message a session sees for an undeclared name
    is the manifest line to add.
    """
    args_file = tmp_path / "curl-args"
    completed = subprocess.run(
        ["bash", "-c", _FETCH_STUB.format(auth=AGENT_AUTH, var="X")],
        capture_output=True,
        text=True,
        env={
            "PATH": "/usr/bin:/bin",
            "MYDEVENV2_BROKER_URL": "http://127.0.0.1:8910/",
            "MYDEVENV2_BROKER_TOKEN": "tok-for-this-session",
            "CURL_ARGS_FILE": str(args_file),
            "FETCH_STATUS": status,
            "FETCH_BODY": body,
        },
        check=False,
    )
    assert completed.returncode == expected_exit, completed.stderr
    assert completed.stdout == expect_stdout
    if expect_stderr:
        assert expect_stderr in completed.stderr
    args = args_file.read_text(encoding="utf-8").splitlines()
    assert "http://127.0.0.1:8910/api/agent-auth/fetch/X" in args, (
        "the engine on loopback, the one route, the trailing slash normalised"
    )
    assert "Authorization: Bearer tok-for-this-session" in args
    assert "POST" in args


@needs_engine
def test_fetch_outside_a_brokered_session_says_so() -> None:
    """Not a misleading 'credential missing': the message names the broker
    variables and the two reasons they can be absent."""
    completed = subprocess.run(
        ["bash", "-c", f"source {AGENT_AUTH!s}\nmain fetch X"],
        capture_output=True,
        text=True,
        env={"PATH": "/usr/bin:/bin"},
        check=False,
    )
    assert completed.returncode != 0
    assert completed.stdout == ""
    assert "MYDEVENV2_BROKER_URL/_TOKEN" in completed.stderr
    assert "ENGINE_AGENT_AUTH_SECRETS" in completed.stderr


# ── The write broker (#598): `set` engine-side, `store` session-side ─────────

# `store` is curl against the engine with the value on stdin; stub curl to
# record both the invocation and what it read on stdin, so a test can prove the
# value went on stdin and never onto the command line.
_STORE_STUB = """
source {auth}
curl() {{
    local output_file=""
    printf '%s\\n' "$@" >"$CURL_ARGS_FILE"
    cat >"$CURL_STDIN_FILE"
    while (($#)); do
        case "$1" in
            -o) output_file="$2"; shift 2 ;;
            -w) shift 2 ;;
            *) shift ;;
        esac
    done
    printf '%s' "$STORE_BODY" >"$output_file"
    printf '%s' "$STORE_STATUS"
}}
printf '%s' "$STORE_VALUE" | main store {var}
"""


@needs_engine
def test_store_sends_the_value_on_stdin_and_never_on_the_command_line(
    tmp_path: Path,
) -> None:
    """#598, the session's half: no identity, only the broker token, and the
    value on stdin. The engine's outcome word is relayed verbatim."""
    args_file = tmp_path / "curl-args"
    stdin_file = tmp_path / "curl-stdin"
    completed = subprocess.run(
        ["bash", "-c", _STORE_STUB.format(auth=AGENT_AUTH, var="RW")],
        capture_output=True,
        text=True,
        env={
            "PATH": "/usr/bin:/bin",
            "MYDEVENV2_BROKER_URL": "http://127.0.0.1:8910/",
            "MYDEVENV2_BROKER_TOKEN": "tok-for-this-session",
            "CURL_ARGS_FILE": str(args_file),
            "CURL_STDIN_FILE": str(stdin_file),
            "STORE_STATUS": "200",
            "STORE_BODY": "created",
            "STORE_VALUE": "brand-new-secret",
        },
        check=False,
    )
    assert completed.returncode == 0, completed.stderr
    assert completed.stdout == "created", "relays the engine's outcome"
    args = args_file.read_text(encoding="utf-8").splitlines()
    assert "http://127.0.0.1:8910/api/agent-auth/store/RW" in args, (
        "the engine on loopback, the store route, the trailing slash normalised"
    )
    assert "Authorization: Bearer tok-for-this-session" in args
    assert "POST" in args
    # The value went on stdin, and nowhere near argv.
    assert stdin_file.read_text(encoding="utf-8") == "brand-new-secret"
    assert "brand-new-secret" not in "\n".join(args)


@needs_engine
@pytest.mark.parametrize(
    ("status", "body", "expect_stderr"),
    [
        ("403", '{"error":"X is declared but not writable"}', "not writable"),
        ("401", "", "no longer knows this session"),
        ("413", '{"error":"too large"}', "size cap"),
    ],
    ids=["not-writable", "revoked", "too-large"],
)
def test_store_repeats_the_engines_refusal(
    tmp_path: Path, status: str, body: str, expect_stderr: str
) -> None:
    completed = subprocess.run(
        ["bash", "-c", _STORE_STUB.format(auth=AGENT_AUTH, var="X")],
        capture_output=True,
        text=True,
        env={
            "PATH": "/usr/bin:/bin",
            "MYDEVENV2_BROKER_URL": "http://127.0.0.1:8910/",
            "MYDEVENV2_BROKER_TOKEN": "tok",
            "CURL_ARGS_FILE": str(tmp_path / "a"),
            "CURL_STDIN_FILE": str(tmp_path / "b"),
            "STORE_STATUS": status,
            "STORE_BODY": body,
            "STORE_VALUE": "v",
        },
        check=False,
    )
    assert completed.returncode != 0
    assert completed.stdout == "", "a refusal never puts anything on the value stream"
    assert expect_stderr in completed.stderr


@needs_engine
def test_set_refuses_an_entry_that_is_not_writable() -> None:
    """#598, the engine's half re-checks the `writable` flag itself: the helper
    is pluggable and must stand alone."""
    completed = subprocess.run(
        ["bash", "-c", _AGENT_AUTH_STUB_PRELUDE + "printf 'x' | main set RO"],
        capture_output=True,
        text=True,
        env={
            **_AGENT_AUTH_STUB_ENV,
            "ENGINE_AGENT_AUTH_SECRETS": "RO proj ro ondemand\n",
        },
        check=False,
    )
    assert completed.returncode != 0
    assert completed.stdout == ""
    assert "not writable" in completed.stderr


@needs_engine
def test_set_upserts_a_writable_entry_and_prints_only_the_outcome(
    tmp_path: Path,
) -> None:
    """`set` reads the value on stdin (never argv), upserts, and prints only
    `created`/`updated` — nothing else on the value stream."""
    written = tmp_path / "written"
    # write_secret is the pluggable write half; stub it to capture stdin.
    prelude = _AGENT_AUTH_STUB_PRELUDE + f"write_secret() {{ cat > '{written}'; }}\n"
    env = {
        **_AGENT_AUTH_STUB_ENV,
        "ENGINE_AGENT_AUTH_SECRETS": "RW proj rw ondemand,writable\n",
    }

    # get_secret (stub) resolves a value, so the secret exists → updated.
    updated = subprocess.run(
        ["bash", "-c", prelude + "printf 'multi\\nline secret' | main set RW"],
        capture_output=True,
        text=True,
        env=env,
        check=False,
    )
    assert updated.returncode == 0, updated.stderr
    assert updated.stdout == "updated"
    # The value arrived on stdin, verbatim, multi-line and all.
    assert written.read_text(encoding="utf-8") == "multi\nline secret"

    # An absent prior value → created.
    created = subprocess.run(
        [
            "bash",
            "-c",
            prelude + "get_secret() { printf ''; }\nprintf 'v' | main set RW",
        ],
        capture_output=True,
        text=True,
        env=env,
        check=False,
    )
    assert created.returncode == 0, created.stderr
    assert created.stdout == "created"


@needs_engine
def test_the_writable_breadcrumb_lists_only_writable_names() -> None:
    """#598: a session is told what it may store in AGENT_AUTH_WRITABLE, beside
    what it may fetch (AGENT_AUTH_ONDEMAND) and what it holds (AGENT_AUTH_GRANTED).
    `writable` is orthogonal to the read policy."""
    script = _AGENT_AUTH_STUB_PRELUDE + "main run -- env"
    completed = subprocess.run(
        ["bash", "-c", script],
        capture_output=True,
        text=True,
        env={
            **_AGENT_AUTH_STUB_ENV,
            "ENGINE_AGENT_AUTH_SECRETS": (
                "LAUNCHED proj l\n"
                "RW proj rw ondemand,writable\n"
                "RWLAUNCH proj rl writable\n"
            ),
        },
        check=False,
    )
    assert completed.returncode == 0, completed.stderr
    child_env = dict(line.partition("=")[::2] for line in completed.stdout.splitlines())
    # Both writable names, ondemand or launched, and nothing that is not writable.
    assert child_env.get("AGENT_AUTH_WRITABLE", "").split() == ["RW", "RWLAUNCH"]
    assert child_env.get("AGENT_AUTH_ONDEMAND", "").split() == ["RW"]
    # An ondemand entry is not exported at launch; a launched one (writable-only,
    # so still required) is.
    assert "RW" not in child_env
    assert child_env.get("RWLAUNCH") == "val-rl"


@needs_engine
def test_a_writable_only_entry_is_still_required_at_launch() -> None:
    """`writable` alone does not relax the read policy: the entry is still
    required, so an absent value fails the launch as any required one does."""
    prelude = _AGENT_AUTH_STUB_PRELUDE + "get_secret() { printf ''; }\n"
    completed = subprocess.run(
        ["bash", "-c", prelude + "main run -- true"],
        capture_output=True,
        text=True,
        env={
            **_AGENT_AUTH_STUB_ENV,
            "ENGINE_AGENT_AUTH_SECRETS": "RW proj rw writable\n",
        },
        check=False,
    )
    assert completed.returncode != 0
    assert "missing or empty" in completed.stderr


@needs_engine
def test_the_writable_breadcrumb_is_published_before_the_strip() -> None:
    """Same ordering property as the other breadcrumbs: exported before the
    identity and manifest are unset, so the three cannot drift apart."""
    body = _without_comments(AGENT_AUTH.read_text(encoding="utf-8"))
    assert 'export AGENT_AUTH_WRITABLE="$AGENT_AUTH_WRITABLE_VARS"' in body
    writable_at = body.index("export AGENT_AUTH_WRITABLE")
    unset_at = body.index("unset INFISICAL_CLIENT_ID")
    assert writable_at < unset_at, "the breadcrumb must be published before the strip"


@needs_engine
def test_an_unknown_manifest_flag_fails_the_launch() -> None:
    """The helper mirrors the engine: an unknown flag is an error, not a silent
    'required' (#598)."""
    completed = subprocess.run(
        ["bash", "-c", _AGENT_AUTH_STUB_PRELUDE + "main run -- true"],
        capture_output=True,
        text=True,
        env={**_AGENT_AUTH_STUB_ENV, "ENGINE_AGENT_AUTH_SECRETS": "X proj s bogus\n"},
        check=False,
    )
    assert completed.returncode != 0
    assert "unknown flag" in completed.stderr


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


def test_newest_wins_ci_is_paired_with_full_classification_on_push() -> None:
    """Cancelling a run is safe only when its replacement covers its ground.

    CI is newest-wins per ref: a superseded run — pending or in progress —
    is cancelled when a newer one joins its group, so a merge burst costs
    one run instead of a bank of queued ones. That is only sound because a
    push run classifies as a change to *everything*: under the earlier
    incremental `before..sha` classification, each commit was examined by
    exactly one run, and a cancelled run's files were checked by nothing,
    ever — two failures escaped to `dev` through that gap. These two
    properties must move together, so they are asserted together.
    """
    raw = (WORKFLOWS / "ci.yml").read_text(encoding="utf-8")
    group = re.search(r"^  group: (.+)$", raw, re.MULTILINE)
    assert group, "ci.yml declares a concurrency group"
    assert "github.ref" in group.group(1), (
        "the ref keeps `dev` and `main` — the same commit after a "
        "fast-forward release — from evicting each other's runs"
    )
    assert "cancel-in-progress: true" in raw, (
        "newest-wins is the policy: superseded runs are cancelled, not banked"
    )
    assert "github.sha" not in group.group(1), (
        "per-commit keying banks a run per push; if that is being restored, "
        "push classification may become incremental again in the same change"
    )
    classify = _without_comments(raw)
    assert '"$BEFORE' not in classify, (
        "a push must not classify by `before..sha`: newest-wins cancellation "
        "would leave a superseded run's files checked by nothing, ever"
    )
    assert "everything" in classify, (
        "the classify step's push arm must declare a change to everything, "
        "which is what makes cancelling a superseded push run lose nothing"
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
    """A workflow that cancels superseded runs must re-cover their ground.

    `docs.yml` checks the whole tree every time, so a later run covers
    everything a superseded one would have — which is what makes its
    cancellation safe, and the property the comment in the file exists to
    protect. `ci.yml` now holds the same property by classifying every push
    as a change to everything (asserted in the newest-wins test above);
    historically it classified by `before..sha`, and cancelling under that
    regime cost two escaped failures (§6.3 finding 19).
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
    assert re.search(r"actions/setup-java@[0-9a-f]{40}", job), (
        "the release runner needs an immutable JDK setup action"
    )
    assert re.search(r"android-actions/setup-android@[0-9a-f]{40}", job), (
        "the release runner needs an immutable Android SDK setup action"
    )
    assert "exit 1" in job, "a missing keystore stops the job"
    assert re.search(r"pnpm/action-setup@[0-9a-f]{40}", job), (
        "the self-hosted runner must install pnpm through an immutable action"
    )
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


def test_release_tags_are_required_to_reference_prod() -> None:
    """A release tag is a production assertion, not an arbitrary ref alias."""
    release = (WORKFLOWS / "release.yml").read_text(encoding="utf-8")
    assert "\n  validate-release:" in release
    guard = release[
        release.index("\n  validate-release:") : release.index("\n  # Docker Hub")
    ]
    assert "fetch-depth: 0" in guard
    assert "refs/heads/prod:refs/remotes/origin/prod" in guard
    assert 'git merge-base --is-ancestor "$TAG_SHA" origin/prod' in guard
    assert "exit 1" in guard

    # Both independent first-release paths wait on the guard. Every image,
    # distribution, and signed APK job is downstream from one of these.
    assert (
        "needs: validate-release"
        in release[
            release.index("\n  base-images:") : release.index("\n  distribution:")
        ]
    )
    assert (
        "needs: validate-release"
        in release[release.index("\n  distribution:") : release.index("\n  image:")]
    )


def test_production_deploy_is_approval_gated_and_pins_an_immutable_digest() -> None:
    workflow = (WORKFLOWS / "deploy-production.yml").read_text(encoding="utf-8")
    assert "workflow_dispatch:" in workflow
    assert "environment: vogt-prod" in workflow
    assert "description: Type DEPLOY to confirm the live production change" in workflow
    assert '[ "$CONFIRM" = DEPLOY ]' in workflow
    assert "git merge-base --is-ancestor" in workflow
    assert "docker buildx imagetools inspect" in workflow
    assert re.search(r"actions/create-github-app-token@[0-9a-f]{40}", workflow)
    assert "VOGT_DEPLOYMENT_REPOSITORY" in workflow
    assert "actions/workflows/${DEPLOY_WORKFLOW}/dispatches" in workflow
    assert "image_digest" in workflow
    assert "vogt-deployment-receipt" in workflow
    assert "KOMODO_API" not in workflow
    assert re.search(r"sigstore/cosign-installer@[0-9a-f]{40}", workflow)
    assert "cosign verify" in workflow


def test_production_handoff_uses_real_url_and_deterministic_correlation() -> None:
    workflow = (WORKFLOWS / "deploy-production.yml").read_text(encoding="utf-8")
    assert (
        "RECEIPT_RUN: ${{ github.server_url }}/${{ github.repository }}"
        "/actions/runs/${{ github.run_id }}"
    ) in workflow
    assert 'RECEIPT_RUN: "${GITHUB_SERVER_URL}' not in workflow
    assert "DISPATCH_ID: ${{ github.run_id }}-${{ github.run_attempt }}" in workflow
    assert "dispatch_id: $dispatch_id" in workflow
    assert "display_title" in workflow
    assert 'run.get("head_branch") == deploy_ref' in workflow
    assert "set the estate workflow run-name to include dispatch_id" in workflow


def test_production_handoff_validates_and_reports_the_receipt() -> None:
    workflow = (WORKFLOWS / "deploy-production.yml").read_text(encoding="utf-8")
    assert "scripts/validate_deployment_receipt.py" in workflow
    assert "deploy/vogt-deployment-receipt.schema.json" in workflow
    assert "GITHUB_STEP_SUMMARY" in workflow
    assert re.search(r"actions/upload-artifact@[0-9a-f]{40}", workflow)
    assert "vogt-prod-deployment-receipt-${{ inputs.tag }}" in workflow


def test_production_receipt_validator_accepts_schema_and_rejects_invalid_receipts(
    tmp_path: Path,
) -> None:
    valid = {
        "source_repository": "TheDancingDeveloper-org/vogt",
        "source_sha": "a" * 40,
        "source_tag": "v0.3.0",
        "image_digest": "sha256:" + "b" * 64,
        "desired_state_commit": "c" * 40,
        "deployment_id": "komodo-123",
        "live_smoke": {"status": "passed"},
        "rollback_plan": "revert the desired-state commit",
        "migration_limits": "restore the pre-release backup",
    }
    receipt = tmp_path / "receipt.json"
    receipt.write_text(json.dumps(valid), encoding="utf-8")
    schema = REPO_ROOT / "deploy/vogt-deployment-receipt.schema.json"

    accepted = subprocess.run(
        [sys.executable, str(RECEIPT_VALIDATOR), str(receipt), str(schema)],
        capture_output=True,
        text=True,
        check=False,
    )
    assert accepted.returncode == 0, accepted.stderr

    invalid = dict(valid)
    invalid.pop("migration_limits")
    receipt.write_text(json.dumps(invalid), encoding="utf-8")
    rejected = subprocess.run(
        [sys.executable, str(RECEIPT_VALIDATOR), str(receipt), str(schema)],
        capture_output=True,
        text=True,
        check=False,
    )
    assert rejected.returncode == 1
    assert "migration_limits" in rejected.stderr


def test_dev_deploy_is_immutable_and_receipt_gated() -> None:
    workflow = (WORKFLOWS / "deploy-dev.yml").read_text(encoding="utf-8")
    assert "workflow_dispatch:" in workflow
    assert "runs-on: [self-hosted, tailnet, docker]" in workflow
    assert "VOGT_KOMODO_URL" in workflow
    assert "VOGT_KOMODO_STACK" in workflow
    assert "DEPLOY-DEV" in workflow
    assert "dev-${SOURCE_SHA}" in workflow
    # Komodo's control credentials stay plain GitHub Actions secrets — no
    # secret broker, no CLI, no GitHub App on the deploy path. The one estate
    # dependency is the dev front-door token: the vogt-stack-estate stack stores
    # it as an `[[infisical://…]]` reference that Komodo resolves out of band, so
    # the helper resolves that same reference against Infisical purely to hand
    # the live smoke a real bearer token (scripts/deploy_dev.py:resolve_secret_ref).
    # That is the sole Infisical use, and it never touches the Komodo control path.
    assert "secrets.KOMODO_API_KEY" in workflow
    assert "secrets.KOMODO_API_SECRET" in workflow
    assert "INFISICAL_CLIENT_ID" in workflow
    assert "INFISICAL_CLIENT_SECRET" in workflow
    # Resolution lives in the helper, not a CLI or an estate-specific action.
    assert "infisical login" not in workflow
    assert "Infisical/secrets-action" not in workflow
    assert "scripts/deploy_dev.py" in workflow
    assert "WriteStackFileContents" not in workflow, (
        "Komodo API details belong in the helper, not in the workflow"
    )
    assert "actions/create-github-app-token" not in workflow
    assert "actions/workflows/${DEPLOY_WORKFLOW}/dispatches" not in workflow
    assert "VOGT_DEV_GITHUB_APP_ID" not in workflow
    assert "VOGT_DEV_GITHUB_APP_PEM" not in workflow
    assert "RefreshStackCache" not in workflow
    assert "DeployStack" not in workflow
    assert "cosign verify" in workflow
    assert "vogt-dev-deployment-receipt" in workflow
    assert "product_version" in workflow
    assert "smoke_merged_stack.sh" in workflow
    assert "api/sessions" in workflow
    assert '"reason": "verified dev deployment"' in workflow
    promote = (WORKFLOWS / "promote.yml").read_text(encoding="utf-8")
    assert "deploy-dev.yml" in promote
    assert "verified dev deployment receipt" in promote


def test_dev_deploy_helper_updates_only_the_active_digest_pins() -> None:
    helper = (REPO_ROOT / "scripts" / "deploy_dev.py").read_text(encoding="utf-8")
    assert "read/GetStack" in helper
    assert "write/WriteStackFileContents" in helper
    assert '"vogt.compose.yml"' in helper
    assert '"estate.overlay.yml"' in helper
    assert "write/RefreshStackCache" in helper
    assert "execute/DeployStack" in helper
    assert "webhook_enabled" in helper
    assert "sha256:[0-9a-f]{64}" in helper
    assert "MYDEVENV2_TOKEN" in helper
    # The front-door token is an `[[infisical://…]]` reference in the Komodo
    # env; the helper resolves it against Infisical so the smoke gets a real
    # bearer token rather than the literal placeholder.
    assert "resolve_secret_ref" in helper
    assert "universal-auth" in helper


def test_github_release_collects_and_publishes_the_complete_release() -> None:
    workflow = (WORKFLOWS / "release.yml").read_text(encoding="utf-8")
    job = workflow[workflow.index("\n  github-release:") :]
    assert (
        "needs: [validate-release, distribution, image, "
        "voice-image, stack-image, android]"
    ) in job
    assert "vogt-android-release-${{ github.ref_name }}" in job
    assert "signed APK is required" in job
    # No wheel, no sdist (2026-09-04): nothing consumed them — no PyPI
    # publish, no image built from them, no doc pointing at them — and the
    # one supported artefact is the merged image. A release that attaches
    # files nobody can use is a release that lies about what it ships.
    assert "vogt-dist-" not in job
    assert "wheel" not in job and "sdist" not in job
    assert "uv build" not in workflow
    assert "CORE_DIGEST" in job and "STACK_DIGEST" in job
    assert "vogt-release-manifest.json" in job
    assert "production_deployment_handoff" in job
    assert "name: install GitHub CLI" in job
    assert "command -v gh" in job
    assert "gh release upload" in job
    assert "--clobber" in job


def test_deployment_receipt_schema_carries_the_full_handoff_evidence() -> None:
    schema = json.loads(
        (REPO_ROOT / "deploy/vogt-deployment-receipt.schema.json").read_text(
            encoding="utf-8"
        )
    )
    assert {
        "source_repository",
        "source_sha",
        "source_tag",
        "image_digest",
        "desired_state_commit",
        "deployment_id",
        "live_smoke",
        "rollback_plan",
        "migration_limits",
    } <= set(schema["required"])
    assert (
        schema["properties"]["live_smoke"]["properties"]["status"]["const"] == "passed"
    )


def test_promotion_is_a_verified_fast_forward_push() -> None:
    workflow = (WORKFLOWS / "promote.yml").read_text(encoding="utf-8")
    assert "type: choice" in workflow
    assert "dev-to-main" in workflow
    assert "main-to-prod" in workflow
    environment = (
        "environment: ${{ inputs.stage == 'dev-to-main' && 'promote-main' || "
        "'promote-prod' }}"
    )
    assert environment in workflow
    assert "git merge-base --is-ancestor" in workflow
    # GitHub has no fast-forward merge method, so the old PR-based promotion
    # rewrote the promoted SHAs on every merge and broke its own ancestry gate
    # for the next stage. Promotion is now a plain fast-forward push with the
    # workflow token, under the release-branch-promotion ruleset: deletions
    # and force pushes blocked, and every update must carry green ci and
    # runner-policy checks on the pushed commit — which a fast-forward of the
    # validated source SHA has by construction. No force flag anywhere: the
    # server rejects a non-fast-forward even if every gate above regressed.
    assert 'git push origin "$SOURCE_SHA:refs/heads/$TARGET"' in workflow
    assert "--force" not in workflow
    assert "force-with-lease" not in workflow
    assert "contents: write" in workflow
    # The PR path and its operator-owned token are gone.
    assert "VOGT_PROMOTION_TOKEN" not in workflow
    assert "/pulls" not in workflow
    # #460: the self-hosted runner has curl and jq but not the gh CLI.
    assert "gh pr create" not in workflow
    assert "gh api" not in workflow
    assert "gh run list" not in workflow
    assert "gh workflow run" not in workflow
    # A token push fires no push-triggered workflows; the promoted commit
    # keeps the checks it earned on the source branch (same SHA), and the
    # ref-bound pipelines are dispatched explicitly — build.yml for the
    # branch-scoped image tags, ci.yml on prod for the production APK.
    assert "/actions/workflows/build.yml/dispatches" in workflow
    assert "/actions/workflows/ci.yml/dispatches" in workflow
    assert "for check in ci runner-policy; do" in workflow


def test_release_branches_accept_only_the_promotion_edges() -> None:
    workflow = (WORKFLOWS / "promotion-policy.yml").read_text(encoding="utf-8")
    assert "branches: [main, prod]" in workflow
    assert "main:dev|prod:main" in workflow
    assert "git merge-base --is-ancestor" in workflow
    assert "HEAD_REPO" in workflow


def test_release_core_build_waits_for_the_mirrored_base() -> None:
    workflow = (WORKFLOWS / "release.yml").read_text(encoding="utf-8")
    image = workflow[
        workflow.index("\n  image:\n") : workflow.index("\n  # The dev-pod")
    ]
    assert "needs: [distribution, base-images]" in image


def test_root_image_base_is_mirrored_before_a_release_build() -> None:
    root_dockerfile = (WORKFLOWS.parent.parent / "Dockerfile").read_text(
        encoding="utf-8"
    )
    mirror = (WORKFLOWS / "mirror-base-images.yml").read_text(encoding="utf-8")

    assert "ARG PYTHON_IMAGE=python:3.13-slim@sha256:" in root_dockerfile
    assert '"Dockerfile"' in mirror
    assert "ARG PYTHON_IMAGE=python:" in mirror
    assert "existing verified mirror" in mirror


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
    assert "engine/agent-versions.env" in text
    assert "agent-versions.resolved" in text
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


@pytest.mark.skipif(
    not ENGINE_DOCKERFILE.exists(),
    reason="the core-alone job (NFR-Q6) deletes engine/; this reads its Dockerfile",
)
def test_agent_cli_smoke_checks_the_resolved_image_versions() -> None:
    for workflow in ("build.yml", "release.yml"):
        text = (WORKFLOWS / workflow).read_text(encoding="utf-8")
        assert "--entrypoint vogt-verify-agent-clis" in text
    verifier = (REPO_ROOT / "engine" / "deploy" / "verify-agent-clis.sh").read_text(
        encoding="utf-8"
    )
    assert "agent-versions.resolved" in verifier
    assert "python3 -c" in verifier, (
        "the pod image provides python3, not the core venv's python alias"
    )
    assert "npm list --global" in verifier


def test_lifecycle_contract_is_image_neutral_and_deployment_owned() -> None:
    runner = (REPO_ROOT / "vogt-lifecycle.sh").read_text(encoding="utf-8")
    assert "/run/vogt/hooks" in runner
    assert "pre-start" in runner and "post-start" in runner and "post-health" in runner
    assert "VOGT_LIFECYCLE_FIRST_START" in runner
    assert "VOGT_HOOKS_REQUIRED" in runner
    sample = (REPO_ROOT / "deploy/lifecycle-hooks/restore-and-verify.sh").read_text(
        encoding="utf-8"
    )
    assert "dirty" in sample
    assert "refusing to overwrite" in sample


@pytest.mark.skipif(
    not (REPO_ROOT / "engine" / "Dockerfile").is_file(),
    reason="the core-alone job (NFR-Q6) deletes engine/; this reads its Dockerfile",
)
def test_the_pinned_agent_cli_cannot_update_past_its_pin() -> None:
    """A pin the tool can walk past at runtime is not a pin (#196).

    Claude Code checks for a newer release on startup. It cannot overwrite the
    install it runs — `/usr/local`, root-owned, pod runs as `sprooty` — so it
    writes the update to `NPM_CONFIG_PREFIX`, which is the *persisted*
    `~/.npm-global`. PATH prefers `/usr/local/bin`, so the update never takes
    effect and is redone every boot: an unpinned copy accumulating on the data
    volume, one PATH-ordering change from winning. Observed live on `vogt-dev`
    — pinned 2.1.236 in the image, 2.1.238 written to the volume minutes after
    boot — and that ordering has failed before (`Dockerfile.pod` records a
    theclawbay-managed `codex` shadowing the system wrapper on 2026-07-31).

    `DISABLE_UPDATES` and not `DISABLE_AUTOUPDATER`: the latter stops the
    background check while leaving `claude update` free to fork the image's
    version anyway, and here the image is the statement of what runs (NFR-C3).
    """
    text = (REPO_ROOT / "engine" / "Dockerfile").read_text("utf-8")
    assert re.search(r"^ENV\s+DISABLE_UPDATES=1\s*$", text, re.MULTILINE), (
        "engine/Dockerfile must set DISABLE_UPDATES=1, or the pinned agent CLI "
        "re-installs itself into the persisted $HOME on every boot (#196)"
    )


@pytest.mark.skipif(
    not ENGINE_DOCKERFILE.exists(),
    reason="the core-alone job (NFR-Q6) deletes engine/; this reads its Dockerfile",
)
def test_the_pinned_claude_proves_it_is_not_a_stub() -> None:
    """`npm install -g` exiting 0 is not evidence that `claude` runs (#505).

    The published tarball ships `bin/claude.exe` as a ~500-byte shell stub; the
    real binary arrives from the `postinstall` script or a platform-native
    optional dependency. When that does not happen the install still succeeds
    and the image still builds — the pod simply has a `claude` that prints an
    error. That is the packaging that broke 2.1.237 (#147/#148) and held the pin
    at 2.1.236 for months, and it has not changed since, so a pin is only worth
    what a check that the binary answers is worth.

    Paired with the sibling test above: `DISABLE_UPDATES` stops the pin being
    walked past at runtime, this stops it being hollow at build time.
    """
    text = ENGINE_DOCKERFILE.read_text(encoding="utf-8")
    assert re.search(r"claude --version \| grep -qF", text), (
        "engine/Dockerfile must run the claude it just installed and match it "
        "against the pin, or a stub install is a green build (#505)"
    )


# ── Runtime-pinned agent CLIs (#590) ─────────────────────────────────────────
#
# The baked pin is the baseline; the version a pod runs is a deploy-time value
# applied by `engine/deploy/agent-cli-install.sh` at container start. These
# exercise the installer and the verifier against a fake `npm`, so the
# contract — exact pins, dist-tags only by opt-in, a failed install leaves
# `current` alone, the image copy is the offline fallback — is pinned without
# a network or an image build.

INSTALLER = REPO_ROOT / "engine" / "deploy" / "agent-cli-install.sh"
VERIFIER = REPO_ROOT / "engine" / "deploy" / "verify-agent-clis.sh"
ENTRYPOINT = REPO_ROOT / "engine" / "deploy" / "entrypoint.sh"

_FAKE_NPM = r"""#!/usr/bin/env bash
# A stand-in for npm: `install -g --prefix` lays down a bin that prints its
# version, `view` answers a dist-tag, `list` reports what was installed.
set -euo pipefail
printf '%s\n' "$*" >> "${FAKE_NPM_LOG:?}"
case "$1" in
  install)
    prefix=""; spec=""
    shift
    while (( $# )); do
      case "$1" in
        --prefix) prefix="$2"; shift ;;
        -g|--global) ;;
        *) spec="$1" ;;
      esac
      shift
    done
    pkg="${spec%@*}"; ver="${spec##*@}"
    if [[ "$ver" == "${FAKE_NPM_FAIL_VERSION:-}" ]]; then
      echo "npm ERR! 404 Not Found - $spec" >&2
      exit 1
    fi
    case "$pkg" in
      @anthropic-ai/claude-code) bin=claude ;;
      @openai/codex) bin=codex ;;
      theclawbay) bin=theclawbay ;;
      *) echo "unexpected package $pkg" >&2; exit 2 ;;
    esac
    mkdir -p "$prefix/bin" "$prefix/lib/node_modules/$pkg"
    printed="$ver"
    [[ "$ver" == "${FAKE_NPM_STUB_VERSION:-}" ]] && printed="0.0.0-stub"
    printf '#!/usr/bin/env bash\necho "%s %s"\n' "$bin" "$printed" > "$prefix/bin/$bin"
    chmod +x "$prefix/bin/$bin"
    printf '{"name":"%s","version":"%s"}\n' "$pkg" "$ver" \
      > "$prefix/lib/node_modules/$pkg/package.json"
    ;;
  view)
    echo "${FAKE_NPM_LATEST:-9.9.9}"
    ;;
  list)
    prefix=""; pkg=""
    for arg in "$@"; do
      case "$arg" in
        --prefix=*) prefix="${arg#--prefix=}" ;;
        list|--*) ;;
        *) pkg="$arg" ;;
      esac
    done
    read_version='import json,sys; print(json.load(open(sys.argv[1]))["version"])'
    ver="$(python3 -c "$read_version" "$prefix/lib/node_modules/$pkg/package.json" \
      2>/dev/null || true)"
    printf '{"dependencies":{"%s":{"version":"%s"}}}\n' "$pkg" "$ver"
    ;;
  *)
    echo "fake npm: unsupported $*" >&2
    exit 2
    ;;
esac
"""


class _AgentCliSandbox:
    """A root, a baked manifest, a fake image bin and a fake npm, in tmp_path."""

    def __init__(self, tmp_path: Path) -> None:
        self.root = tmp_path / "agent-clis"
        self.root.mkdir()
        self.image_bin = tmp_path / "image" / "bin"
        self.image_bin.mkdir(parents=True)
        self.home_bin = tmp_path / "home" / ".npm-global" / "bin"
        self.home_bin.mkdir(parents=True)
        self.tools = tmp_path / "tools"
        self.tools.mkdir()
        (self.tools / "npm").write_text(_FAKE_NPM, encoding="utf-8")
        (self.tools / "npm").chmod(0o755)
        self.npm_log = tmp_path / "npm.log"
        self.npm_log.write_text("", encoding="utf-8")
        self.baked = tmp_path / "agent-versions.resolved"
        self.baked.write_text("codex=0.149.1\nclaude-code=2.1.258\n", encoding="utf-8")
        # The table engine/Dockerfile writes beside the resolved versions.
        self.table = tmp_path / "agent-clis.tools"
        self.table.write_text(
            "codex\t@openai/codex\tcodex\tVOGT_CODEX_VERSION\n"
            "claude-code\t@anthropic-ai/claude-code\tclaude\tVOGT_CLAUDE_CODE_VERSION\n",
            encoding="utf-8",
        )
        # The image's own copy of claude, so PATH has something to fall back to.
        # `npm list` against the image prefix needs the package there too.
        image_pkg = (
            tmp_path
            / "image"
            / "lib"
            / "node_modules"
            / "@anthropic-ai"
            / "claude-code"
        )
        image_pkg.mkdir(parents=True)
        image_pkg.joinpath("package.json").write_text(
            '{"name":"@anthropic-ai/claude-code","version":"2.1.258"}\n',
            encoding="utf-8",
        )
        (self.image_bin / "claude").write_text(
            '#!/usr/bin/env bash\necho "claude 2.1.258 (image)"\n', encoding="utf-8"
        )
        (self.image_bin / "claude").chmod(0o755)

    def env(self, **extra: str) -> dict[str, str]:
        return {
            "PATH": f"{self.tools}:{self.root / 'bin'}:{self.image_bin}:/usr/bin:/bin",
            "HOME": str(self.home_bin.parent.parent),
            "VOGT_AGENT_CLI_ROOT": str(self.root),
            "VOGT_AGENT_CLI_BAKED_MANIFEST": str(self.baked),
            "VOGT_AGENT_CLI_TOOLS": str(self.table),
            "VOGT_AGENT_CLI_IMAGE_BIN": str(self.image_bin),
            "VOGT_AGENT_CLI_HOME_BIN": str(self.home_bin),
            "FAKE_NPM_LOG": str(self.npm_log),
            **extra,
        }

    def install(
        self, tool: str, version: str, **extra: str
    ) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["bash", str(INSTALLER), tool, version],
            env=self.env(**extra),
            capture_output=True,
            text=True,
            check=False,
        )

    def verify(self, **extra: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            ["bash", str(VERIFIER)],
            env=self.env(**extra),
            capture_output=True,
            text=True,
            check=False,
        )

    def which(self, name: str) -> str:
        return subprocess.run(
            ["bash", "-c", f"command -v {name}"],
            env=self.env(),
            capture_output=True,
            text=True,
            check=True,
        ).stdout.strip()

    def manifest(self) -> dict[str, str]:
        text = (self.root / "manifest").read_text(encoding="utf-8")
        return dict(line.split("=", 1) for line in text.splitlines() if line)

    def current(self, tool: str) -> str | None:
        link = self.root / tool / "current"
        return link.readlink().name if link.is_symlink() else None

    def npm_calls(self) -> list[str]:
        return [
            line
            for line in self.npm_log.read_text(encoding="utf-8").splitlines()
            if line
        ]


@pytest.fixture
def agent_clis(tmp_path: Path) -> _AgentCliSandbox:
    return _AgentCliSandbox(tmp_path)


@needs_engine
def test_an_exact_runtime_pin_is_installed_and_put_on_path(
    agent_clis: _AgentCliSandbox,
) -> None:
    result = agent_clis.install("claude-code", "2.1.261")
    assert result.returncode == 0, result.stderr

    assert agent_clis.current("claude-code") == "2.1.261"
    assert (agent_clis.root / "claude-code" / "2.1.261" / "bin" / "claude").exists()
    # PATH-first: a session's `claude` is the pinned one, not the image's.
    assert agent_clis.which("claude") == str(agent_clis.root / "bin" / "claude")
    assert agent_clis.manifest() == {"codex": "0.149.1", "claude-code": "2.1.261"}
    # The image copy is untouched: it is the fallback, not a casualty.
    assert "image" in (agent_clis.image_bin / "claude").read_text(encoding="utf-8")
    # And the verifier is satisfied by the manifest, not by the image pin.
    verified = agent_clis.verify()
    assert verified.returncode == 0, verified.stderr


@needs_engine
def test_setting_the_pin_back_to_the_baked_version_needs_no_network(
    agent_clis: _AgentCliSandbox,
) -> None:
    assert agent_clis.install("claude-code", "2.1.261").returncode == 0
    calls_before = len(agent_clis.npm_calls())

    result = agent_clis.install("claude-code", "2.1.258")
    assert result.returncode == 0, result.stderr
    assert agent_clis.current("claude-code") is None
    assert not (agent_clis.root / "bin" / "claude").exists()
    assert agent_clis.which("claude") == str(agent_clis.image_bin / "claude")
    assert agent_clis.manifest()["claude-code"] == "2.1.258"
    assert len(agent_clis.npm_calls()) == calls_before, "reverting must not touch npm"
    # `image` is the explicit spelling of the same request, and unset reads as
    # `image` in the entrypoint.
    assert agent_clis.install("claude-code", "image").returncode == 0
    assert agent_clis.verify().returncode == 0


@needs_engine
def test_a_version_already_on_the_volume_is_switched_to_offline(
    agent_clis: _AgentCliSandbox,
) -> None:
    assert agent_clis.install("claude-code", "2.1.261").returncode == 0
    assert agent_clis.install("claude-code", "2.1.262").returncode == 0
    calls_before = len(agent_clis.npm_calls())

    result = agent_clis.install("claude-code", "2.1.261")
    assert result.returncode == 0, result.stderr
    assert agent_clis.current("claude-code") == "2.1.261"
    assert len(agent_clis.npm_calls()) == calls_before


@needs_engine
def test_a_failed_install_leaves_current_where_it_was(
    agent_clis: _AgentCliSandbox,
) -> None:
    assert agent_clis.install("claude-code", "2.1.261").returncode == 0

    result = agent_clis.install(
        "claude-code", "2.1.999", FAKE_NPM_FAIL_VERSION="2.1.999"
    )
    assert result.returncode == 1
    assert "stays on 2.1.261" in result.stderr
    assert agent_clis.current("claude-code") == "2.1.261"
    assert agent_clis.manifest()["claude-code"] == "2.1.261"
    assert not (agent_clis.root / "claude-code" / "2.1.999").exists()
    assert not list((agent_clis.root / "claude-code").glob(".tmp-*")), (
        "no half-install left behind"
    )


@needs_engine
def test_a_stub_install_fails_the_smoke_check_and_is_discarded(
    agent_clis: _AgentCliSandbox,
) -> None:
    """The 2.1.237 shape: npm exits 0 and `claude --version` does not say 2.1.237."""
    result = agent_clis.install(
        "claude-code", "2.1.237", FAKE_NPM_STUB_VERSION="2.1.237"
    )
    assert result.returncode == 1
    assert "stub" in result.stderr
    assert agent_clis.current("claude-code") is None
    assert not (agent_clis.root / "claude-code" / "2.1.237").exists()
    assert agent_clis.which("claude") == str(agent_clis.image_bin / "claude")


@needs_engine
def test_dist_tags_are_refused_unless_opted_into(agent_clis: _AgentCliSandbox) -> None:
    refused = agent_clis.install("claude-code", "latest")
    assert refused.returncode == 64
    assert "VOGT_AGENT_CLI_ALLOW_DIST_TAGS" in refused.stderr
    assert agent_clis.npm_calls() == []

    accepted = agent_clis.install(
        "claude-code",
        "latest",
        VOGT_AGENT_CLI_ALLOW_DIST_TAGS="1",
        FAKE_NPM_LATEST="2.1.270",
    )
    assert accepted.returncode == 0, accepted.stderr
    assert agent_clis.current("claude-code") == "2.1.270"


@needs_engine
def test_a_malformed_version_is_refused_before_npm_is_asked(
    agent_clis: _AgentCliSandbox,
) -> None:
    for bad in ("2.1", "v2.1.261", "2.1.261; rm -rf /", "../escape", "--flag"):
        result = agent_clis.install("claude-code", bad)
        assert result.returncode == 64, (bad, result.stderr)
    # A tool the image's table does not name is refused, not guessed at.
    unknown = agent_clis.install("opencode", "1.0.0")
    assert unknown.returncode == 64
    assert "agent-clis.tools" in unknown.stderr
    assert agent_clis.npm_calls() == []


@needs_engine
def test_old_versions_are_pruned_but_current_is_kept(
    agent_clis: _AgentCliSandbox,
) -> None:
    for version in ("2.1.260", "2.1.261", "2.1.262", "2.1.263", "2.1.264"):
        assert (
            agent_clis.install(
                "claude-code", version, VOGT_AGENT_CLI_KEEP="2"
            ).returncode
            == 0
        )
    kept = sorted(
        p.name
        for p in (agent_clis.root / "claude-code").iterdir()
        if p.is_dir() and not p.is_symlink()
    )
    assert "2.1.264" in kept
    assert len(kept) == 3, kept  # current plus two more


@needs_engine
def test_codex_runs_through_the_full_access_wrapper_whichever_copy_is_current(
    agent_clis: _AgentCliSandbox,
) -> None:
    """Codex has no PATH-first link; the wrapper picks the runtime copy itself."""
    wrapper = (REPO_ROOT / "engine" / "deploy" / "codex-full-access.sh").read_text(
        encoding="utf-8"
    )
    assert "codex/current/bin/codex" in wrapper
    assert "/usr/local/libexec/codex-real" in wrapper

    assert agent_clis.install("codex", "0.150.0").returncode == 0
    assert agent_clis.current("codex") == "0.150.0"
    assert not (agent_clis.root / "bin" / "codex").exists()
    assert agent_clis.manifest()["codex"] == "0.150.0"


@needs_engine
def test_a_persisted_home_copy_still_fails_the_boot_check(
    agent_clis: _AgentCliSandbox,
) -> None:
    """The one door #196 closed stays closed: the runtime prefix is the only
    sanctioned non-image copy, so `~/.npm-global/bin/claude` is still exit 78."""
    assert agent_clis.install("claude-code", "2.1.261").returncode == 0
    (agent_clis.home_bin / "claude").write_text(
        "#!/usr/bin/env bash\necho stray\n", encoding="utf-8"
    )
    (agent_clis.home_bin / "claude").chmod(0o755)

    result = agent_clis.verify()
    assert result.returncode == 78
    assert "persisted home copy" in result.stderr
    assert agent_clis.verify(VOGT_AGENT_SHADOW_POLICY="warn").returncode == 0


@needs_engine
def test_the_verifier_reports_a_current_that_disagrees_with_the_manifest(
    agent_clis: _AgentCliSandbox,
) -> None:
    assert agent_clis.install("claude-code", "2.1.261").returncode == 0
    # Somebody edited the manifest by hand — or a second writer got in.
    (agent_clis.root / "manifest").write_text(
        "codex=0.149.1\nclaude-code=2.1.262\n", encoding="utf-8"
    )
    result = agent_clis.verify()
    assert result.returncode == 78
    assert "expected pin 2.1.262" in result.stderr


@pytest.mark.skipif(
    not ENGINE_DOCKERFILE.exists(),
    reason="the core-alone job (NFR-Q6) deletes engine/; this reads its Dockerfile",
)
def test_the_runtime_agent_cli_prefix_is_wired_into_the_image() -> None:
    """The mechanism ships only if every half is in the image (#590).

    PATH must prefer the runtime prefix, the installer must be copied and
    runnable, the directory must exist owned by the runtime user so a named
    volume inherits that ownership, and the entrypoint must apply the pin
    *before* the verifier judges the result.
    """
    text = ENGINE_DOCKERFILE.read_text("utf-8")
    assert re.search(
        r"^ENV\s+PATH=/opt/vogt/agent-clis/bin:\$PATH\s*$", text, re.MULTILINE
    )
    assert (
        "engine/deploy/agent-cli-install.sh /usr/local/bin/vogt-agent-cli-install"
        in text
    )
    assert "/usr/local/bin/vogt-agent-cli-install" in text.split("RUN chmod +x", 1)[1]
    assert re.search(
        r"install -d -o \$\{SPROOTY_UID\} -g \$\{SPROOTY_GID\} -m 0755 "
        r"/opt/vogt/agent-clis",
        text,
    )
    # Still closed: the installer is the only writer (#196).
    assert re.search(r"^ENV\s+DISABLE_UPDATES=1\s*$", text, re.MULTILINE)

    # The table the runtime scripts read, so they name no tool themselves
    # (the shipped scripts must stay estate-free, #205).
    assert "> /usr/local/share/vogt/agent-clis.tools" in text
    for row in (
        "codex\\t@openai/codex\\tcodex\\tVOGT_CODEX_VERSION",
        "claude-code\\t@anthropic-ai/claude-code\\tclaude\\tVOGT_CLAUDE_CODE_VERSION",
    ):
        assert row in text, row

    entrypoint = _without_comments(ENTRYPOINT.read_text("utf-8"))
    install_at = entrypoint.index("vogt-agent-cli-install")
    verify_at = entrypoint.index("/usr/local/bin/vogt-verify-agent-clis\n")
    assert install_at < verify_at, "the pin must be applied before it is verified"
    assert "agent-clis.tools" in entrypoint

    overlay = (REPO_ROOT / "deploy" / "engine.overlay.yml").read_text("utf-8")
    for var in ("VOGT_CLAUDE_CODE_VERSION", "VOGT_CODEX_VERSION"):
        assert f'{var}: "${{{var}:-}}"' in overlay
    assert "engine-agent-clis:/opt/vogt/agent-clis" in overlay


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
    agreeing that nobody could check. Now it is a digest — and NFR-C3's real
    guarantee is that CI passes that digest, checked below. `CORE_IMAGE` carries
    a default so a clean clone can `docker build` with no `--build-arg` (#269),
    but only the public `:latest` placeholder tag: a floating *internal* tag
    would silently decouple the two halves of a commit-identified build, and CI
    overrides the placeholder with the digest regardless (same treatment as
    `POD_BASE_IMAGE`).
    """
    text = ENGINE_DOCKERFILE.read_text("utf-8")
    assert "FROM ${CORE_IMAGE} AS core" in text
    assert "COPY --from=core /opt/vogt /opt/vogt" in text
    assert "AS core-build" not in text, "the engine must not build a second core"
    default = re.search(r"^ARG CORE_IMAGE=(.+)$", text, re.MULTILINE)
    assert default is not None, "CORE_IMAGE needs a default so a clean clone builds"
    assert default.group(1).strip() == "ghcr.io/thedancingdeveloper-org/vogt:latest", (
        "the CORE_IMAGE default must be the public :latest placeholder, "
        "never an internal/floating tag; CI overrides it with the digest"
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


# ── The shipped engine scripts carry no estate, and agent-auth/MCP are optional
#    (#205) ───────────────────────────────────────────────────────────────────
#
# The engine image installs these scripts and the sample compose verbatim, so a
# public reader who pulls the image inherits them. None of it may name the
# maintainer's own estate — its hosts, its host paths, or its Infisical secret
# names — and none of it may *require* the maintainer's services to boot: a
# stranger running the image with just a token must get a working engine, and
# the agent-auth / MCP brokering must skip cleanly when it is not configured.

ENGINE_DEPLOY = REPO_ROOT / "engine" / "deploy"
ENGINE_SAMPLE_COMPOSE = ENGINE_DEPLOY / "docker-compose.yml"
CADASTRE_MCP_WRAPPER_205 = ENGINE_DEPLOY / "cadastre-mcp-auth.sh"

#: Substrings that only exist in the maintainer's deployment. A shipped script
#: (or the sample compose) containing one has leaked the estate into the image.
#: `/home/sprooty` is excluded as a bare path — it is the image's own home and
#: appears as a mount *target* — so only its literal-default form is a marker.
ESTATE_MARKERS = (
    "100.92",
    "winrarhost",
    "indexarr",
    "theclawbay",
    "/mnt/2tnvme",
    "HOMELAB_",
    "GITHUB_AUSAGENTSMITH_PAT",
)


def _shipped_engine_artifacts() -> list[Path]:
    return [*sorted(ENGINE_DEPLOY.glob("*.sh")), ENGINE_SAMPLE_COMPOSE]


@needs_engine
def test_no_shipped_engine_script_names_the_estate() -> None:
    """The image ships these to every reader; the estate stays out of them.

    Comments included, deliberately: a hostname in a comment is still a
    hostname a public reader is handed, and the open-source pass is about what
    the artefact carries, not only what it executes.
    """
    offenders: list[str] = []
    for path in _shipped_engine_artifacts():
        text = path.read_text(encoding="utf-8")
        for marker in ESTATE_MARKERS:
            if marker in text:
                offenders.append(f"{path.name}: {marker!r}")
        if ":-/home/sprooty" in text:
            offenders.append(f"{path.name}: '/home/sprooty' literal default")
    assert not offenders, (
        "shipped engine scripts must carry no estate address, host path or "
        f"secret name (#205): {offenders}"
    )


@needs_engine
def test_the_entrypoint_skips_agent_auth_when_it_is_not_configured() -> None:
    """A clean clone with just a token must boot, plain shells and all (#205).

    Agent auth is a pluggable helper (`ENGINE_AGENT_AUTH_HELPER`): named or
    auto-selected from a secrets-manager identity when present, and otherwise
    absent — a stated, skipped state, never a fatal one.
    """
    body = _without_comments(ENTRYPOINT.read_text(encoding="utf-8"))
    assert "ENGINE_AGENT_AUTH_HELPER" in body, (
        "the entrypoint selects the agent-auth helper through "
        "ENGINE_AGENT_AUTH_HELPER, not a hard-coded command"
    )
    assert "agent auth not configured; skipping" in body, (
        "with no helper and no identity the entrypoint must skip agent auth "
        "cleanly, so the engine still starts and serves"
    )
    # The bundled Infisical helper is still auto-selected when an identity is
    # present, so a deployment that only sets INFISICAL_* keeps working.
    assert "mydevenv2-agent-auth" in body, (
        "the reference Infisical helper stays the auto-selected default when a "
        "machine identity is configured"
    )


@needs_engine
def test_agent_auth_requires_its_endpoints_rather_than_defaulting() -> None:
    """No baked estate address: the helper fails naming the missing variable.

    And it does so only when actually invoked — sourcing the file to reuse
    `probe_mcp` must not fail — which the shared-probe test above already
    exercises by sourcing it with nothing set.
    """
    body = _without_comments(AGENT_AUTH.read_text(encoding="utf-8"))
    assert "INFISICAL_API_URL is not set" in body, (
        "the secrets-manager API URL is required, not defaulted to an estate address"
    )
    assert "CADASTRE_MCP_URL is not set" in body, (
        "the Cadastre endpoint is required when the integration is enabled, "
        "not defaulted to the maintainer's bridge"
    )
    # The secret set and the service-probe list are configuration now, not a
    # baked-in estate map.
    assert "ENGINE_AGENT_AUTH_SECRETS" in body, (
        "which secrets to load is driven by an env manifest, not hard-coded "
        "secret names"
    )
    assert "ENGINE_AGENT_AUTH_PROBES" in body, (
        "the service-probe list is configurable via an env var (#205)"
    )
    # The optional integrations still report skip rather than fail.
    assert "skip: Cadastre MCP" in body and "skip: Vogt MCP" in body


@needs_engine
def test_the_cadastre_wrapper_skips_cleanly_without_an_endpoint() -> None:
    """A wrapper with no CADASTRE_MCP_URL is a no-op, not a crash (#205)."""
    body = _without_comments(CADASTRE_MCP_WRAPPER_205.read_text(encoding="utf-8"))
    assert 'if [[ -z "${CADASTRE_MCP_URL:-}" ]]' in body, (
        "the wrapper guards on the endpoint being configured"
    )
    assert "exit 0" in body, "an unconfigured Cadastre wrapper exits cleanly"


@needs_engine
def test_the_bootstrap_skips_cadastre_without_an_endpoint() -> None:
    """CADASTRE_MCP_ENABLED with no URL registers nothing, and says so."""
    body = _without_comments(MCP_BOOTSTRAP.read_text(encoding="utf-8"))
    assert "CADASTRE_MCP_URL:-" in body, (
        "the bootstrap has no baked Cadastre endpoint; it reads one or skips"
    )
    assert "skipping Cadastre registration" in body, (
        "enabled-but-unconfigured Cadastre is a reported skip, not a default endpoint"
    )


def _load_deploy_dev() -> Any:
    import importlib.util

    spec = importlib.util.spec_from_file_location(
        "deploy_dev", REPO_ROOT / "scripts" / "deploy_dev.py"
    )
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_deploy_dev_set_env_var_replaces_appends_and_preserves() -> None:
    """#590: the deploy-time CLI pin edits exactly one `.env` line and leaves
    every other line — secrets, tokens — byte-for-byte alone."""
    dd = _load_deploy_dev()
    env = (
        "ENGINE_TOKEN=secret\n"
        "VOGT_CODEX_VERSION=0.149.1\n"
        "MYDEVENV2_TOKEN=[[infisical://p/prod/T]]\n"
    )
    # Replace in place, reporting the previous value; nothing else moves.
    out, previous = dd.set_env_var(env, "VOGT_CODEX_VERSION", "0.153.4")
    assert previous == "0.149.1"
    assert "VOGT_CODEX_VERSION=0.153.4\n" in out
    assert "ENGINE_TOKEN=secret\n" in out
    assert "MYDEVENV2_TOKEN=[[infisical://p/prod/T]]\n" in out
    # Absent key is appended with a trailing newline, previous is None.
    out2, previous2 = dd.set_env_var(out, "VOGT_CLAUDE_CODE_VERSION", "2.1.261")
    assert previous2 is None
    assert out2.endswith("VOGT_CLAUDE_CODE_VERSION=2.1.261\n")
    # Setting the same value is a no-op that still reports the current value.
    out3, previous3 = dd.set_env_var(out2, "VOGT_CLAUDE_CODE_VERSION", "2.1.261")
    assert out3 == out2
    assert previous3 == "2.1.261"


def test_deploy_dev_version_input_is_a_plain_version() -> None:
    dd = _load_deploy_dev()
    assert dd.VERSION_INPUT.match("0.153.4")
    assert dd.VERSION_INPUT.match("2.1.261")
    assert not dd.VERSION_INPUT.match("latest; rm -rf /")
    assert not dd.VERSION_INPUT.match("")
