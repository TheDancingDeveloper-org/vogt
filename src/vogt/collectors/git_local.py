"""`git-local` — what the checkout itself says.

Offline by construction: it shells out to the local `git` binary and reads
nothing over the network. A project that is not a git repository is not an
error — it is a folder, which is a first-class kind of project (NFR-PO1), and
the collector simply reports that.
"""

from __future__ import annotations

import subprocess
from collections.abc import Iterable
from pathlib import Path

from vogt.collectors.base import (
    CollectorContext,
    CollectorError,
    Finding,
    finding,
)
from vogt.core.entities import Project

KIND_CHECKOUT = "git.checkout"
KIND_TAG = "git.tag"

#: Long enough for a slow filesystem, short enough that a hung git does not
#: hold a sweep open. A timeout is a partial sweep, not a crash.
GIT_TIMEOUT_SECONDS = 20


class GitLocalCollector:
    """Branch, dirty state, head revision, and tags."""

    @property
    def name(self) -> str:
        return "git-local"

    @property
    def requires_network(self) -> bool:
        return False

    def collect(self, ctx: CollectorContext, project: Project) -> Iterable[Finding]:
        del ctx
        root = Path(project.root_path).expanduser()
        if not (root / ".git").exists():
            yield finding(
                kind=KIND_CHECKOUT,
                subject_key=f"git:{project.slug}",
                project=project,
                payload={
                    "is_git_repository": False,
                    "detail": "no .git directory; this project is a plain folder",
                },
            )
            return

        branch = git_output(root, "rev-parse", "--abbrev-ref", "HEAD")
        head = git_output(root, "rev-parse", "HEAD")
        # Required: `dirty` is the one field here that is a claim about the
        # tree rather than a value read off it, so it must never be inferred
        # from a question that failed (#20).
        status = git_output(root, "status", "--porcelain", required=True)
        describe = git_output(root, "describe", "--tags", "--abbrev=0")

        yield finding(
            kind=KIND_CHECKOUT,
            subject_key=f"git:{project.slug}",
            project=project,
            payload={
                "is_git_repository": True,
                "branch": branch,
                "head": head,
                "dirty": bool(status),
                "dirty_files": 0 if not status else len(status.splitlines()),
                "latest_tag": describe or None,
            },
        )

        if describe:
            # The version a checkout *observes*. Comparing it with the
            # project's declared version is what raises `version_mismatch`
            # drift at M3 (FR-P3); this collector only reports.
            yield finding(
                kind=KIND_TAG,
                subject_key=f"release:{project.slug}@{describe}",
                project=project,
                payload={"tag": describe, "source": "git describe"},
            )


def git_output(root: Path, *args: str, required: bool = False) -> str:
    """Run one git command, returning "" when git ran and had no answer.

    Deliberately forgiving about *answers*: a repository with no commits, no
    tags, or a detached HEAD is a normal thing to observe, not a failure to
    report, and git says so by exiting non-zero.

    Not forgiving about being unable to ask. git missing, unrunnable, or
    hung is not a fact about the checkout, and the empty string it used to
    return here was read by the caller as one — production spent v0.2.0
    recording `dirty: false, branch: "", head: ""` for every project from an
    image with no git, which is a clean checkout asserted by a collector
    that never read a checkout (#20). That becomes a `CollectorError`, which
    the sweeper already records as a partial sweep naming the project, so
    the estate learns "not collected" instead of "there is nothing".

    This also makes the timeout below mean what its comment always claimed.

    `required=True` extends that to a non-zero exit, for the questions whose
    empty answer would become a claim rather than an absence. `git status`
    is the one that matters: "" from a failed status reads as a clean tree,
    and a clean tree is an assertion. "no tags" is an absence, and stays
    forgiving.

    Public, and shared with `session_outcomes.py`, because both collectors
    ask the same *kind* of question — what does this checkout, which already
    exists, say — and the rule about running git in a collector is that it
    reads and never writes (`AGENTS.md`). Two copies of this function would
    be two places for that rule to be forgotten in.
    """
    try:
        completed = subprocess.run(  # fixed argv, never a shell
            ["git", "-C", str(root), *args],
            capture_output=True,
            text=True,
            timeout=GIT_TIMEOUT_SECONDS,
            check=False,
        )
    except (OSError, subprocess.SubprocessError) as exc:
        msg = f"git could not be run against {root}: {exc}"
        raise CollectorError(msg) from exc
    if completed.returncode != 0:
        if required:
            detail = completed.stderr.strip() or f"exit {completed.returncode}"
            msg = f"git {args[0]} could not be read in {root}: {detail}"
            raise CollectorError(msg)
        return ""
    return completed.stdout.strip()
