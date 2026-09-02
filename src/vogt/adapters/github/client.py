"""A small read-only GitHub API client.

Deliberately built on `urllib` rather than a third-party HTTP library: this
is an optional adapter making a handful of GET requests, and the core must
stay installable — and fully functional — without it (NFR-PO1, NFR-PO3).

The token is read from a *file* and never from argv or a URL (FR-S7), which
also means it never appears in a process listing or a shell history.
"""

from __future__ import annotations

import json
import re
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


class _NoContent:
    """A successful response that carried no body.

    Distinct from `None`, which this client uses for 404 — and the difference
    is the whole point. GitHub answers the repository toggles with 204 when
    they are *on* and 404 when they are *off*, so flattening the two would
    report every enabled security setting as disabled.
    """

    __slots__ = ()

    def __bool__(self) -> bool:
        """Falsy, so `if not payload` reads correctly without knowing about it.

        Identity is what distinguishes 204 from 404 (`is None`); truthiness is
        what every caller that just wants "was there anything" already tests.
        """
        return False

    def __repr__(self) -> str:  # pragma: no cover - debugging aid
        return "NO_CONTENT"


#: The one instance; compare with `is`.
NO_CONTENT = _NoContent()


@dataclass(frozen=True)
class GitHubIdentity:
    """Who a token belongs to, and what it may do.

    `login` comes from the `/user` body; `scopes` is the raw `X-OAuth-Scopes`
    response header (a comma-separated list, or empty). Scopes ride a header
    rather than the body, and the `Transport` seam a test substitutes returns
    only status and body — so through a fake, `scopes` is honestly empty
    rather than guessed. On the real network path the header is read (#179).
    """

    login: str
    scopes: str


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

        status, body, _headers = self._fetch(url, headers)
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
        if status == 204 or not body.strip():
            # 204 is the documented answer for the repository toggles, and the
            # collector that reads them said so in its own docstring while this
            # line fed the empty body to a JSON parser. The result was
            # `JSONDecodeError: Expecting value: line 1 column 1 (char 0)`,
            # which took down the whole `gh-posture` sweep for the project
            # rather than one field of it.
            return NO_CONTENT
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
        status, response, _headers = self._fetch(url, headers, body=body, method=method)
        if status == 404:
            return None
        if status >= 400:
            msg = f"GitHub returned {status} for {method} {path}"
            raise GitHubUnavailable(msg)
        return json.loads(response.decode("utf-8")) if response.strip() else {}

    def identity(self) -> GitHubIdentity | None:
        """Who this token is, or `None` when it is invalid (401/403/404).

        The one place a token is *validated* rather than merely used: linking a
        per-actor PAT calls this before storing anything, so an invalid paste
        is refused instead of encrypted (#179). `scopes` is read from the
        `X-OAuth-Scopes` header — empty through a `Transport` fake, which cannot
        carry headers, and populated on the real network path.
        """
        url = f"{self.api_root}/user"
        headers = {
            "Accept": "application/vnd.github+json",
            "X-GitHub-Api-Version": "2022-11-28",
            "User-Agent": USER_AGENT,
        }
        if self.token:
            headers["Authorization"] = f"Bearer {self.token}"
        status, body, response_headers = self._fetch(url, headers)
        if status in (401, 403, 404):
            return None
        if status >= 400:
            msg = f"GitHub returned {status} for /user"
            raise GitHubUnavailable(msg)
        payload = json.loads(body.decode("utf-8")) if body.strip() else {}
        login = payload.get("login") if isinstance(payload, dict) else None
        if not login:
            return None
        return GitHubIdentity(
            login=str(login), scopes=response_headers.get("x-oauth-scopes", "")
        )

    def _fetch(
        self,
        url: str,
        headers: dict[str, str],
        *,
        body: bytes | None = None,
        method: str = "GET",
    ) -> tuple[int, bytes, dict[str, str]]:
        if self.transport is not None:
            # The transport seam carries no headers — a fake cannot supply the
            # scope header — so callers that need one degrade honestly.
            status, payload = self.transport(url, headers, body or b"", method)
            return status, payload, {}
        request = urllib.request.Request(url, headers=headers, data=body, method=method)
        try:
            with urllib.request.urlopen(  # https only; api_root is not caller-set
                request, timeout=self.timeout
            ) as response:
                return (
                    int(response.status),
                    bytes(response.read()),
                    {k.lower(): v for k, v in response.headers.items()},
                )
        except urllib.error.HTTPError as exc:  # pragma: no cover - network shape
            return int(exc.code), bytes(exc.read()), {}
        except (urllib.error.URLError, TimeoutError, OSError) as exc:
            msg = f"GitHub unreachable: {exc}"
            raise GitHubUnavailable(msg) from exc


#: The one host this adapter can read. The registry (`adapters/forge`) now
#: owns "which forge reads what" and "why nothing was read"; this stays as the
#: fact `repo_of` parses against, named rather than buried in the string.
SUPPORTED_HOST = "github.com"

#: What a forge permits in an owner or repository name — the same strict
#: pattern `imports.py` validates against (#517). Owner/repo are interpolated
#: raw into f-string URL builds (`f"{api_root}/repos/{owner}/{repo}/..."`), so
#: a value carrying `..`, `?`, `#`, `%` — settable by any `project.write`
#: token via `repo_url` — would steer the stored credential's request to an
#: arbitrary path/query on the forge host. Reject it at the parser: an
#: unparseable value is "not a GitHub repo", the ordinary None answer.
_VALID_NAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]*")


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
    if not _VALID_NAME.fullmatch(parts[0]) or not _VALID_NAME.fullmatch(parts[1]):
        return None
    return parts[0], parts[1]
