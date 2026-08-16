"""A small read-only GitHub API client.

Deliberately built on `urllib` rather than a third-party HTTP library: this
is an optional adapter making a handful of GET requests, and the core must
stay installable — and fully functional — without it (NFR-PO1, NFR-PO3).

The token is read from a *file* and never from argv or a URL (FR-S7), which
also means it never appears in a process listing or a shell history.
"""

from __future__ import annotations

import json
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Protocol

from vogt.errors import VogtError

API_ROOT = "https://api.github.com"
USER_AGENT = "vogt"
DEFAULT_TIMEOUT_SECONDS = 20
#: One page is plenty for a sweep. Backfill of full history is M5 (FR-O5b),
#: and a collector that silently paginates forever turns one slow repository
#: into a stalled estate sweep.
DEFAULT_PER_PAGE = 100


class Transport(Protocol):
    """How this client actually talks, so tests never need a network.

    Carries the body and method as well as the URL: write-back has to be
    assertable, and a transport that cannot see what is being sent upstream
    cannot verify the one thing that matters — that onboarding mutates
    nothing (FR-B3).
    """

    def __call__(
        self,
        url: str,
        headers: dict[str, str],
        body: bytes = b"",
        method: str = "GET",
    ) -> tuple[int, bytes]: ...


class GitHubUnavailable(VogtError):
    """GitHub could not be reached or refused the request.

    Never fatal to a sweep: the sweeper records the collector as `partial` or
    `failed` and the affected subjects stay "not collected".
    """

    code = "github_unavailable"
    http_status = 502


@dataclass(frozen=True)
class GitHubClient:
    """Read-only access to one GitHub installation."""

    token: str | None = None
    api_root: str = API_ROOT
    transport: Transport | None = None
    timeout: int = DEFAULT_TIMEOUT_SECONDS

    @classmethod
    def from_token_file(
        cls, path: Path | None, *, transport: Transport | None = None
    ) -> GitHubClient | None:
        """Build a client, or `None` when the adapter is not configured.

        `None` is the ordinary case, not an error: no token file means no
        GitHub adapter, which means forge subjects are simply not collected.
        """
        if path is None:
            return None
        resolved = Path(path).expanduser()
        if not resolved.is_file():
            return None
        token = resolved.read_text(encoding="utf-8").strip()
        if not token:
            return None
        return cls(token=token, transport=transport)

    def get(self, path: str, **params: str | int) -> Any:
        """GET one resource, returning parsed JSON, or `None` for a 404."""
        query = urllib.parse.urlencode({k: str(v) for k, v in params.items()})
        url = f"{self.api_root}{path}" + (f"?{query}" if query else "")
        headers = {
            "Accept": "application/vnd.github+json",
            "X-GitHub-Api-Version": "2022-11-28",
            "User-Agent": USER_AGENT,
        }
        if self.token:
            headers["Authorization"] = f"Bearer {self.token}"

        status, body = self._fetch(url, headers)
        if status == 404:
            # A repository that is gone or private to us is a fact to record
            # as absence, not an exception to raise.
            return None
        if status == 403:
            msg = f"GitHub refused {path} (403): rate limited or unauthorised"
            raise GitHubUnavailable(msg)
        if status >= 400:
            msg = f"GitHub returned {status} for {path}"
            raise GitHubUnavailable(msg)
        return json.loads(body.decode("utf-8"))

    def send(self, path: str, payload: dict[str, Any], *, method: str = "POST") -> Any:
        """Make a change upstream. The only mutating method on this client.

        Deliberately one method, and deliberately not called `request`: every
        call site that changes somebody else's data is greppable, and there
        is no DELETE anywhere in the product (FR-B4).
        """
        if method not in ("POST", "PATCH"):
            msg = f"{method} is not an additive operation; write-back is forward-only"
            raise GitHubUnavailable(msg)

        url = f"{self.api_root}{path}"
        headers = {
            "Accept": "application/vnd.github+json",
            "X-GitHub-Api-Version": "2022-11-28",
            "User-Agent": USER_AGENT,
            "Content-Type": "application/json",
        }
        if self.token:
            headers["Authorization"] = f"Bearer {self.token}"

        body = json.dumps(payload).encode("utf-8")
        status, response = self._fetch(url, headers, body=body, method=method)
        if status == 404:
            return None
        if status >= 400:
            msg = f"GitHub returned {status} for {method} {path}"
            raise GitHubUnavailable(msg)
        return json.loads(response.decode("utf-8")) if response.strip() else {}

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
            msg = f"GitHub unreachable: {exc}"
            raise GitHubUnavailable(msg) from exc


#: The one host this adapter can read. Stated as a value rather than left
#: implicit in `repo_of`'s parsing, so an operation can tell a caller *why*
#: it read nothing instead of reporting zero and leaving them to guess.
SUPPORTED_HOST = "github.com"


def unsupported_reason(repo_url: str | None) -> str | None:
    """Why this adapter cannot read that repository, or `None` if it can.

    An empty success is the failure mode this exists to prevent. `forge
    onboard` against a Forgejo project returned `issues: 0, pull_requests: 0,
    …, detail: null` for a repository with an open issue in it — byte-identical
    to the honest answer for a repository with no history at all. Half the
    estate's remaining import queue is Forgejo-hosted, and the import playbook
    reads an empty consolidation as a signal, so that signal was unreadable
    for exactly the repositories it was most needed on.
    """
    if not repo_url:
        return (
            "this project declares no repository URL, so there is no forge to "
            "read — which is 'not collected', not 'there is nothing'"
        )
    if repo_of(repo_url) is not None:
        return None
    host = repo_url.split("://")[-1].split("/")[0] or repo_url
    return (
        f"the GitHub adapter cannot read {host}; it reads {SUPPORTED_HOST} only, "
        "so nothing was collected here and no conclusion should be drawn from "
        "the counts"
    )


def repo_of(repo_url: str | None) -> tuple[str, str] | None:
    """Extract `(owner, repo)` from a project's repository URL.

    A project with no GitHub URL is not an error — it is a project that does
    not live on GitHub, which the product supports as a first-class case.
    """
    if not repo_url:
        return None
    candidate = repo_url.strip().removeprefix("git+")
    for prefix in ("https://", "http://", "ssh://"):
        candidate = candidate.removeprefix(prefix)
    candidate = candidate.replace("git@github.com:", "github.com/")
    candidate = candidate.removesuffix(".git").strip("/")
    if not candidate.startswith("github.com/"):
        return None
    parts = candidate[len("github.com/") :].split("/")
    if len(parts) < 2 or not parts[0] or not parts[1]:
        return None
    return parts[0], parts[1]
