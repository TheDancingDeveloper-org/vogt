"""The Forgejo/Gitea implementation of `ForgeProvider` (D2, #176).

The acceptance test of the seam: a second forge is this module plus one
`_Spec` entry in `registry.py`, and nothing outside `adapters/forge/` learns
it exists. Forgejo's `/api/v1` is Gitea-shaped and deliberately GitHub-like,
so most of the mapping is direct — where it is not, the gap is *declared*
(`supports_posture=False`) rather than papered over, and the capability-gated
collectors turn the declaration into a `not_supported` receipt (FR-O11).

Unlike github.com, a Forgejo host is data, not a constant: any host named
under `[forge_token_files]` that is not github.com is read as a Forgejo
installation (D8). The estate's is `repo.indexarr.net`; a test's is whatever
its config says. That is why `parse` here takes its host list from the
provider's own client rather than a module literal.

Subject keys are host-qualified — `forge:{host}/{owner}/{repo}#{n}` (the #171
scheme) — because two Forgejo installations can each hold an `acme/widgets`
and nothing but the host tells them apart. GitHub keeps its legacy
`gh:{owner}/{repo}#{n}` (D5); the schemes cannot collide because only this
provider builds `forge:` keys and only it parses them back.

The write surface is implemented directly against the API rather than through
`adapters/github`'s `ForgeWriter` — that writer is GitHub's one mutator, and
pointing it at a second forge would re-couple the two hosts the seam exists
to keep apart. The same rules hold by construction: POST and PATCH only,
labels are *added* (never replaced), and there is no destructive verb
anywhere in this module (FR-B4).
"""

from __future__ import annotations

import json
import urllib.error
import urllib.parse
import urllib.request
from collections.abc import Iterable
from dataclasses import dataclass
from pathlib import Path
from typing import Any

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
from vogt.adapters.forge.writeback import WriteBackResult
from vogt.adapters.github.client import (
    DEFAULT_PER_PAGE,
    DEFAULT_TIMEOUT_SECONDS,
    NO_CONTENT,
    USER_AGENT,
    Transport,
)
from vogt.errors import RemoteRepoExists, VogtError


class ForgejoUnavailable(VogtError):
    """The Forgejo host could not be reached or refused the request.

    Never fatal to a sweep — the sweeper records the collector as `partial`
    or `failed` and the affected subjects stay "not collected" — and never
    fatal to a declared write, whose ledger records that the upstream half
    did not land.
    """

    code = "forgejo_unavailable"
    http_status = 502


@dataclass(frozen=True)
class ForgejoClient:
    """Thin access to one Forgejo/Gitea installation's `/api/v1`.

    Its own client rather than a reuse of `GitHubClient` because the two
    disagree on exactly the things a shared client would have to flag on:
    the API root lives under the forge's own domain, the auth header is
    `token …` rather than `Bearer …`, and the errors should name Forgejo.
    The *shape* — token from a file (FR-S7), `Transport` seam for tests,
    404 → `None`, 204/empty → `NO_CONTENT`, one greppable mutating method —
    is kept deliberately identical.
    """

    host: str
    token: str | None = None
    transport: Transport | None = None
    timeout: int = DEFAULT_TIMEOUT_SECONDS

    @property
    def api_root(self) -> str:
        return f"https://{self.host}/api/v1"

    @classmethod
    def from_token_file(
        cls, host: str, path: Path | None, *, transport: Transport | None = None
    ) -> ForgejoClient | None:
        """Build a client, or `None` when this host is not configured.

        `None` is the ordinary case, not an error: no token file means the
        host's subjects are simply not collected (D8)."""
        if path is None:
            return None
        resolved = Path(path).expanduser()
        if not resolved.is_file():
            return None
        token = resolved.read_text(encoding="utf-8").strip()
        if not token:
            return None
        return cls(host=host, token=token, transport=transport)

    def get(self, path: str, **params: str | int) -> Any:
        """GET one resource, returning parsed JSON, or `None` for a 404."""
        query = urllib.parse.urlencode({k: str(v) for k, v in params.items()})
        url = f"{self.api_root}{path}" + (f"?{query}" if query else "")
        status, body = self._fetch(url, self._headers())
        if status == 404:
            return None
        if status == 403:
            msg = f"{self.host} refused {path} (403): rate limited or unauthorised"
            raise ForgejoUnavailable(msg)
        if status >= 400:
            msg = f"{self.host} returned {status} for {path}"
            raise ForgejoUnavailable(msg)
        if status == 204 or not body.strip():
            return NO_CONTENT
        return json.loads(body.decode("utf-8"))

    def send(self, path: str, payload: dict[str, Any], *, method: str = "POST") -> Any:
        """Make a change upstream. The only mutating method on this client.

        POST and PATCH only, the same rule as GitHub's client and for the
        same reason: every call site that changes somebody else's data is
        greppable, and there is no DELETE anywhere in the product (FR-B4).
        """
        if method not in ("POST", "PATCH"):
            msg = f"{method} is not an additive operation; write-back is forward-only"
            raise ForgejoUnavailable(msg)
        url = f"{self.api_root}{path}"
        headers = self._headers()
        headers["Content-Type"] = "application/json"
        body = json.dumps(payload).encode("utf-8")
        status, response = self._fetch(url, headers, body=body, method=method)
        if status == 404:
            return None
        if status >= 400:
            msg = f"{self.host} returned {status} for {method} {path}"
            raise ForgejoUnavailable(msg)
        return json.loads(response.decode("utf-8")) if response.strip() else {}

    def _headers(self) -> dict[str, str]:
        headers = {"Accept": "application/json", "User-Agent": USER_AGENT}
        if self.token:
            # Forgejo/Gitea's documented scheme is `token {token}`, not
            # GitHub's `Bearer {token}`.
            headers["Authorization"] = f"token {self.token}"
        return headers

    def _fetch(
        self,
        url: str,
        headers: dict[str, str],
        *,
        body: bytes | None = None,
        method: str = "GET",
    ) -> tuple[int, bytes]:
        if self.transport is not None:
            return self.transport(url, headers, body or b"", method)
        request = urllib.request.Request(url, headers=headers, data=body, method=method)
        try:
            with urllib.request.urlopen(  # https only; api_root is not caller-set
                request, timeout=self.timeout
            ) as response:
                return int(response.status), bytes(response.read())
        except urllib.error.HTTPError as exc:  # pragma: no cover - network shape
            return int(exc.code), bytes(exc.read())
        except (urllib.error.URLError, TimeoutError, OSError) as exc:
            msg = f"{self.host} unreachable: {exc}"
            raise ForgejoUnavailable(msg) from exc


def parse_repo_url(repo_url: str | None, hosts: tuple[str, ...]) -> RepoRef | None:
    """Extract a `RepoRef` from a URL naming one of `hosts`, else `None`.

    The same normalisation `repo_of` applies to GitHub URLs — `git+`,
    `https`/`ssh`, the `git@host:owner/repo` scp form, a `.git` suffix —
    against a host list that is configuration rather than a constant. `None`
    is the ordinary answer for a URL these hosts do not serve: "not mine",
    not "malformed".
    """
    if not repo_url or not hosts:
        return None
    candidate = repo_url.strip().removeprefix("git+")
    for prefix in ("https://", "http://", "ssh://"):
        candidate = candidate.removeprefix(prefix)
    for host in hosts:
        candidate = candidate.replace(f"git@{host}:", f"{host}/")
    candidate = candidate.removesuffix(".git").strip("/")
    named = candidate.split("/", 1)[0]
    if named not in hosts:
        return None
    parts = candidate[len(named) :].strip("/").split("/")
    if len(parts) < 2 or not parts[0] or not parts[1]:
        return None
    return RepoRef(host=named, owner=parts[0], repo=parts[1])


#: Task states Forgejo reports that are terminal — usable as a conclusion.
#: Gitea/Forgejo fold GitHub's status/conclusion pair into one `status` field;
#: the split is reconstructed rather than invented: a run still in flight has
#: a status and honestly no conclusion.
_TERMINAL_CHECK_STATES = frozenset({"success", "failure", "cancelled", "skipped"})


class ForgejoProvider:
    """One Forgejo/Gitea installation, behind the `ForgeProvider` contract."""

    def __init__(self, client: ForgejoClient) -> None:
        self._client = client

    @property
    def host(self) -> str:
        return self._client.host

    # -- identity ----------------------------------------------------------

    @property
    def capabilities(self) -> ForgeCapabilities:
        """Declared per what `/api/v1` actually offers, never probed (FR-O11).

        - `supports_since` — the issues endpoint takes `since`; PR filtering
          is client-side, exactly as it is for GitHub.
        - `supports_posture=False` — there is no Dependabot-style repository
          posture surface (no vulnerability-alerts or automated-fix toggles,
          no `.github/dependabot.yml` convention worth asserting). The gap
          reports itself as a `not_supported` receipt through the collector's
          capability gate rather than as three fabricated `None`s.
        - `supports_notifications` — `/repos/{o}/{r}/notifications` exists
          and is repo-scoped, so the scope rule (FR-G15) holds the same way.
        - `supports_webhooks=False` — deferred by name in the v1 ceiling
          (D10), same as GitHub.
        """
        return ForgeCapabilities(
            hosts=(self._client.host,),
            supports_since=True,
            supports_posture=False,
            supports_notifications=True,
            supports_webhooks=False,
        )

    def list_repos(self) -> Iterable[ForgeRepo]:
        """`GET /user/repos` — what *this credential* can see, for the picker
        (#180). Enumeration, never a crawl: the token is the scope."""
        payloads = self._client.get("/user/repos", limit=DEFAULT_PER_PAGE)
        for item in _as_list(payloads):
            name = item.get("name")
            owner = _login(item.get("owner"))
            if not name or not owner:
                continue
            yield ForgeRepo(
                owner=owner,
                name=str(name),
                default_branch=item.get("default_branch"),
                visibility=("private" if item.get("private", False) else "public"),
                web_url=item.get("html_url")
                or f"https://{self._client.host}/{owner}/{name}",
            )

    def parse(self, repo_url: str | None) -> RepoRef | None:
        return parse_repo_url(repo_url, (self._client.host,))

    def subject_key(self, ref: RepoRef, number: int | None) -> str:
        """`forge:{host}/{owner}/{repo}#{n}` — the #171 scheme.

        Host-qualified because a second Forgejo installation can hold the
        same `owner/repo`; `gh:` keys stay unqualified for the same reason
        in reverse (D5) — github.com is one host and rekeying its history
        would be churn with no reader. `number` is widened past the
        protocol's `int` exactly as GitHub's is: a collector hands it
        straight from an untrusted payload.
        """
        return f"forge:{ref.host}/{ref.owner}/{ref.repo}#{number}"

    def number_of(self, subject_key: str | None) -> int | None:
        """`forge:host/owner/repo#123` -> 123."""
        if not subject_key or "#" not in subject_key:
            return None
        _, _, tail = subject_key.partition("#")
        return int(tail) if tail.isdigit() else None

    # -- URLs (the provider owns its own address space) ---------------------

    def clone_url(self, ref: RepoRef) -> str:
        return f"https://{ref.host}/{ref.owner}/{ref.repo}.git"

    def web_url(self, ref: RepoRef) -> str:
        return f"https://{ref.host}/{ref.owner}/{ref.repo}"

    def describe(self, ref: RepoRef) -> dict[str, Any] | None:
        """The repository's metadata, or `None` when it is not visible."""
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
        # `type=issues` keeps pull requests out server-side; the belt-and-
        # braces `pull_request` check below guards older installs that ignore
        # the parameter. No `sort` exists on this endpoint, so ascending-by-
        # update — the order the watermark machinery walks — is applied
        # locally, the way the GitHub provider already does for pulls.
        params: dict[str, str | int] = {
            "state": "all",
            "type": "issues",
            "limit": DEFAULT_PER_PAGE,
        }
        if since:
            params["since"] = since
        payloads = self._client.get(f"/repos/{ref.owner}/{ref.repo}/issues", **params)
        # Not GitHub's `"pull_request" in item`: Gitea/Forgejo carry the field
        # on *every* issue, `null` for the real ones, so presence would drop
        # everything and only non-null marks a PR.
        items = [
            item for item in _as_list(payloads) if item.get("pull_request") is None
        ]
        items.sort(key=lambda item: str(item.get("updated_at") or ""))
        for item in items:
            yield ForgeIssue(
                number=int(item.get("number", 0)),
                title=item.get("title", ""),
                state=item.get("state", "open"),
                repo=ref.slug,
                labels=_label_names(item.get("labels")),
                author=_login(item.get("user")),
                assignees=tuple(
                    login
                    for login in (
                        _login(assignee) for assignee in item.get("assignees") or []
                    )
                    if login
                ),
                comments=int(item.get("comments", 0)),
                updated_at=item.get("updated_at"),
                closed_at=item.get("closed_at"),
                source_url=item.get("html_url"),
            )

    def pulls_updated_since(
        self, ref: RepoRef, since: str | None
    ) -> Iterable[ForgePull]:
        # `sort=recentupdate` is newest-first; there is no server-side
        # `since` on pulls (same as GitHub), so filter locally and yield
        # ascending so the caller advances its watermark the same way.
        payloads = self._client.get(
            f"/repos/{ref.owner}/{ref.repo}/pulls",
            state="all",
            sort="recentupdate",
            limit=DEFAULT_PER_PAGE,
        )
        pulls = [
            ForgePull(
                number=int(item.get("number", 0)),
                title=item.get("title", ""),
                state=item.get("state", "open"),
                repo=ref.slug,
                draft=bool(item.get("draft", False)),
                merged=bool(item.get("merged") or item.get("merged_at")),
                author=_login(item.get("user")),
                head=(item.get("head") or {}).get("sha"),
                head_ref=(item.get("head") or {}).get("ref"),
                base=(item.get("base") or {}).get("ref"),
                body=item.get("body"),
                labels=_label_names(item.get("labels")),
                mergeable=_forgejo_mergeable(item),
                updated_at=item.get("updated_at"),
                closed_at=item.get("closed_at"),
                source_url=item.get("html_url"),
            )
            for item in _as_list(payloads)
        ]
        if since:
            pulls = [p for p in pulls if p.updated_at is None or p.updated_at >= since]
        pulls.sort(key=lambda p: p.updated_at or "")
        yield from pulls

    def releases(self, ref: RepoRef) -> Iterable[ForgeRelease]:
        payloads = self._client.get(
            f"/repos/{ref.owner}/{ref.repo}/releases", limit=DEFAULT_PER_PAGE
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
        # Forgejo Actions, where the install has them: the tasks endpoint
        # mirrors GitHub's shape down to the `workflow_runs` envelope. An
        # older install without Actions answers 404, which `get` returns as
        # `None` — an honest empty, not an error (FR-O4).
        payloads = self._client.get(
            f"/repos/{ref.owner}/{ref.repo}/actions/tasks", limit=20
        )
        runs = payloads.get("workflow_runs", []) if isinstance(payloads, dict) else []
        for item in runs:
            if not isinstance(item, dict):
                continue
            status = item.get("status")
            yield ForgeCheck(
                revision=item.get("head_sha", ""),
                check=item.get("name", "workflow"),
                repo=ref.slug,
                status=status,
                # One field upstream, two in the model: a terminal status is
                # the conclusion; anything in flight honestly has none yet.
                conclusion=status if status in _TERMINAL_CHECK_STATES else None,
                branch=item.get("head_branch"),
                event=item.get("event"),
                run_number=item.get("run_number"),
                updated_at=item.get("updated_at") or item.get("created_at"),
                source_url=item.get("url"),
            )

    def labels(self, ref: RepoRef) -> Iterable[ForgeLabel]:
        payloads = self._client.get(
            f"/repos/{ref.owner}/{ref.repo}/labels", limit=DEFAULT_PER_PAGE
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
        """Not offered by this forge — and the capability says so.

        The collector gates on `supports_posture` and never reaches here;
        if some future caller asks anyway, three `None`s are the honest
        "could not tell", which is not the same answer as "off" (FR-D6).
        """
        return ForgePosture(
            version_updates_config=None,
            vulnerability_alerts=None,
            automated_security_fixes=None,
            repo=ref.slug,
        )

    def notifications(self, ref: RepoRef) -> Iterable[ForgeNotification]:
        # Per-repository, never the account-wide inbox (FR-G15). `all=true`
        # keeps read threads: `unread` is a field, not a collection filter.
        payloads = self._client.get(
            f"/repos/{ref.owner}/{ref.repo}/notifications",
            limit=DEFAULT_PER_PAGE,
            all="true",
        )
        for item in _as_list(payloads):
            subject = item.get("subject") or {}
            yield ForgeNotification(
                thread=str(item.get("id", "")),
                repo=ref.slug,
                # Forgejo's thread model carries no reason and no per-thread
                # read timestamp; `None` is the honest value for both, not a
                # guess (FR-O11's rule applied to fields rather than kinds).
                reason=None,
                unread=bool(item.get("unread", False)),
                title=subject.get("title", ""),
                subject_type=subject.get("type"),
                updated_at=item.get("updated_at"),
                last_read_at=None,
                source_url=_subject_web_url(subject.get("url")),
            )

    # -- write surface (append-only by construction, FR-B4) ----------------

    def comment(self, ref: RepoRef, number: int, body: str) -> WriteBackResult:
        return self._post(
            ref,
            endpoint=f"/repos/{ref.owner}/{ref.repo}/issues/{number}/comments",
            payload={"body": body},
            number=number,
        )

    def create_issue(
        self,
        ref: RepoRef,
        *,
        title: str,
        body: str,
        labels: list[str] | None = None,
    ) -> WriteBackResult:
        """Open a new issue; never edits or replaces an existing one.

        Two appends rather than one: Forgejo's create-issue option takes
        label *ids*, so labels ride the labels endpoint — which accepts
        names — after the create. A create that lands with labels that do
        not is still a created issue, and the result says which half held.
        """
        created = self._post(
            ref,
            endpoint=f"/repos/{ref.owner}/{ref.repo}/issues",
            payload={"title": title, "body": body},
        )
        if created.outcome != "succeeded" or not labels:
            return created
        number = self.number_of(created.subject_key)
        if number is None:
            return created
        labelled = self.add_labels(ref, number, labels)
        if labelled.outcome == "succeeded":
            return created
        return WriteBackResult(
            outcome="succeeded",
            detail=(
                f"{created.detail}; labels did not land: {labelled.detail}"
                if created.detail
                else f"labels did not land: {labelled.detail}"
            ),
            source_url=created.source_url,
            subject_key=created.subject_key,
        )

    def add_labels(
        self, ref: RepoRef, number: int, labels: list[str]
    ) -> WriteBackResult:
        """Add labels by name. POST appends; the replacing verb (PUT) is not
        spoken here, so a label somebody set upstream cannot be removed."""
        return self._post(
            ref,
            endpoint=f"/repos/{ref.owner}/{ref.repo}/issues/{number}/labels",
            payload={"labels": labels},
            number=number,
        )

    def set_state(self, ref: RepoRef, number: int, state: str) -> WriteBackResult:
        if state not in ("closed", "open"):
            msg = f"{state!r} is not a state; use 'closed' or 'open'"
            raise ValueError(msg)
        return self._post(
            ref,
            endpoint=f"/repos/{ref.owner}/{ref.repo}/issues/{number}",
            payload={"state": state},
            number=number,
            method="PATCH",
        )

    def create_repo(
        self, name: str, *, private: bool, description: str | None = None
    ) -> ForgeRepo:
        """`POST /user/repos` — a new repository under this credential (#182).

        No auto-init, for the same reason as GitHub: the local history is
        about to be pushed and a generated first commit would make the very
        first push non-fast-forward. A name that already exists is Forgejo's
        409 (422 on some versions for an invalid duplicate), and either is
        the **typed refusal** (`RemoteRepoExists`) — that repository is
        somebody's state, and attaching to it is `forge.link`'s explicit act.
        """
        payload: dict[str, Any] = {
            "name": name,
            "private": private,
            "auto_init": False,
        }
        if description:
            payload["description"] = description
        try:
            response = self._client.send("/user/repos", payload)
        except ForgejoUnavailable as exc:
            if "409" in str(exc) or "422" in str(exc):
                msg = (
                    f"{self._client.host} already has a repository named "
                    f"{name!r} reachable by this account; `forge.publish` "
                    "never adopts or overwrites an existing remote — pick "
                    "another name, or attach to the existing repository with "
                    "`forge link` after setting the project's repo_url"
                )
                raise RemoteRepoExists(msg) from exc
            raise
        answer = response if isinstance(response, dict) else {}
        owner = _login(answer.get("owner"))
        if not owner:
            msg = (
                f"{self._client.host} accepted the repository create but "
                "returned no owner, so there is no address to push to"
            )
            raise ForgejoUnavailable(msg)
        created = str(answer.get("name") or name)
        return ForgeRepo(
            owner=owner,
            name=created,
            default_branch=answer.get("default_branch"),
            visibility=("private" if answer.get("private", private) else "public"),
            web_url=answer.get("html_url")
            or f"https://{self._client.host}/{owner}/{created}",
        )

    def _post(
        self,
        ref: RepoRef,
        *,
        endpoint: str,
        payload: dict[str, Any],
        number: int | None = None,
        method: str = "POST",
    ) -> WriteBackResult:
        """One append upstream, reported the way the ledger reads it.

        Mirrors the GitHub writer's result shape exactly — outcome, the
        provider's own subject key, a detail naming method and endpoint — so
        `forge actions` reads identically whichever forge answered.
        """
        try:
            response = self._client.send(endpoint, payload, method=method)
        except ForgejoUnavailable as exc:
            # Never fatal to the declared write: the local change stands and
            # the ledger records that the upstream half did not land.
            return WriteBackResult(outcome="failed", detail=str(exc))
        if response is None:
            return WriteBackResult(
                outcome="failed", detail=f"{endpoint} returned nothing (404?)"
            )
        answer = response if isinstance(response, dict) else {}
        html_url = answer.get("html_url")
        upstream_number = answer.get("number", number)
        return WriteBackResult(
            outcome="succeeded",
            source_url=None if html_url is None else str(html_url),
            subject_key=(
                None
                if upstream_number is None
                else self.subject_key(ref, upstream_number)
            ),
            detail=json.dumps({"method": method, "endpoint": endpoint}),
        )


def _as_list(payloads: object) -> list[dict[str, Any]]:
    """Narrow a GET result to the list of dicts a reader can iterate.

    `None` (404) and `NO_CONTENT` (204) both mean "nothing to read here",
    which is the honest answer, not an error (FR-O4).
    """
    if payloads is None or payloads is NO_CONTENT or not isinstance(payloads, list):
        return []
    return [item for item in payloads if isinstance(item, dict)]


def _login(user: object) -> str | None:
    """A Gitea user's login. The API carries both `login` and `username`
    (historically identical); either satisfies, neither is invented."""
    if not isinstance(user, dict):
        return None
    login = user.get("login") or user.get("username")
    return None if not login else str(login)


def _forgejo_mergeable(item: dict[str, Any]) -> str | None:
    """Gitea/Forgejo reports mergeability as a boolean; normalise it to the
    same `clean`/`dirty` vocabulary the GitHub provider yields, or `None` when
    the payload does not say."""
    mergeable = item.get("mergeable")
    if not isinstance(mergeable, bool):
        return None
    return "clean" if mergeable else "dirty"


def _label_names(labels: object) -> tuple[str, ...]:
    if not isinstance(labels, list):
        return ()
    return tuple(
        str(label["name"])
        for label in labels
        if isinstance(label, dict) and label.get("name")
    )


def _subject_web_url(api_url: object) -> str | None:
    """Turn the API resource URL a notification subject carries into one a
    person can open: `https://host/api/v1/repos/o/r/issues/5` →
    `https://host/o/r/issues/5`. Forgejo's web paths match its API paths
    once the `/api/v1/repos` prefix is gone (`/pulls/{n}` included)."""
    if not isinstance(api_url, str) or not api_url:
        return None
    replaced = api_url.replace("/api/v1/repos/", "/", 1)
    return replaced if replaced != api_url else api_url


__all__ = [
    "ForgejoClient",
    "ForgejoProvider",
    "ForgejoUnavailable",
    "parse_repo_url",
]
