"""What the core does after the engine stops a session at its budget (WI-1833).

The engine's soft warning asks the agent to commit, push and write a
structured hand-off (`work_comment kind=handoff`) before the hard limit
hibernates it with trigger `budget`. An agent that ran out mid-loop often
never wrote one, and the item then had to be re-discovered by hand.

`settle_budget_stops` closes that gap, once per budget-stopped session:

1. If the item has no hand-off written since the session started, it writes
   a minimal one from the session's git state — branch, head and the dirty
   files — so `session_start(from_handoff=…)` always has something to start
   from.
2. If the deployment's `dispatch.budget_restarts_per_item` cap (default 1)
   allows it, and nothing has picked the item up since, it starts a fresh
   worker from that hand-off with the stopped session's purpose (or its
   template and model when it named no purpose). Each restart leaves a note
   on the item; the notes are the count the cap is read against.

Idempotent by construction: a second pass finds the hand-off, and either the
restart note or the newer session, and does nothing. It runs after every
scheduled sweep.
"""

from __future__ import annotations

import logging
import subprocess
from dataclasses import dataclass, field
from pathlib import Path

from vogt.adapters.engine import EngineUnavailable
from vogt.application.context import AppContext
from vogt.application.models import CommentParams, StartSessionParams
from vogt.application.services.preferences import dispatch_settings
from vogt.core.entities import CodingSession, HandoffFields
from vogt.errors import VogtError

logger = logging.getLogger(__name__)

#: Every automatic restart's note starts with this; the cap counts them.
RESTART_NOTE = "[vogt] budget restart:"
REASON = "budget stop: keep the item's hand-off and continue it (WI-1833)"
#: Dirty files listed on a git-state hand-off, at most.
_DIRTY_LIMIT = 50
_GIT_TIMEOUT_S = 10


@dataclass
class BudgetSettlement:
    """What one pass did, by work item ref."""

    handoffs_written: list[str] = field(default_factory=list)
    restarted: list[str] = field(default_factory=list)


@dataclass(frozen=True)
class GitState:
    branch: str | None
    head_sha: str | None
    dirty: list[str]


def _git(cwd: Path, *args: str) -> str | None:
    try:
        done = subprocess.run(
            ["git", *args],
            cwd=cwd,
            capture_output=True,
            text=True,
            timeout=_GIT_TIMEOUT_S,
            check=False,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    if done.returncode != 0:
        return None
    return done.stdout


def read_git_state(cwd: str) -> GitState | None:
    """The checkout's branch, head and dirty paths; None when it is not one."""
    path = Path(cwd)
    if not cwd or not path.is_dir():
        return None
    head = _git(path, "rev-parse", "HEAD")
    if head is None:
        return None
    branch = (_git(path, "rev-parse", "--abbrev-ref", "HEAD") or "").strip()
    status = _git(path, "status", "--porcelain") or ""
    dirty = [line[3:] for line in status.splitlines() if len(line) > 3]
    return GitState(
        branch=branch if branch and branch != "HEAD" else None,
        head_sha=head.strip() or None,
        dirty=dirty,
    )


def git_state_handoff(
    session_id: str, reason: str | None, state: GitState | None
) -> tuple[str, HandoffFields]:
    """The minimal hand-off a budget stop leaves when the agent wrote none."""
    stopped = reason or "the session reached its budget"
    if state is None:
        body = (
            f"Budget stop ({stopped}). The session wrote no hand-off and its "
            "working tree could not be read, so nothing about its progress is "
            "known here."
        )
        return body, HandoffFields(
            session=session_id,
            remaining=["unknown: re-read the item and its branch"],
        )
    dirty = state.dirty[:_DIRTY_LIMIT]
    more = len(state.dirty) - len(dirty)
    body = (
        f"Budget stop ({stopped}). The session wrote no hand-off; this one is "
        "from its git state. "
        + (
            f"{len(state.dirty)} uncommitted path(s) were left in the tree."
            if state.dirty
            else "The tree was clean."
        )
    )
    remaining = ["not stated by the session: read the branch's commits"]
    if state.dirty:
        remaining.append("commit or discard the uncommitted paths in touch_points")
    if more > 0:
        dirty.append(f"... and {more} more")
    return body, HandoffFields(
        session=session_id,
        branch=state.branch,
        head_sha=state.head_sha,
        touch_points=dirty,
        remaining=remaining,
    )


#: The limits a restart carries over from the stopped session's engine row.
_BUDGET_KEYS = (
    "turns",
    "context_tokens",
    "wall_clock_minutes",
    "session_wall_clock_minutes",
)


def _restart_params(
    link: CodingSession,
    ref: str,
    n: int,
    cap: int,
    budget: dict[str, object] | None = None,
) -> StartSessionParams | None:
    """The start that continues `link`'s item: its own purpose, template,
    model, effort and budget, so a restart runs on what the stopped session
    ran rather than on the profile's defaults."""
    if link.purpose is None and link.template is None:
        return None
    task = (
        f"Continue {ref} from its latest hand-off: the previous session "
        "stopped at its turn budget. Read the hand-off, check out its branch, "
        "carry on, and hand off again when the gates are green."
    )
    fields: dict[str, object] = {
        "from_handoff": ref,
        "task": task,
        "reason": f"{REASON} ({n} of {cap})",
    }
    if link.purpose is not None:
        fields["purpose"] = link.purpose
    if link.template is not None:
        fields["template"] = link.template
    if link.model is not None:
        fields["model"] = link.model
    if link.effort is not None:
        fields["effort"] = link.effort
    limits = {
        key: value
        for key in _BUDGET_KEYS
        if isinstance(value := (budget or {}).get(key), int) and value > 0
    }
    if limits:
        fields["budget"] = limits
    return StartSessionParams.model_validate(fields)


def settle_budget_stops(ctx: AppContext) -> BudgetSettlement:
    """Hand off, and restart within the cap, every budget-stopped session."""
    # Imported here: the services package imports this module.
    from vogt.application.services.sessions import start_session
    from vogt.application.services.work import comment_work

    done = BudgetSettlement()
    if ctx.engine is None:
        return done
    try:
        rows, _hidden = ctx.engine.list_sessions()
    except EngineUnavailable as exc:
        logger.info("budget hand-off pass skipped: %s", exc)
        return done
    stopped = [
        row
        for row in rows
        if row.hibernated
        and row.hibernation is not None
        and row.hibernation.trigger == "budget"
    ]
    if not stopped:
        return done
    with ctx.declared.read() as view:
        cap = dispatch_settings(view).budget_restarts_per_item
    for row in stopped:
        with ctx.declared.read() as view:
            link = view.session_by_engine_id(row.id)
            if link is None or link.work_item_id is None:
                continue
            item = view.work_item_by_id(link.work_item_id)
            if item is None:
                continue
            comments = view.comments_for(item.id, limit=200, order="newest")
            later = [
                one
                for one in view.list_sessions(
                    work_item_id=item.id, include_stopped=True, limit=50, offset=0
                )
                if one.id != link.id and one.started_at > link.started_at
            ]
        since = [one for one in comments if one.created_at >= link.started_at]
        if not any(one.kind == "handoff" for one in since):
            assert row.hibernation is not None
            body, fields = git_state_handoff(
                row.id, row.hibernation.reason, read_git_state(row.cwd or link.cwd)
            )
            try:
                comment_work(
                    ctx,
                    CommentParams(
                        ref=item.ref,
                        body=body,
                        kind="handoff",
                        handoff=fields,
                        reason=REASON,
                    ),
                )
            except VogtError as exc:
                # An upstream item has no local thread to carry a hand-off.
                logger.warning("budget hand-off on %s refused: %s", item.ref, exc)
                continue
            done.handoffs_written.append(item.ref)
        # Anything started on the item since is the continuation, a person's
        # or this pass's own: never a second one.
        if later:
            continue
        used = sum(
            1
            for one in comments
            if one.kind == "note" and one.body.startswith(RESTART_NOTE)
        )
        if used >= cap:
            continue
        params = _restart_params(link, item.ref, used + 1, cap, row.budget)
        if params is None:
            continue
        try:
            started = start_session(ctx, params)
        except (VogtError, EngineUnavailable) as exc:
            logger.warning("budget restart of %s refused: %s", item.ref, exc)
            continue
        comment_work(
            ctx,
            CommentParams(
                ref=item.ref,
                body=(
                    f"{RESTART_NOTE} {started.session.engine_session_id} "
                    f"continues from the hand-off of {row.id} "
                    f"({used + 1} of {cap})."
                ),
                reason=REASON,
            ),
        )
        done.restarted.append(item.ref)
    return done


__all__ = [
    "RESTART_NOTE",
    "BudgetSettlement",
    "GitState",
    "git_state_handoff",
    "read_git_state",
    "settle_budget_stops",
]
