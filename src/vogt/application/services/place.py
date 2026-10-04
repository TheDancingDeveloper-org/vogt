"""Small aggregate reads owned by a product surface."""

from __future__ import annotations

from collections.abc import Callable

from vogt.application.context import AppContext
from vogt.application.models import (
    BacklogParams,
    DriftListParams,
    InboxSavedFilter,
    ListProjectsParams,
    ListWorkParams,
    PlaceMetricsParams,
    PlaceMetricsResult,
)
from vogt.application.services.drift_service import list_drift
from vogt.application.services.inbox import InboxBadge, inbox_badge
from vogt.application.services.preferences import saved_inbox_filter
from vogt.application.services.projects import list_projects
from vogt.application.services.views import backlog
from vogt.application.services.work import list_work


def _optional(read: Callable[[], int | bool | None]) -> int | bool | None:
    try:
        return read()
    except Exception:
        # A badge is supporting information. One provider being unavailable
        # must not hide the other four answers or make the route fail.
        return None


def _optional_badge(read: Callable[[], InboxBadge]) -> InboxBadge | None:
    try:
        return read()
    except Exception:
        return None


def place_metrics(ctx: AppContext, params: PlaceMetricsParams) -> PlaceMetricsResult:
    """Return all shell badge values in one bounded response."""
    del params
    # The badge honours the caller's saved Inbox filter (WI-840): the count a
    # person sees beside "Inbox" is the length of the list they will land on.
    # Search text is not part of a saved filter, so it never moves the badge.
    saved: InboxSavedFilter | None = None
    try:
        with ctx.declared.read() as view:
            saved = saved_inbox_filter(ctx, view)
    except Exception:
        saved = None
    badge = _optional_badge(lambda: inbox_badge(ctx, saved))
    projects_total = _optional(
        lambda: list_projects(ctx, ListProjectsParams(limit=1)).total
    )
    work_total = _optional(lambda: list_work(ctx, ListWorkParams(limit=1)).total)
    backlog_total = _optional(
        lambda: backlog(ctx, BacklogParams(limit=1)).total_considered
    )
    drift_present = _optional(
        lambda: len(list_drift(ctx, DriftListParams(limit=1)).proposals) > 0
    )
    with ctx.declared.read() as view:
        revision = view.current_revision()

    return PlaceMetricsResult(
        inbox_active=None if badge is None else badge.filtered,
        inbox_active_unfiltered=None if badge is None else badge.active_total,
        inbox_filter=saved,
        projects_total=(projects_total if isinstance(projects_total, int) else None),
        work_total=work_total if isinstance(work_total, int) else None,
        backlog_total_considered=(
            backlog_total if isinstance(backlog_total, int) else None
        ),
        drift_present=(drift_present if isinstance(drift_present, bool) else None),
        revision=revision,
        generated_at=ctx.clock(),
    )


__all__ = ["place_metrics"]
