"""Domain primitives: ids, time, digests, principals."""

from __future__ import annotations

from datetime import UTC, datetime, timedelta, timezone

import pytest

from vogt.core.clock import from_iso, to_iso, utc_now
from vogt.core.digest import canonical_json, digest_of
from vogt.core.ids import new_id, new_ulid, slugify
from vogt.core.principal import Principal, local_principal


def test_ids_sort_by_creation_time() -> None:
    earlier = new_ulid(1_700_000_000_000, 0)
    later = new_ulid(1_700_000_001_000, 0)
    assert earlier < later


def test_ids_carry_their_prefix() -> None:
    generated = new_id("prj")
    assert generated.startswith("prj_")
    assert len(generated) == len("prj_") + 26


def test_ids_are_unique() -> None:
    assert len({new_id("x") for _ in range(1000)}) == 1000


@pytest.mark.parametrize(
    ("name", "expected"),
    [
        ("Vogt", "vogt"),
        ("  Rust NZB  ", "rust-nzb"),
        ("nzb-core", "nzb-core"),
        ("A//B", "a-b"),
        ("///", ""),
        ("Region 2 (west)", "region-2-west"),
    ],
)
def test_slugify(name: str, expected: str) -> None:
    assert slugify(name) == expected


def test_timestamps_round_trip_as_utc() -> None:
    moment = datetime(2026, 8, 12, 5, 0, 0, tzinfo=timezone(timedelta(hours=10)))
    assert from_iso(to_iso(moment)) == moment.astimezone(UTC)


def test_a_naive_timestamp_is_read_as_utc() -> None:
    assert from_iso("2026-08-12T05:00:00").tzinfo == UTC


def test_utc_now_is_aware() -> None:
    assert utc_now().tzinfo is not None


def test_digests_ignore_key_order() -> None:
    assert digest_of({"a": 1, "b": 2}) == digest_of({"b": 2, "a": 1})


def test_digests_notice_content() -> None:
    assert digest_of({"a": 1}) != digest_of({"a": 2})


def test_canonical_json_is_compact_and_sorted() -> None:
    assert canonical_json({"b": 1, "a": 2}) == '{"a":2,"b":1}'


def test_the_local_principal_names_the_os_user() -> None:
    principal = local_principal()
    assert principal.identity_ref.startswith("local:")
    assert principal.kind == "human"


def test_a_principal_needs_an_identity() -> None:
    with pytest.raises(ValueError, match="identity_ref"):
        Principal(identity_ref="  ", kind="human", display_name="nobody")
