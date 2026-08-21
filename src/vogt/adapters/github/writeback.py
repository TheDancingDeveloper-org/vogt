"""Write-back: additive, forward-only, and opt-in (FR-B1–B5).

The rule that shapes this module is what it *cannot* do. There is no
deletion, no history rewriting, and no force operation anywhere in it — not
disabled, not gated, absent. A tool that holds a token for somebody's issue
tracker should not have the capability to destroy things in it, and the most
reliable way to guarantee that is not to write the code.

What it can do, by policy level:

- `none` (default) — nothing. Vogt observes and never speaks.
- `comment_only` — post comments authored in Vogt to the linked object.
- `full` — the above, plus create, label, and close/reopen.

`create_repo` (#182) sits outside the per-project policy table because the
project it serves has no upstream yet to have a policy about; the gate is the
`writeback` scope on the one operation that calls it, `forge.publish`, and a
name that already exists upstream is refused there, never clobbered.

Comments flow **outbound only** (FR-B5). A comment authored here posts
upstream; a comment authored on GitHub stays an observation shown against
the linked item and is never copied into `comments`. That keeps `comments`
unambiguously ours — every row has a Vogt actor and an audit trail — and
avoids needing forge-author identity mapping and loop suppression to tell
our own echo from somebody else's remark.
"""

from __future__ import annotations

import json
from typing import Any, Literal

from vogt.adapters.forge import GitHubProvider
from vogt.adapters.forge.writeback import (
    PERMITTED,
    WriteBackAction,
    WriteBackPolicy,
    WriteBackResult,
    permits,
)
from vogt.adapters.github.client import GitHubClient, GitHubUnavailable

__all__ = [
    "PERMITTED",
    "ForgeWriter",
    "WriteBackAction",
    "WriteBackPolicy",
    "WriteBackResult",
    "permits",
]


class ForgeWriter:
    """The only thing in Vogt that changes anything on GitHub."""

    def __init__(self, client: GitHubClient) -> None:
        self._client = client
        # The provider owns the host check and the subject-key scheme (D2, D5),
        # so the key a write reports lands in exactly the form a collector
        # reads it back in.
        self._provider = GitHubProvider(client)

    # -- the write verbs ---------------------------------------------------

    def comment(
        self, *, repo_url: str | None, number: int, body: str
    ) -> WriteBackResult:
        """Post a comment on a linked issue or pull request."""
        return self._post(
            repo_url,
            path="/repos/{owner}/{repo}/issues/{number}/comments",
            number=number,
            payload={"body": body},
        )

    def create_issue(
        self,
        *,
        repo_url: str | None,
        title: str,
        body: str,
        labels: list[str] | None = None,
    ) -> WriteBackResult:
        """Open a new issue. Never edits or replaces an existing one."""
        payload: dict[str, Any] = {"title": title, "body": body}
        if labels:
            payload["labels"] = labels
        return self._post(
            repo_url, path="/repos/{owner}/{repo}/issues", payload=payload
        )

    def add_labels(
        self, *, repo_url: str | None, number: int, labels: list[str]
    ) -> WriteBackResult:
        """Add labels. Adds only — it never replaces the existing set.

        GitHub's `PUT .../labels` replaces; `POST` appends. Using POST means
        a label somebody added upstream cannot be removed by a Vogt sync,
        which is the additive rule applied to the one endpoint where getting
        it wrong is silent.
        """
        return self._post(
            repo_url,
            path="/repos/{owner}/{repo}/issues/{number}/labels",
            number=number,
            payload={"labels": labels},
        )

    def set_state(
        self, *, repo_url: str | None, number: int, state: Literal["closed", "open"]
    ) -> WriteBackResult:
        """Close or reopen. Both directions are recoverable by the other."""
        return self._post(
            repo_url,
            path="/repos/{owner}/{repo}/issues/{number}",
            number=number,
            payload={"state": state},
            method="PATCH",
        )

    def create_repo(
        self, *, name: str, private: bool, description: str | None = None
    ) -> dict[str, Any]:
        """Create a repository under the authenticated account (#182).

        `POST /user/repos` and nothing else: no auto-init (the local history
        is about to be pushed and a generated first commit would make the
        very first push non-fast-forward), no template, no transfer. Errors
        propagate as `GitHubUnavailable` with the status in the message; the
        provider maps the 422 name-conflict onto the typed refusal, because
        the provider is the layer that knows the product's error taxonomy.
        """
        payload: dict[str, Any] = {
            "name": name,
            "private": private,
            "auto_init": False,
        }
        if description:
            payload["description"] = description
        response = self._client.send("/user/repos", payload)
        return response if isinstance(response, dict) else {}

    # -- transport ---------------------------------------------------------

    def _post(
        self,
        repo_url: str | None,
        *,
        path: str,
        payload: dict[str, Any],
        number: int | None = None,
        method: str = "POST",
    ) -> WriteBackResult:
        ref = self._provider.parse(repo_url)
        if ref is None:
            return WriteBackResult(
                outcome="skipped",
                detail="this project has no GitHub repository to write to",
            )
        endpoint = path.format(owner=ref.owner, repo=ref.repo, number=number)
        try:
            response = self._client.send(endpoint, payload, method=method)
        except GitHubUnavailable as exc:
            # Never fatal to the declared write: the local change stands and
            # the ledger records that the upstream half did not land.
            return WriteBackResult(outcome="failed", detail=str(exc))

        if response is None:
            return WriteBackResult(
                outcome="failed", detail=f"{endpoint} returned nothing (404?)"
            )
        html_url = response.get("html_url")
        upstream_number = response.get("number", number)
        return WriteBackResult(
            outcome="succeeded",
            source_url=None if html_url is None else str(html_url),
            subject_key=(
                None
                if upstream_number is None
                else self._provider.subject_key(ref, upstream_number)
            ),
            detail=json.dumps({"method": method, "endpoint": endpoint}),
        )
