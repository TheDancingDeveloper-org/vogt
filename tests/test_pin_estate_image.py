from __future__ import annotations

import importlib.util
from pathlib import Path

import pytest

SCRIPT = Path(__file__).parents[1] / "scripts" / "pin_estate_image.py"
SPEC = importlib.util.spec_from_file_location("pin_estate_image", SCRIPT)
assert SPEC and SPEC.loader
module = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(module)


def test_pin_image_replaces_one_immutable_merged_image_pin(tmp_path: Path) -> None:
    compose = tmp_path / "estate.overlay.yml"
    old = "a" * 64
    new = "b" * 64
    compose.write_text(
        "services:\n  engine:\n"
        f"    image: ghcr.io/thedancingdeveloper-org/vogt-stack@sha256:{old}\n"
    )

    assert module.pin_image(compose, f"sha256:{new}")
    assert f"vogt-stack@sha256:{new}" in compose.read_text()
    assert not module.pin_image(compose, f"sha256:{new}")


def test_pin_environment_replaces_only_the_merged_stack_pin() -> None:
    old = "a" * 64
    new = "b" * 64
    environment = (
        "KEEP=this\n"
        f"VOGT_STACK_IMAGE=ghcr.io/thedancingdeveloper-org/vogt-stack@sha256:{old}\n"
        "SECRET=not-to-log\n"
    )

    updated, changed = module.pin_environment(environment, f"sha256:{new}")
    assert changed
    assert (
        f"VOGT_STACK_IMAGE=ghcr.io/thedancingdeveloper-org/vogt-stack@sha256:{new}"
        in updated
    )
    assert "SECRET=not-to-log" in updated


@pytest.mark.parametrize("digest", ["v0.3.0", "sha256:ABC", "sha256:" + "a" * 63])
def test_pin_image_rejects_non_immutable_digests(tmp_path: Path, digest: str) -> None:
    compose = tmp_path / "estate.overlay.yml"
    compose.write_text("services: {}\n")

    with pytest.raises(ValueError, match="immutable"):
        module.pin_image(compose, digest)
