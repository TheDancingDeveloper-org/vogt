#!/usr/bin/env python3
"""Prove that demo packaging added metadata without rebuilding the PWA."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path

DEMO_ONLY = {"demo-manifest.json", "demo-gui.html"}


def digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--normal", type=Path, required=True)
    parser.add_argument("--demo", type=Path, required=True)
    parser.add_argument("--source-sha", required=True)
    args = parser.parse_args()

    if not args.source_sha or len(args.source_sha) != 40:
        parser.error("--source-sha must be the 40-character commit SHA")

    build = json.loads((args.normal / "demo-build.json").read_text())
    demo_build = json.loads((args.demo / "demo-build.json").read_text())
    manifest = json.loads((args.demo / "demo-manifest.json").read_text())
    assert build == demo_build, "demo-build.json changed while augmenting the artifact"
    assert build["source_sha"] == args.source_sha
    assert manifest["source_sha"] == args.source_sha
    assert manifest["scenario"] == "full-estate-v1"
    assert not (args.normal / "demo-manifest.json").exists()
    assert not (args.normal / "demo-gui.html").exists()

    recorded = build.get("assets", {})
    assert recorded, "the asset manifest is empty"
    for name, expected in recorded.items():
        normal = args.normal / name
        demo = args.demo / name
        assert normal.is_file(), f"normal artifact is missing {name}"
        assert demo.is_file(), f"demo artifact is missing {name}"
        assert digest(normal) == expected, f"normal asset hash drift: {name}"
        assert digest(demo) == expected, f"demo asset hash drift: {name}"

    unrecorded = (
        {
            str(path.relative_to(args.demo))
            for path in args.demo.rglob("*")
            if path.is_file()
        }
        - set(recorded)
        - {".vite/manifest.json", "demo-build.json"}
        - DEMO_ONLY
    )
    assert not unrecorded, f"unrecorded demo assets: {sorted(unrecorded)}"
    print(f"{len(recorded)} PWA assets are byte-identical; demo provenance is current")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
