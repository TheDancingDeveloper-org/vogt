"""Shipped migrations are byte-frozen once released.

The migrator records `sha256(sql.strip())` for every applied migration and
refuses to boot if an applied migration's file no longer matches (see
``migrator.py``). So editing a *shipped* migration — even a comment — silently
breaks every existing database on its next deploy.

This test freezes the checksum of every shipped migration against a recorded
golden value. Adding a new migration means adding its id + checksum here in the
same change. A failure means an already-shipped migration was edited: revert the
edit and add a new forward migration instead. Do not "fix" this test by pasting
the new checksum for an existing id.

Backstory: the 2026-09 history reset scrubbed citation comments inside applied
migration SQL, which changed their checksums and crash-looped the engine on
deploy. This guard exists so that cannot recur silently.
"""

from __future__ import annotations

from vogt.storage.sqlite import declared, observed
from vogt.storage.sqlite.migrator import checksum_of, load_migrations

# id (relative to migrations/) -> sha256(sql.strip()). Frozen at release.
FROZEN: dict[str, str] = {
    "declared/0001_foundation": "83cf77f4abe04e89c2efa22271c6d9275532f1dd37a270b7667226b3748ae643",
    "declared/0002_work": "f690b73e4b9523cd85b818ac11c6fecf086c020683324dccbc03ec3d7b59627d",
    "declared/0003_observed_first": "9032649fc02951fe5d6340a6f9576a45b8d7197057990c68dca834a1d26f48b8",
    "declared/0004_drift": "cc588f67774949d31caf0305200e557cc666617b8f5a2469f958f511944ee3f4",
    "declared/0005_tokens": "0e2adf0935fc90a6810710297ab3490edb430b47d9d169a2166ae189c24223b2",
    "declared/0006_writeback": "37b95c74c33d9d7c5ee649ab72b265b4de517b41a40cb3a2c768099f210b0f58",
    "declared/0007_sessions": "47c82ee427931ca7c0a3a8d047298ffe5c2b2ad49ff06f83b00acf59306a1ff6",
    "declared/0008_superseded_drift": "f533369dd3625830341933caac68f4611a02d0e4055b9d37a62db9c59f731ce3",
    "declared/0009_inbox_triage": "b0ca1559346b05c6ef818b3b65c2ae798f05c755c7a0dd326c73227da770c226",
    "declared/0010_session_model": "de3f70faf7ccb5fd276455a9a56eb7a731c8334fb6964ea6d5eb2407dbcb03f5",
    "declared/0011_contract_adoption": "7b4ab541ce8b97e31f1871373c6400e7a6390bdc70356fe703f3aa5e29211582",
    "declared/0012_forge_accounts": "c819361c246e6c76035ed27c1a296d723235259c449a2ee2ce192163b8cb8a3a",
    "declared/0013_upstream_truth": "fa8e380cd0761fe49bf3a46fd7e9305799fe8f142fdc331c71d2a0836fa00c3f",
    "declared/0014_native_migration": "2fef3fae64abd522ef65b21eef63f5a93d2b44ded81e5836da1288639a59a70d",
    "declared/0015_work_overlay_branches": "6f60bd5cae2d90f79cfb72228c88b21927958bdeedc7f00da59835c3e18a928c",
    "declared/0016_perf_indexes": "e9d6f101a8d95929b51b6dd9a4d1ccc0e3a732e49fd605c60de3ee849be7a113",
    "observed/0001_foundation": "c050f9e4dba983119f045b8fb4ccbe8b1ee97473170eead7d28279e99468de4b",
    "observed/0002_evidence": "9cdc9c9179740af96606bab0b557c7b35639eaabc78faf16630c7705f1d368f5",
    "observed/0003_inherited_dep_refs": "8bf0b8533774a156762fddc2e56bbb9656ba3b6bf0e38ec85a66c9dc9c325ed1",
    "observed/0004_forge_sync": "8d5cffbb2acb6ff18099ad73384eaa4ea5ebca7a6f45afb79d353fb4cbbcaee6",
    "observed/0005_perf_indexes": "e6902e6616ab9948f9d222b2763ce9bc421a136b6acb1422240e5957683e5e45",
}


def _live_checksums() -> dict[str, str]:
    live: dict[str, str] = {}
    for name, directory in (
        ("declared", declared.MIGRATIONS_DIR),
        ("observed", observed.MIGRATIONS_DIR),
    ):
        for migration in load_migrations(directory):
            live[f"{name}/{migration.id}"] = checksum_of(migration.sql)
    return live


def test_shipped_migrations_are_frozen() -> None:
    live = _live_checksums()

    added = sorted(set(live) - set(FROZEN))
    removed = sorted(set(FROZEN) - set(live))
    changed = sorted(mid for mid in set(live) & set(FROZEN) if live[mid] != FROZEN[mid])

    assert not changed, (
        "A shipped migration was edited after release, which breaks every "
        "existing database on deploy (the migrator verifies checksums and is "
        "forward-only). Revert the edit and add a new forward migration "
        f"instead. Edited: {changed}"
    )
    assert not removed, f"A shipped migration disappeared from the tree: {removed}"
    assert not added, (
        "A new migration was added without freezing its checksum. Add it to "
        f"FROZEN in this test in the same change: {added}"
    )
