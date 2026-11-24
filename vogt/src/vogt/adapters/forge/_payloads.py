"""Payload shapes GitHub and Forgejo/Gitea share.

Both forges answer `contents` and `compare` with the same envelope, so the
two providers parse them here once rather than drifting apart in two copies.
"""

from __future__ import annotations

import base64
import binascii
import urllib.parse

from vogt.adapters.forge.models import ForgeComparison


def quote_path(path: str) -> str:
    """URL-quote a repository path or ref, keeping its slashes."""
    return urllib.parse.quote(path.strip("/"), safe="/")


def decoded_content(payload: object) -> bytes | None:
    """The bytes of a `contents` API answer (GitHub and Forgejo agree)."""
    if not isinstance(payload, dict) or payload.get("type", "file") != "file":
        return None
    content = payload.get("content")
    if not isinstance(content, str):
        return None
    try:
        return base64.b64decode(content)
    except (binascii.Error, ValueError):
        return None


def comparison(base: str, head: str, payload: object) -> ForgeComparison | None:
    """A compare answer, in the shape GitHub and Forgejo share."""
    if not isinstance(payload, dict):
        return None
    commits: list[tuple[str, str]] = []
    for item in payload.get("commits") or []:
        if not isinstance(item, dict):
            continue
        sha = item.get("sha")
        commit = item.get("commit") if isinstance(item.get("commit"), dict) else {}
        message = commit.get("message") if isinstance(commit, dict) else None
        if isinstance(sha, str):
            first = (
                message.splitlines()[0] if isinstance(message, str) and message else ""
            )
            commits.append((sha, first))
    ahead = payload.get("ahead_by")
    behind = payload.get("behind_by")
    total = payload.get("total_commits")
    ahead_by = (
        ahead
        if isinstance(ahead, int)
        else (total if isinstance(total, int) else len(commits))
    )
    status = payload.get("status")
    head_sha = commits[-1][0] if commits else (base if status == "identical" else None)
    return ForgeComparison(
        base=base,
        head=head,
        head_sha=head_sha,
        status=status if isinstance(status, str) else None,
        ahead_by=ahead_by,
        behind_by=behind if isinstance(behind, int) else 0,
        commits=tuple(commits),
    )
