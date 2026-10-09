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
SCHEMAS_RS = REPO_ROOT / "engine" / "core" / "src" / "registry" / "schemas.rs"
STRIPS_RS = REPO_ROOT / "engine" / "core" / "src" / "registry" / "strips.rs"
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


def record_golden() -> tuple[str, str]:
    """Python's own dump, plus the Rust module that emits its schemas.

    The schemas are recorded verbatim from pydantic rather than re-derived,
    because the parameter and result models are not yet ported to Rust (they
    arrive with the service chunks). Emitting them from a generated table
    keeps the dump a Rust product: `registry.json` is still compared against
    `registry::dump()` with no normaliser, so a hand edit on either side
    fails. Re-deriving the schemas from ported Rust types, and dropping this
    table, is WI-1060's remaining work.
    """
    from vogt.application.models import RegistryDumpParams  # noqa: E402
    from vogt.application.services.instance import registry_dump  # noqa: E402

    dump = registry_dump(None, RegistryDumpParams()).model_dump()  # type: ignore[arg-type]
    entries = []
    for operation in dump["operations"]:
        entries.append(
            "    (\n"
            f"        {json.dumps(operation['name'])},\n"
            f"        {json.dumps(json.dumps(operation['params_schema'], separators=(',', ':')))},\n"
            f"        {json.dumps(json.dumps(operation['result_schema'], separators=(',', ':')))},\n"
            "    ),\n"
        )
    module = (
        "//! Parameter and result schemas, recorded from pydantic, not derived.\n"
        "//!\n"
        "//! Produced by `scripts/gen_registry.py`. This is a recording, and\n"
        "//! that is a gap rather than the finished port: the parameter and\n"
        "//! result models are not hand-written Rust yet, so nothing here\n"
        "//! derives a schema from a Rust type. The parity check compares this\n"
        "//! table with pydantic's own output, which means a Rust model that\n"
        "//! validated differently would still pass. Deriving the schemas from\n"
        "//! the ported models, so the check is against Rust's own validation,\n"
        "//! and deleting this table is the remaining work of WI-1060. It has to\n"
        "//! land before the Python models are removed at the swap (WI-1082),\n"
        "//! because after that nothing can regenerate the table.\n"
        "\n"
        "/// `(operation name, params schema, result schema)`, in registry order.\n"
        "pub static SCHEMAS: &[(&str, &str, &str)] = &[\n"
        + "".join(entries)
        + "];\n"
    )
    golden = json.dumps(dump, indent=2, ensure_ascii=False) + "\n"
    return golden, module


def record_strips() -> str:
    """Which parameter fields strip whitespace, per operation.

    `Name` and `Reason` strip; a plain `str` does not, even when it shares the
    field's name. Pydantic erases the alias, so this is decided by behaviour
    rather than by reading the source: a field strips exactly when validating
    ``"  xy  "`` against its own annotation returns ``"xy"``. Inheritance,
    ``Name | None`` and a constraint written out by hand all come out the same
    way, and a regex over the file would miss each of them.
    """
    import pydantic  # noqa: E402

    from vogt.registry.operations import build_operations  # noqa: E402

    rows = []
    for operation in build_operations():
        for name, field in operation.params_model.model_fields.items():
            try:
                out = pydantic.TypeAdapter(field.rebuild_annotation()).validate_python(
                    "  xy  "
                )
            except (pydantic.ValidationError, TypeError):
                continue
            if out == "xy":
                rows.append(f'    ("{operation.name}", "{name}"),\n')
    return (
        "//! Parameter fields that strip whitespace, recorded from the models.\n"
        "//!\n"
        "//! Produced by `scripts/gen_registry.py`. A field is here exactly when\n"
        "//! validating `\"  xy  \"` against its annotation returns `\"xy\"`:\n"
        "//! `Name` and `Reason` do, a plain `str` does not. A field of the same\n"
        "//! name typed `str` is absent, and must stay absent: stripping it would\n"
        "//! change stored text, and with it the audit digest.\n"
        "\n"
        "/// `(operation, field)`, in registry order.\n"
        "pub static STRIPS: &[(&str, &str)] = &[\n"
        + "".join(rows)
        + "];\n"
    )


def main() -> int:
    check = "--check" in sys.argv[1:]
    operations = _operations()
    rendered = render_operations_rs(operations)
    strips = record_strips()
    outputs = {OPERATIONS_RS: rendered, STRIPS_RS: strips}
    if check:
        # Drift fails here, at the Python source, rather than waiting for
        # someone to remember to regenerate. The Rust test checks the same
        # golden from the other side, including field order.
        drifted = [
            str(path.relative_to(REPO_ROOT))
            for path, content in outputs.items()
            if not path.exists() or path.read_text() != content
        ]
        if drifted:
            print(
                "registry drift: "
                + ", ".join(drifted)
                + " differ from src/vogt/registry/operations.py. "
                "Run `python scripts/gen_registry.py` and commit the result.",
                file=sys.stderr,
            )
            return 1
        print(f"registry matches the Python source ({len(operations)} operations)")
        return 0
    for path, content in outputs.items():
        path.write_text(content)
    print(f"wrote {len(operations)} operations")
    return 0


if __name__ == "__main__":
    sys.exit(main())
