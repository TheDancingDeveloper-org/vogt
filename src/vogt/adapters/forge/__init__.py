"""The forge-provider seam (D2).

One interface, `ForgeProvider`, that every forge Vogt speaks to satisfies;
one registry, `provider_for`, that maps a repository URL to the provider that
hosts it. GitHub is the only implementation in v1 — the interface is designed
against GitLab and Gitea/Forgejo on paper (see `provider.py`) so a second
forge is a new class here rather than a change at every call site.
"""

from __future__ import annotations

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
