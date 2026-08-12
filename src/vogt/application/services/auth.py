"""Issuing, revoking and checking tokens.

The authenticating half of FR-S2: a bearer token resolves to an actor and a
set of scopes, and *that* is the principal. Nothing here takes an identity
from a caller — the only input is the secret, and the only output is who the
secret belongs to.
"""

from __future__ import annotations

from dataclasses import dataclass
from datetime import timedelta

from vogt.application.context import AppContext
from vogt.application.models import (
    AuthDecisionListParams,
    AuthDecisionListResult,
    IssueTokenParams,
    IssueTokenResult,
    ListTokensParams,
    RevokeTokenParams,
    TokenListResult,
    TokenResult,
)
from vogt.application.services import _resolve
from vogt.application.writes import WriteOutcome, audited_write
from vogt.core.auth import (
    LOCAL_GRANT,
    AuthDecisionCode,
    Grant,
    Scope,
    hash_token,
    is_expired,
    issue,
    parse_scopes,
)
from vogt.core.entities import Actor, AuthDecision, Token
from vogt.core.principal import Principal
from vogt.errors import Conflict, InvalidRequest, NotFound, VogtError
from vogt.storage.interface import WriteTxn

TOKEN_ISSUE = "token.issue"
TOKEN_REVOKE = "token.revoke"

TOKEN_ISSUED_EVENT = "token.issued"
TOKEN_REVOKED_EVENT = "token.revoked"


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

    ctx.declared.touch_token(token.id, at=ctx.clock())
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
    """The second gate, recorded either way (FR-S4, FR-S5)."""
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
                    "URL (FR-S7)."
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
    """Read the allow/deny log (FR-S5).

    The denials are the interesting half: they are what tells you an agent
    tried something it could not do.
    """
    with ctx.declared.read() as view:
        return AuthDecisionListResult(
            decisions=view.list_auth_decisions(
                decision=params.decision, limit=params.limit
            )
        )
