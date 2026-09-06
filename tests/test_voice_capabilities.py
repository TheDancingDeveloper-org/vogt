"""Voice assistant policy is exhaustive and synchronized with the engine."""

import re
from pathlib import Path

from vogt.registry import default_registry

ENGINE = Path(__file__).parents[1] / "engine/server/src/vogt_tools.rs"
DOC = Path(__file__).parents[1] / "docs/ENGINE.md"
ROW = re.compile(
    r"^\| `([^`]+)` \| (Voice-readable|Confirmation-gated|Operator-only) \|",
    re.M,
)


def _const(name: str) -> set[str]:
    text = ENGINE.read_text()
    match = re.search(rf"pub const {name}: &\[&str\] = &\[(.*?)\];", text, re.S)
    assert match, f"missing {name}"
    return set(re.findall(r'"([^"]+)"', match.group(1)))


def test_voice_matrix_covers_every_registered_operation_once() -> None:
    rows = ROW.findall(DOC.read_text())
    registry = set(default_registry().names)
    assert len(rows) == len(registry)
    assert {name for name, _ in rows} == registry
    assert len({name for name, _ in rows}) == len(rows)


def test_engine_sets_match_matrix_classes() -> None:
    rows = dict(ROW.findall(DOC.read_text()))
    reads, writes = _const("CURATED_READS"), _const("CURATED_WRITES")
    assert reads == {n for n, c in rows.items() if c == "Voice-readable"}
    assert writes == {n for n, c in rows.items() if c == "Confirmation-gated"}
    assert not reads & writes
