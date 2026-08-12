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
from vogt.errors import InvalidRequest

PRUNE = "observations.prune"
PRUNED_EVENT = "observations.pruned"


def _protected_observation_ids(ctx: AppContext) -> frozenset[str]:
    """Observations pinned by a drift proposal (FR-R5).

    Empty until M3 introduces `drift_proposals`. It is a function rather
    than a constant so that the call site reads correctly now and keeps
    working when the table appears — the alternative is remembering to add
    the exemption later, which is exactly the kind of thing nobody
    remembers.
    """
    del ctx
    return frozenset()


def prune(ctx: AppContext, params: PruneParams) -> PruneResult:
    """Apply the retention policy to observation history."""
    if not ctx.observed.has_evidence_tables():
        msg = "nothing has been swept yet; there is no history to prune"
        raise InvalidRequest(msg)

    horizon = ctx.clock() - timedelta(days=ctx.config.retention_days)
    report = ctx.observed.prune(
        before=horizon, protected_observation_ids=_protected_observation_ids(ctx)
    )
    ctx.declared.publish_event(
        kind=PRUNED_EVENT,
        entity_kind="instance",
        entity_id="observations",
        summary={
            "removed": report.removed,
            "kept_latest": report.kept_latest,
            "kept_referenced": report.kept_referenced,
            "reason": params.reason,
        },
        at=ctx.clock(),
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
