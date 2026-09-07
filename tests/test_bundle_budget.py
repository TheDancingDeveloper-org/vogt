"""#538: the first-screen bundle budget must count what index.tsx loads before
paint. index.tsx `await`s a handful of dynamic imports in a `Promise.all` before
`render`, so they are first-screen even though they are dynamic. check_bundle.py
lists them in STARTUP_DYNAMIC; if index.tsx's startup imports change and the list
does not, the gate silently goes blind again (the exact regression #538 fixed).
This guards that they stay in step.
"""

from __future__ import annotations

import importlib.util
import re
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
INDEX = REPO_ROOT / "web" / "src" / "index.tsx"
CHECK_BUNDLE = REPO_ROOT / "scripts" / "check_bundle.py"


def _startup_dynamic() -> tuple[str, ...]:
    spec = importlib.util.spec_from_file_location("check_bundle", CHECK_BUNDLE)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return tuple(module.STARTUP_DYNAMIC)


def test_startup_roots_match_index_tsx() -> None:
    source = INDEX.read_text(encoding="utf-8")
    # index.tsx's only top-level `import("./X")` calls are its startup Promise.all
    # (route-lazy imports live in routes.ts / App.tsx, not here).
    in_index = set(re.findall(r'import\("\./(\w+)"\)', source))
    assert in_index, "index.tsx has no startup dynamic imports — did the shape change?"
    listed = set(_startup_dynamic())
    assert in_index == listed, (
        "check_bundle.py's STARTUP_DYNAMIC has drifted from index.tsx's startup "
        f"imports.\n  index.tsx awaits: {sorted(in_index)}\n  budget counts:  "
        f"{sorted(listed)}\nUpdate STARTUP_DYNAMIC so the first-screen budget "
        "stays honest (#538)."
    )
