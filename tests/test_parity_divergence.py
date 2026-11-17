"""The CI parity check's Rust-only divergence allowances (WI-4317) stay narrow."""

from __future__ import annotations

import importlib.util
from pathlib import Path
from typing import Any

_spec = importlib.util.spec_from_file_location(
    "parity_script", Path(__file__).resolve().parents[1] / "scripts" / "parity.py"
)
assert _spec is not None and _spec.loader is not None
parity = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(parity)

FULL: dict[str, Any] = {
    "projects": [
        {
            "slug": "parity",
            "name": "Parity",
            "lifecycle_state": "incubating",
            "link_state": "unlinked",
            "root_path": "<root>/proj",
        }
    ],
    "total": 1,
}
SUMMARY: dict[str, Any] = {
    "projects": [
        {
            "slug": "parity",
            "name": "Parity",
            "lifecycle_state": "incubating",
            "link_state": "unlinked",
        }
    ],
    "total": 1,
}


def _step(body: dict[str, Any]) -> list[dict[str, Any]]:
    return [{"operation": "project.list", "body": body}]


def _adopted(golden: dict[str, Any], ours: dict[str, Any], transport: str) -> Any:
    return parity._adopt_rust_divergences(_step(golden), _step(ours), transport)[0][
        "body"
    ]


def test_the_summary_projection_is_adopted_over_mcp() -> None:
    assert _adopted(FULL, SUMMARY, "mcp") == SUMMARY


def test_a_drifted_slug_still_fails() -> None:
    drifted = {"projects": [{**SUMMARY["projects"][0], "slug": "other"}], "total": 1}

    assert _adopted(FULL, drifted, "mcp") == FULL != drifted


def test_a_drifted_total_still_fails() -> None:
    assert _adopted(FULL, {**SUMMARY, "total": 2}, "mcp") == FULL


def test_an_extra_field_still_fails() -> None:
    extra = {"projects": [{**SUMMARY["projects"][0], "id": "x"}], "total": 1}

    assert _adopted(FULL, extra, "mcp") == FULL


def test_other_transports_adopt_nothing() -> None:
    assert _adopted(FULL, SUMMARY, "http") == FULL


def test_only_the_recorded_help_blocks_are_allowed() -> None:
    before, after = parity._RUST_HELP_DIVERGENCES[1]
    golden = "x\n" + before + "y\n"

    assert parity._adopt_rust_help(golden, "x\n" + after + "y\n") == (
        "x\n" + after + "y\n"
    )
    assert parity._adopt_rust_help(golden, "x\n" + before + "y\n") == golden
