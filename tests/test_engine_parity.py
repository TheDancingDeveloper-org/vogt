"""Regenerator and freshness check for the engine-route parity golden.

The engine's routes are Rust handlers the PWA calls directly, so the GUI can
gain a capability agents lack. `src/vogt/registry/engine_routes.py` holds the
tables (`ENGINE_COUNTERPARTS`, `ENGINE_ONLY`); this file dumps them, with the
MCP registry entries the counterparts name, to
`tests/fixtures/engine_parity.json`.

`scripts/check-engine-files.mjs` reads that golden and runs every route
check (router vs tables, stale entries, overlap, reasons, PWA paths, and each
counterpart being an MCP operation) with no Python. What stays here, because
it needs the live Python registry, is the one check that the golden still
matches the tables and the registry.

    uv run python tests/test_engine_parity.py --regenerate
"""

from __future__ import annotations

import json
import sys
from pathlib import Path

from vogt.registry import ENGINE_COUNTERPARTS, ENGINE_ONLY, default_registry

GOLDEN = Path(__file__).resolve().parent / "fixtures" / "engine_parity.json"


def build() -> dict[str, object]:
    """The golden's content: sorted, so the file is deterministic."""
    registry = default_registry()
    mcp_operations = sorted(
        name
        for name in set(ENGINE_COUNTERPARTS.values())
        if name in registry and "mcp" in registry.transports_for(name)
    )
    return {
        "engine_counterparts": dict(sorted(ENGINE_COUNTERPARTS.items())),
        "engine_only": dict(sorted(ENGINE_ONLY.items())),
        "mcp_operations": mcp_operations,
    }


def render() -> str:
    return json.dumps(build(), indent=2, sort_keys=True) + "\n"


def test_engine_parity_golden_is_current() -> None:
    assert GOLDEN.read_text("utf-8") == render(), (
        "tests/fixtures/engine_parity.json is out of date: run "
        "`uv run python tests/test_engine_parity.py --regenerate`"
    )


if __name__ == "__main__":
    if "--regenerate" not in sys.argv[1:]:
        sys.exit("usage: test_engine_parity.py --regenerate")
    GOLDEN.write_text(render(), "utf-8")
    print(f"wrote {GOLDEN}")
