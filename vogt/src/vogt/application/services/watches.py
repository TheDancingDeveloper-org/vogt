"""Watches: a session waits on an event instead of polling (WI-2036).

Workers and overseers spent whole model turns polling CI. A watch is the
alternative: arm it (`watch.create`, or `session.hibernate` with `wake_on`),
hibernate, and the core wakes the session with the outcome when the trigger
fires — a pull request merged or closed, CI concluded on a revision, a work
item reached a state, another session ended.

Three rules shape this module:

- **A watch is a row.** It lives in the declared store, so it survives an
  engine or core restart; the watch loop (`adapters/http/scheduler.py`)
  reads the active rows on every pass. Nothing is held in memory.
- **The engine does the polling, not the model.** `evaluate_watches` reads
  evidence Vogt already has — forge observations the sweep collected, the
  work item, the engine's session list — and decides with `core.watches`.
- **Waking reuses session input.** A fired watch is delivered by
  `session_input`, which wakes a hibernated session (never forced), waits
  until it is ready and types the one-line notice. A delivery that fails is
  retried on later passes, up to `MAX_ATTEMPTS`, then the watch reads
  `failed` with the error.
"""

from __future__ import annotations

import logging
from dataclasses import dataclass, field
from datetime import datetime, timedelta
from typing import Any

from vogt.adapters.engine import EngineUnavailable
from vogt.adapters.forge.kinds import KIND_CHECK, KIND_PULL_REQUEST
from vogt.application import writes
from vogt.application.context import AppContext
from vogt.application.models import (
    CancelWatchParams,
    CreateWatchParams,
    ListWatchesParams,
    ListWatchesResult,
    SessionInputParams,
    WatchResult,
    WatchTriggerModel,
    WatchView,
)
from vogt.application.services import _resolve
from vogt.application.services.grants import _own_engine_session
from vogt.application.writes import WriteOutcome, audited_write
from vogt.core import watches as rules
from vogt.core.entities import Actor, Observation, Watch
from vogt.core.workflow import TERMINAL_STATES
from vogt.errors import Conflict, GrantRefused, InvalidRequest, NotFound, VogtError
from vogt.storage.interface import ReadView, WriteTxn

logger = logging.getLogger(__name__)

WATCH_CREATE = "watch.create"
WATCH_CANCEL = "watch.cancel"
WATCH_FIRE = "watch.fire"
WATCH_CREATED_EVENT = "watch.created"
WATCH_CANCELLED_EVENT = "watch.cancelled"
WATCH_FIRED_EVENT = "watch.fired"
WATCH_FAILED_EVENT = "watch.failed"
WATCH_EXPIRED_EVENT = "watch.expired"

#: Deliveries tried per fired watch before it reads `failed`.
MAX_ATTEMPTS = 5
#: Active watches evaluated per pass; more wait for the next pass.
MAX_ACTIVE = 500
#: How long a delivery waits for a woken session to be ready.
#: Short, because the loop is serial: one unready session must not hold
#: every other watch for minutes.
WAKE_TIMEOUT_S = 30
#: The first retry waits this long; each later one doubles it (60 s, 2, 4,
#: 8 min), so a refused session is not re-tried — and does not block the
#: loop — on every pass.
RETRY_BASE_S = 60
FIRE_REASON = "watch fired: deliver its outcome (WI-2036)"


# -- arming, cancelling, listing --------------------------------------------


def create_watch(ctx: AppContext, params: CreateWatchParams) -> WatchResult:
    """Arm a watch that wakes a session when its trigger fires."""
    reason = writes.validate_reason(params.reason)
    if params.session is not None:
        target = _engine_id(ctx, params.session)
    else:
        own = _own_engine_session(ctx)
        if own is None:
            msg = "name the session to wake: the caller is not a session"
            raise InvalidRequest(msg)
        target = own
    watch = arm(
        ctx,
        params.trigger,
        target_session=target,
        note=params.note,
        ttl_seconds=params.ttl_seconds,
        reason=reason,
    )
    with ctx.declared.read() as view:
        return WatchResult(watch=_view(view, watch, ctx))


def arm(
    ctx: AppContext,
    trigger: WatchTriggerModel,
    *,
    target_session: str,
    note: str,
    ttl_seconds: int,
    reason: str,
) -> Watch:
    """Validate `trigger`, resolve what it names, and store the watch.

    Shared by `watch.create` and `session.hibernate`'s `wake_on`."""
    stored = _resolved_trigger(ctx, trigger, target_session)

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[Watch]:
        now = ctx.clock()
        watch = Watch(
            id=ctx.id_factory("wat"),
            owner_actor_id=actor.id,
            target_session=target_session,
            kind=trigger.kind,
            trigger=stored,
            note=note.strip(),
            created_at=now,
            expires_at=now + timedelta(seconds=ttl_seconds),
            reason=reason,
        )
        txn.insert_watch(watch)
        return _outcome(txn, watch, WATCH_CREATED_EVENT)

    return audited_write(ctx, operation=WATCH_CREATE, reason=reason, body=body)


def cancel_watch(ctx: AppContext, params: CancelWatchParams) -> WatchResult:
    """Cancel an active watch: a person, its owner, or the session it wakes."""
    reason = writes.validate_reason(params.reason)
    watch = _load(ctx, params.id)
    if ctx.principal.kind == "agent" and not _may_cancel(ctx, watch):
        msg = (
            "only a person, the watch's owner or the session it wakes cancels "
            f"a watch ({ctx.principal.identity_ref} is none of them)"
        )
        raise GrantRefused(msg)

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[Watch]:
        del actor
        current = txn.watch(watch.id)
        if current is None:
            msg = f"no watch {watch.id!r}"
            raise NotFound(msg)
        state = current.effective_state(ctx.clock())
        if state != "active":
            msg = f"watch {watch.id} is {state}, not active"
            raise Conflict(msg)
        cancelled = current.model_copy(update={"state": "cancelled"})
        txn.update_watch(cancelled)
        return _outcome(txn, cancelled, WATCH_CANCELLED_EVENT)

    cancelled = audited_write(ctx, operation=WATCH_CANCEL, reason=reason, body=body)
    with ctx.declared.read() as view:
        return WatchResult(watch=_view(view, cancelled, ctx))


def list_watches(ctx: AppContext, params: ListWatchesParams) -> ListWatchesResult:
    """Watches, newest first, by state or by the session they wake."""
    stored_state = "active" if params.state == "expired" else params.state
    target = None if params.session is None else _engine_id(ctx, params.session)
    with ctx.declared.read() as view:
        rows = view.list_watches(
            state=stored_state, target_session=target, limit=params.limit
        )
        if params.state == "expired":
            # An expired watch is stored `expired` once the loop has seen it,
            # and reads so beforehand.
            rows += view.list_watches(
                state="expired", target_session=target, limit=params.limit
            )
        views = [_view(view, row, ctx) for row in rows]
    if params.state is not None:
        views = [v for v in views if v.state == params.state]
    return ListWatchesResult(watches=views[: params.limit])


# -- the watch loop ----------------------------------------------------------


@dataclass
class WatchPass:
    """What one pass did, by watch id."""

    fired: list[str] = field(default_factory=list)
    retrying: list[str] = field(default_factory=list)
    failed: list[str] = field(default_factory=list)
    expired: list[str] = field(default_factory=list)


def evaluate_watches(ctx: AppContext) -> WatchPass:
    """Evaluate every active watch once; deliver the ones that fired.

    Never raises for a missing engine or a refused delivery: the watch
    records the error and is retried, so one bad session cannot stop the
    loop for every other watch."""
    done = WatchPass()
    now = ctx.clock()
    with ctx.declared.read() as view:
        active = view.list_watches(state="active", limit=MAX_ACTIVE)
    if not active:
        return done
    evidence = _Evidence(ctx)
    for watch in active:
        if watch.expires_at <= now:
            _settle(ctx, watch, state="expired", event=WATCH_EXPIRED_EVENT)
            done.expired.append(watch.id)
            continue
        if not _due(watch, now):
            continue
        try:
            outcome = watch.outcome or _evaluate(ctx, evidence, watch)
        except (VogtError, EngineUnavailable) as exc:
            logger.info("watch %s not evaluated: %s", watch.id, exc)
            continue
        if outcome is None:
            continue
        error = _deliver(ctx, watch, outcome)
        if error is None:
            _settle(
                ctx,
                watch,
                state="fired",
                event=WATCH_FIRED_EVENT,
                outcome={**outcome, "delivered": True},
                fired=True,
            )
            done.fired.append(watch.id)
        elif watch.attempts + 1 >= MAX_ATTEMPTS:
            _settle(
                ctx,
                watch,
                state="failed",
                event=WATCH_FAILED_EVENT,
                outcome={**outcome, "delivered": False},
                error=error,
                fired=True,
            )
            done.failed.append(watch.id)
        else:
            _settle(
                ctx,
                watch,
                state="active",
                event=WATCH_FAILED_EVENT,
                outcome={
                    **outcome,
                    "next_attempt_at": (
                        now + timedelta(seconds=RETRY_BASE_S * 2**watch.attempts)
                    ).isoformat(),
                },
                error=error,
            )
            done.retrying.append(watch.id)
    return done


def _due(watch: Watch, now: datetime) -> bool:
    """Whether a watch whose delivery failed has waited out its backoff."""
    stamp = None if watch.outcome is None else watch.outcome.get("next_attempt_at")
    if not isinstance(stamp, str):
        return True
    try:
        return datetime.fromisoformat(stamp) <= now
    except ValueError:
        return True


class _Evidence:
    """Observations read once per pass and per project, not once per watch."""

    def __init__(self, ctx: AppContext) -> None:
        self._ctx = ctx
        self._observed: dict[tuple[str, str], list[Observation]] = {}

    def observations(self, kind: str, project_id: str) -> list[Observation]:
        key = (kind, project_id)
        if key not in self._observed:
            self._observed[key] = self._ctx.observed.latest(
                kinds=(kind,), project_id=project_id, limit=2000
            )
        return self._observed[key]

    def pull(self, project_id: str, number: int) -> Observation | None:
        return next(
            (
                obs
                for obs in self.observations(KIND_PULL_REQUEST, project_id)
                if obs.payload.get("number") == number
            ),
            None,
        )


def _evaluate(
    ctx: AppContext, evidence: _Evidence, watch: Watch
) -> dict[str, Any] | None:
    trigger = watch.trigger
    if watch.kind == rules.KIND_PR:
        pull = evidence.pull(trigger["project_id"], int(trigger["number"]))
        if pull is None:
            return None
        return rules.pr_outcome(pull.payload, trigger["on"], url=pull.source_url)
    if watch.kind == rules.KIND_CI:
        revision = trigger.get("sha")
        if not revision:
            pull = evidence.pull(trigger["project_id"], int(trigger["pr"]))
            head = None if pull is None else pull.payload.get("head")
            if not isinstance(head, str) or not head:
                return None
            revision = head
        return rules.ci_outcome(
            evidence.observations(KIND_CHECK, trigger["project_id"]),
            str(revision),
            trigger.get("checks") or (),
        )
    if watch.kind == rules.KIND_WORK_ITEM:
        with ctx.declared.read() as view:
            item = view.work_item_by_id(trigger["work_item_id"])
        if item is None:
            return {"state": "gone"}
        return rules.work_item_outcome(item.state, trigger["states"])
    if watch.kind == rules.KIND_SESSION:
        if ctx.engine is None:
            return None
        seen = ctx.engine.get_session(trigger["engine_session_id"])
        return rules.session_outcome(
            present=seen is not None,
            activity=None if seen is None else seen.activity,
            alive=False if seen is None else seen.alive,
            done_at=None if seen is None else seen.done_at,
            until=trigger["until"],
        )
    return None


def _deliver(ctx: AppContext, watch: Watch, outcome: dict[str, Any]) -> str | None:
    """Type the notice into the watch's session, waking it first. The error
    text when it could not be delivered, else `None`."""
    # Imported here: the services package imports this module.
    from vogt.application.services.sessions import session_input

    if ctx.engine is None:
        return "no engine is configured"
    text = rules.notice_text(watch.kind, watch.trigger, outcome, watch.note)
    try:
        session_input(
            ctx,
            SessionInputParams(
                id=watch.target_session,
                text=text,
                wake_timeout_s=WAKE_TIMEOUT_S,
                reason=f"{FIRE_REASON}: {watch.id}",
            ),
        )
    except (VogtError, EngineUnavailable) as exc:
        logger.info("watch %s not delivered: %s", watch.id, exc)
        return str(exc)[:500] or type(exc).__name__
    return None


def _settle(
    ctx: AppContext,
    watch: Watch,
    *,
    state: str,
    event: str,
    outcome: dict[str, Any] | None = None,
    error: str | None = None,
    fired: bool = False,
) -> None:
    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[Watch]:
        del actor
        current = txn.watch(watch.id) or watch
        if current.state != "active":
            # Cancelled (or settled by another pass) meanwhile: keep that.
            return _outcome(txn, current, event)
        updated = current.model_copy(
            update={
                "state": state,
                "outcome": outcome if outcome is not None else current.outcome,
                "attempts": current.attempts + (1 if outcome is not None else 0),
                "last_error": error,
                "fired_at": ctx.clock() if fired else current.fired_at,
            }
        )
        txn.update_watch(updated)
        return _outcome(txn, updated, event)

    audited_write(ctx, operation=WATCH_FIRE, reason=FIRE_REASON, body=body)


# -- helpers ---------------------------------------------------------------


def _resolved_trigger(
    ctx: AppContext, trigger: WatchTriggerModel, target_session: str
) -> dict[str, Any]:
    """The trigger as stored: what it names resolved to ids, so the loop
    never re-resolves a slug or ref, and unused fields dropped."""
    kind = trigger.kind
    stored: dict[str, Any] = {"kind": kind}
    with ctx.declared.read() as view:
        if kind in (rules.KIND_PR, rules.KIND_CI):
            assert trigger.project is not None  # the model validated it
            project = view.project_by_slug(trigger.project)
            if project is None:
                msg = f"no project {trigger.project!r}"
                raise NotFound(msg)
            stored |= {"project": project.slug, "project_id": project.id}
        if kind == rules.KIND_PR:
            stored |= {"number": trigger.number, "on": sorted(set(trigger.on))}
        elif kind == rules.KIND_CI:
            stored |= {
                "sha": trigger.sha,
                "pr": trigger.pr,
                "checks": sorted({c.strip() for c in trigger.checks if c.strip()}),
            }
        elif kind == rules.KIND_WORK_ITEM:
            assert trigger.work_item is not None
            item = _resolve.work_item(view, trigger.work_item)
            states = [s.strip() for s in trigger.states if s.strip()] or sorted(
                TERMINAL_STATES
            )
            stored |= {"ref": item.ref, "work_item_id": item.id, "states": states}
    if kind == rules.KIND_SESSION:
        assert trigger.session is not None
        watched = _engine_id(ctx, trigger.session)
        if watched == target_session:
            msg = "a session cannot wait for itself to end"
            raise InvalidRequest(msg)
        stored |= {
            "session": trigger.session,
            "engine_session_id": watched,
            "until": trigger.until,
        }
    return stored


def _engine_id(ctx: AppContext, session_id: str) -> str:
    wanted = session_id.strip()
    if not wanted:
        msg = "a session id is required (ses_… or the engine's session UUID)"
        raise InvalidRequest(msg)
    if not wanted.startswith("ses_"):
        return wanted
    with ctx.declared.read() as view:
        session = view.session_by_id(wanted)
    if session is None:
        msg = f"no session {wanted!r}"
        raise NotFound(msg)
    return session.engine_session_id


def _may_cancel(ctx: AppContext, watch: Watch) -> bool:
    with ctx.declared.read() as view:
        caller = view.actor_by_identity(ctx.principal.identity_ref)
    if caller is not None and caller.id == watch.owner_actor_id:
        return True
    return _own_engine_session(ctx) == watch.target_session


def _load(ctx: AppContext, watch_id: str) -> Watch:
    with ctx.declared.read() as view:
        watch = view.watch(watch_id.strip())
    if watch is None:
        msg = f"no watch {watch_id!r}"
        raise NotFound(msg)
    return watch


def _outcome(txn: WriteTxn, watch: Watch, event: str) -> WriteOutcome[Watch]:
    del txn
    return WriteOutcome(
        result=watch,
        entity_kind="watch",
        entity_id=watch.id,
        payload=watch.model_dump(mode="json"),
        event_kind=event,
        summary={
            "watch_id": watch.id,
            "kind": watch.kind,
            "session": watch.target_session,
            "waits_for": rules.describe(watch.kind, watch.trigger),
            "state": watch.state,
            "outcome_state": None
            if watch.outcome is None
            else watch.outcome.get("state"),
            "attempts": watch.attempts,
        },
    )


def _view(view: ReadView, watch: Watch, ctx: AppContext) -> WatchView:
    owner = view.actor_by_id(watch.owner_actor_id)
    outcome = watch.outcome
    return WatchView(
        id=watch.id,
        owner=watch.owner_actor_id if owner is None else owner.identity_ref,
        session=watch.target_session,
        kind=watch.kind,  # type: ignore[arg-type]
        trigger=watch.trigger,
        waits_for=rules.describe(watch.kind, watch.trigger),
        note=watch.note,
        state=watch.effective_state(ctx.clock()),
        created_at=watch.created_at,
        expires_at=watch.expires_at,
        fired_at=watch.fired_at,
        outcome=outcome,
        delivered=None if outcome is None else outcome.get("delivered"),
        attempts=watch.attempts,
        last_error=watch.last_error,
        reason=watch.reason,
    )


__all__ = [
    "MAX_ATTEMPTS",
    "WatchPass",
    "arm",
    "cancel_watch",
    "create_watch",
    "evaluate_watches",
    "list_watches",
]
