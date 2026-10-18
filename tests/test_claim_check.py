"""Completion claims checked against git and the forge (WI-1835)."""

from __future__ import annotations

import dataclasses
import json
import subprocess
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import Any

import pytest

from vogt.adapters.engine import EngineClient
from vogt.application.context import AppContext
from vogt.application.models import (
    GetWorkParams,
    RegisterProjectParams,
    StartSessionParams,
    SweepSessionsParams,
    TransitionWorkParams,
)
from vogt.application.services import (
    get_work,
    register_project,
    start_session,
    sweep_sessions,
    transition_work,
)
from vogt.core.claims import ClaimFacts, PullFact, mentioned_pulls, mismatches
from vogt.core.principal import Principal
from vogt.storage.observed_types import PendingObservation

from tests.conftest import native_work_item

WHY = "claim check test"
STARTED = datetime(2026, 10, 18, 9, 0, tzinfo=UTC)

# -- the comparison, pure ----------------------------------------------------


def test_names_the_prs_a_claim_mentions() -> None:
    assert mentioned_pulls("PR #193 was opened and merged; see pull/7") == {193, 7}
    assert mentioned_pulls(None) == frozenset()


def test_a_pr_merged_before_the_session_started_is_flagged() -> None:
    """The WI-1707 claim: "PR #193 was opened and merged", merged the day before."""
    facts = ClaimFacts(
        claim_text="PR #193 was opened and merged. AUTOPILOT: DONE",
        session_started_at=STARTED,
        branch="wi-1707-x",
        head="abc",
        pushed=True,
        new_commits=0,
        pulls=(
            PullFact(
                number=193,
                state="merged",
                merged_at=STARTED - timedelta(days=1),
                merged_by="sprooty",
            ),
        ),
    )
    found = mismatches(facts)
    assert any(
        "PR #193" in line
        and "before this session started" in line
        and "sprooty" in line
        for line in found
    )
    assert "no new commits from this session on wi-1707-x" in found


def test_a_claimed_merge_of_an_open_pr_is_flagged() -> None:
    facts = ClaimFacts(
        claim_text="merged #4",
        session_started_at=STARTED,
        new_commits=2,
        pushed=True,
        pulls=(PullFact(number=4, state="open", checks="failure"),),
    )
    assert mismatches(facts) == [
        "claimed merged; PR #4 is open",
        "PR #4 CI is failure",
    ]


def test_an_honest_claim_and_unknown_facts_flag_nothing() -> None:
    honest = ClaimFacts(
        claim_text="pushed wi-1-x, PR #9 opened",
        session_started_at=STARTED,
        new_commits=3,
        pushed=True,
        pulls=(PullFact(number=9, state="open", checks="success"),),
    )
    assert mismatches(honest) == []
    assert mismatches(ClaimFacts(claim_text="done", session_started_at=None)) == []


def test_unpushed_work_is_flagged() -> None:
    facts = ClaimFacts(
        claim_text="done",
        session_started_at=STARTED,
        head="0123456789abcdef",
        new_commits=1,
        pushed=False,
    )
    assert mismatches(facts) == ["branch head 0123456789ab is not pushed"]


# -- through the sweep -------------------------------------------------------


def _git(root: Path, *args: str) -> str:
    return subprocess.run(
        ["git", "-C", str(root), *args],
        capture_output=True,
        text=True,
        check=True,
        env={
            "GIT_AUTHOR_NAME": "t",
            "GIT_AUTHOR_EMAIL": "t@example.com",
            "GIT_COMMITTER_NAME": "t",
            "GIT_COMMITTER_EMAIL": "t@example.com",
            "GIT_AUTHOR_DATE": "2026-10-17T09:00:00Z",
            "GIT_COMMITTER_DATE": "2026-10-17T09:00:00Z",
            "HOME": str(root),
            "PATH": "/usr/bin:/bin",
        },
    ).stdout.strip()


class Engine:
    def __init__(self, cwd: str) -> None:
        self.cwd = cwd
        self.rows: dict[str, dict[str, Any]] = {}

    def __call__(
        self, url: str, headers: dict[str, str], body: bytes = b"", method: str = "GET"
    ) -> tuple[int, bytes]:
        path = url.removeprefix("http://127.0.0.1:8910").partition("?")[0]
        if method == "POST" and path == "/api/sessions":
            engine_id = f"00000000-0000-4000-8000-00000000000{len(self.rows) + 1}"
            self.rows[engine_id] = {
                "id": engine_id,
                "name": engine_id,
                "activity": "running",
                "alive": True,
                "cwd": self.cwd,
                "created_at": STARTED.isoformat(),
                "last_output_at": STARTED.isoformat(),
            }
            return 200, json.dumps(self.rows[engine_id]).encode()
        if method == "GET" and path == "/api/sessions":
            return 200, json.dumps(
                {"sessions": list(self.rows.values()), "hidden_count": 0}
            ).encode()
        if method == "GET" and path == "/api/sessions/sweep":
            return 200, json.dumps(
                [
                    {"summary": row, "screen_tail": [], "ready": True}
                    for row in self.rows.values()
                ]
            ).encode()
        return 404, b""


@pytest.fixture
def repo(tmp_path: Path) -> Path:
    root = tmp_path / "proj"
    root.mkdir()
    _git(root, "init", "-q", "-b", "wi-1707-x")
    (root / "f").write_text("x")
    _git(root, "add", "f")
    _git(root, "commit", "-q", "-m", "old work")
    return root


def _wired(instance: AppContext, repo: Path) -> tuple[AppContext, Engine, str, str]:
    engine = Engine(str(repo))
    ctx = dataclasses.replace(
        instance,
        engine=EngineClient(base_url="http://127.0.0.1:8910", transport=engine),
        clock=lambda: STARTED,
    )
    register_project(
        ctx, RegisterProjectParams(name="Proj", root_path=str(repo), reason=WHY)
    )
    item = native_work_item(ctx, title="the header button", project="proj")
    started = start_session(ctx, StartSessionParams(work_item=item.ref, reason=WHY))
    with ctx.declared.read() as view:
        project = view.project_by_slug("proj")
    assert project is not None
    sweep = ctx.observed.begin_sweep(collector="test", scope=[project.id], at=STARTED)
    ctx.observed.append(
        sweep.id,
        [
            PendingObservation(
                kind="forge.pull_request",
                subject_key="gh:acme/app#193",
                payload={
                    "number": 193,
                    "state": "merged",
                    "head_ref": "wi-1707-x",
                    "checks": "success",
                    "implements": [],
                    "closed_at": (STARTED - timedelta(days=1)).isoformat(),
                },
                content_digest="pr-193",
                project_id=project.id,
            )
        ],
        at=STARTED,
    )
    ctx.observed.finish_sweep(
        sweep.id,
        outcome="ok",
        stats={"projects": 1, "new": 1, "unchanged": 0},
        at=STARTED,
    )
    ctx.observed.rebuild_latest()
    return ctx, engine, item.ref, started.session.id


def test_the_wi_1707_claim_is_flagged_on_the_done_row(
    instance: AppContext, repo: Path
) -> None:
    ctx, engine, ref, _session = _wired(instance, repo)
    for row in engine.rows.values():
        row.update(
            activity="waiting-for-input",
            done_at=STARTED.isoformat(),
            last_reply_excerpt="PR #193 was opened and merged.\nAUTOPILOT: DONE",
        )

    result = sweep_sessions(ctx, SweepSessionsParams(mode="digest"))

    (row,) = result.rows
    assert row.attention == "done"
    check = row.claim_check
    assert check is not None
    assert check.new_commits == 0
    assert any("before this session started" in line for line in check.mismatches)
    assert "claim check:" in row.attention_reason
    # The DONE claim is also noted on the item, once however often it is swept.
    sweep_sessions(ctx, SweepSessionsParams(mode="digest"))
    notes = [
        comment
        for comment in get_work(ctx, GetWorkParams(ref=ref)).comments
        if "Claim check (autopilot-done)" in comment.body
    ]
    assert len(notes) == 1


def test_others_commits_brought_in_by_a_rebase_are_not_the_sessions(
    tmp_path: Path, repo: Path
) -> None:
    """A worker that pulled a moved main after it started and did nothing
    still made no commits of its own."""
    from vogt.application.services.claims import _session_commits

    origin = tmp_path / "origin.git"
    _git(tmp_path, "init", "-q", "--bare", str(origin))
    _git(repo, "remote", "add", "origin", str(origin))
    _git(repo, "push", "-q", "origin", "wi-1707-x:main")
    other = tmp_path / "other"
    _git(tmp_path, "clone", "-q", "-b", "main", str(origin), str(other))
    (other / "g").write_text("y")
    _git(other, "add", "g")
    subprocess.run(
        ["git", "-C", str(other), "commit", "-q", "-m", "someone else"],
        check=True,
        capture_output=True,
        env={
            "GIT_AUTHOR_NAME": "o",
            "GIT_AUTHOR_EMAIL": "o@example.com",
            "GIT_COMMITTER_NAME": "o",
            "GIT_COMMITTER_EMAIL": "o@example.com",
            "GIT_COMMITTER_DATE": "2026-10-18T10:00:00Z",
            "HOME": str(other),
            "PATH": "/usr/bin:/bin",
        },
    )
    _git(other, "push", "-q", "origin", "main")
    _git(repo, "fetch", "-q", "origin")
    _git(repo, "merge", "-q", "--ff-only", "origin/main")
    head = _git(repo, "rev-parse", "HEAD")

    assert _session_commits(repo, head, "wi-1707-x", STARTED) == 0

    (repo / "h").write_text("z")
    _git(repo, "add", "h")
    subprocess.run(
        ["git", "-C", str(repo), "commit", "-q", "-m", "mine"],
        check=True,
        capture_output=True,
        env={
            "GIT_AUTHOR_NAME": "t",
            "GIT_AUTHOR_EMAIL": "t@example.com",
            "GIT_COMMITTER_NAME": "t",
            "GIT_COMMITTER_EMAIL": "t@example.com",
            "GIT_COMMITTER_DATE": "2026-10-18T11:00:00Z",
            "HOME": str(repo),
            "PATH": "/usr/bin:/bin",
        },
    )
    _git(repo, "push", "-q", "origin", "wi-1707-x")
    head = _git(repo, "rev-parse", "HEAD")
    assert _session_commits(repo, head, "wi-1707-x", STARTED) == 1


def test_a_sessions_move_to_review_is_checked_and_noted_on_the_item(
    instance: AppContext, repo: Path
) -> None:
    ctx, _engine, ref, session = _wired(instance, repo)
    as_session = dataclasses.replace(
        ctx,
        principal=Principal(
            identity_ref=f"agent:session:{session}",
            kind="agent",
            display_name=f"Session {session}",
        ),
    )

    result = transition_work(
        as_session,
        TransitionWorkParams(
            ref=ref,
            to_state="review",
            walk=True,
            reason="PR #193 was opened and merged",
        ),
    )

    check = result.claim_check
    assert check is not None and check.claim == "review"
    assert any("claimed PR #193" in line for line in check.mismatches)
    comments = get_work(ctx, GetWorkParams(ref=ref)).comments
    assert any("Claim check (review)" in comment.body for comment in comments)


def test_a_persons_move_is_not_a_claim(instance: AppContext, repo: Path) -> None:
    ctx, _engine, ref, _session = _wired(instance, repo)

    result = transition_work(
        ctx, TransitionWorkParams(ref=ref, to_state="review", walk=True, reason=WHY)
    )

    assert result.claim_check is None
