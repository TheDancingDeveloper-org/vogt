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


def markdown_files() -> list[Path]:
    return sorted(
        path
        for path in REPO_ROOT.rglob("*.md")
        if not SKIP_DIRS & set(path.relative_to(REPO_ROOT).parts)
    )


def broken_links(path: Path) -> list[str]:
    problems: list[str] = []
    for match in LINK.finditer(path.read_text(encoding="utf-8")):
        target = match.group(1)
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
