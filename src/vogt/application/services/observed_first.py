"""Suppression and adoption — the two decisions a person makes about evidence.

Observed-first shows you work nobody typed in. That is only tolerable if
there are cheap, audited ways to say "this is not work" and "this *is* work,
and now it is mine". Those are the two operations here.

Both are declared writes with a required reason, because both are judgements
somebody will want explained later.
"""

from __future__ import annotations

from vogt.application.context import AppContext
from vogt.application.models import (
    AdoptParams,
    AdoptResult,
    ListSuppressionsParams,
    RevokeSuppressionParams,
    SuppressionListResult,
    SuppressionResult,
    SuppressParams,
)
from vogt.application.services import _resolve
from vogt.application.writes import WriteOutcome, audited_write
from vogt.core.entities import Actor, Observation, Suppression, WorkItem, WorkLink
from vogt.core.observed import priority_of, title_of, work_kind_of
from vogt.errors import Conflict, InvalidRequest, NotFound
from vogt.storage.interface import WriteTxn

SUPPRESS = "suppress"
SUPPRESSION_REVOKE = "suppression.revoke"
WORK_ADOPT = "work.adopt"

SUPPRESSED_EVENT = "subject.suppressed"
SUPPRESSION_REVOKED_EVENT = "subject.unsuppressed"
ADOPTED_EVENT = "work.adopted"


def suppress(ctx: AppContext, params: SuppressParams) -> SuppressionResult:
    """Remove an observed subject from ranked views, permanently (FR-W10).

    A first-class operation rather than `adopt` + `wont_do`, because the
    latter fabricates a declared work item for every piece of noise. The
    subject stays observable and queryable — the decision hides it from
    views, it does not delete evidence.

    It survives re-observation because it lives in the declared store: the
    next sweep will find the same marker again, and it will stay hidden.
    """

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[SuppressionResult]:
        scope_project_id = (
            None if params.project is None else _resolve.project(txn, params.project).id
        )
        for existing in txn.list_suppressions(limit=1000):
            if (
                existing.subject_key_or_pattern == params.subject
                and existing.scope_project_id == scope_project_id
            ):
                msg = f"{params.subject!r} is already suppressed ({existing.id})"
                raise Conflict(msg)

        suppression = Suppression(
            id=ctx.id_factory("sup"),
            match_kind="pattern" if params.pattern else "exact",
            subject_key_or_pattern=params.subject,
            scope_project_id=scope_project_id,
            actor_id=actor.id,
            actor_identity_ref=actor.identity_ref,
            reason=params.reason,
            created_at=ctx.clock(),
        )
        txn.insert_suppression(suppression)
        return WriteOutcome(
            result=SuppressionResult(suppression=suppression),
            entity_kind="suppression",
            entity_id=suppression.id,
            payload=suppression.model_dump(mode="json"),
            event_kind=SUPPRESSED_EVENT,
            summary={
                "subject": params.subject,
                "match_kind": suppression.match_kind,
            },
        )

    return audited_write(ctx, operation=SUPPRESS, reason=params.reason, body=body)


def list_suppressions(
    ctx: AppContext, params: ListSuppressionsParams
) -> SuppressionListResult:
    with ctx.declared.read() as view:
        return SuppressionListResult(
            suppressions=view.list_suppressions(
                include_revoked=params.include_revoked, limit=params.limit
            )
        )


def revoke_suppression(
    ctx: AppContext, params: RevokeSuppressionParams
) -> SuppressionResult:
    """Un-suppress a subject. Revoked, not deleted: the decision is history."""

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[SuppressionResult]:
        existing = txn.suppression_by_id(params.id)
        if existing is None:
            msg = f"no suppression {params.id!r}"
            raise NotFound(msg)
        revoked = txn.revoke_suppression(
            params.id, actor_id=actor.id, reason=params.reason, at=ctx.clock()
        )
        if not revoked:
            msg = f"suppression {params.id!r} is already revoked"
            raise Conflict(msg)
        updated = txn.suppression_by_id(params.id)
        assert updated is not None  # just written in this transaction
        return WriteOutcome(
            result=SuppressionResult(suppression=updated),
            entity_kind="suppression",
            entity_id=params.id,
            payload=updated.model_dump(mode="json"),
            event_kind=SUPPRESSION_REVOKED_EVENT,
            summary={"subject": updated.subject_key_or_pattern},
        )

    return audited_write(
        ctx, operation=SUPPRESSION_REVOKE, reason=params.reason, body=body
    )


def adopt(ctx: AppContext, params: AdoptParams) -> AdoptResult:
    """Promote an observed subject into a declared work item (FR-W5).

    The link back to the origin is maintained, which is what lets drift keep
    the pair honest from M3: an issue closed upstream becomes a proposal to
    close the item here, with the evidence attached.

    Everything Vogt inferred about the subject — its kind, its priority — is
    overridable in the same call, because adoption is exactly the moment
    somebody is in a position to correct a guess.
    """
    observation = _subject(ctx, params.subject)
    inferred_kind = work_kind_of(observation)
    inferred_priority = priority_of(observation)

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[AdoptResult]:
        del actor
        if txn.work_item_by_subject(params.subject) is not None:
            existing = txn.work_item_by_subject(params.subject)
            assert existing is not None  # just checked
            msg = f"{params.subject!r} was already adopted as {existing.ref}"
            raise Conflict(msg)

        project_id = observation.project_id
        if params.project is not None:
            project_id = _resolve.project(txn, params.project).id
        assignee = (
            None if params.assignee is None else _resolve.actor(txn, params.assignee)
        )

        kind = params.kind or inferred_kind
        workflow = txn.workflow_for(kind)
        now = ctx.clock()
        item = WorkItem(
            id=ctx.id_factory("wrk"),
            ref=txn.next_work_ref(),
            kind=kind,
            title=title_of(observation)[:300],
            body=_body_for(observation),
            state=workflow.initial_state,
            priority=params.priority or inferred_priority,
            project_id=project_id,
            # `adopted`, not `created`: this work existed before Vogt was
            # told about it, and the origin is part of its provenance.
            origin="adopted",
            trust_state="unverified",
            assignee_actor_id=None if assignee is None else assignee.id,
            created_at=now,
            updated_at=now,
        )
        txn.insert_work_item(item)
        txn.insert_work_link(
            WorkLink(
                work_item_id=item.id,
                subject_key=observation.subject_key,
                origin_kind=observation.kind,
                source_url=observation.source_url,
                relation="completion",
                created_at=now,
            )
        )
        stored = txn.work_item_by_id(item.id)
        assert stored is not None  # written in this transaction
        return WriteOutcome(
            result=AdoptResult(
                item=stored,
                subject_key=observation.subject_key,
                inferred_kind=inferred_kind,
                inferred_priority=inferred_priority,
            ),
            entity_kind="work_item",
            entity_id=item.id,
            payload=stored.model_dump(mode="json"),
            event_kind=ADOPTED_EVENT,
            summary={
                "ref": item.ref,
                "subject": observation.subject_key,
                "origin_kind": observation.kind,
            },
        )

    return audited_write(ctx, operation=WORK_ADOPT, reason=params.reason, body=body)


def _subject(ctx: AppContext, subject_key: str) -> Observation:
    if not ctx.observed.has_evidence_tables():
        msg = "nothing has been swept yet; there are no subjects to adopt"
        raise InvalidRequest(msg)
    found = ctx.observed.list_observations(subject_key=subject_key, limit=1)
    if not found:
        msg = f"no observed subject {subject_key!r}"
        raise NotFound(msg)
    return found[0]


def _body_for(observation: Observation) -> str:
    """Carry the evidence into the item, so the item stands on its own."""
    lines = [f"Adopted from observed subject `{observation.subject_key}`."]
    if observation.source_url:
        lines.append(f"Source: {observation.source_url}")
    if observation.kind == "marker":
        payload = observation.payload
        lines.append(
            f"Marker `{payload.get('tag')}` at "
            f"{payload.get('path')}:{payload.get('line')}"
        )
        text = str(payload.get("text", "")).strip()
        if text:
            lines.append("")
            lines.append(text)
    return "\n".join(lines)
