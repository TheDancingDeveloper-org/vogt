"""Contract checking: a value you read, not a barrier you pass.

FR-G13 is the rule that shapes this whole module: **no component may consume
contract compliance as a precondition for any operation.** Registering a
non-compliant folder succeeds. Creating work on one succeeds. Nothing here
returns a boolean that anything else branches on — it returns a status, its
failing criteria, and how old the answer is.

And nothing re-checks on a timer (r3). `contract check` runs when somebody
asks; the recorded status is always reported with its age, because a
three-week-old `compliant` is honest in a way a silently refreshed one is
not.
"""

from __future__ import annotations

from pathlib import Path

from vogt.application.context import AppContext
from vogt.application.models import (
    ComplianceParams,
    ComplianceResult,
    ContractCheckParams,
    ContractCheckResult,
    CriterionView,
)
from vogt.application.services import _resolve
from vogt.application.writes import WriteOutcome, audited_write
from vogt.collectors.base import CollectorContext
from vogt.collectors.contract_checker import ContractCheckerCollector
from vogt.core.contract import DEFAULT_CONTRACT, NOT_CHECKED, evaluate
from vogt.core.entities import Actor
from vogt.errors import InvalidRequest
from vogt.storage.interface import ProjectUpdate, WriteTxn

CONTRACT_CHECK = "contract.check"
CONTRACT_CHECKED_EVENT = "contract.checked"


def contract_check(ctx: AppContext, params: ContractCheckParams) -> ContractCheckResult:
    """Evaluate the contract against a path or a registered project (FR-G4).

    Against an unregistered folder it is a pure read: nothing is stored,
    because there is nothing to store it against. Against a registered
    project it also records the result — as evidence in the observed store
    and as a projection on the project row (FR-G14).
    """
    if not params.path and not params.project:
        msg = "give either --path or --project"
        raise InvalidRequest(msg)
    if params.path and params.project:
        msg = "give --path or --project, not both"
        raise InvalidRequest(msg)

    if params.path:
        result = evaluate(Path(params.path), DEFAULT_CONTRACT)
        return ContractCheckResult(
            path=result.path,
            project=None,
            contract_version=result.contract_version,
            status=result.status,
            criteria=[_view(c) for c in result.criteria],
            failing=[_view(c) for c in result.failing],
            recorded=False,
            checked_at=None,
        )

    with ctx.declared.read() as view:
        project = _resolve.project(view, params.project or "")

    # Run it as the collector, so the answer lands in the evidence store with
    # a subject key and a timestamp like every other observation.
    collector = ContractCheckerCollector()
    findings = list(
        collector.collect(CollectorContext(config=ctx.config, clock=ctx.clock), project)
    )
    now = ctx.clock()
    if ctx.observed.has_evidence_tables():
        sweep = ctx.observed.begin_sweep(
            collector=collector.name, scope=[project.id], at=now
        )
        stats = ctx.observed.append(sweep.id, findings, at=now)
        ctx.observed.finish_sweep(
            sweep.id,
            outcome="ok",
            stats={"projects": 1, "new": stats.new, "unchanged": stats.unchanged},
            at=ctx.clock(),
        )
        ctx.observed.rebuild_latest()

    result = evaluate(Path(project.root_path), DEFAULT_CONTRACT)

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[ContractCheckResult]:
        del actor
        txn.update_project(
            project.id,
            ProjectUpdate(
                compliance_status=result.status,
                compliance_checked_at=now,
            ),
            at=now,
        )
        return WriteOutcome(
            result=ContractCheckResult(
                path=result.path,
                project=project.slug,
                contract_version=result.contract_version,
                status=result.status,
                criteria=[_view(c) for c in result.criteria],
                failing=[_view(c) for c in result.failing],
                recorded=True,
                checked_at=now,
            ),
            entity_kind="project",
            entity_id=project.id,
            payload={
                "compliance_status": result.status,
                "failing": [c.target for c in result.failing],
            },
            event_kind=CONTRACT_CHECKED_EVENT,
            summary={
                "slug": project.slug,
                "status": result.status,
                "failing": len(result.failing),
            },
        )

    return audited_write(ctx, operation=CONTRACT_CHECK, reason=params.reason, body=body)


def compliance(ctx: AppContext, params: ComplianceParams) -> ComplianceResult:
    """The last recorded result, always with its age (FR-G14).

    `not_checked` is a first-class, unembarrassing answer. So is "compliant,
    checked 23 days ago" — this never refreshes implicitly, because a value
    that refreshes when you look at it cannot be reasoned about.
    """
    with ctx.declared.read() as view:
        project = _resolve.project(view, params.project)

    checked_at = project.compliance_checked_at
    age = (
        None if checked_at is None else int((ctx.clock() - checked_at).total_seconds())
    )
    failing: list[CriterionView] = []
    if ctx.observed.has_evidence_tables():
        recorded = ctx.observed.latest(
            kinds=("contract.check",), project_id=project.id, limit=1
        )
        if recorded:
            failing = [
                CriterionView(
                    rule=str(entry.get("rule", "")),
                    target=str(entry.get("target", "")),
                    satisfied=False,
                    detail=str(entry.get("detail", "")),
                )
                for entry in _as_list(recorded[0].payload.get("failing"))
            ]

    return ComplianceResult(
        project=project.slug,
        status=project.compliance_status,
        contract_version=DEFAULT_CONTRACT.version,
        checked_at=checked_at,
        age_seconds=age,
        failing=failing,
        detail=(
            "nobody has checked this project's contract; run `contract check`"
            if project.compliance_status == NOT_CHECKED
            else None
        ),
    )


def _view(criterion: object) -> CriterionView:
    return CriterionView(
        rule=criterion.rule,  # type: ignore[attr-defined]
        target=criterion.target,  # type: ignore[attr-defined]
        satisfied=criterion.satisfied,  # type: ignore[attr-defined]
        detail=criterion.detail,  # type: ignore[attr-defined]
    )


def _as_list(value: object) -> list[dict[str, object]]:
    if not isinstance(value, list):
        return []
    return [entry for entry in value if isinstance(entry, dict)]
