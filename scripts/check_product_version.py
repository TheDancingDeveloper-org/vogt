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
    release = (ROOT / ".github/workflows/release.yml").read_text()
    if "VOGT_PRODUCT_VERSION=${{ github.ref_name }}" not in release:
        raise SystemExit("tagged release does not inject its tag as product version")
    if '"local/dev"' not in (ROOT / "engine/server/src/product.rs").read_text():
        raise SystemExit("engine local/dev fallback is missing")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
