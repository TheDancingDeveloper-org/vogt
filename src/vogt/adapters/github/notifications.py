"""What GitHub is trying to tell you about a registered project (FR-O8).

Read through the **per-repository** notifications endpoint rather than the
account-wide one, and that choice is the requirement rather than an
optimisation. `GET /notifications` returns the token holder's entire inbox,
which would mean collecting everything and discarding most of it — the
system going looking, then filtering, which is exactly the shape r3 removed
(FR-G15). `GET /repos/{owner}/{repo}/notifications` can only answer about a
repository somebody registered here, so the scope rule holds by
construction.

Nothing here is promoted. A notification says something happened, not that
there is work to do, and the ranked views have one job that unfiltered
signals destroy (DESIGN §3.6). They are read through their own view instead.
"""

from __future__ import annotations

from collections.abc import Iterable
from typing import Any

from vogt.adapters.github.client import (
    DEFAULT_PER_PAGE,
    GitHubClient,
    repo_of,
)
from vogt.collectors.base import CollectorContext, Finding, finding
from vogt.core.entities import Project

KIND_NOTIFICATION = "forge.notification"

#: What GitHub calls the reason a notification exists. Recorded verbatim;
#: the view filters on it and nothing here judges which ones matter.
REASONS = (
    "assign",
    "author",
    "comment",
    "ci_activity",
    "invitation",
    "manual",
    "mention",
    "review_requested",
    "security_alert",
    "state_change",
    "subscribed",
    "team_mention",
)


class GitHubNotificationCollector:
    """Notifications for one registered repository, as observations."""

    endpoint = "/repos/{owner}/{repo}/notifications"

    def __init__(self, client: GitHubClient) -> None:
        self._client = client

    @property
    def name(self) -> str:
        return "gh-notifications"

    @property
    def requires_network(self) -> bool:
        return True

    def collect(self, ctx: CollectorContext, project: Project) -> Iterable[Finding]:
        del ctx
        repo = repo_of(project.repo_url)
        if repo is None:
            return []
        owner, name = repo
        # A `GitHubUnavailable` here is left to propagate. The commonest
        # cause by far is a token without the notifications scope, which is
        # a configuration fact rather than a fault: the sweeper turns it into
        # partial coverage and the subject stays "not collected" (FR-O4).
        payloads = self._client.get(
            self.endpoint.format(owner=owner, repo=name),
            per_page=DEFAULT_PER_PAGE,
            # Read notifications are still notifications: an inbox that
            # forgets what it showed you yesterday cannot be reviewed, and
            # `unread` is a field on the observation rather than a decision
            # made at collection time.
            all="true",
        )
        if not payloads:
            return []
        return list(self._to_findings(project, owner, name, payloads))

    def _to_findings(
        self, project: Project, owner: str, repo: str, payloads: list[Any]
    ) -> Iterable[Finding]:
        for item in payloads:
            if not isinstance(item, dict):
                continue
            thread = item.get("id")
            subject = item.get("subject") or {}
            yield finding(
                kind=KIND_NOTIFICATION,
                subject_key=f"gh:{owner}/{repo}!{thread}",
                project=project,
                source_url=_web_url(subject.get("url")),
                # Never promoted (FR-O8). See the module docstring.
                promoted=False,
                payload={
                    "thread": thread,
                    "reason": item.get("reason"),
                    "unread": bool(item.get("unread", False)),
                    "title": subject.get("title", ""),
                    "subject_type": subject.get("type"),
                    "updated_at": item.get("updated_at"),
                    "last_read_at": item.get("last_read_at"),
                    "repo": f"{owner}/{repo}",
                    "source": "github notification",
                },
            )


def _web_url(api_url: object) -> str | None:
    """Turn the API URL GitHub gives a subject into one a person can open.

    The notifications payload carries `subject.url` pointing at the REST
    resource — `api.github.com/repos/o/r/issues/7` — which is unhelpful in a
    view whose entire purpose is "take me to the thing".
    """
    if not isinstance(api_url, str) or not api_url:
        return None
    tail = api_url.partition("api.github.com/repos/")[2]
    if not tail:
        return api_url
    return "https://github.com/" + tail.replace("/pulls/", "/pull/")


__all__ = ["KIND_NOTIFICATION", "REASONS", "GitHubNotificationCollector"]
