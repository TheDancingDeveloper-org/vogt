"""engine/core/migrations is canonical; the Python copy must stay byte-identical."""

from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
PY = ROOT / "src/vogt/storage/sqlite/migrations"
RUST = ROOT / "engine/core/migrations"


def test_python_migrations_match_engine_core() -> None:
    if not RUST.exists():
        pytest.skip("engine subtree is intentionally absent in core-only CI")
    for store in ("declared", "observed"):
        py = {p.name: p.read_bytes() for p in (PY / store).glob("*.sql")}
        rs = {p.name: p.read_bytes() for p in (RUST / store).glob("*.sql")}
        assert py == rs
