"""Identifier generation.

Ids are ULID-shaped: a 48-bit millisecond timestamp followed by 80 bits of
randomness, Crockford base32 encoded, carrying a short entity prefix. They
sort lexicographically by creation time, which keeps `ORDER BY id` useful on
any backend without depending on SQLite rowids (NFR-S3).
"""

from __future__ import annotations

import secrets
from collections.abc import Callable
from typing import Final

_ALPHABET: Final = "0123456789ABCDEFGHJKMNPQRSTVWXYZ"
_TIME_CHARS: Final = 10
_RANDOM_CHARS: Final = 16

#: An id factory takes an entity prefix and returns a fresh identifier.
IdFactory = Callable[[str], str]


def _encode(value: int, length: int) -> str:
    chars = []
    for _ in range(length):
        chars.append(_ALPHABET[value & 0x1F])
        value >>= 5
    return "".join(reversed(chars))


def new_ulid(timestamp_ms: int, randomness: int) -> str:
    """Build a ULID from an explicit timestamp and randomness (testable)."""
    return _encode(timestamp_ms, _TIME_CHARS) + _encode(randomness, _RANDOM_CHARS)


def new_id(prefix: str) -> str:
    """Return a fresh prefixed identifier, e.g. ``prj_01J8...``."""
    from vogt.core.clock import utc_now

    timestamp_ms = int(utc_now().timestamp() * 1000)
    return f"{prefix}_{new_ulid(timestamp_ms, secrets.randbits(80))}"


def slugify(name: str) -> str:
    """Derive a project slug from a display name.

    Deliberately lossy and deliberately simple: lowercase, non-alphanumeric
    runs collapse to a single hyphen. A collision is a `Conflict` the caller
    resolves by choosing another name, not something the system silently
    disambiguates with a counter.
    """
    out: list[str] = []
    previous_was_sep = False
    for char in name.strip().lower():
        if char.isalnum():
            out.append(char)
            previous_was_sep = False
        elif not previous_was_sep:
            out.append("-")
            previous_was_sep = True
    return "".join(out).strip("-")
