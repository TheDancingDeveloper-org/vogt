"""Conditional-read support for a deliberately small GET allow-list.

The validator is the declared store revision rather than a response-body
digest. That revision changes in the same transaction as every authoritative
declared write, so it is cheap and cannot make a declared read appear current
after a write. Observed-only and composite reads stay uncached until they have
an equally precise validator.
"""

from __future__ import annotations

# Keep this list aligned with the browser's stable metadata policy. These are
# declared-only, read-only facets; the operation name is the registry's source
# of truth, not a URL substring that could accidentally include a write route.
CONDITIONAL_READ_OPERATIONS: frozenset[str] = frozenset(
    {
        "project.list",
        "workflow.list",
        "label.list",
        "initiative.list",
        "actor.list",
    }
)


def is_conditional_read(operation: str, *, method: str) -> bool:
    """Whether an operation is eligible for a revision ETag."""
    return method == "GET" and operation in CONDITIONAL_READ_OPERATIONS


def etag_for_revision(revision: int) -> str:
    """Return the weak validator for one declared-store revision."""
    return f'W/"{revision}"'


def if_none_match_matches(header: str | None, current: str) -> bool:
    """Apply the GET/HEAD weak comparison for an ``If-None-Match`` header."""
    if not header:
        return False
    current_tag = _opaque_tag(current)
    return any(
        candidate.strip() == "*" or _opaque_tag(candidate.strip()) == current_tag
        for candidate in header.split(",")
    )


def _opaque_tag(value: str) -> str:
    """Remove the weak marker for the RFC weak comparison."""
    return value[2:].strip() if value.startswith("W/") else value
