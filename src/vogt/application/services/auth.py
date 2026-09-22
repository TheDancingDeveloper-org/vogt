"""Issuing, revoking and checking tokens.

The authenticating half of the principal rule: a bearer token resolves to an
actor and a
set of scopes, and *that* is the principal. Nothing here takes an identity
from a caller — the only input is the secret, and the only output is who the
secret belongs to.
"""

from __future__ import annotations

import threading
from collections import deque
from dataclasses import dataclass
from datetime import datetime, timedelta
from pathlib import Path

from vogt.application.context import AppContext
from vogt.application.models import (
    AuthDecisionListParams,
    AuthDecisionListResult,
    CreateUserParams,
    IssueTokenParams,
    IssueTokenResult,
    ListTokensParams,
    LoginParams,
    LoginResult,
    LogoutParams,
    LogoutResult,
    RemoveUserParams,
    RemoveUserResult,
    RevokeTokenParams,
    SetPasswordParams,
    TokenListResult,
    TokenResult,
    UserListParams,
    UserListResult,
    UserResult,
    WhoamiParams,
    WhoamiResult,
)
from vogt.application.services import _resolve
from vogt.application.writes import WriteOutcome, audited_write
from vogt.core.auth import (
    LOCAL_GRANT,
    AuthDecisionCode,
    Grant,
    Scope,
    adopt,
    hash_password,
    hash_token,
    is_expired,
    issue,
    normalise_username,
    parse_scopes,
    verify_password,
)
from vogt.core.entities import Actor, AuthDecision, PasswordCredential, Token
from vogt.core.principal import Principal
from vogt.errors import Conflict, InvalidRequest, LoginThrottled, NotFound, VogtError
from vogt.storage.interface import WriteTxn

TOKEN_ISSUE = "token.issue"
TOKEN_REVOKE = "token.revoke"

TOKEN_ISSUED_EVENT = "token.issued"
TOKEN_ADOPT = "token.adopt"
TOKEN_ADOPTED_EVENT = "token.adopted"
TOKEN_REVOKED_EVENT = "token.revoked"

AUTH_LOGIN = "auth.login"
AUTH_LOGOUT = "auth.logout"
SESSION_OPENED_EVENT = "auth.session_opened"
SESSION_CLOSED_EVENT = "auth.session_closed"
USER_CREATE = "user.create"
USER_SET_PASSWORD = "user.set_password"
USER_REMOVE = "user.remove"
USER_CREATED_EVENT = "user.created"
USER_PASSWORD_SET_EVENT = "user.password_set"
USER_REMOVED_EVENT = "user.removed"

#: The name every bootstrap-adopted core token carries, so a rotation can
#: find the row it supersedes without a column for it.
BOOTSTRAP_CORE_TOKEN_NAME = "bootstrap core token"
BOOTSTRAP_AGENT_TOKEN_NAME = "bootstrap agent token"


class Unauthenticated(VogtError):
    """No usable credential was presented."""

    code = "unauthenticated"
    http_status = 401


class Forbidden(VogtError):
    """A valid principal that does not hold the required scope."""

    code = "forbidden"
    http_status = 403


@dataclass(frozen=True)
class Authenticated:
    """A resolved principal and what it may do."""

    principal: Principal
    grant: Grant
    token: Token | None = None

    @property
    def token_id(self) -> str | None:
        return None if self.token is None else self.token.id


#: How stale a token's `last_used_at` may get before a request refreshes it
#: Long enough that a busy token is not writing on every request,
#: short enough that "last used" stays operationally useful.
_TOUCH_DEBOUNCE = timedelta(minutes=5)


def authenticate(
    ctx: AppContext, *, bearer: str | None, writes_enabled: bool = True
) -> Authenticated:
    """Resolve a bearer token to a principal, or refuse.

    A refusal names its reason code but never says *which* part of the token
    was wrong: "unknown token" and "revoked token" are the same message to
    the caller and different rows in `auth_decisions`, where the operator can
    see them and the holder cannot.
    """
    if bearer is None:
        raise Unauthenticated("no bearer token presented")

    with ctx.declared.read() as view:
        token = view.token_by_hash(hash_token(bearer.strip()))
        if token is None:
            _record(ctx, decision="deny", code=AuthDecisionCode.UNKNOWN_TOKEN)
            raise Unauthenticated("the presented token is not valid")
        if token.revoked_at is not None:
            _record(
                ctx,
                decision="deny",
                code=AuthDecisionCode.REVOKED,
                token=token,
            )
            raise Unauthenticated("the presented token is not valid")
        if is_expired(token.expires_at, now=ctx.clock()):
            _record(ctx, decision="deny", code=AuthDecisionCode.EXPIRED, token=token)
            raise Unauthenticated("the presented token is not valid")
        actor = view.actor_by_id(token.actor_id)
        if actor is None or actor.disabled:
            _record(
                ctx,
                decision="deny",
                code=AuthDecisionCode.DISABLED_ACTOR,
                token=token,
            )
            raise Unauthenticated("the presented token is not valid")

    # Debounce the last-used write. touch_token opens its own
    # BEGIN IMMEDIATE write transaction, and closing the last WAL connection
    # checkpoints (~25ms) — paid on *every* authenticated request, reads
    # included. The exact last-used instant is telemetry, not a correctness
    # input, so update it at most once per token per debounce window, compared
    # against the value already loaded above (no extra read, no process state
    # that could collide across instances or tests).
    now = ctx.clock()
    if token.last_used_at is None or now - token.last_used_at >= _TOUCH_DEBOUNCE:
        ctx.declared.touch_token(token.id, at=now)
    return Authenticated(
        principal=Principal(
            identity_ref=actor.identity_ref,
            kind=actor.kind,
            display_name=actor.display_name,
        ),
        grant=Grant(
            scopes=frozenset(token.scopes),  # type: ignore[arg-type]
            writes_enabled=writes_enabled,
        ),
        token=token,
    )


def local(ctx: AppContext) -> Authenticated:
    """The loopback path: no authentication, `local:<os-user>` (DEPLOY §3)."""
    return Authenticated(principal=ctx.principal, grant=LOCAL_GRANT)


def authorize(
    ctx: AppContext,
    caller: Authenticated,
    *,
    operation: str,
    scope: str,
    mutating: bool,
    transport: str,
) -> None:
    """The second gate, recorded either way."""
    allowed, code = caller.grant.allows(scope, mutating=mutating)
    _record(
        ctx,
        decision="allow" if allowed else "deny",
        code=code,
        operation=operation,
        scope=scope,
        transport=transport,
        token=caller.token,
        identity_ref=caller.principal.identity_ref,
    )
    if allowed:
        return
    if code == AuthDecisionCode.WRITES_DISABLED:
        msg = f"{operation} is a write, and this server was started read-only"
    else:
        msg = (
            f"{operation} requires the {scope!r} scope; this token holds "
            f"{', '.join(sorted(caller.grant.scopes)) or 'nothing'}"
        )
    raise Forbidden(msg)


def _record(
    ctx: AppContext,
    *,
    decision: str,
    code: str,
    operation: str = "authenticate",
    scope: str | None = None,
    transport: str = "http",
    token: Token | None = None,
    identity_ref: str | None = None,
) -> None:
    ctx.declared.record_auth_decision(
        AuthDecision(
            id=ctx.id_factory("aut"),
            at=ctx.clock(),
            decision="allow" if decision == "allow" else "deny",
            reason_code=code,
            operation=operation,
            scope=scope,
            actor_id=None if token is None else token.actor_id,
            token_id=None if token is None else token.id,
            identity_ref=identity_ref
            or (None if token is None else token.actor_identity_ref),
            transport=transport,
        )
    )


# -- operations ------------------------------------------------------------


def issue_token(ctx: AppContext, params: IssueTokenParams) -> IssueTokenResult:
    """Mint a scoped token bound to an actor.

    The secret is returned exactly once. It is not stored, cannot be
    recovered, and never appears in any other response — losing it means
    rotating, which is the correct answer.
    """
    try:
        scopes: tuple[Scope, ...] = parse_scopes(params.scopes)
    except ValueError as exc:
        raise InvalidRequest(str(exc)) from exc

    credential = issue(scopes)
    expires_at = (
        None
        if params.expires_in_days is None
        else ctx.clock() + timedelta(days=params.expires_in_days)
    )

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[IssueTokenResult]:
        del actor
        holder = _resolve.actor(txn, params.actor)
        token = Token(
            id=ctx.id_factory("tok"),
            actor_id=holder.id,
            actor_identity_ref=holder.identity_ref,
            name=params.name,
            scopes=list(scopes),
            created_at=ctx.clock(),
            expires_at=expires_at,
        )
        txn.insert_token(token, token_hash=credential.token_hash)
        return WriteOutcome(
            result=IssueTokenResult(
                token=token,
                secret=credential.secret,
                warning=(
                    "This is the only time the secret is shown. Store it in a "
                    "file and point VOGT_TOKEN_FILE at it — never in argv or a "
                    "URL."
                ),
            ),
            entity_kind="token",
            entity_id=token.id,
            # The secret is deliberately absent from the audited payload: an
            # audit row that contains the credential is a credential leak
            # with a timestamp.
            payload={
                "actor": holder.identity_ref,
                "scopes": list(scopes),
                "name": params.name,
            },
            event_kind=TOKEN_ISSUED_EVENT,
            summary={"actor": holder.identity_ref, "scopes": list(scopes)},
        )

    return audited_write(ctx, operation=TOKEN_ISSUE, reason=params.reason, body=body)


def adopt_bootstrap_core_token(ctx: AppContext) -> str:
    """Adopt the operator-supplied core token named by configuration.

     Returns what happened, for `init` to report: `"not_configured"`,
     `"already_present"` or `"adopted"`.

     Idempotent by construction. The check is on the *hash* of the supplied
     secret, so a second boot with the same value finds its own token and
     writes nothing — which matters because `init` runs on every container
     start, not only the first.

     Two different failures, deliberately handled differently:

     *No token* — the file is absent, unreadable or empty — is `not_configured`
     and the instance comes up regardless. That is the shape a `pre_deploy`
     hook produces when the value is simply unset, and it leaves the deployment
     exactly where it is today: `/api/vogt` refusing with a named reason
    . The instance is the thing that must boot, and it does not need
     this credential for its own operation.

     *A malformed configuration* — an unknown scope, a secret too short to be
     one — raises, and startup fails. It is the r20 rule applied here: a key
     whose destination cannot be honoured is a startup error, because the
     alternative is a deployment that believes it supplied a credential and
     silently did not.
    """
    configured = ctx.config.bootstrap_core_token_file
    if configured is None:
        return "not_configured"
    try:
        secret = Path(configured).read_text(encoding="utf-8").strip()
    except OSError:
        return "not_configured"
    if not secret:
        return "not_configured"

    try:
        scopes = parse_scopes(ctx.config.bootstrap_core_token_scopes)
        credential = adopt(secret, scopes)
    except ValueError as exc:
        raise InvalidRequest(str(exc)) from exc

    with ctx.declared.read() as view:
        present = view.token_by_hash(credential.token_hash)
        if present is not None and present.revoked_at is None:
            return "already_present"

    identity_ref = ctx.config.bootstrap_core_token_actor

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[TokenResult]:
        del actor
        # Re-checked inside the transaction: two containers of one stack can
        # call `init` at the same moment, and the read above is not a lock.
        existing = txn.token_by_hash(credential.token_hash)
        if existing is not None and existing.revoked_at is None:
            return WriteOutcome(
                result=TokenResult(token=existing),
                entity_kind="token",
                entity_id=existing.id,
                payload={"adopted": False, "reason": "already present"},
                event_kind=TOKEN_ADOPTED_EVENT,
                summary={"actor": existing.actor_identity_ref},
            )
        if existing is not None:
            # The secret rotated *back* to a value a rotation revoked. The
            # hash is unique, so the row is reinstated rather than re-inserted,
            # and whatever superseded it is revoked in turn.
            superseded = _revoke_named_tokens(
                txn, existing.actor_id, name=existing.name, at=ctx.clock()
            )
            txn.reinstate_token(existing.id)
            reinstated = txn.token_by_id(existing.id)
            assert reinstated is not None  # just updated in this transaction
            return WriteOutcome(
                result=TokenResult(token=reinstated),
                entity_kind="token",
                entity_id=reinstated.id,
                payload={
                    "adopted": True,
                    "reason": "reinstated",
                    "superseded": superseded,
                },
                event_kind=TOKEN_ADOPTED_EVENT,
                summary={"actor": reinstated.actor_identity_ref},
            )
        holder = txn.actor_by_identity(identity_ref)
        if holder is None:
            holder = Actor(
                id=ctx.id_factory("act"),
                kind="agent",
                display_name=identity_ref,
                identity_ref=identity_ref,
                disabled=False,
                created_at=ctx.clock(),
            )
            txn.insert_actor(holder)
        # A changed secret is a rotation, not an addition. Every earlier
        # bootstrap token on this actor is revoked in the same transaction,
        # so the old value stops working the moment the new one is adopted
        # rather than accumulating as a live credential nobody remembers.
        superseded = _revoke_named_tokens(
            txn, holder.id, name=BOOTSTRAP_CORE_TOKEN_NAME, at=ctx.clock()
        )
        token = Token(
            id=ctx.id_factory("tok"),
            actor_id=holder.id,
            actor_identity_ref=holder.identity_ref,
            name=BOOTSTRAP_CORE_TOKEN_NAME,
            scopes=list(scopes),
            created_at=ctx.clock(),
            expires_at=None,
        )
        txn.insert_token(token, token_hash=credential.token_hash)
        return WriteOutcome(
            result=TokenResult(token=token),
            entity_kind="token",
            entity_id=token.id,
            # No secret in the payload, for the reason `issue_token` gives:
            # an audit row holding the credential is a leak with a timestamp.
            payload={
                "actor": holder.identity_ref,
                "scopes": list(scopes),
                "name": token.name,
                "source": "operator-supplied",
                "superseded": superseded,
            },
            event_kind=TOKEN_ADOPTED_EVENT,
            summary={"actor": holder.identity_ref, "scopes": list(scopes)},
        )

    audited_write(
        ctx,
        operation=TOKEN_ADOPT,
        reason="adopting the operator-supplied core token at init",
        body=body,
    )
    return "adopted"


def adopt_bootstrap_agent_token(ctx: AppContext) -> str:
    """Adopt the operator-supplied brokered agent token named by configuration.

    The session-side mirror of `adopt_bootstrap_core_token` (#199/#726): the
    pod-wide token an engine default shell or a command-launched session brokers
    becomes a deploy-time secret, so widening what sessions may do is "change the
    secret, redeploy" rather than an `admin` mint plus an Infisical rotation plus
    an engine restart. Its scopes are `agent_session_scopes` — the *same* one
    knob `session.start` uses — so both token paths share a single scope decision.

    Same contract as the core adoption, deliberately identical so an operator has
    one shape to learn: `"not_configured"` when the file is unset/unreadable/empty
    (the instance boots regardless), `"already_present"` when the secret's hash is
    already stored (idempotent across the restarts `init` runs on), `"adopted"` on
    a fresh write; a malformed scope or a too-short secret raises and startup
    fails, so a deployment cannot believe it supplied a credential and silently
    not have.
    """
    configured = ctx.config.bootstrap_agent_token_file
    if configured is None:
        return "not_configured"
    try:
        secret = Path(configured).read_text(encoding="utf-8").strip()
    except OSError:
        return "not_configured"
    if not secret:
        return "not_configured"

    try:
        scopes = parse_scopes(ctx.config.agent_session_scopes)
        credential = adopt(secret, scopes)
    except ValueError as exc:
        raise InvalidRequest(str(exc)) from exc

    with ctx.declared.read() as view:
        present = view.token_by_hash(credential.token_hash)
        if present is not None and present.revoked_at is None:
            return "already_present"

    identity_ref = ctx.config.bootstrap_agent_token_actor

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[TokenResult]:
        del actor
        # Re-checked inside the transaction: two containers of one stack can
        # call `init` at the same moment, and the read above is not a lock.
        existing = txn.token_by_hash(credential.token_hash)
        if existing is not None and existing.revoked_at is None:
            return WriteOutcome(
                result=TokenResult(token=existing),
                entity_kind="token",
                entity_id=existing.id,
                payload={"adopted": False, "reason": "already present"},
                event_kind=TOKEN_ADOPTED_EVENT,
                summary={"actor": existing.actor_identity_ref},
            )
        if existing is not None:
            # The secret rotated *back* to a value a rotation revoked. The
            # hash is unique, so the row is reinstated rather than re-inserted,
            # and whatever superseded it is revoked in turn.
            superseded = _revoke_named_tokens(
                txn, existing.actor_id, name=existing.name, at=ctx.clock()
            )
            txn.reinstate_token(existing.id)
            reinstated = txn.token_by_id(existing.id)
            assert reinstated is not None  # just updated in this transaction
            return WriteOutcome(
                result=TokenResult(token=reinstated),
                entity_kind="token",
                entity_id=reinstated.id,
                payload={
                    "adopted": True,
                    "reason": "reinstated",
                    "superseded": superseded,
                },
                event_kind=TOKEN_ADOPTED_EVENT,
                summary={"actor": reinstated.actor_identity_ref},
            )
        holder = txn.actor_by_identity(identity_ref)
        if holder is None:
            holder = Actor(
                id=ctx.id_factory("act"),
                kind="agent",
                display_name=identity_ref,
                identity_ref=identity_ref,
                disabled=False,
                created_at=ctx.clock(),
            )
            txn.insert_actor(holder)
        superseded = _revoke_named_tokens(
            txn, holder.id, name=BOOTSTRAP_AGENT_TOKEN_NAME, at=ctx.clock()
        )
        token = Token(
            id=ctx.id_factory("tok"),
            actor_id=holder.id,
            actor_identity_ref=holder.identity_ref,
            name=BOOTSTRAP_AGENT_TOKEN_NAME,
            scopes=list(scopes),
            created_at=ctx.clock(),
            expires_at=None,
        )
        txn.insert_token(token, token_hash=credential.token_hash)
        return WriteOutcome(
            result=TokenResult(token=token),
            entity_kind="token",
            entity_id=token.id,
            payload={
                "actor": holder.identity_ref,
                "scopes": list(scopes),
                "name": token.name,
                "source": "operator-supplied",
                "superseded": superseded,
            },
            event_kind=TOKEN_ADOPTED_EVENT,
            summary={"actor": holder.identity_ref, "scopes": list(scopes)},
        )

    audited_write(
        ctx,
        operation=TOKEN_ADOPT,
        reason="adopting the operator-supplied agent token at init",
        body=body,
    )
    return "adopted"


def list_tokens(ctx: AppContext, params: ListTokensParams) -> TokenListResult:
    with ctx.declared.read() as view:
        return TokenListResult(
            tokens=view.list_tokens(
                include_revoked=params.include_revoked, limit=params.limit
            )
        )


def revoke_token(ctx: AppContext, params: RevokeTokenParams) -> TokenResult:
    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[TokenResult]:
        del actor
        existing = txn.token_by_id(params.id)
        if existing is None:
            msg = f"no token {params.id!r}"
            raise NotFound(msg)
        if not txn.revoke_token(params.id, reason=params.reason, at=ctx.clock()):
            msg = f"token {params.id!r} is already revoked"
            raise Conflict(msg)
        updated = txn.token_by_id(params.id)
        assert updated is not None  # just written in this transaction
        return WriteOutcome(
            result=TokenResult(token=updated),
            entity_kind="token",
            entity_id=params.id,
            payload=updated.model_dump(mode="json"),
            event_kind=TOKEN_REVOKED_EVENT,
            summary={"actor": updated.actor_identity_ref},
        )

    return audited_write(ctx, operation=TOKEN_REVOKE, reason=params.reason, body=body)


def list_auth_decisions(
    ctx: AppContext, params: AuthDecisionListParams
) -> AuthDecisionListResult:
    """Read the allow/deny log.

    The denials are the interesting half: they are what tells you an agent
    tried something it could not do.
    """
    with ctx.declared.read() as view:
        return AuthDecisionListResult(
            decisions=view.list_auth_decisions(
                decision=params.decision, limit=params.limit
            )
        )


def _revoke_named_tokens(
    txn: WriteTxn, actor_id: str, *, name: str, at: datetime
) -> list[str]:
    """Revoke every live token of one name on one actor; the ids revoked."""
    revoked: list[str] = []
    for existing in txn.tokens_for_actor(actor_id):
        if existing.name == name and txn.revoke_token(
            existing.id, reason="superseded by a rotated bootstrap secret", at=at
        ):
            revoked.append(existing.id)
    return revoked


# -- password logins ----------------------------------------------------------
#
# The human credential. A person signs in with a username and a password and
# receives a *session*: a token row like any other, bound to their own actor,
# carrying the scopes on their login, expiring after `session_ttl_days`, and
# revocable by `auth.logout` or `token.revoke`. Authentication does not know
# the difference — which is the point: the front door, the CLI bridge and
# MCP all keep accepting one kind of thing, a bearer bound to an actor.


#: Failed logins per username before the throttle engages, and how long a
#: failure counts for. Five in a minute is a person retyping; more is not.
LOGIN_FAILURE_LIMIT = 5
LOGIN_FAILURE_WINDOW = timedelta(seconds=60)


class LoginThrottle:
    """Per-username failure window. Process-local by design: the store keeps
    the durable record in `auth_decisions`; this only shapes the reply."""

    def __init__(self) -> None:
        self._failures: dict[str, deque[datetime]] = {}
        self._lock = threading.Lock()

    def check(self, username: str, *, now: datetime) -> None:
        with self._lock:
            window = self._failures.get(username)
            if window is None:
                return
            while window and now - window[0] > LOGIN_FAILURE_WINDOW:
                window.popleft()
            if len(window) >= LOGIN_FAILURE_LIMIT:
                retry = LOGIN_FAILURE_WINDOW - (now - window[0])
                msg = (
                    f"too many failed logins for {username!r}; try again in "
                    f"{max(1, int(retry.total_seconds()))} seconds"
                )
                raise LoginThrottled(msg)

    def failed(self, username: str, *, now: datetime) -> None:
        with self._lock:
            self._failures.setdefault(username, deque()).append(now)

    def succeeded(self, username: str) -> None:
        with self._lock:
            self._failures.pop(username, None)


_throttle = LoginThrottle()


def login(ctx: AppContext, params: LoginParams) -> LoginResult:
    """Exchange a username and password for a session token.

    Refusals are one message for "no such user" and "wrong password", and
    both cost the caller a hash — a missing user is verified against a
    dummy hash so the timing does not say which it was. The distinct row in
    `auth_decisions` names the real reason for the operator.
    """
    try:
        username = normalise_username(params.username)
    except ValueError as exc:
        raise Unauthenticated("the username or password is not right") from exc
    now = ctx.clock()
    _throttle.check(username, now=now)

    with ctx.declared.read() as view:
        credential = view.password_credential_by_username(username)
        stored = None if credential is None else view.password_hash(credential.actor_id)
        actor = None if credential is None else view.actor_by_id(credential.actor_id)

    ok = verify_password(params.password, stored or _DUMMY_HASH)
    if (
        credential is None
        or stored is None
        or actor is None
        or actor.disabled
        or not ok
    ):
        _throttle.failed(username, now=now)
        _record(
            ctx,
            decision="deny",
            code=AuthDecisionCode.BAD_PASSWORD,
            operation=AUTH_LOGIN,
            identity_ref=None if actor is None else actor.identity_ref,
        )
        raise Unauthenticated("the username or password is not right")
    _throttle.succeeded(username)

    scopes = tuple(parse_scopes(",".join(credential.scopes)))
    minted = issue(scopes)
    expires_at = now + timedelta(days=ctx.config.session_ttl_days)
    principal = Principal(
        identity_ref=actor.identity_ref,
        kind=actor.kind,
        display_name=actor.display_name,
    )

    def body(txn: WriteTxn, holder: Actor) -> WriteOutcome[LoginResult]:
        token = Token(
            id=ctx.id_factory("tok"),
            actor_id=holder.id,
            actor_identity_ref=holder.identity_ref,
            name=params.session_name,
            scopes=list(scopes),
            kind="session",
            created_at=now,
            expires_at=expires_at,
        )
        txn.insert_token(token, token_hash=minted.token_hash)
        return WriteOutcome(
            result=LoginResult(actor=holder, token=token, secret=minted.secret),
            entity_kind="token",
            entity_id=token.id,
            payload={
                "actor": holder.identity_ref,
                "scopes": list(scopes),
                "name": token.name,
                "kind": "session",
                "expires_at": expires_at.isoformat(),
            },
            event_kind=SESSION_OPENED_EVENT,
            summary={"actor": holder.identity_ref},
        )

    from dataclasses import replace

    result = audited_write(
        replace(ctx, principal=principal),
        operation=AUTH_LOGIN,
        reason=f"password login by {username}",
        body=body,
    )
    _record(
        ctx,
        decision="allow",
        code=AuthDecisionCode.LOGIN_OK,
        operation=AUTH_LOGIN,
        token=result.token,
        identity_ref=actor.identity_ref,
    )
    return result


#: Verified against when the username names nobody, so a miss costs the same
#: scrypt as a hit. Computed once per process; the value never matters.
_DUMMY_HASH = hash_password("not-a-real-password")


def logout(ctx: AppContext, params: LogoutParams) -> LogoutResult:
    """Revoke the token this call arrived with.

    On a surface with no token behind it — the local CLI, an unauthenticated
    loopback listener — there is nothing to revoke and the answer says so
    rather than failing, so a client can call it unconditionally.
    """
    current = ctx.token
    if current is None:
        return LogoutResult(revoked=False, token=None)

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[LogoutResult]:
        del actor
        revoked = txn.revoke_token(current.id, reason=params.reason, at=ctx.clock())
        updated = txn.token_by_id(current.id)
        return WriteOutcome(
            result=LogoutResult(revoked=revoked, token=updated),
            entity_kind="token",
            entity_id=current.id,
            payload={"revoked": revoked, "kind": current.kind},
            event_kind=SESSION_CLOSED_EVENT,
            summary={"actor": current.actor_identity_ref},
        )

    return audited_write(ctx, operation=AUTH_LOGOUT, reason=params.reason, body=body)


def whoami(ctx: AppContext, params: WhoamiParams) -> WhoamiResult:
    """Who authentication decided the caller is, and what they may do.

    The answer the front door relies on: it presents whatever bearer a client
    sent and lets this instance say whose it is. The scope set is the
    effective one, implications applied, so a consumer never re-derives them.
    """
    del params
    if ctx.token is None:
        scopes = LOCAL_GRANT.effective()
    else:
        scopes = Grant(scopes=frozenset(ctx.token.scopes)).effective()  # type: ignore[arg-type]
    return WhoamiResult(
        identity_ref=ctx.principal.identity_ref,
        kind=ctx.principal.kind,
        display_name=ctx.principal.display_name,
        scopes=sorted(scopes),
        token=ctx.token,
    )


def create_user(ctx: AppContext, params: CreateUserParams) -> UserResult:
    """Give a human a login. Creates the actor unless one is named."""
    username = _username(params.username)
    scopes = _scopes(params.scopes)
    password_hash = _password_hash(params.password)

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[UserResult]:
        del actor
        if txn.password_credential_by_username(username) is not None:
            msg = f"a user named {username!r} already exists"
            raise Conflict(msg)
        now = ctx.clock()
        if params.actor is not None:
            holder = _resolve.actor(txn, params.actor)
            if holder.kind != "human":
                msg = (
                    f"{holder.identity_ref} is an agent; only a human may hold "
                    "a password"
                )
                raise InvalidRequest(msg)
        else:
            identity_ref = f"human:{username}"
            existing = txn.actor_by_identity(identity_ref)
            if existing is not None:
                holder = existing
            else:
                holder = Actor(
                    id=ctx.id_factory("act"),
                    kind="human",
                    display_name=params.display_name or username,
                    identity_ref=identity_ref,
                    disabled=False,
                    created_at=now,
                )
                txn.insert_actor(holder)
        if txn.password_credential_for_actor(holder.id) is not None:
            msg = f"{holder.identity_ref} already has a login"
            raise Conflict(msg)
        txn.upsert_password_credential(
            actor_id=holder.id,
            username=username,
            password_hash=password_hash,
            scopes=list(scopes),
            at=now,
        )
        user = txn.password_credential_for_actor(holder.id)
        assert user is not None  # just written in this transaction
        return WriteOutcome(
            result=UserResult(user=user),
            entity_kind="user",
            entity_id=holder.id,
            payload={
                "actor": holder.identity_ref,
                "username": username,
                "scopes": list(scopes),
            },
            event_kind=USER_CREATED_EVENT,
            summary={"actor": holder.identity_ref, "username": username},
        )

    return audited_write(ctx, operation=USER_CREATE, reason=params.reason, body=body)


def list_users(ctx: AppContext, params: UserListParams) -> UserListResult:
    del params
    with ctx.declared.read() as view:
        return UserListResult(users=view.list_password_credentials())


def set_password(ctx: AppContext, params: SetPasswordParams) -> UserResult:
    """Replace a user's password (and optionally scopes), ending their sessions."""
    username = _username(params.username)
    password_hash = _password_hash(params.password)
    scopes = None if params.scopes is None else _scopes(params.scopes)

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[UserResult]:
        del actor
        current = txn.password_credential_by_username(username)
        if current is None:
            msg = f"no user named {username!r}"
            raise NotFound(msg)
        now = ctx.clock()
        txn.upsert_password_credential(
            actor_id=current.actor_id,
            username=username,
            password_hash=password_hash,
            scopes=list(scopes) if scopes is not None else current.scopes,
            at=now,
        )
        revoked = (
            _revoke_sessions(txn, current.actor_id, reason=params.reason, at=now)
            if params.revoke_sessions
            else 0
        )
        user = txn.password_credential_for_actor(current.actor_id)
        assert user is not None  # just written in this transaction
        return WriteOutcome(
            result=UserResult(user=user),
            entity_kind="user",
            entity_id=current.actor_id,
            payload={
                "username": username,
                "scopes": user.scopes,
                "sessions_revoked": revoked,
            },
            event_kind=USER_PASSWORD_SET_EVENT,
            summary={"username": username},
        )

    return audited_write(
        ctx, operation=USER_SET_PASSWORD, reason=params.reason, body=body
    )


def remove_user(ctx: AppContext, params: RemoveUserParams) -> RemoveUserResult:
    """Take a login away. The actor and its audit history stay."""
    username = _username(params.username)

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[RemoveUserResult]:
        del actor
        current = txn.password_credential_by_username(username)
        if current is None:
            msg = f"no user named {username!r}"
            raise NotFound(msg)
        now = ctx.clock()
        txn.delete_password_credential(current.actor_id)
        revoked = _revoke_sessions(txn, current.actor_id, reason=params.reason, at=now)
        return WriteOutcome(
            result=RemoveUserResult(username=username, sessions_revoked=revoked),
            entity_kind="user",
            entity_id=current.actor_id,
            payload={"username": username, "sessions_revoked": revoked},
            event_kind=USER_REMOVED_EVENT,
            summary={"username": username},
        )

    return audited_write(ctx, operation=USER_REMOVE, reason=params.reason, body=body)


def _revoke_sessions(txn: WriteTxn, actor_id: str, *, reason: str, at: datetime) -> int:
    count = 0
    for existing in txn.tokens_for_actor(actor_id):
        if existing.kind == "session" and txn.revoke_token(
            existing.id, reason=reason, at=at
        ):
            count += 1
    return count


def _username(raw: str) -> str:
    try:
        return normalise_username(raw)
    except ValueError as exc:
        raise InvalidRequest(str(exc)) from exc


def _scopes(raw: str) -> tuple[Scope, ...]:
    try:
        return parse_scopes(raw)
    except ValueError as exc:
        raise InvalidRequest(str(exc)) from exc


def _password_hash(password: str) -> str:
    try:
        return hash_password(password)
    except ValueError as exc:
        raise InvalidRequest(str(exc)) from exc


__all__ = [
    "Authenticated",
    "Forbidden",
    "PasswordCredential",
    "Unauthenticated",
    "adopt_bootstrap_agent_token",
    "adopt_bootstrap_core_token",
    "authenticate",
    "authorize",
    "create_user",
    "issue_token",
    "list_auth_decisions",
    "list_tokens",
    "list_users",
    "local",
    "login",
    "logout",
    "remove_user",
    "revoke_token",
    "set_password",
    "whoami",
]
