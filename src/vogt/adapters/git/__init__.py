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
    GitCommandFailed,
    GitUnavailable,
    PublishSource,
    Pusher,
    PushOutcome,
    PushRequest,
    clone_repository,
    inspect_publish_source,
    push_branch,
)

__all__ = [
    "CloneOutcome",
    "CloneRequest",
    "Cloner",
    "GitCommandFailed",
    "GitUnavailable",
    "PublishSource",
    "PushOutcome",
    "PushRequest",
    "Pusher",
    "clone_repository",
    "inspect_publish_source",
    "push_branch",
]
