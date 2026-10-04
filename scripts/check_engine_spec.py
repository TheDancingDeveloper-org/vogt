#!/usr/bin/env python3
"""Check that `docs/engine-openapi.yaml` and the engine's routes agree.

The engine's OpenAPI document is hand-written, so it drifts the moment a
route is added without it. This catches the two drifts that matter to an
integrator, without parsing YAML or Rust properly:

- every `.route("/api/sessions…")` in `engine/server/src/app.rs` must have a
  path in the spec — the session API is the part the spec promises to cover;
- every path in the spec must be a route the engine registers — a documented
  route that does not exist is worse than an undocumented one.

Deterministic and offline: two regular expressions over two text files.
Run on its own, from `scripts/check_docs.py`, and from pytest.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
APP_RS = REPO_ROOT / "engine" / "server" / "src" / "app.rs"
SPEC = REPO_ROOT / "docs" / "engine-openapi.yaml"

#: The route prefix the spec must cover in full.
COVERED_PREFIX = "/api/sessions"

#: `.route(` then, possibly across lines, a string literal path. Routes built
#: from constants (`.route(secret_broker::FETCH_ROUTE, …)`) are not literals
#: and are not seen; none of them is under the covered prefix.
ROUTE = re.compile(r"\.route\(\s*\"(/[^\"]*)\"")

#: A path key under `paths:` — two-space indent, starting with `/`.
SPEC_PATH = re.compile(r"^  (/[^:\s]*):\s*$", re.MULTILINE)


def engine_routes(app_rs: str) -> set[str]:
    """Every literal route path registered in `app.rs`."""
    return set(ROUTE.findall(app_rs))


def spec_paths(spec: str) -> set[str]:
    """Every path key in the OpenAPI document's `paths:` block."""
    _, sep, rest = spec.partition("\npaths:\n")
    if not sep:
        return set()
    # The block ends at the next top-level key.
    block = re.split(r"\n(?=[A-Za-z])", rest, maxsplit=1)[0]
    return set(SPEC_PATH.findall("\n" + block))


def problems(app_rs: str, spec: str) -> list[str]:
    routes = engine_routes(app_rs)
    paths = spec_paths(spec)
    found: list[str] = []
    for route in sorted(r for r in routes if r.startswith(COVERED_PREFIX)):
        if route not in paths:
            found.append(f"{route} is an engine route with no path in the spec")
    for path in sorted(paths - routes):
        found.append(f"{path} is in the spec but is not an engine route")
    return found


def check_repository() -> list[str]:
    """The check against this checkout's `app.rs` and spec."""
    app_rs = APP_RS.read_text(encoding="utf-8")
    return problems(app_rs, SPEC.read_text(encoding="utf-8"))


def main() -> int:
    if not APP_RS.exists() or not SPEC.exists():
        # A partial checkout (the CI job that runs with most of the tree
        # removed) has nothing to compare.
        print("engine spec check skipped: app.rs or the spec is absent")
        return 0
    found = check_repository()
    if found:
        where = SPEC.relative_to(REPO_ROOT)
        print(f"engine OpenAPI spec drift ({len(found)}) in {where}:")
        for problem in found:
            print(f"  {problem}")
        return 1
    print("engine OpenAPI spec covers every session route")
    return 0


if __name__ == "__main__":
    sys.exit(main())
