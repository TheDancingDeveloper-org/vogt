"""The write plane: work items, transitions, relations, comments.

Since #181 the write plane has two shapes, split by the project's persisted
link state (decision 1 of #178):

- **Linked project** — the work items are the upstream issues. `work.create`
  writes through to the forge and the new issue's subject key is the item's
  identity; comments, label adds and open/close write through the same way,
  synchronously and fail-loud (decision 9): a provider failure raises a
  typed error and nothing local commits, so a caller can never hold a local
  success the forge never heard of. Overlay-only changes — priority, effort,
  assignee, initiative, vogt-only workflow states — touch the `work_overlay`
  row and produce **zero** provider calls (decision 2).
- **No project** — native declared items, exactly as before.
- **Unlinked project** — the write verbs refuse with `NotLinked` (decision
  10): link the project or publish it (#182). Native items that already
  exist there stay readable; the surface withdrawal is #183's.
"""

from __future__ import annotations

from dataclasses import replace
from datetime import datetime
from typing import Literal

from vogt.adapters.forge import ForgeProvider
from vogt.adapters.forge.models import RepoRef
from vogt.adapters.forge.writeback import WriteBackResult, permits
from vogt.application.context import AppContext
from vogt.application.models import (
    CommentParams,
    CommentResult,
    CreateWorkParams,
    GetWorkParams,
    ListSessionsParams,
    ListWorkParams,
    RelateWorkParams,
    TransitionWorkParams,
    UnrelateWorkParams,
    UpdateWorkParams,
    WorkListResult,
    WorkResult,
)
from vogt.application.services import _resolve, upstream, writeback
from vogt.application.services.branches import branch_views_for
from vogt.application.services.sessions import list_sessions
from vogt.application.writes import WriteOutcome, audited_write
from vogt.core.entities import (
    Actor,
    Comment,
    Project,
    WorkItem,
    WorkOverlay,
    WriteBackRecord,
)
from vogt.core.workflow import (
    TERMINAL_STATES,
    check_completion_allowed,
)
from vogt.errors import (
    Conflict,
    InvalidRequest,
    NotFound,
    NotLinked,
    UpstreamWriteFailed,
    UpstreamWriteRefused,
)
from vogt.storage.interface import ReadView, WorkFilter, WorkItemUpdate, WriteTxn

WORK_CREATE = "work.create"
WORK_UPDATE = "work.update"
WORK_TRANSITION = "work.transition"
WORK_RELATE = "work.relate"
WORK_UNRELATE = "work.unrelate"
WORK_COMMENT = "work.comment"

WORK_CREATED_EVENT = "work.created"
WORK_UPDATED_EVENT = "work.updated"
WORK_TRANSITIONED_EVENT = "work.transitioned"
WORK_RELATED_EVENT = "work.related"
WORK_UNRELATED_EVENT = "work.unrelated"
WORK_COMMENTED_EVENT = "work.commented"


# -- the linked/unlinked gate ----------------------------------------------


def _refuse_unlinked(project: Project, verb: str) -> NotLinked:
    """The decision-10 refusal, with both ways forward named."""
    return NotLinked(
        f"{verb} needs a forge-linked project, and {project.slug!r} is not "
        "linked: link it (`forge link`, or re-import through `project "
        "import`) or publish it (`forge publish`) first"
    )


def _require_permitted(project: Project, action: str) -> None:
    """Fail loud on policy before anything is sent (decision 9).

    Write-through makes the upstream half *part of* the operation, so a
    policy that does not permit the action refuses the whole thing rather
    than committing a local half silently.
    """
    if not permits(project.write_back, action):
        raise UpstreamWriteRefused(
            f"{project.slug!r} has write-back policy {project.write_back!r}, "
            f"which does not permit {action!r}; on a linked project the "
            "write goes upstream or not at all — set the policy with "
            "`forge writeback` first"
        )


def _upstream_writer(
    ctx: AppContext, project: Project
) -> tuple[ForgeProvider, str, RepoRef]:
    """The provider, identity and repo ref a write-through will use.

    Resolved *before* the declared transaction opens, so a network round
    trip never runs while a write lock is held. Reuses #179's identity
    selection: the acting actor's linked PAT when there is one, else the
    FR-S7 file token.
    """
    with ctx.declared.read() as view:
        actor = view.actor_by_identity(ctx.principal.identity_ref)
    provider, identity = writeback._writer_provider(ctx, actor, project.repo_url)
    if provider is None:
        raise UpstreamWriteFailed(
            f"{project.slug!r} is linked but no usable forge credential "
            "resolves for it now — link a forge account or restore the "
            "instance token file, then retry"
        )
    repo = provider.parse(project.repo_url)
    if repo is None:
        raise UpstreamWriteFailed(
            f"{project.slug!r} is linked but its repo_url "
            f"{project.repo_url!r} no longer parses for the provider"
        )
    return provider, identity, repo


def _sent_or_fail(result: WriteBackResult, action: str) -> WriteBackResult:
    """Decision 9: anything but an upstream success aborts the operation."""
    if result.outcome != "succeeded":
        raise UpstreamWriteFailed(
            f"the forge did not accept the {action}: "
            f"{result.detail or 'no detail from the provider'} — nothing "
            "was changed locally"
        )
    return result


def _upstream_record(
    ctx: AppContext,
    *,
    actor: Actor,
    action: str,
    project: Project,
    subject_key: str | None,
    reason: str,
    identity: str,
    sent: WriteBackResult,
) -> WriteBackRecord:
    """The FR-B2 ledger row for one successful write-through."""
    return WriteBackRecord(
        id=ctx.id_factory("wbk"),
        at=ctx.clock(),
        project_id=project.id,
        work_item_id=None,
        actor_id=actor.id,
        action=action,  # type: ignore[arg-type]
        subject_key=sent.subject_key or subject_key,
        policy=project.write_back,
        outcome="succeeded",
        reason=reason,
        detail=f"as {identity}",
        source_url=sent.source_url,
    )


def _merged_overlay(
    existing: WorkOverlay | None,
    *,
    subject_key: str,
    project_id: str,
    at: datetime,
    **changes: object,
) -> WorkOverlay:
    """The overlay row to store: the existing one plus these changes."""
    if existing is None:
        base = WorkOverlay(
            subject_key=subject_key,
            project_id=project_id,
            created_at=at,
            updated_at=at,
        )
    else:
        base = existing
    return base.model_copy(update={**changes, "updated_at": at})


# -- create ----------------------------------------------------------------


def create_work(ctx: AppContext, params: CreateWorkParams) -> WorkResult:
    """Create a work item (FR-W1) — upstream on a linked project (#181).

    Three paths, split by the project named: none → a native declared item,
    unchanged; linked → `provider.create_issue` write-through, the subject
    key becomes the ref, and an overlay row carries the vogt-local fields;
    unlinked → the typed decision-10 refusal.
    """
    if params.project is not None:
        with ctx.declared.read() as view:
            project = _resolve.project(view, params.project)
        if not upstream.is_linked(project):
            raise _refuse_unlinked(project, "work.create")
        return _create_upstream(ctx, params, project)
    return _create_native(ctx, params)


def _create_native(ctx: AppContext, params: CreateWorkParams) -> WorkResult:
    """The pre-#181 native create, for items that belong to no project."""

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[WorkResult]:
        del actor
        workflow = txn.workflow_for(params.kind)
        initiative_id = (
            None
            if params.initiative is None
            else _resolve.initiative(txn, params.initiative).id
        )
        assignee = (
            None if params.assignee is None else _resolve.actor(txn, params.assignee)
        )
        labels = list(params.labels or [])
        for name in labels:
            _resolve.label_exists(txn, name)

        now = ctx.clock()
        item = WorkItem(
            id=ctx.id_factory("wrk"),
            ref=txn.next_work_ref(),
            kind=params.kind,
            title=params.title,
            body=params.body,
            state=workflow.initial_state,
            priority=params.priority,
            effort=params.effort,
            project_id=None,
            initiative_id=initiative_id,
            origin="created",
            trust_state="unverified",
            assignee_actor_id=None if assignee is None else assignee.id,
            labels=labels,
            created_at=now,
            updated_at=now,
        )
        txn.insert_work_item(item)
        stored = txn.work_item_by_id(item.id)
        assert stored is not None  # written in this transaction
        return WriteOutcome(
            result=WorkResult(item=stored),
            entity_kind="work_item",
            entity_id=item.id,
            payload=stored.model_dump(mode="json"),
            event_kind=WORK_CREATED_EVENT,
            summary={"ref": item.ref, "kind": item.kind, "title": item.title},
        )

    return audited_write(ctx, operation=WORK_CREATE, reason=params.reason, body=body)


def _create_upstream(
    ctx: AppContext, params: CreateWorkParams, project: Project
) -> WorkResult:
    """Write-through create: the issue first, the overlay only on success.

    Ordering is the fail-loud guarantee (decision 9). The provider call runs
    before the declared transaction opens — a network round-trip must not
    hold the write lock — and a refusal or failure raises before anything
    local exists. The crash window the ordering leaves (issue created, local
    transaction lost) heals on the next sweep, because the mirror re-observes
    the issue like anything else (FR-B2); the reverse window — a local item
    the forge never heard of — is the one decision 9 forbids.
    """
    _require_permitted(project, "create")
    labels = list(params.labels or [])
    with ctx.declared.read() as view:
        workflow = view.workflow_for(params.kind)
        initiative_id = (
            None
            if params.initiative is None
            else _resolve.initiative(view, params.initiative).id
        )
        assignee = (
            None if params.assignee is None else _resolve.actor(view, params.assignee)
        )
        for name in labels:
            _resolve.label_exists(view, name)

    provider, identity, repo = _upstream_writer(ctx, project)
    sent = _sent_or_fail(
        provider.create_issue(
            repo,
            title=params.title,
            body=params.body or "",
            labels=labels or None,
        ),
        "issue create",
    )
    subject_key = sent.subject_key
    if not subject_key:
        raise UpstreamWriteFailed(
            "the forge accepted the issue but returned no number, so there "
            "is no subject key to track it under — nothing was stored locally"
        )

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[WorkResult]:
        now = ctx.clock()
        txn.upsert_work_overlay(
            _merged_overlay(
                txn.work_overlay(subject_key),
                subject_key=subject_key,
                project_id=project.id,
                at=now,
                workflow_state=workflow.initial_state,
                priority=params.priority,
                effort=params.effort,
                assignee_actor_id=None if assignee is None else assignee.id,
                initiative_id=initiative_id,
            )
        )
        txn.insert_writeback(
            _upstream_record(
                ctx,
                actor=actor,
                action="create",
                project=project,
                subject_key=subject_key,
                reason=params.reason,
                identity=identity,
                sent=sent,
            )
        )
        # The mirror learns about the issue on the next sweep; until then
        # this is the item, assembled from what was just sent — which is
        # exactly what the forge holds.
        item = WorkItem(
            id=subject_key,
            ref=subject_key,
            kind=params.kind,
            title=params.title,
            body=params.body,
            state=workflow.initial_state,
            priority=params.priority,
            effort=params.effort,
            project_id=project.id,
            project_slug=project.slug,
            initiative_id=initiative_id,
            origin="observed",
            trust_state="unverified",
            assignee_actor_id=None if assignee is None else assignee.id,
            labels=labels,
            created_at=now,
            updated_at=now,
        )
        return WriteOutcome(
            result=WorkResult(item=item),
            entity_kind="work_item",
            entity_id=subject_key,
            payload=item.model_dump(mode="json"),
            event_kind=WORK_CREATED_EVENT,
            summary={"ref": subject_key, "kind": item.kind, "title": item.title},
        )

    return audited_write(ctx, operation=WORK_CREATE, reason=params.reason, body=body)


# -- reads -----------------------------------------------------------------


def get_work(ctx: AppContext, params: GetWorkParams) -> WorkResult:
    """One work item, with what is running for it (FR-E4).

    The sessions are read through the same path `session.list` uses, so a
    work item's view and the session list cannot disagree about liveness —
    and an engine that cannot be reached costs the activity badge here
    exactly as it does there, rather than costing the item.

    A subject key resolves here like a `WI-n` ref does (#181, design §6):
    the returned item is the observed mirror joined to the overlay. Its
    comments live upstream (the mirror's `source_url` is the discussion),
    so the local comment list is empty for upstream-truth items.
    """
    with ctx.declared.read() as view:
        item = upstream.resolve_work_ref(ctx, view, params.ref)
        native = view.work_item_by_ref(params.ref) is not None
        comments = (
            view.comments_for(item.id, limit=params.comment_limit)
            if params.comment_limit and native
            else []
        )
        # Where this item is worked on in git (#283): declared branches from
        # the overlay joined to the observed ones, kept separate so a
        # disagreement reads as drift. Computed for upstream items too, which
        # have no native sessions but can still carry a `gh-<n>` branch.
        branches = branch_views_for(ctx, view, item)
    sessions = (
        list_sessions(ctx, ListSessionsParams(work_item=item.ref)).sessions
        if native
        else []
    )
    return WorkResult(
        item=item, comments=comments, sessions=sessions, branches=branches
    )


def list_work(ctx: AppContext, params: ListWorkParams) -> WorkListResult:
    """Work items in scope: declared rows, plus upstream-truth items (#181).

    On a linked project the list *is* the mirror joined to the overlay; a
    global list carries every linked project's upstream items alongside the
    declared rows. Each upstream issue appears exactly once — a subject an
    old-model `work_link` adopted is excluded in `upstream_items`, because
    its declared row is the item.

    An **unlinked** project scope answers with the #183 marker: empty items
    and `link_state: "unlinked"`, the machine-readable half of the
    link-or-publish CTA. Not an error, because asking is legitimate; not the
    native rows either, because the forge-less work surface is withdrawn —
    they remain reachable by ref, and a link or publish migrates them.
    """
    with ctx.declared.read() as view:
        project_row = (
            None if params.project is None else _resolve.project(view, params.project)
        )
        if project_row is not None and not upstream.is_linked(project_row):
            return WorkListResult(items=[], total=0, link_state="unlinked")
        work_filter = WorkFilter(
            project_id=None if project_row is None else project_row.id,
            kinds=tuple(params.kinds or ()),
            states=tuple(params.states or ()),
            priorities=tuple(params.priorities or ()),
            assignee_actor_id=(
                None
                if params.assignee is None
                else _resolve.actor(view, params.assignee).id
            ),
            initiative_id=(
                None
                if params.initiative is None
                else _resolve.initiative(view, params.initiative).id
            ),
            label=params.label,
            exclude_terminal=not params.include_finished,
            limit=params.limit,
            offset=params.offset,
        )
        upstream_rows: list[WorkItem] = []
        for linked in upstream.linked_projects(view, project_row):
            upstream_rows.extend(
                item
                for item in upstream.upstream_items(
                    ctx, view, linked, include_closed=params.include_finished
                )
                if upstream.matches(item, work_filter)
            )
        scope_state: Literal["linked"] | None = (
            None if project_row is None else "linked"
        )
        if not upstream_rows:
            return WorkListResult(
                items=view.list_work_items(work_filter),
                total=view.count_work_items(work_filter),
                link_state=scope_state,
            )
        # Merged paging: the declared page window cannot be pushed into SQL
        # once upstream rows join the list, so both halves are gathered and
        # the one ordering — (created_at, ref), the same the SQL uses — is
        # sliced once.
        unpaged = replace(
            work_filter,
            limit=work_filter.limit + work_filter.offset + len(upstream_rows),
            offset=0,
        )
        declared_rows = view.list_work_items(unpaged)
        merged = sorted(
            [*declared_rows, *upstream_rows],
            key=lambda item: (item.created_at, item.ref),
        )
        total = view.count_work_items(work_filter) + len(upstream_rows)
    return WorkListResult(
        items=merged[params.offset : params.offset + params.limit],
        total=total,
        link_state=scope_state,
    )


# -- update ----------------------------------------------------------------


def update_work(ctx: AppContext, params: UpdateWorkParams) -> WorkResult:
    """Change a work item's fields. State changes go through `transition`."""
    with ctx.declared.read() as view:
        item = upstream.resolve_work_ref(ctx, view, params.ref)
        native = view.work_item_by_ref(params.ref) is not None
        project = (
            None if item.project_id is None else view.project_by_id(item.project_id)
        )
    if not native:
        assert project is not None  # an upstream item always has its project
        return _update_upstream(ctx, params, item, project)
    if (
        project is not None
        and not upstream.is_linked(project)
        and (params.add_labels or params.remove_labels)
    ):
        # Label writes are shared vocabulary with the forge (decision 10);
        # everything else on a native item stays a local edit.
        raise _refuse_unlinked(project, "work.update with labels")
    return _update_native(ctx, params)


def _update_native(ctx: AppContext, params: UpdateWorkParams) -> WorkResult:
    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[WorkResult]:
        del actor
        item = _resolve.work_item(txn, params.ref)
        add_labels = list(params.add_labels or [])
        for name in add_labels:
            _resolve.label_exists(txn, name)

        update = WorkItemUpdate(
            title=params.title,
            body=params.body,
            priority=params.priority,
            effort=params.effort,
            assignee_actor_id=(
                None
                if params.assignee is None
                else _resolve.actor(txn, params.assignee).id
            ),
            initiative_id=(
                None
                if params.initiative is None
                else _resolve.initiative(txn, params.initiative).id
            ),
            project_id=(
                None
                if params.project is None
                else _resolve.project(txn, params.project).id
            ),
            clear_effort=params.clear_effort,
            clear_assignee=params.clear_assignee,
            clear_initiative=params.clear_initiative,
            add_labels=tuple(add_labels),
            remove_labels=tuple(params.remove_labels or ()),
        )
        txn.update_work_item(item.id, update, at=ctx.clock())
        updated = txn.work_item_by_id(item.id)
        assert updated is not None  # written in this transaction
        return WriteOutcome(
            result=WorkResult(item=updated),
            entity_kind="work_item",
            entity_id=item.id,
            payload=updated.model_dump(mode="json"),
            event_kind=WORK_UPDATED_EVENT,
            summary={"ref": item.ref},
        )

    return audited_write(ctx, operation=WORK_UPDATE, reason=params.reason, body=body)


def _update_upstream(
    ctx: AppContext, params: UpdateWorkParams, item: WorkItem, project: Project
) -> WorkResult:
    """Split the update along the boundary decision 2 draws.

    Labels are shared vocabulary → an upstream `add_labels` write, fail-loud.
    Title/body are upstream truth vogt does not edit (FR-B4's verb set has no
    edit), and an item cannot move project. Priority, effort, assignee and
    initiative are vogt-local → overlay only, zero provider calls — the
    invariant the transport-recording test pins.
    """
    if params.title is not None or params.body is not None:
        raise InvalidRequest(
            f"{item.ref} is upstream truth: vogt does not edit issue titles "
            "or bodies on a linked project (FR-B4's verb set) — edit it on "
            "the forge"
        )
    if params.project is not None:
        raise InvalidRequest(
            f"{item.ref} lives in {project.slug!r}'s repository; an upstream "
            "issue cannot be moved to another project"
        )
    if params.remove_labels:
        raise InvalidRequest(
            f"{item.ref} is upstream truth and the write surface is "
            "append-only (FR-B4): labels can be added upstream, never "
            "removed from here — remove them on the forge"
        )

    add_labels = list(params.add_labels or [])
    with ctx.declared.read() as view:
        for name in add_labels:
            _resolve.label_exists(view, name)
        assignee_id = (
            None
            if params.assignee is None
            else _resolve.actor(view, params.assignee).id
        )
        initiative_id = (
            None
            if params.initiative is None
            else _resolve.initiative(view, params.initiative).id
        )

    sent = None
    identity = ""
    if add_labels:
        _require_permitted(project, "label")
        provider, identity, repo = _upstream_writer(ctx, project)
        number = provider.number_of(item.ref)
        if number is None:
            raise UpstreamWriteFailed(
                f"{item.ref} carries no issue number to label upstream"
            )
        sent = _sent_or_fail(
            provider.add_labels(repo, number, add_labels),
            "label add",
        )

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[WorkResult]:
        now = ctx.clock()
        existing = txn.work_overlay(item.ref)
        changes: dict[str, object] = {}
        if params.priority is not None:
            changes["priority"] = params.priority
        if params.effort is not None:
            changes["effort"] = params.effort
        elif params.clear_effort:
            changes["effort"] = None
        if assignee_id is not None:
            changes["assignee_actor_id"] = assignee_id
        elif params.clear_assignee:
            changes["assignee_actor_id"] = None
        if initiative_id is not None:
            changes["initiative_id"] = initiative_id
        elif params.clear_initiative:
            changes["initiative_id"] = None
        txn.upsert_work_overlay(
            _merged_overlay(
                existing,
                subject_key=item.ref,
                project_id=project.id,
                at=now,
                **changes,
            )
        )
        if sent is not None:
            txn.insert_writeback(
                _upstream_record(
                    ctx,
                    actor=actor,
                    action="label",
                    project=project,
                    subject_key=item.ref,
                    reason=params.reason,
                    identity=identity,
                    sent=sent,
                )
            )
        merged_labels = [
            *item.labels,
            *[la for la in add_labels if la not in item.labels],
        ]
        updated = item.model_copy(
            update={
                **{
                    key: value
                    for key, value in changes.items()
                    if key
                    in ("priority", "effort", "assignee_actor_id", "initiative_id")
                },
                "labels": merged_labels,
                "updated_at": now,
            }
        )
        return WriteOutcome(
            result=WorkResult(item=updated),
            entity_kind="work_item",
            entity_id=item.ref,
            payload=updated.model_dump(mode="json"),
            event_kind=WORK_UPDATED_EVENT,
            summary={"ref": item.ref},
        )

    return audited_write(ctx, operation=WORK_UPDATE, reason=params.reason, body=body)


# -- transition ------------------------------------------------------------


def transition_work(ctx: AppContext, params: TransitionWorkParams) -> WorkResult:
    """Move an item through its kind's state machine (FR-W2).

    Two rules can refuse, and both name themselves: the machine has no such
    edge, or an unfinished `depends_on` target blocks completion (FR-W8).

    On an upstream-truth item (#181), entering a terminal state closes the
    issue and leaving one reopens it — write-through, fail-loud — while a
    move between non-terminal vogt states is overlay-only and sends nothing
    (decision 2).
    """
    with ctx.declared.read() as view:
        item = upstream.resolve_work_ref(ctx, view, params.ref)
        native = view.work_item_by_ref(params.ref) is not None
        project = (
            None if item.project_id is None else view.project_by_id(item.project_id)
        )
    if not native:
        assert project is not None  # an upstream item always has its project
        return _transition_upstream(ctx, params, item, project)
    if project is not None and not upstream.is_linked(project):
        raise _refuse_unlinked(project, "work.transition")
    return _transition_native(ctx, params)


def _transition_native(ctx: AppContext, params: TransitionWorkParams) -> WorkResult:
    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[WorkResult]:
        item = _resolve.work_item(txn, params.ref)
        workflow = txn.workflow_for(item.kind)
        workflow.check(from_state=item.state, to_state=params.to_state)
        blockers = txn.unfinished_blockers(
            item.id, terminal_states=tuple(sorted(TERMINAL_STATES))
        )
        check_completion_allowed(
            to_state=params.to_state,
            blockers=[(blocker.ref, blocker.state) for blocker in blockers],
        )
        txn.update_work_item(
            item.id, WorkItemUpdate(state=params.to_state), at=ctx.clock()
        )
        updated = txn.work_item_by_id(item.id)
        assert updated is not None  # written in this transaction

        # Additive and forward-only, both directions recoverable by the
        # other (FR-B4). Reaching a terminal state closes upstream; leaving
        # one reopens. Nothing else is ever sent.
        action = (
            "close"
            if params.to_state in TERMINAL_STATES
            else ("reopen" if item.state in TERMINAL_STATES else None)
        )
        if action is not None:
            txn.insert_writeback(
                writeback.attempt(
                    ctx,
                    actor=actor,
                    action=action,
                    project=_project_of(txn, item),
                    item=updated,
                    subject_key=_linked_subject(txn, item),
                    reason=params.reason,
                )
            )

        return WriteOutcome(
            result=WorkResult(item=updated),
            entity_kind="work_item",
            entity_id=item.id,
            payload=updated.model_dump(mode="json"),
            event_kind=WORK_TRANSITIONED_EVENT,
            summary={"ref": item.ref, "from": item.state, "to": params.to_state},
        )

    return audited_write(
        ctx, operation=WORK_TRANSITION, reason=params.reason, body=body
    )


def _transition_upstream(
    ctx: AppContext, params: TransitionWorkParams, item: WorkItem, project: Project
) -> WorkResult:
    with ctx.declared.read() as view:
        workflow = view.workflow_for(item.kind)
    workflow.check(from_state=item.state, to_state=params.to_state)

    action = (
        "close"
        if params.to_state in TERMINAL_STATES
        else ("reopen" if item.state in TERMINAL_STATES else None)
    )
    sent = None
    identity = ""
    if action is not None:
        _require_permitted(project, action)
        provider, identity, repo = _upstream_writer(ctx, project)
        number = provider.number_of(item.ref)
        if number is None:
            raise UpstreamWriteFailed(
                f"{item.ref} carries no issue number to {action} upstream"
            )
        sent = _sent_or_fail(
            provider.set_state(repo, number, "closed" if action == "close" else "open"),
            action,
        )

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[WorkResult]:
        now = ctx.clock()
        txn.upsert_work_overlay(
            _merged_overlay(
                txn.work_overlay(item.ref),
                subject_key=item.ref,
                project_id=project.id,
                at=now,
                workflow_state=params.to_state,
            )
        )
        if sent is not None:
            txn.insert_writeback(
                _upstream_record(
                    ctx,
                    actor=actor,
                    action=action or "close",
                    project=project,
                    subject_key=item.ref,
                    reason=params.reason,
                    identity=identity,
                    sent=sent,
                )
            )
        updated = item.model_copy(update={"state": params.to_state, "updated_at": now})
        return WriteOutcome(
            result=WorkResult(item=updated),
            entity_kind="work_item",
            entity_id=item.ref,
            payload=updated.model_dump(mode="json"),
            event_kind=WORK_TRANSITIONED_EVENT,
            summary={"ref": item.ref, "from": item.state, "to": params.to_state},
        )

    return audited_write(
        ctx, operation=WORK_TRANSITION, reason=params.reason, body=body
    )


# -- relations -------------------------------------------------------------


def relate_work(ctx: AppContext, params: RelateWorkParams) -> WorkResult:
    """Add a typed edge between two work items, cross-project (FR-W8).

    Declared items only in v1 of the upstream-truth model: the relations
    table is keyed by declared ids, and re-keying it to subjects is not part
    of #181. An upstream-truth ref is refused with the reason named rather
    than a bare not-found.
    """

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[WorkResult]:
        del actor
        item = _declared_only(ctx, txn, params.ref)
        target = _declared_only(ctx, txn, params.target)
        if item.id == target.id:
            msg = "a work item cannot relate to itself"
            raise InvalidRequest(msg)
        if any(
            relation.kind == params.kind and relation.related_id == target.id
            for relation in item.relations
        ):
            msg = f"{item.ref} already {params.kind} {target.ref}"
            raise Conflict(msg)
        txn.insert_relation(
            work_item_id=item.id,
            related_id=target.id,
            kind=params.kind,
            at=ctx.clock(),
        )
        updated = txn.work_item_by_id(item.id)
        assert updated is not None  # written in this transaction
        return WriteOutcome(
            result=WorkResult(item=updated),
            entity_kind="work_item",
            entity_id=item.id,
            payload={"ref": item.ref, "kind": params.kind, "target": target.ref},
            event_kind=WORK_RELATED_EVENT,
            summary={"ref": item.ref, "kind": params.kind, "target": target.ref},
        )

    return audited_write(ctx, operation=WORK_RELATE, reason=params.reason, body=body)


def unrelate_work(ctx: AppContext, params: UnrelateWorkParams) -> WorkResult:
    """Remove a typed edge. Mistakes in a graph must be correctable."""

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[WorkResult]:
        del actor
        item = _declared_only(ctx, txn, params.ref)
        target = _declared_only(ctx, txn, params.target)
        removed = txn.delete_relation(
            work_item_id=item.id, related_id=target.id, kind=params.kind
        )
        if not removed:
            msg = f"{item.ref} does not {params.kind} {target.ref}"
            raise InvalidRequest(msg)
        updated = txn.work_item_by_id(item.id)
        assert updated is not None  # written in this transaction
        return WriteOutcome(
            result=WorkResult(item=updated),
            entity_kind="work_item",
            entity_id=item.id,
            payload={"ref": item.ref, "kind": params.kind, "target": target.ref},
            event_kind=WORK_UNRELATED_EVENT,
            summary={"ref": item.ref, "kind": params.kind, "target": target.ref},
        )

    return audited_write(ctx, operation=WORK_UNRELATE, reason=params.reason, body=body)


def _declared_only(ctx: AppContext, view: ReadView, ref: str) -> WorkItem:
    """Resolve a ref that must name a declared row, saying why when it can't."""
    try:
        return _resolve.work_item(view, ref)
    except NotFound:
        if upstream.resolve_upstream(ctx, view, ref) is not None:
            raise InvalidRequest(
                f"{ref} is an upstream-truth item; relations between "
                "upstream issues are not supported in v1 of #181 — relate "
                "them on the forge"
            ) from None
        raise


# -- comments --------------------------------------------------------------


def comment_work(ctx: AppContext, params: CommentParams) -> CommentResult:
    """Add a comment attributed to the acting actor (FR-W6).

    Comments authored here are unambiguously ours: every row has a Vogt actor
    and an audit trail. Inbound forge comments stay observations and are never
    copied in (FR-B5, at M5).

    On an upstream-truth item (#181) the comment *is* the upstream comment:
    it posts through the provider fail-loud and no local row is stored — the
    forge thread is the record, and the mirror is the way it comes back.
    """
    with ctx.declared.read() as view:
        item = upstream.resolve_work_ref(ctx, view, params.ref)
        native = view.work_item_by_ref(params.ref) is not None
        project = (
            None if item.project_id is None else view.project_by_id(item.project_id)
        )
    if not native:
        assert project is not None  # an upstream item always has its project
        return _comment_upstream(ctx, params, item, project)
    if project is not None and not upstream.is_linked(project):
        raise _refuse_unlinked(project, "work.comment")
    return _comment_native(ctx, params)


def _comment_native(ctx: AppContext, params: CommentParams) -> CommentResult:
    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[CommentResult]:
        item = _resolve.work_item(txn, params.ref)
        comment = Comment(
            id=ctx.id_factory("cmt"),
            work_item_id=item.id,
            actor_id=actor.id,
            actor_display_name=actor.display_name,
            body=params.body,
            created_at=ctx.clock(),
        )
        txn.insert_comment(comment)

        # Outbound only (FR-B5): a comment authored here posts upstream; a
        # comment authored on GitHub stays an observation and is never
        # copied into `comments`. That keeps this table unambiguously ours.
        pushed = writeback.attempt(
            ctx,
            actor=actor,
            action="comment",
            project=_project_of(txn, item),
            item=item,
            subject_key=_linked_subject(txn, item),
            reason=params.reason,
            body=f"{params.body}\n\n— posted from Vogt as {item.ref}",
        )
        txn.insert_writeback(pushed)

        return WriteOutcome(
            result=CommentResult(comment=comment, write_back=pushed.outcome),
            entity_kind="comment",
            entity_id=comment.id,
            payload=comment.model_dump(mode="json"),
            event_kind=WORK_COMMENTED_EVENT,
            summary={"ref": item.ref, "comment_id": comment.id},
        )

    return audited_write(ctx, operation=WORK_COMMENT, reason=params.reason, body=body)


def _comment_upstream(
    ctx: AppContext, params: CommentParams, item: WorkItem, project: Project
) -> CommentResult:
    _require_permitted(project, "comment")
    provider, identity, repo = _upstream_writer(ctx, project)
    number = provider.number_of(item.ref)
    if number is None:
        raise UpstreamWriteFailed(
            f"{item.ref} carries no issue number to comment on upstream"
        )
    sent = _sent_or_fail(
        provider.comment(repo, number, params.body),
        "comment",
    )

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[CommentResult]:
        record = _upstream_record(
            ctx,
            actor=actor,
            action="comment",
            project=project,
            subject_key=item.ref,
            reason=params.reason,
            identity=identity,
            sent=sent,
        )
        txn.insert_writeback(record)
        # Returned, not stored: on a linked project the forge thread is the
        # comment's home, and the mirror is how it is read back (FR-B5).
        comment = Comment(
            id=ctx.id_factory("cmt"),
            work_item_id=item.ref,
            actor_id=actor.id,
            actor_display_name=actor.display_name,
            body=params.body,
            created_at=ctx.clock(),
        )
        return WriteOutcome(
            result=CommentResult(comment=comment, write_back=record.outcome),
            entity_kind="comment",
            entity_id=comment.id,
            payload=comment.model_dump(mode="json"),
            event_kind=WORK_COMMENTED_EVENT,
            summary={"ref": item.ref, "comment_id": comment.id},
        )

    return audited_write(ctx, operation=WORK_COMMENT, reason=params.reason, body=body)


# -- shared helpers --------------------------------------------------------


def _project_of(txn: WriteTxn, item: WorkItem) -> Project | None:
    """The project a work item belongs to, if any."""
    return None if item.project_id is None else txn.project_by_id(item.project_id)


def _linked_subject(txn: WriteTxn, item: WorkItem) -> str | None:
    """The forge object this item was adopted from, if it was.

    Write-back speaks only to objects a work item is *linked* to. An item
    nobody adopted has nowhere upstream to speak to, and inventing one would
    be a `create`, which is a different decision.
    """
    for subject, ref in txn.work_links_for_subjects_by_item(item.id).items():
        del ref
        return subject
    return None
