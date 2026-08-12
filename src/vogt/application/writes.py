"""The transactional write path (NFR-I1, FR-S1, FR-N1).

One function, used by every declared write there will ever be. It opens the
transaction, resolves the acting actor, runs the caller's body, and lands the
audit row and the event row inside the same transaction as the entity change.
A write that skips this path is a bug the parity and audit tests are designed
to catch.
"""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass, field
from typing import Any, Generic, TypeVar

from vogt.application.context import AppContext
from vogt.core.digest import digest_of
from vogt.core.entities import Actor
from vogt.errors import MissingReason
from vogt.storage.interface import WriteTxn

T = TypeVar("T")

#: Emitted when a principal is seen for the first time and auto-registered.
ACTOR_AUTO_REGISTER = "actor.auto_register"


@dataclass(frozen=True)
class WriteOutcome(Generic[T]):
    """What a write produced, and what the audit and event rows should say."""

    result: T
    entity_kind: str
    entity_id: str
    payload: dict[str, Any]
    event_kind: str
    summary: dict[str, object] = field(default_factory=dict)


def validate_reason(reason: str) -> str:
    """Reject a blank reason before it reaches the store.

    The database has a CHECK for this too. Both exist on purpose: the CHECK
    guarantees no blank reason can ever be stored by any code path, and this
    guarantees the caller gets a useful error instead of an integrity one.
    """
    cleaned = reason.strip()
    if not cleaned:
        msg = "a non-empty reason is required on every write"
        raise MissingReason(msg)
    return cleaned


def ensure_actor(ctx: AppContext, txn: WriteTxn) -> Actor:
    """Resolve the acting principal to an Actor row, creating it if new.

    Auto-registration is itself a declared write: it lands its own audit row
    and its own event, inside the caller's transaction, so an actor never
    appears in the audit trail without an explanation of where it came from.
    """
    existing = txn.actor_by_identity(ctx.principal.identity_ref)
    if existing is not None:
        return existing
    now = ctx.clock()
    actor = Actor(
        id=ctx.id_factory("act"),
        kind=ctx.principal.kind,
        display_name=ctx.principal.display_name,
        identity_ref=ctx.principal.identity_ref,
        disabled=False,
        created_at=now,
    )
    txn.insert_actor(actor)
    record = txn.append_audit(
        actor=actor,
        operation=ACTOR_AUTO_REGISTER,
        entity_kind="actor",
        entity_id=actor.id,
        reason=(
            f"first authenticated use by {actor.identity_ref}; "
            "auto-registered so the write it accompanies can be attributed"
        ),
        payload_digest=digest_of(actor.model_dump(mode="json")),
        at=now,
    )
    txn.append_event(
        kind=ACTOR_AUTO_REGISTER,
        entity_kind="actor",
        entity_id=actor.id,
        actor_id=actor.id,
        audit_id=record.id,
        summary={"identity_ref": actor.identity_ref, "kind": actor.kind},
        at=now,
    )
    return actor


def audited_write(
    ctx: AppContext,
    *,
    operation: str,
    reason: str,
    body: Callable[[WriteTxn, Actor], WriteOutcome[T]],
) -> T:
    """Run one declared write atomically, audited and evented."""
    cleaned_reason = validate_reason(reason)
    with ctx.declared.write() as txn:
        actor = ensure_actor(ctx, txn)
        outcome = body(txn, actor)
        now = ctx.clock()
        record = txn.append_audit(
            actor=actor,
            operation=operation,
            entity_kind=outcome.entity_kind,
            entity_id=outcome.entity_id,
            reason=cleaned_reason,
            payload_digest=digest_of(outcome.payload),
            at=now,
        )
        txn.append_event(
            kind=outcome.event_kind,
            entity_kind=outcome.entity_kind,
            entity_id=outcome.entity_id,
            actor_id=actor.id,
            audit_id=record.id,
            summary=outcome.summary,
            at=now,
        )
        return outcome.result
