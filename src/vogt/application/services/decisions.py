"""Decisions a person makes for an agent (WI-1321).

An agent — usually an overseer — puts a question to a person with two to four
options, one recommended; a person answers it from the Inbox. The record is
shaped like `SessionGrant` (`grants.py`) so the Inbox and the person-only rule
are reused. The design is `docs/design/agent-harness-services.md` §2.3.

Three rules shape this module:

- **Only a person answers.** `answer_decision` refuses every agent principal —
  session tokens, `agent:engine:` tokens, the brokered pod token — before
  anything else, as `grants.decide_grant` does.
- **Nothing is applied.** A decision is not a grant and not a gate: the asker
  reads the answer and acts through the op that checks the action.
- **Expiry implies no answer.** A pending decision past `expires_at` reads
  `expired`, computed on read; it cannot be answered, and the asker decides
  what that means. There is no background job.
"""

from __future__ import annotations

from datetime import timedelta

from vogt.application import writes
from vogt.application.context import AppContext
from vogt.application.models import (
    AnswerDecisionParams,
    AskDecisionParams,
    DecisionAnswerView,
    DecisionOptionModel,
    DecisionResult,
    DecisionView,
    ListDecisionsParams,
    ListDecisionsResult,
    WithdrawDecisionParams,
)
from vogt.application.services import _resolve
from vogt.application.services.grants import _own_engine_session
from vogt.application.writes import WriteOutcome, audited_write
from vogt.core.entities import Actor, Decision, DecisionAnswer, DecisionOption
from vogt.errors import Conflict, GrantRefused, InvalidRequest, NotFound, PersonRequired
from vogt.storage.interface import ReadView, WriteTxn

DECISION_ASK = "decision.ask"
DECISION_ANSWER = "decision.answer"
DECISION_WITHDRAW = "decision.withdraw"
DECISION_ASKED_EVENT = "decision.asked"
DECISION_ANSWERED_EVENT = "decision.answered"
DECISION_WITHDRAWN_EVENT = "decision.withdrawn"


def ask_decision(ctx: AppContext, params: AskDecisionParams) -> DecisionResult:
    """Record a question for a person; the Inbox shows it until it is answered."""
    reason = writes.validate_reason(params.reason)
    question = params.question.strip()
    if not question:
        raise InvalidRequest("a decision asks a question")
    labels = [option.label.strip() for option in params.options]
    if any(not label for label in labels):
        raise InvalidRequest("every option has a label")
    if len(set(labels)) != len(labels):
        raise InvalidRequest("option labels must differ")
    if sum(option.recommended for option in params.options) != 1:
        raise InvalidRequest("exactly one option is recommended")
    options = [
        DecisionOption(
            label=label,
            description=option.description.strip(),
            recommended=option.recommended,
        )
        for label, option in zip(labels, params.options, strict=True)
    ]
    work_item_id = None
    if params.work_item is not None:
        with ctx.declared.read() as view:
            work_item_id = _resolve.work_item(view, params.work_item).id
    # The asker's session is the token's, never a parameter.
    asker_session = _own_engine_session(ctx) if ctx.principal.kind == "agent" else None

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[DecisionResult]:
        now = ctx.clock()
        decision = Decision(
            id=ctx.id_factory("dec"),
            asker_actor_id=actor.id,
            asker_session=asker_session,
            work_item_id=work_item_id,
            question=question,
            options=options,
            context=params.context.strip(),
            created_at=now,
            expires_at=now + timedelta(seconds=params.ttl_seconds),
            reason=reason,
        )
        txn.insert_decision(decision)
        return _outcome(txn, ctx, decision, DECISION_ASKED_EVENT)

    return audited_write(ctx, operation=DECISION_ASK, reason=reason, body=body)


def answer_decision(ctx: AppContext, params: AnswerDecisionParams) -> DecisionResult:
    """Answer a pending decision with one of its options. A person only."""
    reason = writes.validate_reason(params.reason)
    if ctx.principal.kind == "agent":
        msg = (
            "only a person answers a decision, not an agent "
            f"({ctx.principal.identity_ref}); leave it for the Inbox"
        )
        raise PersonRequired(msg)
    decision = _load(ctx, params.id)
    label = params.label.strip()
    if label not in {option.label for option in decision.options}:
        offered = ", ".join(repr(option.label) for option in decision.options)
        msg = f"{label!r} is not an option of {decision.id}; it offers {offered}"
        raise InvalidRequest(msg)

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[DecisionResult]:
        current = _pending(txn, ctx, decision.id)
        answered = current.model_copy(
            update={
                "state": "answered",
                "answer": DecisionAnswer(label=label, note=params.note.strip()),
                "answered_by_actor_id": actor.id,
                "answered_at": ctx.clock(),
            }
        )
        txn.update_decision(answered)
        return _outcome(txn, ctx, answered, DECISION_ANSWERED_EVENT)

    return audited_write(ctx, operation=DECISION_ANSWER, reason=reason, body=body)


def withdraw_decision(
    ctx: AppContext, params: WithdrawDecisionParams
) -> DecisionResult:
    """Withdraw a pending decision: a person, or the session that asked."""
    reason = writes.validate_reason(params.reason)
    decision = _load(ctx, params.id)
    if ctx.principal.kind == "agent" and not _is_asker(ctx, decision):
        msg = (
            "only a person, or the session that asked, withdraws a decision "
            f"({ctx.principal.identity_ref} did neither)"
        )
        raise GrantRefused(msg)

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[DecisionResult]:
        del actor
        current = _pending(txn, ctx, decision.id)
        withdrawn = current.model_copy(update={"state": "withdrawn"})
        txn.update_decision(withdrawn)
        return _outcome(txn, ctx, withdrawn, DECISION_WITHDRAWN_EVENT)

    return audited_write(ctx, operation=DECISION_WITHDRAW, reason=reason, body=body)


def list_decisions(ctx: AppContext, params: ListDecisionsParams) -> ListDecisionsResult:
    """Decisions, newest first, by state, work item or asker."""
    stored_state = "pending" if params.state == "expired" else params.state
    with ctx.declared.read() as view:
        work_item_id = (
            None
            if params.work_item is None
            else _resolve.work_item(view, params.work_item).id
        )
        asker_id = None
        if params.asker is not None:
            asker = view.actor_by_identity(params.asker.strip())
            if asker is None:
                return ListDecisionsResult(decisions=[])
            asker_id = asker.id
        rows = view.list_decisions(
            state=stored_state,
            work_item_id=work_item_id,
            asker_actor_id=asker_id,
            limit=params.limit,
        )
        views = [_view(view, row, ctx) for row in rows]
    if params.state in ("pending", "expired"):
        views = [v for v in views if v.state == params.state]
    return ListDecisionsResult(decisions=views)


def pending_decisions(view: ReadView, ctx: AppContext) -> list[DecisionView]:
    """What the Inbox shows: every decision still waiting for a person."""
    rows = view.list_decisions(state="pending")
    return [v for v in (_view(view, row, ctx) for row in rows) if v.state == "pending"]


# -- helpers ---------------------------------------------------------------


def _is_asker(ctx: AppContext, decision: Decision) -> bool:
    with ctx.declared.read() as view:
        caller = view.actor_by_identity(ctx.principal.identity_ref)
    if caller is not None and caller.id == decision.asker_actor_id:
        return True
    own = _own_engine_session(ctx)
    return own is not None and own == decision.asker_session


def _load(ctx: AppContext, decision_id: str) -> Decision:
    with ctx.declared.read() as view:
        decision = view.decision(decision_id.strip())
    if decision is None:
        msg = f"no decision {decision_id!r}"
        raise NotFound(msg)
    return decision


def _pending(txn: WriteTxn, ctx: AppContext, decision_id: str) -> Decision:
    current = txn.decision(decision_id)
    if current is None:
        msg = f"no decision {decision_id!r}"
        raise NotFound(msg)
    state = current.effective_state(ctx.clock())
    if state != "pending":
        msg = f"decision {decision_id} is {state}, not pending"
        raise Conflict(msg)
    return current


def _outcome(
    txn: WriteTxn, ctx: AppContext, decision: Decision, event: str
) -> WriteOutcome[DecisionResult]:
    return WriteOutcome(
        result=DecisionResult(decision=_view(txn, decision, ctx)),
        entity_kind="decision",
        entity_id=decision.id,
        payload=decision.model_dump(mode="json"),
        event_kind=event,
        summary=_summary(txn, decision),
    )


def _ref(view: ReadView, actor_id: str | None) -> str | None:
    if actor_id is None:
        return None
    actor = view.actor_by_id(actor_id)
    return actor_id if actor is None else actor.identity_ref


def _work_ref(view: ReadView, work_item_id: str | None) -> str | None:
    if work_item_id is None:
        return None
    item = view.work_item_by_id(work_item_id)
    return work_item_id if item is None else item.ref


def _view(view: ReadView, decision: Decision, ctx: AppContext) -> DecisionView:
    return DecisionView(
        id=decision.id,
        asker=_ref(view, decision.asker_actor_id) or decision.asker_actor_id,
        asker_session=decision.asker_session,
        work_item=_work_ref(view, decision.work_item_id),
        question=decision.question,
        options=[
            DecisionOptionModel(
                label=o.label, description=o.description, recommended=o.recommended
            )
            for o in decision.options
        ],
        context=decision.context,
        state=decision.effective_state(ctx.clock()),
        answer=None
        if decision.answer is None
        else DecisionAnswerView(label=decision.answer.label, note=decision.answer.note),
        answered_by=_ref(view, decision.answered_by_actor_id),
        created_at=decision.created_at,
        answered_at=decision.answered_at,
        expires_at=decision.expires_at,
        reason=decision.reason,
    )


def _summary(view: ReadView, decision: Decision) -> dict[str, object]:
    return {
        "decision_id": decision.id,
        "asker_session": decision.asker_session,
        "work_item": _work_ref(view, decision.work_item_id),
        "question": decision.question[:200],
        "state": decision.state,
        "answer": None if decision.answer is None else decision.answer.label,
    }
