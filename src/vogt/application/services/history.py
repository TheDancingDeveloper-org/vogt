"""The event feed and the audit log."""

from __future__ import annotations

from vogt.application.context import AppContext
from vogt.application.models import (
    AuditListResult,
    EventListResult,
    ListAuditParams,
    ListEventsParams,
)


def list_events(ctx: AppContext, params: ListEventsParams) -> EventListResult:
    """Read the cursor-based notification feed (FR-N1).

    `next_cursor` is the seq of the last row returned — pass it back as
    `after`. When the feed is empty it is the caller's own cursor, so a
    polling client never rewinds.
    """
    with ctx.declared.read() as view:
        events = view.list_events(after=params.after, limit=params.limit)
    next_cursor = events[-1].seq if events else params.after
    return EventListResult(events=events, next_cursor=next_cursor)


def list_audit(ctx: AppContext, params: ListAuditParams) -> AuditListResult:
    """Query the audit log by actor, operation or entity (FR-S6)."""
    with ctx.declared.read() as view:
        return AuditListResult(
            records=view.list_audit(
                limit=params.limit,
                actor_id=params.actor_id,
                operation=params.operation,
                entity_id=params.entity_id,
            )
        )
