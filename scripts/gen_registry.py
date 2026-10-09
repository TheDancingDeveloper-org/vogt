#!/usr/bin/env python3
"""Regenerate the Rust operation registry from the Python one.

The Rust registry in `engine/core/src/registry/operations.rs` is derived, not
authored: it carries every operation's name, summary, scope, mutation flag,
route and CLI path exactly as `src/vogt/registry/operations.py` defines them.
Hand-editing it is how the Rust side fell behind (it shipped 122 operations
against a Python tree that had grown to 134). Run this after changing the
Python operation set; the committed golden at
`tests/parity/golden/registry.json` is what the Rust dump is checked against.

The golden is recorded from Python's own `registry.dump`, with the parameter
and result schemas replaced by ``{"not_ported": true}``. Real schemas arrive
with the schema emitter (WI-1060); until then the marker is honest, and it is
deliberately not hidden by a normaliser rule.
"""

from __future__ import annotations

import ast
import json
import re
import sys
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(REPO_ROOT / "src"))

OPERATIONS_RS = REPO_ROOT / "engine" / "core" / "src" / "registry" / "operations.rs"
GOLDEN = REPO_ROOT / "tests" / "parity" / "golden" / "registry.json"

SCOPES = {
    "read": "Read",
    "work.write": "WorkWrite",
    "project.write": "ProjectWrite",
    "admin": "Admin",
    "writeback": "Writeback",
}
METHODS = {"GET": "Get", "POST": "Post", "PATCH": "Patch", "DELETE": "Delete"}

HEADER = """\
//! The operation set, generated from `src/vogt/registry/operations.py`.
//!
//! Produced by `scripts/gen_registry.py`. Do not edit by hand: the generator
//! is what stops the Rust registry drifting behind the Python one, and a test
//! checks the dump against a golden recorded from Python. Every handler is
//! `not_ported` until its service lands, except `registry.dump`, which the
//! registry implements itself.

use super::{CliBinding, HttpMethod, HttpRoute, Operation, Scope};

"""


def _operations() -> list[dict[str, object]]:
    """Read the operation set from source, so this runs without importing it."""
    tree = ast.parse((REPO_ROOT / "src" / "vogt" / "registry" / "operations.py").read_text())
    found: list[dict[str, object]] = []
    for node in ast.walk(tree):
        if not (isinstance(node, ast.Call) and isinstance(node.func, ast.Name) and node.func.id == "Operation"):
            continue
        kw = {keyword.arg: keyword.value for keyword in node.keywords}
        found.append(
            {
                "name": ast.literal_eval(kw["name"]),
                "summary": ast.literal_eval(kw["summary"]),
                "scope": ast.literal_eval(kw["scope"]),
                "mutating": ast.literal_eval(kw["mutating"]),
                "method": ast.literal_eval(kw["route"].args[0]),
                "path": ast.literal_eval(kw["route"].args[1]),
                "cli": list(ast.literal_eval(kw["cli"].args[0])),
            }
        )
    return found


def render_operations_rs(operations: list[dict[str, object]]) -> str:
    lines = [HEADER, "pub fn build_operations() -> Vec<Operation> {\n    vec![\n"]
    for operation in operations:
        cli = ", ".join(json.dumps(part) for part in operation["cli"])  # type: ignore[union-attr]
        lines.append(
            "        Operation::new(\n"
            f"            {json.dumps(operation['name'])},\n"
            f"            {json.dumps(operation['summary'])},\n"
            f"            Scope::{SCOPES[operation['scope']]},\n"  # type: ignore[index]
            f"            {str(operation['mutating']).lower()},\n"
            f"            HttpRoute::new(HttpMethod::{METHODS[operation['method']]}, "  # type: ignore[index]
            f"{json.dumps(operation['path'])}),\n"
            f"            CliBinding::new(&[{cli}]),\n"
            "        ),\n"
        )
    lines.append("    ]\n}\n")
    # JSON writes non-ASCII as \uXXXX, which is not a Rust escape. Rust wants
    # \u{XXXX}, and a character is unambiguous where a decoded code point is not.
    return re.sub(r"\\u([0-9a-fA-F]{4})", r"\\u{\1}", "".join(lines))


def record_golden() -> str:
    """Python's own dump, with schemas marked not-yet-ported."""
    from vogt.application.models import RegistryDumpParams  # noqa: E402
    from vogt.application.services.instance import registry_dump  # noqa: E402

    dump = registry_dump(None, RegistryDumpParams()).model_dump()  # type: ignore[arg-type]
    for operation in dump["operations"]:
        operation["params_schema"] = {"not_ported": True}
        operation["result_schema"] = {"not_ported": True}
    return json.dumps(dump, indent=2, ensure_ascii=False) + "\n"


def main() -> int:
    operations = _operations()
    OPERATIONS_RS.write_text(render_operations_rs(operations))
    GOLDEN.write_text(record_golden())
    print(f"wrote {len(operations)} operations to {OPERATIONS_RS.relative_to(REPO_ROOT)}")
    print(f"recorded {GOLDEN.relative_to(REPO_ROOT)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
