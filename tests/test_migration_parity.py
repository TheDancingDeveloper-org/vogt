"""engine/core/migrations is canonical; the Python copy must stay byte-identical."""

from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PY = ROOT / "src/vogt/storage/sqlite/migrations"
RUST = ROOT / "engine/core/migrations"


def test_python_migrations_match_engine_core() -> None:
    for store in ("declared", "observed"):
        py = {p.name: p.read_bytes() for p in (PY / store).glob("*.sql")}
        rs = {p.name: p.read_bytes() for p in (RUST / store).glob("*.sql")}
        assert py == rs
