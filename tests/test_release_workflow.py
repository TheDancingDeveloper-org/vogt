"""The release is *build once, promote by digest* (`.github/workflows/release.yml`).

`build.yml` builds, smoke-tests, signs and publishes the three images once per
commit on `main`; the dev lane deploys and validates those digests; a `v*` tag
then retags and release-signs the same digests. These tests pin the shape that
makes that true, because each property is one innocent-looking edit away from
quietly going back to "a release rebuilds something the dev lane never ran".
"""

from __future__ import annotations

import re
from pathlib import Path

import pytest

from check_workflow_policy import parse_workflow, timeout_problems

ROOT = Path(__file__).resolve().parents[1]
WORKFLOWS = ROOT / ".github" / "workflows"
RELEASE = WORKFLOWS / "release.yml"
BUILD = WORKFLOWS / "build.yml"

pytestmark = pytest.mark.skipif(
    not RELEASE.is_file(), reason="workflows are absent from the core-only test image"
)

IMAGES = ("IMAGE", "STACK_IMAGE", "VOICE_IMAGE")


def _release() -> str:
    return RELEASE.read_text(encoding="utf-8")


def _group(pattern: str, text: str, flags: int = 0) -> str:
    match = re.search(pattern, text, flags)
    assert match, pattern
    return match.group(1)


def _job_block(text: str, job_id: str) -> str:
    match = re.search(
        rf"^  {re.escape(job_id)}:\n(.*?)(?=^  [A-Za-z0-9_-]+:\n|\Z)", text, re.M | re.S
    )
    assert match, f"release.yml has no job {job_id!r}"
    return match.group(1)


def test_release_builds_no_image() -> None:
    text = _release()
    assert "docker/build-push-action" not in text
    assert "VOGT_PRODUCT_VERSION=" not in text
    jobs = {job.job_id for job in parse_workflow(str(RELEASE), text).jobs}
    # The build prerequisites went with the builds: nothing here needs a base.
    assert "base-images" not in jobs
    assert "pod-base" not in jobs
    assert {
        "validate-release",
        "distribution",
        "build-run",
        "promote",
        "release-pair",
        "android",
        "github-release",
    } <= jobs


def test_every_release_job_times_out_on_a_self_hosted_runner() -> None:
    text = _release()
    workflow = parse_workflow(str(RELEASE), text)
    assert timeout_problems(workflow) == []
    for line in text.splitlines():
        if line.strip().startswith("runs-on:"):
            assert "self-hosted" in line


def test_the_images_promoted_are_the_ones_build_yml_publishes() -> None:
    release, build = _release(), BUILD.read_text(encoding="utf-8")
    for name in IMAGES:
        pattern = rf"^  {name}: (\S+)$"
        assert _group(pattern, release, re.M) == _group(pattern, build, re.M)
    # build.yml tags a main commit `sha-<7>` (metadata-action `type=sha`);
    # the release finds the digest by exactly that tag.
    assert build.count("type=sha,enable=${{ github.ref == 'refs/heads/main' }}") == 3
    assert "BUILD_TAG_PREFIX: sha-" in release
    assert 'BUILD_TAG_SHA_LENGTH: "7"' in release


def test_the_build_run_gate_names_build_yml_jobs_that_exist() -> None:
    build_names = {
        job.name
        for job in parse_workflow(str(BUILD), BUILD.read_text(encoding="utf-8")).jobs
    }
    block = _job_block(_release(), "build-run")
    required = _group(r"REQUIRED_JOBS: \|\n((?:\s{12}.+\n)+)", block)
    names = [line.strip() for line in required.splitlines() if line.strip()]
    assert len(names) == 3
    assert set(names) <= build_names, set(names) - build_names
    assert "actions/workflows/build.yml/runs?head_sha=" in block
    assert "branch=main" in block


def test_promotion_verifies_then_retags_and_signs_by_digest() -> None:
    block = _job_block(_release(), "promote")
    # Only what build.yml signed, on main, at this exact commit, is promoted.
    assert "/.github/workflows/build.yml@refs/heads/main" in block
    assert '--certificate-github-workflow-sha "$TAG_SHA"' in block
    assert "{{json .${kind}}}" in block and "SBOM Provenance" in block
    # Registry-side retag of the digest, re-checked afterwards.
    # `--prefer-index=false` keeps buildx from wrapping a single manifest in
    # a new index: the deployment's gate compares these digests exactly.
    assert "docker buildx imagetools create --prefer-index=false" in block
    assert '"${args[@]}" "${repo}@${digest}"' in block
    assert 'if [ "$got" != "$digest" ]; then' in block
    assert 'tags="$version ${version%.*} latest"' in block
    # Signed by digest, never by tag, under this workflow's own identity.
    for env in ("IMAGE", "STACK_IMAGE", "VOICE_IMAGE"):
        assert re.search(
            rf'cosign sign --yes "\${{{env}}}@\$\{{\w+_DIGEST\}}"', block
        ), env
    assert "id-token: write" in block
    # No silent fallback: the failure is an error, not a rebuild.
    assert "Nothing is rebuilt" in block


def test_the_manifest_records_the_source_and_every_digest() -> None:
    block = _job_block(_release(), "github-release")
    assert '"vogt-release-manifest.v1"' in block
    for key in (
        "source_sha:",
        "image_digests:",
        "release_tags:",
        "promotion:",
        'method: "build-once-promote-by-digest"',
        "rebuilt: false",
        "build_run_id:",
        "cosign_identities:",
    ):
        assert key in block, key
    for need in ("build-run", "promote", "android", "release-pair"):
        assert need in _group(r"needs: \[(.*)\]", block)


def test_the_android_shell_is_still_built_at_tag_time() -> None:
    block = _job_block(_release(), "android")
    assert "./gradlew assembleRelease" in block
    assert "apksigner" in block
