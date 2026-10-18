"""A small read-only GitHub API client.

Deliberately built on `urllib` rather than a third-party HTTP library: this
is an optional adapter making a handful of GET requests, and the core must
stay installable — and fully functional — without it (NFR-PO1, NFR-PO3).

The token is read from a *file* and never from argv or a URL, which
also means it never appears in a process listing or a shell history.
"""

from __future__ import annotations

import hashlib
import json
import re
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
from collections import OrderedDict
from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Protocol

from vogt.errors import VogtError

API_ROOT = "https://api.github.com"
USER_AGENT = "vogt"
DEFAULT_TIMEOUT_SECONDS = 20
#: One page is plenty for a sweep. Backfill of full history is a separate
#: concern, and a collector that silently paginates forever turns one slow repository
#: into a stalled estate sweep.
DEFAULT_PER_PAGE = 100
#: Below this many core requests left in the hour, reads stop until the
#: window resets (WI-1838). The limit is per *user*, shared with every agent
#: session and CI helper on the same account, so the sweep leaves headroom.
DEFAULT_RATE_LIMIT_FLOOR = 500
#: How many conditional-request entries are kept. One per URL the sweep reads;
#: the estate is ~70 repositories at ~15 URLs each.
ETAG_CACHE_SIZE = 4096


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
    rather than guessed. On the real network path the header is read.
    """

    login: str
    scopes: str


class Transport(Protocol):
    """How this client actually talks, so tests never need a network.

    Carries the body and method as well as the URL: write-back has to be
    assertable, and a transport that cannot see what is being sent upstream
    cannot verify the one thing that matters — that onboarding mutates
    nothing.
    """

    def __call__(
        self,
        url: str,
        headers: dict[str, str],
        body: bytes = b"",
        method: str = "GET",
    ) -> tuple[int, bytes] | tuple[int, bytes, dict[str, str]]: ...

    # A transport may return response headers as a third element; the real
    # network path always does. ETags and rate-limit accounting need them,
    # and a fake that returns two elements simply opts out of both.


class GitHubUnavailable(VogtError):
    """GitHub could not be reached or refused the request.

    Never fatal to a sweep: the sweeper records the collector as `partial` or
    `failed` and the affected subjects stay "not collected".
    """

    code = "github_unavailable"
    http_status = 502


class GitHubRateLimited(GitHubUnavailable):
    """The token's core budget is below the floor; reads wait for the reset.

    Raised *before* a request is sent once the budget is known to be low, so
    a sweep over seventy repositories costs nothing while it waits instead of
    spending the last of a limit every agent session on the account shares.
    """

    code = "github_rate_limited"
    http_status = 503

    def __init__(self, message: str, *, reset_at: float | None = None) -> None:
        super().__init__(message)
        self.reset_at = reset_at


def _token_key(token: str | None) -> str:
    """A non-reversible key for per-token state; never the token itself."""
    if not token:
        return "anonymous"
    return hashlib.sha256(token.encode("utf-8")).hexdigest()[:16]


class _ConditionalCache:
    """ETag → body per (token, URL), so an unchanged resource costs nothing.

    GitHub answers `If-None-Match` with 304 when nothing changed, and a 304
    does not count against the rate limit. Process-wide because providers are
    built per project per sweep; bounded LRU so it cannot grow without limit.
    """

    def __init__(self, size: int = ETAG_CACHE_SIZE) -> None:
        self._size = size
        self._entries: OrderedDict[tuple[str, str], tuple[str, bytes]] = OrderedDict()
        self._lock = threading.Lock()

    def get(self, key: tuple[str, str]) -> tuple[str, bytes] | None:
        with self._lock:
            entry = self._entries.get(key)
            if entry is not None:
                self._entries.move_to_end(key)
            return entry

    def put(self, key: tuple[str, str], etag: str, body: bytes) -> None:
        with self._lock:
            self._entries[key] = (etag, body)
            self._entries.move_to_end(key)
            while len(self._entries) > self._size:
                self._entries.popitem(last=False)

    def clear(self) -> None:
        with self._lock:
            self._entries.clear()


class _RateBudget:
    """The last `X-RateLimit-Remaining`/`-Reset` seen per token.

    Read on every response. When remaining drops below the floor, reads are
    refused locally until the reset time passes; then the budget is unknown
    again and the next response re-learns it.
    """

    def __init__(self, clock: Callable[[], float] = time.time) -> None:
        self.clock = clock
        self.floor = DEFAULT_RATE_LIMIT_FLOOR
        self._seen: dict[str, tuple[int, float]] = {}
        self._lock = threading.Lock()

    def observe(self, token_key: str, headers: dict[str, str]) -> None:
        remaining = headers.get("x-ratelimit-remaining")
        reset = headers.get("x-ratelimit-reset")
        if remaining is None or reset is None:
            return
        # Only the core resource gates the sweep; search and GraphQL have
        # their own buckets.
        if headers.get("x-ratelimit-resource", "core") != "core":
            return
        try:
            value = (int(remaining), float(reset))
        except ValueError:
            return
        with self._lock:
            self._seen[token_key] = value

    def blocked_until(self, token_key: str) -> float | None:
        """The reset epoch while the budget is below the floor, else None."""
        with self._lock:
            seen = self._seen.get(token_key)
            if seen is None:
                return None
            remaining, reset = seen
            if self.clock() >= reset:
                del self._seen[token_key]
                return None
            return reset if remaining < self.floor else None

    def clear(self) -> None:
        with self._lock:
            self._seen.clear()


#: Process-wide: every `GitHubClient` for one token shares one budget and one
#: conditional cache, whichever collector built it.
ETAGS = _ConditionalCache()
RATE_BUDGET = _RateBudget()


def configure_rate_limit_floor(floor: int) -> None:
    """Set the remaining-requests floor below which reads pause (WI-1838)."""
    RATE_BUDGET.floor = max(0, int(floor))


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

        token_key = _token_key(self.token)
        reset_at = RATE_BUDGET.blocked_until(token_key)
        if reset_at is not None:
            when = time.strftime("%H:%M:%SZ", time.gmtime(reset_at))
            msg = (
                f"GitHub rate budget below {RATE_BUDGET.floor}; "
                f"skipping {path} until the reset at {when}"
            )
            raise GitHubRateLimited(msg, reset_at=reset_at)
        cache_key = (token_key, url)
        cached = ETAGS.get(cache_key)
        if cached is not None:
            headers["If-None-Match"] = cached[0]

        status, body, response_headers = self._fetch(url, headers)
        RATE_BUDGET.observe(token_key, response_headers)
        if status == 304 and cached is not None:
            # Unchanged since the last read, and free: replay what we kept.
            status, body = 200, cached[1]
        elif status == 200 and response_headers.get("etag"):
            ETAGS.put(cache_key, response_headers["etag"], body)
        exhausted = response_headers.get("x-ratelimit-remaining") == "0"
        if status in (403, 429) and exhausted:
            reset = response_headers.get("x-ratelimit-reset")
            msg = f"GitHub rate limit exhausted reading {path}"
            raise GitHubRateLimited(
                msg, reset_at=float(reset) if reset and reset.isdigit() else None
            )
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
        is no DELETE anywhere in the product.
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
        status, response, response_headers = self._fetch(
            url, headers, body=body, method=method
        )
        RATE_BUDGET.observe(_token_key(self.token), response_headers)
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
        is refused instead of encrypted. `scopes` is read from the
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
            # A two-element answer carries no headers — a fake that does not
            # supply them gets no scopes, ETags or rate accounting, honestly.
            answer = self.transport(url, headers, body or b"", method)
            if len(answer) == 3:
                status, payload, extra = answer
                return status, payload, {k.lower(): v for k, v in extra.items()}
            status, payload = answer[0], answer[1]
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
            # 304 and 403/429 arrive here; their headers carry the ETag
            # answer and the rate-limit reset, so keep them.
            return (
                int(exc.code),
                bytes(exc.read()),
                {k.lower(): v for k, v in (exc.headers or {}).items()},
            )
        except (urllib.error.URLError, TimeoutError, OSError) as exc:
            msg = f"GitHub unreachable: {exc}"
            raise GitHubUnavailable(msg) from exc


#: The one host this adapter can read. The registry (`adapters/forge`) now
#: owns "which forge reads what" and "why nothing was read"; this stays as the
#: fact `repo_of` parses against, named rather than buried in the string.
SUPPORTED_HOST = "github.com"

#: What a forge permits in an owner or repository name — the same strict
#: pattern `imports.py` validates against. Owner/repo are interpolated
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
    # Fold the scp-like `git@github.com:owner/repo` form into a path before the
    # scheme strip, then drop any leading scheme so a bare `github.com/owner/repo`
    # parses the same as an explicit `https://` URL.
    candidate = candidate.replace("git@github.com:", "github.com/")
    for prefix in ("https://", "http://", "ssh://"):
        candidate = candidate.removeprefix(prefix)
    # Check the host structurally rather than by substring: `urlparse` isolates
    # the true hostname, so look-alikes like `github.com.evil.com/o/r` or
    # `github.com@evil.com/o/r` resolve to their real host and are rejected. A
    # query or fragment carries injection metacharacters into no legitimate repo
    # URL, so their presence is itself disqualifying.
    parsed = urllib.parse.urlparse("https://" + candidate)
    if parsed.hostname != "github.com" or parsed.query or parsed.fragment:
        return None
    path = parsed.path.removesuffix(".git").strip("/")
    parts = path.split("/")
    if len(parts) < 2 or not parts[0] or not parts[1]:
        return None
    if not _VALID_NAME.fullmatch(parts[0]) or not _VALID_NAME.fullmatch(parts[1]):
        return None
    return parts[0], parts[1]
