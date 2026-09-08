"""An existing data volume survives an image upgrade through the migrator.

The release pipeline treats `vogt-stack` and `vogt-voice` as one versioned
pair (`.github/workflows/release.yml`, `release-pair`). What that pairing
protects is the store on disk: when a deployer swaps the image, the new build
runs `vogt init`, whose migrator must carry the *existing* `vogt-data` volume
forward without losing a row and without disturbing an already-applied
migration.

This exercises that path with no Docker. It builds a database at the *oldest*
shipped migration set — the state a volume created by the first release is in —
writes a row into it, then runs the current build's full migrator over that
same file, the way an upgraded image's boot does. It asserts the upgrade is
forward-only (pending migrations apply in order, nothing already applied is
touched), that the checksums the migrator records match the shipped bytes
(so the *next* boot of that volume verifies clean rather than crash-looping),
that a second run is a no-op, and that the pre-existing data is still there.

Both stores' migration directories live under `src/`, so this runs in every
CI job, including the core-alone job that checks out without the engine.
"""

from __future__ import annotations

from datetime import UTC, datetime
from pathlib import Path

import pytest

from vogt.storage.sqlite.connection import connect
from vogt.storage.sqlite.declared import MIGRATIONS_DIR as DECLARED_MIGRATIONS
from vogt.storage.sqlite.migrator import Migration, Migrator, checksum_of, load_migrations
from vogt.storage.sqlite.observed import MIGRATIONS_DIR as OBSERVED_MIGRATIONS

NOW = datetime(2026, 9, 8, 12, 0, 0, tzinfo=UTC)


def _copy_prefix(shipped: list[Migration], upto: int, directory: Path) -> None:
    """Write the shipped migrations with number <= `upto` into `directory`.

    A copy of the real files, not a hand-written old schema, so the state this
    starts from is a state a released build actually produced.
    """
    directory.mkdir(parents=True, exist_ok=True)
    for migration in shipped:
        if migration.number <= upto:
            (directory / f"{migration.id}.sql").write_text(
                migration.sql, encoding="utf-8"
            )


@pytest.mark.parametrize(
    ("store", "shipped_dir"),
    [
        ("declared", DECLARED_MIGRATIONS),
        ("observed", OBSERVED_MIGRATIONS),
    ],
)
def test_existing_volume_upgrades_forward_with_data_intact(
    store: str, shipped_dir: Path, tmp_path: Path
) -> None:
    shipped = load_migrations(shipped_dir)
    assert len(shipped) >= 2, "need a real upgrade gap to exercise"

    # An "old image": only the first migration ships, the state a volume from
    # the very first release is in. Both stores create `meta(key, value)` in
    # `0001_foundation`, so a marker row can be written store-agnostically.
    old_dir = tmp_path / "old-build"
    _copy_prefix(shipped, upto=1, directory=old_dir)

    db = tmp_path / f"{store}.sqlite3"
    conn = connect(db, create=True)
    old_report = Migrator(store=store, directory=old_dir, holder="old/1").migrate(
        conn, now=NOW
    )
    assert old_report.version == 1

    # The row an existing volume holds before the upgrade.
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?, ?)",
        ("upgrade_marker", "written-before-upgrade"),
    )
    conn.commit()
    conn.close()

    # The upgraded image boots against the same file with the full shipped set.
    conn = connect(db, create=False)
    new = Migrator(store=store, directory=shipped_dir, holder="new/2")
    report = new.migrate(conn, now=NOW)

    # Every migration past the first applied, in order, and nothing before it.
    expected_pending = tuple(m.id for m in shipped if m.number > 1)
    assert report.applied == expected_pending
    assert report.version == shipped[-1].number == new.bundled_version()

    # Checksums recorded on the volume match the shipped bytes. This is exactly
    # what the migrator re-verifies on every later boot; if it did not hold, the
    # next start of this upgraded volume would fail forward-only verification.
    recorded = {
        str(row["id"]): str(row["checksum"])
        for row in conn.execute("SELECT id, checksum FROM migrations")
    }
    shipped_checksums = {m.id: checksum_of(m.sql) for m in shipped}
    assert recorded == shipped_checksums

    # The pre-existing row survived the upgrade untouched.
    marker = conn.execute(
        "SELECT value FROM meta WHERE key = ?", ("upgrade_marker",)
    ).fetchone()
    assert marker is not None
    assert marker["value"] == "written-before-upgrade"

    # A second boot of the now-current volume is a clean no-op: the upgraded
    # store verifies forward-only and applies nothing.
    again = new.migrate(conn, now=NOW)
    assert again.applied == ()
    assert again.version == shipped[-1].number
    conn.close()
