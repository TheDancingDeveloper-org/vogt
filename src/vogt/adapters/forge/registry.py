"""Host → provider resolution (D2, D8).

This is what replaces the free `repo_of()` at every call site: instead of
"parse a GitHub URL", the question becomes "which forge hosts this, and is it
configured", and the answer is a `ForgeProvider` or an honest `None`. The
registry is the one place that knows the set of forges Vogt can speak to —
Forgejo joined in Phase 5 (#176) as exactly the new entry this module
promised, with no change to any call site outside the seam.

Two kinds of host live here. github.com is a constant: one host, known
without configuration. A Forgejo host is *data*: any host named under
`[forge_token_files]` that is not github.com is read as a Forgejo/Gitea
installation, which is why the resolution helpers take the config — the set
of supported hosts is no longer knowable without it, and a call that omits
it gets the constant half of the answer only.

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

from vogt.adapters.forge.forgejo import ForgejoClient, ForgejoProvider, parse_repo_url
from vogt.adapters.forge.github import HOST as GITHUB_HOST
from vogt.adapters.forge.github import GitHubProvider
from vogt.adapters.forge.models import RepoRef
from vogt.adapters.forge.provider import ForgeProvider
from vogt.adapters.github.client import GitHubClient, Transport
from vogt.config import VogtConfig


@dataclass(frozen=True)
class _Spec:
    """One registered forge kind: which hosts it answers for under a given
    configuration, how to recognise a URL, and how to build the provider."""

    hosts: Callable[[VogtConfig | None], tuple[str, ...]]
    match: Callable[[str | None, VogtConfig | None], RepoRef | None]
    build: Callable[[str, Path | None, Transport | None], ForgeProvider | None]
    identity: Callable[[str, Transport | None], ForgeProvider]
    build_token: Callable[[str, str, Transport | None], ForgeProvider]


def _build_github(
    host: str, token_file: Path | None, transport: Transport | None
) -> ForgeProvider | None:
    del host  # one host; the client knows its own API root
    client = GitHubClient.from_token_file(token_file, transport=transport)
    return None if client is None else GitHubProvider(client)


def _build_forgejo(
    host: str, token_file: Path | None, transport: Transport | None
) -> ForgeProvider | None:
    client = ForgejoClient.from_token_file(host, token_file, transport=transport)
    return None if client is None else ForgejoProvider(client)


def _identity_github(_host: str, _transport: Transport | None) -> ForgeProvider:
    """The token-less GitHub provider used for URL identity and cloning."""
    return _GITHUB_MATCHER


def _identity_forgejo(host: str, transport: Transport | None) -> ForgeProvider:
    """A token-less Forgejo provider used to import public repositories."""
    return ForgejoProvider(ForgejoClient(host=host, transport=transport))


def _build_github_token(
    _host: str, token: str, transport: Transport | None
) -> ForgeProvider:
    return GitHubProvider(GitHubClient(token=token, transport=transport))


def _build_forgejo_token(
    host: str, token: str, transport: Transport | None
) -> ForgeProvider:
    return ForgejoProvider(ForgejoClient(host=host, token=token, transport=transport))


#: A token-less instance used only for its pure `parse` — matching a URL to a
#: host never touches the network or needs a credential.
_GITHUB_MATCHER = GitHubProvider(GitHubClient())


def _forgejo_hosts(config: VogtConfig | None) -> tuple[str, ...]:
    """Every Forgejo host this configuration names (D8).

    The `[forge_token_files]` map is the declaration: a host in it is a
    Forgejo installation unless it is github.com, whose entry is only ever a
    token override for the constant host. No config, no Forgejo hosts —
    which is the honest answer, not a default.
    """
    if config is None:
        return ()
    return tuple(host for host in config.forge_token_files if host != GITHUB_HOST)


#: The forges this build can speak to (D2). Adding one is one entry here —
#: the claim Phase 5 (#176) existed to test, and Forgejo is the proof.
_SPECS: tuple[_Spec, ...] = (
    _Spec(
        hosts=lambda config: (GITHUB_HOST,),
        match=lambda repo_url, config: _GITHUB_MATCHER.parse(repo_url),
        build=_build_github,
        identity=_identity_github,
        build_token=_build_github_token,
    ),
    _Spec(
        hosts=_forgejo_hosts,
        match=lambda repo_url, config: parse_repo_url(repo_url, _forgejo_hosts(config)),
        build=_build_forgejo,
        identity=_identity_forgejo,
        build_token=_build_forgejo_token,
    ),
)


def supported_hosts(config: VogtConfig | None = None) -> tuple[str, ...]:
    """Every host some registered provider can read under `config`.

    Without a config only the constant hosts answer — github.com — because
    a Forgejo host exists as a fact about a configuration, not this build.
    """
    return tuple(host for spec in _SPECS for host in spec.hosts(config))


def _spec_for(
    repo_url: str | None, config: VogtConfig | None
) -> tuple[_Spec, RepoRef] | None:
    for spec in _SPECS:
        ref = spec.match(repo_url, config)
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
    matched = _spec_for(repo_url, config)
    if matched is None:
        return None
    spec, ref = matched
    token_file = token_file_for(config, ref.host)
    return spec.build(ref.host, token_file, transport)


def provider_for_host(
    host: str,
    config: VogtConfig,
    *,
    transport: Transport | None = None,
) -> ForgeProvider | None:
    """Resolve a configured host to its credentialed provider, if available.

    This is the host-shaped counterpart to :func:`provider_for`: picker and
    import operations already have a host field, so they should not invent a
    dummy repository merely to resolve its token. A host named in
    ``forge_token_files`` is supported even when its token file is absent;
    ``None`` then means ``not configured`` rather than ``unsupported``.
    """
    for spec in _SPECS:
        if host not in spec.hosts(config):
            continue
        return spec.build(host, token_file_for(config, host), transport)
    return None


def provider_for_token(
    host: str,
    token: str,
    *,
    config: VogtConfig | None = None,
    transport: Transport | None = None,
) -> ForgeProvider | None:
    """Build a provider from an explicitly resolved actor token."""
    for spec in _SPECS:
        if host not in spec.hosts(config):
            continue
        return spec.build_token(host, token, transport)
    return None


def identity_for(
    repo_url: str | None,
    config: VogtConfig,
    *,
    transport: Transport | None = None,
) -> tuple[ForgeProvider, RepoRef] | None:
    """Resolve repository identity without requiring a forge token.

    GitHub is always an identity host. Forgejo identities are available for
    hosts explicitly declared in ``forge_token_files``; the resulting
    token-less provider can build canonical URLs and clone public repositories
    while the credentialed provider remains the gate for consolidation.
    """
    matched = _spec_for(repo_url, config)
    if matched is None:
        return None
    spec, ref = matched
    return spec.identity(ref.host, transport), ref


def unsupported_reason(
    repo_url: str | None, config: VogtConfig | None = None
) -> str | None:
    """Why no forge can read this repository, or `None` if one can.

    An empty success is the failure mode this exists to prevent. `forge
    onboard` against a Forgejo project returned `issues: 0, pull_requests: 0,
    …, detail: null` for a repository with an open issue in it — byte-identical
    to the honest answer for a repository with no history at all. The reader
    of an empty consolidation treats it as a signal, so the signal has to
    carry its own cause.

    Takes the config because "supported" is partly configuration now: a
    Forgejo host is readable exactly when `[forge_token_files]` names it.
    Without the config the constant hosts still answer, and a configured
    Forgejo host would be misreported — pass the config wherever one exists.
    """
    if not repo_url:
        return (
            "this project declares no repository URL, so there is no forge to "
            "read — which is 'not collected', not 'there is nothing'"
        )
    if _spec_for(repo_url, config) is not None:
        return None
    host = repo_url.split("://")[-1].split("/")[0] or repo_url
    readable = ", ".join(supported_hosts(config))
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
        for host in spec.hosts(config):
            if spec.build(host, token_file_for(config, host), None) is not None:
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
    return _build_github(GITHUB_HOST, token_file, transport)  # type: ignore[return-value]


__all__ = [
    "github_identity",
    "github_provider",
    "has_configured_forge",
    "identity_for",
    "provider_for",
    "provider_for_host",
    "provider_for_token",
    "supported_hosts",
    "token_file_for",
    "unsupported_reason",
]
