#!/usr/bin/env python3
"""Pin Vogt's merged image in its estate desired state, without guessing."""

from __future__ import annotations

import argparse
import re
from pathlib import Path

IMAGE = "ghcr.io/thedancingdeveloper-org/vogt-stack"
DIGEST = re.compile(r"sha256:[0-9a-f]{64}\Z")
COMPOSE_IMAGE_LINE = re.compile(
    rf"^(?P<indent>\s*image:\s*){re.escape(IMAGE)}@sha256:[0-9a-f]{{64}}\s*$",
    re.MULTILINE,
)


def _check_digest(digest: str) -> None:
    if not DIGEST.fullmatch(digest):
        msg = f"not an immutable sha256 digest: {digest!r}"
        raise ValueError(msg)


def pin_environment(environment: str, digest: str) -> tuple[str, bool]:
    """Replace exactly one VOGT_STACK_IMAGE assignment."""
    _check_digest(digest)
    image_line = re.compile(
        rf"^(VOGT_STACK_IMAGE={re.escape(IMAGE)}@)sha256:[0-9a-f]{{64}}$",
        re.MULTILINE,
    )
    updated, count = image_line.subn(rf"\g<1>{digest}", environment)
    if count != 1:
        msg = "expected exactly one VOGT_STACK_IMAGE merged-image digest pin"
        raise ValueError(msg)
    return updated, updated != environment


def pin_image(path: Path, digest: str) -> bool:
    """Replace exactly one merged-image pin and return whether it changed."""
    _check_digest(digest)

    text = path.read_text(encoding="utf-8")
    replacement = rf"\g<indent>{IMAGE}@{digest}"
    updated, count = COMPOSE_IMAGE_LINE.subn(replacement, text)
    if count != 1:
        msg = f"expected exactly one {IMAGE} digest pin in {path}, found {count}"
        raise ValueError(msg)
    if updated == text:
        return False
    path.write_text(updated, encoding="utf-8")
    return True


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("compose", type=Path)
    parser.add_argument("digest", help="immutable sha256:<64 lowercase hex> digest")
    args = parser.parse_args()
    print("updated" if pin_image(args.compose, args.digest) else "already pinned")


if __name__ == "__main__":
    main()
