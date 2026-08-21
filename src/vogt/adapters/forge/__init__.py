"""The forge-provider seam (D2).

One interface, `ForgeProvider`, that every forge Vogt speaks to satisfies;
one registry, `provider_for`, that maps a repository URL to the provider that
hosts it. GitHub was the only implementation in v1; Forgejo/Gitea joined in
Phase 5 (#176) as a new class and one registry entry — the change-at-no-call-
site the seam was designed to make true.
"""

from __future__ import annotations

from vogt.adapters.forge.forgejo import (
    ForgejoClient,
    ForgejoProvider,
    ForgejoUnavailable,
)
from vogt.adapters.forge.github import GITHUB_CAPABILITIES, GitHubProvider
from vogt.adapters.forge.kinds import (
    KIND_CHECK,
    KIND_ISSUE,
    KIND_LABEL,
    KIND_NOTIFICATION,
    KIND_POSTURE,
    KIND_PULL_REQUEST,
    KIND_RELEASE,
    KIND_SYNC,
    current_collector,
)
from vogt.adapters.forge.models import (
    ForgeCapabilities,
    ForgeCheck,
    ForgeIssue,
    ForgeLabel,
    ForgeNotification,
    ForgePosture,
    ForgePull,
    ForgeRelease,
    ForgeRepo,
    RepoRef,
)
from vogt.adapters.forge.provider import ForgeProvider
from vogt.adapters.forge.registry import (
    github_identity,
    github_provider,
    has_configured_forge,
    provider_for,
    supported_hosts,
    token_file_for,
    unsupported_reason,
)

__all__ = [
    "GITHUB_CAPABILITIES",
    "KIND_CHECK",
    "KIND_ISSUE",
    "KIND_LABEL",
    "KIND_NOTIFICATION",
    "KIND_POSTURE",
    "KIND_PULL_REQUEST",
    "KIND_RELEASE",
    "KIND_SYNC",
    "ForgeCapabilities",
    "ForgeCheck",
    "ForgeIssue",
    "ForgeLabel",
    "ForgeNotification",
    "ForgePosture",
    "ForgeProvider",
    "ForgePull",
    "ForgeRelease",
    "ForgeRepo",
    "ForgejoClient",
    "ForgejoProvider",
    "ForgejoUnavailable",
    "GitHubProvider",
    "RepoRef",
    "current_collector",
    "github_identity",
    "github_provider",
    "has_configured_forge",
    "provider_for",
    "supported_hosts",
    "token_file_for",
    "unsupported_reason",
]
