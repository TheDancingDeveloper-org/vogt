"""State machines, and rejections that name their rule (FR-W2, FR-W8, FR-P4)."""

from __future__ import annotations

import pytest

from vogt.application.context import AppContext
from vogt.application.models import (
    CreateWorkParams,
    RegisterProjectParams,
    RelateWorkParams,
    TransitionProjectParams,
    TransitionWorkParams,
)
from vogt.application.services import (
    create_work,
    register_project,
    relate_work,
    transition_project,
    transition_work,
)
from vogt.core.workflow import (
    TransitionRejected,
    Workflow,
    check_lifecycle_transition,
    default_workflow,
)

WHY = "a test said so"


def _bug(ctx: AppContext, title: str = "A bug") -> str:
    return create_work(
        ctx, CreateWorkParams(kind="bug", title=title, reason=WHY)
    ).item.ref


# -- the machine itself ----------------------------------------------------


def test_the_default_machine_has_the_documented_shape() -> None:
    workflow = default_workflow("bug")
    assert workflow.initial_state == "open"
    assert set(workflow.states) == {
        "open",
        "in_progress",
        "review",
        "done",
        "blocked",
        "wont_do",
    }


def test_a_missing_edge_names_the_rule_and_the_alternatives() -> None:
    workflow = default_workflow("bug")
    with pytest.raises(TransitionRejected) as caught:
        workflow.check(from_state="open", to_state="done")
    assert caught.value.rule == "transition.not_allowed"
    assert "open -> done" in str(caught.value)
    assert "in_progress" in str(caught.value), "the message must say what is allowed"


def test_an_unknown_state_is_distinguished_from_a_missing_edge() -> None:
    with pytest.raises(TransitionRejected) as caught:
        default_workflow("bug").check(from_state="open", to_state="nonsense")
    assert caught.value.rule == "transition.unknown_state"


def test_transitioning_to_the_current_state_is_a_no_op_rule() -> None:
    with pytest.raises(TransitionRejected) as caught:
        default_workflow("bug").check(from_state="open", to_state="open")
    assert caught.value.rule == "transition.no_op"


def test_a_definition_round_trips() -> None:
    original = default_workflow("chore")
    restored = Workflow.from_definition("chore", original.to_definition())
    assert restored == original


def test_a_definition_without_transitions_is_refused() -> None:
    with pytest.raises(ValueError, match="no transitions map"):
        Workflow.from_definition("bug", {"initial_state": "open"})


# -- work transitions through the services --------------------------------


def test_work_starts_in_the_initial_state(instance: AppContext) -> None:
    result = create_work(
        instance, CreateWorkParams(kind="feature", title="New thing", reason=WHY)
    )
    assert result.item.state == "open"
    assert result.item.ref == "WI-1"


def test_a_rejected_transition_names_the_rule(instance: AppContext) -> None:
    ref = _bug(instance)
    with pytest.raises(TransitionRejected) as caught:
        transition_work(
            instance, TransitionWorkParams(ref=ref, to_state="done", reason=WHY)
        )
    assert caught.value.rule == "transition.not_allowed"


def test_a_rejected_transition_writes_nothing(instance: AppContext) -> None:
    ref = _bug(instance)
    with instance.declared.read() as view:
        before = view.current_revision()
    with pytest.raises(TransitionRejected):
        transition_work(
            instance, TransitionWorkParams(ref=ref, to_state="done", reason=WHY)
        )
    with instance.declared.read() as view:
        assert view.current_revision() == before
        assert view.work_item_by_ref(ref) is not None
        assert view.work_item_by_ref(ref).state == "open"  # type: ignore[union-attr]


def test_the_happy_path_reaches_done(instance: AppContext) -> None:
    ref = _bug(instance)
    for state in ("in_progress", "review", "done"):
        result = transition_work(
            instance, TransitionWorkParams(ref=ref, to_state=state, reason=WHY)
        )
        assert result.item.state == state


def test_depends_on_blocks_completion_and_lists_the_blockers(
    instance: AppContext,
) -> None:
    """FR-W8: the edge the caller declared is what refuses, and it says which."""
    blocked = _bug(instance, "Needs the other thing")
    blocker = _bug(instance, "The other thing")
    relate_work(
        instance,
        RelateWorkParams(ref=blocked, kind="depends_on", target=blocker, reason=WHY),
    )
    for state in ("in_progress", "review"):
        transition_work(
            instance, TransitionWorkParams(ref=blocked, to_state=state, reason=WHY)
        )

    with pytest.raises(TransitionRejected) as caught:
        transition_work(
            instance, TransitionWorkParams(ref=blocked, to_state="done", reason=WHY)
        )
    assert caught.value.rule == "transition.blocked_by_dependency"
    assert blocker in str(caught.value)


def test_completion_is_allowed_once_the_blocker_finishes(
    instance: AppContext,
) -> None:
    blocked = _bug(instance, "Second")
    blocker = _bug(instance, "First")
    relate_work(
        instance,
        RelateWorkParams(ref=blocked, kind="depends_on", target=blocker, reason=WHY),
    )
    for state in ("in_progress", "review", "done"):
        transition_work(
            instance, TransitionWorkParams(ref=blocker, to_state=state, reason=WHY)
        )
    for state in ("in_progress", "review", "done"):
        transition_work(
            instance, TransitionWorkParams(ref=blocked, to_state=state, reason=WHY)
        )
    with instance.declared.read() as view:
        item = view.work_item_by_ref(blocked)
        assert item is not None
        assert item.state == "done"


def test_a_wont_do_blocker_does_not_block(instance: AppContext) -> None:
    """`wont_do` is finished too; treating it as pending would deadlock work."""
    blocked = _bug(instance, "Depends on something abandoned")
    blocker = _bug(instance, "Abandoned")
    relate_work(
        instance,
        RelateWorkParams(ref=blocked, kind="depends_on", target=blocker, reason=WHY),
    )
    transition_work(
        instance, TransitionWorkParams(ref=blocker, to_state="wont_do", reason=WHY)
    )
    for state in ("in_progress", "review", "done"):
        transition_work(
            instance, TransitionWorkParams(ref=blocked, to_state=state, reason=WHY)
        )


def test_other_relation_kinds_do_not_block(instance: AppContext) -> None:
    blocked = _bug(instance, "Mentions another")
    other = _bug(instance, "The other")
    relate_work(
        instance,
        RelateWorkParams(ref=blocked, kind="relates_to", target=other, reason=WHY),
    )
    for state in ("in_progress", "review", "done"):
        transition_work(
            instance, TransitionWorkParams(ref=blocked, to_state=state, reason=WHY)
        )


# -- project lifecycle -----------------------------------------------------


def test_lifecycle_edges_are_validated() -> None:
    check_lifecycle_transition(from_state="incubating", to_state="active")
    with pytest.raises(TransitionRejected) as caught:
        check_lifecycle_transition(from_state="incubating", to_state="maintenance")
    assert caught.value.rule == "lifecycle.not_allowed"


def test_an_archived_project_can_come_back() -> None:
    check_lifecycle_transition(from_state="archived", to_state="active")


def test_project_transition_is_audited(instance: AppContext) -> None:
    register_project(
        instance,
        RegisterProjectParams(name="Lifecycle", root_path="/srv/lc", reason=WHY),
    )
    result = transition_project(
        instance,
        TransitionProjectParams(slug="lifecycle", to_state="maintenance", reason=WHY),
    )
    assert result.project.lifecycle_state == "maintenance"
    with instance.declared.read() as view:
        record = view.list_audit(limit=1)[0]
        assert record.operation == "project.transition"
        events = view.list_events(after=0, limit=10)
        assert events[-1].kind == "project.transitioned"
        assert events[-1].summary["to"] == "maintenance"
