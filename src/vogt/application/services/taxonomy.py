"""Labels, initiatives, actors, and the workflows transitions are checked against."""

from __future__ import annotations

from vogt.application.context import AppContext
from vogt.application.models import (
    ActorListResult,
    ActorResult,
    CreateActorParams,
    CreateInitiativeParams,
    CreateLabelParams,
    InitiativeListResult,
    InitiativeResult,
    LabelListResult,
    LabelResult,
    ListActorsParams,
    ListInitiativesParams,
    ListLabelsParams,
    WorkflowListParams,
    WorkflowListResult,
    WorkflowView,
)
from vogt.application.writes import WriteOutcome, audited_write
from vogt.core.entities import Actor, Initiative, Label
from vogt.core.ids import slugify
from vogt.errors import Conflict, InvalidRequest
from vogt.storage.interface import WriteTxn

LABEL_CREATE = "label.create"
INITIATIVE_CREATE = "initiative.create"
ACTOR_CREATE = "actor.create"

LABEL_CREATED_EVENT = "label.created"
INITIATIVE_CREATED_EVENT = "initiative.created"
ACTOR_CREATED_EVENT = "actor.created"

WORK_KINDS = ("feature", "bug", "chore", "question")


def create_label(ctx: AppContext, params: CreateLabelParams) -> LabelResult:
    """Define a tag, shared instance-wide and GitHub-label aligned (FR-W9)."""

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[LabelResult]:
        del actor
        if txn.label_by_name(params.name) is not None:
            msg = f"a label named {params.name!r} already exists"
            raise Conflict(msg)
        label = Label(
            id=ctx.id_factory("lbl"),
            name=params.name,
            color=params.color,
            created_at=ctx.clock(),
        )
        txn.insert_label(label)
        return WriteOutcome(
            result=LabelResult(label=label),
            entity_kind="label",
            entity_id=label.id,
            payload=label.model_dump(mode="json"),
            event_kind=LABEL_CREATED_EVENT,
            summary={"name": label.name},
        )

    return audited_write(ctx, operation=LABEL_CREATE, reason=params.reason, body=body)


def list_labels(ctx: AppContext, params: ListLabelsParams) -> LabelListResult:
    with ctx.declared.read() as view:
        return LabelListResult(
            labels=view.list_labels(limit=params.limit, offset=params.offset)
        )


def create_initiative(
    ctx: AppContext, params: CreateInitiativeParams
) -> InitiativeResult:
    """Create a cross-project epic whose weight feeds ranking (FR-W3)."""
    slug = slugify(params.title)
    if not slug:
        msg = f"cannot derive a slug from title {params.title!r}"
        raise InvalidRequest(msg)

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[InitiativeResult]:
        del actor
        if txn.initiative_by_slug(slug) is not None:
            msg = f"an initiative with slug {slug!r} already exists"
            raise Conflict(msg)
        now = ctx.clock()
        initiative = Initiative(
            id=ctx.id_factory("ini"),
            slug=slug,
            title=params.title,
            body=params.body,
            state=params.state,
            weight=params.weight,
            created_at=now,
            updated_at=now,
        )
        txn.insert_initiative(initiative)
        return WriteOutcome(
            result=InitiativeResult(initiative=initiative),
            entity_kind="initiative",
            entity_id=initiative.id,
            payload=initiative.model_dump(mode="json"),
            event_kind=INITIATIVE_CREATED_EVENT,
            summary={"slug": initiative.slug, "weight": initiative.weight},
        )

    return audited_write(
        ctx, operation=INITIATIVE_CREATE, reason=params.reason, body=body
    )


def list_initiatives(
    ctx: AppContext, params: ListInitiativesParams
) -> InitiativeListResult:
    with ctx.declared.read() as view:
        return InitiativeListResult(
            initiatives=view.list_initiatives(limit=params.limit, offset=params.offset)
        )


def create_actor(ctx: AppContext, params: CreateActorParams) -> ActorResult:
    """Register a human or agent so work can be assigned to it (FR-W7).

    Note what this does *not* do: it creates no credential and grants no
    access. An actor here is somebody work can be attributed to; the tokens
    that let them act arrive at M4 (FR-S3), and the acting principal is never
    taken from a parameter (FR-S2).
    """
    if params.kind not in ("human", "agent"):
        msg = f"actor kind must be 'human' or 'agent', not {params.kind!r}"
        raise InvalidRequest(msg)

    def body(txn: WriteTxn, acting: Actor) -> WriteOutcome[ActorResult]:
        del acting
        if txn.actor_by_identity(params.identity_ref) is not None:
            msg = f"an actor with identity {params.identity_ref!r} already exists"
            raise Conflict(msg)
        actor = Actor(
            id=ctx.id_factory("act"),
            kind="agent" if params.kind == "agent" else "human",
            display_name=params.display_name,
            identity_ref=params.identity_ref,
            disabled=False,
            created_at=ctx.clock(),
        )
        txn.insert_actor(actor)
        return WriteOutcome(
            result=ActorResult(actor=actor),
            entity_kind="actor",
            entity_id=actor.id,
            payload=actor.model_dump(mode="json"),
            event_kind=ACTOR_CREATED_EVENT,
            summary={"identity_ref": actor.identity_ref, "kind": actor.kind},
        )

    return audited_write(ctx, operation=ACTOR_CREATE, reason=params.reason, body=body)


def list_actors(ctx: AppContext, params: ListActorsParams) -> ActorListResult:
    with ctx.declared.read() as view:
        return ActorListResult(
            actors=view.list_actors(limit=params.limit, offset=params.offset)
        )


def list_workflows(ctx: AppContext, params: WorkflowListParams) -> WorkflowListResult:
    """Publish the state machines transitions are checked against (FR-W2).

    An agent that can read the machine can pick a legal next state instead of
    guessing and handling a rejection — which is the difference between a
    tool an agent can drive and one it can only poke.
    """
    del params
    with ctx.declared.read() as view:
        workflows = [view.workflow_for(kind) for kind in WORK_KINDS]
    return WorkflowListResult(
        workflows=[
            WorkflowView(
                kind=workflow.kind,
                initial_state=workflow.initial_state,
                states=sorted(workflow.states),
                transitions={
                    source: list(targets)
                    for source, targets in sorted(workflow.transitions.items())
                },
            )
            for workflow in workflows
        ]
    )
