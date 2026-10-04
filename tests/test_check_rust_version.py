"""Tests for `scripts/check_rust_version.py`, against hand-built metadata."""

from __future__ import annotations

from typing import Any

from check_rust_version import parse_version, problems


def metadata(member_floor: str | None, *deps: tuple[str, str | None]) -> dict[str, Any]:
    packages: list[dict[str, Any]] = [
        {
            "id": "member",
            "name": "member",
            "version": "0.1.0",
            "rust_version": member_floor,
        }
    ]
    packages += [
        {"id": name, "name": name, "version": "1.0.0", "rust_version": floor}
        for name, floor in deps
    ]
    nodes = [{"id": package["id"]} for package in packages]
    # A crate that is listed but not resolved (another platform's optional
    # dependency, say) never sets the floor.
    packages.append(
        {
            "id": "unresolved",
            "name": "unresolved",
            "version": "9.0.0",
            "rust_version": "1.99",
        }
    )
    return {
        "workspace_members": ["member"],
        "packages": packages,
        "resolve": {"nodes": nodes},
    }


def test_parse_version_treats_a_missing_patch_as_zero() -> None:
    assert parse_version("1.88") == parse_version("1.88.0") == (1, 88, 0)


def test_a_floor_that_covers_every_locked_crate_passes() -> None:
    assert (
        problems(metadata("1.88", ("time", "1.88.0"), ("icu", "1.86"), ("old", None)))
        == []
    )


def test_a_locked_crate_above_the_declared_floor_is_named() -> None:
    found = problems(
        metadata("1.80", ("time", "1.88.0"), ("icu", "1.88"), ("old", "1.70"))
    )
    assert found == [
        "member declares rust-version 1.80, but the lockfile needs 1.88"
        " (icu 1.0.0, time 1.0.0)"
    ]


def test_a_member_without_a_declared_floor_is_reported() -> None:
    assert problems(metadata(None, ("time", "1.88"))) == [
        "member declares no rust-version; the lockfile needs 1.88"
    ]
