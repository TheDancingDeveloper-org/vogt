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

from datetime import datetime
from pathlib import Path

from vogt.application.context import AppContext
from vogt.application.models import (
    ComplianceParams,
    ComplianceResult,
    ContractAdoptParams,
    ContractAdoptResult,
    ContractApplicableParams,
    ContractCheckParams,
    ContractCheckResult,
    ContractEvaluateParams,
    ContractExemptionResult,
    ContractExemptionView,
    ContractInapplicableParams,
    CriterionView,
    RecommendationView,
)
from vogt.application.services import _resolve
from vogt.application.writes import WriteOutcome, audited_write
from vogt.collectors.base import CollectorContext
from vogt.collectors.contract_checker import ContractCheckerCollector
from vogt.collectors.git_local import tracked_names
from vogt.core.contract import (
    NOT_APPLICABLE,
    NOT_CHECKED,
    Recommendation,
    configured_contract,
    default_scaffold,
    evaluate,
    recommendations,
)
from vogt.core.entities import Actor, ContractExemption, Project
from vogt.core.ids import new_id
from vogt.storage.interface import ProjectUpdate, ReadView, WriteTxn

CONTRACT_CHECK = "contract.check"
CONTRACT_CHECKED_EVENT = "contract.checked"
CONTRACT_ADOPT = "contract.adopt"
CONTRACT_DECLINE = "contract.decline"
CONTRACT_ADOPTION_EVENT = "contract.adoption_changed"
CONTRACT_INAPPLICABLE = "contract.inapplicable"
CONTRACT_APPLICABLE = "contract.applicable"
CONTRACT_EXEMPTION_EVENT = "contract.exemption_changed"

#: What a project that never adopted the contract is told, and why that is
#: not a criticism (FR-G16).
NOT_ADOPTED_DETAIL = (
    "this project has not adopted the contract, so there is nothing for it to "
    "comply with — this is not a fault. `contract adopt` opts in."
)


def contract_evaluate(
    ctx: AppContext, params: ContractEvaluateParams
) -> ContractCheckResult:
    """Evaluate the contract against any path, storing nothing (FR-G4).

    A pure read, and now shaped like one: no `reason`, no `project.write`
    scope. Both were required while this shared an operation with the
    recording half, so the CLI collected a justification for a write that
    never happened and a read-only token could not run it at all.
    """
    root = Path(params.path)
    result = evaluate(
        root, configured_contract(ctx.config), tracked=tracked_names(root)
    )
    return ContractCheckResult(
        path=result.path,
        project=None,
        contract_version=result.contract_version,
        status=result.status,
        criteria=[_view(c) for c in result.criteria],
        failing=[_view(c) for c in result.failing],
        inapplicable=[_view(c) for c in result.inapplicable],
        recommendations=_advice(result),
        recorded=False,
        checked_at=None,
    )


def contract_check(ctx: AppContext, params: ContractCheckParams) -> ContractCheckResult:
    """Evaluate a registered project's contract and record the result (FR-G14).

    The result lands twice: as evidence in the observed store with a subject
    key and a timestamp, and as a projection on the project row. `recorded:
    true` is the thing that distinguishes this from `contract evaluate`.
    """
    with ctx.declared.read() as view:
        project = _resolve.project(view, params.project)
        exempt = _exemptions(view, project)

    # A project that never opted in is not measured, and nothing is recorded
    # against it (FR-G16). The answer is still an answer — it says which
    # question was not asked, and how to ask it — but it is not a verdict,
    # and no criterion is reported, because a list of "failures" under a
    # `not_applicable` heading is the judgement this requirement removes.
    if project.contract_adopted_at is None:
        return ContractCheckResult(
            path=project.root_path,
            project=project.slug,
            contract_version=configured_contract(ctx.config).version,
            status=NOT_APPLICABLE,
            criteria=[],
            failing=[],
            inapplicable=[],
            recommendations=[],
            recorded=False,
            checked_at=None,
            detail=NOT_ADOPTED_DETAIL,
        )

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

    root = Path(project.root_path)
    result = evaluate(
        root,
        configured_contract(ctx.config),
        tracked=tracked_names(root),
        inapplicable=exempt,
    )

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
                inapplicable=[_view(c) for c in result.inapplicable],
                recommendations=_advice(result, project=project),
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
        exemptions = view.contract_exemptions(project.id)

    # FR-G16: the fourth answer, and the one that is never stored. A project
    # that declined the contract has no compliance to report, and reporting
    # its last recorded status would be reporting a verdict on a question it
    # never agreed to be asked.
    if project.contract_adopted_at is None:
        return ComplianceResult(
            project=project.slug,
            status=NOT_APPLICABLE,
            contract_version=configured_contract(ctx.config).version,
            checked_at=None,
            age_seconds=None,
            failing=[],
            adopted=False,
            adopted_at=None,
            inapplicable=[],
            detail=NOT_ADOPTED_DETAIL,
        )

    exempt_keys = {(exemption.rule, exemption.target) for exemption in exemptions}
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
        contract_version=configured_contract(ctx.config).version,
        checked_at=checked_at,
        age_seconds=age,
        # A criterion somebody declared unmeetable is not a failure, and a
        # stored answer from before the declaration should not keep saying it
        # is (FR-G19).
        failing=[
            criterion
            for criterion in failing
            if (criterion.rule, criterion.target) not in exempt_keys
        ],
        adopted=True,
        adopted_at=project.contract_adopted_at,
        inapplicable=[
            CriterionView(
                rule=exemption.rule,
                target=exemption.target,
                satisfied=False,
                applicable=False,
                detail=f"declared inapplicable to this project: {exemption.reason}",
            )
            for exemption in exemptions
        ],
        detail=_compliance_detail(project.compliance_status, checked_at=checked_at),
    )


# -- adoption, and the declarations around it (FR-G16, FR-G18, FR-G19) ------


def _exemptions(view: ReadView, project: Project) -> dict[tuple[str, str], str]:
    """The criteria somebody declared unmeetable for this project."""
    return {
        (exemption.rule, exemption.target): exemption.reason
        for exemption in view.contract_exemptions(project.id)
    }


def _advice(
    result: object, *, project: Project | None = None
) -> list[RecommendationView]:
    """FR-G18's recommendations, in the shape the transports return."""
    scaffold = default_scaffold(
        name=project.name if project else "this project",
        owner="the project's owner",
        lifecycle_state=project.lifecycle_state if project else "active",
    )
    advice: tuple[Recommendation, ...] = recommendations(
        result,  # type: ignore[arg-type]
        scaffold=scaffold,
    )
    return [
        RecommendationView(
            rule=one.rule,
            target=one.target,
            remedy=one.remedy,  # type: ignore[arg-type]
            instruction=one.instruction,
        )
        for one in advice
    ]


def contract_adopt(ctx: AppContext, params: ContractAdoptParams) -> ContractAdoptResult:
    """Opt a project into the contract (FR-G16).

    Adoption is a declaration with an author and a reason, like every other
    write here. It is deliberately not inferred from anything — not from a
    passing check, not from a scaffolded directory — because the whole point
    is that a project which wants Vogt as a plain issue tracker never has to
    make it.
    """
    return _set_adoption(ctx, params.project, params.reason, adopted=True)


def contract_decline(
    ctx: AppContext, params: ContractAdoptParams
) -> ContractAdoptResult:
    """Opt a project back out of the contract (FR-G16).

    The reverse of adoption, and audited the same way: an adoption made by
    mistake is not something to be stuck with, and a project whose posture
    changed should be able to say so rather than accumulate a status nobody
    agreed to.
    """
    return _set_adoption(ctx, params.project, params.reason, adopted=False)


def _set_adoption(
    ctx: AppContext, slug: str, reason: str, *, adopted: bool
) -> ContractAdoptResult:
    with ctx.declared.read() as view:
        project = _resolve.project(view, slug)
    now = ctx.clock()
    already = (project.contract_adopted_at is not None) == adopted

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[ContractAdoptResult]:
        del actor
        if not already:
            txn.update_project(
                project.id,
                ProjectUpdate(
                    contract_adopted_at=now if adopted else None,
                    clear_contract_adopted_at=not adopted,
                    # Declining does not leave the last verdict standing: a
                    # project that is no longer measured has nothing to have
                    # been found non-compliant about.
                    compliance_status=None if adopted else NOT_CHECKED,
                ),
                at=now,
            )
        adopted_at = (
            (project.contract_adopted_at or now)
            if adopted and already
            else (now if adopted else None)
        )
        return WriteOutcome(
            result=ContractAdoptResult(
                project=project.slug,
                adopted=adopted,
                adopted_at=adopted_at,
                status=NOT_CHECKED if adopted else NOT_APPLICABLE,
                detail=(
                    f"{project.slug} already had that posture; nothing changed"
                    if already
                    else (
                        "the contract applies to this project from now on; "
                        "`contract check` evaluates it"
                        if adopted
                        else NOT_ADOPTED_DETAIL
                    )
                ),
            ),
            entity_kind="project",
            entity_id=project.id,
            payload={"contract_adopted": adopted},
            event_kind=CONTRACT_ADOPTION_EVENT,
            summary={"slug": project.slug, "adopted": adopted},
        )

    return audited_write(
        ctx,
        operation=CONTRACT_ADOPT if adopted else CONTRACT_DECLINE,
        reason=reason,
        body=body,
    )


def contract_inapplicable(
    ctx: AppContext, params: ContractInapplicableParams
) -> ContractExemptionResult:
    """Declare that a criterion cannot apply to a project (FR-G19).

    Not an exemption from a rule the project could keep: a statement that the
    rule does not describe this project — a Cargo workspace has no root
    `src/`. It carries an author and a reason for exactly that reason: the
    difference between the two is an argument somebody has to make.
    """
    with ctx.declared.read() as view:
        project = _resolve.project(view, params.project)
    now = ctx.clock()

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[ContractExemptionResult]:
        txn.insert_contract_exemption(
            ContractExemption(
                id=new_id("cex"),
                project_id=project.id,
                project_slug=project.slug,
                rule=params.rule,
                target=params.target,
                reason=params.reason,
                declared_by=actor.identity_ref or actor.id,
                declared_at=now,
            )
        )
        return WriteOutcome(
            result=ContractExemptionResult(
                project=project.slug,
                declared=True,
                exemptions=_exemption_views(txn, project),
                detail=(
                    f"{params.target} is recorded as unmeetable by "
                    f"{project.slug}; it is reported, and not counted as a "
                    "failure"
                ),
            ),
            entity_kind="project",
            entity_id=project.id,
            payload={"rule": params.rule, "target": params.target},
            event_kind=CONTRACT_EXEMPTION_EVENT,
            summary={
                "slug": project.slug,
                "target": params.target,
                "inapplicable": True,
            },
        )

    return audited_write(
        ctx, operation=CONTRACT_INAPPLICABLE, reason=params.reason, body=body
    )


def contract_applicable(
    ctx: AppContext, params: ContractApplicableParams
) -> ContractExemptionResult:
    """Withdraw an inapplicability declaration (FR-G19)."""
    with ctx.declared.read() as view:
        project = _resolve.project(view, params.project)

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[ContractExemptionResult]:
        del actor
        removed = txn.delete_contract_exemption(
            project_id=project.id, rule=params.rule, target=params.target
        )
        return WriteOutcome(
            result=ContractExemptionResult(
                project=project.slug,
                declared=False,
                exemptions=_exemption_views(txn, project),
                detail=(
                    f"{params.target} applies to {project.slug} again"
                    if removed
                    else f"{params.target} was not declared inapplicable here"
                ),
            ),
            entity_kind="project",
            entity_id=project.id,
            payload={"rule": params.rule, "target": params.target},
            event_kind=CONTRACT_EXEMPTION_EVENT,
            summary={
                "slug": project.slug,
                "target": params.target,
                "inapplicable": False,
            },
        )

    return audited_write(
        ctx, operation=CONTRACT_APPLICABLE, reason=params.reason, body=body
    )


def _exemption_views(view: ReadView, project: Project) -> list[ContractExemptionView]:
    return [
        ContractExemptionView(
            rule=exemption.rule,
            target=exemption.target,
            reason=exemption.reason,
            declared_by=exemption.declared_by,
            declared_at=exemption.declared_at,
        )
        for exemption in view.contract_exemptions(project.id)
    ]


def _compliance_detail(status: str, *, checked_at: datetime | None) -> str | None:
    """Which `not_checked` this is.

    Two things record it and they mean different things: nobody has run the
    check, or somebody ran it against a root path that could not be read
    (#50). Both are honestly "not checked"; only one of them is fixed by
    running `contract check`.
    """
    if status != NOT_CHECKED:
        return None
    if checked_at is None:
        return "nobody has checked this project's contract; run `contract check`"
    return (
        "the last check could not read this project's root path, so no "
        "criterion was evaluated — check the registered path"
    )


def _view(criterion: object) -> CriterionView:
    return CriterionView(
        rule=criterion.rule,  # type: ignore[attr-defined]
        target=criterion.target,  # type: ignore[attr-defined]
        satisfied=criterion.satisfied,  # type: ignore[attr-defined]
        detail=criterion.detail,  # type: ignore[attr-defined]
        tracked=criterion.tracked,  # type: ignore[attr-defined]
    )


def _as_list(value: object) -> list[dict[str, object]]:
    if not isinstance(value, list):
        return []
    return [entry for entry in value if isinstance(entry, dict)]
