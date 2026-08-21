"""The forge inbox (FR-N3).

Deliberately not part of `/events`. That feed is this instance's own
audit-backed history with a cursor it owns (FR-N1); a forge's inbox has a
different origin, a different ordering, and a different notion of who it
belongs to. Merging them would make the cursor meaningless and the
provenance unreadable, so they stay two surfaces.

What this reads is observations the `gh-notifications` collector appended
(FR-O8), which is why the scope question answers itself: collection scope is
the registered project list, so nothing here can describe a repository
nobody onboarded.
"""

from __future__ import annotations

from datetime import datetime

from vogt.adapters.forge import KIND_NOTIFICATION
from vogt.application.context import AppContext
from vogt.application.models import (
    NotificationsParams,
    NotificationsResult,
    NotificationView,
)
from vogt.application.services import _resolve
from vogt.application.services.views import freshness_of
from vogt.core.entities import Observation

#: How many collected notifications to consider before filtering. Bounded
#: for the same reason every other view is: an inbox is read a page at a
#: time, and a query that walks an estate's whole history to render fifty
#: rows is a performance bug waiting for the first busy repository.
SCAN_LIMIT = 2000


def list_notifications(
    ctx: AppContext, params: NotificationsParams
) -> NotificationsResult:
    """What GitHub is trying to say about the registered projects."""
    if not ctx.observed.has_evidence_tables():
        return NotificationsResult(
            notifications=[],
            total=0,
            detail="no sweep has run; notifications are not collected",
        )

    project_id = None
    slugs: dict[str, str] = {}
    with ctx.declared.read() as view:
        if params.project:
            project = _resolve.project(view, params.project)
            project_id = project.id
        for known in view.list_projects(limit=10_000, offset=0):
            slugs[known.id] = known.slug

    observed = ctx.observed.latest(
        kinds=(KIND_NOTIFICATION,), project_id=project_id, limit=SCAN_LIMIT
    )
    views = [_view_of(entry, slugs) for entry in observed]
    if params.reason:
        views = [entry for entry in views if entry.reason == params.reason]
    if params.unread_only:
        views = [entry for entry in views if entry.unread]

    # Newest first: an inbox ordered any other way is a list, not an inbox.
    views.sort(key=lambda entry: entry.updated_at or entry.observed_at, reverse=True)

    by_reason: dict[str, int] = {}
    for entry in views:
        key = entry.reason or "unknown"
        by_reason[key] = by_reason.get(key, 0) + 1

    window = views[params.offset : params.offset + params.limit]
    return NotificationsResult(
        notifications=window,
        total=len(views),
        by_reason=dict(sorted(by_reason.items())),
        unread=sum(1 for entry in views if entry.unread),
        freshness=freshness_of(ctx),
        detail=(
            None
            if views
            else (
                "nothing collected — either there is nothing to say, or the "
                "configured token cannot read notifications; `coverage` "
                "distinguishes the two"
            )
        ),
    )


def _view_of(entry: Observation, slugs: dict[str, str]) -> NotificationView:
    payload = entry.payload
    return NotificationView(
        thread=str(payload.get("thread") or entry.subject_key),
        project_slug=slugs.get(entry.project_id or ""),
        repo=_text(payload.get("repo")),
        title=str(payload.get("title") or ""),
        reason=_text(payload.get("reason")),
        subject_type=_text(payload.get("subject_type")),
        unread=bool(payload.get("unread", False)),
        url=entry.source_url,
        updated_at=_when(payload.get("updated_at")),
        observed_at=entry.observed_at,
    )


def _text(value: object) -> str | None:
    return str(value) if isinstance(value, str) and value else None


def _when(value: object) -> datetime | None:
    """Parse GitHub's timestamp, or report that there is not one.

    The payload is whatever the forge sent. A field that is missing or
    malformed is one notification without a time, not a reason to fail the
    whole inbox.
    """
    if not isinstance(value, str) or not value:
        return None
    try:
        return datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None


__all__ = ["list_notifications"]
