#!/usr/bin/env python3
"""List what origin/main changed in the Python core since rust-core branched.

    scripts/drift_ledger.py                # print the table
    scripts/drift_ledger.py --write        # also update docs/local/RUST_PORT_DRIFT.md

The ledger is operator-local and git-ignored. A `ported` mark entered by hand
survives regeneration, keyed by commit SHA. A new migration file is printed
again under its own heading, because a schema change is not optional drift.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import tomllib
from pathlib import Path

REPO = Path(__file__).resolve().parents[1]
OWNERSHIP = REPO / "tests" / "parity" / "ownership.toml"
LEDGER = REPO / "docs" / "local" / "RUST_PORT_DRIFT.md"
WATCHED = (
    "src/vogt/",
    "tests/",
    "docs/CONFIG.md",
    "config.example.toml",
)
MIGRATIONS = "src/vogt/storage/sqlite/migrations/"
PORTED = re.compile(r"^\| `([0-9a-f]{7,})` \| (yes|no) \|", re.MULTILINE)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--write", action="store_true", help=f"update {LEDGER}")
    parser.add_argument("--base", default="origin/main")
    parser.add_argument("--head", default="rust-core")
    args = parser.parse_args()

    base = _git("merge-base", args.base, args.head).strip()
    commits = _commits(base, args.base)
    previous = _previous_marks()
    table = _render(commits, previous, base, args.base)
    print(table)
    if args.write:
        LEDGER.parent.mkdir(parents=True, exist_ok=True)
        LEDGER.write_text(table)
        print(f"\nwrote {LEDGER}")
    return 0


def _git(*args: str) -> str:
    return subprocess.run(
        ["git", *args], cwd=REPO, check=True, capture_output=True, text=True
    ).stdout


def _commits(base: str, tip: str) -> list[dict[str, object]]:
    log = _git(
        "log",
        "--name-only",
        "--pretty=format:%H%x09%s",
        f"{base}..{tip}",
        "--",
        *WATCHED,
    )
    commits: list[dict[str, object]] = []
    current: dict[str, object] | None = None
    for line in log.splitlines():
        if "\t" in line:
            sha, subject = line.split("\t", 1)
            current = {"sha": sha, "subject": subject, "files": []}
            commits.append(current)
        elif line and current is not None:
            files = current["files"]
            assert isinstance(files, list)
            files.append(line)
    return [commit for commit in commits if commit["files"]]


def _chunk_for(path: str, chunks: list[tuple[str, list[str]]]) -> str:
    for name, prefixes in chunks:
        if any(path.startswith(prefix) or path == prefix for prefix in prefixes):
            return name
    return "unassigned"


def _previous_marks() -> dict[str, str]:
    if not LEDGER.is_file():
        return {}
    return {sha[:7]: mark for sha, mark in PORTED.findall(LEDGER.read_text())}


def _render(
    commits: list[dict[str, object]], previous: dict[str, str], base: str, tip: str
) -> str:
    chunks = [
        (item["name"], list(item["paths"]))
        for item in tomllib.loads(OWNERSHIP.read_text())["chunk"]
    ]
    lines = [
        "# Rust port drift",
        "",
        f"Commits on `{tip}` since `{base[:12]}` that touch the Python core.",
        "Edit the `ported` cell; regeneration keeps it.",
        "",
        "| commit | ported | chunk | subject |",
        "| --- | --- | --- | --- |",
    ]
    migrations: list[str] = []
    for commit in commits:
        sha = str(commit["sha"])
        files = [str(path) for path in commit["files"]]  # type: ignore[union-attr]
        owners = sorted({_chunk_for(path, chunks) for path in files})
        mark = previous.get(sha[:7], "no")
        lines.append(
            f"| `{sha[:7]}` | {mark} | {', '.join(owners)} | {commit['subject']} |"
        )
        migrations.extend(
            path
            for path in files
            if path.startswith(MIGRATIONS) and path.endswith(".sql")
        )
    lines.append("")
    seen = {str(commit["sha"])[:7] for commit in commits}
    kept = {
        sha: mark for sha, mark in previous.items() if sha not in seen and mark == "yes"
    }
    if kept:
        lines.append("Marks kept from commits no longer in this range:")
        lines.append("")
        for sha, mark in sorted(kept.items()):
            lines.append(f"| `{sha}` | {mark} | | |")
        lines.append("")
    lines.append("## Migrations")
    lines.append("")
    if migrations:
        lines.append("A new migration is a hard item, not optional drift.")
        lines.append("")
        lines.extend(f"- `{path}`" for path in sorted(set(migrations)))
    else:
        lines.append("None.")
    lines.append("")
    return "\n".join(lines)


if __name__ == "__main__":
    raise SystemExit(main())
