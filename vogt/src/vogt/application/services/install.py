"""First-run install mode: the door that closes itself.

A new instance has no operator, so no person can authenticate to it over the
network — which until now meant the first credential had to be minted from a
shell inside the container. Install mode is the bootstrap that replaces that
incantation: while **no person holds a credential** — no token bound to a
non-agent actor, revoked or not, and no password login — an unauthenticated
caller may name the first operator and receive the first browser token; the
moment a person holds one — this one, one minted over loopback for a person,
`vogt user create` — the mode is closed and the bootstrap refuses with
`install_closed`.

Tokens bound to *agent* actors do not count (#903). The stack secret adopted
at `init` (`bootstrap_core_token_file`, bound to `agent:vogt-engine`), the
brokered agent token and coding-session tokens are machinery: they
authenticate exactly as before, but a Docker quick start that supplies the
stack secret has still created no operator, and closing the wizard on it
left a fresh install with no way to sign in short of `vogt user create`
inside the container.

Install mode is deliberately a *property of the credential store*, not a
flag an operation flips: the declared store latches it closed by itself
(`install_latch`, migration 0020; set in the same transaction by every write
that gives a credential) the moment a person is given a login or a token, by any
path, and nothing reopens it short of deleting the store. The same migration
latched every store that already held a token when it was upgraded, so a
running instance operated only through agent-bound tokens and the engine's
break-glass `ENGINE_TOKEN` stays closed — it was closed under the old rule,
and an upgrade must not hand its port an unauthenticated admin bootstrap.
Revoked tokens count as "a credential exists" on purpose, and removing a
user does not undo the latch: a lockout is fixed over loopback (`vogt token
issue`, `vogt user create`), not by reopening an unauthenticated door on
whatever network the port is published to.

Why an unauthenticated write is acceptable here: `serve` publishes on
loopback unless the operator binds elsewhere (`VOGT_BIND_IP` defaults to
127.0.0.1, and `--host` has no default at all), so during setup the only
parties who can reach this endpoint are the ones who could already mint
tokens over the loopback surface. Optional hardening — a boot code, a
loopback-only check on the bootstrap — would slot into the HTTP adapter
(`adapters/http/install.py`) in front of this service, and is deliberately
not v1.
"""

from __future__ import annotations

from dataclasses import replace
from datetime import timedelta

from vogt.application.context import AppContext
from vogt.application.models import (
    InstallBootstrapParams,
    InstallBootstrapResult,
    InstallStatusResult,
)
from vogt.application.writes import WriteOutcome, audited_write
from vogt.core.auth import Scope, hash_password, issue, normalise_username
from vogt.core.entities import Actor, Token
from vogt.core.ids import slugify
from vogt.core.principal import Principal
from vogt.errors import InstallClosed, InvalidRequest
from vogt.storage.interface import ReadView, WriteTxn

INSTALL_BOOTSTRAP = "install.bootstrap"
INSTALL_BOOTSTRAPPED_EVENT = "install.bootstrapped"

#: What the first token holds. `admin` on purpose: this is the operator's own
#: credential, minted before any other actor exists, and everything the wizard
#: goes on to do — linking a forge, importing a project, issuing narrower
#: tokens for agents — needs it. Scoping it down would only force the wizard
#: to mint a second, broader token immediately.
BOOTSTRAP_SCOPES: tuple[Scope, ...] = ("admin",)


def install_mode_active(view: ReadView) -> bool:
    """Active exactly while no person holds a credential — no token bound to
    a non-agent actor (revoked included), no password login — and the store
    was never latched closed. Agent-bound tokens, the adopted stack secret
    among them, never close it (#903); see `ReadView.install_closed`."""
    return not view.install_closed()


def install_status(ctx: AppContext) -> InstallStatusResult:
    """Whether the first-run bootstrap is still open. Unauthenticated and
    truthful either way: the closed answer tells a wizard to go log in.

    An operator who has refused the bootstrap by config is closed
    regardless of who holds a credential — the wizard is told to go log in,
    because a deployment that turned the door off creates its first
    operator another way (`vogt user create`)."""
    if not ctx.config.install_bootstrap_enabled:
        return InstallStatusResult(install_mode=False)
    with ctx.declared.read() as view:
        return InstallStatusResult(install_mode=install_mode_active(view))


def install_bootstrap(
    ctx: AppContext, params: InstallBootstrapParams
) -> InstallBootstrapResult:
    """Name the first operator and mint the first token — exactly once.

    The zero-token check runs *inside* the write transaction, so two racing
    bootstraps cannot both succeed: the loser finds the winner's row and
    rolls back everything it did, its auto-registered actor included.

    The write is attributed to the actor it creates — there is no other
    principal in existence to attribute it to, and `ensure_actor`'s
    auto-register row says where that actor came from.
    """
    if not ctx.config.install_bootstrap_enabled:
        # An operator who creates the first operator another way (`vogt user
        # create` in the container) closes the unauthenticated door outright —
        # no window on the public front for a caller to race.
        msg = (
            "install mode is disabled on this instance "
            "(install_bootstrap_enabled=false): create the first operator in "
            "the container with `vogt user create --scopes admin`, not over "
            "this endpoint."
        )
        raise InstallClosed(msg)
    identity_ref = (
        params.identity_ref
        if params.identity_ref is not None
        else _derived_identity(params.display_name)
    )
    principal = Principal(
        identity_ref=identity_ref, kind="human", display_name=params.display_name
    )
    credential = issue(BOOTSTRAP_SCOPES)

    # With a password, the bootstrap creates a *login* and the token it hands
    # back is a browser session — expiring, revocable by `auth.logout` — so
    # the operator's durable credential is the password and not a secret they
    # were shown once. Without one, the headless shape is unchanged: an admin
    # API token, shown once.
    username: str | None = None
    password_hash: str | None = None
    if params.password is not None:
        try:
            username = normalise_username(
                params.username
                if params.username is not None
                else identity_ref.removeprefix("human:")
            )
            password_hash = hash_password(params.password)
        except ValueError as exc:
            raise InvalidRequest(str(exc)) from exc
    elif params.username is not None:
        msg = "a username needs a password to go with it"
        raise InvalidRequest(msg)
    now = ctx.clock()
    expires_at = (
        None
        if password_hash is None
        else now + timedelta(days=ctx.config.session_ttl_days)
    )

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[InstallBootstrapResult]:
        if not install_mode_active(txn):
            msg = (
                "install mode is closed: this instance already has an "
                "operator. Sign in, or create another login over the loopback "
                "surface with `vogt user create`."
            )
            raise InstallClosed(msg)
        if username is not None and password_hash is not None:
            txn.upsert_password_credential(
                actor_id=actor.id,
                username=username,
                password_hash=password_hash,
                scopes=list(BOOTSTRAP_SCOPES),
                at=now,
            )
        token = Token(
            id=ctx.id_factory("tok"),
            actor_id=actor.id,
            actor_identity_ref=actor.identity_ref,
            name=params.token_name,
            scopes=list(BOOTSTRAP_SCOPES),
            kind="api" if password_hash is None else "session",
            created_at=now,
            expires_at=expires_at,
        )
        txn.insert_token(token, token_hash=credential.token_hash)
        return WriteOutcome(
            result=InstallBootstrapResult(
                actor=actor,
                token=token,
                secret=credential.secret,
                warning=(
                    "This is the only time the secret is shown. It is not "
                    "stored and cannot be recovered — losing it means "
                    "issuing another over the loopback surface."
                    if password_hash is None
                    else "This is a browser session; sign in again with your "
                    "password when it expires. API tokens for agents come "
                    "from `vogt token issue`."
                ),
                username=username,
            ),
            entity_kind="token",
            entity_id=token.id,
            # No secret in the payload, for the reason `issue_token` gives: an
            # audit row holding the credential is a leak with a timestamp.
            payload={
                "actor": actor.identity_ref,
                "scopes": list(BOOTSTRAP_SCOPES),
                "name": token.name,
                "kind": token.kind,
                "username": username,
                "source": "first-run install bootstrap",
            },
            event_kind=INSTALL_BOOTSTRAPPED_EVENT,
            summary={"actor": actor.identity_ref, "scopes": list(BOOTSTRAP_SCOPES)},
        )

    return audited_write(
        replace(ctx, principal=principal),
        operation=INSTALL_BOOTSTRAP,
        reason=f"first-run install bootstrap for {identity_ref}",
        body=body,
    )


def _derived_identity(display_name: str) -> str:
    slug = slugify(display_name)
    if not slug:
        msg = f"cannot derive an identity from display name {display_name!r}"
        raise InvalidRequest(msg)
    return f"human:{slug}"
