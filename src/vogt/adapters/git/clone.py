"""Cloning a repository, without ever writing the token down (FR-S8).

There are three easy ways to authenticate a clone and all of them leak. The
credential in the remote URL (`https://x-access-token:TOKEN@github.com/...`)
is recorded in `.git/config` forever and printed by `git remote -v`. A
`-c http.extraHeader=Authorization: Bearer TOKEN` argument sits in argv,
readable by any process listing. `git config credential.helper store` writes
it to a file nobody remembers to delete.

What is used here instead is `GIT_ASKPASS`: a short-lived helper script that
prints the token it reads from its own environment. The token reaches the
`git` child through its environment (`/proc/<pid>/environ` is readable only
by its owner) and reaches the helper the same way; it appears in no
command line, no configuration file, and no stored URL. FR-S7 said secrets
travel by file reference rather than argv, and this is that rule applied at
the one place a token would otherwise be trivially embeddable.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import tempfile
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path

from vogt.errors import (
    Conflict,
    ImportBranchDiverged,
    ImportWorkingTreeDirty,
    VogtError,
)

#: Long enough for a large repository on a slow link, bounded so an import
#: cannot hold a request open indefinitely. A timeout fails the import; it
#: never leaves a half-registered project, because the declared write has
#: not happened yet.
CLONE_TIMEOUT_SECONDS = 600
GIT_TIMEOUT_SECONDS = 20

#: The username half of a GitHub token credential. GitHub ignores it — the
#: token is the password — but git insists on asking for both.
TOKEN_USERNAME = "x-access-token"

_ASKPASS_SCRIPT = """#!/bin/sh
# Written by Vogt for one clone and deleted immediately afterwards.
# Prints the credential git asks for, reading it from this process's own
# environment so that it never reaches a command line (FR-S8).
case "$1" in
    Username*) printf '%s\\n' "$VOGT_GIT_USERNAME" ;;
    *) printf '%s\\n' "$VOGT_GIT_TOKEN" ;;
esac
"""


class GitUnavailable(VogtError):
    """`git` is missing, failed, or could not reach the remote."""

    code = "git_unavailable"
    http_status = 502


class GitCommandFailed(GitUnavailable):
    """git ran, and exited non-zero.

    Separated from its parent because the two mean opposite things to a
    caller asking a checkout a question. "git exited non-zero" is often the
    answer — a repository with no commits has no HEAD, a clone with no origin
    has no URL — while "git could not be run at all" is never an answer, and
    treating it as one is what let a missing binary read as a checkout with
    no origin (#21).
    """


@dataclass(frozen=True)
class CloneRequest:
    """One repository to put on disk."""

    remote: str
    destination: Path
    token: str | None = None


@dataclass(frozen=True)
class CloneOutcome:
    """What ended up on disk, and whether this call is what put it there."""

    destination: Path
    revision: str | None = None
    default_branch: str | None = None
    #: True when the destination was already a clone of the same remote.
    #: Import then registers what is there rather than cloning again
    #: (FR-P7) — re-running an import must not be destructive.
    reused: bool = False


#: How the service obtains a checkout. Injectable for the same reason
#: `GitHubClient` takes a transport: the test suite asserts what would have
#: been run, and asserts it without a network.
Cloner = Callable[[CloneRequest], CloneOutcome]


def clone_repository(request: CloneRequest) -> CloneOutcome:
    """Clone `request.remote` to `request.destination` (FR-P6, FR-P7)."""
    destination = request.destination.expanduser()

    # Before anything reads a checkout, not after. `_reuse_existing` asks git
    # what the destination's origin is; with no git that question comes back
    # empty and is indistinguishable from a checkout that genuinely has no
    # origin, so the import failed as "a clone of an unknown remote" against a
    # checkout whose remote was exactly right (#21). The guard existed — it
    # just sat downstream of the call that needed it.
    if shutil.which("git") is None:
        msg = "git is not installed, so a repository cannot be imported"
        raise GitUnavailable(msg)

    existing = _reuse_existing(destination, request.remote)
    if existing is not None:
        # A pre-existing checkout of the same remote is the one import path that
        # is gated (#180, design #178 decision 6). A new folder never reaches
        # here — `_reuse_existing` returns `None` and the clone runs — so only
        # the folder that already holds this remote's clone is inspected, and it
        # is refused unless it is clean and at parity with origin. Vogt runs no
        # merge, rebase or stash to make it so; the reconciliation is the
        # person's, not ours.
        _enforce_import_parity(destination, request.token)
        return existing

    destination.parent.mkdir(parents=True, exist_ok=True)
    with _AskPass(request.token) as env:
        _run_git(
            ["clone", "--origin", "origin", request.remote, str(destination)],
            cwd=destination.parent,
            env=env,
            timeout=CLONE_TIMEOUT_SECONDS,
        )

    return CloneOutcome(
        destination=destination,
        revision=_read(destination, "rev-parse", "HEAD"),
        default_branch=_read(destination, "rev-parse", "--abbrev-ref", "HEAD"),
    )


def _reuse_existing(destination: Path, remote: str) -> CloneOutcome | None:
    """Decide what an occupied destination means (FR-P7).

    A clone of the same remote is the repeat-import case and is reported as
    reused. Anything else — a checkout of a different repository, or an
    unrelated directory with files in it — fails without being touched.
    Overwriting somebody's working tree to satisfy an import would be the
    single most destructive thing this product could do.
    """
    if not destination.exists():
        return None
    if not (destination / ".git").exists():
        if any(destination.iterdir()):
            msg = (
                f"{destination} already exists and is not a git repository; "
                "import will not write into it"
            )
            raise Conflict(msg)
        return None

    origin = _read(destination, "remote", "get-url", "origin")
    if origin is None or not _same_remote(origin, remote):
        msg = (
            f"{destination} is a clone of {origin or 'an unknown remote'}, "
            f"not of {remote}"
        )
        raise Conflict(msg)
    return CloneOutcome(
        destination=destination,
        revision=_read(destination, "rev-parse", "HEAD"),
        default_branch=_read(destination, "rev-parse", "--abbrev-ref", "HEAD"),
        reused=True,
    )


def _enforce_import_parity(destination: Path, token: str | None) -> None:
    """Refuse a re-import onto a checkout that is not clean and at parity (#180).

    The gate reads only — `git status`, `git rev-parse`, `git ls-remote` — and
    changes nothing: no fetch, no merge, no rebase, no stash. It refuses on the
    first failing condition with a typed receipt naming exactly what is wrong,
    because the point of the gate is to hand the reconciliation back to the
    person rather than have Vogt guess at a merge it has no business making.

    Both conditions must hold. The working tree is checked first because it is a
    local, no-network read; only then does the divergence check reach origin.
    Branches other than the default are ignored — the default branch is the one
    a clone tracks and the one an import's consolidation reads.
    """
    branch = _default_branch(destination)
    dirty = _working_tree_changes(destination)
    if dirty:
        msg = (
            f"the working tree at {destination} has uncommitted changes "
            f"({len(dirty)} path(s), e.g. {dirty[0]!r}); Vogt does not touch "
            "your working tree, so commit, stash or discard them yourself and "
            "retry the import"
        )
        raise ImportWorkingTreeDirty(msg)
    local = _local_head(destination, branch)
    origin = _origin_head(destination, branch, token)
    if local != origin:
        msg = (
            f"local HEAD {local or 'unknown'} on branch {branch!r} has diverged "
            f"from origin HEAD {origin or 'unknown'}; Vogt performs no merge, "
            "rebase or stash, so push or pull the branch yourself and retry the "
            "import"
        )
        raise ImportBranchDiverged(msg)


def _default_branch(destination: Path) -> str:
    """The branch the gate inspects: the remote's default, as the clone tracks it.

    `refs/remotes/origin/HEAD` is what `git clone` sets to the default branch, so
    reading it names the default without a network round-trip or a guess. If it
    is absent (an older or hand-made checkout) the current branch is the honest
    fallback — it is the one an import's consolidation reads either way.
    """
    tracked = _read(destination, "symbolic-ref", "--short", "refs/remotes/origin/HEAD")
    if tracked:
        return tracked.removeprefix("origin/")
    return _read(destination, "rev-parse", "--abbrev-ref", "HEAD") or "HEAD"


def _working_tree_changes(destination: Path) -> list[str]:
    """The porcelain lines for a dirty tree, empty when it is clean.

    `--porcelain` is the stable, script-facing form: staged, unstaged and
    untracked changes all appear, one per line, and a clean tree is the empty
    string. Untracked files count — writing an import over somebody's unsaved
    new file is exactly the loss the gate exists to prevent.
    """
    out = _read(destination, "status", "--porcelain")
    return [line for line in (out or "").splitlines() if line.strip()]


def _local_head(destination: Path, branch: str) -> str | None:
    """The commit the local default branch points at, or `None` if it has none."""
    return _read(destination, "rev-parse", branch)


def _origin_head(destination: Path, branch: str, token: str | None) -> str | None:
    """The commit origin's default branch points at, read live (no fetch).

    `git ls-remote` asks origin for its current refs and writes nothing — no
    objects are fetched, no tracking ref is moved — so it answers "where is
    origin now" without performing any part of a merge. The token rides the same
    `GIT_ASKPASS` helper the clone uses, so a private origin is reachable without
    the credential ever touching a command line (FR-S8).
    """
    with _AskPass(token) as env:
        try:
            out = _run_git(
                ["ls-remote", "origin", f"refs/heads/{branch}"],
                cwd=destination,
                env=env,
            )
        except GitCommandFailed:
            return None
    return out.split()[0] if out else None


def _same_remote(left: str, right: str) -> bool:
    """Compare two remotes ignoring the ways git spells the same one."""
    return _normalise(left) == _normalise(right)


def _normalise(remote: str) -> str:
    candidate = remote.strip().removeprefix("git+")
    for prefix in ("https://", "http://", "ssh://"):
        candidate = candidate.removeprefix(prefix)
    candidate = candidate.replace("git@github.com:", "github.com/")
    # A credential embedded in a URL we are comparing against is somebody
    # else's doing, not ours; strip it rather than fail to match.
    _, _, tail = candidate.rpartition("@")
    return tail.removesuffix(".git").strip("/").lower()


class _AskPass:
    """A `GIT_ASKPASS` helper that exists for the length of one clone."""

    def __init__(self, token: str | None) -> None:
        self._token = token
        self._dir: str | None = None

    def __enter__(self) -> dict[str, str]:
        env = dict(os.environ)
        # Never let git stop for input: in a server process an interactive
        # prompt is an indefinite hang, not a question anyone can answer.
        env["GIT_TERMINAL_PROMPT"] = "0"
        if self._token is None:
            env.pop("GIT_ASKPASS", None)
            return env

        self._dir = tempfile.mkdtemp(prefix="vogt-askpass-")
        script = Path(self._dir) / "askpass.sh"
        script.write_text(_ASKPASS_SCRIPT, encoding="utf-8")
        script.chmod(0o700)
        env["GIT_ASKPASS"] = str(script)
        env["VOGT_GIT_USERNAME"] = TOKEN_USERNAME
        env["VOGT_GIT_TOKEN"] = self._token
        return env

    def __exit__(self, *exc: object) -> None:
        if self._dir is not None:
            shutil.rmtree(self._dir, ignore_errors=True)


def _run_git(
    args: list[str],
    *,
    cwd: Path,
    env: dict[str, str] | None = None,
    timeout: int = GIT_TIMEOUT_SECONDS,
) -> str:
    try:
        completed = subprocess.run(
            ["git", *args],
            cwd=cwd,
            env=env,
            capture_output=True,
            text=True,
            timeout=timeout,
            check=False,
        )
    except (OSError, subprocess.SubprocessError) as exc:
        msg = f"git {args[0]} failed: {exc}"
        raise GitUnavailable(msg) from exc
    if completed.returncode != 0:
        msg = f"git {args[0]} failed: {_redact(completed.stderr.strip())}"
        raise GitCommandFailed(msg)
    return completed.stdout.strip()


def _redact(message: str) -> str:
    """Keep git's diagnostics useful without repeating a credential.

    git echoes the remote URL in most failures. Ours never carries one, but
    a caller can pass a URL that does, and an error message is a place
    secrets escape to logs.
    """
    words = []
    for word in message.split():
        if "@" in word and "://" in word:
            scheme, _, rest = word.partition("://")
            words.append(f"{scheme}://{rest.rpartition('@')[2]}")
        else:
            words.append(word)
    return " ".join(words)


def _read(destination: Path, *args: str) -> str | None:
    """Ask an existing checkout something, tolerating an empty answer.

    Only an answer is tolerated. A repository with no commits has no HEAD,
    and a clone with no origin has no URL — git says so by exiting non-zero,
    and both are facts. git being absent, unrunnable or hung is not a fact
    about the checkout, and must not be reported as one: swallowing it here
    is what turned "this instance has no git" into "that is a clone of an
    unknown remote" (#21).
    """
    try:
        return _run_git(list(args), cwd=destination) or None
    except GitCommandFailed:
        return None
