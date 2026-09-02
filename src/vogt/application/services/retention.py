"""Retention over the observed store (NFR-I5).

History is pruned; evidence is not. Three rules in precedence order, and a
row survives if any of them protects it:

1. **The newest observation per subject is kept indefinitely.** Digest dedup
   (FR-O7) means a stable subject's newest row can be far older than the
   history window — a repository nobody has touched in a year still has a
   current answer, and age alone must never prune it.
2. **Anything a drift proposal references is kept** (FR-R5). The proposals
   arrive at M3; the exemption is wired now, because retention that runs
   before the exemption exists is how evidence goes missing.
3. Everything else is history, and the configured window applies.
"""

from __future__ import annotations

from datetime import timedelta

from vogt.application.context import AppContext
from vogt.application.models import PruneParams, PruneResult
from vogt.application.writes import audited_action
from vogt.errors import InvalidRequest

PRUNE = "observations.prune"
PRUNED_EVENT = "observations.pruned"


def _protected_observation_ids(ctx: AppContext) -> frozenset[str]:
    """Observations pinned by a drift proposal (FR-R5).

    Every proposal, of any status — including resolved ones. A resolved
    proposal still has to be able to show what it was resolved *on*, and an
    accepted change whose evidence has vanished is indistinguishable from an
    unexplained one.
    """
    with ctx.declared.read() as view:
        return view.drift_evidence_ids()


def prune(ctx: AppContext, params: PruneParams) -> PruneResult:
    """Apply the retention policy to observation history."""
    if not ctx.observed.has_evidence_tables():
        msg = "nothing has been swept yet; there is no history to prune"
        raise InvalidRequest(msg)

    now = ctx.clock()
    horizon = now - timedelta(days=ctx.config.retention_days)
    report = ctx.observed.prune(
        before=horizon, protected_observation_ids=_protected_observation_ids(ctx)
    )
    # Retention also caps the auth-decision telemetry (#526), which otherwise
    # grows one row per authenticated request forever. Allows get the ordinary
    # horizon; denies (the security-interesting rows) are kept 4x longer.
    ctx.declared.prune_auth_decisions(
        allow_before=horizon,
        deny_before=now - timedelta(days=ctx.config.retention_days * 4),
    )
    # Deletion is the one effect that cannot be re-derived by running the
    # operation again, so it is the least defensible of the four to have left
    # unattributed. The reason travelled in the event summary and nowhere a
    # reader of `audit list` would look.
    audited_action(
        ctx,
        operation="observations.prune",
        reason=params.reason,
        entity_kind="instance",
        entity_id="observations",
        outcome={
            "removed": report.removed,
            "kept_latest": report.kept_latest,
            "kept_referenced": report.kept_referenced,
            "horizon_days": ctx.config.retention_days,
        },
        event_kind=PRUNED_EVENT,
    )
    # The projection is rebuilt from what survived, so it can never point at
    # a row retention has just removed (NFR-I4).
    ctx.observed.rebuild_latest()
    return PruneResult(
        removed=report.removed,
        kept_latest=report.kept_latest,
        kept_referenced=report.kept_referenced,
        horizon_days=ctx.config.retention_days,
    )
