"""Projects: registration, scaffolding, the per-repo brief, lifecycle."""

from __future__ import annotations

from pathlib import Path

from vogt.application.context import AppContext
from vogt.application.models import (
    DEFAULT_EXCLUSIONS,
    CiSummary,
    CreateProjectParams,
    CreateProjectResult,
    DependencySummary,
    GetProjectParams,
    ListProjectsParams,
    ProjectBriefParams,
    ProjectBriefResult,
    ProjectListResult,
    ProjectResult,
    RegisterProjectParams,
    ScaffoldProjectParams,
    ScaffoldProjectResult,
    TransitionProjectParams,
    UpdateProjectParams,
)
from vogt.application.services import _resolve
from vogt.application.services.views import _gather, freshness_of
from vogt.application.writes import WriteOutcome, audited_write
from vogt.collectors.dep_refs import KIND_DEP_SCAN
from vogt.core.checks import roll_up
from vogt.core.contract import (
    DEFAULT_CONTRACT,
    NOT_APPLICABLE,
    Scaffold,
    default_scaffold,
)
from vogt.core.entities import Actor, Project
from vogt.core.ids import slugify
from vogt.core.workflow import check_lifecycle_transition
from vogt.errors import Conflict, InvalidRequest, NotFound
from vogt.storage.interface import ProjectUpdate, WorkFilter, WriteTxn

PROJECT_REGISTER = "project.register"
PROJECT_CREATE = "project.create"
PROJECT_TRANSITION = "project.transition"
PROJECT_UPDATE = "project.update"

PROJECT_REGISTERED_EVENT = "project.registered"
PROJECT_TRANSITIONED_EVENT = "project.transitioned"
PROJECT_UPDATED_EVENT = "project.updated"
PROJECT_SCAFFOLD = "project.scaffold"
PROJECT_SCAFFOLDED_EVENT = "project.scaffolded"


def _new_project(
    ctx: AppContext,
    *,
    slug: str,
    params: RegisterProjectParams,
    adopts_contract: bool = False,
) -> Project:
    """A new project row.

    `adopts_contract` is false for a project Vogt is *handed* (FR-G16): the
    contract is opt-in, and registering a folder is not opting in. It is true
    for one Vogt *creates*, because asking for a contract-shaped project is
    the opting in — the skeleton and the adoption are the same request.
    """
    now = ctx.clock()
    return Project(
        id=ctx.id_factory("prj"),
        slug=slug,
        name=params.name,
        root_path=params.root_path,
        repo_url=params.repo_url,
        lifecycle_state=params.lifecycle_state,
        compliance_status="not_checked",
        contract_adopted_at=now if adopts_contract else None,
        exclusions=(
            list(DEFAULT_EXCLUSIONS)
            if params.exclusions is None
            else list(params.exclusions)
        ),
        trust_state="unverified",
        created_at=now,
        updated_at=now,
    )


def _slug_for(name: str) -> str:
    slug = slugify(name)
    if not slug:
        msg = f"cannot derive a slug from name {name!r}"
        raise InvalidRequest(msg)
    return slug


def register_project(ctx: AppContext, params: RegisterProjectParams) -> ProjectResult:
    """Register an existing folder or repository as a project (FR-P1).

    Registration is never refused on contract grounds (FR-G11) — the contract
    is a status to be read, not a barrier to pass. It *is* refused for a
    duplicate slug, which is a naming collision rather than a judgement about
    the project.
    """
    return record_registration(ctx, params)


def record_registration(
    ctx: AppContext,
    params: RegisterProjectParams,
    *,
    operation: str = PROJECT_REGISTER,
    event_kind: str = PROJECT_REGISTERED_EVENT,
    adopts_contract: bool = False,
) -> ProjectResult:
    """The declared half of registration, under a caller-named operation.

    `project.import` lands the same row, but the audit must say which act
    put it there: "registered" and "imported from GitHub" are different
    answers to "where did this project come from", and collapsing them would
    lose the only record that a clone happened (FR-S1).
    """
    slug = _slug_for(params.name)

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[ProjectResult]:
        del actor
        if txn.project_by_slug(slug) is not None:
            msg = f"a project with slug {slug!r} is already registered"
            raise Conflict(msg)
        project = _new_project(
            ctx, slug=slug, params=params, adopts_contract=adopts_contract
        )
        txn.insert_project(project)
        return WriteOutcome(
            result=ProjectResult(project=project),
            entity_kind="project",
            entity_id=project.id,
            payload=project.model_dump(mode="json"),
            event_kind=event_kind,
            summary={"slug": project.slug, "name": project.name},
        )

    return audited_write(ctx, operation=operation, reason=params.reason, body=body)


def _lay_scaffold(
    root: Path, scaffold: Scaffold, *, create_root: bool
) -> tuple[list[str], list[str]]:
    """Write a scaffold into a directory, never overwriting (FR-G11, FR-G17).

    Shared by `project create` and `project scaffold` so the two report the
    same way: a path that was already there is *skipped*, and skipped means
    untouched — not merged, not backed up, not rewritten. That is the whole
    guarantee that makes offering the scaffold to an existing project safe.
    """
    created: list[str] = []
    skipped: list[str] = []

    if not root.exists():
        if not create_root:
            raise NotFound(
                f"{root} does not exist, so there is nothing to scaffold into"
            )
        root.mkdir(parents=True)
        created.append(str(root))
    for directory in scaffold.directories:
        target = root / directory
        if target.exists():
            skipped.append(str(target))
        else:
            target.mkdir(parents=True)
            created.append(str(target))
    for entry in scaffold.files:
        target = root / entry.path
        if target.exists():
            skipped.append(str(target))
        else:
            target.write_text(entry.content, encoding="utf-8")
            created.append(str(target))
    return created, skipped


def scaffold_project(
    ctx: AppContext, params: ScaffoldProjectParams
) -> ScaffoldProjectResult:
    """Lay the contract's skeleton into an already-registered project (FR-G17).

    The asymmetry this closes: a project Vogt created got the template, and a
    project Vogt was handed got a verdict. Reporting a gap the product can
    close, without offering to close it, is the enforcing posture in a
    different costume — so the same scaffold `project create` writes is
    reachable here, against a project that already exists, and it still never
    overwrites anything.

    Nothing about this adopts the contract or changes a status: it writes
    files. `contract adopt` is the declaration, and it is a separate act on
    purpose.
    """
    with ctx.declared.read() as view:
        project = _resolve.project(view, params.project)

    root = Path(project.root_path).expanduser()
    scaffold = default_scaffold(
        name=project.name,
        owner=ctx.principal.display_name,
        lifecycle_state=project.lifecycle_state,
    )
    created, skipped = _lay_scaffold(root, scaffold, create_root=False)

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[ScaffoldProjectResult]:
        del txn, actor
        return WriteOutcome(
            result=ScaffoldProjectResult(
                project=project.slug,
                root_path=str(root),
                created=sorted(created),
                skipped=sorted(skipped),
                detail=(
                    f"{len(created)} written, {len(skipped)} already there and "
                    "left exactly as they were"
                ),
            ),
            entity_kind="project",
            entity_id=project.id,
            payload={"created": sorted(created), "skipped": sorted(skipped)},
            event_kind=PROJECT_SCAFFOLDED_EVENT,
            summary={
                "slug": project.slug,
                "created": len(created),
                "skipped": len(skipped),
            },
        )

    return audited_write(
        ctx, operation=PROJECT_SCAFFOLD, reason=params.reason, body=body
    )


def create_project(ctx: AppContext, params: CreateProjectParams) -> CreateProjectResult:
    """Scaffold a contract-compliant skeleton, then register it (FR-G11).

    The scaffold is written before the declared write, and never overwrites:
    a file that exists is reported as skipped, not replaced. That ordering
    means a failed registration leaves a harmless skeleton on disk rather
    than a registered project pointing at a half-written directory.
    """
    slug = _slug_for(params.name)
    owner = params.owner or ctx.principal.display_name
    root = Path(params.root_path).expanduser()

    scaffold = default_scaffold(
        name=params.name, owner=owner, lifecycle_state=params.lifecycle_state
    )
    created, skipped = _lay_scaffold(root, scaffold, create_root=True)

    registered = record_registration(
        ctx,
        RegisterProjectParams(
            name=params.name,
            root_path=str(root),
            repo_url=params.repo_url,
            lifecycle_state=params.lifecycle_state,
            reason=params.reason,
        ),
        # Asking Vogt to create a contract-shaped project *is* adopting the
        # contract; a project handed to `project register` is not (FR-G16).
        adopts_contract=True,
    )
    del slug
    return CreateProjectResult(
        project=registered.project,
        created_paths=sorted(created),
        skipped_paths=sorted(skipped),
    )


def get_project(ctx: AppContext, params: GetProjectParams) -> ProjectResult:
    with ctx.declared.read() as view:
        return ProjectResult(project=_resolve.project(view, params.slug))


def list_projects(ctx: AppContext, params: ListProjectsParams) -> ProjectListResult:
    with ctx.declared.read() as view:
        return ProjectListResult(
            projects=view.list_projects(limit=params.limit, offset=params.offset),
            total=view.counts().projects,
        )


def brief_project(ctx: AppContext, params: ProjectBriefParams) -> ProjectBriefResult:
    """The per-repo view in one call (FR-P2).

    The fields nothing collects yet — CI status, dependency summary,
    freshness — are present and say `not_collected` rather than being absent
    or zero. "Nobody has looked" and "there is nothing" are different
    answers, and a shape that changes at M2 is a shape every client has to
    handle twice.
    """
    with ctx.declared.read() as view:
        project = _resolve.project(view, params.slug)
        base = WorkFilter(project_id=project.id, limit=1000)
        items = view.list_work_items(base)

        by_state: dict[str, int] = {}
        by_kind: dict[str, int] = {}
        for item in items:
            by_state[item.state] = by_state.get(item.state, 0) + 1
            by_kind[item.kind] = by_kind.get(item.kind, 0) + 1

        observed_version = _observed_version(ctx, project)

    # The same population `backlog` and `bugs` rank, so the three views agree
    # about one project at one moment. They did not: this read the declared
    # store alone, so a project whose whole backlog arrived through
    # `forge onboard` briefed as `open_work: 0` with nine issues ranked in
    # `backlog --project` for the same project at the same moment. The brief
    # is the first surface an import's owner reads, and it said the import had
    # found nothing.
    gathered = _gather(
        ctx,
        project=params.slug,
        kinds=None,
        priorities=None,
        assignee=None,
        initiative=None,
        label=None,
    )

    return ProjectBriefResult(
        project=project,
        open_work=len(gathered.ranked),
        open_bugs=sum(1 for entry in gathered.ranked if entry.kind == "bug"),
        declared_work=gathered.declared,
        observed_work=gathered.observed,
        by_state=dict(sorted(by_state.items())),
        by_kind=dict(sorted(by_kind.items())),
        top_backlog=gathered.ranked[: params.backlog_limit],
        current_version=project.current_version,
        declared_version=project.current_version,
        observed_version=observed_version,
        version_matches=(
            None
            if observed_version is None or project.current_version is None
            else observed_version.lstrip("v") == project.current_version.lstrip("v")
        ),
        # FR-G16: no view may imply a fault in a project that never adopted
        # the contract. The brief is a view, and `not_checked` on a project
        # nobody may check reads as an omission rather than as a choice.
        compliance_status=(
            project.compliance_status
            if project.contract_adopted_at is not None
            else NOT_APPLICABLE
        ),
        compliance_checked_at=(
            project.compliance_checked_at
            if project.contract_adopted_at is not None
            else None
        ),
        ci_status=_ci_summary(ctx, project),
        dependencies=_dependency_summary(ctx, project),
        freshness=freshness_of(ctx),
    )


def _observed_version(ctx: AppContext, project: Project) -> str | None:
    """The newest tag or release a collector has seen (FR-P3).

    Reported next to the declared version rather than instead of it. The two
    disagreeing is `version_mismatch` drift, which becomes a proposal at M3;
    here it is simply a fact the brief states.
    """
    if not ctx.observed.has_evidence_tables():
        return None
    seen = ctx.observed.latest(
        kinds=("git.tag", "release"), project_id=project.id, limit=100
    )
    tags = [str(o.payload.get("tag", "")) for o in seen if o.payload.get("tag")]
    return max(tags, default=None) or None


def _ci_summary(ctx: AppContext, project: Project) -> CiSummary:
    """The CI story for the newest observed revision (FR-O6).

    Scoped to one revision, and says which. It used to be a verdict over
    every retained check across every commit, pinned to whichever row sorted
    last by sweep time — so a project whose head was green read `failing`
    because something failed days earlier, and could never read green again
    until the old row aged out. `core.checks` does the grouping; this decides
    how to say it.
    """
    if not ctx.observed.has_evidence_tables():
        return CiSummary(detail="no sweep has run; CI status is not collected")
    checks = ctx.observed.latest(kinds=("ci.check",), project_id=project.id, limit=200)
    rollup = roll_up(checks)
    if rollup is None:
        return CiSummary(
            status="no_checks",
            detail=(
                "swept, but no CI checks were observed — either this project "
                "has none, or the optional forge adapter is not configured"
            ),
        )
    return CiSummary(
        status="failing" if rollup.failing else "passing",
        checks=len(rollup.checks),
        failing=list(rollup.failing),
        revision=rollup.revision,
        revisions_observed=rollup.revisions_observed,
        earlier_failures=rollup.earlier_failures,
        detail=(
            None
            if not rollup.earlier_failures
            else (
                f"{rollup.earlier_failures} failing check(s) on earlier "
                f"revisions are not counted here; this is the state of "
                f"{(rollup.revision or '')[:12]} alone"
            )
        ),
    )


def _dependency_summary(ctx: AppContext, project: Project) -> DependencySummary:
    """What this project references, and what references it (FR-D1–D4).

    `collected` is claimed per project, not per estate. `has_evidence_tables`
    answers whether anything anywhere has been swept, and a project
    registered after the last sweep passes that test while having been
    walked by nothing — which reported `collected, 0 references` for a
    project nobody had looked at (#50).
    """
    if not ctx.observed.has_evidence_tables():
        return DependencySummary(
            detail="no sweep has run; dependency references are not collected"
        )
    if not ctx.observed.latest(kinds=(KIND_DEP_SCAN,), project_id=project.id, limit=1):
        return DependencySummary(
            detail=(
                "`dep-refs` has not walked this project; its references are "
                "not collected, which is not the same as none"
            )
        )
    out = ctx.observed.dep_refs(from_project_id=project.id)
    incoming = ctx.observed.dep_refs(to_project_id=project.id)
    return DependencySummary(
        status="collected",
        references_out=len(out),
        referenced_by=len(incoming),
        unresolved=sum(1 for ref in out if ref.to_project_id is None),
    )


def update_project(ctx: AppContext, params: UpdateProjectParams) -> ProjectResult:
    """Correct a project's declaration after registration (FR-G12).

    Registration used to be the only chance to state exclusions, and it did
    not take them either — so the defaults were what every project got, for
    good. `pingRAG` was registered with 20,212 of its 20,329 tracked files
    inside a vendored `corpus/`, and every source marker Vogt holds for it
    comes from somebody else's documentation as a result. There was nothing
    to run to fix that.
    """

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[ProjectResult]:
        del actor
        project = _resolve.project(txn, params.slug)
        if params.repo_url is None and params.exclusions is None:
            msg = "give --repo-url or --exclusions; there is nothing else to update"
            raise InvalidRequest(msg)
        txn.update_project(
            project.id,
            ProjectUpdate(
                repo_url=params.repo_url,
                exclusions=(
                    None if params.exclusions is None else tuple(params.exclusions)
                ),
            ),
            at=ctx.clock(),
        )
        updated = txn.project_by_slug(project.slug)
        assert updated is not None  # just written in this transaction
        return WriteOutcome(
            result=ProjectResult(project=updated),
            entity_kind="project",
            entity_id=project.id,
            payload=updated.model_dump(mode="json"),
            event_kind=PROJECT_UPDATED_EVENT,
            summary={
                "slug": project.slug,
                "exclusions": list(updated.exclusions),
                "repo_url": updated.repo_url,
            },
        )

    return audited_write(ctx, operation=PROJECT_UPDATE, reason=params.reason, body=body)


def transition_project(
    ctx: AppContext, params: TransitionProjectParams
) -> ProjectResult:
    """Move a project through its lifecycle, validating the edge (FR-P4)."""

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[ProjectResult]:
        del actor
        project = _resolve.project(txn, params.slug)
        check_lifecycle_transition(
            from_state=project.lifecycle_state, to_state=params.to_state
        )
        now = ctx.clock()
        txn.update_project(
            project.id, ProjectUpdate(lifecycle_state=params.to_state), at=now
        )
        updated = txn.project_by_slug(params.slug)
        assert updated is not None  # just written in this transaction
        return WriteOutcome(
            result=ProjectResult(project=updated),
            entity_kind="project",
            entity_id=project.id,
            payload=updated.model_dump(mode="json"),
            event_kind=PROJECT_TRANSITIONED_EVENT,
            summary={
                "slug": project.slug,
                "from": project.lifecycle_state,
                "to": params.to_state,
            },
        )

    return audited_write(
        ctx, operation=PROJECT_TRANSITION, reason=params.reason, body=body
    )


__all__ = [
    "DEFAULT_CONTRACT",
    "brief_project",
    "create_project",
    "get_project",
    "list_projects",
    "record_registration",
    "register_project",
    "transition_project",
    "update_project",
]
