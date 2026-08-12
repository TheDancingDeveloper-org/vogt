"""Onboarding a repository: read-only consolidation (FR-B3).

Enabling the adapter for a repository that already has years of issues must
be **non-destructive**. Existing GitHub objects are authoritative for
themselves: nothing is created, nothing is edited, nothing is closed. The
backfill reads history into observations, `adopt` attaches declared items to
what is already there, and disagreement surfaces as a drift proposal rather
than a correction pushed upstream.

The difference from an ordinary sweep is only depth: a sweep takes the open
page, consolidation walks the history. Both use exactly the same read
paths — which is the point, because a separate "import" code path is where
a mutation would eventually get added by accident.
"""

from __future__ import annotations

from collections.abc import Iterable
from dataclasses import dataclass

from vogt.adapters.github.client import (
    DEFAULT_PER_PAGE,
    GitHubClient,
    GitHubUnavailable,
    repo_of,
)
from vogt.adapters.github.collectors import (
    KIND_ISSUE,
    KIND_PULL_REQUEST,
    KIND_RELEASE,
)
from vogt.collectors.base import CollectorContext, Finding, finding
from vogt.core.entities import Project

KIND_LABEL = "forge.label"

#: How far back a consolidation walks. Bounded because "all of history" on a
#: busy repository is thousands of requests, and a backfill that never
#: finishes is indistinguishable from one that never started. `coverage`
#: reports what was actually swept, so a truncated backfill is visible rather
#: than silently partial (FR-O4).
DEFAULT_MAX_PAGES = 10


@dataclass(frozen=True)
class ConsolidationReport:
    """What onboarding read, and what it deliberately did not do."""

    repo: str
    issues: int = 0
    pull_requests: int = 0
    labels: int = 0
    releases: int = 0
    truncated: bool = False
    mutations: int = 0  # always zero; asserted, not assumed


class GitHubConsolidator:
    """Reads a repository's existing state into observations. Reads only."""

    def __init__(
        self, client: GitHubClient, *, max_pages: int = DEFAULT_MAX_PAGES
    ) -> None:
        self._client = client
        self._max_pages = max_pages

    @property
    def name(self) -> str:
        return "gh-consolidate"

    @property
    def requires_network(self) -> bool:
        return True

    def collect(self, ctx: CollectorContext, project: Project) -> Iterable[Finding]:
        del ctx
        repo = repo_of(project.repo_url)
        if repo is None:
            return []
        owner, name = repo

        findings: list[Finding] = []
        findings += self._issues(project, owner, name)
        findings += self._labels(project, owner, name)
        findings += self._releases(project, owner, name)
        return findings

    # -- readers -----------------------------------------------------------

    def _pages(self, path: str, **params: str | int) -> Iterable[list[object]]:
        """Walk pages until one comes back short, or the cap is reached."""
        for page in range(1, self._max_pages + 1):
            try:
                payload = self._client.get(
                    path, per_page=DEFAULT_PER_PAGE, page=page, **params
                )
            except GitHubUnavailable:
                return
            if not isinstance(payload, list) or not payload:
                return
            yield payload
            if len(payload) < DEFAULT_PER_PAGE:
                return

    def _issues(self, project: Project, owner: str, name: str) -> list[Finding]:
        """Every issue and pull request, open *and* closed.

        `state=all` is the whole difference between a sweep and a
        consolidation: an estate's history is mostly closed work, and a
        backfill that only reads open items has not backfilled anything.
        """
        found: list[Finding] = []
        for page in self._pages(
            f"/repos/{owner}/{name}/issues", state="all", direction="asc"
        ):
            for item in page:
                if not isinstance(item, dict):
                    continue
                number = item.get("number")
                is_pr = "pull_request" in item
                found.append(
                    finding(
                        kind=KIND_PULL_REQUEST if is_pr else KIND_ISSUE,
                        subject_key=f"gh:{owner}/{name}#{number}",
                        project=project,
                        source_url=item.get("html_url"),
                        # Closed history is context, not backlog: promoting it
                        # would put years of finished work into the ranked
                        # view on the day somebody onboards a repository.
                        promoted=(not is_pr and item.get("state") == "open"),
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
                            "closed_at": item.get("closed_at"),
                            "updated_at": item.get("updated_at"),
                            "repo": f"{owner}/{name}",
                            "consolidated": True,
                        },
                    )
                )
        return found

    def _labels(self, project: Project, owner: str, name: str) -> list[Finding]:
        found: list[Finding] = []
        for page in self._pages(f"/repos/{owner}/{name}/labels"):
            for item in page:
                if not isinstance(item, dict):
                    continue
                found.append(
                    finding(
                        kind=KIND_LABEL,
                        subject_key=f"ghlabel:{owner}/{name}/{item.get('name')}",
                        project=project,
                        payload={
                            "name": item.get("name"),
                            "color": item.get("color"),
                            "description": item.get("description"),
                            "repo": f"{owner}/{name}",
                        },
                    )
                )
        return found

    def _releases(self, project: Project, owner: str, name: str) -> list[Finding]:
        found: list[Finding] = []
        for page in self._pages(f"/repos/{owner}/{name}/releases"):
            for item in page:
                if not isinstance(item, dict):
                    continue
                tag = item.get("tag_name", "")
                found.append(
                    finding(
                        kind=KIND_RELEASE,
                        subject_key=f"release:{owner}/{name}@{tag}",
                        project=project,
                        source_url=item.get("html_url"),
                        payload={
                            "tag": tag,
                            "name": item.get("name"),
                            "published_at": item.get("published_at"),
                            "repo": f"{owner}/{name}",
                            "source": "github release",
                            "consolidated": True,
                        },
                    )
                )
        return found
