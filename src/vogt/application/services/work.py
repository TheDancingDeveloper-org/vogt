"""The write plane: work items, transitions, relations, comments."""

from __future__ import annotations

from vogt.application.context import AppContext
from vogt.application.models import (
    CommentParams,
    CommentResult,
    CreateWorkParams,
    GetWorkParams,
    ListWorkParams,
    RelateWorkParams,
    TransitionWorkParams,
    UnrelateWorkParams,
    UpdateWorkParams,
    WorkListResult,
    WorkResult,
)
from vogt.application.services import _resolve
from vogt.application.writes import WriteOutcome, audited_write
from vogt.core.entities import Actor, Comment, WorkItem
from vogt.core.workflow import (
    TERMINAL_STATES,
    check_completion_allowed,
)
from vogt.errors import Conflict, InvalidRequest
from vogt.storage.interface import WorkFilter, WorkItemUpdate, WriteTxn

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


def create_work(ctx: AppContext, params: CreateWorkParams) -> WorkResult:
    """Create a work item in its kind's initial state (FR-W1)."""

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[WorkResult]:
        del actor
        workflow = txn.workflow_for(params.kind)
        project_id = (
            None if params.project is None else _resolve.project(txn, params.project).id
        )
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
            project_id=project_id,
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


def get_work(ctx: AppContext, params: GetWorkParams) -> WorkResult:
    with ctx.declared.read() as view:
        item = _resolve.work_item(view, params.ref)
        comments = (
            view.comments_for(item.id, limit=params.comment_limit)
            if params.comment_limit
            else []
        )
        return WorkResult(item=item, comments=comments)


def list_work(ctx: AppContext, params: ListWorkParams) -> WorkListResult:
    with ctx.declared.read() as view:
        work_filter = WorkFilter(
            project_id=(
                None
                if params.project is None
                else _resolve.project(view, params.project).id
            ),
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
        return WorkListResult(
            items=view.list_work_items(work_filter),
            total=view.count_work_items(work_filter),
        )


def update_work(ctx: AppContext, params: UpdateWorkParams) -> WorkResult:
    """Change a work item's fields. State changes go through `transition`."""

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


def transition_work(ctx: AppContext, params: TransitionWorkParams) -> WorkResult:
    """Move an item through its kind's state machine (FR-W2).

    Two rules can refuse, and both name themselves: the machine has no such
    edge, or an unfinished `depends_on` target blocks completion (FR-W8).
    """

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[WorkResult]:
        del actor
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


def relate_work(ctx: AppContext, params: RelateWorkParams) -> WorkResult:
    """Add a typed edge between two work items, cross-project (FR-W8)."""

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[WorkResult]:
        del actor
        item = _resolve.work_item(txn, params.ref)
        target = _resolve.work_item(txn, params.target)
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
        item = _resolve.work_item(txn, params.ref)
        target = _resolve.work_item(txn, params.target)
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


def comment_work(ctx: AppContext, params: CommentParams) -> CommentResult:
    """Add a comment attributed to the acting actor (FR-W6).

    Comments authored here are unambiguously ours: every row has a Vogt actor
    and an audit trail. Inbound forge comments stay observations and are never
    copied in (FR-B5, at M5).
    """

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
        return WriteOutcome(
            result=CommentResult(comment=comment),
            entity_kind="comment",
            entity_id=comment.id,
            payload=comment.model_dump(mode="json"),
            event_kind=WORK_COMMENTED_EVENT,
            summary={"ref": item.ref, "comment_id": comment.id},
        )

    return audited_write(ctx, operation=WORK_COMMENT, reason=params.reason, body=body)
