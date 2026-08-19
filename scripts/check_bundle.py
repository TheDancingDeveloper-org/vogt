#!/usr/bin/env python3
"""What the PWA's first screen costs, and a budget it has to stay inside.

The production build once shipped one 848 kB chunk: every place, the terminal
(xterm) and the editor (Monaco) downloaded before anything was drawn, whether
or not the reader ever opened a terminal. Splitting them out is the fix; this
script is what keeps them split, because a lazy import is one careless static
`import` away from being eager again and nothing about the build says so.

What it measures is the *initial graph*: the entry chunk, everything the entry
statically imports (transitively), and the stylesheets those pull in. A chunk
reached only through a dynamic import is not in it — that is the whole point —
so the numbers below are what a cold visit to `/` transfers before the app can
render, and not the size of `dist/`.

Run after `pnpm build`, from anywhere:

    uv run python scripts/check_bundle.py

`--record` prints the table without enforcing anything, for a before/after.
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path

WEB = Path(__file__).resolve().parent.parent / "web"
DIST = WEB / "dist"
MANIFEST = DIST / ".vite" / "manifest.json"

# The budgets. Raw bytes, not gzip: gzip depends on the server's settings and
# these have to hold whatever the front door does. Both were set from the
# measured build that first split the bundle (JS 217 kB, CSS 114 kB) with room
# for ordinary growth — they are a ratchet against another 600 kB arriving
# unnoticed, not a limbo bar.
JS_BUDGET = 400 * 1024
CSS_BUDGET = 200 * 1024

# Chunks a first screen must never contain, by the module that gives them
# away. A file whose name carries one of these is an editor or terminal chunk,
# and the acceptance criterion is that a reader on Board, Inbox or Backlog
# never fetches one.
DEFERRED_MARKERS = (
    "monaco",
    "editor.api",
    "standaloneServices",
    "xterm",
    ".worker",
    "TerminalWorkspace",
    "EditorWorkspace",
)


def load_manifest() -> dict[str, dict]:
    if not MANIFEST.exists():
        sys.exit(
            f"no build manifest at {MANIFEST}.\n"
            "Run `pnpm build` in web/ first; `build.manifest` must stay true."
        )
    return json.loads(MANIFEST.read_text())


def initial_graph(manifest: dict[str, dict]) -> tuple[set[str], set[str]]:
    """The entry, what it statically imports, and the CSS that comes with it."""
    entries = [key for key, chunk in manifest.items() if chunk.get("isEntry")]
    if not entries:
        sys.exit("the manifest declares no entry chunk")

    js: set[str] = set()
    css: set[str] = set()
    seen: set[str] = set()

    def walk(key: str) -> None:
        if key in seen:
            return
        seen.add(key)
        chunk = manifest.get(key)
        if chunk is None:
            return
        js.add(chunk["file"])
        css.update(chunk.get("css", []))
        # `imports` is static; `dynamicImports` is deliberately not walked.
        for name in chunk.get("imports", []):
            walk(name)

    for entry in entries:
        walk(entry)
    return js, css


def size_of(file: str) -> int:
    path = DIST / file
    return path.stat().st_size if path.exists() else 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--record",
        action="store_true",
        help="print the measurement without enforcing the budget",
    )
    args = parser.parse_args()

    manifest = load_manifest()
    js, css = initial_graph(manifest)

    print("the first screen:")
    total_js = 0
    for file in sorted(js, key=lambda name: -size_of(name)):
        size = size_of(file)
        total_js += size
        print(f"  {size / 1024:8.1f} kB  {file}")
    total_css = 0
    for file in sorted(css, key=lambda name: -size_of(name)):
        size = size_of(file)
        total_css += size
        print(f"  {size / 1024:8.1f} kB  {file}")

    print(
        f"\n  JS  {total_js / 1024:7.1f} kB of {JS_BUDGET / 1024:.0f} kB"
        f"\n  CSS {total_css / 1024:7.1f} kB of {CSS_BUDGET / 1024:.0f} kB"
    )

    deferred = sorted(
        chunk["file"]
        for chunk in manifest.values()
        if chunk["file"] not in js
        and any(marker in chunk["file"] for marker in DEFERRED_MARKERS)
    )
    if deferred:
        print(f"\n  deferred behind a route: {len(deferred)} editor/terminal chunks")

    if args.record:
        return 0

    failures: list[str] = []
    eager = sorted(
        file for file in js if any(marker in file for marker in DEFERRED_MARKERS)
    )
    if eager:
        failures.append(
            "the first screen pulls in editor or terminal code: "
            + ", ".join(eager)
            + "\n  Something imported it statically. Import it through "
            "`lazy()` or a dynamic `import()` instead."
        )
    if total_js > JS_BUDGET:
        failures.append(
            f"initial JS is {total_js / 1024:.1f} kB, over the "
            f"{JS_BUDGET / 1024:.0f} kB budget"
        )
    if total_css > CSS_BUDGET:
        failures.append(
            f"initial CSS is {total_css / 1024:.1f} kB, over the "
            f"{CSS_BUDGET / 1024:.0f} kB budget"
        )

    if failures:
        print("\nthe bundle budget is not met:", file=sys.stderr)
        for failure in failures:
            print(f"  - {failure}", file=sys.stderr)
        return 1

    print("\nthe first screen is inside its budget")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
