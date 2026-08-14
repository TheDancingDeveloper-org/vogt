"""The event feed and the audit log."""

from __future__ import annotations

from vogt.application.context import AppContext
from vogt.application.models import (
    AuditListResult,
    EventListResult,
    ListAuditParams,
    ListEventsParams,
)
from vogt.application.services import _resolve


def list_events(ctx: AppContext, params: ListEventsParams) -> EventListResult:
    """Read the cursor-based notification feed (FR-N1).

    `next_cursor` is the seq of the last row returned — pass it back as
    `after`. When the feed is empty it is the caller's own cursor, so a
    polling client never rewinds.

    **`entity_id` narrows it to one thing's history, and that is what makes a
    work item's state history answerable at all.** The audit log records that
    a transition happened, who made it and why, but keeps a `payload_digest`
    rather than the payload — deliberately, since the audit proves what
    changed without duplicating it — so the audit alone cannot say *which*
    state an item came from. The event can: `work.transitioned` carries
    `{ref, from, to}` in its summary, and this feed is never pruned, so an
    entity's slice of it is complete rather than recent.

    The two feeds answer different halves of the same question and are meant
    to be read together: the event says what the state was, the audit row it
    names in `audit_id` says why somebody changed it.
    """
    with ctx.declared.read() as view:
        events = view.list_events(
            after=params.after, limit=params.limit, entity_id=params.entity_id
        )
    next_cursor = events[-1].seq if events else params.after
    return EventListResult(events=events, next_cursor=next_cursor)


def list_audit(ctx: AppContext, params: ListAuditParams) -> AuditListResult:
    """Query the audit log by actor, operation, entity, project or time.

    FR-S6 and FR-U19. Four things are worth knowing about what this answers:

    **A work item's trail includes its comments.** A comment is audited
    against the comment, so an exact match on the item's id would return its
    creation, its updates and its transitions and quietly drop every comment
    anybody wrote on it. An audit trail that omits a kind of write is worse
    than no trail, because it is believed. The link lives in `comments` and
    the query follows it (see `_audit_where`).

    **`since` is inclusive, `until` is exclusive.** Consecutive windows
    therefore tile the log exactly: no write is in two of them and none falls
    between them.

    **`project` is answered in the query, not in the reader.** It keeps the
    writes this instance can attribute to that project through a foreign key
    — the project row itself, its work items, the comments on them, the
    coding sessions opened in it, the drift proposals raised against it and
    the suppressions scoped to it. Writes that belong to the instance rather
    than to any project — actors, labels, initiatives, tokens, `init` — are
    not part of any project's trail and are excluded, not lost.

    **`total` counts matches, not the page.** It is what tells a reader
    whether they are looking at the whole story, and with `offset` it is what
    lets them reach past the newest page — the log is read newest-first, so
    without both, everything older than one window was unreachable.

    This is a read of the declared store and nothing else: no reason, no
    audit row, no write. Querying the record of what happened must not itself
    become part of it.
    """
    with ctx.declared.read() as view:
        project_id = (
            None
            if params.project is None
            else _resolve.project(view, params.project).id
        )
        return AuditListResult(
            records=view.list_audit(
                limit=params.limit,
                offset=params.offset,
                actor_id=params.actor_id,
                operation=params.operation,
                entity_id=params.entity_id,
                project_id=project_id,
                since=params.since,
                until=params.until,
            ),
            total=view.count_audit(
                actor_id=params.actor_id,
                operation=params.operation,
                entity_id=params.entity_id,
                project_id=project_id,
                since=params.since,
                until=params.until,
            ),
        )
