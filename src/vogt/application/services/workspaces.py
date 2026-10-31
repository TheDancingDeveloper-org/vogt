"""`workspace.list` (WI-1340): every session's working tree, joined to Vogt.

The engine owns the trees: `GET /api/workspaces` gives each session's
workspace block with its size and whether it is running. Vogt adds what the
engine cannot know — the project and work item the session was started for —
and whether the branch has landed, read from the `git-local` collector's
`git.branch` observations: a branch with nothing ahead of the project's
default branch has its tip in that branch, so it is merged. A branch the
collector has not observed is `merged: null`, never a guess.
"""

from __future__ import annotations

from vogt.adapters.engine import EngineUnavailable
from vogt.application import writes
from vogt.application.context import AppContext
from vogt.application.models import (
    EngineDiskStatus,
    WorkspaceCleanCandidate,
    WorkspaceCleanParams,
    WorkspaceCleanResult,
    WorkspaceListParams,
    WorkspaceListResult,
    WorkspaceRow,
)
from vogt.application.services.sessions import _engine
from vogt.application.writes import audited_action
from vogt.core.entities import Project
from vogt.errors import CleanRefused, InvalidRequest, NotFound

WORKSPACE_CLEAN = "workspace.clean"
WORKSPACE_CLEANED_EVENT = "workspace.cleaned"


def workspace_list(ctx: AppContext, params: WorkspaceListParams) -> WorkspaceListResult:
    """Every live or hibernated session's workspace, with Vogt's join."""
    rows = _engine(ctx).list_workspaces()
    if rows is None:
        msg = (
            "the session engine does not list workspaces yet (it has no "
            "GET /api/workspaces route); upgrade the engine"
        )
        raise EngineUnavailable(msg)
    out: list[WorkspaceRow] = []
    projects: dict[str, Project | None] = {}
    merged_by_project: dict[str, dict[str, bool | None]] = {}
    with ctx.declared.read() as view:
        wanted: Project | None = None
        if params.project is not None:
            wanted = view.project_by_slug(params.project)
            if wanted is None:
                msg = f"no project {params.project!r}"
                raise NotFound(msg)
        for row in rows:
            if params.alive is not None and row.alive != params.alive:
                continue
            linked = view.session_by_engine_id(row.session_id)
            project: Project | None = None
            work_item: str | None = None
            if linked is not None:
                if linked.project_id not in projects:
                    projects[linked.project_id] = view.project_by_id(linked.project_id)
                project = projects[linked.project_id]
                if linked.work_item_id is not None:
                    item = view.work_item_by_id(linked.work_item_id)
                    work_item = item.ref if item is not None else None
            if wanted is not None and (project is None or project.id != wanted.id):
                continue
            merged: bool | None = None
            if project is not None:
                if project.id not in merged_by_project:
                    merged_by_project[project.id] = _merged_branches(ctx, project.id)
                merged = merged_by_project[project.id].get(row.branch)
            out.append(
                WorkspaceRow(
                    path=row.path,
                    kind=row.kind,
                    branch=row.branch,
                    base_sha=row.base_sha,
                    size_bytes=row.size_bytes,
                    cleaned_at=row.cleaned_at,
                    alive=row.alive,
                    merged=merged,
                    work_item=work_item,
                    session=linked.id if linked is not None else row.session_id,
                    project=project.slug if project is not None else None,
                )
            )
    return WorkspaceListResult(workspaces=out)


def _merged_branches(ctx: AppContext, project_id: str) -> dict[str, bool | None]:
    """Branch name -> merged, from the collector's latest observations."""
    if not ctx.observed.has_evidence_tables():
        return {}
    merged: dict[str, bool | None] = {}
    for obs in ctx.observed.latest(
        kinds=("git.branch",), project_id=project_id, limit=1000
    ):
        name = obs.payload.get("name")
        if not isinstance(name, str) or not name:
            continue
        ahead = obs.payload.get("ahead")
        merged[name] = ahead == 0 if isinstance(ahead, int) else None
    return merged


def workspace_clean(
    ctx: AppContext, params: WorkspaceCleanParams
) -> WorkspaceCleanResult:
    """Drop a workspace's build output, or remove its worktree (WI-1325).

    The engine makes every safety check — no session in the tree, a clean
    tree, a merged branch unless forced — and refuses with a 409 that names
    the reason. Here: who may force, and the audit row.
    """
    reason = writes.validate_reason(params.reason)
    if params.force and ctx.principal.kind == "agent":
        # A forced removal throws away a worktree whose branch has not
        # landed, so it is a person's call, as a grant decision is.
        msg = (
            "only a person forces a workspace removal, not an agent "
            f"({ctx.principal.identity_ref}); ask a person to do it"
        )
        raise CleanRefused(msg)
    if (params.id is None) == (params.path is None):
        msg = "give exactly one of `id` (a session) or `path` (a workspace)"
        raise InvalidRequest(msg)
    engine = _engine(ctx)
    path = params.path
    if path is None:
        path = _path_of_session(ctx, params.id or "")
    done = engine.clean_workspace(
        path=path, mode=params.mode, force=params.force, reason=reason
    )
    if done is None:
        msg = (
            "the session engine cannot clean workspaces yet (it has no "
            "POST /api/workspaces/clean route); upgrade the engine"
        )
        raise EngineUnavailable(msg)
    result = WorkspaceCleanResult(
        path=str(done.get("path", path)),
        mode=params.mode,
        bytes_freed=int(done.get("bytes_freed", 0) or 0),
        removed=bool(done.get("removed", False)),
        branch_deleted=done.get("branch_deleted"),
        cleaned_at=done.get("cleaned_at"),
    )
    audited_action(
        ctx,
        operation=WORKSPACE_CLEAN,
        reason=reason,
        entity_kind="workspace",
        entity_id=result.path,
        outcome={
            "mode": result.mode,
            "force": params.force,
            "bytes_freed": result.bytes_freed,
            "removed": result.removed,
            "branch_deleted": result.branch_deleted,
        },
        event_kind=WORKSPACE_CLEANED_EVENT,
    )
    return result


def _path_of_session(ctx: AppContext, ref: str) -> str:
    """The workspace path of session `ref` (a `ses_…` id or engine UUID)."""
    engine_id = ref
    with ctx.declared.read() as view:
        linked = view.session_by_id(ref) if ref.startswith("ses_") else None
        if linked is not None and linked.engine_session_id:
            engine_id = linked.engine_session_id
    for row in _engine(ctx).list_workspaces() or []:
        if row.session_id == engine_id:
            return row.path
    msg = f"session {ref!r} has no workspace"
    raise NotFound(msg)


DISK_CANDIDATES = 5


def disk_candidates(
    ctx: AppContext, disk: EngineDiskStatus | None, *, limit: int = DISK_CANDIDATES
) -> list[WorkspaceCleanCandidate]:
    """The sweep's disk row (WI-1325): while the workspace volume needs
    attention, the `limit` largest stopped worktrees whose branch is merged,
    with the call that removes each. Empty when the volume is fine or the
    engine cannot list workspaces. A suggestion only.
    """
    if disk is None or not disk.needs_attention:
        return []
    try:
        rows = workspace_list(ctx, WorkspaceListParams(alive=False)).workspaces
    except EngineUnavailable:
        return []
    merged = [r for r in rows if r.kind == "worktree" and r.merged is True]
    merged.sort(key=lambda r: r.size_bytes or 0, reverse=True)
    return [
        WorkspaceCleanCandidate(
            path=r.path,
            branch=r.branch,
            size_bytes=r.size_bytes,
            session=r.session,
            call=(
                f"workspace_clean(path={r.path!r}, mode='remove', "
                "reason='disk over threshold; branch merged')"
            ),
        )
        for r in merged[:limit]
    ]
