"""The GitHub implementation of `ForgeProvider` (D2).

A thin wrapper over the existing `GitHubClient` — its injectable `api_root`
is the seam that lets a test point the provider at a fake without touching a
network, and the whole reason the client was built that way. Nothing here
duplicates the client's transport, its 204-vs-404 handling, or its
"read a token from a file" rule; the provider adds the *shape* the rest of
Vogt will speak (Phase 4), not a second HTTP stack.

Write-back delegates to `ForgeWriter`, the one object allowed to mutate
GitHub (FR-B4). The import is function-local on purpose: `ForgeWriter` reaches
back through this provider for its key scheme, and a module-level import in
both directions would be a cycle. Reading never needs it, so reading never
pays for it.
"""

from __future__ import annotations

from collections.abc import Iterable
from typing import TYPE_CHECKING, Any

from vogt.adapters.forge.models import (
    ForgeCapabilities,
    ForgeCheck,
    ForgeIssue,
    ForgePull,
    ForgeRelease,
    RepoRef,
)
from vogt.adapters.github.client import (
    DEFAULT_PER_PAGE,
    NO_CONTENT,
    GitHubClient,
    repo_of,
)

if TYPE_CHECKING:
    from vogt.adapters.github.writeback import ForgeWriter, WriteBackResult

#: The one host this provider answers for. A value, not a literal buried in
#: `parse`, so the registry can advertise it and a caller can be told which
#: hosts are readable rather than left to infer it from a zero.
HOST = "github.com"

GITHUB_CAPABILITIES = ForgeCapabilities(
    hosts=(HOST,),
    supports_since=True,
    supports_posture=True,
    supports_notifications=True,
    # Deferred by name in the v1 ceiling (D10); the flag exists now so Phase 2
    # can branch on it rather than special-casing GitHub.
    supports_webhooks=False,
)


class GitHubProvider:
    """GitHub, behind the `ForgeProvider` contract."""

    def __init__(self, client: GitHubClient) -> None:
        self._client = client

    @property
    def client(self) -> GitHubClient:
        """The underlying client, for the Phase-1 collectors still reading raw.

        Phase 4 removes every caller of this; until then it is how the seam
        coexists with the code it is replacing without a flag day.
        """
        return self._client

    # -- identity ----------------------------------------------------------

    @property
    def capabilities(self) -> ForgeCapabilities:
        return GITHUB_CAPABILITIES

    def parse(self, repo_url: str | None) -> RepoRef | None:
        parsed = repo_of(repo_url)
        if parsed is None:
            return None
        owner, repo = parsed
        return RepoRef(host=HOST, owner=owner, repo=repo)

    def subject_key(self, ref: RepoRef, number: int | None) -> str:
        """`gh:{owner}/{repo}#{n}` — unchanged for github.com (D5).

        `number` is widened past the protocol's `int` because a collector hands
        it straight from an untrusted payload, where a malformed item can carry
        `None`; the key it produces is byte-identical to what the inline
        f-string produced before the seam existed.
        """
        return f"gh:{ref.owner}/{ref.repo}#{number}"

    def number_of(self, subject_key: str | None) -> int | None:
        """`gh:owner/repo#123` -> 123."""
        if not subject_key or "#" not in subject_key:
            return None
        _, _, tail = subject_key.partition("#")
        return int(tail) if tail.isdigit() else None

    # -- URLs (for import; the provider owns its own address space) ---------

    def clone_url(self, ref: RepoRef) -> str:
        return f"https://{HOST}/{ref.owner}/{ref.repo}.git"

    def web_url(self, ref: RepoRef) -> str:
        return f"https://{HOST}/{ref.owner}/{ref.repo}"

    # -- read surface ------------------------------------------------------

    def issues_updated_since(
        self, ref: RepoRef, since: str | None
    ) -> Iterable[ForgeIssue]:
        # Ascending by update time so a watermark walks history *forward*: the
        # max `updated_at` in a page is where the next sweep resumes, and a
        # first sync backfills oldest-first a page at a time (D3).
        params: dict[str, str | int] = {
            "state": "all",
            "sort": "updated",
            "direction": "asc",
            "per_page": DEFAULT_PER_PAGE,
        }
        if since:
            params["since"] = since
        payloads = self._client.get(
            f"/repos/{ref.owner}/{ref.repo}/issues", **params
        )
        for item in _as_list(payloads):
            if "pull_request" in item:
                # The issues endpoint returns PRs too; they have their own
                # reader and their own kind.
                continue
            yield _to_issue(ref, item)

    def pulls_updated_since(
        self, ref: RepoRef, since: str | None
    ) -> Iterable[ForgePull]:
        # The pulls endpoint has no server-side `since`, so this asks for the
        # most-recently-updated first and filters locally, then yields ascending
        # so the caller advances its watermark the same way it does for issues.
        # One page covers an estate's PR volume comfortably; a fuller backfill
        # is bounded the same way issues are, and reported through `truncated`.
        payloads = self._client.get(
            f"/repos/{ref.owner}/{ref.repo}/pulls",
            state="all",
            sort="updated",
            direction="desc",
            per_page=DEFAULT_PER_PAGE,
        )
        pulls = [_to_pull(ref, item) for item in _as_list(payloads)]
        if since:
            pulls = [p for p in pulls if p.updated_at is None or p.updated_at >= since]
        yield from reversed(pulls)

    def releases(self, ref: RepoRef) -> Iterable[ForgeRelease]:
        payloads = self._client.get(
            f"/repos/{ref.owner}/{ref.repo}/releases", per_page=DEFAULT_PER_PAGE
        )
        for item in _as_list(payloads):
            yield ForgeRelease(
                tag=item.get("tag_name", ""),
                repo=ref.slug,
                name=item.get("name"),
                draft=bool(item.get("draft", False)),
                prerelease=bool(item.get("prerelease", False)),
                published_at=item.get("published_at"),
                source_url=item.get("html_url"),
            )

    def checks(self, ref: RepoRef) -> Iterable[ForgeCheck]:
        payloads = self._client.get(
            f"/repos/{ref.owner}/{ref.repo}/actions/runs", per_page=20
        )
        runs = payloads.get("workflow_runs", []) if isinstance(payloads, dict) else []
        for item in runs:
            if not isinstance(item, dict):
                continue
            yield ForgeCheck(
                revision=item.get("head_sha", ""),
                check=item.get("name", "workflow"),
                repo=ref.slug,
                status=item.get("status"),
                conclusion=item.get("conclusion"),
                branch=item.get("head_branch"),
                event=item.get("event"),
                run_number=item.get("run_number"),
                updated_at=item.get("updated_at"),
                source_url=item.get("html_url"),
            )

    # -- write surface (delegates to the one mutator, FR-B4) ---------------

    def comment(self, ref: RepoRef, number: int, body: str) -> WriteBackResult:
        return self._writer().comment(
            repo_url=self.web_url(ref), number=number, body=body
        )

    def create_issue(
        self,
        ref: RepoRef,
        *,
        title: str,
        body: str,
        labels: list[str] | None = None,
    ) -> WriteBackResult:
        return self._writer().create_issue(
            repo_url=self.web_url(ref), title=title, body=body, labels=labels
        )

    def add_labels(
        self, ref: RepoRef, number: int, labels: list[str]
    ) -> WriteBackResult:
        return self._writer().add_labels(
            repo_url=self.web_url(ref), number=number, labels=labels
        )

    def set_state(self, ref: RepoRef, number: int, state: str) -> WriteBackResult:
        if state not in ("closed", "open"):
            msg = f"{state!r} is not a state; use 'closed' or 'open'"
            raise ValueError(msg)
        return self._writer().set_state(
            repo_url=self.web_url(ref),
            number=number,
            state=state,  # type: ignore[arg-type]
        )

    def _writer(self) -> ForgeWriter:
        from vogt.adapters.github.writeback import ForgeWriter

        return ForgeWriter(self._client)


def _as_list(payloads: object) -> list[dict[str, Any]]:
    """Narrow a GET result to the list of dicts a reader can iterate.

    `None` (404) and `NO_CONTENT` (204) both mean "nothing to read here",
    which is the honest answer, not an error (FR-O4).
    """
    if payloads is None or payloads is NO_CONTENT or not isinstance(payloads, list):
        return []
    return [item for item in payloads if isinstance(item, dict)]


def _to_issue(ref: RepoRef, item: dict[str, Any]) -> ForgeIssue:
    return ForgeIssue(
        number=int(item.get("number", 0)),
        title=item.get("title", ""),
        state=item.get("state", "open"),
        repo=ref.slug,
        labels=tuple(
            str(label["name"])
            for label in item.get("labels", [])
            if isinstance(label, dict) and label.get("name")
        ),
        author=(item.get("user") or {}).get("login"),
        assignees=tuple(
            str((a or {}).get("login"))
            for a in item.get("assignees", [])
            if (a or {}).get("login")
        ),
        comments=int(item.get("comments", 0)),
        updated_at=item.get("updated_at"),
        closed_at=item.get("closed_at"),
        source_url=item.get("html_url"),
    )


def _to_pull(ref: RepoRef, item: dict[str, Any]) -> ForgePull:
    return ForgePull(
        number=int(item.get("number", 0)),
        title=item.get("title", ""),
        state=item.get("state", "open"),
        repo=ref.slug,
        draft=bool(item.get("draft", False)),
        author=(item.get("user") or {}).get("login"),
        head=(item.get("head") or {}).get("sha"),
        base=(item.get("base") or {}).get("ref"),
        labels=tuple(
            str(label["name"])
            for label in item.get("labels", [])
            if isinstance(label, dict) and label.get("name")
        ),
        updated_at=item.get("updated_at"),
        closed_at=item.get("closed_at"),
        source_url=item.get("html_url"),
    )


__all__ = ["GITHUB_CAPABILITIES", "HOST", "GitHubProvider"]
