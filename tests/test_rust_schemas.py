"""The Rust registry's schemas, checked against Python's live registry dump.

`tests/parity/golden/registry.json` is a recording of Python's dump, and the
Rust golden test compares `registry::dump()` with it. That shows the Rust side
matches the recording, not that the recording still matches Python: a model
edit that moves a schema changes both sides' input and passes. This test asks
the Python registry now, in the same process, and asks the Rust binary now,
and compares the two.

The schemas the Rust side derives (`application::models::DERIVED`) are the
ones this is for: `schemars` built them from Rust types, so only a comparison
with Python shows that the Rust model validates what Python's does.
"""

from __future__ import annotations

import json
import shutil
import subprocess
from pathlib import Path
from typing import Any

import pytest

from vogt.application.models import RegistryDumpParams
from vogt.application.services.instance import registry_dump

REPO_ROOT = Path(__file__).resolve().parents[1]
ENGINE = REPO_ROOT / "engine"
MODELS_RS = ENGINE / "core" / "src" / "application" / "models.rs"


def _rust_dump() -> dict[str, Any]:
    """The Rust binary's `registry dump`, built and run through cargo."""
    completed = subprocess.run(
        [
            "cargo",
            "run",
            "--quiet",
            "--manifest-path",
            str(ENGINE / "Cargo.toml"),
            "-p",
            "vogt-core",
            "--",
            "--json",
            "registry",
            "dump",
        ],
        capture_output=True,
        text=True,
        check=False,
        timeout=600,
    )
    assert completed.returncode == 0, completed.stderr
    dump: dict[str, Any] = json.loads(completed.stdout)
    return dump


def _python_dump() -> dict[str, Any]:
    """Python's live registry dump. `registry_dump` reads no store."""
    return registry_dump(None, RegistryDumpParams()).model_dump()  # type: ignore[arg-type]


@pytest.fixture(scope="module")
def dumps() -> tuple[dict[str, Any], dict[str, Any]]:
    if shutil.which("cargo") is None:
        pytest.skip("cargo is not installed; the Rust registry cannot be dumped")
    if not (ENGINE / "Cargo.toml").is_file():
        # The "core alone" CI job deletes engine/. There is no Rust side to compare.
        pytest.skip("engine/ is absent; the Rust registry cannot be dumped")
    return _rust_dump(), _python_dump()


def _by_name(dump: dict[str, Any]) -> dict[str, dict[str, Any]]:
    operations = dump["operations"]
    assert isinstance(operations, list)
    return {operation["name"]: operation for operation in operations}


def test_rust_and_python_list_the_same_operations(
    dumps: tuple[dict[str, Any], dict[str, Any]],
) -> None:
    rust, python = dumps

    assert [op["name"] for op in rust["operations"]] == [
        op["name"] for op in python["operations"]
    ]


def test_every_schema_matches_python_live(
    dumps: tuple[dict[str, Any], dict[str, Any]],
) -> None:
    rust, python = dumps
    rust_ops, python_ops = _by_name(rust), _by_name(python)

    mismatches = []
    for name, expected in python_ops.items():
        actual = rust_ops[name]
        for field in ("params_schema", "result_schema"):
            if actual[field] != expected[field]:
                mismatches.append(f"{name}.{field}")

    assert mismatches == [], (
        "Rust schemas differ from Python's live registry dump: " + ", ".join(mismatches)
    )


def test_derived_operations_are_compared_against_python(
    dumps: tuple[dict[str, Any], dict[str, Any]],
) -> None:
    """The derived set is non-empty and each name is in the live comparison.

    Guards against `DERIVED` being emptied, which would turn the comparison
    above into a pass over the recorded table alone.
    """
    rust, python = dumps
    text = MODELS_RS.read_text(encoding="utf-8")
    block = text.split("pub const DERIVED: &[&str] = &[", 1)[1].split("];", 1)[0]
    derived = [part.strip().strip('",') for part in block.split(",") if part.strip()]

    assert derived, "application::models::DERIVED is empty"
    python_names = set(_by_name(python))
    assert set(derived) <= python_names
    assert set(derived) <= set(_by_name(rust))
