"""Read-only GitHub collectors (FR-O5a).

Issues, pull requests, Actions runs, and releases — as observations, in the
same store and the same shape as everything the offline collectors find. CI
is modelled as a generic per-revision check (FR-O6): GitHub Actions is one
producer of those, not the model.

No writes here. Consolidation lives in `consolidate.py`, update-automation
posture in `posture.py`, and write-back in `writeback.py` — separate files
because "reads the forge" and "changes the forge" should not share one.
"""

from __future__ import annotations

from collections.abc import Iterable
from typing import Any

from vogt.adapters.github.client import (
    DEFAULT_PER_PAGE,
    GitHubClient,
    GitHubUnavailable,
    Transport,
    repo_of,
)
from vogt.collectors.base import Collector, CollectorContext, Finding, finding
from vogt.config import VogtConfig
from vogt.core.entities import Project

KIND_ISSUE = "forge.issue"
KIND_PULL_REQUEST = "forge.pull_request"
KIND_CHECK = "ci.check"
KIND_RELEASE = "release"


class _GitHubCollector:
    """Shared plumbing: resolve the repo, call the API, skip what is absent."""

    kind = ""
    endpoint = ""

    def __init__(self, client: GitHubClient) -> None:
        self._client = client

    @property
    def requires_network(self) -> bool:
        return True

    def collect(self, ctx: CollectorContext, project: Project) -> Iterable[Finding]:
        del ctx
        repo = repo_of(project.repo_url)
        if repo is None:
            # Not a GitHub project. Nothing to say — and saying nothing is
            # different from saying there is nothing (FR-O4).
            return []
        owner, name = repo
        # Merged rather than splatted alongside a default: a subclass that
        # sets its own `per_page` would otherwise pass the argument twice.
        query: dict[str, str | int] = {"per_page": DEFAULT_PER_PAGE}
        query.update(self.query)
        payloads = self._client.get(
            self.endpoint.format(owner=owner, repo=name), **query
        )
        if payloads is None:
            return []
        if isinstance(payloads, dict):
            payloads = payloads.get(self.envelope_key, [])
        return list(self.to_findings(project, owner, name, payloads))

    @property
    def query(self) -> dict[str, str | int]:
        return {}

    @property
    def envelope_key(self) -> str:
        return ""

    def to_findings(
        self, project: Project, owner: str, repo: str, payloads: list[Any]
    ) -> Iterable[Finding]:
        raise NotImplementedError  # pragma: no cover - subclasses implement


class GitHubIssueCollector(_GitHubCollector):
    """Open issues, which are work the estate already has."""

    endpoint = "/repos/{owner}/{repo}/issues"

    @property
    def name(self) -> str:
        return "gh-issues"

    @property
    def query(self) -> dict[str, str | int]:
        return {"state": "open"}

    def to_findings(
        self, project: Project, owner: str, repo: str, payloads: list[Any]
    ) -> Iterable[Finding]:
        for item in payloads:
            if not isinstance(item, dict) or "pull_request" in item:
                # GitHub returns PRs from the issues endpoint. They are
                # collected separately, with their own kind.
                continue
            number = item.get("number")
            yield finding(
                kind=KIND_ISSUE,
                subject_key=f"gh:{owner}/{repo}#{number}",
                project=project,
                source_url=item.get("html_url"),
                promoted=True,
                payload={
                    "number": number,
                    "title": item.get("title", ""),
                    "state": item.get("state", "open"),
                    "labels": [
                        label.get("name")
                        for label in item.get("labels", [])
                        if isinstance(label, dict)
                    ],
                    "author": (item.get("user") or {}).get("login"),
                    "assignees": [
                        (a or {}).get("login") for a in item.get("assignees", [])
                    ],
                    "comments": item.get("comments", 0),
                    "updated_at": item.get("updated_at"),
                    "repo": f"{owner}/{repo}",
                },
            )


class GitHubPullRequestCollector(_GitHubCollector):
    """Open pull requests."""

    endpoint = "/repos/{owner}/{repo}/pulls"

    @property
    def name(self) -> str:
        return "gh-prs"

    @property
    def query(self) -> dict[str, str | int]:
        return {"state": "open"}

    def to_findings(
        self, project: Project, owner: str, repo: str, payloads: list[Any]
    ) -> Iterable[Finding]:
        for item in payloads:
            if not isinstance(item, dict):
                continue
            number = item.get("number")
            yield finding(
                kind=KIND_PULL_REQUEST,
                subject_key=f"gh:{owner}/{repo}#{number}",
                project=project,
                source_url=item.get("html_url"),
                payload={
                    "number": number,
                    "title": item.get("title", ""),
                    "state": item.get("state", "open"),
                    "draft": bool(item.get("draft", False)),
                    "author": (item.get("user") or {}).get("login"),
                    "head": (item.get("head") or {}).get("sha"),
                    "base": (item.get("base") or {}).get("ref"),
                    "updated_at": item.get("updated_at"),
                    "repo": f"{owner}/{repo}",
                },
            )


class GitHubActionsCollector(_GitHubCollector):
    """Workflow runs, as generic per-revision checks (FR-O6)."""

    endpoint = "/repos/{owner}/{repo}/actions/runs"

    @property
    def name(self) -> str:
        return "gh-actions"

    @property
    def envelope_key(self) -> str:
        return "workflow_runs"

    @property
    def query(self) -> dict[str, str | int]:
        return {"per_page": 20}

    def to_findings(
        self, project: Project, owner: str, repo: str, payloads: list[Any]
    ) -> Iterable[Finding]:
        for item in payloads:
            if not isinstance(item, dict):
                continue
            sha = item.get("head_sha", "")
            workflow = item.get("name", "workflow")
            yield finding(
                kind=KIND_CHECK,
                subject_key=f"ci:{owner}/{repo}@{sha}:{workflow}",
                project=project,
                source_url=item.get("html_url"),
                payload={
                    # Shaped as a check, not as a GitHub run: the model is
                    # "a check on a revision", and Actions is one producer.
                    "revision": sha,
                    "check": workflow,
                    "status": item.get("status"),
                    "conclusion": item.get("conclusion"),
                    "branch": item.get("head_branch"),
                    "event": item.get("event"),
                    "run_number": item.get("run_number"),
                    "updated_at": item.get("updated_at"),
                    "repo": f"{owner}/{repo}",
                },
            )


class GitHubReleaseCollector(_GitHubCollector):
    """Releases, which is where an observed version comes from (FR-P3)."""

    endpoint = "/repos/{owner}/{repo}/releases"

    @property
    def name(self) -> str:
        return "gh-releases"

    def to_findings(
        self, project: Project, owner: str, repo: str, payloads: list[Any]
    ) -> Iterable[Finding]:
        for item in payloads:
            if not isinstance(item, dict):
                continue
            tag = item.get("tag_name", "")
            yield finding(
                kind=KIND_RELEASE,
                subject_key=f"release:{owner}/{repo}@{tag}",
                project=project,
                source_url=item.get("html_url"),
                payload={
                    "tag": tag,
                    "name": item.get("name"),
                    "draft": bool(item.get("draft", False)),
                    "prerelease": bool(item.get("prerelease", False)),
                    "published_at": item.get("published_at"),
                    "repo": f"{owner}/{repo}",
                    "source": "github release",
                },
            )


def github_collectors(
    config: VogtConfig, *, transport: Transport | None = None
) -> list[Collector]:
    """The GitHub collectors, or none at all when unconfigured.

    An empty list is the ordinary case. It is what makes the forge-less test
    layer real rather than mocked, and what makes "no GitHub" a supported
    deployment rather than a degraded one.
    """
    client = GitHubClient.from_token_file(config.github_token_file, transport=transport)
    if client is None:
        return []
    from vogt.adapters.github.notifications import GitHubNotificationCollector
    from vogt.adapters.github.posture import GitHubPostureCollector

    return [
        GitHubIssueCollector(client),
        GitHubPullRequestCollector(client),
        GitHubActionsCollector(client),
        GitHubReleaseCollector(client),
        GitHubPostureCollector(client),
        GitHubNotificationCollector(client),
    ]


__all__ = [
    "KIND_CHECK",
    "KIND_ISSUE",
    "KIND_PULL_REQUEST",
    "KIND_RELEASE",
    "GitHubActionsCollector",
    "GitHubIssueCollector",
    "GitHubPullRequestCollector",
    "GitHubReleaseCollector",
    "GitHubUnavailable",
    "github_collectors",
]
