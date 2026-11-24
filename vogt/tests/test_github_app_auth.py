"""The forge adapter can authenticate as a GitHub App installation.

Tested through the `Transport` seam, with fixture IDs and owners only: the
JWT is a real RS256 signature over a generated key, each installation token is
cached until five minutes before expiry, a 401 refreshes it once, the
installation is discovered per owner when none is configured, App config wins
over the token file except for notifications, and `create_repo` targets an
organisation installation's `/orgs/{owner}/repos`.
"""

from __future__ import annotations

import base64
import json
import logging
from collections.abc import Iterator
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

import pytest
from cryptography.hazmat.primitives import hashes, serialization
from cryptography.hazmat.primitives.asymmetric import padding, rsa

from vogt.adapters.forge.github import GitHubProvider
from vogt.adapters.forge.models import RepoRef
from vogt.adapters.forge.registry import github_provider
from vogt.adapters.github.app_auth import (
    NOT_INSTALLED_TTL_SECONDS,
    GitHubAppAuth,
    clear_app_auth_cache,
    github_client_for,
    sign_jwt,
)
from vogt.adapters.github.client import (
    ETAGS,
    RATE_BUDGET,
    GitHubClient,
    GitHubRateLimited,
    GitHubUnavailable,
)
from vogt.adapters.github.collectors import github_collectors
from vogt.adapters.github.writeback import ForgeWriter
from vogt.config import VogtConfig

APP_ID = "12345"
CLIENT_ID = "Iv1.0123456789abcdef"
NOW = 1_800_000_000.0
#: Fixture installations: owner → (installation id, account type).
INSTALLS = {"example-org": (111, "Organization"), "example-user": (222, "User")}


@pytest.fixture(autouse=True)
def _fresh_state() -> Iterator[None]:
    clear_app_auth_cache()
    ETAGS.clear()
    RATE_BUDGET.clear()
    yield
    clear_app_auth_cache()
    ETAGS.clear()
    RATE_BUDGET.clear()


@pytest.fixture(scope="module")
def key() -> rsa.RSAPrivateKey:
    return rsa.generate_private_key(public_exponent=65537, key_size=2048)


@pytest.fixture
def key_file(tmp_path: Path, key: rsa.RSAPrivateKey) -> Path:
    path = tmp_path / "app.pem"
    path.write_bytes(
        key.private_bytes(
            serialization.Encoding.PEM,
            serialization.PrivateFormat.PKCS8,
            serialization.NoEncryption(),
        )
    )
    return path


def _unb64(segment: str) -> bytes:
    return base64.urlsafe_b64decode(segment + "=" * (-len(segment) % 4))


def _iso(epoch: float) -> str:
    return datetime.fromtimestamp(epoch, UTC).strftime("%Y-%m-%dT%H:%M:%SZ")


def _json(status: int, payload: object) -> tuple[int, bytes]:
    return status, json.dumps(payload).encode()


class _GitHub:
    """A fake GitHub: installations per owner, numbered tokens, plain reads."""

    def __init__(self) -> None:
        self.minted: list[int] = []
        self.expires_at = NOW + 3600
        self.revoked: set[str] = set()
        self.calls: list[tuple[str, str, str]] = []  # method, path, bearer
        #: Forced status and extra headers for JWT-signed (`/app`, lookup) calls.
        self.app_status: int | None = None
        self.app_headers: dict[str, str] = {}
        self.installations = list(INSTALLS)

    def _by_id(self, installation: int) -> tuple[str, str] | None:
        for owner, (number, kind) in INSTALLS.items():
            if number == installation:
                return owner, kind
        return None

    def __call__(
        self, url: str, headers: dict[str, str], body: bytes = b"", method: str = "GET"
    ) -> Any:
        del body
        path = url.removeprefix("https://api.github.com")
        bearer = headers.get("Authorization", "").removeprefix("Bearer ")
        self.calls.append((method, path, bearer))
        parts = path.strip("/").split("/")
        if parts[0] == "app" or parts[-1] == "installation":
            status, payload = self._app(parts)
            if self.app_status is not None:
                status = self.app_status
            return status, payload, self.app_headers
        if bearer in self.revoked:
            return _json(401, {"message": "Bad credentials"})
        if method == "POST":
            return _json(201, {"name": "fresh", "owner": {"login": "example-org"}})
        return _json(200, {"ok": True})

    def _app(self, parts: list[str]) -> tuple[int, bytes]:
        if parts == ["app", "installations"]:
            return _json(
                200,
                [
                    {
                        "id": INSTALLS[o][0],
                        "account": {"login": o, "type": INSTALLS[o][1]},
                    }
                    for o in self.installations
                ],
            )
        if parts[0] == "app":
            installation = int(parts[2])
            found = self._by_id(installation)
            if found is None:
                return _json(404, {"message": "Not Found"})
            if parts[-1] == "access_tokens":
                self.minted.append(installation)
                token = f"ghs_{installation}_{len(self.minted)}"
                return _json(201, {"token": token, "expires_at": _iso(self.expires_at)})
            return _json(200, {"account": {"login": found[0], "type": found[1]}})
        if parts[-1] == "installation":
            owner = parts[1]
            # A repository lookup finds any installation; the org and user
            # lookups only find their own kind of account.
            if owner not in INSTALLS or (
                parts[0] != "repos"
                and (parts[0] == "orgs") != (INSTALLS[owner][1] == "Organization")
            ):
                return _json(404, {"message": "Not Found"})
            number, kind = INSTALLS[owner]
            return _json(200, {"id": number, "account": {"login": owner, "type": kind}})
        return _json(404, {"message": "Not Found"})

    def app_calls(self) -> list[str]:
        return [
            p
            for _m, p, _b in self.calls
            if p.startswith("/app/") or p.endswith("/installation")
        ]

    def bearers(self, path: str) -> list[str]:
        return [bearer for _m, p, bearer in self.calls if p == path]

    def posts(self) -> list[str]:
        return [p for m, p, _b in self.calls if m == "POST" and "/app/" not in p]


def _low() -> dict[str, str]:
    return {
        "X-RateLimit-Remaining": "3",
        "X-RateLimit-Reset": str(int(NOW) + 10_000_000),
        "X-RateLimit-Resource": "core",
    }


def _auth(
    key: rsa.RSAPrivateKey,
    fake: _GitHub,
    clock: list[float] | None = None,
    *,
    installation_id: int | None = None,
    app_id: str = APP_ID,
) -> GitHubAppAuth:
    now = clock if clock is not None else [NOW]
    return GitHubAppAuth(
        app_id=app_id,
        private_key=key,
        installation_id=installation_id,
        transport=fake,
        clock=lambda: now[0],
    )


# -- the JWT ------------------------------------------------------------------


@pytest.mark.parametrize("issuer", [APP_ID, CLIENT_ID])
def test_the_jwt_is_rs256_signed_with_the_app_claims(
    key: rsa.RSAPrivateKey, issuer: str
) -> None:
    token = sign_jwt(key, issuer, NOW)
    header, claims, signature = token.split(".")
    assert json.loads(_unb64(header)) == {"alg": "RS256", "typ": "JWT"}
    assert json.loads(_unb64(claims)) == {
        "iat": int(NOW) - 60,
        "exp": int(NOW) + 540,
        "iss": issuer,
    }
    key.public_key().verify(  # raises InvalidSignature if wrong
        _unb64(signature),
        f"{header}.{claims}".encode(),
        padding.PKCS1v15(),
        hashes.SHA256(),
    )


def test_the_token_exchange_is_authenticated_by_the_jwt(
    key: rsa.RSAPrivateKey,
) -> None:
    fake = _GitHub()
    assert _auth(key, fake, installation_id=111).token() == "ghs_111_1"
    method, path, bearer = fake.calls[0]
    assert (method, path) == ("POST", "/app/installations/111/access_tokens")
    assert json.loads(_unb64(bearer.split(".")[1]))["iss"] == APP_ID


# -- caching and refresh ------------------------------------------------------


def test_the_token_is_cached_until_five_minutes_before_expiry(
    key: rsa.RSAPrivateKey,
) -> None:
    fake = _GitHub()
    clock = [NOW]
    auth = _auth(key, fake, clock, installation_id=111)
    assert auth.token() == "ghs_111_1"
    clock[0] = NOW + 3600 - 301
    assert auth.token() == "ghs_111_1"
    clock[0] = NOW + 3600 - 300
    fake.expires_at = clock[0] + 3600
    assert auth.token() == "ghs_111_2"
    assert fake.minted == [111, 111]


def test_a_401_refreshes_the_token_once_and_retries(key: rsa.RSAPrivateKey) -> None:
    fake = _GitHub()
    auth = _auth(key, fake, installation_id=111)
    client = GitHubClient(token_provider=auth, transport=fake)
    assert client.get("/repos/example-org/r") == {"ok": True}
    fake.revoked.add("ghs_111_1")
    assert client.get("/repos/example-org/r/releases") == {"ok": True}
    assert fake.bearers("/repos/example-org/r/releases") == ["ghs_111_1", "ghs_111_2"]


def test_a_second_401_is_not_retried_again(key: rsa.RSAPrivateKey) -> None:
    fake = _GitHub()
    fake.revoked.update({"ghs_111_1", "ghs_111_2", "ghs_111_3"})
    auth = _auth(key, fake, installation_id=111)
    client = GitHubClient(token_provider=auth, transport=fake)
    with pytest.raises(GitHubUnavailable):
        client.get("/repos/example-org/r")
    assert fake.minted == [111, 111]


def test_a_fixed_token_is_not_refreshed_on_401() -> None:
    fake = _GitHub()
    fake.revoked.add("pat-fixture")
    client = GitHubClient(token="pat-fixture", transport=fake)
    with pytest.raises(GitHubUnavailable, match="401"):
        client.get("/repos/example-org/r")
    assert fake.minted == []
    assert len(fake.calls) == 1


# -- installation discovery ---------------------------------------------------


def test_installations_are_discovered_per_owner(key: rsa.RSAPrivateKey) -> None:
    fake = _GitHub()
    client = GitHubClient(token_provider=_auth(key, fake), transport=fake)
    client.get("/repos/example-org/a")
    client.get("/repos/example-org/b")
    client.get("/repos/example-user/c")
    assert fake.bearers("/repos/example-org/a") == ["ghs_111_1"]
    assert fake.bearers("/repos/example-org/b") == ["ghs_111_1"]
    assert fake.bearers("/repos/example-user/c") == ["ghs_222_2"]
    # One discovery per owner, then remembered.
    assert fake.bearers("/repos/example-org/a/installation")
    assert not fake.bearers("/repos/example-org/b/installation")
    assert fake.minted == [111, 222]


def test_an_owner_without_the_app_falls_back_to_the_pat(
    key: rsa.RSAPrivateKey,
) -> None:
    """F1: not an error; the owner is read with the personal token."""
    fake = _GitHub()
    auth = _auth(key, fake)
    auth.fallback_token = "pat-fixture"
    client = GitHubClient(token_provider=auth, transport=fake)
    assert client.get("/repos/stranger/r") == {"ok": True}
    assert fake.bearers("/repos/stranger/r") == ["pat-fixture"]
    assert fake.minted == []


def test_not_installed_is_cached_per_owner_with_a_ttl(
    key: rsa.RSAPrivateKey,
) -> None:
    """F1: no further App calls for that owner until the TTL runs out."""
    fake = _GitHub()
    clock = [NOW]
    auth = _auth(key, fake, clock)
    auth.fallback_token = "pat-fixture"
    client = GitHubClient(token_provider=auth, transport=fake)
    client.get("/repos/stranger/a")
    app_calls = len(fake.app_calls())
    assert app_calls == 3  # repo, org and user installation lookups
    client.get("/repos/stranger/b")
    clock[0] = NOW + NOT_INSTALLED_TTL_SECONDS - 1
    client.get("/repos/stranger/c")
    assert len(fake.app_calls()) == app_calls
    clock[0] = NOW + NOT_INSTALLED_TTL_SECONDS
    client.get("/repos/stranger/d")
    assert len(fake.app_calls()) > app_calls


def test_an_owner_without_the_app_and_no_pat_is_read_anonymously(
    key: rsa.RSAPrivateKey,
) -> None:
    fake = _GitHub()
    client = GitHubClient(token_provider=_auth(key, fake), transport=fake)
    assert client.get("/repos/stranger/r") == {"ok": True}
    assert fake.bearers("/repos/stranger/r") == [""]


def test_the_fallback_owner_shares_the_pat_budget(key: rsa.RSAPrivateKey) -> None:
    fake = _GitHub()
    auth = _auth(key, fake)
    auth.fallback_token = "pat-fixture"
    pat = GitHubClient(token="pat-fixture")
    assert auth.cache_key("/repos/stranger/r") == pat._budget_key()


def test_a_fallback_pat_401_is_not_retried(key: rsa.RSAPrivateKey) -> None:
    fake = _GitHub()
    fake.revoked.add("pat-fixture")
    auth = _auth(key, fake)
    auth.fallback_token = "pat-fixture"
    client = GitHubClient(token_provider=auth, transport=fake)
    with pytest.raises(GitHubUnavailable, match="401"):
        client.get("/repos/stranger/r")
    assert fake.bearers("/repos/stranger/r") == ["pat-fixture"]


def test_a_failed_lookup_is_not_cached_as_not_installed(
    key: rsa.RSAPrivateKey,
) -> None:
    fake = _GitHub()
    fake.app_status = 500
    auth = _auth(key, fake)
    with pytest.raises(GitHubUnavailable):
        auth.token("/repos/example-org/a")
    fake.app_status = None
    assert auth.token("/repos/example-org/a") == "ghs_111_1"


# -- F2: the rate budget comes first ----------------------------------------


def test_a_low_installation_budget_mints_nothing(key: rsa.RSAPrivateKey) -> None:
    fake = _GitHub()
    auth = _auth(key, fake, installation_id=111)
    # The budget reads response headers, which the client lower-cases.
    low = {k.lower(): v for k, v in _low().items()}
    RATE_BUDGET.observe(auth.cache_key(), low)
    client = GitHubClient(token_provider=auth, transport=fake)
    with pytest.raises(GitHubRateLimited):
        client.get("/repos/example-org/a")
    with pytest.raises(GitHubRateLimited):
        client.send("/repos/example-org/a/issues", {"title": "t"})
    assert fake.calls == []


def test_a_low_app_budget_stops_lookups_and_mints(key: rsa.RSAPrivateKey) -> None:
    fake = _GitHub()
    auth = _auth(key, fake)
    assert auth.token("/repos/example-org/a") == "ghs_111_1"
    # A JWT-signed response reports the App's budget is nearly spent.
    fake.app_headers = _low()
    fake.installations = ["example-org"]
    auth._app_get("/app/installations")
    calls = len(fake.calls)
    with pytest.raises(GitHubRateLimited):
        auth.token("/repos/example-user/a")  # would need a lookup
    auth.invalidate("/repos/example-org/a")
    with pytest.raises(GitHubRateLimited):
        auth.token("/repos/example-org/a")  # would need a mint
    assert len(fake.calls) == calls


def test_a_rate_limited_lookup_is_not_cached_as_not_installed(
    key: rsa.RSAPrivateKey,
) -> None:
    fake = _GitHub()
    fake.app_status = 403
    fake.app_headers = {**_low(), "X-RateLimit-Remaining": "0"}
    auth = _auth(key, fake)
    auth.fallback_token = "pat-fixture"
    with pytest.raises(GitHubRateLimited):
        auth.token("/repos/example-org/a")
    RATE_BUDGET.clear()
    fake.app_status = None
    fake.app_headers = {}
    assert auth.token("/repos/example-org/a") == "ghs_111_1"


def test_a_configured_installation_skips_discovery(key: rsa.RSAPrivateKey) -> None:
    fake = _GitHub()
    auth = _auth(key, fake, installation_id=222)
    GitHubClient(token_provider=auth, transport=fake).get("/repos/example-org/a")
    assert not [p for _m, p, _b in fake.calls if p.endswith("/installation")]
    assert fake.minted == [222]


def test_installations_have_separate_rate_budgets(key: rsa.RSAPrivateKey) -> None:
    fake = _GitHub()
    auth = _auth(key, fake)
    assert auth.cache_key("/repos/example-org/a") != auth.cache_key(
        "/repos/example-user/a"
    )
    assert auth.cache_key("/repos/example-org/a") == auth.cache_key(
        "/repos/example-org/b"
    )


def test_an_ownerless_request_needs_one_known_installation(
    key: rsa.RSAPrivateKey,
) -> None:
    fake = _GitHub()
    auth = _auth(key, fake)
    with pytest.raises(GitHubUnavailable, match="names no owner"):
        auth.token()
    auth.token("/repos/example-org/a")
    assert auth.token() == "ghs_111_1"
    auth.token("/repos/example-user/a")
    with pytest.raises(GitHubUnavailable, match="names no owner"):
        auth.token()


# -- configuration and wiring ------------------------------------------------


def _config(
    key_file: Path, token_file: Path | None, installation_id: int | None = None
) -> VogtConfig:
    return VogtConfig(
        github_app_id=APP_ID,
        github_app_installation_id=installation_id,
        github_app_private_key_file=key_file,
        github_token_file=token_file,
    )


def _pat_file(tmp_path: Path) -> Path:
    path = tmp_path / "github_token"
    path.write_text("pat-fixture\n")
    return path


def test_app_config_takes_precedence_over_the_token_file(
    tmp_path: Path, key_file: Path
) -> None:
    fake = _GitHub()
    token_file = _pat_file(tmp_path)
    config = _config(key_file, token_file, installation_id=111)
    client = github_client_for(config, token_file, transport=fake)
    assert client is not None
    assert client.bearer() == "ghs_111_1"


def test_without_the_app_the_token_file_is_used(tmp_path: Path) -> None:
    token_file = _pat_file(tmp_path)
    client = github_client_for(VogtConfig(github_token_file=token_file), token_file)
    assert client is not None
    assert client.token == "pat-fixture"
    assert client.token_provider is None


def test_an_unreadable_key_falls_back_to_the_token_file(tmp_path: Path) -> None:
    bad = tmp_path / "bad.pem"
    bad.write_text("not a key")
    token_file = _pat_file(tmp_path)
    client = github_client_for(_config(bad, token_file), token_file)
    assert client is not None
    assert client.token == "pat-fixture"


def test_providers_share_one_cached_installation_token(key_file: Path) -> None:
    fake = _GitHub()
    config = _config(key_file, None)
    ref = RepoRef(host="github.com", owner="example-org", repo="a")
    for _ in range(3):
        provider = github_provider(config, transport=fake)
        assert provider is not None
        assert provider.clone_token(ref) == "ghs_111_1"
    assert fake.minted == [111]


def test_notifications_stay_on_the_pat(tmp_path: Path, key_file: Path) -> None:
    fake = _GitHub()
    token_file = _pat_file(tmp_path)
    collectors = {
        c.name: c
        for c in github_collectors(_config(key_file, token_file), transport=fake)
    }
    notifications = collectors.pop("gh-notifications")
    assert notifications._client.token == "pat-fixture"  # type: ignore[attr-defined]
    assert collectors
    for collector in collectors.values():
        assert collector._client.token_provider is not None  # type: ignore[attr-defined]


def test_with_the_app_and_no_pat_notifications_are_not_registered(
    key_file: Path,
) -> None:
    names = {c.name for c in github_collectors(_config(key_file, None))}
    assert names
    assert "gh-notifications" not in names


# -- create_repo ---------------------------------------------------------------


def _writer(client: GitHubClient) -> ForgeWriter:
    return GitHubProvider(client)._writer()


def test_create_repo_uses_the_org_endpoint_for_an_org_installation(
    key: rsa.RSAPrivateKey,
) -> None:
    fake = _GitHub()
    auth = _auth(key, fake, installation_id=111)
    _writer(GitHubClient(token_provider=auth, transport=fake)).create_repo(
        name="fresh", private=True
    )
    assert fake.posts() == ["/orgs/example-org/repos"]


def test_create_repo_uses_the_user_endpoint_for_a_user_installation(
    key: rsa.RSAPrivateKey,
) -> None:
    fake = _GitHub()
    auth = _auth(key, fake, installation_id=222)
    _writer(GitHubClient(token_provider=auth, transport=fake)).create_repo(
        name="fresh", private=True
    )
    assert fake.posts() == ["/user/repos"]


def test_create_repo_with_a_pat_is_unchanged() -> None:
    fake = _GitHub()
    _writer(GitHubClient(token="pat-fixture", transport=fake)).create_repo(
        name="fresh", private=True
    )
    assert fake.posts() == ["/user/repos"]


# -- F3: publishing without a configured installation ------------------------


def test_publish_discovers_the_only_installation(key: rsa.RSAPrivateKey) -> None:
    fake = _GitHub()
    fake.installations = ["example-org"]
    auth = _auth(key, fake)
    _writer(GitHubClient(token_provider=auth, transport=fake)).create_repo(
        name="fresh", private=True
    )
    assert "/app/installations" in fake.app_calls()
    assert fake.posts() == ["/orgs/example-org/repos"]


def test_publish_refuses_to_guess_between_installations(
    key: rsa.RSAPrivateKey,
) -> None:
    fake = _GitHub()
    auth = _auth(key, fake)
    with pytest.raises(GitHubUnavailable, match="github_app_installation_id"):
        _writer(GitHubClient(token_provider=auth, transport=fake)).create_repo(
            name="fresh", private=True
        )
    assert fake.posts() == []


# -- F4: a broken key is loud -------------------------------------------------


def test_a_missing_key_file_warns_once_and_uses_the_pat(
    tmp_path: Path, caplog: pytest.LogCaptureFixture
) -> None:
    token_file = _pat_file(tmp_path)
    config = _config(tmp_path / "absent.pem", token_file)
    with caplog.at_level(logging.WARNING, logger="vogt.github.app"):
        for _ in range(3):
            client = github_client_for(config, token_file)
            assert client is not None
            assert client.token == "pat-fixture"
    warnings = [r for r in caplog.records if r.levelno == logging.WARNING]
    assert len(warnings) == 1
    assert "falling back to github_token_file" in warnings[0].getMessage()


def test_an_invalid_key_file_warns(
    tmp_path: Path, caplog: pytest.LogCaptureFixture
) -> None:
    bad = tmp_path / "bad.pem"
    bad.write_text("not a key")
    with caplog.at_level(logging.WARNING, logger="vogt.github.app"):
        github_client_for(_config(bad, None), None)
    assert any("bad.pem" in r.getMessage() for r in caplog.records)
