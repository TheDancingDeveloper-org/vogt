"""Enumerate the repositories a credential can see, to pick one to import.

Design #178 decision 5, issue #180: after an actor links their forge PAT (#179), the
picker lists the repositories that PAT reaches and lets them choose which to
import. This is the *only* listing operation in the product, and it lists by
credential rather than by crawl — the token is the scope, so nothing here breaks
the "nothing discovers" rule (FR-G15): a person still names what they import, and
the list is what their own credential is entitled to see, not a candidate sweep.

The credential is resolved exactly as the write path resolves attribution
(#179): the acting actor's linked PAT for the host when they have one — which
works even with no instance file token — and the file token (FR-S7) otherwise.
`already_registered` is computed against the declared project list, never by the
provider, because a forge knows nothing of what this Vogt instance has registered.
"""

from __future__ import annotations

from vogt.adapters.forge import ForgeProvider
from vogt.adapters.forge.accounts import account_linking_enabled, load_cipher
from vogt.adapters.forge.registry import (
    provider_for_host,
    provider_for_token,
    supported_hosts,
)
from vogt.application.context import AppContext
from vogt.application.models import (
    ForgeReposParams,
    ForgeReposResult,
    ForgeRepoView,
)
from vogt.errors import InvalidRequest

#: How many projects to scan when computing `already_registered`. An estate of
#: thousands is far beyond the picker's scale; the cap keeps the read bounded
#: without paging a list that is, in practice, dozens long.
_PROJECT_SCAN_LIMIT = 100_000


def list_forge_repos(ctx: AppContext, params: ForgeReposParams) -> ForgeReposResult:
    """The repositories the acting credential can see, for the import picker."""
    if params.host not in supported_hosts(ctx.config):
        raise InvalidRequest(
            f"{params.host!r} is not a configured forge host; add it to "
            "forge_token_files before listing repositories"
        )
    provider, login = _reader_provider(ctx, params.host)
    if provider is None:
        return ForgeReposResult(
            repos=[],
            login=None,
            detail=(
                "no forge credential is available for this host — neither a "
                "linked account nor the instance file token — so nothing was "
                "listed, which is 'not collected', not 'you have no repositories'"
            ),
        )

    registered = _registered_keys(ctx)
    repos = [
        ForgeRepoView(
            owner=repo.owner,
            name=repo.name,
            default_branch=repo.default_branch,
            visibility=repo.visibility,
            url=repo.web_url,
            already_registered=f"{params.host}/{repo.owner}/{repo.name}".lower()
            in registered,
        )
        for repo in provider.list_repos()
    ]
    return ForgeReposResult(repos=repos, login=login)


def _reader_provider(
    ctx: AppContext, host: str
) -> tuple[ForgeProvider | None, str | None]:
    """The provider a read lists through, and whose login it is.

    Prefers the acting actor's linked PAT for the host — which works even when
    the instance has no file token — and falls back to the file-token provider
    (`None` when neither is configured). The actor lookup is skipped entirely
    when linking is not configured, so an instance with no key pays nothing.
    Mirrors the write path's attribution choice (#179) so the picker and the
    writes it leads to speak upstream as the same identity.
    """
    if account_linking_enabled(ctx.config):
        with ctx.declared.read() as view:
            actor = view.actor_by_identity(ctx.principal.identity_ref)
            if actor is not None:
                secret = view.forge_account_secret(actor_id=actor.id, host=host)
                account = view.forge_account(actor_id=actor.id, host=host)
        if secret is not None and account is not None:
            pat = load_cipher(ctx.config).decrypt(secret)
            provider = provider_for_token(
                host, pat, config=ctx.config, transport=ctx.forge_transport
            )
            return provider, account.login

    file_provider = provider_for_host(host, ctx.config, transport=ctx.forge_transport)
    return file_provider, None


def _registered_keys(ctx: AppContext) -> set[str]:
    """The host-qualified identity of every registered forge repository.

    Built from the declared project list so `already_registered` is a fact about
    this instance's own state, not something the forge could answer. Matching is
    on the parsed repository identity rather than the raw URL, so the http/ssh/
    `.git` spellings of the same repository all collapse to one key.
    """
    from vogt.adapters.forge.registry import identity_for

    keys: set[str] = set()
    with ctx.declared.read() as view:
        projects = view.list_projects(limit=_PROJECT_SCAN_LIMIT, offset=0)
    for project in projects:
        resolved = identity_for(project.repo_url, ctx.config)
        if resolved is not None:
            _, ref = resolved
            keys.add(f"{ref.host}/{ref.owner}/{ref.repo}".lower())
    return keys


__all__ = ["list_forge_repos"]
