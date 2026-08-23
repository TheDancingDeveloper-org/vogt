"""Raising drift proposals, and resolving them.

The engine reads both stores and writes only proposals (`SCHEMA.md` §1).
Accepting one is an ordinary audited application write — the same path every
other declared change goes through, so an accepted proposal is as
explainable as anything a human typed.
"""

from __future__ import annotations

from datetime import datetime

from vogt.adapters.forge import KIND_ISSUE, current_collector
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
from vogt.application.services.views import freshness_of
from vogt.application.writes import WriteOutcome, audited_write
from vogt.collectors.dep_refs import (
    SCOPE_BROKEN,
    SCOPE_EXTERNAL,
    SCOPE_INTERNAL,
)
from vogt.core.checks import roll_up
from vogt.core.drift import (
    AUTO_ACCEPTABLE_KINDS,
    FORGE_STATE_MISMATCH,
    HUMAN_GATED_REASON,
    UNRESOLVED_DEPENDENCY,
    VERSION_MISMATCH,
    DriftFinding,
    EvidenceSnapshot,
    broken_path_dependency,
    ci_red_vs_healthy,
    forge_state_mismatch,
    issue_references,
    referenced_issue_state_mismatch,
    unresolved_dependency,
    update_automation_gap,
    vanished_upstream,
    version_mismatch,
)
from vogt.core.entities import Actor, DriftProposal, Observation, WorkItem
from vogt.core.workflow import TERMINAL_STATES
from vogt.errors import Conflict, InvalidRequest, NotFound
from vogt.storage.interface import ProjectUpdate, WorkFilter, WorkItemUpdate, WriteTxn

DRIFT_DETECT = "drift.detect"
DRIFT_RESOLVE = "drift.resolve"

DRIFT_RAISED_EVENT = "drift.raised"
DRIFT_RESOLVED_EVENT = "drift.resolved"
DRIFT_SUPERSEDED_EVENT = "drift.superseded"

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
    """Unresolved references, split by where the reference actually lands.

    A reference the collector scoped `internal` is a project pointing at its
    own crates, which is what a workspace *is* — it never resolves to another
    registered project and must never be proposed as one. Thirty of rustnzb's
    thirty-one proposals were that, and the gate text on every one of them
    read "usually it is a project nobody has registered yet".
    """
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
        # Read from the observation rather than the projection: the scope is
        # a fact the collector established while it had the manifest and the
        # tree in front of it, and re-deriving it here would be a second
        # implementation to disagree with the first.
        scope = observations[0].payload.get("scope", SCOPE_EXTERNAL)
        if scope == SCOPE_INTERNAL:
            continue
        raise_as = (
            broken_path_dependency if scope == SCOPE_BROKEN else unresolved_dependency
        )
        findings.append(
            raise_as(
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


def _referenced_issue_findings(
    ctx: AppContext, *, item: WorkItem, linked: set[str]
) -> list[DriftFinding]:
    """Issues a work item's own text names, against what was observed (FR-R7).

    The gap this closes was found by an onboarding agent, not by Vogt:
    `WI-16` mirrored issue `#44`, named it in the first line of its body, and
    was marked done while `#44` stayed open for hours. Vogt already collects
    `forge.issue` observations and already treats declared-versus-observed
    disagreement as the thing drift exists to catch — this shape had simply
    never been wired up as one (#49).

    Three guards, each earning its place:

    - **Only issues.** A `forge.pull_request` observation shares the subject
      key space; comparing an item's state against a PR it mentions is a
      different question nobody asked.
    - **Not what is already linked.** An adopted `WorkLink` is
      `forge_state_mismatch`'s subject, and raising both would put two
      proposals on one disagreement.
    - **Only what was observed.** No observation is "not collected", and
      raising drift from that is the mistake FR-O4 exists to prevent.
    """
    findings: list[DriftFinding] = []
    for subject_key in issue_references(f"{item.title}\n{item.body or ''}"):
        if subject_key in linked:
            continue
        seen = ctx.observed.list_observations(subject_key=subject_key, limit=1)
        if not seen or seen[0].kind != KIND_ISSUE:
            continue
        observation = seen[0]
        upstream = str(observation.payload.get("state", "open"))
        if (upstream == "closed") == (item.state in TERMINAL_STATES):
            continue
        findings.append(
            referenced_issue_state_mismatch(
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
                swept = coverage.get(current_collector("gh-issues"))
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

        # referenced_issue_state_mismatch, from the item's own text.
        findings.extend(
            _referenced_issue_findings(
                ctx, item=item, linked=set(links.get(item.id, {}))
            )
        )

    # ci_red_vs_healthy and update_automation_gap, per project.
    for project in projects.values():
        checks = ctx.observed.latest(
            kinds=("ci.check",), project_id=project.id, limit=200
        )
        # Scoped to the newest observed revision. Reading the whole retained
        # window as one population raised this proposal against projects whose
        # head was green — the failure it named was days old and fixed, and
        # the same workflow appeared twice in one summary because it was two
        # runs on two commits. A stale red is not drift; it is history.
        rollup = roll_up(checks)
        if (
            rollup is not None
            and rollup.failing
            and project.lifecycle_state in ("active", "maintenance")
        ):
            newest = max(
                (
                    c
                    for c in rollup.checks
                    if c.payload.get("conclusion") not in (None, "success", "skipped")
                ),
                key=lambda c: c.observed_at,
            )
            findings.append(
                ci_red_vs_healthy(
                    project_id=project.id,
                    project_slug=project.slug,
                    lifecycle_state=project.lifecycle_state,
                    failing=list(rollup.failing),
                    revision=rollup.revision or "",
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


def _initiative_findings(ctx: AppContext) -> list[DriftFinding]:
    """A tracking-issue checkbox ticked upstream out of step with its member (#286).

    The projection renders the boxes from workflow state; a human ticking one
    on the forge is an edit inside Vogt's managed region, and the next
    re-render would restore it. Surfacing the disagreement as drift is what
    keeps that from being a silent overwrite: the tick is observed
    (the tracking issue is an ordinary issue), and here its state is compared
    against what the member's workflow state says the box should be.
    """
    from vogt.application.services import upstream
    from vogt.core.drift import initiative_checkbox_drift
    from vogt.core.initiative_projection import body_has_marker, parse_checkbox_states

    with ctx.declared.read() as view:
        initiatives = view.list_initiatives(limit=10_000, offset=0)
        if not initiatives:
            return []
        # (initiative_id, project_id) -> {forge_number: is_terminal}. Read the
        # upstream-truth way: on a linked project the members are the observed
        # issues joined to the overlay, so `list_work_items` (declared rows
        # only) would see none of them.
        expected: dict[tuple[str, str], dict[int, bool]] = {}
        member_refs: dict[tuple[str, str, int], str] = {}
        for project in upstream.linked_projects(view, None):
            items = upstream.upstream_items(
                ctx, view, project, include_closed=True, limit=10_000
            )
            for item in items:
                number = _forge_number(item.ref)
                if item.initiative_id is None or number is None:
                    continue
                key = (item.initiative_id, project.id)
                expected.setdefault(key, {})[number] = item.state in TERMINAL_STATES
                member_refs[(item.initiative_id, project.id, number)] = item.ref

    by_slug = {ini.slug: ini for ini in initiatives}
    findings: list[DriftFinding] = []
    for obs in ctx.observed.latest(kinds=(KIND_ISSUE,), limit=10_000):
        body = obs.payload.get("body")
        if not isinstance(body, str) or not body or obs.project_id is None:
            continue
        for slug, ini in by_slug.items():
            if not body_has_marker(body, slug):
                continue
            member_state = expected.get((ini.id, obs.project_id), {})
            for number, checked in parse_checkbox_states(body).items():
                if number not in member_state or checked == member_state[number]:
                    continue
                findings.append(
                    initiative_checkbox_drift(
                        initiative_id=ini.id,
                        initiative_slug=ini.slug,
                        project_id=obs.project_id,
                        subject_key=obs.subject_key,
                        number=number,
                        work_ref=member_refs[(ini.id, obs.project_id, number)],
                        upstream_checked=checked,
                        expected_checked=member_state[number],
                        evidence=_snapshot(obs),
                        evidence_observation_id=obs.id,
                    )
                )
    return findings


def _forge_number(ref: str) -> int | None:
    """The trailing `#<n>` of a subject key, the member's forge issue number.

    Every provider keys an object by number (D5), so the numeric tail is the
    scheme itself — the same read `native_migration` uses — not a guess about
    one forge's spelling."""
    tail = ref.rpartition("#")[2]
    return int(tail) if tail.isdigit() else None


def _raise_proposal(
    ctx: AppContext, proposal: DriftProposal, *, reason: str
) -> DriftProposal:
    """Insert one proposal as the declared write it is (NFR-I1).

    It landed in a bare `ctx.declared.write()` until 2026-08-16, which left `drift
    detect` putting entity rows into the declared store with no audit row and
    no actor — the rule `audited_write` exists to enforce, broken by the
    operation that creates more rows per run than any other.
    """

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[DriftProposal]:
        txn.insert_drift(proposal)
        # `project` is additive (#290): it lets the engine's agent-task trigger
        # scope a `drift-proposed` fire to one project from the event alone.
        # The proposal carries a `project_id`, not always a slug, so resolve the
        # slug here — a slug is what a person types and what the engine matches.
        # Omitted, not null, for a proposal that names no project. The
        # `kind`/`summary` a reader relied on are untouched.
        project = (
            txn.project_by_id(proposal.project_id)
            if proposal.project_id is not None
            else None
        )
        summary: dict[str, object] = {
            "kind": proposal.kind,
            "summary": proposal.summary,
        }
        if project is not None:
            summary["project"] = project.slug
        return WriteOutcome(
            result=proposal,
            entity_kind="drift_proposal",
            entity_id=proposal.id,
            payload=proposal.model_dump(mode="json"),
            event_kind=DRIFT_RAISED_EVENT,
            summary=summary,
        )

    return audited_write(ctx, operation="drift.detect", reason=reason, body=body)


def _mark_superseded(
    ctx: AppContext,
    proposal: DriftProposal,
    *,
    detail: str | None,
    at: datetime | None,
    reason: str,
) -> None:
    """Flag — or un-flag — an open proposal as raised under stale evidence.

    An audited write like every other change to a declared row, because it
    changes what the inbox says about a proposal somebody is going to act on.
    """

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[DriftProposal]:
        del actor
        txn.mark_drift_superseded(proposal.id, detail=detail, at=at)
        updated = txn.drift_by_id(proposal.id)
        assert updated is not None  # read back inside the same transaction
        return WriteOutcome(
            result=updated,
            entity_kind="drift_proposal",
            entity_id=proposal.id,
            payload=updated.model_dump(mode="json"),
            event_kind=DRIFT_SUPERSEDED_EVENT,
            summary={
                "kind": proposal.kind,
                "superseded": at is not None,
                "detail": detail,
            },
        )

    audited_write(ctx, operation=DRIFT_DETECT, reason=reason, body=body)


def _reconcile_open_proposals(
    ctx: AppContext, *, findings: list[DriftFinding], reason: str
) -> list[str]:
    """Re-validate the board against the evidence this run just read (FR-R6).

    `drift detect` only ever added. A fix that changes what a collector
    reports — a `ref_kind` reclassification, a corrected posture check —
    leaves behind however many proposals it had already raised under the old
    logic, and nothing distinguished "this is still true" from "this stopped
    being true when the fix landed". Thirty-six `unresolved_dependency`
    proposals survived WI-2's fix, its deploy, a regression and a re-fix, and
    were cleared by hand after somebody reconstructed the timeline from
    timestamps (#48).

    **Not auto-close, deliberately.** FR-R2 keeps resolution with a human or
    an authorised agent, and FR-U18 refuses even bulk *accept* in the GUI
    because a person should see the evidence per proposal. This marks; it
    never resolves.

    **Coverage-gated, deliberately.** Silence is only meaningful inside a
    sweep that provably covered the subject (FR-O4). A proposal is marked
    only when the collector that raised it has completed a sweep *since* the
    proposal was opened and the condition did not reappear in it — absence
    without that sweep is "not collected", which is the mistake this whole
    layer exists to avoid making.
    """
    current = {(f.kind, f.subject_kind, f.subject_id) for f in findings}
    coverage = ctx.observed.coverage()
    with ctx.declared.read() as view:
        board = view.list_drift(status="open", limit=10_000)

    marked: list[str] = []
    for proposal in board:
        key = (proposal.kind, proposal.subject_kind, proposal.subject_id)
        if key in current:
            if proposal.superseded_at is not None:
                # It came back. A stale "superseded" flag is worse than none:
                # it tells a reader to ignore a proposal that is true again.
                _mark_superseded(ctx, proposal, detail=None, at=None, reason=reason)
            continue
        if proposal.superseded_at is not None:
            continue
        collector = str(proposal.evidence_snapshot.get("collector", ""))
        # A proposal raised before the Phase 2 rename names the old collector;
        # coverage records only the new one, so resolve through the alias map
        # or its retirement path is dead (D4, #173).
        swept = coverage.get(current_collector(collector))
        if swept is None or swept.finished_at is None:
            continue
        if swept.finished_at <= proposal.opened_at:
            continue
        _mark_superseded(
            ctx,
            proposal,
            detail=(
                f"{collector} completed a sweep at "
                f"{swept.finished_at.isoformat()}, after this was raised, and "
                "the condition that raised it no longer reproduces — the "
                "evidence snapshot above is what it was raised on"
            ),
            at=ctx.clock(),
            reason=reason,
        )
        marked.append(proposal.id)
    return marked


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

    findings = (
        _version_findings(ctx)
        + _dependency_findings(ctx)
        + _forge_findings(ctx)
        + _initiative_findings(ctx)
    )
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
        _raise_proposal(ctx, proposal, reason=params.reason)
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
        superseded=_reconcile_open_proposals(
            ctx, findings=findings, reason=params.reason
        ),
        not_collected=_projects_without_evidence(ctx),
        auto_acceptable_kinds=sorted(AUTO_ACCEPTABLE_KINDS),
    )


def _projects_without_evidence(ctx: AppContext) -> list[str]:
    """Registered projects no collector has ever swept (FR-O4, #50).

    `detect` refuses outright when *nothing* has been collected, which was
    the whole guard: with one project swept and eleven not, it reported the
    raised count and said nothing about the eleven it could not have raised
    anything for. A zero from a project nobody looked at is not a zero.
    """
    if not ctx.observed.has_evidence_tables():
        return []
    seen: set[str] = set()
    for project_ids in ctx.observed.coverage_by_project().values():
        seen.update(project_ids)
    with ctx.declared.read() as view:
        return sorted(
            project.slug
            for project in view.list_projects(limit=10_000, offset=0)
            if project.id not in seen
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
            freshness=freshness_of(ctx),
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
