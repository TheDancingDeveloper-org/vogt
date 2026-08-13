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

from vogt.errors import Conflict, VogtError

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
    existing = _reuse_existing(destination, request.remote)
    if existing is not None:
        return existing

    if shutil.which("git") is None:
        msg = "git is not installed, so a repository cannot be imported"
        raise GitUnavailable(msg)

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
        raise GitUnavailable(msg)
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
    """Ask an existing checkout something, tolerating an empty answer."""
    try:
        return _run_git(list(args), cwd=destination) or None
    except GitUnavailable:
        # A repository with no commits has no HEAD, and a clone with no
        # origin has no URL. Both are facts, not failures.
        return None
