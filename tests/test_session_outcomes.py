"""Session outcomes and bound agent-task runs as evidence (FR-E6, FR-E7).

The engine is stood in for by a transport that serves its four relevant
routes and remembers what it was asked, for the reason `test_sessions.py`
gives: what these tests are about is not that an observation was written but
*what it claims*, and above all what it refuses to claim about a session that
has not finished.

The git fixtures commit with an explicit committer date, because the whole
point of the working-tree delta is that it is bounded by the session's
window. A test whose commits landed "now" would pass against a collector
that ignored the window entirely.
"""

from __future__ import annotations

import dataclasses
import json
import subprocess
import urllib.parse
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import Any

import pytest

from vogt.adapters.engine import EngineClient, EngineUnavailable
from vogt.application.context import AppContext
from vogt.application.models import (
    CoverageParams,
    ObservationsParams,
    RegisterProjectParams,
    StartSessionParams,
    StopSessionParams,
    SweepParams,
)
from vogt.application.services import (
    coverage,
    observations,
    register_project,
    start_session,
    stop_session,
    sweep,
)
from vogt.application.services.collect import collector_registry
from vogt.collectors.session_outcomes import KIND_SESSION_OUTCOME, KIND_TASK_RUN
from vogt.core.entities import Observation

from tests.conftest import native_work_item

WHY = "session outcome test"

#: Inside the window of every session these tests start: the fixture clock
#: begins at 05:00 on 2026-08-12 and advances a second per read.
IN_WINDOW = "2026-08-12T05:00:30+00:00"
SESSION_END = datetime(2026, 8, 12, 6, 0, tzinfo=UTC)


class StandInEngine:
    """An engine with sessions, an archive of ended ones, and agent tasks."""

    def __init__(self) -> None:
        self.live: dict[str, dict[str, Any]] = {}
        self.archive: dict[str, dict[str, Any]] = {}
        self.tasks: list[dict[str, Any]] = []
        self.counter = 0
        self.cwds: dict[str, str] = {}

    def __call__(
        self,
        url: str,
        headers: dict[str, str],
        body: bytes = b"",
        method: str = "GET",
    ) -> tuple[int, bytes]:
        path = urllib.parse.urlsplit(url).path
        payload = json.loads(body.decode("utf-8")) if body else {}

        if method == "POST" and path == "/api/sessions":
            self.counter += 1
            engine_id = f"eng-{self.counter}"
            self.cwds[engine_id] = payload.get("cwd", "")
            self.live[engine_id] = {
                "id": engine_id,
                "name": payload.get("name", ""),
                "activity": "running",
                "cwd": payload.get("cwd", ""),
                "exit_code": None,
            }
            return 200, json.dumps(self.live[engine_id]).encode()
        if method == "GET" and path == "/api/sessions":
            return 200, json.dumps(list(self.live.values())).encode()
        if method == "POST" and path.endswith("/kill"):
            engine_id = path.rsplit("/", 2)[-2]
            existed = self.live.pop(engine_id, None) is not None
            return (200, b'{"ok":true}') if existed else (404, b"")
        if method == "GET" and path.startswith("/api/history/"):
            engine_id = path.rsplit("/", 1)[-1]
            record = self.archive.get(engine_id)
            return (200, json.dumps(record).encode()) if record else (404, b"")
        if method == "GET" and path == "/api/agent-tasks":
            return 200, json.dumps(self.tasks).encode()
        return 404, b""

    # -- what a test makes true about the engine ---------------------------

    def finish(
        self,
        engine_id: str,
        *,
        exit_code: int | None = 0,
        ended_at: datetime | None = SESSION_END,
        archived: bool = True,
    ) -> None:
        """The process exited. `archived=False` is an engine restarted since."""
        self.live.pop(engine_id, None)
        if not archived:
            return
        self.archive[engine_id] = {
            "id": engine_id,
            "name": engine_id,
            "created_at": "2026-08-12T05:00:00+00:00",
            "ended_at": None if ended_at is None else ended_at.isoformat(),
            "exit_code": exit_code,
            "cwd": self.cwds.get(engine_id, ""),
            "command": None,
            "scrollback_bytes": 0,
        }

    def add_task(
        self,
        *,
        name: str = "Nightly audit",
        project: str | None = None,
        work_item: str | None = None,
        findings: list[dict[str, str]] | None = None,
        status: str = "completed",
        exit_code: int | None = 0,
    ) -> None:
        task: dict[str, Any] = {
            "id": f"task-{len(self.tasks) + 1}",
            "name": name,
            "cwd": None,
            "runs": [
                {
                    "id": f"run-{len(self.tasks) + 1}",
                    "session_id": "eng-task",
                    "started_at": "2026-08-12T05:00:00+00:00",
                    "status": status,
                    "completed_at": (
                        None if status == "running" else "2026-08-12T05:30:00+00:00"
                    ),
                    "exit_code": exit_code,
                    "summary": "Exited successfully",
                    "findings": findings or [],
                }
            ],
        }
        if project is not None:
            task["vogt_project"] = project
        if work_item is not None:
            task["vogt_work_item"] = work_item
        self.tasks.append(task)


class DeadEngine:
    def __call__(
        self,
        url: str,
        headers: dict[str, str],
        body: bytes = b"",
        method: str = "GET",
    ) -> tuple[int, bytes]:
        raise EngineUnavailable("the engine is not answering: connection refused")


def _git(root: Path, *args: str, at: str | None = None) -> None:
    env = None
    if at is not None:
        import os

        env = {**os.environ, "GIT_COMMITTER_DATE": at, "GIT_AUTHOR_DATE": at}
    subprocess.run(
        ["git", "-C", str(root), *args], check=True, capture_output=True, env=env
    )


@pytest.fixture
def repo(tmp_path: Path) -> Path:
    """A real checkout, so the delta is computed and not simulated."""
    root = tmp_path / "estate"
    root.mkdir()
    _git(root, "init", "-q", "-b", "main")
    _git(root, "config", "user.email", "t@example.com")
    _git(root, "config", "user.name", "Test")
    (root / "before.txt").write_text("before\n", encoding="utf-8")
    _git(root, "add", "-A")
    _git(root, "commit", "-qm", "before the session", at="2026-08-01T00:00:00+00:00")
    return root


@pytest.fixture
def engine() -> StandInEngine:
    return StandInEngine()


@pytest.fixture
def wired(instance: AppContext, engine: StandInEngine, repo: Path) -> AppContext:
    ctx = dataclasses.replace(
        instance,
        engine=EngineClient(base_url="http://127.0.0.1:8910", transport=engine),
    )
    register_project(
        ctx, RegisterProjectParams(name="Estate", root_path=str(repo), reason=WHY)
    )
    native_work_item(
        ctx, kind="bug", title="Something to open a terminal on", project="estate"
    )
    return ctx


def _sweep(ctx: AppContext) -> None:
    sweep(ctx, SweepParams(collectors=["session-outcomes"], reason=WHY))


def _outcomes(ctx: AppContext, kind: str = KIND_SESSION_OUTCOME) -> list[Observation]:
    return observations(
        ctx, ObservationsParams(kind=kind, latest_only=True, limit=50)
    ).observations


def _commit_in_window(root: Path, name: str = "during.txt") -> None:
    (root / name).write_text("written by the session\n", encoding="utf-8")
    _git(root, "add", "-A")
    _git(root, "commit", "-qm", "during the session", at=IN_WINDOW)


# -- FR-E6: the outcome of a session that ended ----------------------------


def test_a_finished_session_reports_its_exit_code_and_duration(
    wired: AppContext, engine: StandInEngine
) -> None:
    """The two facts only the engine has, as evidence rather than as columns."""
    started = start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    engine.finish("eng-1", exit_code=0)
    _sweep(wired)

    found = _outcomes(wired)
    assert len(found) == 1
    outcome = found[0]
    assert outcome.subject_key == f"session:{started.session.id}"
    assert outcome.payload["state"] == "finished"
    assert outcome.payload["exit_code"] == 0
    assert outcome.payload["provisional"] is False
    assert outcome.payload["work_item"] == "WI-1"
    # Duration is the process's own lifetime, from the engine's archive —
    # 05:00 to 06:00 — not the gap between Vogt's two writes.
    assert outcome.payload["duration_seconds"] == 3600


def test_a_nonzero_exit_is_recorded_as_itself(
    wired: AppContext, engine: StandInEngine
) -> None:
    start_session(wired, StartSessionParams(project="estate", reason=WHY))
    engine.finish("eng-1", exit_code=137)
    _sweep(wired)
    assert _outcomes(wired)[0].payload["exit_code"] == 137


# -- FR-E6 + FR-U17: a session that has not finished -----------------------


def test_a_running_session_has_no_outcome_and_says_which(wired: AppContext) -> None:
    """The rule this collector exists to keep.

    An outcome row with a null exit code cannot be told apart from one whose
    exit code was lost. A running session is therefore recorded as running,
    provisionally (FR-U17), with no exit code and no duration at all.
    """
    start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    _sweep(wired)

    payload = _outcomes(wired)[0].payload
    assert payload["state"] == "running"
    assert payload["provisional"] is True
    assert "exit_code" not in payload
    assert "duration_seconds" not in payload


def test_a_running_session_does_not_record_what_it_is_doing(
    wired: AppContext,
) -> None:
    """No cached liveness: activity is the engine's, asked for when needed.

    `SCHEMA.md` §2.6 forbids a stored activity column for exactly this
    reason, and an observation is a stored column with a timestamp on it.
    """
    start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    _sweep(wired)
    assert "activity" not in _outcomes(wired)[0].payload


def test_the_outcome_becomes_final_once_the_session_ends(
    wired: AppContext, engine: StandInEngine
) -> None:
    start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    _sweep(wired)
    assert _outcomes(wired)[0].payload["state"] == "running"

    engine.finish("eng-1", exit_code=0)
    _sweep(wired)

    final = _outcomes(wired)
    assert len(final) == 1, "one subject key, so the latest row is the outcome"
    assert final[0].payload["state"] == "finished"


def test_a_session_the_engine_has_forgotten_is_unknown_not_clean(
    wired: AppContext, engine: StandInEngine
) -> None:
    """An engine restarted since costs the outcome, and says so by name."""
    start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    engine.finish("eng-1", archived=False)
    _sweep(wired)

    payload = _outcomes(wired)[0].payload
    assert payload["state"] == "unknown"
    assert "exit_code" not in payload
    assert "not collected" in str(payload["detail"])


def test_vogt_stopping_a_session_is_not_an_exit_code(
    wired: AppContext, engine: StandInEngine
) -> None:
    """`stopped_at` says Vogt asked; it never says how the process ended."""
    started = start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    engine.finish("eng-1", archived=False)
    stop_session(wired, StopSessionParams(id=started.session.id, reason=WHY))
    _sweep(wired)

    payload = _outcomes(wired)[0].payload
    assert "exit_code" not in payload
    assert payload["state"] == "unknown"
    # The end time it does have is Vogt's, and is used only for the window.
    assert str(payload["ended_at"]).startswith("2026-08-12")
    assert "Vogt's start" in str(payload["duration_source"])


# -- FR-E6: the working-tree delta -----------------------------------------


def test_the_delta_counts_what_the_tree_recorded_in_the_window(
    wired: AppContext, engine: StandInEngine, repo: Path
) -> None:
    start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    _commit_in_window(repo)
    engine.finish("eng-1", exit_code=0)
    _sweep(wired)

    delta = _outcomes(wired)[0].payload["delta"]
    assert isinstance(delta, dict)
    assert delta["is_git_repository"] is True
    assert delta["commits_in_window"] == 1
    assert delta["files_changed"] == 1
    assert delta["insertions"] == 1
    assert delta["attributed_to_the_session"] is False, (
        "the delta is a window over one tree, not a claim about who caused it"
    )


def test_commits_outside_the_window_are_not_the_sessions(
    wired: AppContext, engine: StandInEngine, repo: Path
) -> None:
    """The commit made before the session started must not be counted."""
    start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    engine.finish("eng-1", exit_code=0)
    _sweep(wired)

    delta = _outcomes(wired)[0].payload["delta"]
    assert isinstance(delta, dict)
    assert delta["commits_in_window"] == 0


def test_uncommitted_work_is_recorded_while_the_session_is_running(
    wired: AppContext, engine: StandInEngine, repo: Path
) -> None:
    """Where the dirty tree is knowable, and where it stops being knowable.

    While the session runs, what it has not committed is part of what it has
    done so far. Once it has ended, "dirty now" is somebody else's edit as
    easily as the session's, and re-reading it every sweep would write a new
    evidence row every sweep — so the final outcome carries the committed
    record and says that it does.
    """
    start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    (repo / "scratch.txt").write_text("half-done\n", encoding="utf-8")
    _sweep(wired)

    running = _outcomes(wired)[0].payload["delta"]
    assert isinstance(running, dict)
    assert running["uncommitted_files"] == 1

    engine.finish("eng-1", exit_code=0)
    _sweep(wired)

    finished = _outcomes(wired)[0].payload["delta"]
    assert isinstance(finished, dict)
    assert "uncommitted_files" not in finished
    assert "only observable while the session runs" in str(finished["detail"])


def test_an_ended_session_with_no_end_time_counts_no_commits(
    wired: AppContext, engine: StandInEngine, repo: Path
) -> None:
    """An open `--since` would attribute next month's work to this session."""
    start_session(wired, StartSessionParams(project="estate", reason=WHY))
    _commit_in_window(repo)
    engine.finish("eng-1", exit_code=0, ended_at=None)
    _sweep(wired)

    payload = _outcomes(wired)[0].payload
    delta = payload["delta"]
    assert isinstance(delta, dict)
    assert payload["exit_code"] == 0, "the exit code is known even so"
    assert "commits_in_window" not in delta
    assert "no commit counts" in str(delta["detail"])


def test_a_session_whose_tree_is_gone_says_so(
    wired: AppContext, engine: StandInEngine, repo: Path
) -> None:
    import shutil

    start_session(wired, StartSessionParams(project="estate", reason=WHY))
    engine.finish("eng-1", exit_code=0)
    shutil.rmtree(repo)
    _sweep(wired)

    delta = _outcomes(wired)[0].payload["delta"]
    assert isinstance(delta, dict)
    assert delta["is_git_repository"] is False
    assert "no longer exists" in str(delta["detail"])


# -- the observation is evidence, with everything that implies -------------


def test_an_outcome_is_collected_and_never_declared(
    wired: AppContext, engine: StandInEngine
) -> None:
    """FR-O2: the outcome arrives as evidence, and audits nothing."""
    start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    engine.finish("eng-1", exit_code=0)
    with wired.declared.read() as view:
        before = len(view.list_audit(limit=200))

    _sweep(wired)

    with wired.declared.read() as view:
        after = len(view.list_audit(limit=200))
        session = view.list_sessions(include_stopped=True, limit=10, offset=0)[0]
    assert after == before, "a sweep must not audit anything"
    assert not hasattr(session, "exit_code"), (
        "the outcome is an observation; the declared row keeps no copy"
    )


def test_an_unchanged_outcome_does_not_grow_the_store(
    wired: AppContext, engine: StandInEngine
) -> None:
    """FR-O7: a finished session is re-observed forever and written once."""
    start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    engine.finish("eng-1", exit_code=0)
    _sweep(wired)
    _sweep(wired)
    _sweep(wired)
    assert wired.observed.counts()["observations"] == 1


def test_the_outcome_carries_the_sweeps_coverage(
    wired: AppContext, engine: StandInEngine
) -> None:
    """FR-O3: freshness is the sweep record, the same as every other kind."""
    start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    engine.finish("eng-1", exit_code=0)
    _sweep(wired)

    covered = coverage(wired, CoverageParams()).collectors
    entries = {row.collector: row for row in covered}
    assert entries["session-outcomes"].status == "ok"
    assert entries["session-outcomes"].last_swept_at is not None


def test_without_an_engine_there_is_no_outcome_collector(
    instance: AppContext,
) -> None:
    """No engine is "not collected", not "no outcomes"."""
    assert "session-outcomes" not in collector_registry(instance).names


def test_an_engine_that_cannot_be_asked_makes_the_sweep_fail_loudly(
    wired: AppContext, repo: Path
) -> None:
    dead = EngineClient(base_url="http://127.0.0.1:8910", transport=DeadEngine())
    ctx = dataclasses.replace(wired, engine=dead)
    result = sweep(ctx, SweepParams(collectors=["session-outcomes"], reason=WHY))
    report = result.reports[0]
    assert report.outcome == "failed"
    assert "not answering" in str(report.failures["estate"])


def test_an_offline_sweep_does_not_reach_for_the_engine(wired: AppContext) -> None:
    """NFR-PO2: the forge-less layer must not need a second process running."""
    result = sweep(wired, SweepParams(offline_only=True, reason=WHY))
    assert "session-outcomes" not in {report.collector for report in result.reports}


# -- FR-E7: a bound agent task's run ---------------------------------------


def test_a_bound_runs_findings_become_observations(
    wired: AppContext, engine: StandInEngine
) -> None:
    """The requirement's own words: not only as push notifications."""
    engine.add_task(
        project="estate",
        findings=[
            {
                "at": "2026-08-12T05:20:00+00:00",
                "text": "three dependencies are unresolved",
                "source": "notify-phrase",
            }
        ],
    )
    _sweep(wired)

    found = _outcomes(wired, KIND_TASK_RUN)
    assert len(found) == 1
    payload = found[0].payload
    assert found[0].subject_key == "task-run:run-1"
    assert payload["exit_code"] == 0
    assert payload["duration_seconds"] == 1800
    findings = payload["findings"]
    assert isinstance(findings, list)
    assert findings[0]["text"] == "three dependencies are unresolved"


def test_an_unbound_task_is_not_vogts_business(
    wired: AppContext, engine: StandInEngine
) -> None:
    """A task nobody bound is the engine's own; Vogt records nothing of it."""
    engine.add_task()
    _sweep(wired)
    assert _outcomes(wired, KIND_TASK_RUN) == []


def test_a_task_bound_to_a_work_item_lands_on_that_items_project(
    wired: AppContext, engine: StandInEngine
) -> None:
    engine.add_task(work_item="WI-1")
    _sweep(wired)

    found = _outcomes(wired, KIND_TASK_RUN)
    assert len(found) == 1
    assert found[0].payload["work_item"] == "WI-1"
    with wired.declared.read() as view:
        project = view.project_by_slug("estate")
    assert project is not None
    assert found[0].project_id == project.id, (
        "an observation is scoped to a project, which is what makes coverage "
        "answerable at all"
    )


def test_a_binding_naming_something_this_instance_lacks_is_ignored(
    wired: AppContext, engine: StandInEngine
) -> None:
    engine.add_task(work_item="WI-404")
    engine.add_task(project="not-registered")
    _sweep(wired)
    assert _outcomes(wired, KIND_TASK_RUN) == []


def test_a_running_bound_run_is_provisional_too(
    wired: AppContext, engine: StandInEngine
) -> None:
    engine.add_task(project="estate", status="running", exit_code=None)
    _sweep(wired)

    payload = _outcomes(wired, KIND_TASK_RUN)[0].payload
    assert payload["state"] == "running"
    assert payload["provisional"] is True
    assert "exit_code" not in payload
    assert "duration_seconds" not in payload


def test_the_two_kinds_are_queryable_apart(
    wired: AppContext, engine: StandInEngine
) -> None:
    start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    engine.finish("eng-1", exit_code=0)
    engine.add_task(project="estate")
    _sweep(wired)

    assert len(_outcomes(wired, KIND_SESSION_OUTCOME)) == 1
    assert len(_outcomes(wired, KIND_TASK_RUN)) == 1


def test_the_window_of_a_run_survives_a_clock_that_moves(
    wired: AppContext, engine: StandInEngine, repo: Path
) -> None:
    """A finished outcome's payload is stable, which is what dedup rests on."""
    start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    _commit_in_window(repo)
    engine.finish("eng-1", exit_code=0)
    _sweep(wired)
    first = _outcomes(wired)[0].content_digest

    # Time passes, and something else happens in the tree afterwards.
    (repo / "later.txt").write_text("unrelated\n", encoding="utf-8")
    _git(repo, "add", "-A")
    _git(
        repo,
        "commit",
        "-qm",
        "long after the session",
        at=(SESSION_END + timedelta(days=1)).isoformat(),
    )
    _sweep(wired)

    outcome = _outcomes(wired)[0]
    assert outcome.payload["delta"]["commits_in_window"] == 1  # type: ignore[index]
    assert outcome.content_digest == first, (
        "a closed window's answer cannot change; only the tree's uncommitted "
        "state can, and that commit left none"
    )
