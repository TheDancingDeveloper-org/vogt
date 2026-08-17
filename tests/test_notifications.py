"""GitHub notifications as observations, and the inbox over them (FR-O8, FR-N3).

Includes the M7 inbox demo. From `ROADMAP.md`:

    …its notifications show up in the inbox on the next sweep — while
    `/events` still contains only what this instance did.

That last clause is the point of the whole slice, and is asserted rather
than described: two feeds with different origins that never contaminate
each other.
"""

from __future__ import annotations

import json
from datetime import UTC, datetime
from typing import Any

import pytest

from vogt.adapters.github.notifications import (
    KIND_NOTIFICATION,
    GitHubNotificationCollector,
)
from vogt.application.context import AppContext
from vogt.application.models import (
    BacklogParams,
    CoverageParams,
    InboxArchiveParams,
    InboxListParams,
    InboxRestoreParams,
    InboxSnoozeParams,
    ListEventsParams,
    NotificationsParams,
    ObservationsParams,
    RegisterProjectParams,
    SweepParams,
)
from vogt.application.services import (
    archive_inbox,
    backlog,
    coverage,
    list_events,
    list_inbox,
    list_notifications,
    observations,
    register_project,
    restore_inbox,
    snooze_inbox,
    sweep,
)

WHY = "notification test"
REPO = "https://github.com/TheDancingDeveloper-org/rustnzb"

NOTIFICATIONS = [
    {
        "id": "1001",
        "reason": "review_requested",
        "unread": True,
        "updated_at": "2026-08-11T09:00:00Z",
        "subject": {
            "title": "Bound the retry loop",
            "type": "PullRequest",
            "url": "https://api.github.com/repos/TheDancingDeveloper-org/rustnzb/pulls/31",
        },
    },
    {
        "id": "1002",
        "reason": "ci_activity",
        "unread": False,
        "updated_at": "2026-08-10T09:00:00Z",
        "subject": {
            "title": "ci workflow run failed",
            "type": "CheckSuite",
            "url": None,
        },
    },
]


class Forge:
    """A fake GitHub that answers the notifications endpoint."""

    def __init__(self, *, notifications: list[dict[str, Any]] | None = None) -> None:
        self.notifications = NOTIFICATIONS if notifications is None else notifications
        self.requests: list[tuple[str, str]] = []
        self.notification_status = 200

    def __call__(
        self,
        url: str,
        headers: dict[str, str],
        body: bytes = b"",
        method: str = "GET",
    ) -> tuple[int, bytes]:
        del headers, body
        self.requests.append((method, url))
        if "/notifications" in url:
            if self.notification_status != 200:
                return self.notification_status, b'{"message":"Bad credentials"}'
            return 200, json.dumps(self.notifications).encode("utf-8")
        if "/actions/runs" in url:
            return 200, json.dumps({"workflow_runs": []}).encode("utf-8")
        if "/contents/" in url or "/vulnerability-alerts" in url:
            return 404, b""
        if "/automated-security-fixes" in url:
            return 404, b""
        return 200, b"[]"


@pytest.fixture
def forge(instance: AppContext, monkeypatch: pytest.MonkeyPatch) -> Forge:
    """A registered GitHub project wired to a fake forge."""
    fake = Forge()
    from vogt.adapters.github.client import GitHubClient

    def configured(cls: Any, path: Any, *, transport: Any = None) -> GitHubClient:
        del cls, path, transport
        return GitHubClient(token="ghp_fake", transport=fake)

    monkeypatch.setattr(GitHubClient, "from_token_file", classmethod(configured))
    register_project(
        instance,
        RegisterProjectParams(
            name="rustnzb", root_path="/srv/rustnzb", repo_url=REPO, reason=WHY
        ),
    )
    return fake


# -- collection (FR-O8) ----------------------------------------------------


def test_notifications_are_collected_for_registered_repositories(
    instance: AppContext, forge: Forge
) -> None:
    sweep(instance, SweepParams(collectors=["gh-notifications"], reason=WHY))

    found = observations(instance, ObservationsParams(kind=KIND_NOTIFICATION))
    assert found.total == 2
    titles = {str(entry.payload.get("title")) for entry in found.observations}
    assert titles == {"Bound the retry loop", "ci workflow run failed"}


def test_the_scope_is_the_registered_project_list(
    instance: AppContext, forge: Forge
) -> None:
    """FR-G15 by construction, not by filtering.

    The account-wide endpoint would return the token holder's whole inbox and
    leave Vogt to discard most of it — the system going looking, then
    filtering. Asserted by reading the URLs the adapter actually requested.
    """
    sweep(instance, SweepParams(collectors=["gh-notifications"], reason=WHY))

    notification_urls = [url for _, url in forge.requests if "/notifications" in url]
    assert notification_urls, "the collector asked GitHub nothing"
    for url in notification_urls:
        assert "/repos/TheDancingDeveloper-org/rustnzb/notifications" in url, (
            f"{url} is not scoped to a registered repository"
        )


def test_notifications_never_enter_the_ranked_views(
    instance: AppContext, forge: Forge
) -> None:
    """A notification says something happened, not that there is work.

    Promoting them would drown the backlog exactly as unfiltered markers
    would (DESIGN §3.6) — the observed-first hazard arriving through a new
    door.
    """
    sweep(instance, SweepParams(collectors=["gh-notifications"], reason=WHY))

    found = observations(instance, ObservationsParams(kind=KIND_NOTIFICATION))
    assert all(not entry.promoted for entry in found.observations)

    ranked = backlog(instance, BacklogParams())
    assert all(item.observation_kind != KIND_NOTIFICATION for item in ranked.items)


def test_a_token_without_the_scope_is_partial_coverage_not_a_failed_sweep(
    instance: AppContext, forge: Forge
) -> None:
    """FR-O4: "we could not look" is an answer, and it degrades gracefully."""
    forge.notification_status = 403

    result = sweep(instance, SweepParams(collectors=["gh-notifications"], reason=WHY))
    assert result.reports, "the sweep produced no coverage record"
    assert all(entry.outcome != "ok" for entry in result.reports)

    reported = coverage(instance, CoverageParams())
    assert any(entry.collector == "gh-notifications" for entry in reported.collectors)


def test_a_project_that_is_not_on_github_is_simply_not_asked_about(
    instance: AppContext, forge: Forge
) -> None:
    register_project(
        instance,
        RegisterProjectParams(name="Local Only", root_path="/srv/local", reason=WHY),
    )
    sweep(instance, SweepParams(collectors=["gh-notifications"], reason=WHY))

    assert len([url for _, url in forge.requests if "/notifications" in url]) == 1


def test_the_subject_url_is_one_a_person_can_open(
    instance: AppContext, forge: Forge
) -> None:
    sweep(instance, SweepParams(collectors=["gh-notifications"], reason=WHY))

    result = list_notifications(instance, NotificationsParams())
    urls = {entry.title: entry.url for entry in result.notifications}
    assert urls["Bound the retry loop"] == (
        "https://github.com/TheDancingDeveloper-org/rustnzb/pull/31"
    )
    assert urls["ci workflow run failed"] is None


def test_the_collector_needs_a_network_and_says_so() -> None:
    """NFR-PO2: the forge-less layer runs exactly the offline collectors."""
    from vogt.adapters.github.client import GitHubClient

    collector = GitHubNotificationCollector(GitHubClient())
    assert collector.requires_network is True
    assert collector.name == "gh-notifications"


# -- the view (FR-N3) ------------------------------------------------------


def test_the_inbox_reports_what_it_holds(instance: AppContext, forge: Forge) -> None:
    sweep(instance, SweepParams(collectors=["gh-notifications"], reason=WHY))

    result = list_notifications(instance, NotificationsParams())
    assert result.total == 2
    assert result.unread == 1
    assert result.by_reason == {"ci_activity": 1, "review_requested": 1}
    # Newest first: an inbox ordered any other way is a list.
    assert result.notifications[0].title == "Bound the retry loop"
    assert result.notifications[0].project_slug == "rustnzb"


def test_the_inbox_says_whose_it_is(instance: AppContext, forge: Forge) -> None:
    """FR-N3: instance-scoped, not per-actor, and stated rather than hidden.

    Notifications belong to the account whose token is configured. A reader
    who assumes otherwise is reading somebody else's inbox believing it is
    theirs, which is worse than not having the view.
    """
    sweep(instance, SweepParams(collectors=["gh-notifications"], reason=WHY))

    result = list_notifications(instance, NotificationsParams())
    assert "instance-scoped" in result.scope
    assert "not per-actor" in result.scope


@pytest.mark.parametrize(
    ("params", "expected"),
    [
        (NotificationsParams(unread_only=True), 1),
        (NotificationsParams(reason="ci_activity"), 1),
        (NotificationsParams(reason="mention"), 0),
        (NotificationsParams(project="rustnzb"), 2),
    ],
)
def test_the_inbox_filters(
    instance: AppContext, forge: Forge, params: NotificationsParams, expected: int
) -> None:
    sweep(instance, SweepParams(collectors=["gh-notifications"], reason=WHY))
    assert list_notifications(instance, params).total == expected


def test_an_empty_inbox_distinguishes_absence_from_ignorance(
    instance: AppContext,
) -> None:
    """FR-O4, at the view where the two look most alike."""
    result = list_notifications(instance, NotificationsParams())
    assert result.total == 0
    assert result.detail is not None
    assert result.freshness.status == "never_swept"


def test_the_inbox_carries_freshness(instance: AppContext, forge: Forge) -> None:
    sweep(instance, SweepParams(collectors=["gh-notifications"], reason=WHY))
    result = list_notifications(instance, NotificationsParams())
    assert result.freshness.status != "never_swept"


def test_the_forge_inbox_never_leaks_into_the_events_feed(
    instance: AppContext, forge: Forge
) -> None:
    """The M7 inbox demo's real assertion (FR-N1 vs FR-N3).

    `/events` is what *this instance* did, ordered by a cursor it owns.
    A forge's inbox has a different origin and a different owner; merging
    them would make the cursor meaningless and the provenance unreadable.
    """
    sweep(instance, SweepParams(collectors=["gh-notifications"], reason=WHY))

    kinds = {event.kind for event in list_events(instance, ListEventsParams()).events}
    assert KIND_NOTIFICATION not in kinds
    assert not any(kind.startswith("forge.") for kind in kinds)
    assert "sweep.completed" in kinds


def test_normalized_inbox_paginates_and_audits_occurrence_triage(
    instance: AppContext, forge: Forge
) -> None:
    sweep(instance, SweepParams(collectors=["gh-notifications"], reason=WHY))

    first_page = list_inbox(instance, InboxListParams(limit=1))
    assert len(first_page.entries) == 1
    assert first_page.next_cursor is not None
    assert first_page.coverage["github"].status == "ok"
    assert first_page.github_scope == "registered projects only"

    second_page = list_inbox(
        instance, InboxListParams(limit=1, cursor=first_page.next_cursor)
    )
    assert len(second_page.entries) == 1
    assert second_page.entries[0].entry_key != first_page.entries[0].entry_key

    entry_key = first_page.entries[0].entry_key
    snoozed = snooze_inbox(
        instance,
        InboxSnoozeParams(
            entry_key=entry_key,
            until=datetime(2099, 1, 1, tzinfo=UTC),
            reason=WHY,
        ),
    )
    assert snoozed.entry.triage_state == "snoozed"
    assert (
        list_inbox(instance, InboxListParams(triage_states=["snoozed"]))
        .entries[0]
        .entry_key
        == entry_key
    )

    restored = restore_inbox(
        instance,
        InboxRestoreParams(entry_key=entry_key, reason=WHY),
    )
    assert restored.entry.triage_state == "active"
    archived = archive_inbox(
        instance,
        InboxArchiveParams(entry_key=entry_key, reason=WHY),
    )
    assert archived.entry.triage_state == "archived"
    assert (
        list_inbox(instance, InboxListParams(triage_states=["archived"]))
        .entries[0]
        .entry_key
        == entry_key
    )
