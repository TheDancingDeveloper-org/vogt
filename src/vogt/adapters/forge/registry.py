"""Host → provider resolution (D2, D8).

This is what replaces the free `repo_of()` at every call site: instead of
"parse a GitHub URL", the question becomes "which forge hosts this, and is it
configured", and the answer is a `ForgeProvider` or an honest `None`. The
registry is the one place that knows the set of forges Vogt can speak to, so
adding Forgejo in Phase 5 is a new entry here and nothing else.

A provider resolves to `None` for two different reasons, and the caller that
cares about the difference asks `unsupported_reason` first:

- **Unsupported host** — no forge here reads it. That is a permanent fact
  about the URL, and it is why an empty consolidation must never read as
  "there is nothing" (WI-9).
- **Not configured** — the host is supported but has no token file, so it is
  "not collected". Registering only when a token exists is the honesty
  mechanism: an always-present forge collector that fails on the network
  would pin the whole estate's freshness to `partial` (D8).
"""

from __future__ import annotations

from collections.abc import Callable
from dataclasses import dataclass
from pathlib import Path

from vogt.adapters.forge.github import HOST as GITHUB_HOST
from vogt.adapters.forge.github import GitHubProvider
from vogt.adapters.forge.models import RepoRef
from vogt.adapters.forge.provider import ForgeProvider
from vogt.adapters.github.client import GitHubClient, Transport
from vogt.config import VogtConfig


@dataclass(frozen=True)
class _Spec:
    """One registered forge kind: how to recognise it, how to build it."""

    hosts: tuple[str, ...]
    match: Callable[[str | None], RepoRef | None]
    build: Callable[[Path | None, Transport | None], ForgeProvider | None]


def _build_github(
    token_file: Path | None, transport: Transport | None
) -> ForgeProvider | None:
    client = GitHubClient.from_token_file(token_file, transport=transport)
    return None if client is None else GitHubProvider(client)


#: A token-less instance used only for its pure `parse` — matching a URL to a
#: host never touches the network or needs a credential.
_GITHUB_MATCHER = GitHubProvider(GitHubClient())

#: The forges this build can speak to. GitHub is the only v1 provider (D2);
#: Forgejo joins here in Phase 5 (#176) with no change to any call site.
_SPECS: tuple[_Spec, ...] = (
    _Spec(
        hosts=(GITHUB_HOST,),
        match=_GITHUB_MATCHER.parse,
        build=_build_github,
    ),
)


def supported_hosts() -> tuple[str, ...]:
    """Every host some registered provider can read."""
    return tuple(host for spec in _SPECS for host in spec.hosts)


def _spec_for(repo_url: str | None) -> tuple[_Spec, RepoRef] | None:
    for spec in _SPECS:
        ref = spec.match(repo_url)
        if ref is not None:
            return spec, ref
    return None


def token_file_for(config: VogtConfig, host: str) -> Path | None:
    """The token file configured for `host`, honouring the github alias (D8).

    `forge_token_files` is the general map; `github_token_file` remains the
    name for github.com, so an existing deployment keeps working with no TOML
    change and a new host is one line under `[forge_token_files]`.
    """
    mapped = config.forge_token_files.get(host)
    if mapped is not None:
        return mapped
    if host == GITHUB_HOST:
        return config.github_token_file
    return None


def provider_for(
    repo_url: str | None,
    config: VogtConfig,
    *,
    transport: Transport | None = None,
) -> ForgeProvider | None:
    """The provider for `repo_url`, or `None` when there is not one.

    `None` folds "no forge hosts this" and "the forge is not configured"
    together on purpose — both mean "do not read", and the caller that needs
    to tell a person *why* reaches for `unsupported_reason`, which answers the
    first case without a token.
    """
    matched = _spec_for(repo_url)
    if matched is None:
        return None
    spec, ref = matched
    token_file = token_file_for(config, ref.host)
    return spec.build(token_file, transport)


def unsupported_reason(repo_url: str | None) -> str | None:
    """Why no forge can read this repository, or `None` if one can.

    An empty success is the failure mode this exists to prevent. `forge
    onboard` against a Forgejo project returned `issues: 0, pull_requests: 0,
    …, detail: null` for a repository with an open issue in it — byte-identical
    to the honest answer for a repository with no history at all. The reader
    of an empty consolidation treats it as a signal, so the signal has to
    carry its own cause.
    """
    if not repo_url:
        return (
            "this project declares no repository URL, so there is no forge to "
            "read — which is 'not collected', not 'there is nothing'"
        )
    if _spec_for(repo_url) is not None:
        return None
    host = repo_url.split("://")[-1].split("/")[0] or repo_url
    readable = ", ".join(supported_hosts())
    return (
        f"no configured forge reads {host}; this build reads {readable} only, "
        "so nothing was collected here and no conclusion should be drawn from "
        "the counts"
    )


def has_configured_forge(config: VogtConfig) -> bool:
    """Whether any registered forge has a usable token file (D8).

    The conditional-registration gate for the per-project sync collectors: an
    always-registered collector that fails on the network would pin the whole
    estate's freshness to `partial`, so "no forge configured" must mean "not
    registered", not "registered and failing"."""
    # Through `spec.build` (not a direct file read) so it resolves a provider
    # exactly as a sweep will — same token path, same client factory a test
    # may have substituted.
    for spec in _SPECS:
        for host in spec.hosts:
            if spec.build(token_file_for(config, host), None) is not None:
                return True
    return False


def github_identity() -> GitHubProvider:
    """A token-less GitHub provider for pure identity work — parsing a
    reference, building clone/web URLs — which needs no credential. Import is
    GitHub-shaped in v1 and reaches for this rather than constructing a client
    of its own, so no service outside this package names `GitHubClient`."""
    return _GITHUB_MATCHER


def github_provider(
    config: VogtConfig, *, transport: Transport | None = None
) -> GitHubProvider | None:
    """The github.com provider specifically, for the GitHub-only paths.

    Import and write-back are GitHub-shaped in v1 (import clones from GitHub;
    the writer is the GitHub mutator). They ask for the concrete provider by
    name rather than resolving one per URL, and this keeps that honest —
    it still goes through the same token resolution as everything else (D8).
    """
    token_file = token_file_for(config, GITHUB_HOST)
    return _build_github(token_file, transport)  # type: ignore[return-value]


__all__ = [
    "github_identity",
    "github_provider",
    "has_configured_forge",
    "provider_for",
    "supported_hosts",
    "token_file_for",
    "unsupported_reason",
]
