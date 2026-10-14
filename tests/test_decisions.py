"""Decisions a person makes for an agent (WI-1321).

`docs/design/agent-harness-services.md` §2.3. A decision is shaped like a
grant: an agent asks, only a person answers, and the Inbox shows it until
then. Nothing reaches the engine, so no engine stand-in is needed.
"""

from __future__ import annotations

import dataclasses
from datetime import timedelta
from pathlib import Path
from typing import Any

import pytest
from pydantic import ValidationError

from vogt.application.context import AppContext
from vogt.application.models import (
    AnswerDecisionParams,
    AskDecisionParams,
    CreateWorkParams,
    InboxListParams,
    ListAuditParams,
    ListDecisionsParams,
    RegisterProjectParams,
    WithdrawDecisionParams,
)
from vogt.application.services import (
    answer_decision,
    ask_decision,
    create_work,
    list_audit,
    list_decisions,
    list_inbox,
    register_project,
    withdraw_decision,
)
from vogt.core.principal import Principal
from vogt.errors import Conflict, GrantRefused, InvalidRequest, PersonRequired

WHY = "decision test"
OPTIONS: list[dict[str, Any]] = [
    {"label": "Merge", "description": "land it now", "recommended": True},
    {"label": "Hold", "description": "wait for the review"},
]


def as_agent(ctx: AppContext, identity_ref: str) -> AppContext:
    return dataclasses.replace(
        ctx,
        principal=Principal(
            identity_ref=identity_ref, kind="agent", display_name=identity_ref
        ),
    )


def ask(ctx: AppContext, **extra: Any) -> str:
    fields: dict[str, Any] = {
        "question": "Merge the branch?",
        "options": OPTIONS,
        "context": "CI is green.",
        "reason": "the branch is reviewed",
        **extra,
    }
    return ask_decision(ctx, AskDecisionParams(**fields)).decision.id


def answer(ctx: AppContext, decision_id: str, label: str = "Merge") -> str:
    params = AnswerDecisionParams(id=decision_id, label=label, note="ok", reason=WHY)
    return answer_decision(ctx, params).decision.state


SESSION = "agent:engine:eng-overseer"
POD = "agent:pod:vogt"


def test_an_agent_asks_and_its_session_comes_from_the_token(
    instance: AppContext,
) -> None:
    decision = ask_decision(
        as_agent(instance, SESSION),
        AskDecisionParams.model_validate(
            {"question": "Merge the branch?", "options": OPTIONS, "reason": "reviewed"}
        ),
    ).decision
    assert decision.id.startswith("dec")
    assert decision.state == "pending"
    assert decision.asker == SESSION
    assert decision.asker_session == "eng-overseer"
    assert decision.expires_at - decision.created_at == timedelta(hours=24)
    assert decision.answer is None


def test_no_agent_answers_only_a_person(instance: AppContext) -> None:
    decision_id = ask(as_agent(instance, SESSION))
    for ref in (SESSION, "agent:session:ses_x", POD):
        with pytest.raises(PersonRequired, match="only a person answers"):
            answer(as_agent(instance, ref), decision_id)
    (row,) = list_decisions(instance, ListDecisionsParams()).decisions
    assert row.state == "pending"
    assert answer(instance, decision_id) == "answered"
    (row,) = list_decisions(instance, ListDecisionsParams(state="answered")).decisions
    assert row.answer is not None and row.answer.label == "Merge"
    assert row.answer.note == "ok"
    assert row.answered_by == instance.principal.identity_ref


def test_an_answer_must_be_an_offered_option(instance: AppContext) -> None:
    decision_id = ask(instance)
    with pytest.raises(InvalidRequest, match="not an option"):
        answer(instance, decision_id, label="Deploy")
    assert answer(instance, decision_id, label="Hold") == "answered"
    with pytest.raises(Conflict, match="answered, not pending"):
        answer(instance, decision_id)


def test_options_are_two_to_four_with_exactly_one_recommended(
    instance: AppContext,
) -> None:
    with pytest.raises(ValidationError):
        ask(instance, options=OPTIONS[:1])
    with pytest.raises(ValidationError):
        ask(instance, options=[*OPTIONS, *[{"label": f"o{i}"} for i in range(3)]])
    with pytest.raises(ValidationError):
        ask(instance, options=[{"label": "x" * 61, "recommended": True}, OPTIONS[1]])
    with pytest.raises(ValidationError):
        ask(instance, question="q" * 2001)
    with pytest.raises(ValidationError):
        ask(instance, ttl_seconds=8 * 86_400)
    with pytest.raises(InvalidRequest, match="exactly one"):
        ask(instance, options=[{"label": "a"}, {"label": "b"}])
    with pytest.raises(InvalidRequest, match="exactly one"):
        ask(instance, options=[{**o, "recommended": True} for o in OPTIONS])
    with pytest.raises(InvalidRequest, match="differ"):
        ask(instance, options=[OPTIONS[0], {"label": "Merge"}])


def test_a_pending_decision_expires_on_read_and_cannot_be_answered(
    instance: AppContext,
) -> None:
    decision_id = ask(instance, ttl_seconds=60)
    later = instance.clock() + timedelta(minutes=2)
    after = dataclasses.replace(instance, clock=lambda: later)
    (row,) = list_decisions(after, ListDecisionsParams(state="expired")).decisions
    assert row.id == decision_id and row.answer is None
    assert list_decisions(after, ListDecisionsParams(state="pending")).decisions == []
    with pytest.raises(Conflict, match="expired"):
        answer(after, decision_id)
    assert not [
        e
        for e in list_inbox(after, InboxListParams()).entries
        if e.kind == "decision.pending"
    ]


def test_the_asker_or_a_person_withdraws(instance: AppContext) -> None:
    asker = as_agent(instance, SESSION)
    first, second = ask(asker), ask(asker)
    with pytest.raises(GrantRefused, match="only a person, or the session"):
        withdraw_decision(
            as_agent(instance, "agent:engine:eng-worker"),
            WithdrawDecisionParams(id=first, reason=WHY),
        )
    params = WithdrawDecisionParams(id=first, reason=WHY)
    assert withdraw_decision(asker, params).decision.state == "withdrawn"
    with pytest.raises(Conflict):
        withdraw_decision(asker, params)
    with pytest.raises(Conflict, match="withdrawn"):
        answer(instance, first)
    params = WithdrawDecisionParams(id=second, reason=WHY)
    assert withdraw_decision(instance, params).decision.state == "withdrawn"


def test_list_filters_by_work_item_and_asker(
    instance: AppContext, tmp_path: Path
) -> None:
    root = tmp_path / "proj"
    root.mkdir()
    register_project(
        instance, RegisterProjectParams(name="Proj", root_path=str(root), reason=WHY)
    )
    ref = create_work(
        instance,
        CreateWorkParams(
            kind="chore", title="t", project="proj", local_only=True, reason=WHY
        ),
    ).item.ref
    on_item = ask(as_agent(instance, SESSION), work_item=ref)
    ask(instance)
    by_item = list_decisions(instance, ListDecisionsParams(work_item=ref)).decisions
    assert [d.id for d in by_item] == [on_item]
    assert by_item[0].work_item == ref
    by_asker = list_decisions(instance, ListDecisionsParams(asker=SESSION)).decisions
    assert [d.id for d in by_asker] == [on_item]
    assert list_decisions(instance, ListDecisionsParams(asker="nobody")).decisions == []
    (entry,) = [
        e
        for e in list_inbox(instance, InboxListParams()).entries
        if e.kind == "decision.pending" and e.source_subject_key == on_item
    ]
    assert entry.work_item_ref == ref and entry.project_slug == "proj"


def test_a_pending_decision_is_an_inbox_entry_until_answered(
    instance: AppContext,
) -> None:
    decision_id = ask(as_agent(instance, SESSION))
    (entry,) = [
        e
        for e in list_inbox(instance, InboxListParams()).entries
        if e.kind == "decision.pending"
    ]
    assert entry.action is not None and entry.action.kind == "decision"
    assert entry.action.decision_id == decision_id
    assert entry.session_id == "eng-overseer"
    assert "Merge the branch?" in entry.title
    assert "Merge (recommended)" in entry.summary and "Hold" in entry.summary
    assert entry.actor_login == SESSION and entry.actor_kind == "bot"
    snapshot = entry.evidence_snapshot
    assert snapshot is not None
    assert snapshot["recommended"] == "Merge"
    assert [o["label"] for o in snapshot["options"]] == ["Merge", "Hold"]  # type: ignore[attr-defined]
    assert snapshot["context"] == "CI is green."
    answer(instance, decision_id)
    assert not [
        e
        for e in list_inbox(instance, InboxListParams()).entries
        if e.kind == "decision.pending"
    ]


def test_every_step_is_audited(instance: AppContext) -> None:
    decision_id = ask(instance)
    answer(instance, decision_id)
    other = ask(instance)
    withdraw_decision(instance, WithdrawDecisionParams(id=other, reason=WHY))
    operations = [
        row.operation
        for row in list_audit(instance, ListAuditParams(limit=50)).records
        if row.entity_kind == "decision"
    ]
    assert sorted(operations) == sorted(
        ["decision.ask", "decision.answer", "decision.ask", "decision.withdraw"]
    )
