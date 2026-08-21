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
from vogt.core.ids import slugify
from vogt.errors import Conflict, InvalidRequest, NotFound

#: What GitHub permits in an owner or repository name. Strict on purpose:
#: without it `https://example.com` splits into two parts and is accepted as
#: a repository called `example.com` owned by `https:`.
_NAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]*")

PROJECT_IMPORT = "project.import"
PROJECT_IMPORTED_EVENT = "project.imported"

#: Import is GitHub-shaped in v1 (it clones from GitHub and consolidates its
#: history). The pure identity operations — parsing a reference and building
#: its URLs — need no credential, so they go through the registry's token-less
#: identity provider rather than a client of import's own (D2).
_GITHUB = github_identity()


def import_project(ctx: AppContext, params: ImportProjectParams) -> ImportProjectResult:
    """Clone, register, and consolidate — one operation, one reason.

    Ordering is load-bearing. The clone happens before the declared write,
    so the failure mode of a half-finished import is a directory nobody
    registered rather than a project pointing at nothing. `project create`
    scaffolds in the same order, for the same reason.
    """
    owner, repo = _resolve_repo(params.repo)
    ref = RepoRef(host="github.com", owner=owner, repo=repo)
    remote = _GITHUB.clone_url(ref)
    display_name = params.name or repo
    slug = slugify(display_name)
    if not slug:
        msg = f"cannot derive a slug from name {display_name!r}"
        raise InvalidRequest(msg)

    # A read, not a gate: cloning a large repository only to fail on a name
    # collision wastes minutes and leaves a directory behind. The write path
    # still refuses duplicates itself, and remains the authority.
    with ctx.declared.read() as view:
        if view.project_by_slug(slug) is not None:
            msg = f"a project with slug {slug!r} is already registered"
            raise Conflict(msg)

    provider = github_provider(ctx.config)
    metadata = _describe(provider, ref)

    destination = (
        Path(params.root_path).expanduser()
        if params.root_path
        else ctx.config.resolved_import_root / slug
    )
    outcome = ctx.cloner(
        CloneRequest(
            remote=remote,
            destination=destination,
            token=None if provider is None else provider.clone_token(),
        )
    )

    registered = record_registration(
        ctx,
        RegisterProjectParams(
            name=display_name,
            root_path=str(outcome.destination),
            repo_url=_GITHUB.web_url(ref),
            lifecycle_state=params.lifecycle_state,
            reason=params.reason,
        ),
        operation=PROJECT_IMPORT,
        event_kind=PROJECT_IMPORTED_EVENT,
    )

    consolidated = None
    detail = None
    if params.consolidate and provider is not None:
        consolidated = onboard(
            ctx,
            OnboardParams(project=registered.project.slug, reason=params.reason),
        )
    elif params.consolidate:
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


__all__ = ["PROJECT_IMPORT", "PROJECT_IMPORTED_EVENT", "import_project"]
