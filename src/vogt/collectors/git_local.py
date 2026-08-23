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
from vogt.core.branches import match_branch
from vogt.core.entities import Project

KIND_CHECKOUT = "git.checkout"
KIND_TAG = "git.tag"
KIND_BRANCH = "git.branch"

#: Long enough for a slow filesystem, short enough that a hung git does not
#: hold a sweep open. A timeout is a partial sweep, not a crash.
GIT_TIMEOUT_SECONDS = 20


class GitLocalCollector:
    """Branch, dirty state, head revision, tags, and per-branch bindings."""

    @property
    def name(self) -> str:
        return "git-local"

    @property
    def requires_network(self) -> bool:
        return False

    def collect(self, ctx: CollectorContext, project: Project) -> Iterable[Finding]:
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

        # Where work is in git, reported never driven (#283, FR-B4): one
        # finding per local branch, carrying which work item its name binds to
        # by the configured pattern. Declared branches (the overlay) and these
        # observed ones are compared elsewhere and disagree as drift (FR-O2);
        # this half only says what the checkout has.
        yield from _branch_findings(ctx, project, root)


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


def _default_branch(root: Path, names: list[str]) -> str | None:
    """The branch ahead/behind is measured against, best effort.

    `origin/HEAD` names it when the remote does; otherwise a checkout that has
    never spoken to a remote falls back to whichever of the conventional names
    it actually carries. `None` when neither can be established — in which case
    ahead/behind is reported absent rather than measured against a guess.
    """
    head = git_output(root, "rev-parse", "--abbrev-ref", "origin/HEAD")
    if head.startswith("origin/"):
        candidate = head[len("origin/") :]
        if candidate:
            return candidate
    for name in ("main", "master", "trunk"):
        if name in names:
            return name
    return None


def _ahead_behind(root: Path, default: str, name: str) -> tuple[int | None, int | None]:
    """How far `name` is ahead of and behind `default`.

    `git rev-list --left-right --count default...name` counts the two sides of
    the symmetric difference: left is commits on `default` the branch has not
    got (behind), right is commits on the branch `default` has not got (ahead).
    An unreadable answer is absent, not zero — the same rule the rest of this
    module keeps about a count that would otherwise read as a claim.
    """
    if name == default:
        return 0, 0
    counts = git_output(
        root, "rev-list", "--left-right", "--count", f"{default}...{name}"
    )
    parts = counts.split()
    if len(parts) != 2 or not all(p.isdigit() for p in parts):
        return None, None
    behind, ahead = int(parts[0]), int(parts[1])
    return ahead, behind


def _branch_findings(
    ctx: CollectorContext, project: Project, root: Path
) -> Iterable[Finding]:
    """One finding per local branch, with the work item its name binds to.

    Deliberately records `last_commit_at` rather than an age: a timestamp is
    stable, so re-observing an unchanged branch writes nothing (NFR-S2), and
    the surface derives "active 2h ago" from it at read time. `work_item_ref`
    and `forge_number` are the pattern match — a vogt ref resolves here, a
    forge number is left for the application layer to resolve against the
    linked project, the same store boundary every other collector keeps.
    """
    listing = git_output(
        root,
        "for-each-ref",
        "--format=%(refname:short)%09%(objectname)%09%(committerdate:iso-strict)",
        "refs/heads",
    )
    if not listing:
        return
    rows: list[tuple[str, str, str]] = []
    for line in listing.splitlines():
        parts = line.split("\t")
        if len(parts) == 3:
            rows.append((parts[0], parts[1], parts[2]))
    names = [name for name, _sha, _date in rows]
    default = _default_branch(root, names)
    patterns = tuple(ctx.config.branch_binding_patterns)

    for name, sha, committed in rows:
        ahead, behind = (
            (None, None) if default is None else _ahead_behind(root, default, name)
        )
        match = match_branch(name, patterns)
        yield finding(
            kind=KIND_BRANCH,
            subject_key=f"git-branch:{project.slug}:{name}",
            project=project,
            payload={
                "project": project.slug,
                "name": name,
                "tip": sha,
                "default_branch": default,
                "ahead": ahead,
                "behind": behind,
                "last_commit_at": committed or None,
                "work_item_ref": None if match is None else match.work_ref,
                "forge_number": None if match is None else match.forge_number,
            },
        )


def tracked_names(root: Path) -> frozenset[str] | None:
    """The top-level names this repository actually carries, or `None`.

    `None` means the question could not be asked — `root` is not a git
    checkout, or git is not runnable here — which is different from "it
    carries nothing" and is what lets the contract fall back to reading the
    filesystem rather than failing every criterion at once (FR-O9).

    Only the first path segment is kept, because that is all a contract
    criterion names: `AGENTS.md`, `docs`. A directory is tracked if anything
    under it is, which is also the only sense in which git has directories at
    all — and is exactly why an empty `docs/` cannot be tracked and should
    never have counted.
    """
    if not (root / ".git").exists():
        return None
    try:
        listing = git_output(root, "ls-files", "-z", required=True)
    except CollectorError:
        return None
    return frozenset(entry.split("/", 1)[0] for entry in listing.split("\0") if entry)
