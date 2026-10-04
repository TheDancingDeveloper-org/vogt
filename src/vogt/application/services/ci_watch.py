"""Watching CI on the branches work items are bound to (WI-855).

Agents polled CI because nothing told them when a run finished. A branch
bound to a work item (`work.bind_branch`, or `session.start`'s automatic
binding) is now watched: the `forge-checks` sweep observes its runs, and
once every workflow on the branch's newest revision has concluded, this
module

- raises an Inbox entry (`ci.branch_concluded`, built by `inbox.py` from
  `bound_branches`), and
- after the sweep, publishes one `ci.branch_concluded` event on the
  declared feed and types a one-line notice into each live session started
  for the item, so the agent wakes on the result.

"Once" is the event: its entity id names the item, branch, revision and
verdict, and a conclusion that already has an event is never re-announced —
not by the next sweep, not by a restart. Only *fresh* conclusions (the last
run finished within `FRESH_WINDOW`) are announced, so binding an old branch,
or re-keying observations, never floods a session with history.
"""

from __future__ import annotations

from collections.abc import Iterable, Mapping
from dataclasses import dataclass
from datetime import datetime, timedelta

from vogt.adapters.engine import EngineUnavailable
from vogt.adapters.forge.kinds import KIND_CHECK
from vogt.application.context import AppContext
from vogt.core.ci_alerts import BranchCi, branch_ci
from vogt.core.entities import Observation
from vogt.core.workflow import TERMINAL_STATES
from vogt.observability import logger
from vogt.storage.interface import ReadView

CI_BRANCH_CONCLUDED_EVENT = "ci.branch_concluded"
#: How recent a conclusion must be to be announced to a session.
FRESH_WINDOW = timedelta(hours=6)
#: Bound overlays considered per read. A work item binds a handful of
#: branches; an estate with more than this many open bound items has the
#: newest ones watched and the rest waiting, never an unbounded scan.
MAX_BOUND = 500
#: Live sessions nudged per conclusion.
MAX_SESSIONS = 10
NOTICE_MAX_CHARS = 400

_log = logger("ci_watch")


@dataclass(frozen=True)
class BoundBranch:
    """One branch a still-open work item is bound to, and its CI."""

    work_item_ref: str
    work_item_id: str | None
    project_id: str
    branch: str
    ci: BranchCi

    @property
    def event_entity(self) -> str:
        return f"{self.work_item_ref}:{self.branch}@{self.ci.revision}:{self.ci.state}"


def bound_branches(
    view: ReadView,
    checks_by_branch: Mapping[tuple[str, str], list[Observation]],
) -> list[BoundBranch]:
    """CI on the newest revision of every branch an open item is bound to.

    `checks_by_branch` is keyed `(project_id, branch)` — built once from the
    checks the caller already holds — so nothing here re-reads observations
    per branch (the 0.5.4 Inbox lesson). The item's state is looked up only
    for an overlay with observed runs, so a hundred bound branches nobody
    pushed cost one query, not a hundred and one."""
    found: list[BoundBranch] = []
    for overlay in view.bound_branch_overlays(limit=MAX_BOUND):
        settled: list[tuple[str, BranchCi]] = []
        for branch in overlay.branches:
            runs = checks_by_branch.get((overlay.project_id, branch))
            ci = None if not runs else branch_ci(runs, branch)
            if ci is not None:
                settled.append((branch, ci))
        if not settled:
            continue
        item = view.work_item_by_ref(overlay.subject_key)
        state = item.state if item is not None else overlay.workflow_state
        if state in TERMINAL_STATES:
            continue
        found.extend(
            BoundBranch(
                work_item_ref=overlay.subject_key,
                work_item_id=None if item is None else item.id,
                project_id=overlay.project_id,
                branch=branch,
                ci=ci,
            )
            for branch, ci in settled
        )
    return found


def index_by_branch(
    checks: Iterable[Observation],
) -> dict[tuple[str, str], list[Observation]]:
    """Check observations keyed `(project_id, branch)`."""
    index: dict[tuple[str, str], list[Observation]] = {}
    for check in checks:
        branch = check.payload.get("branch")
        if check.project_id is None or not isinstance(branch, str) or not branch:
            continue
        index.setdefault((check.project_id, branch), []).append(check)
    return index


def notice_text(bound: BoundBranch) -> str:
    """The one line typed into a bound session. Plain text, no control
    characters: it is data for the agent, not a command."""
    ci = bound.ci
    verdict = {"passed": "PASSED", "failed": "FAILED", "cancelled": "CANCELLED"}.get(
        ci.state, ci.state.upper()
    )
    detail = (
        f"; failing: {', '.join(ci.failing)}"
        if ci.failing
        else f"; {len(ci.runs)} workflow(s)"
    )
    link = next(
        (
            run.observation.source_url
            for run in ci.runs
            if run.workflow in ci.failing and run.observation.source_url
        ),
        None,
    )
    tail = f" — {link}" if link else ""
    line = (
        f"[vogt] CI {verdict} on {bound.branch} @ {ci.revision[:12]} "
        f"({bound.work_item_ref}){detail}{tail}"
    )
    # Workflow names and URLs come from the forge: strip every control
    # character (an Esc or a carriage return would be a keystroke, not text)
    # and bound the length, so the line can only ever be read, never act.
    return "".join(ch for ch in line if ch.isprintable())[:NOTICE_MAX_CHARS]


def announce_concluded(ctx: AppContext) -> int:
    """Announce fresh, not-yet-announced bound-branch conclusions.

    Called after a sweep's projections are rebuilt. Returns how many
    conclusions were announced. Never raises for an engine problem: the
    Inbox entry already carries the fact, and a sweep must not fail because
    a terminal could not be typed into.
    """
    now = ctx.clock()
    with ctx.declared.read() as view:
        projects = {
            overlay.project_id
            for overlay in view.bound_branch_overlays(limit=MAX_BOUND)
        }
        if not projects:
            return 0
        checks: list[Observation] = []
        for project_id in sorted(projects):
            checks.extend(
                ctx.observed.latest(
                    kinds=(KIND_CHECK,), project_id=project_id, limit=2000
                )
            )
        found = bound_branches(view, index_by_branch(checks))
    announced = 0
    for bound in found:
        if not bound.ci.settled or not _fresh(bound.ci.concluded_at, now):
            continue
        with ctx.declared.read() as view:
            if view.list_events(after=0, limit=1, entity_id=bound.event_entity):
                continue
            sessions = (
                []
                if bound.work_item_id is None
                else view.list_sessions(
                    work_item_id=bound.work_item_id,
                    include_stopped=False,
                    limit=MAX_SESSIONS,
                    offset=0,
                )
            )
        nudged = _nudge(ctx, [s.engine_session_id for s in sessions], bound)
        ctx.declared.publish_event(
            kind=CI_BRANCH_CONCLUDED_EVENT,
            entity_kind="work_item_branch",
            entity_id=bound.event_entity,
            summary={
                "work_item": bound.work_item_ref,
                "branch": bound.branch,
                "revision": bound.ci.revision,
                "state": bound.ci.state,
                "failing": list(bound.ci.failing),
                "sessions_notified": nudged,
            },
            at=ctx.clock(),
        )
        announced += 1
    return announced


def _nudge(ctx: AppContext, engine_ids: list[str], bound: BoundBranch) -> int:
    if ctx.engine is None or not ctx.config.ci_watch_notify_sessions:
        return 0
    text = notice_text(bound)
    sent = 0
    for engine_id in engine_ids:
        try:
            if ctx.engine.send_input(engine_id, text) and ctx.engine.send_input(
                engine_id, "", submit=True
            ):
                sent += 1
        except EngineUnavailable as exc:
            _log.warning(
                "could not notify a bound session of a CI conclusion",
                extra={"vogt": {"session": engine_id, "error": str(exc)}},
            )
    return sent


def _fresh(concluded_at: str | None, now: datetime) -> bool:
    if concluded_at is None:
        return False
    try:
        moment = datetime.fromisoformat(concluded_at.replace("Z", "+00:00"))
    except ValueError:
        return False
    if moment.tzinfo is None:
        return False
    return moment >= now - FRESH_WINDOW


__all__ = [
    "CI_BRANCH_CONCLUDED_EVENT",
    "BoundBranch",
    "announce_concluded",
    "bound_branches",
    "index_by_branch",
    "notice_text",
]
