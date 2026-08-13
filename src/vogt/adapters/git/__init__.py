"""The git adapter: the one place Vogt runs `git` to *change* local state.

`collectors/git_local.py` also shells out to git, and deliberately stays
where it is: it reads a checkout that already exists and writes nothing,
which makes it a collector. Cloning creates a working tree, so it is not a
collector and must not live among them (FR-O2).
"""

from __future__ import annotations

from vogt.adapters.git.clone import (
    CloneOutcome,
    Cloner,
    CloneRequest,
    GitUnavailable,
    clone_repository,
)

__all__ = [
    "CloneOutcome",
    "CloneRequest",
    "Cloner",
    "GitUnavailable",
    "clone_repository",
]
