"""Check a session's completion claim against git and the forge (WI-1835).

Reads the facts (the worktree's branch, HEAD, whether it is pushed, and how
many commits it gained since the session started, plus the observed pull
requests the claim names or that come from that branch) and hands them to
`vogt.core.claims` to compare. Git is read with fixed `rev-parse`/`rev-list`/
`branch` commands that run no hooks. The forge is read from the observed
store, not called, so a check costs no forge request.

A fact that cannot be read is listed under `unchecked`. It is never counted
as a mismatch, and never as a pass.
"""

from __future__ import annotations

import os
import subprocess
from datetime import datetime
from pathlib import Path

from vogt.application.context import AppContext
from vogt.application.models import (
    ClaimCheck,
    ClaimPull,
    CommentParams,
    SessionSummary,
)
from vogt.core import claims
from vogt.errors import VogtError

_GIT_TIMEOUT = 10


def _git(root: Path, *args: str) -> str | None:
    """One read-only git question, or `None` when git cannot answer it."""
    env = {
        key: value
        for key, value in os.environ.items()
        if key not in {"GIT_DIR", "GIT_WORK_TREE", "GIT_COMMON_DIR", "GIT_CONFIG_COUNT"}
    }
    env["GIT_CONFIG_NOSYSTEM"] = "1"
    try:
        completed = subprocess.run(  # fixed argv, never a shell
            [
                "git",
                "-C",
                str(root),
                "-c",
                "core.fsmonitor=",
                "-c",
                "core.hooksPath=/dev/null",
                *args,
            ],
            capture_output=True,
            text=True,
            timeout=_GIT_TIMEOUT,
            check=False,
            env=env,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    if completed.returncode != 0:
        return None
    return completed.stdout.strip()


def _session_commits(
    root: Path, head: str, branch: str | None, started: datetime
) -> int | None:
    """Commits on `head` made since `started` that only this branch has.

    A commit any other remote branch already carries is someone else's work,
    brought in by a rebase or pull onto a `main` that moved after the start,
    and does not count. Only this branch's own remote ref is exempt, so this
    session's pushed commits still count. A commit date is all git keeps, so
    a commit this session rebased also counts: its committer date is new.
    """
    args = ["rev-list", "--count", f"--since={started.isoformat()}", head, "--not"]
    if branch and branch != "HEAD":
        # Matched against the name under refs/remotes/, e.g. `origin/<branch>`.
        args.append(f"--exclude=*/{branch}")
    args.append("--remotes")
    count = _git(root, *args)
    return int(count) if count and count.isdigit() else None


def _parse(value: object) -> datetime | None:
    if not isinstance(value, str) or not value:
        return None
    try:
        return datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None


def _pulls(
    ctx: AppContext,
    *,
    project_id: str | None,
    branch: str | None,
    work_item: str | None,
    named: frozenset[int],
) -> list[claims.PullFact]:
    """The observed PRs the claim names, plus any from this branch or bound
    to the work item."""
    if not ctx.observed.has_evidence_tables():
        return []
    found: dict[int, claims.PullFact] = {}
    for obs in ctx.observed.latest(
        kinds=("forge.pull_request",), project_id=project_id, limit=1000
    ):
        payload = obs.payload
        number = payload.get("number")
        if not isinstance(number, int) or isinstance(number, bool):
            continue
        head_ref = payload.get("head_ref")
        implements = payload.get("implements")
        bound = isinstance(implements, list) and any(
            isinstance(edge, dict) and edge.get("subject") == work_item
            for edge in implements
        )
        if not (
            number in named
            or (branch is not None and head_ref == branch)
            or (work_item is not None and bound)
        ):
            continue
        state = str(payload.get("state") or "unknown")
        merged_by = payload.get("merged_by")
        checks = payload.get("checks")
        found[number] = claims.PullFact(
            number=number,
            state=state,
            merged_at=_parse(payload.get("merged_at") or payload.get("closed_at"))
            if state == "merged"
            else None,
            merged_by=merged_by if isinstance(merged_by, str) else None,
            checks=checks if isinstance(checks, str) else None,
            head_ref=head_ref if isinstance(head_ref, str) else None,
        )
    return [found[number] for number in sorted(found)]


def _project_id(ctx: AppContext, slug: str | None) -> str | None:
    if slug is None:
        return None
    with ctx.declared.read() as view:
        project = view.project_by_slug(slug)
    return None if project is None else project.id


def check_claim(
    ctx: AppContext,
    summary: SessionSummary,
    *,
    claim: str,
    text: str | None,
) -> ClaimCheck:
    """Check what `summary`'s session claims (`text`) against git and the
    forge."""
    unchecked: list[str] = []
    started = summary.started_at
    if started is None:
        unchecked.append("session start time unknown")
    branch = head = None
    pushed: bool | None = None
    new_commits: int | None = None
    root = Path(summary.cwd) if summary.cwd else None
    if root is None or not root.is_dir():
        unchecked.append(f"worktree {summary.cwd!r} is not readable here")
    else:
        head = _git(root, "rev-parse", "HEAD")
        branch = _git(root, "rev-parse", "--abbrev-ref", "HEAD")
        if head is None:
            unchecked.append("worktree HEAD could not be read")
        else:
            remotes = _git(root, "branch", "-r", "--contains", head)
            pushed = None if remotes is None else bool(remotes)
            if started is not None:
                new_commits = _session_commits(root, head, branch, started)
    named = claims.mentioned_pulls(text)
    pulls = _pulls(
        ctx,
        project_id=_project_id(ctx, summary.project),
        branch=None if branch == "HEAD" else branch,
        work_item=summary.work_item,
        named=named,
    )
    missing = sorted(named - {pull.number for pull in pulls})
    if missing:
        unchecked.append(
            "no forge observation of "
            + ", ".join(f"PR #{number}" for number in missing)
        )
    facts = claims.ClaimFacts(
        claim_text=text,
        session_started_at=started,
        branch=branch,
        head=head,
        pushed=pushed,
        new_commits=new_commits,
        pulls=tuple(pulls),
    )
    return ClaimCheck(
        claim=claim,  # type: ignore[arg-type]
        session=summary.id,
        work_item=summary.work_item,
        session_started_at=started,
        branch=branch,
        head=head,
        pushed=pushed,
        new_commits=new_commits,
        pulls=[
            ClaimPull(
                number=pull.number,
                state=pull.state,
                merged_at=pull.merged_at,
                merged_by=pull.merged_by,
                checks=pull.checks,
                head_ref=pull.head_ref,
            )
            for pull in pulls
        ],
        mismatches=claims.mismatches(facts),
        unchecked=unchecked,
    )


def comment_body(check: ClaimCheck) -> str:
    lines = [f"Claim check ({check.claim}) for session {check.session}: mismatches"]
    lines += [f"- {mismatch}" for mismatch in check.mismatches]
    lines.append(
        f"Facts: branch {check.branch}, head {(check.head or '?')[:12]}, "
        f"pushed {check.pushed}, new commits {check.new_commits}."
    )
    if check.unchecked:
        lines.append("Not checked: " + "; ".join(check.unchecked) + ".")
    return "\n".join(lines)


def record_claim_check(
    ctx: AppContext, check: ClaimCheck, *, once_key: str | None = None
) -> bool:
    """Put a flagged check on its item as a note. Returns whether it did.

    Nothing is recorded when the claim matched. A comment that cannot be
    written does not undo or fail the operation that prompted it.

    `once_key` makes the note idempotent, for a caller that sees the same
    claim again and again (every sweep sees a done session until it moves
    on). The key is written into the note, and a note already carrying it
    among the item's recent comments is not written twice. Such a note is
    only written on a native item, whose comments are local: on an upstream
    item it would be a forge comment, posted from a read.
    """
    from vogt.application.services.work import comment_work

    if not check.mismatches or check.work_item is None:
        return False
    body = comment_body(check)
    if once_key is not None:
        marker = f"Check key: {once_key}"
        with ctx.declared.read() as view:
            item = view.work_item_by_ref(check.work_item)
            if item is None:
                return False
            recent = view.comments_for(item.id, limit=50, order="newest")
        if any(marker in comment.body for comment in recent):
            return False
        body = f"{body}\n{marker}"
    try:
        comment_work(
            ctx,
            CommentParams(
                ref=check.work_item,
                body=body,
                reason=f"claim check ({check.claim}) found mismatches (WI-1835)",
            ),
        )
    except VogtError:
        return False
    return True
