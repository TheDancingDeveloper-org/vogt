"""The audited write path (FR-S1, FR-S2, FR-N1, NFR-I1)."""

from __future__ import annotations

import pytest

from vogt.application.context import AppContext, build_context
from vogt.application.models import (
    InitParams,
    ListEventsParams,
    RegisterProjectParams,
)
from vogt.application.services import (
    init_instance,
    list_events,
    register_project,
)
from vogt.application.writes import WriteOutcome, audited_write, validate_reason
from vogt.core.entities import Actor
from vogt.core.ids import new_id
from vogt.core.principal import Principal
from vogt.errors import Conflict, InvalidRequest, MissingReason
from vogt.storage.interface import WriteTxn

from tests.conftest import StepClock


def _register(context: AppContext, name: str = "Example") -> str:
    result = register_project(
        context,
        RegisterProjectParams(
            name=name, root_path="/srv/example", reason="registering for a test"
        ),
    )
    return result.project.id


def test_a_write_lands_entity_audit_and_event_together(instance: AppContext) -> None:
    project_id = _register(instance)

    with instance.declared.read() as view:
        counts = view.counts()
        record = view.list_audit(limit=1)[0]
        events = view.list_events(after=0, limit=10)

    assert counts.projects == 1
    assert record.operation == "project.register"
    assert record.entity_id == project_id
    assert record.reason == "registering for a test"
    assert record.actor_identity_ref == "local:test-user"
    assert record.revision == 1

    assert len(events) == 1
    event = events[0]
    assert event.seq == 1
    assert event.kind == "project.registered"
    assert event.audit_id == record.id
    assert event.summary == {"slug": "example", "name": "Example"}


def test_the_event_and_the_audit_row_share_the_transaction(
    instance: AppContext,
) -> None:
    """A write is never visible without its event, nor an event without it."""
    with pytest.raises(RuntimeError, match="body failed"):

        def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[None]:
            del txn, actor
            msg = "body failed"
            raise RuntimeError(msg)

        audited_write(instance, operation="test.op", reason="a reason", body=body)

    with instance.declared.read() as view:
        assert view.counts().events == 0
        assert view.counts().audit == 1  # the bootstrap row only


@pytest.mark.parametrize("reason", ["", "   ", "\n\t "])
def test_a_blank_reason_is_refused(instance: AppContext, reason: str) -> None:
    with pytest.raises(MissingReason):
        validate_reason(reason)


def test_the_model_refuses_a_blank_reason_before_the_service_does() -> None:
    """Both layers check: one gives a good message, one guarantees the rule."""
    with pytest.raises(ValueError, match="reason"):
        RegisterProjectParams(name="X", root_path="/srv/x", reason="  ")


def test_a_reason_is_stored_stripped(instance: AppContext) -> None:
    register_project(
        instance,
        RegisterProjectParams(
            name="Spaced", root_path="/srv/spaced", reason="  padded reason  "
        ),
    )
    with instance.declared.read() as view:
        assert view.list_audit(limit=1)[0].reason == "padded reason"


def test_an_unseen_principal_is_auto_registered_and_explained(
    instance: AppContext,
) -> None:
    """A second principal appears in the audit trail with its provenance."""
    agent_context = build_context(
        config=instance.config,
        principal=Principal(
            identity_ref="agent:claude-code", kind="agent", display_name="Claude Code"
        ),
        clock=StepClock(),
        # Real ids here: this context shares a database with `instance`, whose
        # sequential ids would collide.
        id_factory=new_id,
    )
    _register(agent_context, name="Agent Project")

    with instance.declared.read() as view:
        actor = view.actor_by_identity("agent:claude-code")
        assert actor is not None
        assert actor.kind == "agent"
        registrations = view.list_audit(limit=10, operation="actor.auto_register")
        assert len(registrations) == 1
        assert "first authenticated use" in registrations[0].reason
        kinds = [event.kind for event in view.list_events(after=0, limit=10)]
        assert kinds == ["actor.auto_register", "project.registered"]


def test_registering_a_duplicate_slug_is_a_conflict(instance: AppContext) -> None:
    _register(instance, name="Same Name")
    with pytest.raises(Conflict, match="already registered"):
        _register(instance, name="same-name")

    with instance.declared.read() as view:
        assert view.counts().projects == 1
        assert view.current_revision() == 1, "a refused write must not burn a revision"


def test_a_name_with_no_slug_is_refused(instance: AppContext) -> None:
    with pytest.raises(InvalidRequest, match="slug"):
        register_project(
            instance,
            RegisterProjectParams(name="///", root_path="/srv/x", reason="try it"),
        )


def test_the_event_cursor_advances_and_never_rewinds(instance: AppContext) -> None:
    _register(instance, name="First")
    _register(instance, name="Second")

    page = list_events(instance, ListEventsParams(after=0, limit=1))
    assert [event.seq for event in page.events] == [1]
    assert page.next_cursor == 1

    page = list_events(instance, ListEventsParams(after=page.next_cursor, limit=10))
    assert [event.seq for event in page.events] == [2]

    empty = list_events(instance, ListEventsParams(after=page.next_cursor, limit=10))
    assert empty.events == []
    assert empty.next_cursor == page.next_cursor


def test_init_is_idempotent(context: AppContext) -> None:
    first = init_instance(context, InitParams())
    second = init_instance(context, InitParams())

    assert first.created is True
    assert second.created is False
    assert second.instance_id == first.instance_id
    assert second.migrations_applied == []


def test_both_stores_carry_the_same_instance_id(instance: AppContext) -> None:
    """Backups are per-store; a mismatched pair must be detectable (FR-L2)."""
    with instance.declared.read() as view:
        declared_id = view.instance_id()
    observed_id = instance.observed.instance_id()
    assert observed_id == declared_id
