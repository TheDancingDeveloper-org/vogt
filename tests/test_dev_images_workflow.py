"""The rust-2 dev images are reproducible and immutable (WI-3887).

The pod image's build context is `engine/` and its Dockerfile copies the whole
tree, so its content tag has to cover that tree. Both the pod and the engine
images are published by digest with provenance, and the engine image is built
on the digest the same run published for the pod.
"""

from __future__ import annotations

import re
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
POD_DOCKERFILE = REPO_ROOT / "engine" / "Dockerfile.pod"
POD_BASE_WORKFLOW = REPO_ROOT / ".github" / "workflows" / "pod-base.yml"
DEV_IMAGES_WORKFLOW = REPO_ROOT / ".github" / "workflows" / "rust2-images.yml"


def _args(text: str) -> dict[str, str]:
    return dict(re.findall(r"^ARG\s+([A-Z0-9_]+)=(.*)$", text, re.MULTILINE))


def _from_refs(text: str) -> list[str]:
    return re.findall(r"^FROM\s+(\S+)", text, re.MULTILINE)


def test_pod_image_every_from_is_digest_pinned() -> None:
    text = POD_DOCKERFILE.read_text(encoding="utf-8")
    args = _args(text)
    for name in ("UBUNTU_IMAGE", "RUST_IMAGE"):
        assert "@sha256:" in args[name], f"{name} is not pinned by digest"

    stages = {"base", "rust"}
    for ref in _from_refs(text):
        match = re.fullmatch(r"\$\{([A-Z0-9_]+)\}", ref)
        if match:
            image = args[match.group(1)]
        elif "/" in ref:
            image = ref
        else:
            assert ref in stages, f"FROM names an unknown stage: {ref}"
            continue
        assert "@sha256:" in image, f"FROM {image} is not pinned by digest"
        assert not image.endswith(":latest"), f"FROM {image} uses :latest"


def test_pod_base_tag_covers_the_engine_tree_the_image_copies() -> None:
    dockerfile = POD_DOCKERFILE.read_text(encoding="utf-8")
    assert "\nCOPY . ." in dockerfile, "the hash rule below assumes COPY . ."

    workflow = POD_BASE_WORKFLOW.read_text(encoding="utf-8")
    assert "git ls-tree -r HEAD -- engine" in workflow
    assert "sha256sum engine/Dockerfile.pod" not in workflow


def test_dev_image_workflow_publishes_both_images_by_digest() -> None:
    workflow = DEV_IMAGES_WORKFLOW.read_text(encoding="utf-8")
    assert "workflow_dispatch:" in workflow
    assert "branches: [rust-2]" in workflow
    assert "repo.indexarr.net/indexarr/vogt-dev-pod" in workflow
    assert "repo.indexarr.net/indexarr/vogt-dev-engine" in workflow
    assert workflow.count("push-by-digest=true") == 2
    assert workflow.count("provenance: true") == 2
    assert workflow.count("sbom: true") == 2
    assert "steps.push.outputs.digest" in workflow


def test_the_engine_image_is_built_on_the_runs_pod_digest() -> None:
    workflow = DEV_IMAGES_WORKFLOW.read_text(encoding="utf-8")
    pod_ref = "${{ env.POD_IMAGE }}@${{ needs.pod.outputs.digest }}"
    assert f"POD_BASE_IMAGE={pod_ref}" in workflow
    assert f"ESTATE_IMAGE={pod_ref}" in workflow


def test_the_run_reports_the_pair_it_published() -> None:
    workflow = DEV_IMAGES_WORKFLOW.read_text(encoding="utf-8")
    assert "${ENGINE_IMAGE}@${ENGINE_DIGEST}" in workflow
    assert "${POD_IMAGE}@${POD_DIGEST}" in workflow
