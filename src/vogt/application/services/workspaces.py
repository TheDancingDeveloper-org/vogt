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
from vogt.application.context import AppContext
from vogt.application.models import (
    WorkspaceListParams,
    WorkspaceListResult,
    WorkspaceRow,
)
from vogt.application.services.sessions import _engine
from vogt.core.entities import Project
from vogt.errors import NotFound


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
