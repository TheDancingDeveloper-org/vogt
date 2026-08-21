"""Upstream-truth work items: the observed mirror joined to the overlay (#181).

On a **linked** project (decision 1 of #178) the work items *are* the
project's observed forge issues: the mirror the #173 sync collectors maintain
is the truth for title/labels/open-closed, and the `work_overlay` table
carries the vogt-local half — a workflow state richer than open/closed,
priority, effort, assignee, initiative (decision 2). This module is the one
place that joins the two into a `WorkItem`, so every surface — `work.get`,
`work.list`, the Backlog, the Board — reads the same item.

## The ref decision (design §6), made once, here

**The subject key is the ref.** An upstream-truth item's `ref` *and* `id`
are its forge subject key (`gh:{owner}/{repo}#{n}`), because the item has no
`wrk_*` row to own a `WI-n` handle and inventing a parallel numbering would
mean two names for one thing across three transports. `resolve_work_ref` is
the single resolver: a declared ref (`WI-7`) resolves first, and anything
else that names a mirrored forge issue or pull request on a linked project
resolves to the assembled upstream item. `_resolve.work_item` remains the
declared-only resolver for paths that must not see upstream items.
"""

from __future__ import annotations

from datetime import datetime

from vogt.application.context import AppContext
from vogt.application.services.views import trust_for
from vogt.core.clock import from_iso
from vogt.core.entities import Observation, Project, WorkItem, WorkOverlay
from vogt.core.observed import priority_of, upstream_state, work_kind_of
from vogt.core.workflow import TERMINAL_STATES, Workflow
from vogt.errors import NotFound
from vogt.storage.interface import ReadView, WorkFilter

#: The observed kinds an upstream-truth work item can be assembled from.
#: Issues are the work plane; pull requests stay observed-only candidates in
#: the Backlog, exactly as before, because nobody "creates" a PR through
#: `work.create` and the Board's columns are issue workflow states.
UPSTREAM_ITEM_KINDS: tuple[str, ...] = ("forge.issue",)


def is_linked(project: Project | None) -> bool:
    """Whether this project's work model is the forge's (#181).

    Reads the persisted `link_state` and nothing else: linking is an explicit
    act (`project.import`, `forge.link`, later `forge.publish`), never an
    inference from which tokens resolve this second.
    """
    return project is not None and project.link_state == "linked"


def build_item(
    ctx: AppContext,
    observation: Observation,
    *,
    overlay: WorkOverlay | None,
    project: Project,
    workflow: Workflow,
    confirmed_at: datetime | None = None,
) -> WorkItem:
    """One upstream-truth work item, assembled the only way there is.

    Identity is the subject key (id *and* ref — the design §6 decision this
    module's docstring records). Kind and priority reuse the observed-side
    classification (`work_kind_of`, `priority_of`), refined by the overlay
    where somebody said otherwise. The body is empty because the mirror does
    not carry issue bodies; `source_url` on the observation is where the
    prose lives, and vogt does not edit bodies on linked projects (FR-B4).
    """
    payload = observation.payload
    title = str(payload.get("title", "")).strip() or observation.subject_key
    labels = payload.get("labels")
    updated_raw = payload.get("updated_at")
    updated_at = observation.observed_at
    if isinstance(updated_raw, str) and updated_raw:
        try:
            updated_at = from_iso(updated_raw)
        except ValueError:
            # An unparseable upstream timestamp costs recency, not the item.
            updated_at = observation.observed_at
    return WorkItem(
        id=observation.subject_key,
        ref=observation.subject_key,
        kind=work_kind_of(observation),
        title=title[:300],
        body="",
        state=upstream_state(observation, overlay, workflow),
        priority=(
            overlay.priority
            if overlay and overlay.priority
            else priority_of(observation)
        ),
        effort=None if overlay is None else overlay.effort,
        project_id=project.id,
        project_slug=project.slug,
        initiative_id=None if overlay is None else overlay.initiative_id,
        origin="observed",
        trust_state=trust_for(  # type: ignore[arg-type]
            ctx,
            observed_at=observation.observed_at,
            confirmed_at=confirmed_at,
        ),
        assignee_actor_id=None if overlay is None else overlay.assignee_actor_id,
        labels=[str(label) for label in labels] if isinstance(labels, list) else [],
        created_at=observation.observed_at,
        updated_at=updated_at,
    )


def upstream_items(
    ctx: AppContext,
    view: ReadView,
    project: Project,
    *,
    include_closed: bool = False,
    limit: int = 1000,
) -> list[WorkItem]:
    """Every upstream-truth item of one linked project, unfiltered.

    The latest observation per subject, joined to the overlay. Subjects a
    `work_link` already adopted into a declared row are excluded — that row
    is the item, and emitting both would be the double-count #181 exists to
    end. Callers filter with `matches`; ordering follows the declared list's
    `(created_at, ref)` so merged pages page stably.
    """
    if not ctx.observed.has_evidence_tables():
        return []
    observations = ctx.observed.latest(
        kinds=UPSTREAM_ITEM_KINDS,
        project_id=project.id,
        exclude_closed=not include_closed,
        limit=limit,
    )
    if not observations:
        return []
    keys = [observation.subject_key for observation in observations]
    adopted = view.work_links_for_subjects(keys)
    overlays = view.work_overlays(keys)
    confirmed = ctx.observed.last_confirmed(keys)
    items = [
        build_item(
            ctx,
            observation,
            overlay=overlays.get(observation.subject_key),
            project=project,
            workflow=view.workflow_for(work_kind_of(observation)),
            confirmed_at=confirmed.get(observation.subject_key),
        )
        for observation in observations
        if observation.subject_key not in adopted
    ]
    items.sort(key=lambda item: (item.created_at, item.ref))
    return items


def matches(item: WorkItem, work_filter: WorkFilter) -> bool:
    """Apply a `WorkFilter` to an assembled item, the SQL `_work_where` way.

    The observed store cannot join the declared filters in SQL — the two
    stores are separate databases by design — so the same narrowing runs
    here, and it must keep meaning what the SQL means as filters are added.
    `limit`/`offset` are paging, not narrowing, and are applied by callers
    after any merge.
    """
    if work_filter.project_id is not None and item.project_id != work_filter.project_id:
        return False
    if work_filter.kinds and item.kind not in work_filter.kinds:
        return False
    if work_filter.states and item.state not in work_filter.states:
        return False
    if work_filter.priorities and item.priority not in work_filter.priorities:
        return False
    if (
        work_filter.assignee_actor_id is not None
        and item.assignee_actor_id != work_filter.assignee_actor_id
    ):
        return False
    if (
        work_filter.initiative_id is not None
        and item.initiative_id != work_filter.initiative_id
    ):
        return False
    if work_filter.label is not None and work_filter.label not in item.labels:
        return False
    if work_filter.trust_states and item.trust_state not in work_filter.trust_states:
        return False
    return not (work_filter.exclude_terminal and item.state in TERMINAL_STATES)


def linked_projects(view: ReadView, project: Project | None) -> list[Project]:
    """The linked projects a scope covers: one, or every linked one."""
    if project is not None:
        return [project] if is_linked(project) else []
    return [
        candidate
        for candidate in view.list_projects(limit=10_000, offset=0)
        if is_linked(candidate)
    ]


def resolve_upstream(ctx: AppContext, view: ReadView, ref: str) -> WorkItem | None:
    """The upstream-truth item a subject key names, or `None`.

    Only subjects of `UPSTREAM_ITEM_KINDS` on a *linked* project resolve —
    a marker key or an issue on an unlinked project is not a work item, and
    saying so with `None` lets the caller raise the same "no work item"
    every typo has always produced.
    """
    if ":" not in ref:
        return None
    if not ctx.observed.has_evidence_tables():
        return None
    observation = ctx.observed.latest_by_subject(ref)
    if observation is None or observation.kind not in UPSTREAM_ITEM_KINDS:
        return None
    if observation.project_id is None:
        return None
    project = view.project_by_id(observation.project_id)
    if not is_linked(project):
        return None
    assert project is not None  # is_linked returned True
    return build_item(
        ctx,
        observation,
        overlay=view.work_overlay(ref),
        project=project,
        workflow=view.workflow_for(work_kind_of(observation)),
        confirmed_at=ctx.observed.last_confirmed([ref]).get(ref),
    )


def resolve_work_ref(ctx: AppContext, view: ReadView, ref: str) -> WorkItem:
    """THE work-item resolver: declared ref first, then upstream subject key.

    Every operation that accepts a work-item ref accepts a subject key
    through this one function (design §6). Declared wins because `WI-n` and
    subject keys cannot collide (a `WI-` ref never carries `:`), and because
    an adopted declared row *is* the item for its subject.
    """
    found = view.work_item_by_ref(ref)
    if found is not None:
        return found
    upstream = resolve_upstream(ctx, view, ref)
    if upstream is not None:
        return upstream
    msg = f"no work item {ref!r}"
    raise NotFound(msg)


__all__ = [
    "UPSTREAM_ITEM_KINDS",
    "build_item",
    "is_linked",
    "linked_projects",
    "matches",
    "resolve_upstream",
    "resolve_work_ref",
    "upstream_items",
    "upstream_state",
]
