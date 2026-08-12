"""Projects: registration, scaffolding, the per-repo brief, lifecycle."""

from __future__ import annotations

from pathlib import Path

from vogt.application.context import AppContext
from vogt.application.models import (
    DEFAULT_EXCLUSIONS,
    CreateProjectParams,
    CreateProjectResult,
    GetProjectParams,
    ListProjectsParams,
    NotCollected,
    ProjectBriefParams,
    ProjectBriefResult,
    ProjectListResult,
    ProjectResult,
    RegisterProjectParams,
    TransitionProjectParams,
)
from vogt.application.services import _resolve
from vogt.application.services.views import rank_items
from vogt.application.writes import WriteOutcome, audited_write
from vogt.core.contract import DEFAULT_CONTRACT, default_scaffold
from vogt.core.entities import Actor, Project
from vogt.core.ids import slugify
from vogt.core.workflow import TERMINAL_STATES, check_lifecycle_transition
from vogt.errors import Conflict, InvalidRequest
from vogt.storage.interface import ProjectUpdate, WorkFilter, WriteTxn

PROJECT_REGISTER = "project.register"
PROJECT_CREATE = "project.create"
PROJECT_TRANSITION = "project.transition"

PROJECT_REGISTERED_EVENT = "project.registered"
PROJECT_TRANSITIONED_EVENT = "project.transitioned"


def _new_project(
    ctx: AppContext, *, slug: str, params: RegisterProjectParams
) -> Project:
    now = ctx.clock()
    return Project(
        id=ctx.id_factory("prj"),
        slug=slug,
        name=params.name,
        root_path=params.root_path,
        repo_url=params.repo_url,
        lifecycle_state=params.lifecycle_state,
        compliance_status="not_checked",
        exclusions=list(DEFAULT_EXCLUSIONS),
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
    slug = _slug_for(params.name)

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[ProjectResult]:
        del actor
        if txn.project_by_slug(slug) is not None:
            msg = f"a project with slug {slug!r} is already registered"
            raise Conflict(msg)
        project = _new_project(ctx, slug=slug, params=params)
        txn.insert_project(project)
        return WriteOutcome(
            result=ProjectResult(project=project),
            entity_kind="project",
            entity_id=project.id,
            payload=project.model_dump(mode="json"),
            event_kind=PROJECT_REGISTERED_EVENT,
            summary={"slug": project.slug, "name": project.name},
        )

    return audited_write(
        ctx, operation=PROJECT_REGISTER, reason=params.reason, body=body
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
    created: list[str] = []
    skipped: list[str] = []

    if not root.exists():
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

    registered = register_project(
        ctx,
        RegisterProjectParams(
            name=params.name,
            root_path=str(root),
            repo_url=params.repo_url,
            lifecycle_state=params.lifecycle_state,
            reason=params.reason,
        ),
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

        live = [item for item in items if item.state not in TERMINAL_STATES]
        ranked = rank_items(view, live, now=ctx.clock())

        return ProjectBriefResult(
            project=project,
            open_work=len(live),
            open_bugs=sum(1 for item in live if item.kind == "bug"),
            by_state=dict(sorted(by_state.items())),
            by_kind=dict(sorted(by_kind.items())),
            top_backlog=ranked[: params.backlog_limit],
            current_version=project.current_version,
            compliance_status=project.compliance_status,
            compliance_checked_at=project.compliance_checked_at,
            ci_status=NotCollected(
                detail="CI check observations arrive with the collectors at M2 (FR-O6)."
            ),
            dependencies=NotCollected(
                detail="Dependency references arrive with the dep-refs collector "
                "at M2 (FR-D1)."
            ),
            freshness=NotCollected(
                detail="No collector has swept this project; sweeps and their "
                "coverage records arrive at M2 (FR-O3, FR-V4)."
            ),
        )


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
    "register_project",
    "transition_project",
]
