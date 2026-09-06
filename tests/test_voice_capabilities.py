"""Voice assistant policy is exhaustive and synchronized with the engine."""
from pathlib import Path
import re
from vogt.registry import default_registry

ENGINE = Path(__file__).parents[1] / "engine/server/src/vogt_tools.rs"
DOC = Path(__file__).parents[1] / "docs/ENGINE.md"

def _const(name: str) -> set[str]:
    text = ENGINE.read_text()
    match = re.search(rf"pub const {name}: &\[&str\] = &\[(.*?)\];", text, re.S)
    assert match, f"missing {name}"
    return set(re.findall(r'"([^"]+)"', match.group(1)))

def test_voice_matrix_covers_every_registered_operation_once():
    rows = re.findall(r'^\| `([^`]+)` \| (Voice-readable|Confirmation-gated|Operator-only) \|', DOC.read_text(), re.M)
    registry = set(default_registry().names)
    assert len(rows) == len(registry)
    assert {name for name, _ in rows} == registry
    assert len({name for name, _ in rows}) == len(rows)

def test_engine_sets_match_matrix_classes():
    rows = dict(re.findall(r'^\| `([^`]+)` \| (Voice-readable|Confirmation-gated|Operator-only) \|', DOC.read_text(), re.M))
    reads, writes = _const("CURATED_READS"), _const("CURATED_WRITES")
    assert reads == {n for n, c in rows.items() if c == "Voice-readable"}
    assert writes == {n for n, c in rows.items() if c == "Confirmation-gated"}
    assert not reads & writes
