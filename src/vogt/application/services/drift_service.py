"""Raising drift proposals, and resolving them.

The engine reads both stores and writes only proposals (`SCHEMA.md` §1).
Accepting one is an ordinary audited application write — the same path every
other declared change goes through, so an accepted proposal is as
explainable as anything a human typed.
"""

from __future__ import annotations

from vogt.application.context import AppContext
from vogt.application.models import (
    DriftDetectParams,
    DriftDetectResult,
    DriftListParams,
    DriftListResult,
    DriftResolveParams,
    DriftResult,
)
from vogt.application.services import _resolve
from vogt.application.writes import WriteOutcome, audited_write
from vogt.core.drift import (
    AUTO_ACCEPTABLE_KINDS,
    FORGE_STATE_MISMATCH,
    HUMAN_GATED_REASON,
    UNRESOLVED_DEPENDENCY,
    VERSION_MISMATCH,
    DriftFinding,
    EvidenceSnapshot,
    ci_red_vs_healthy,
    forge_state_mismatch,
    unresolved_dependency,
    update_automation_gap,
    vanished_upstream,
    version_mismatch,
)
from vogt.core.entities import Actor, DriftProposal, Observation
from vogt.core.workflow import TERMINAL_STATES
from vogt.errors import Conflict, InvalidRequest, NotFound
from vogt.storage.interface import ProjectUpdate, WorkFilter, WorkItemUpdate, WriteTxn

DRIFT_DETECT = "drift.detect"
DRIFT_RESOLVE = "drift.resolve"

DRIFT_RAISED_EVENT = "drift.raised"
DRIFT_RESOLVED_EVENT = "drift.resolved"

RESOLUTIONS = ("accepted", "rejected", "contested")


def _snapshot(observation: Observation) -> EvidenceSnapshot:
    """Copy the evidence as it stands now (FR-R5).

    Taken at raise time and stored on the proposal, so the proposal still
    explains itself when retention has pruned the history around it. The
    pointer to the live row is kept as well, and retention refuses to prune
    it — belt and braces, because the two stores are pruned independently.
    """
    return EvidenceSnapshot(
        subject_key=observation.subject_key,
        content_digest=observation.content_digest,
        observed_at=observation.observed_at,
        collector=observation.collector,
        payload=dict(observation.payload),
    )


def _version_findings(ctx: AppContext) -> list[DriftFinding]:
    findings: list[DriftFinding] = []
    with ctx.declared.read() as view:
        projects = view.list_projects(limit=10_000, offset=0)
    for project in projects:
        seen = ctx.observed.latest(
            kinds=("git.tag", "release"), project_id=project.id, limit=100
        )
        tagged = [(str(o.payload.get("tag", "")), o) for o in seen]
        tagged = [(tag, o) for tag, o in tagged if tag]
        if not tagged:
            continue
        newest_tag, observation = max(tagged, key=lambda pair: pair[0])
        found = version_mismatch(
            project_id=project.id,
            project_slug=project.slug,
            declared=project.current_version,
            observed=newest_tag,
            evidence=_snapshot(observation),
            evidence_observation_id=observation.id,
        )
        if found is not None:
            findings.append(found)
    return findings


def _dependency_findings(ctx: AppContext) -> list[DriftFinding]:
    findings: list[DriftFinding] = []
    with ctx.declared.read() as view:
        slugs = {p.id: p.slug for p in view.list_projects(limit=10_000, offset=0)}
    for ref in ctx.observed.dep_refs():
        if ref.to_project_id is not None:
            continue
        observations = ctx.observed.list_observations(
            subject_key=ref.subject_key, limit=1
        )
        if not observations:
            continue
        findings.append(
            unresolved_dependency(
                subject_key=ref.subject_key,
                project_id=ref.from_project_id,
                project_slug=slugs.get(ref.from_project_id, ref.from_project_id),
                raw_target=ref.raw_target,
                manifest=ref.manifest,
                evidence=_snapshot(observations[0]),
                evidence_observation_id=observations[0].id,
            )
        )
    return findings


def _forge_findings(ctx: AppContext) -> list[DriftFinding]:
    """The M5 kinds: state mismatch, vanished upstream, red CI, posture."""
    findings: list[DriftFinding] = []
    with ctx.declared.read() as view:
        projects = {p.id: p for p in view.list_projects(limit=10_000, offset=0)}
        items = view.list_work_items(WorkFilter(limit=10_000))
        links = {
            item.id: view.work_links_for_subjects_by_item(item.id) for item in items
        }
    coverage = ctx.observed.coverage()

    # forge_state_mismatch and vanished_upstream, per linked item.
    for item in items:
        for subject_key in links.get(item.id, {}):
            if not subject_key.startswith("gh:"):
                continue
            seen = ctx.observed.list_observations(subject_key=subject_key, limit=1)
            if not seen:
                # Absence is only meaningful inside provably swept scope
                # (FR-O4). Without a completed forge sweep this is "not
                # collected", and raising it would be the exact mistake that
                # made most of cadastre's "missing" drift an artefact.
                swept = coverage.get("gh-issues")
                if swept is None or swept.finished_at is None:
                    continue
                findings.append(
                    vanished_upstream(
                        work_item_id=item.id,
                        work_ref=item.ref,
                        subject_key=subject_key,
                        project_id=item.project_id,
                        swept_at=swept.finished_at,
                    )
                )
                continue

            observation = seen[0]
            upstream = str(observation.payload.get("state", "open"))
            finished = item.state in TERMINAL_STATES
            if (upstream == "closed") != finished:
                findings.append(
                    forge_state_mismatch(
                        work_item_id=item.id,
                        work_ref=item.ref,
                        declared_state=item.state,
                        upstream_state=upstream,
                        subject_key=subject_key,
                        project_id=item.project_id,
                        evidence=_snapshot(observation),
                        evidence_observation_id=observation.id,
                    )
                )

    # ci_red_vs_healthy and update_automation_gap, per project.
    for project in projects.values():
        checks = ctx.observed.latest(
            kinds=("ci.check",), project_id=project.id, limit=200
        )
        failing = [
            (str(c.payload.get("check", "?")), c)
            for c in checks
            if c.payload.get("conclusion") not in (None, "success", "skipped")
        ]
        if failing and project.lifecycle_state in ("active", "maintenance"):
            newest = max((c for _, c in failing), key=lambda c: c.observed_at)
            findings.append(
                ci_red_vs_healthy(
                    project_id=project.id,
                    project_slug=project.slug,
                    lifecycle_state=project.lifecycle_state,
                    failing=[name for name, _ in failing],
                    revision=str(newest.payload.get("revision", "")),
                    evidence=_snapshot(newest),
                    evidence_observation_id=newest.id,
                )
            )

        posture = ctx.observed.latest(
            kinds=("forge.posture",), project_id=project.id, limit=1
        )
        if posture:
            payload = posture[0].payload
            # Three independent facts, named individually (FR-D6). `None`
            # means "could not tell" and is not a gap.
            missing = [
                label
                for label, key in (
                    ("version updates", "version_updates"),
                    ("vulnerability alerts", "vulnerability_alerts"),
                    ("automated security fixes", "automated_security_fixes"),
                )
                if payload.get(key) is False
            ]
            if missing:
                findings.append(
                    update_automation_gap(
                        project_id=project.id,
                        project_slug=project.slug,
                        missing=missing,
                        evidence=_snapshot(posture[0]),
                        evidence_observation_id=posture[0].id,
                    )
                )
    return findings


def detect_drift(ctx: AppContext, params: DriftDetectParams) -> DriftDetectResult:
    """Compare declared state against observation and raise proposals (FR-R1).

    Idempotent by construction: one open proposal per (kind, subject), so
    running this repeatedly over unchanged state asks the same question once.
    """
    # Refused rather than answered "no drift": with nothing collected, the
    # honest answer is "not collected", and silently reporting agreement
    # between declared state and evidence that does not exist is precisely
    # the confusion FR-O4 exists to prevent.
    if not ctx.observed.has_evidence_tables() or not ctx.observed.coverage():
        msg = (
            "no collector has completed a sweep, so there is nothing to "
            "compare declared state against — run `sweep` first"
        )
        raise InvalidRequest(msg)

    findings = _version_findings(ctx) + _dependency_findings(ctx) + _forge_findings(ctx)
    raised: list[DriftProposal] = []
    auto_accepted: list[str] = []

    with ctx.declared.read() as view:
        already_open = view.open_drift_subjects()

    for found in findings:
        key = (found.kind, found.subject_kind, found.subject_id)
        if key in already_open:
            continue
        proposal = DriftProposal(
            id=ctx.id_factory("dft"),
            kind=found.kind,
            subject_kind=found.subject_kind,
            subject_id=found.subject_id,
            project_id=found.project_id,
            summary=found.summary,
            evidence_observation_id=found.evidence_observation_id,
            evidence_snapshot=(
                {} if found.evidence is None else found.evidence.to_json()
            ),
            proposed_change=found.proposed_change,
            status="open",
            opened_at=ctx.clock(),
        )
        with ctx.declared.write() as txn:
            txn.insert_drift(proposal)
        ctx.declared.publish_event(
            kind=DRIFT_RAISED_EVENT,
            entity_kind="drift_proposal",
            entity_id=proposal.id,
            summary={"kind": proposal.kind, "summary": proposal.summary},
            at=ctx.clock(),
        )
        raised.append(proposal)

        if params.auto_accept and found.auto_acceptable:
            resolve_drift(
                ctx,
                DriftResolveParams(
                    id=proposal.id,
                    resolution="accepted",
                    reason=(
                        "auto-accepted under the shipped low-risk policy "
                        f"({found.kind} is a state-sync kind)"
                    ),
                ),
            )
            auto_accepted.append(proposal.id)

    return DriftDetectResult(
        raised=raised,
        auto_accepted=auto_accepted,
        already_open=len(findings) - len(raised),
        auto_acceptable_kinds=sorted(AUTO_ACCEPTABLE_KINDS),
    )


def list_drift(ctx: AppContext, params: DriftListParams) -> DriftListResult:
    with ctx.declared.read() as view:
        project_id = (
            None
            if params.project is None
            else _resolve.project(view, params.project).id
        )
        return DriftListResult(
            proposals=view.list_drift(
                status=params.status,
                kind=params.kind,
                project_id=project_id,
                limit=params.limit,
            ),
            human_gated=dict(HUMAN_GATED_REASON),
        )


def resolve_drift(ctx: AppContext, params: DriftResolveParams) -> DriftResult:
    """Accept, reject, or leave contested — always audited (FR-R2, FR-R3).

    `contested` is a resolution somebody *chose*: it records that the
    disagreement is real and unresolved. It is deliberately a different word
    from the computed trust state `disputed` (decided 2026-08-12); one word
    never means both things.
    """
    if params.resolution not in RESOLUTIONS:
        msg = f"resolution must be one of {', '.join(RESOLUTIONS)}"
        raise InvalidRequest(msg)

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[DriftResult]:
        proposal = txn.drift_by_id(params.id)
        if proposal is None:
            msg = f"no drift proposal {params.id!r}"
            raise NotFound(msg)
        if proposal.status != "open":
            msg = f"proposal {params.id!r} was already {proposal.status}"
            raise Conflict(msg)

        applied = False
        if params.resolution == "accepted":
            applied = _apply(txn, proposal, ctx=ctx)

        txn.resolve_drift(
            params.id,
            status=params.resolution,
            actor_id=actor.id,
            reason=params.reason,
            at=ctx.clock(),
        )
        updated = txn.drift_by_id(params.id)
        assert updated is not None  # just written in this transaction
        return WriteOutcome(
            result=DriftResult(proposal=updated, change_applied=applied),
            entity_kind="drift_proposal",
            entity_id=params.id,
            payload=updated.model_dump(mode="json"),
            event_kind=DRIFT_RESOLVED_EVENT,
            summary={
                "kind": updated.kind,
                "resolution": params.resolution,
                "change_applied": applied,
            },
        )

    return audited_write(ctx, operation=DRIFT_RESOLVE, reason=params.reason, body=body)


def _apply(txn: WriteTxn, proposal: DriftProposal, *, ctx: AppContext) -> bool:
    """Make the proposed change, in the same transaction as the resolution.

    Only kinds that *propose* a change apply one. `unresolved_dependency`
    proposes nothing to write — accepting it records the judgement that the
    reference is fine as it stands.
    """
    if proposal.kind == FORGE_STATE_MISMATCH:
        # The upstream change already happened; accepting syncs our copy.
        ref = str(proposal.proposed_change.get("work_ref", ""))
        target = str(proposal.proposed_change.get("to", ""))
        item = txn.work_item_by_ref(ref)
        if item is None or not target:
            return False
        txn.update_work_item(item.id, WorkItemUpdate(state=target), at=ctx.clock())
        return True
    if proposal.kind != VERSION_MISMATCH:
        return False
    change = proposal.proposed_change
    if change.get("entity") != "project" or proposal.project_id is None:
        return False
    txn.update_project(
        proposal.project_id,
        ProjectUpdate(current_version=str(change.get("to", ""))),
        at=ctx.clock(),
    )
    return True


__all__ = [
    "UNRESOLVED_DEPENDENCY",
    "VERSION_MISMATCH",
    "detect_drift",
    "list_drift",
    "resolve_drift",
]
