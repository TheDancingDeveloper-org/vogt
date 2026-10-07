"""Person-approved grants to a live session (WI-973).

A session asks — usually an overseer, for the worker it drives — for one
scoped item for one target session; a person approves or denies it from the
Inbox; the engine applies an approved credential grant to that live session,
which fetches it through the secret broker it already has. The design, and the
invariants each check here enforces, are `docs/design/oversight-grants.md`.

Three rules shape this module:

- **Only a person decides.** `decide_grant` refuses every agent principal —
  session tokens, `agent:engine:` tokens, the brokered pod token — before
  anything else. The engine half of the same rule is that it accepts a grant
  only from the core's own credential.
- **The engine applies before the row says approved.** The engine and SQLite
  cannot share a transaction, so the approval is sent first and recorded only
  once the engine has said yes. A refusal leaves the request pending and says
  why; the person can deny it instead.
- **No value ever comes here.** The core records names; the engine fetches the
  value at the moment the session asks for it.
"""

from __future__ import annotations

import re
from datetime import datetime, timedelta

from vogt.application import writes
from vogt.application.context import AppContext
from vogt.application.models import (
    DecideGrantParams,
    ListGrantsParams,
    ListGrantsResult,
    RequestGrantParams,
    RevokeGrantParams,
    SessionGrantResult,
    SessionGrantView,
)
from vogt.application.services.sessions import _engine, _target
from vogt.application.writes import WriteOutcome, audited_write
from vogt.core.entities import Actor, SessionGrant
from vogt.errors import Conflict, GrantRefused, InvalidRequest, NotFound
from vogt.storage.interface import ReadView, WriteTxn

SESSION_GRANT_REQUEST = "session.grant_request"
SESSION_GRANT_DECIDE = "session.grant_decide"
SESSION_GRANT_REVOKE = "session.grant_revoke"
SESSION_GRANT_REQUESTED_EVENT = "session.grant_requested"
SESSION_GRANT_DECIDED_EVENT = "session.grant_decided"
SESSION_GRANT_REVOKED_EVENT = "session.grant_revoked"

_PLAIN = re.compile(r"^[A-Za-z0-9._/-]{1,256}$")
_ENV_NAME = re.compile(r"^[A-Za-z_][A-Za-z0-9_]{0,127}$")


def default_var(secret_name: str) -> str:
    """`GRANT_<secret>` as an environment-variable name: `100.109.218.11_SSH`
    becomes `GRANT_100_109_218_11_SSH`."""
    return "GRANT_" + re.sub(r"[^A-Za-z0-9_]", "_", secret_name).upper()


def request_grant(ctx: AppContext, params: RequestGrantParams) -> SessionGrantResult:
    """Ask a person to approve one scoped item for one live session."""
    reason = writes.validate_reason(params.reason)
    if params.kind != "credential":
        msg = (
            "capability grants are not available yet (WI-973 milestone 2); "
            "ask for a named credential, or report the denied action as blocked"
        )
        raise InvalidRequest(msg)
    secret = (params.secret_name or "").strip()
    project = (params.project_id or "").strip()
    if not secret or not project:
        msg = "a credential grant names secret_name and project_id"
        raise InvalidRequest(msg)
    if (
        secret.startswith("-")
        or project.startswith("-")
        or not (_PLAIN.match(secret) and _PLAIN.match(project))
    ):
        msg = (
            "secret_name and project_id must be plain names (letters, digits, . _ - /)"
        )
        raise InvalidRequest(msg)
    var = (params.var or "").strip() or default_var(secret)
    if not _ENV_NAME.match(var):
        msg = f"var {var!r} is not a valid environment variable name"
        raise InvalidRequest(msg)

    engine = _engine(ctx)
    target = _target(ctx, params.target)
    live = engine.get_session(target.engine_session_id)
    if live is None or not live.alive:
        msg = f"session {params.target!r} is not running; a grant is for a live session"
        raise Conflict(msg)
    _check_requester(ctx, target.engine_session_id)

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[SessionGrantResult]:
        grant = SessionGrant(
            id=ctx.id_factory("grt"),
            target_engine_session_id=target.engine_session_id,
            kind="credential",
            var=var,
            project_id=project,
            secret_name=secret,
            uses=params.uses,
            ttl_seconds=params.ttl_seconds,
            reason=reason,
            requested_by=actor.id,
            requested_at=ctx.clock(),
        )
        txn.insert_session_grant(grant)
        return WriteOutcome(
            result=SessionGrantResult(grant=_view(txn, grant, ctx)),
            entity_kind="session_grant",
            entity_id=grant.id,
            payload=_payload(grant),
            event_kind=SESSION_GRANT_REQUESTED_EVENT,
            summary=_summary(grant),
        )

    return audited_write(ctx, operation=SESSION_GRANT_REQUEST, reason=reason, body=body)


def decide_grant(ctx: AppContext, params: DecideGrantParams) -> SessionGrantResult:
    """Approve or deny a pending grant. A person's decision, never an agent's."""
    reason = writes.validate_reason(params.reason)
    if ctx.principal.kind == "agent":
        msg = (
            "only a person decides a grant, not an agent "
            f"({ctx.principal.identity_ref}); approve or deny it from the Inbox"
        )
        raise GrantRefused(msg)
    grant = _load(ctx, params.id)
    if grant.state != "pending":
        msg = f"grant {grant.id} is {grant.state}, not pending"
        raise Conflict(msg)

    if params.decision == "deny":
        return _record_decision(ctx, grant, reason, approved=False)

    decided_at = ctx.clock()
    expires_at = decided_at + timedelta(seconds=grant.ttl_seconds)
    engine = _engine(ctx)
    # The engine first: the row says approved only once the grant is held.
    engine.apply_grant(
        grant.target_engine_session_id,
        {
            "grant_id": grant.id,
            "var": grant.var,
            "project_id": grant.project_id,
            "secret_name": grant.secret_name,
            "uses": grant.uses,
            "expires_at": expires_at.isoformat().replace("+00:00", "Z"),
            # The purpose the person is approving, so the session (and the
            # classifier reading `vogt-agent-auth grants`) sees it. Text, not
            # a value; the engine caps its length.
            "reason": grant.reason,
        },
    )
    try:
        return _record_decision(
            ctx,
            grant,
            reason,
            approved=True,
            decided_at=decided_at,
            expires_at=expires_at,
        )
    except BaseException:
        # The record did not say approved. Whatever the cause — someone else
        # decided it between the read and the write, or the write itself
        # failed — the engine must not hold a grant the record does not
        # stand behind. The one exception is another person's approval of
        # this same grant landing first: the engine holds their grant (the
        # same id replaced ours), and taking it back would revoke a decision
        # that was made.
        if not _approved_meanwhile(ctx, grant.id):
            engine.revoke_grant(grant.target_engine_session_id, grant.id)
        raise


def revoke_grant(ctx: AppContext, params: RevokeGrantParams) -> SessionGrantResult:
    """Withdraw a pending grant or revoke an approved one, at once.

    A person may revoke any grant; the actor that asked for one may give it up.
    Narrowing is always safe, so nothing more is required.
    """
    reason = writes.validate_reason(params.reason)
    grant = _load(ctx, params.id)
    if grant.state not in ("pending", "approved"):
        msg = f"grant {grant.id} is {grant.state}; there is nothing to revoke"
        raise Conflict(msg)
    if ctx.principal.kind == "agent":
        with ctx.declared.read() as view:
            asker = view.actor_by_identity(ctx.principal.identity_ref)
        if asker is None or asker.id != grant.requested_by:
            msg = (
                "only a person, or the session that asked for it, revokes a grant "
                f"({ctx.principal.identity_ref} did neither)"
            )
            raise GrantRefused(msg)
    if grant.state == "approved" and ctx.engine is not None:
        # Absence at the engine is the revoked state, so a grant the engine
        # already lost (restart, session gone) still records as revoked.
        ctx.engine.revoke_grant(grant.target_engine_session_id, grant.id)

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[SessionGrantResult]:
        current = txn.session_grant(grant.id)
        if current is None:
            raise NotFound(f"no grant {grant.id!r}")
        revoked = current.model_copy(
            update={
                "state": "revoked",
                "revoked_by": actor.id,
                "revoked_at": ctx.clock(),
            }
        )
        txn.update_session_grant(revoked)
        return WriteOutcome(
            result=SessionGrantResult(grant=_view(txn, revoked, ctx)),
            entity_kind="session_grant",
            entity_id=revoked.id,
            payload=_payload(revoked),
            event_kind=SESSION_GRANT_REVOKED_EVENT,
            summary=_summary(revoked),
        )

    return audited_write(ctx, operation=SESSION_GRANT_REVOKE, reason=reason, body=body)


def list_grants(ctx: AppContext, params: ListGrantsParams) -> ListGrantsResult:
    """Grants, newest first; `state=expired` and `pending` are what most ask.

    A person sees every grant. An agent sees its own session's: which secrets
    another session was granted, and until when, is not every agent's to read
    (names only, but names are a map). An agent that is not a session sees
    nothing.
    """
    target = (
        None if params.target is None else _target(ctx, params.target).engine_session_id
    )
    if ctx.principal.kind == "agent":
        own = _own_engine_session(ctx)
        if own is None or (target is not None and target != own):
            return ListGrantsResult(grants=[])
        target = own
    stored_state = "approved" if params.state == "expired" else params.state
    with ctx.declared.read() as view:
        rows = view.list_session_grants(
            state=stored_state, target_engine_session_id=target, limit=params.limit
        )
        views = [_view(view, row, ctx) for row in rows]
    if params.state in ("approved", "expired"):
        views = [v for v in views if v.state == params.state]
    return ListGrantsResult(grants=views)


def pending_grants(view: ReadView, ctx: AppContext) -> list[SessionGrantView]:
    """What the Inbox shows: every grant waiting for a person."""
    return [_view(view, row, ctx) for row in view.list_session_grants(state="pending")]


# -- helpers ---------------------------------------------------------------


def _check_requester(ctx: AppContext, target_engine_id: str) -> None:
    """An agent may ask for its own session; for another, only an overseer.

    Every grant still needs a person, so this is not what protects the
    boundary — it keeps the Inbox to overseers and their workers.
    """
    if ctx.principal.kind != "agent":
        return
    own = _own_engine_session(ctx)
    if own == target_engine_id:
        return
    if own is None:
        msg = (
            f"{ctx.principal.identity_ref} is not a session, so it can ask for a "
            "grant only through an oversight session"
        )
        raise GrantRefused(msg)
    asker = _engine(ctx).get_session(own)
    if asker is None or asker.role != "oversight":
        msg = (
            "only an oversight session asks for a grant on another session's "
            "behalf; ask your overseer, or ask for your own session"
        )
        raise GrantRefused(msg)


def _own_engine_session(ctx: AppContext) -> str | None:
    """The engine session the calling agent runs in, from its identity."""
    ref = ctx.principal.identity_ref
    if ref.startswith("agent:engine:"):
        return ref.removeprefix("agent:engine:")
    if ref.startswith("agent:session:"):
        with ctx.declared.read() as view:
            session = view.session_by_id(ref.removeprefix("agent:session:"))
        return None if session is None else session.engine_session_id
    return None


def _approved_meanwhile(ctx: AppContext, grant_id: str) -> bool:
    """Whether the grant is recorded approved now — by a decision that was
    not this one, when called from a failed approval."""
    with ctx.declared.read() as view:
        current = view.session_grant(grant_id)
    return current is not None and current.state == "approved"


def _load(ctx: AppContext, grant_id: str) -> SessionGrant:
    with ctx.declared.read() as view:
        grant = view.session_grant(grant_id.strip())
    if grant is None:
        msg = f"no grant {grant_id!r}"
        raise NotFound(msg)
    return grant


def _record_decision(
    ctx: AppContext,
    grant: SessionGrant,
    reason: str,
    *,
    approved: bool,
    decided_at: datetime | None = None,
    expires_at: datetime | None = None,
) -> SessionGrantResult:
    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[SessionGrantResult]:
        current = txn.session_grant(grant.id)
        if current is None or current.state != "pending":
            state = "gone" if current is None else current.state
            msg = f"grant {grant.id} is {state}, not pending"
            raise Conflict(msg)
        decided = current.model_copy(
            update={
                "state": "approved" if approved else "denied",
                "decided_by": actor.id,
                "decided_at": decided_at or ctx.clock(),
                "decision_reason": reason,
                "expires_at": expires_at if approved else None,
            }
        )
        txn.update_session_grant(decided)
        return WriteOutcome(
            result=SessionGrantResult(grant=_view(txn, decided, ctx)),
            entity_kind="session_grant",
            entity_id=decided.id,
            payload=_payload(decided),
            event_kind=SESSION_GRANT_DECIDED_EVENT,
            summary=_summary(decided),
        )

    return audited_write(ctx, operation=SESSION_GRANT_DECIDE, reason=reason, body=body)


def _ref(view: ReadView, actor_id: str | None) -> str | None:
    if actor_id is None:
        return None
    actor = view.actor_by_id(actor_id)
    return actor_id if actor is None else actor.identity_ref


def _view(view: ReadView, grant: SessionGrant, ctx: AppContext) -> SessionGrantView:
    return SessionGrantView(
        id=grant.id,
        target=grant.target_engine_session_id,
        kind=grant.kind,
        var=grant.var,
        project_id=grant.project_id,
        secret_name=grant.secret_name,
        capability=grant.capability,
        uses=grant.uses,
        ttl_seconds=grant.ttl_seconds,
        reason=grant.reason,
        requested_by=_ref(view, grant.requested_by) or grant.requested_by,
        requested_at=grant.requested_at,
        state=grant.effective_state(ctx.clock()),
        decided_by=_ref(view, grant.decided_by),
        decided_at=grant.decided_at,
        decision_reason=grant.decision_reason,
        expires_at=grant.expires_at,
        revoked_by=_ref(view, grant.revoked_by),
        revoked_at=grant.revoked_at,
    )


def _payload(grant: SessionGrant) -> dict[str, object]:
    return grant.model_dump(mode="json")


def _summary(grant: SessionGrant) -> dict[str, object]:
    """What an event says: names and the decision, never a value."""
    return {
        "grant_id": grant.id,
        "target": grant.target_engine_session_id,
        "kind": grant.kind,
        "var": grant.var,
        "project_id": grant.project_id,
        "secret_name": grant.secret_name,
        "uses": grant.uses,
        "state": grant.state,
    }
