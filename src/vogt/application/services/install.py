"""First-run install mode (#292): the door that closes itself.

A new instance has no tokens, so nothing can authenticate to it over the
network — which until now meant the first credential had to be minted from a
shell inside the container. Install mode is the bootstrap that replaces that
incantation: while the token store holds **no token rows at all**, an
unauthenticated caller may name the first operator and receive the first
browser token; the moment any token exists — this one, an operator-adopted
one (#199), one minted over loopback — the mode is closed and the bootstrap
refuses with `install_closed`.

Install mode is deliberately a *property of the token store*, not a flag:
there is no row to forget to flip and no way to reopen it short of deleting
the store. Revoked tokens count as "a token exists" on purpose — revoking
your last credential is a lockout to be fixed over loopback (`vogt token
issue`), not a reason to reopen an unauthenticated door on whatever network
the port is published to.

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

from vogt.application.context import AppContext
from vogt.application.models import (
    InstallBootstrapParams,
    InstallBootstrapResult,
    InstallStatusResult,
)
from vogt.application.writes import WriteOutcome, audited_write
from vogt.core.auth import Scope, issue
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
    """Active exactly while the store holds no token rows, revoked included."""
    return not view.list_tokens(include_revoked=True, limit=1)


def install_status(ctx: AppContext) -> InstallStatusResult:
    """Whether the first-run bootstrap is still open. Unauthenticated and
    truthful either way: the closed answer tells a wizard to go log in."""
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
    identity_ref = (
        params.identity_ref
        if params.identity_ref is not None
        else _derived_identity(params.display_name)
    )
    principal = Principal(
        identity_ref=identity_ref, kind="human", display_name=params.display_name
    )
    credential = issue(BOOTSTRAP_SCOPES)

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[InstallBootstrapResult]:
        if not install_mode_active(txn):
            msg = (
                "install mode is closed: this instance already has a token. "
                "Sign in with it, or mint another over the loopback surface "
                "with `vogt token issue`."
            )
            raise InstallClosed(msg)
        token = Token(
            id=ctx.id_factory("tok"),
            actor_id=actor.id,
            actor_identity_ref=actor.identity_ref,
            name=params.token_name,
            scopes=list(BOOTSTRAP_SCOPES),
            created_at=ctx.clock(),
            expires_at=None,
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
                ),
            ),
            entity_kind="token",
            entity_id=token.id,
            # No secret in the payload, for the reason `issue_token` gives: an
            # audit row holding the credential is a leak with a timestamp.
            payload={
                "actor": actor.identity_ref,
                "scopes": list(BOOTSTRAP_SCOPES),
                "name": token.name,
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
