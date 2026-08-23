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
    ForgeLabel,
    ForgeNotification,
    ForgePosture,
    ForgePull,
    ForgeRelease,
    ForgeRepo,
    RepoRef,
)
from vogt.adapters.github.client import (
    DEFAULT_PER_PAGE,
    NO_CONTENT,
    GitHubClient,
    GitHubUnavailable,
    repo_of,
)
from vogt.errors import RemoteRepoExists

if TYPE_CHECKING:
    from vogt.adapters.forge.writeback import WriteBackResult
    from vogt.adapters.github.writeback import ForgeWriter

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

    def list_repos(self) -> Iterable[ForgeRepo]:
        """`GET /user/repos` — every repository this token is entitled to see.

        `affiliation` names the three ways a token reaches a repository (owned,
        an org member, an outside collaborator), so a private repository the
        actor can push to is enumerable while the account-wide crawl the scope
        rule forbids is not — the token *is* the scope. Sorted by most-recently
        pushed so the picker's first page is the repositories a person is likely
        to be importing. One page, like every other read here; a fuller listing
        is bounded the same way the collectors are.
        """
        payloads = self._client.get(
            "/user/repos",
            per_page=DEFAULT_PER_PAGE,
            sort="pushed",
            direction="desc",
            affiliation="owner,collaborator,organization_member",
        )
        for item in _as_list(payloads):
            name = item.get("name")
            owner = (item.get("owner") or {}).get("login")
            if not name or not owner:
                continue
            yield ForgeRepo(
                owner=str(owner),
                name=str(name),
                default_branch=item.get("default_branch"),
                visibility=("private" if item.get("private", False) else "public"),
                web_url=item.get("html_url") or f"https://{HOST}/{owner}/{name}",
            )

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

    # -- import support (GitHub-shaped in v1) ------------------------------

    def describe(self, ref: RepoRef) -> dict[str, Any] | None:
        """The repository's metadata, or `None` when it is not visible.

        `None` is a 404 — gone, private, or wrong name — which the import
        service turns into its own NotFound; an unconfigured provider is a
        separate case the caller handles before it gets here."""
        payload = self._client.get(f"/repos/{ref.owner}/{ref.repo}")
        if payload is None or payload is NO_CONTENT:
            return None
        return payload if isinstance(payload, dict) else {}

    def clone_token(self) -> str | None:
        """The token a clone should authenticate with, if any."""
        return self._client.token

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
        payloads = self._client.get(f"/repos/{ref.owner}/{ref.repo}/issues", **params)
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

    def labels(self, ref: RepoRef) -> Iterable[ForgeLabel]:
        payloads = self._client.get(
            f"/repos/{ref.owner}/{ref.repo}/labels", per_page=DEFAULT_PER_PAGE
        )
        for item in _as_list(payloads):
            name = item.get("name")
            if not name:
                continue
            yield ForgeLabel(
                name=str(name),
                repo=ref.slug,
                color=item.get("color"),
                description=item.get("description"),
            )

    def posture(self, ref: RepoRef) -> ForgePosture:
        config = self._version_update_config(ref)
        return ForgePosture(
            version_updates_config=config,
            vulnerability_alerts=self._toggle(ref, "vulnerability-alerts"),
            automated_security_fixes=self._toggle(ref, "automated-security-fixes"),
            repo=ref.slug,
        )

    def notifications(self, ref: RepoRef) -> Iterable[ForgeNotification]:
        # Per-repository, never the account-wide inbox: the scope rule holds by
        # construction (FR-G15). `all=true` keeps read threads, which are still
        # notifications — `unread` is a field, not a collection-time decision.
        payloads = self._client.get(
            f"/repos/{ref.owner}/{ref.repo}/notifications",
            per_page=DEFAULT_PER_PAGE,
            all="true",
        )
        for item in _as_list(payloads):
            subject = item.get("subject") or {}
            yield ForgeNotification(
                thread=str(item.get("id", "")),
                repo=ref.slug,
                reason=item.get("reason"),
                unread=bool(item.get("unread", False)),
                title=subject.get("title", ""),
                subject_type=subject.get("type"),
                updated_at=item.get("updated_at"),
                last_read_at=item.get("last_read_at"),
                source_url=_notification_web_url(subject.get("url")),
            )

    #: Where the ecosystems keep their update-automation config. Presence is
    #: the signal; the contents are not parsed.
    _VERSION_UPDATE_CONFIGS = (
        "renovate.json",
        "renovate.json5",
        ".github/renovate.json",
        ".github/renovate.json5",
        ".renovaterc",
        ".renovaterc.json",
        ".github/dependabot.yml",
        ".github/dependabot.yaml",
    )

    def _version_update_config(self, ref: RepoRef) -> str | None:
        for path in self._VERSION_UPDATE_CONFIGS:
            try:
                found = self._client.get(
                    f"/repos/{ref.owner}/{ref.repo}/contents/{path}"
                )
            except GitHubUnavailable:
                return None
            if found is not None:
                return path
        return None

    def _toggle(self, ref: RepoRef, endpoint: str) -> bool | None:
        """One repository toggle: 204 (on) → True, 404 (off) → False, error →
        None ("could not tell", never flattened to off)."""
        try:
            found = self._client.get(f"/repos/{ref.owner}/{ref.repo}/{endpoint}")
        except GitHubUnavailable:
            return None
        return found is not None

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

    def create_repo(
        self, name: str, *, private: bool, description: str | None = None
    ) -> ForgeRepo:
        """`POST /user/repos` — a new repository under this credential (#182).

        Under the acting actor's PAT the repository lands in *their* account,
        which is the whole point of #179's identity work. GitHub answers a
        name that already exists with 422, and that is a **typed refusal**
        here (`RemoteRepoExists`), never an adoption of the existing
        repository: the status travels in the client's error message, which
        is the one stable thing `send` reports about a refused write.
        """
        try:
            response = self._writer().create_repo(
                name=name, private=private, description=description
            )
        except GitHubUnavailable as exc:
            if "422" in str(exc):
                msg = (
                    f"github.com already has a repository named {name!r} "
                    "reachable by this account; `forge.publish` never adopts "
                    "or overwrites an existing remote — pick another name, or "
                    "attach to the existing repository with `forge link` "
                    "after setting the project's repo_url"
                )
                raise RemoteRepoExists(msg) from exc
            raise
        owner = (response.get("owner") or {}).get("login")
        if not owner:
            msg = (
                "GitHub accepted the repository create but returned no owner, "
                "so there is no address to push to"
            )
            raise GitHubUnavailable(msg)
        created = str(response.get("name") or name)
        return ForgeRepo(
            owner=str(owner),
            name=created,
            default_branch=response.get("default_branch"),
            visibility=("private" if response.get("private", private) else "public"),
            web_url=response.get("html_url") or f"https://{HOST}/{owner}/{created}",
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


def _notification_web_url(api_url: object) -> str | None:
    """Turn the REST resource URL a notification subject carries into one a
    person can open."""
    if not isinstance(api_url, str) or not api_url:
        return None
    tail = api_url.partition("api.github.com/repos/")[2]
    if not tail:
        return api_url
    return "https://github.com/" + tail.replace("/pulls/", "/pull/")


def _to_pull(ref: RepoRef, item: dict[str, Any]) -> ForgePull:
    head = item.get("head") or {}
    return ForgePull(
        number=int(item.get("number", 0)),
        title=item.get("title", ""),
        state=item.get("state", "open"),
        repo=ref.slug,
        draft=bool(item.get("draft", False)),
        # A merged PR reads `state: closed` with `merged`/`merged_at` set; the
        # list endpoint omits `merged` but carries `merged_at`, so read either.
        merged=bool(item.get("merged") or item.get("merged_at")),
        author=(item.get("user") or {}).get("login"),
        head=head.get("sha"),
        head_ref=head.get("ref"),
        base=(item.get("base") or {}).get("ref"),
        body=item.get("body"),
        labels=tuple(
            str(label["name"])
            for label in item.get("labels", [])
            if isinstance(label, dict) and label.get("name")
        ),
        # Present on a single-PR read, absent from the list page — `None` then,
        # which the observation carries honestly rather than as a false rollup.
        mergeable=item.get("mergeable_state"),
        checks=_check_rollup(item),
        updated_at=item.get("updated_at"),
        closed_at=item.get("closed_at"),
        source_url=item.get("html_url"),
    )


def _check_rollup(item: dict[str, Any]) -> str | None:
    """The PR's combined check state, where the payload carries one.

    GitHub exposes it as `status.state` on the enriched PR object (the list
    page does not include it, so this is `None` there — a gap reported, not a
    green light)."""
    status = item.get("status")
    if isinstance(status, dict) and isinstance(status.get("state"), str):
        return str(status["state"])
    return None


__all__ = ["GITHUB_CAPABILITIES", "HOST", "GitHubProvider"]
