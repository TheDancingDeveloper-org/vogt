#!/usr/bin/env python3
"""Check that relative links in the documentation resolve.

Small on purpose. The design documents cross-reference each other heavily,
and a broken
link between them is the kind of rot that makes a documentation set stop
being trusted. External URLs are not fetched: a link checker that depends on
the network fails for reasons that have nothing to do with the change.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
SKIP_DIRS = {".git", ".venv", "node_modules", "dist", "build", ".mypy_cache", "local"}
LINK = re.compile(r"\[[^\]]*\]\(([^)\s]+)(?:\s+\"[^\"]*\")?\)")
EXTERNAL = ("http://", "https://", "mailto:", "tel:")
HEADING = re.compile(r"^(#{1,6})\s+(.*?)\s*#*\s*$", re.MULTILINE)


def slugify(heading: str) -> str:
    """GitHub's heading-anchor slug: lowercase, drop anything but word chars,
    spaces and hyphens, then spaces to hyphens. Enough for our own docs, which
    use plain-text headings (a few with numbers and dots, like `7.4 Upgrade`).
    """
    text = heading.strip().lower()
    text = re.sub(r"[^\w\s-]", "", text)
    return re.sub(r"\s+", "-", text)


def anchors(path: Path) -> set[str]:
    """Every in-page anchor a link may target: one per heading, with GitHub's
    `-1`, `-2` … disambiguation for repeated slugs.
    """
    found: set[str] = set()
    seen: dict[str, int] = {}
    for match in HEADING.finditer(path.read_text(encoding="utf-8")):
        base = slugify(match.group(2))
        if not base:
            continue
        count = seen.get(base, 0)
        slug = base if count == 0 else f"{base}-{count}"
        seen[base] = count + 1
        found.add(slug)
    return found


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


def broken_links(path: Path, anchor_cache: dict[Path, set[str]]) -> list[str]:
    problems: list[str] = []
    for match in LINK.finditer(path.read_text(encoding="utf-8")):
        target = match.group(1)
        if target.startswith("/"):
            # An absolute filesystem path is not a relative link, and checking
            # whether it exists asks about *this* machine, not the repository:
            # such a link can pass locally and fail on any other checkout.
            problems.append(f"{target} (an absolute path is not a link)")
            continue
        if target.startswith(EXTERNAL):
            continue
        relative, _, fragment = target.partition("#")
        if not relative:
            # A same-page anchor: the target file is this one.
            resolved = path
        else:
            resolved = (path.parent / relative).resolve()
            if not resolved.exists():
                problems.append(f"{path.relative_to(REPO_ROOT)} -> {target}")
                continue
        # A fragment must name a heading in the target markdown file. Only
        # checked for markdown targets — a fragment on another file type is not
        # a heading anchor.
        if fragment and resolved.suffix == ".md":
            if resolved not in anchor_cache:
                anchor_cache[resolved] = anchors(resolved)
            if fragment not in anchor_cache[resolved]:
                problems.append(
                    f"{path.relative_to(REPO_ROOT)} -> {target} "
                    f"(no heading anchor #{fragment})"
                )
    return problems


def main() -> int:
    problems: list[str] = []
    files = markdown_files()
    anchor_cache: dict[Path, set[str]] = {}
    for path in files:
        problems.extend(broken_links(path, anchor_cache))

    if problems:
        print(f"broken relative links ({len(problems)}):")
        for problem in problems:
            print(f"  {problem}")
        return 1

    print(f"checked {len(files)} markdown files; all relative links resolve")
    return 0


if __name__ == "__main__":
    sys.exit(main())
