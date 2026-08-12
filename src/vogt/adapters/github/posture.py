"""`gh-posture` — update automation, as three facts (FR-D6).

Never one boolean. "Is dependency automation on?" is three independent
questions with three independent answers, and collapsing them is how a
repository ends up with a Renovate config, no vulnerability alerts, and a
green tick:

1. **Version updates** — is there a Renovate or Dependabot configuration?
2. **Vulnerability alerts** — is GitHub told to look?
3. **Automated security fixes** — is it told to act?

A repository can have any subset. The drift kind `update_automation_gap`
names *which* of the three is missing, because "automation is incomplete" is
not something anybody can act on.
"""

from __future__ import annotations

from collections.abc import Iterable

from vogt.adapters.github.client import GitHubClient, GitHubUnavailable, repo_of
from vogt.collectors.base import CollectorContext, Finding, finding
from vogt.core.entities import Project

KIND_POSTURE = "forge.posture"

#: Where the ecosystems keep their configuration. Presence is the signal;
#: the contents are not parsed, because "is this configured" and "is this
#: configured well" are different questions and only the first is cheap.
VERSION_UPDATE_CONFIGS = (
    "renovate.json",
    "renovate.json5",
    ".github/renovate.json",
    ".github/renovate.json5",
    ".renovaterc",
    ".renovaterc.json",
    ".github/dependabot.yml",
    ".github/dependabot.yaml",
)


class GitHubPostureCollector:
    """The three automation facts, collected separately."""

    def __init__(self, client: GitHubClient) -> None:
        self._client = client

    @property
    def name(self) -> str:
        return "gh-posture"

    @property
    def requires_network(self) -> bool:
        return True

    def collect(self, ctx: CollectorContext, project: Project) -> Iterable[Finding]:
        del ctx
        repo = repo_of(project.repo_url)
        if repo is None:
            return []
        owner, name = repo

        config = self._version_update_config(owner, name)
        alerts = self._toggle(owner, name, "vulnerability-alerts")
        fixes = self._toggle(owner, name, "automated-security-fixes")

        return [
            finding(
                kind=KIND_POSTURE,
                subject_key=f"posture:{owner}/{name}",
                project=project,
                payload={
                    # Three keys, three answers. `None` means "we could not
                    # tell", which is different from `False`.
                    "version_updates_config": config,
                    "version_updates": config is not None,
                    "vulnerability_alerts": alerts,
                    "automated_security_fixes": fixes,
                    "repo": f"{owner}/{name}",
                },
            )
        ]

    def _version_update_config(self, owner: str, name: str) -> str | None:
        for path in VERSION_UPDATE_CONFIGS:
            try:
                found = self._client.get(f"/repos/{owner}/{name}/contents/{path}")
            except GitHubUnavailable:
                return None
            if found is not None:
                return path
        return None

    def _toggle(self, owner: str, name: str, endpoint: str) -> bool | None:
        """Read one repository toggle.

        GitHub answers these with 204 (on) or 404 (off), and the client turns
        a 404 into `None`. `None` from an error is deliberately preserved as
        "could not tell" rather than flattened to `False`: reporting a
        security toggle as off because the token lacked a scope would be a
        false alarm that costs somebody an afternoon.
        """
        try:
            found = self._client.get(f"/repos/{owner}/{name}/{endpoint}")
        except GitHubUnavailable:
            return None
        return found is not None
