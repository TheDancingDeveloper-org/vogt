"""Importing a repository that lives on GitHub (FR-P6, FR-P7).

Registration assumes the working tree is authoritative for its own
provenance. That is true of a folder and false of a checkout of somebody
else's repository: register the local tree and the first sweep compares two
sources whose relationship nobody established. Import establishes it — clone
the named repository, register the result with its remote, consolidate what
is already upstream — so every later divergence is news rather than
ambiguity (DESIGN §5.2).

The caller names the repository. Nothing here lists, searches or suggests
one; that is the registration-candidate listing r3 removed (FR-G15).
"""

from __future__ import annotations

import re
from pathlib import Path

from vogt.adapters.forge import (
    GitHubProvider,
    RepoRef,
    github_identity,
    github_provider,
)
from vogt.adapters.git import CloneRequest
from vogt.application.context import AppContext
from vogt.application.models import (
    ImportProjectParams,
    ImportProjectResult,
    OnboardParams,
    RegisterProjectParams,
)
from vogt.application.services.projects import record_registration
from vogt.application.services.writeback import onboard
from vogt.core.entities import LifecycleState
from vogt.core.ids import slugify
from vogt.errors import Conflict, InvalidRequest, NotFound

#: What GitHub permits in an owner or repository name. Strict on purpose:
#: without it `https://example.com` splits into two parts and is accepted as
#: a repository called `example.com` owned by `https:`.
_NAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]*")

PROJECT_IMPORT = "project.import"
PROJECT_IMPORTED_EVENT = "project.imported"

#: `forge.import` (#344) lands the same clone+register+consolidate as
#: `project.import`, but the audit must say which act put the project there:
#: "imported from GitHub by reference" and "imported from the forge picker
#: under my credential" are different provenance answers (FR-S1).
FORGE_IMPORT = "forge.import"
FORGE_IMPORTED_EVENT = "forge.imported"

#: Import is GitHub-shaped in v1 (it clones from GitHub and consolidates its
#: history). The pure identity operations — parsing a reference and building
#: its URLs — need no credential, so they go through the registry's token-less
#: identity provider rather than a client of import's own (D2).
_GITHUB = github_identity()


def import_project(ctx: AppContext, params: ImportProjectParams) -> ImportProjectResult:
    """Clone, register, and consolidate — one operation, one reason.

    The caller names the repository by reference (`owner/name` or a URL), and
    the clone runs under the instance file token (FR-S7). The clone, register
    and consolidate themselves are `import_from_ref`, which `forge.import`
    (#344) shares — the only difference between the two verbs is how the
    repository is named and whose credential the clone runs under.
    """
    owner, repo = _resolve_repo(params.repo)
    ref = RepoRef(host="github.com", owner=owner, repo=repo)
    provider = github_provider(ctx.config, transport=ctx.forge_transport)
    return import_from_ref(
        ctx,
        ref=ref,
        provider=provider,
        display_name=params.name,
        root_path=params.root_path,
        lifecycle_state=params.lifecycle_state,
        consolidate=params.consolidate,
        reason=params.reason,
        operation=PROJECT_IMPORT,
        event_kind=PROJECT_IMPORTED_EVENT,
    )


def import_from_ref(
    ctx: AppContext,
    *,
    ref: RepoRef,
    provider: GitHubProvider | None,
    display_name: str | None,
    root_path: str | None,
    lifecycle_state: LifecycleState,
    consolidate: bool,
    reason: str,
    operation: str,
    event_kind: str,
) -> ImportProjectResult:
    """Clone the named repository, register it, and consolidate (#344).

    The shared core of `project.import` and `forge.import`. The caller
    resolves the `provider` — the file token for `project.import`, the
    acting actor's linked credential for `forge.import` (#179) — so the same
    clone+register+consolidate runs under whichever credential named the
    repository; `None` is the honest unconfigured case (a public repository
    still clones, and consolidation is skipped with a 'not collected' detail).

    Ordering is load-bearing. The clone happens before the declared write, so
    the failure mode of a half-finished import is a directory nobody
    registered rather than a project pointing at nothing. `project create`
    scaffolds in the same order, for the same reason.
    """
    remote = _GITHUB.clone_url(ref)
    name = display_name or ref.repo
    slug = slugify(name)
    if not slug:
        msg = f"cannot derive a slug from name {name!r}"
        raise InvalidRequest(msg)

    # A read, not a gate: cloning a large repository only to fail on a name
    # collision wastes minutes and leaves a directory behind. The write path
    # still refuses duplicates itself, and remains the authority.
    with ctx.declared.read() as view:
        if view.project_by_slug(slug) is not None:
            msg = f"a project with slug {slug!r} is already registered"
            raise Conflict(msg)

    metadata = _describe(provider, ref)

    destination = (
        Path(root_path).expanduser()
        if root_path
        else ctx.config.resolved_import_root / slug
    )
    outcome = ctx.cloner(
        CloneRequest(
            remote=remote,
            destination=destination,
            token=None if provider is None else provider.clone_token(),
            # On the data volume, not the default temp dir: the hardened
            # deployment's /tmp is a noexec tmpfs, and the askpass helper
            # must be executable (see `_AskPass`).
            helper_dir=ctx.config.data_dir / "tmp",
        )
    )

    registered = record_registration(
        ctx,
        RegisterProjectParams(
            name=name,
            root_path=str(outcome.destination),
            repo_url=_GITHUB.web_url(ref),
            lifecycle_state=lifecycle_state,
            reason=reason,
        ),
        operation=operation,
        event_kind=event_kind,
        # A clone + consolidate of a forge repo is one of the explicit acts
        # that make a project upstream-truth (#181): its work items are its
        # issues from the first read. Without a provider, or without a
        # consolidation, the project stays unlinked and `forge.link` remains
        # the way to opt it in later.
        link_state=("linked" if consolidate and provider is not None else "unlinked"),
    )

    consolidated = None
    detail = None
    if consolidate and provider is not None:
        consolidated = onboard(
            ctx,
            OnboardParams(project=registered.project.slug, reason=reason),
        )
    elif consolidate:
        detail = (
            "cloned and registered, but the forge adapter is not configured, "
            "so nothing upstream was read — which is 'not collected', not "
            "'there is nothing'"
        )

    return ImportProjectResult(
        project=registered.project,
        remote=remote,
        root_path=str(outcome.destination),
        revision=outcome.revision,
        default_branch=outcome.default_branch or _text(metadata.get("default_branch")),
        cloned=not outcome.reused,
        consolidated=consolidated,
        detail=detail
        or (
            f"{destination} already held a clone of this remote; "
            "it was registered as it stood"
            if outcome.reused
            else None
        ),
    )


def _resolve_repo(reference: str) -> tuple[str, str]:
    """Accept the ways a person names a repository, and only those.

    `owner/name`, an HTTPS URL, an SSH URL. Not a search term and not a
    bare name: a repository this instance cannot address unambiguously is a
    request to go looking for it, and nothing here looks.
    """
    candidate = reference.strip()
    parsed = _GITHUB.parse(candidate)
    if parsed is not None:
        return parsed.owner, parsed.repo
    parts = [part for part in candidate.strip("/").split("/") if part]
    if len(parts) == 2 and all(_NAME.fullmatch(part) for part in parts):
        return parts[0], parts[1].removesuffix(".git")
    msg = (
        f"{reference!r} does not name a GitHub repository; "
        "give owner/name or a repository URL"
    )
    raise InvalidRequest(msg)


def _text(value: object) -> str | None:
    """Narrow one field of an untrusted payload to the type it claims."""
    return value if isinstance(value, str) and value else None


def _describe(provider: GitHubProvider | None, ref: RepoRef) -> dict[str, object]:
    """Confirm the repository exists and is visible to this token.

    Unconfigured is not an error — a public repository clones perfectly well
    without a token, and refusing to import one because the optional adapter
    is absent would make the core depend on it (NFR-PO1).
    """
    if provider is None:
        return {}
    payload = provider.describe(ref)
    if payload is None:
        msg = f"the forge has no repository {ref.slug} visible to this instance's token"
        raise NotFound(msg)
    return payload


__all__ = [
    "FORGE_IMPORT",
    "FORGE_IMPORTED_EVENT",
    "PROJECT_IMPORT",
    "PROJECT_IMPORTED_EVENT",
    "import_from_ref",
    "import_project",
]
