"""`forge.import` — turn a picker-listed configured-forge repository into a
project (#344).

`forge.repos` lists the repositories the acting credential can see, with
`already_registered: false` for the ones no project holds — but nothing turned
one of those into a project. `forge.link` needs a project that already exists,
and `forge.publish` refuses when the remote is already there; neither imports.
`forge.import` is the missing verb that mirrors the UI pick: it takes a
repository exactly as `forge.repos` names it (`owner` + `name`), clones it
under the same credential the listing used (#179), registers it, and runs a
full read-only consolidation — so afterwards `forge.link` and `forge.writeback`
operate on it like any other linked project.

It composes with `project.import` rather than duplicating it: the clone,
register and consolidate are `imports.import_from_ref`, and the only thing this
adds is resolving the acting actor's linked credential the way the picker does
(`forge_repos._reader_provider`). That matters for a private repository: the
credential that could *see* it to list it is the credential the clone must run
under, and the instance file token may not be able to reach it at all.
"""

from __future__ import annotations

from vogt.adapters.forge.models import RepoRef
from vogt.adapters.forge.registry import identity_for, supported_hosts
from vogt.application.context import AppContext
from vogt.application.models import ForgeImportParams, ImportProjectResult
from vogt.application.services.forge_repos import _reader_provider
from vogt.application.services.imports import (
    FORGE_IMPORT,
    FORGE_IMPORTED_EVENT,
    import_from_ref,
)
from vogt.errors import InvalidRequest


def import_forge_repo(
    ctx: AppContext, params: ForgeImportParams
) -> ImportProjectResult:
    """Clone + register + consolidate a repository the picker listed (#344)."""
    if params.host not in supported_hosts(ctx.config):
        raise InvalidRequest(
            f"{params.host!r} is not a configured forge host; add it to "
            "forge_token_files before importing by host"
        )

    # The same credential resolution the picker uses (#179): the acting actor's
    # linked PAT when they have one — which reaches their private repos even
    # with no instance file token — and the file token otherwise. `None` means
    # neither is configured, so there is nothing to clone the repository with.
    provider, _login = _reader_provider(ctx, params.host)
    if provider is None:
        msg = (
            "no forge credential is available for this host — neither a linked "
            "account (`forge account link`) nor the instance file token "
            "(FR-S7) — so there is nothing to clone "
            f"{params.host}/{params.owner}/{params.name} with"
        )
        raise InvalidRequest(msg)
    ref = RepoRef(host=params.host, owner=params.owner, repo=params.name)
    identity = identity_for(
        f"https://{params.host}/{params.owner}/{params.name}",
        ctx.config,
        transport=ctx.forge_transport,
    )
    if identity is None:
        raise InvalidRequest(
            f"could not resolve {params.host}/{params.owner}/{params.name}"
        )
    return import_from_ref(
        ctx,
        ref=ref,
        provider=provider,
        display_name=params.display_name,
        root_path=params.root_path,
        lifecycle_state=params.lifecycle_state,
        consolidate=params.consolidate,
        reason=params.reason,
        operation=FORGE_IMPORT,
        event_kind=FORGE_IMPORTED_EVENT,
    )


__all__ = ["import_forge_repo"]
