"""Authorization: scopes, and the two gates a write passes through.

ARCHITECTURE.md sets out the rules this module implements:

- **Identity is never a caller-supplied field**. A principal comes
  from a token, from mTLS, from a trusted proxy, or from the local OS user.
  There is deliberately no code path that reads it from a request body.
- **Writes are double-gated**: the server must be started with
  writes enabled *and* the principal must hold the scope. Both are checked
  at `tools/list` and again at `tools/call`, so an agent's tool list is
  exactly what it can do.
- **Ungranted tools are invisible, not erroring.** A tool that appears and
  then refuses teaches an agent to retry; a tool that is absent teaches it
  to do something else.
- **Both allow and deny decisions are audited**.
"""

from __future__ import annotations

import base64
import hashlib
import hmac
import secrets
from dataclasses import dataclass, field
from datetime import datetime
from typing import Literal

Scope = Literal["read", "work.write", "project.write", "admin", "writeback"]

ALL_SCOPES: tuple[Scope, ...] = (
    "read",
    "work.write",
    "project.write",
    "admin",
    "writeback",
)

#: `admin` implies everything. Nothing else implies anything: `work.write`
#: does not grant `project.write`, because registering a project and filing a
#: bug are different powers and an agent usually needs only one.
#:
#: `writeback` gates exactly one operation — `forge.writeback`, which arms or
#: disarms a project's upstream pushing. It is deliberately *not* implied by
#: `project.write`: managing projects and deciding that this instance may
#: speak to a forge on your behalf are different powers, and the second is the
#: one with effects outside this instance. Causing an individual upstream
#: write still needs `work.write`, because it is a consequence of commenting
#: or transitioning; what this scope decides is whether that consequence is
#: switched on at all (`docs/ARCHITECTURE.md`).
IMPLIED: dict[Scope, frozenset[Scope]] = {
    "admin": frozenset(ALL_SCOPES),
    "project.write": frozenset({"project.write", "read"}),
    "work.write": frozenset({"work.write", "read"}),
    "writeback": frozenset({"writeback", "read"}),
    "read": frozenset({"read"}),
}

TOKEN_PREFIX = "vogt_"
#: 32 bytes of urandom, base32-ish via token_urlsafe. Long enough that
#: guessing is not a strategy, short enough to paste.
TOKEN_ENTROPY_BYTES = 32


class AuthDecisionCode:
    """Why a request was allowed or refused, as a stable code."""

    LOCAL = "local_principal"
    TOKEN_OK = "token_valid"
    NO_CREDENTIAL = "no_credential"
    UNKNOWN_TOKEN = "unknown_token"
    REVOKED = "token_revoked"
    EXPIRED = "token_expired"
    DISABLED_ACTOR = "actor_disabled"
    MISSING_SCOPE = "missing_scope"
    WRITES_DISABLED = "writes_disabled"
    #: A password login named a user that does not exist or gave the wrong
    #: password. One code for both: the holder is told neither.
    BAD_PASSWORD = "bad_password"
    #: A password login refused before the password was even checked, because
    #: that username has failed too often too recently.
    LOGIN_THROTTLED = "login_throttled"
    #: A password login that succeeded and minted a session token.
    LOGIN_OK = "login_ok"


#: What a token row is for. Authentication treats every kind identically;
#: the kind exists so "every browser session this person holds" is a
#: question the store can answer.
TokenKind = Literal["api", "session", "agent"]


@dataclass(frozen=True)
class Grant:
    """What a principal is allowed to do."""

    scopes: frozenset[Scope]
    #: False when the server was started read-only. The first of the two
    #: gates: no token can grant a write the process refuses to make.
    writes_enabled: bool = True

    def effective(self) -> frozenset[Scope]:
        granted: set[Scope] = set()
        for scope in self.scopes:
            granted |= IMPLIED.get(scope, frozenset({scope}))
        return frozenset(granted)

    def allows(self, scope: str, *, mutating: bool) -> tuple[bool, str]:
        """Whether this grant permits an operation, and why not if it does not."""
        if mutating and not self.writes_enabled:
            return False, AuthDecisionCode.WRITES_DISABLED
        if scope not in self.effective():
            return False, AuthDecisionCode.MISSING_SCOPE
        return True, AuthDecisionCode.TOKEN_OK


#: What the local, unauthenticated loopback path gets. `DEPLOYMENT.md`:
#: loopback has no authentication, and the principal is `local:<os-user>`.
#: It is full access because the caller already has the data directory —
#: pretending otherwise would be theatre, not security.
LOCAL_GRANT = Grant(scopes=frozenset({"admin"}), writes_enabled=True)


@dataclass(frozen=True)
class Credential:
    """A token as issued: the secret, shown once, and its stored form."""

    secret: str
    token_hash: str
    scopes: tuple[Scope, ...] = field(default=())


def hash_token(secret: str) -> str:
    """Hash a token for storage.

    SHA-256 rather than a password hash on purpose: a token is 256 bits of
    urandom, not a human-chosen password, so there is nothing for a slow KDF
    to protect against — and a slow hash on every request would be a
    per-request cost for no gain.
    """
    return hashlib.sha256(secret.encode("utf-8")).hexdigest()


def matches(secret: str, token_hash: str) -> bool:
    """Constant-time comparison, so timing does not leak the prefix."""
    return hmac.compare_digest(hash_token(secret), token_hash)


#: The shortest operator-supplied secret `adopt` will accept. Well under
#: what `issue` produces, and far above anything guessable: the point is to
#: refuse a placeholder like "changeme" that would otherwise become a
#: working credential, not to second-guess a generated value.
MIN_ADOPTED_SECRET_LEN = 24


def issue(scopes: tuple[Scope, ...]) -> Credential:
    """Mint a new token. The secret is returned once and never stored."""
    secret = TOKEN_PREFIX + secrets.token_urlsafe(TOKEN_ENTROPY_BYTES)
    return adopt(secret, scopes)


def adopt(secret: str, scopes: tuple[Scope, ...]) -> Credential:
    """Take an operator-supplied secret as a credential, minting nothing.

    The counterpart to `issue`, and the reason it exists is symmetry: the
    engine's session token is already a value the operator chooses and
    declares to both halves, while the core token could only be minted —
    which forced every first boot through "deploy, get a 401, exec in, mint,
    edit config, deploy again".

    Storage is identical either way. A token is a hash in this database and
    nothing else, so where the secret came from changes the bootstrap and
    changes nothing afterwards.
    """
    if len(secret) < MIN_ADOPTED_SECRET_LEN:
        msg = (
            f"a supplied token must be at least {MIN_ADOPTED_SECRET_LEN} "
            f"characters, not {len(secret)} — generate one with "
            "`openssl rand -hex 32`"
        )
        raise ValueError(msg)
    return Credential(secret=secret, token_hash=hash_token(secret), scopes=scopes)


def parse_scopes(raw: str) -> tuple[Scope, ...]:
    """Parse a comma-separated scope list, rejecting anything unknown."""
    parsed: list[Scope] = []
    for part in raw.split(","):
        candidate = part.strip()
        if not candidate:
            continue
        if candidate not in ALL_SCOPES:
            msg = f"unknown scope {candidate!r} (known: {', '.join(ALL_SCOPES)})"
            raise ValueError(msg)
        parsed.append(candidate)
    if not parsed:
        msg = "a token needs at least one scope"
        raise ValueError(msg)
    return tuple(parsed)


def is_expired(expires_at: datetime | None, *, now: datetime) -> bool:
    return expires_at is not None and expires_at <= now


# -- passwords ---------------------------------------------------------------
#
# The human credential. Everything above is about tokens — 256 bits of urandom
# that nobody chooses — and is hashed with plain SHA-256 for exactly that
# reason. A password is chosen by a person, is short, and is reused, so it
# gets the slow, salted hash a person's secret needs. scrypt is in the
# standard library, which keeps the core's dependency set where it is.

PASSWORD_SCHEME = "scrypt"
#: CPU/memory cost. 2**15 with r=8 is ~32 MiB and a few tens of milliseconds
#: on commodity hardware: negligible per login, ruinous per guess.
PASSWORD_SCRYPT_N = 2**15
PASSWORD_SCRYPT_R = 8
PASSWORD_SCRYPT_P = 1
PASSWORD_SALT_BYTES = 16
PASSWORD_HASH_BYTES = 32
#: The floor on a chosen password. Short enough not to be a nuisance; the
#: throttle in the service is what defeats guessing, not this.
MIN_PASSWORD_LEN = 8
MAX_PASSWORD_LEN = 1024


def hash_password(password: str) -> str:
    """Hash a chosen password for storage: `scrypt$n$r$p$<salt>$<hash>`."""
    if len(password) < MIN_PASSWORD_LEN:
        msg = f"a password must be at least {MIN_PASSWORD_LEN} characters"
        raise ValueError(msg)
    if len(password) > MAX_PASSWORD_LEN:
        msg = f"a password must be at most {MAX_PASSWORD_LEN} characters"
        raise ValueError(msg)
    salt = secrets.token_bytes(PASSWORD_SALT_BYTES)
    digest = _scrypt(
        password, salt, PASSWORD_SCRYPT_N, PASSWORD_SCRYPT_R, PASSWORD_SCRYPT_P
    )
    return "$".join(
        (
            PASSWORD_SCHEME,
            str(PASSWORD_SCRYPT_N),
            str(PASSWORD_SCRYPT_R),
            str(PASSWORD_SCRYPT_P),
            _b64(salt),
            _b64(digest),
        )
    )


def verify_password(password: str, stored: str) -> bool:
    """Constant-time check of a password against its stored hash.

    A malformed stored value verifies as false rather than raising: the row is
    the operator's to repair, and the caller's answer is still "no".
    """
    try:
        scheme, n_raw, r_raw, p_raw, salt_raw, digest_raw = stored.split("$")
        if scheme != PASSWORD_SCHEME:
            return False
        salt = _unb64(salt_raw)
        expected = _unb64(digest_raw)
        candidate = _scrypt(password, salt, int(n_raw), int(r_raw), int(p_raw))
    except (ValueError, TypeError):
        return False
    return hmac.compare_digest(candidate, expected)


def _scrypt(password: str, salt: bytes, n: int, r: int, p: int) -> bytes:
    return hashlib.scrypt(
        password.encode("utf-8"),
        salt=salt,
        n=n,
        r=r,
        p=p,
        maxmem=128 * n * r * 2,
        dklen=PASSWORD_HASH_BYTES,
    )


def _b64(raw: bytes) -> str:
    return base64.urlsafe_b64encode(raw).decode("ascii").rstrip("=")


def _unb64(text: str) -> bytes:
    padded = text + "=" * (-len(text) % 4)
    return base64.urlsafe_b64decode(padded.encode("ascii"))


#: A login name: lower-case letters, digits, dots, dashes and underscores,
#: 2–64 characters. Case-folded before storage so `Ada` and `ada` are one
#: person; the actor's display name keeps the case the person typed.
USERNAME_MIN_LEN = 2
USERNAME_MAX_LEN = 64
_USERNAME_ALPHABET = frozenset("abcdefghijklmnopqrstuvwxyz0123456789._-")


def normalise_username(raw: str) -> str:
    """Fold and validate a login name, raising `ValueError` when it is not one."""
    username = raw.strip().lower()
    if not USERNAME_MIN_LEN <= len(username) <= USERNAME_MAX_LEN:
        msg = (
            f"a username is {USERNAME_MIN_LEN} to {USERNAME_MAX_LEN} characters, "
            f"not {len(username)}"
        )
        raise ValueError(msg)
    if not set(username) <= _USERNAME_ALPHABET or username[0] in "._-":
        msg = (
            "a username is lower-case letters, digits, '.', '-' and '_', "
            "starting with a letter or digit"
        )
        raise ValueError(msg)
    return username
