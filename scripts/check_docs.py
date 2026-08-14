#!/usr/bin/env python3
"""Check that relative links in the documentation resolve.

Small on purpose. The design documents cross-reference each other heavily
(`DESIGN.md` §5 → `REQUIREMENTS.md` FR-G14 → `ROADMAP.md` M3), and a broken
link between them is the kind of rot that makes a documentation set stop
being trusted. External URLs are not fetched: a link checker that depends on
the network fails for reasons that have nothing to do with the change.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
SKIP_DIRS = {".git", ".venv", "node_modules", "dist", "build", ".mypy_cache"}
LINK = re.compile(r"\[[^\]]*\]\(([^)\s]+)(?:\s+\"[^\"]*\")?\)")
EXTERNAL = ("http://", "https://", "mailto:", "tel:")


def in_nested_checkout(path: Path) -> bool:
    """Is this file inside another checkout of something?

    A git worktree — an agent's branch, a bisect, a release build — is a whole
    second copy of the tree living under this one, and its documents are the
    *other* branch's problem. Checking them here reports work in progress as
    rot in `main`, and the first time it happened it reported eighteen broken
    links that were already fixed on the branch being checked.

    Detected by the marker rather than by name: a worktree's `.git` is a file,
    a clone's is a directory, and a rule about either would miss the other.
    """
    for parent in path.parents:
        if parent == REPO_ROOT:
            return False
        if (parent / ".git").exists():
            return True
    return False


def markdown_files() -> list[Path]:
    return sorted(
        path
        for path in REPO_ROOT.rglob("*.md")
        if not SKIP_DIRS & set(path.relative_to(REPO_ROOT).parts)
        and not in_nested_checkout(path)
    )


def broken_links(path: Path) -> list[str]:
    problems: list[str] = []
    for match in LINK.finditer(path.read_text(encoding="utf-8")):
        target = match.group(1)
        if target.startswith("/"):
            # An absolute filesystem path is not a relative link, and checking
            # whether it exists asks about *this* machine. One such link —
            # `/home/sprooty/Working/AGENTS.md` — passed here for a day and
            # failed on the first CI run, because the estate guide is outside
            # this repository and the runner has no such directory.
            problems.append(f"{target} (an absolute path is not a link)")
            continue
        if target.startswith(EXTERNAL) or target.startswith("#"):
            continue
        relative, _, _ = target.partition("#")
        if not relative:
            continue
        resolved = (path.parent / relative).resolve()
        if not resolved.exists():
            problems.append(f"{path.relative_to(REPO_ROOT)} -> {target}")
    return problems


def main() -> int:
    problems: list[str] = []
    files = markdown_files()
    for path in files:
        problems.extend(broken_links(path))

    if problems:
        print(f"broken relative links ({len(problems)}):")
        for problem in problems:
            print(f"  {problem}")
        return 1

    print(f"checked {len(files)} markdown files; all relative links resolve")
    return 0


if __name__ == "__main__":
    sys.exit(main())
