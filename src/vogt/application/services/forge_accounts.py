"""Per-actor forge account linking (#179, design #178 decision 4).

An actor links their own forge identity by pasting a Personal Access Token.
Vogt validates it, stores it encrypted at rest, and — when that actor drives a
write — speaks upstream as *them* rather than as the instance file token. The
file token (FR-S7) stays the fallback for sweeps and for actors who never
linked one.

Three operations, so the capability arrives on CLI, REST and MCP at once
(FR-A1). The token is never echoed on any surface: `status` returns only the
cleartext host / login / scopes, which is why those columns are cleartext and
the token is not.

Linking is *off* unless `forge_account_key_file` is configured — there is
nowhere safe to keep a recoverable secret without it, so the honest answer is
"not configured", raised as `ForgeAccountsNotConfigured`.
"""

from __future__ import annotations

from vogt.adapters.forge.accounts import load_cipher
from vogt.adapters.forge.github import HOST as GITHUB_HOST
from vogt.adapters.github.client import GitHubClient
from vogt.application.context import AppContext
from vogt.application.models import (
    ForgeAccountLinkParams,
    ForgeAccountResult,
    ForgeAccountStatusParams,
    ForgeAccountStatusResult,
    ForgeAccountUnlinkParams,
    ForgeAccountView,
)
from vogt.application.writes import WriteOutcome, audited_write
from vogt.core.entities import Actor
from vogt.errors import InvalidRequest
from vogt.storage.interface import WriteTxn

FORGE_ACCOUNT_LINKED_EVENT = "forge.account_linked"
FORGE_ACCOUNT_UNLINKED_EVENT = "forge.account_unlinked"


def _require_supported_host(host: str) -> None:
    """Only github.com in v1 — the same ceiling the rest of the forge holds."""
    if host != GITHUB_HOST:
        msg = (
            f"account linking supports {GITHUB_HOST} only in v1; "
            f"{host!r} has no provider to validate a token against"
        )
        raise InvalidRequest(msg)


def link_forge_account(
    ctx: AppContext, params: ForgeAccountLinkParams
) -> ForgeAccountResult:
    """Validate a pasted PAT, then store it encrypted, scoped to (actor, host).

    Order matters: the key is loaded first (so an unconfigured instance refuses
    before a token is ever handled), then the token is validated against the
    forge (so an invalid paste is refused before anything is stored), and only
    then is the ciphertext written. The plaintext never leaves this function.
    """
    _require_supported_host(params.host)
    # Refuse early when linking is not configured — before touching the token.
    cipher = load_cipher(ctx.config)

    client = GitHubClient(token=params.token, transport=ctx.forge_transport)
    identity = client.identity()
    if identity is None:
        msg = (
            "the forge did not accept this token; it may be invalid, expired, "
            "or lack the scope to read its own identity"
        )
        raise InvalidRequest(msg)

    encrypted = cipher.encrypt(params.token)

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[ForgeAccountResult]:
        txn.upsert_forge_account(
            actor_id=actor.id,
            host=params.host,
            login=identity.login,
            scopes=identity.scopes,
            encrypted_token=encrypted,
            at=ctx.clock(),
        )
        result = ForgeAccountResult(
            host=params.host,
            login=identity.login,
            scopes=identity.scopes,
            linked=True,
        )
        return WriteOutcome(
            result=result,
            entity_kind="forge_account",
            entity_id=f"{actor.id}:{params.host}",
            # The digest covers who and where, never the token.
            payload={"host": params.host, "login": identity.login},
            event_kind=FORGE_ACCOUNT_LINKED_EVENT,
            summary={"host": params.host, "login": identity.login},
        )

    return audited_write(
        ctx, operation="forge.account_link", reason=params.reason, body=body
    )


def status_forge_account(
    ctx: AppContext, params: ForgeAccountStatusParams
) -> ForgeAccountStatusResult:
    """What the acting actor has linked. Works with no key — reads cleartext."""
    with ctx.declared.read() as view:
        actor = view.actor_by_identity(ctx.principal.identity_ref)
        if actor is None:
            # A principal that has never written has no actor row yet, and so
            # no linked accounts. Nothing to auto-register on a read.
            accounts = []
        elif params.host is not None:
            account = view.forge_account(actor_id=actor.id, host=params.host)
            accounts = [] if account is None else [account]
        else:
            accounts = view.forge_accounts_for_actor(actor.id)
    return ForgeAccountStatusResult(
        accounts=[
            ForgeAccountView(
                host=account.host,
                login=account.login,
                scopes=account.scopes,
                linked=True,
            )
            for account in accounts
        ]
    )


def unlink_forge_account(
    ctx: AppContext, params: ForgeAccountUnlinkParams
) -> ForgeAccountResult:
    """Remove the stored PAT for (actor, host).

    Unlinking deletes Vogt's copy of the token and returns attribution to the
    file-token fallback. It does not — cannot — revoke the token upstream; the
    forge is the only place that can, so a caller who wants it dead revokes it
    there too."""
    _require_supported_host(params.host)

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[ForgeAccountResult]:
        existing = txn.forge_account(actor_id=actor.id, host=params.host)
        txn.delete_forge_account(actor_id=actor.id, host=params.host)
        result = ForgeAccountResult(
            host=params.host,
            login=None,
            scopes="",
            linked=False,
        )
        return WriteOutcome(
            result=result,
            entity_kind="forge_account",
            entity_id=f"{actor.id}:{params.host}",
            payload={"host": params.host, "unlinked": True},
            event_kind=FORGE_ACCOUNT_UNLINKED_EVENT,
            summary={
                "host": params.host,
                "was_linked": existing is not None,
            },
        )

    return audited_write(
        ctx, operation="forge.account_unlink", reason=params.reason, body=body
    )


__all__ = [
    "link_forge_account",
    "status_forge_account",
    "unlink_forge_account",
]
