"""Check that a Cargo workspace's declared `rust-version` is a real floor.

A `rust-version` older than what the lockfile's crates require is a promise
nothing keeps: a toolchain at the declared version cannot build the locked
tree. This reads `cargo metadata --locked` for one workspace and fails when
any resolved crate declares a newer `rust-version` than a workspace member
does, naming the crates that set the floor.

Usage: python scripts/check_rust_version.py engine voice
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]

Version = tuple[int, int, int]


def parse_version(text: str) -> Version:
    """`1.88` and `1.88.0` are the same floor."""
    parts = [int(part) for part in text.split(".")]
    parts += [0] * (3 - len(parts))
    return parts[0], parts[1], parts[2]


def show(version: Version) -> str:
    major, minor, patch = version
    return f"{major}.{minor}" if patch == 0 else f"{major}.{minor}.{patch}"


def problems(metadata: dict[str, Any]) -> list[str]:
    """Every workspace member whose declared floor is below its locked tree's."""
    members = set(metadata["workspace_members"])
    resolved = {node["id"] for node in metadata["resolve"]["nodes"]}
    floor: Version = (0, 0, 0)
    setters: list[str] = []
    for package in metadata["packages"]:
        if package["id"] in members or package["id"] not in resolved:
            continue
        declared = package.get("rust_version")
        if not declared:
            continue
        version = parse_version(declared)
        label = f"{package['name']} {package['version']}"
        if version > floor:
            floor, setters = version, [label]
        elif version == floor:
            setters.append(label)

    found: list[str] = []
    for package in metadata["packages"]:
        if package["id"] not in members:
            continue
        declared = package.get("rust_version")
        if not declared:
            found.append(
                f"{package['name']} declares no rust-version; the lockfile "
                f"needs {show(floor)}"
            )
        elif parse_version(declared) < floor:
            found.append(
                f"{package['name']} declares rust-version {declared}, but the "
                f"lockfile needs {show(floor)} ({', '.join(sorted(setters)[:5])})"
            )
    return found


def main() -> int:
    workspaces = sys.argv[1:] or ["engine", "voice"]
    failed = False
    for workspace in workspaces:
        output = subprocess.run(
            ["cargo", "metadata", "--locked", "--format-version", "1"],
            cwd=ROOT / workspace,
            check=True,
            capture_output=True,
            text=True,
        ).stdout
        found = problems(json.loads(output))
        for problem in found:
            print(f"{workspace}: {problem}", file=sys.stderr)
        if found:
            failed = True
        else:
            print(f"{workspace}: declared rust-version covers the lockfile")
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
