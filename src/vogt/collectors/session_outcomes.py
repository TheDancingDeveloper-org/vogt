"""`session-outcomes` — what a run left behind (FR-E6, FR-E7).

Two questions, one collector, because they have one source and they fail
together. *What did this session end up doing?* and *what did this bound
agent task's run find?* are both answered by asking the engine and looking at
the tree the run happened in, and an engine that cannot be asked makes both
answers "not collected" at the same moment. One coverage row that says so is
more honest than two that say it twice (FR-O3).

Three decisions worth knowing about before reading the code.

**An outcome is evidence, not a column.** Nothing here writes the declared
store, and nothing adds a field to `coding_sessions`. `SCHEMA.md` §2.6 says
what Vogt *asked for* lives in the declared row and what *happened in there*
is observed, and this module is the half of that sentence that was missing:
the outcome arrives as an observation, with the subject key, digest dedup,
sweep coverage, freshness and trust that every other piece of evidence has.

**A session that has not finished has no outcome, and says so.** The state
is `running`, there is no exit code and no duration, and the finding carries
`provisional: true` — FR-U17's rule, written into the evidence rather than
left to each surface to remember. The alternative, an outcome row with a
null exit code, is indistinguishable from "it exited and we lost the code",
which is the confusion this product exists to prevent.

**The working-tree delta is a window, not an attribution.** Vogt does not
snapshot the tree when a session starts, so the delta is what the checkout
recorded between the session's start and its end — every commit in that
window, whoever made it. Two agents in one tree, or a person committing while
a session ran, land in the same numbers. The payload says so in
`attributed_to_the_session: false` rather than implying a precision that is
not there, and a window with no known end carries no commit counts at all:
an open-ended `--since` would quietly attribute next month's work to a
session that ended in August. `_delta` documents the three shapes and why a
finished session's delta stops looking at the tree.
"""

from __future__ import annotations

from collections.abc import Iterable, Sequence
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path
from typing import Protocol

from vogt.adapters.engine import (
    EngineAgentTask,
    EngineArchivedSession,
    EngineClient,
    EngineSession,
    EngineTaskRun,
)
from vogt.collectors.base import CollectorContext, Finding, finding
from vogt.collectors.git_local import git_output
from vogt.core.entities import Project

KIND_SESSION_OUTCOME = "session.outcome"
KIND_TASK_RUN = "agent_task.run"

#: How many of a project's sessions are re-observed on each sweep, newest
#: first. A cap is needed because every session costs an engine call; 200 is
#: chosen to be far above any real project's live history. Sessions past it
#: keep the observations they already have — retention rule 1 keeps the
#: latest row per subject indefinitely (`SCHEMA.md` §5) — they simply stop
#: being re-asked about, which for a finished session changes nothing.
SESSION_OBSERVATION_LIMIT = 200

#: Upper bound on the commits a delta will count. A session's window is
#: hours; a repository where this bites has had something else happen in it,
#: and the finding says `truncated` rather than reporting a number it did
#: not finish computing.
MAX_COMMITS_COUNTED = 500


@dataclass(frozen=True)
class SessionRecord:
    """What Vogt declared about one session, flattened for the collector.

    Deliberately not `CodingSession`: collectors never touch declared data
    (FR-O2), so the application layer reads the row, resolves the work-item
    ref, and hands over the four facts an outcome needs. A collector holding
    a store handle is how that rule gets broken by accident.
    """

    id: str
    engine_session_id: str
    cwd: str
    started_at: datetime
    stopped_at: datetime | None = None
    work_item: str | None = None


class DeclaredSessions(Protocol):
    """The declared reads this collector needs, done for it."""

    def for_project(self, project: Project) -> Sequence[SessionRecord]:
        """This project's sessions, newest first."""
        ...

    def project_of_work_item(self, ref: str) -> str | None:
        """The slug of the project a work-item ref belongs to, if any."""
        ...


class SessionOutcomeCollector:
    """Session outcomes and bound agent-task runs, as observations."""

    def __init__(self, engine: EngineClient, sessions: DeclaredSessions) -> None:
        self._engine = engine
        self._sessions = sessions
        # One snapshot of the engine per sweep, not per project. Both caches
        # live on the instance because `collector_registry` builds a fresh
        # one for each sweep; a longer-lived cache would be exactly the
        # stale copy of somebody else's running state that FR-E2 forbids.
        self._live: dict[str, EngineSession] | None = None
        self._tasks: list[EngineAgentTask] | None = None

    @property
    def name(self) -> str:
        return "session-outcomes"

    @property
    def requires_network(self) -> bool:
        # The engine is usually a loopback address in the same pod, but it is
        # still another process over HTTP, and the forge-less layer (NFR-PO2)
        # must not need one running. `True` keeps it out of an offline sweep.
        return True

    def collect(self, ctx: CollectorContext, project: Project) -> Iterable[Finding]:
        del ctx
        yield from self._session_outcomes(project)
        yield from self._bound_task_runs(project)

    # -- FR-E6: what a session left behind ---------------------------------

    def _session_outcomes(self, project: Project) -> Iterable[Finding]:
        for record in self._sessions.for_project(project):
            yield self._outcome_of(project, record)

    def _outcome_of(self, project: Project, record: SessionRecord) -> Finding:
        live = self._live_sessions().get(record.engine_session_id)
        running = live is not None and live.exit_code is None

        archived: EngineArchivedSession | None = None
        if not running:
            archived = self._engine.archived_session(record.engine_session_id)

        payload: dict[str, object] = {
            "session": record.id,
            "engine_session_id": record.engine_session_id,
            "project": project.slug,
            "work_item": record.work_item,
            "cwd": record.cwd,
            "started_at": record.started_at.isoformat(),
        }

        if running:
            payload["state"] = "running"
            # FR-U17: a claim backed by a still-running session is
            # provisional. Note what is *not* here — the engine's activity
            # state. An observation is a timestamped copy, and copying live
            # activity into one is the cached-liveness mistake `SCHEMA.md`
            # §2.6 rules out; this says only "it had not finished when we
            # looked", which is still true a second later.
            payload["provisional"] = True
            payload["delta"] = _delta(
                record.cwd, since=record.started_at, until=None, still_open=True
            )
            return _finding(project, record.id, payload)

        end = _ended_at(archived) or record.stopped_at
        exit_code = archived.exit_code if archived is not None else None
        if exit_code is None and live is not None:
            exit_code = live.exit_code

        if archived is None and live is None:
            payload["state"] = "unknown"
            payload["provisional"] = False
            payload["detail"] = (
                "the engine no longer has this session and has no archive of "
                "it, so how it ended is not collected"
            )
        else:
            payload["state"] = "finished"
            payload["provisional"] = False
            if exit_code is None:
                payload["detail"] = (
                    "the session has ended and the engine did not record an "
                    "exit code for it"
                )

        if exit_code is not None:
            payload["exit_code"] = exit_code
        if end is not None:
            payload["ended_at"] = end.isoformat()
            payload.update(_duration_of(record, archived, end))
        payload["delta"] = _delta(
            record.cwd, since=record.started_at, until=end, still_open=False
        )
        return _finding(project, record.id, payload)

    def _live_sessions(self) -> dict[str, EngineSession]:
        if self._live is None:
            self._live = {row.id: row for row in self._engine.list_sessions()}
        return self._live

    # -- FR-E7: what a bound task's run found ------------------------------

    def _bound_task_runs(self, project: Project) -> Iterable[Finding]:
        """Runs of tasks bound to this project, findings and all.

        No working-tree delta here, unlike a session: an agent task's `cwd`
        is the task author's, not the project registry's, so a delta computed
        against it would be a claim about a tree Vogt never chose. The
        binding says which subject the run is *about*; it does not say the
        run happened in that subject's checkout.
        """
        for task in self._bound_tasks():
            if not self._binds(task, project):
                continue
            for run in task.runs:
                yield _task_run_finding(project, task, run)

    def _bound_tasks(self) -> list[EngineAgentTask]:
        if self._tasks is None:
            self._tasks = [
                task for task in self._engine.list_agent_tasks() if task.is_bound
            ]
        return self._tasks

    def _binds(self, task: EngineAgentTask, project: Project) -> bool:
        if task.project is not None and task.project.strip().lower() == project.slug:
            return True
        if task.work_item is None:
            return False
        # A task bound to a work item is bound to that item's project too —
        # every work item that can host a session has one (FR-E3), and
        # scoping an observation to a project is what makes coverage
        # answerable (`SCHEMA.md` §4).
        return self._sessions.project_of_work_item(task.work_item) == project.slug


def _finding(project: Project, session_id: str, payload: dict[str, object]) -> Finding:
    return finding(
        kind=KIND_SESSION_OUTCOME,
        subject_key=f"session:{session_id}",
        project=project,
        payload=payload,
    )


def _task_run_finding(
    project: Project, task: EngineAgentTask, run: EngineTaskRun
) -> Finding:
    running = run.status == "running"
    payload: dict[str, object] = {
        "task": task.name,
        "task_id": task.id,
        "run": run.id,
        "session_id": run.session_id,
        "project": project.slug,
        "work_item": task.work_item,
        "started_at": run.started_at,
        "state": "running" if running else "finished",
        "status": run.status,
        "provisional": running,
        "findings": [
            {"at": item.at, "text": item.text, "source": item.source}
            for item in run.findings
        ],
    }
    if run.summary is not None:
        payload["summary"] = run.summary
    if run.exit_code is not None:
        payload["exit_code"] = run.exit_code
    if run.completed_at is not None:
        payload["completed_at"] = run.completed_at
    duration = _duration(run.started_at, run.completed_at)
    if duration is not None:
        payload["duration_seconds"] = duration
    return finding(
        kind=KIND_TASK_RUN,
        subject_key=f"task-run:{run.id}",
        project=project,
        payload=payload,
    )


# -- the working-tree delta ------------------------------------------------


def _delta(
    cwd: str, *, since: datetime, until: datetime | None, still_open: bool
) -> dict[str, object]:
    """What the tree the run happened in has to say about that window.

    Three shapes, and the difference between them is the difference between
    a fact and a number that keeps moving.

    **An open window** — the session is still running — reports the tree as
    it stands: current HEAD, how many files are uncommitted, and everything
    committed since the session started. All of it is provisional, and all of
    it is allowed to change, because the session is still doing it.

    **A closed window** — the session ended and something recorded when —
    reports only what the window's commits say, and deliberately *not* what
    is uncommitted now. A finished session's outcome must not change every
    time somebody edits the tree afterwards: that is a new evidence row per
    sweep for as long as the checkout is dirty, which is growth proportional
    to how often we look rather than to what changed (FR-O7, NFR-S2). What a
    session left uncommitted is knowable only while it runs, and the
    provisional row above is where it is kept.

    **No end and not open** — the session ended and nobody recorded when —
    reports no counts at all. An open `--since` here would sweep in next
    month's commits and call them this session's.
    """
    root = Path(cwd).expanduser()
    if not root.is_dir():
        return {
            "is_git_repository": False,
            "detail": "the path the run happened in no longer exists",
        }
    if not (root / ".git").exists():
        return {
            "is_git_repository": False,
            "detail": "the path the run happened in is not a git checkout",
        }

    delta: dict[str, object] = {
        "is_git_repository": True,
        "attributed_to_the_session": False,
    }
    if until is None and not still_open:
        delta["detail"] = (
            "no commit counts: nothing recorded when this session ended, so "
            "the window has no end and any number would include later work"
        )
        return delta

    window = ["--since", since.isoformat()]
    if until is not None:
        window += ["--until", until.isoformat()]
    delta["window"] = {
        "from": since.isoformat(),
        "to": None if until is None else until.isoformat(),
    }
    delta.update(_commit_stats(root, window))

    if still_open:
        # Same rule as `git-local`: `uncommitted_files: 0` is a claim, and a
        # status that could not be read must not become one (#20).
        status = git_output(root, "status", "--porcelain", required=True)
        delta["head"] = git_output(root, "rev-parse", "HEAD") or None
        delta["uncommitted_files"] = 0 if not status else len(status.splitlines())
        delta["detail"] = (
            "the tree as it stands, and every commit reachable from HEAD "
            "since this session started, whoever made it"
        )
        return delta

    delta["head_at_window_end"] = (
        git_output(root, "log", "-1", "--format=%H", *window) or None
    )
    delta["detail"] = (
        "every commit reachable from HEAD whose committer date falls in the "
        "window, whoever made it. What the session left uncommitted is not "
        "here: it is only observable while the session runs"
    )
    return delta


def _commit_stats(root: Path, window: list[str]) -> dict[str, object]:
    cap = f"--max-count={MAX_COMMITS_COUNTED}"
    revisions = git_output(root, "log", cap, "--format=%H", *window)
    commits = len(revisions.splitlines()) if revisions else 0

    files: set[str] = set()
    insertions = 0
    deletions = 0
    numstat = git_output(root, "log", cap, "--format=", "--numstat", *window)
    for line in numstat.splitlines():
        parts = line.split("\t")
        if len(parts) != 3:
            continue
        added, removed, path = parts
        files.add(path)
        # `-` is git's way of saying "binary": the file changed and the line
        # counts do not apply. Counting it as zero lines is right; dropping
        # the file would under-report what the run touched.
        insertions += int(added) if added.isdigit() else 0
        deletions += int(removed) if removed.isdigit() else 0

    stats: dict[str, object] = {
        "commits_in_window": commits,
        "files_changed": len(files),
        "insertions": insertions,
        "deletions": deletions,
    }
    if commits >= MAX_COMMITS_COUNTED:
        stats["truncated"] = True
    return stats


def _duration_of(
    record: SessionRecord, archived: EngineArchivedSession | None, end: datetime
) -> dict[str, object]:
    """How long the process ran, and by whose clock.

    Both of the engine's timestamps or neither: subtracting the engine's end
    from Vogt's start is a difference between two machines' clocks as much as
    a duration, and a number that is silently either one is worse than a
    number that says which it is. `duration_source` is that disclosure.
    """
    if archived is not None:
        began = _parse(archived.created_at)
        if began is not None:
            return {
                "duration_seconds": max(0, int((end - began).total_seconds())),
                "duration_source": "the engine's archive of the process",
            }
    return {
        "duration_seconds": max(0, int((end - record.started_at).total_seconds())),
        "duration_source": (
            "Vogt's start to the end it recorded; the engine has no archive "
            "of this session, so the two ends of it are two clocks"
        ),
    }


def _ended_at(archived: EngineArchivedSession | None) -> datetime | None:
    if archived is None or archived.ended_at is None:
        return None
    return _parse(archived.ended_at)


def _duration(started_at: str, completed_at: str | None) -> int | None:
    if completed_at is None:
        return None
    start = _parse(started_at)
    end = _parse(completed_at)
    if start is None or end is None:
        return None
    return max(0, int((end - start).total_seconds()))


def _parse(value: str) -> datetime | None:
    """Read one RFC 3339 timestamp from the engine, or give up quietly.

    The engine formats its own times and Vogt does not validate them: a
    timestamp it cannot read costs the duration on one observation, which is
    reported as absent, and must never cost the sweep.
    """
    try:
        return datetime.fromisoformat(value)
    except ValueError:
        return None


__all__ = [
    "KIND_SESSION_OUTCOME",
    "KIND_TASK_RUN",
    "SESSION_OBSERVATION_LIMIT",
    "DeclaredSessions",
    "SessionOutcomeCollector",
    "SessionRecord",
]
