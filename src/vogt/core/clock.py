"""Time, injectable so tests do not have to sleep.

Every timestamp Vogt stores is UTC and second-or-finer ISO-8601. Freshness
is a first-class product concept (DESIGN §6), so "when was this true" must
never depend on the reader's local timezone.
"""

from __future__ import annotations

from collections.abc import Callable
from datetime import UTC, datetime

#: A clock is anything that answers "now", in UTC.
Clock = Callable[[], datetime]


def utc_now() -> datetime:
    """Return the current time as a timezone-aware UTC datetime."""
    return datetime.now(UTC)


def to_iso(moment: datetime) -> str:
    """Render a datetime as a UTC ISO-8601 string for storage."""
    return moment.astimezone(UTC).isoformat()


def from_iso(text: str) -> datetime:
    """Parse a stored ISO-8601 timestamp back into an aware UTC datetime."""
    parsed = datetime.fromisoformat(text)
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=UTC)
    return parsed.astimezone(UTC)
