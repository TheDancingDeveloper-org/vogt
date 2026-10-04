"""Check that every user-facing product manifest carries one version."""

from __future__ import annotations

import json
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def main() -> int:
    expected = sys.argv[1] if len(sys.argv) > 1 else None
    project = tomllib.loads((ROOT / "pyproject.toml").read_text())["project"]
    version = str(project["version"])
    if expected is not None and version != expected:
        raise SystemExit(
            f"pyproject version {version} does not match expected {expected}"
        )
    if f'__version__ = "{version}"' not in (ROOT / "src/vogt/__init__.py").read_text():
        raise SystemExit("Python package version disagrees with pyproject.toml")
    for relative in ("web/package.json", "mobile/package.json"):
        if json.loads((ROOT / relative).read_text()).get("version") != version:
            raise SystemExit(f"{relative} disagrees with product version {version}")
    build = (ROOT / ".github/workflows/build.yml").read_text()
    if f"VOGT_PRODUCT_VERSION={version}" not in build:
        raise SystemExit("dev build does not inject the canonical product version")
    # A release promotes the images build.yml built for the tagged commit
    # (build once, promote by digest), so the version above, baked by build.yml,
    # is the one a release ships. A release that built again would ship bytes
    # the dev lane never ran, with whatever version it chose to inject.
    release = (ROOT / ".github/workflows/release.yml").read_text()
    if "docker/build-push-action" in release or "VOGT_PRODUCT_VERSION=" in release:
        raise SystemExit(
            "tagged release must promote build.yml's images, not rebuild them"
        )
    if "docker buildx imagetools create" not in release:
        raise SystemExit("tagged release does not promote build.yml's images by digest")
    if '"local/dev"' not in (ROOT / "engine/server/src/product.rs").read_text():
        raise SystemExit("engine local/dev fallback is missing")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
