"""GitHub App installation tokens for the forge adapter.

A personal token's core rate limit is per *user*, shared with everything else
on that account; an App installation has its own. The App signs a short RS256
JWT with its private key and exchanges it for an installation token that lives
an hour. This module keeps each installation's token until five minutes before
it expires.

One App can be installed on several accounts and organisations. With no
installation configured, the installation for a request is discovered from the
owner in its path (`GET /repos/{owner}/{repo}/installation`, else
`/orgs/{owner}/installation` or `/users/{owner}/installation`) and remembered,
so a forge config that spans several organisations works with one App.

Signed with `cryptography`, which the core already depends on, rather than a
JWT library: RS256 is one PKCS#1 v1.5 signature over two base64url segments.
Setup is documented in `docs/GITHUB_APP.md`.
"""

from __future__ import annotations

import base64
import json
import threading
import time
from collections.abc import Callable
from datetime import datetime
from pathlib import Path
from typing import TYPE_CHECKING, Any

from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import padding, rsa

from vogt.adapters.github.client import (
    API_ROOT,
    USER_AGENT,
    GitHubClient,
    GitHubUnavailable,
    Transport,
)

if TYPE_CHECKING:
    from vogt.config import VogtConfig

#: GitHub refuses a JWT whose lifetime exceeds ten minutes; 9 leaves room for
#: clock skew on top of the backdated `iat`.
JWT_LIFETIME_SECONDS = 540
#: `iat` is backdated so a server clock slightly ahead of ours still accepts it.
JWT_BACKDATE_SECONDS = 60
#: Refresh this long before `expires_at`, so a sweep never starts on a token
#: that dies halfway through it.
REFRESH_MARGIN_SECONDS = 300
#: GitHub documents installation tokens as valid for one hour.
DEFAULT_TOKEN_LIFETIME_SECONDS = 3600


def _b64url(raw: bytes) -> str:
    return base64.urlsafe_b64encode(raw).rstrip(b"=").decode("ascii")


def sign_jwt(private_key: rsa.RSAPrivateKey, issuer: str, now: float) -> str:
    """An RS256 App JWT: `iss` = App or client ID, `iat` = now−60, `exp` = now+540."""
    header = {"alg": "RS256", "typ": "JWT"}
    claims = {
        "iat": int(now) - JWT_BACKDATE_SECONDS,
        "exp": int(now) + JWT_LIFETIME_SECONDS,
        "iss": str(issuer),
    }
    signing_input = ".".join(
        _b64url(json.dumps(part, separators=(",", ":")).encode("utf-8"))
        for part in (header, claims)
    )
    signature = private_key.sign(
        signing_input.encode("ascii"), padding.PKCS1v15(), hashes.SHA256()
    )
    return f"{signing_input}.{_b64url(signature)}"


def _parse_expiry(value: object) -> float | None:
    if not isinstance(value, str) or not value:
        return None
    try:
        return datetime.fromisoformat(value.replace("Z", "+00:00")).timestamp()
    except ValueError:
        return None


def _owner_repo(path: str | None) -> tuple[str | None, str | None]:
    """The owner (and repository, if any) a request path is about."""
    if not path:
        return None, None
    parts = path.split("?", 1)[0].strip("/").split("/")
    if len(parts) >= 3 and parts[0] == "repos":
        return parts[1], parts[2]
    if len(parts) >= 2 and parts[0] in ("orgs", "users"):
        return parts[1], None
    return None, None


class GitHubAppAuth:
    """A token provider that mints and caches installation tokens.

    `GitHubClient` calls `token(path)` before every request; it only goes to
    the network when that installation's cached token is missing or within
    five minutes of expiry. `invalidate(path)` drops it, which the client does
    once on a 401.
    """

    def __init__(
        self,
        *,
        app_id: str,
        private_key: rsa.RSAPrivateKey,
        installation_id: int | None = None,
        api_root: str = API_ROOT,
        transport: Transport | None = None,
        clock: Callable[[], float] = time.time,
    ) -> None:
        self.app_id = str(app_id)
        self.installation_id = installation_id
        self._key = private_key
        self._api_root = api_root
        self._transport = transport
        self._clock = clock
        self._lock = threading.Lock()
        #: installation id → (token, expires_at)
        self._tokens: dict[int, tuple[str, float]] = {}
        #: owner login (lower-cased) → installation id
        self._owners: dict[str, int] = {}
        #: installation id → its account (`login`, `type`)
        self._accounts: dict[int, dict[str, Any]] = {}

    @classmethod
    def from_key_file(
        cls,
        *,
        app_id: str,
        key_file: Path,
        installation_id: int | None = None,
        transport: Transport | None = None,
    ) -> GitHubAppAuth | None:
        """Load the PEM, or `None` when the file is missing or not an RSA key."""
        resolved = Path(key_file).expanduser()
        if not resolved.is_file():
            return None
        try:
            key = serialization.load_pem_private_key(
                resolved.read_bytes(), password=None
            )
        except (ValueError, TypeError):
            return None
        if not isinstance(key, rsa.RSAPrivateKey):
            return None
        return cls(
            app_id=app_id,
            private_key=key,
            installation_id=installation_id,
            transport=transport,
        )

    # -- the TokenProvider surface ------------------------------------------

    def cache_key(self, path: str | None = None) -> str:
        """Stable across token refreshes: the rate limit is per installation."""
        return f"github-app:{self.app_id}:{self._installation_for(path)}"

    def token(self, path: str | None = None) -> str:
        installation = self._installation_for(path)
        with self._lock:
            cached = self._tokens.get(installation)
            if (
                cached is not None
                and self._clock() < cached[1] - REFRESH_MARGIN_SECONDS
            ):
                return cached[0]
            token, expires_at = self._mint(installation)
            self._tokens[installation] = (token, expires_at)
            return token

    def invalidate(self, path: str | None = None) -> None:
        installation = self._installation_for(path)
        with self._lock:
            self._tokens.pop(installation, None)

    def account(self, path: str | None = None) -> dict[str, Any] | None:
        """The account (`login`, `type`) of the installation `path` routes to."""
        installation = self._installation_for(path)
        with self._lock:
            known = self._accounts.get(installation)
        if known is None:
            payload = self._app_get(f"/app/installations/{installation}")
            account = payload.get("account") if isinstance(payload, dict) else None
            known = account if isinstance(account, dict) else {}
            with self._lock:
                self._accounts[installation] = known
        return known or None

    # -- internals -----------------------------------------------------------

    def jwt(self) -> str:
        return sign_jwt(self._key, self.app_id, self._clock())

    def _installation_for(self, path: str | None) -> int:
        if self.installation_id is not None:
            return self.installation_id
        owner, repo = _owner_repo(path)
        with self._lock:
            if owner is not None and owner.lower() in self._owners:
                return self._owners[owner.lower()]
            known = set(self._owners.values())
        if owner is None:
            # Nothing in the path names an owner (`/user`, a bare publish);
            # one discovered installation is unambiguous, more are not.
            if len(known) == 1:
                return next(iter(known))
            msg = (
                "this GitHub request names no owner, and with no "
                "github_app_installation_id configured the App cannot tell "
                "which installation to use"
            )
            raise GitHubUnavailable(msg)
        installation = self._discover(owner, repo)
        with self._lock:
            self._owners[owner.lower()] = installation
        return installation

    def _discover(self, owner: str, repo: str | None) -> int:
        candidates = [f"/orgs/{owner}/installation", f"/users/{owner}/installation"]
        if repo is not None:
            candidates.insert(0, f"/repos/{owner}/{repo}/installation")
        for candidate in candidates:
            payload = self._app_get(candidate)
            if isinstance(payload, dict) and isinstance(payload.get("id"), int):
                account = payload.get("account")
                if isinstance(account, dict):
                    with self._lock:
                        self._accounts[payload["id"]] = account
                return int(payload["id"])
        msg = f"the GitHub App is not installed on {owner!r}"
        raise GitHubUnavailable(msg)

    def _app_request(
        self, path: str, *, method: str = "GET", body: bytes | None = None
    ) -> tuple[int, Any]:
        """A request authenticated as the App itself (the JWT).

        Bypasses the client's ETag cache and rate budget: the JWT changes on
        every call, and App endpoints have their own limit.
        """
        client = GitHubClient(api_root=self._api_root, transport=self._transport)
        headers = {
            "Accept": "application/vnd.github+json",
            "X-GitHub-Api-Version": "2022-11-28",
            "User-Agent": USER_AGENT,
            "Authorization": f"Bearer {self.jwt()}",
        }
        status, raw, _headers = client._fetch(
            f"{self._api_root}{path}", headers, body=body, method=method
        )
        payload = json.loads(raw.decode("utf-8")) if raw.strip() else None
        return status, payload

    def _app_get(self, path: str) -> object:
        status, payload = self._app_request(path)
        if status == 404:
            return None
        if status >= 400:
            msg = f"GitHub returned {status} for App request {path}"
            raise GitHubUnavailable(msg)
        return payload

    def _mint(self, installation: int) -> tuple[str, float]:
        path = f"/app/installations/{installation}/access_tokens"
        status, payload = self._app_request(path, method="POST", body=b"{}")
        token = payload.get("token") if isinstance(payload, dict) else None
        if status >= 400 or not token:
            msg = (
                f"GitHub App {self.app_id} could not mint a token for "
                f"installation {installation} (HTTP {status})"
            )
            raise GitHubUnavailable(msg)
        expires = _parse_expiry(payload.get("expires_at"))
        if expires is None:
            expires = self._clock() + DEFAULT_TOKEN_LIFETIME_SECONDS
        return str(token), expires


#: One `GitHubAppAuth` per App, installation setting, key file and transport.
#: The registry builds a provider per project per sweep; without this each one
#: would mint its own installation token.
_AUTHS: dict[tuple[str, int | None, str, int], GitHubAppAuth] = {}
_AUTHS_LOCK = threading.Lock()


def app_auth_from_config(
    config: VogtConfig, *, transport: Transport | None = None
) -> GitHubAppAuth | None:
    """The configured App, or `None` unless its ID and a usable key are set."""
    if not config.github_app_id or config.github_app_private_key_file is None:
        return None
    key = (
        config.github_app_id,
        config.github_app_installation_id,
        str(Path(config.github_app_private_key_file).expanduser()),
        id(transport),
    )
    with _AUTHS_LOCK:
        cached = _AUTHS.get(key)
        if cached is not None and cached._transport is transport:
            return cached
        auth = GitHubAppAuth.from_key_file(
            app_id=config.github_app_id,
            key_file=config.github_app_private_key_file,
            installation_id=config.github_app_installation_id,
            transport=transport,
        )
        if auth is not None:
            _AUTHS[key] = auth
        return auth


def clear_app_auth_cache() -> None:
    """Forget every cached App token (tests; a rotated key)."""
    with _AUTHS_LOCK:
        _AUTHS.clear()


def github_client_for(
    config: VogtConfig,
    token_file: Path | None,
    *,
    transport: Transport | None = None,
) -> GitHubClient | None:
    """The App client when the App is configured, else the token-file client."""
    auth = app_auth_from_config(config, transport=transport)
    if auth is not None:
        return GitHubClient(token_provider=auth, transport=transport)
    return GitHubClient.from_token_file(token_file, transport=transport)


__all__ = [
    "GitHubAppAuth",
    "app_auth_from_config",
    "clear_app_auth_cache",
    "github_client_for",
    "sign_jwt",
]
