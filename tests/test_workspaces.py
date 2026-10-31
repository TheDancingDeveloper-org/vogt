"""`workspace.list` (WI-1340): engine workspace rows joined to Vogt."""

from __future__ import annotations

import dataclasses
import json
from typing import Any

import pytest

from vogt.adapters.engine import EngineClient, EngineUnavailable
from vogt.application.context import AppContext
from vogt.application.models import (
    EngineDiskStatus,
    RegisterProjectParams,
    StartSessionParams,
    WorkspaceCleanParams,
    WorkspaceListParams,
)
from vogt.application.services import (
    register_project,
    start_session,
    workspace_clean,
    workspace_list,
)
from vogt.application.services.workspaces import disk_candidates
from vogt.errors import CleanRefused, Conflict, InvalidRequest, NotFound
from vogt.storage.observed_types import PendingObservation

from tests.test_session_drive import ROOT, WHY, Engine, _as

UNLINKED = "0b0e5f0c-1111-4222-8333-944455556666"


class WorkspaceEngine(Engine):
    def __init__(self) -> None:
        super().__init__()
        self.rows: list[dict[str, Any]] | None = []
        self.cleans: list[dict[str, Any]] = []
        self.clean_status = 200
        self.clean_refusal = ""

    def __call__(
        self,
        url: str,
        headers: dict[str, str],
        body: bytes = b"",
        method: str = "GET",
    ) -> tuple[int, bytes]:
        path = url.split("?", 1)[0].removeprefix("http://127.0.0.1:8910")
        if method == "POST" and path == "/api/workspaces/clean":
            sent = json.loads(body)
            self.cleans.append(sent)
            if self.clean_status != 200:
                text = json.dumps({"error": f"conflict: {self.clean_refusal}"})
                return self.clean_status, text.encode()
            return 200, json.dumps(
                {
                    "path": sent["path"],
                    "mode": sent["mode"],
                    "bytes_freed": 1234,
                    "removed": sent["mode"] == "remove",
                }
            ).encode()
        if method == "GET" and path == "/api/workspaces":
            if self.rows is None:
                return 404, b'{"error":"not found"}'
            return 200, json.dumps({"workspaces": self.rows}).encode()
        return super().__call__(url, headers, body, method)


def _seed_branch(ctx: AppContext, project_id: str, name: str, ahead: int) -> None:
    """A `git.branch` observation, as the branch collector writes it."""
    now = ctx.clock()
    sweep = ctx.observed.begin_sweep(collector="test", scope=[project_id], at=now)
    ctx.observed.append(
        sweep.id,
        [
            PendingObservation(
                kind="git.branch",
                subject_key=f"branch:{project_id}:{name}",
                payload={"name": name, "ahead": ahead},
                content_digest=f"branch-{name}-{ahead}",
                project_id=project_id,
            )
        ],
        at=now,
    )
    ctx.observed.finish_sweep(
        sweep.id, outcome="ok", stats={"projects": 1, "new": 1, "unchanged": 0}, at=now
    )
    ctx.observed.rebuild_latest()


def _row(session_id: str, branch: str, *, alive: bool = True) -> dict[str, Any]:
    return {
        "session_id": session_id,
        "alive": alive,
        "size_bytes": 4096,
        "kind": "worktree",
        "path": f"/w/{branch}",
        "repo_root": ROOT,
        "branch": branch,
        "base_sha": "a" * 40,
        "created_at": "2026-10-30T00:00:00Z",
        "baseline_dirty": [],
        "build_dirs": [],
    }


@pytest.fixture
def engine() -> WorkspaceEngine:
    return WorkspaceEngine()


@pytest.fixture
def wired(instance: AppContext, engine: WorkspaceEngine) -> AppContext:
    ctx = dataclasses.replace(
        instance,
        engine=EngineClient(base_url="http://127.0.0.1:8910", transport=engine),
    )
    register_project(
        ctx, RegisterProjectParams(name="Vogt", root_path=ROOT, reason=WHY)
    )
    with ctx.declared.read() as view:
        project = view.project_by_slug("vogt")
    assert project is not None
    _seed_branch(ctx, project.id, "wi-1-landed", ahead=0)
    _seed_branch(ctx, project.id, "wi-2-open", ahead=3)
    return ctx


def test_rows_join_project_session_and_merged(
    wired: AppContext, engine: WorkspaceEngine
) -> None:
    started = start_session(wired, StartSessionParams(project="vogt", reason=WHY))
    engine_id = started.session.engine_session_id
    engine.rows = [
        _row(engine_id, "wi-1-landed"),
        _row(UNLINKED, "wi-2-open", alive=False),
    ]

    every = workspace_list(wired, WorkspaceListParams()).workspaces
    assert len(every) == 2
    linked = next(r for r in every if r.session == started.session.id)
    assert linked.project == "vogt"
    assert linked.merged is True
    assert linked.size_bytes == 4096
    assert linked.alive is True
    unlinked = next(r for r in every if r.session == UNLINKED)
    # No Vogt session, so no project to look the branch up in.
    assert unlinked.project is None
    assert unlinked.merged is None

    only = workspace_list(wired, WorkspaceListParams(project="vogt")).workspaces
    assert [r.session for r in only] == [started.session.id]
    assert workspace_list(wired, WorkspaceListParams(alive=False)).workspaces == [
        unlinked
    ]


def test_an_unobserved_branch_is_merged_null(
    wired: AppContext, engine: WorkspaceEngine
) -> None:
    started = start_session(wired, StartSessionParams(project="vogt", reason=WHY))
    engine.rows = [_row(started.session.engine_session_id, "wi-9-unseen")]
    (row,) = workspace_list(wired, WorkspaceListParams()).workspaces
    assert row.merged is None


def test_an_unknown_project_is_not_found(wired: AppContext) -> None:
    with pytest.raises(NotFound):
        workspace_list(wired, WorkspaceListParams(project="nope"))


def test_an_engine_without_the_route_says_so(
    wired: AppContext, engine: WorkspaceEngine
) -> None:
    engine.rows = None
    with pytest.raises(EngineUnavailable):
        workspace_list(wired, WorkspaceListParams())


def test_clean_by_session_sends_its_path_and_audits(
    wired: AppContext, engine: WorkspaceEngine
) -> None:
    started = start_session(wired, StartSessionParams(project="vogt", reason=WHY))
    engine.rows = [_row(started.session.engine_session_id, "wi-1-landed")]
    done = workspace_clean(
        wired,
        WorkspaceCleanParams(id=started.session.id, mode="build_output", reason=WHY),
    )
    assert done.bytes_freed == 1234
    assert engine.cleans[-1] == {
        "path": "/w/wi-1-landed",
        "mode": "build_output",
        "force": False,
        "reason": WHY,
    }


def test_an_engine_refusal_is_a_conflict(
    wired: AppContext, engine: WorkspaceEngine
) -> None:
    engine.clean_status = 409
    engine.clean_refusal = "a live or hibernated session works in this tree"
    with pytest.raises(Conflict, match="live or hibernated"):
        workspace_clean(
            wired, WorkspaceCleanParams(path="/w/x", mode="remove", reason=WHY)
        )


def test_only_a_person_forces_a_removal(
    wired: AppContext, engine: WorkspaceEngine
) -> None:
    agent = _as(wired, "agent:session:ses_x")
    with pytest.raises(CleanRefused):
        workspace_clean(
            agent,
            WorkspaceCleanParams(path="/w/x", mode="remove", force=True, reason=WHY),
        )
    assert engine.cleans == []
    workspace_clean(agent, WorkspaceCleanParams(path="/w/x", mode="remove", reason=WHY))
    assert engine.cleans[-1]["force"] is False


def test_clean_needs_exactly_one_of_id_and_path(wired: AppContext) -> None:
    with pytest.raises(InvalidRequest):
        workspace_clean(wired, WorkspaceCleanParams(mode="remove", reason=WHY))


def test_the_disk_row_lists_merged_stopped_worktrees_only_over_threshold(
    wired: AppContext, engine: WorkspaceEngine
) -> None:
    started = start_session(wired, StartSessionParams(project="vogt", reason=WHY))
    other = start_session(wired, StartSessionParams(project="vogt", reason=WHY))
    big = _row(started.session.engine_session_id, "wi-1-landed", alive=False)
    big["size_bytes"] = 10**9
    engine.rows = [
        big,
        _row(other.session.engine_session_id, "wi-2-open", alive=False),
    ]
    fine = EngineDiskStatus(used_percent=50, needs_attention=False)
    assert disk_candidates(wired, fine) == []
    assert disk_candidates(wired, None) == []
    full = EngineDiskStatus(used_percent=95, needs_attention=True)
    (row,) = disk_candidates(wired, full)
    assert row.branch == "wi-1-landed"
    assert row.size_bytes == 10**9
    assert "mode='remove'" in row.call
    assert "/w/wi-1-landed" in row.call
