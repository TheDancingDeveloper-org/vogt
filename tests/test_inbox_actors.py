"""Who caused an Inbox entry, the *External people only* filter, the saved
per-account Inbox filter, and the badge that honours it (WI-837..840).

Layered the way the feature is: the pure classifier first, then the
collector that resolves an author at collect time (and only there), then the
Inbox read and its filter, then the per-actor preference store, and last the
sidebar badge — which must agree with `inbox.list` under the same filter and
must never reach the forge.
"""

from __future__ import annotations

import json
from dataclasses import replace
from typing import Any

import pytest

from vogt.adapters.forge import KIND_NOTIFICATION
from vogt.adapters.forge.actors import ActorResolver
from vogt.adapters.forge.github import GitHubProvider
from vogt.adapters.forge.models import ForgeNotification, RepoRef
from vogt.adapters.github.client import GitHubClient
from vogt.application.context import AppContext
from vogt.application.models import (
    InboxArchiveParams,
    InboxListParams,
    InboxSavedFilter,
    ObservationsParams,
    PlaceMetricsParams,
    PreferenceGetParams,
    PreferenceSetParams,
    RegisterProjectParams,
    SweepParams,
)
from vogt.application.services import (
    archive_inbox,
    get_preferences,
    list_inbox,
    observations,
    place_metrics,
    register_project,
    set_preference,
    sweep,
)
from vogt.application.services.inbox import inbox_badge
from vogt.config import VogtConfig
from vogt.core.actors import (
    DEFAULT_BOT_LOGINS,
    ActorClass,
    ActorFacts,
    classify,
    matches,
    normalise_bot_logins,
)
from vogt.core.principal import Principal
from vogt.errors import InvalidPreference, PreferenceVersionConflict

WHY = "inbox actor test"
API = "https://api.github.com/repos/acme/widgets"
REPO = "https://github.com/acme/widgets"
BOTS = normalise_bot_logins(DEFAULT_BOT_LOGINS)


# -- the classifier ------------------------------------------------------------


@pytest.mark.parametrize(
    ("facts", "kind", "relation"),
    [
        # The org list is the source of truth.
        (
            ActorFacts(login="alice", user_type="User", org_member=True),
            "human",
            "org_member",
        ),
        (
            ActorFacts(login="mallory", user_type="User", org_member=False),
            "human",
            "external",
        ),
        # An outside collaborator is external: not on the org's list.
        (
            ActorFacts(
                login="col",
                user_type="User",
                association="COLLABORATOR",
                org_member=False,
            ),
            "human",
            "external",
        ),
        # A private member is missing from a list a non-member token reads, but
        # GitHub still reports MEMBER — a member either way.
        (
            ActorFacts(
                login="quiet", user_type="User", association="MEMBER", org_member=False
            ),
            "human",
            "org_member",
        ),
        # No list: the association decides.
        (ActorFacts(login="owner", association="OWNER"), "human", "org_member"),
        (ActorFacts(login="drive-by", association="NONE"), "human", "external"),
        (
            ActorFacts(login="first", association="FIRST_TIME_CONTRIBUTOR"),
            "human",
            "external",
        ),
        # Neither: unknown, never guessed.
        (ActorFacts(login="who"), "human", "unknown"),
        (ActorFacts(), None, "unknown"),
    ],
)
def test_relation_follows_the_org_list_then_the_association(
    facts: ActorFacts, kind: str | None, relation: str
) -> None:
    actor = classify(facts, bots=BOTS)
    assert actor.kind == kind
    assert actor.relation == relation


@pytest.mark.parametrize(
    "facts",
    [
        ActorFacts(login="some-app", user_type="Bot"),
        ActorFacts(login="dependabot[bot]", user_type="User", org_member=False),
        ActorFacts(login="Renovate", association="NONE"),
        ActorFacts(login="github-actions", org_member=False),
    ],
)
def test_a_bot_is_never_external(facts: ActorFacts) -> None:
    actor = classify(facts, bots=BOTS)
    assert actor.kind == "bot"
    assert not matches(actor, "external")
    assert not matches(actor, "org")
    assert matches(actor, "bot")


def test_the_bot_list_is_configurable() -> None:
    facts = ActorFacts(login="release-robot", org_member=False)
    assert classify(facts, bots=BOTS).kind == "human"
    assert classify(facts, bots=normalise_bot_logins(["Release-Robot"])).kind == "bot"


def test_the_config_default_bot_list_is_the_core_default() -> None:
    assert tuple(VogtConfig().inbox_bot_logins) == DEFAULT_BOT_LOGINS


def test_the_filter_vocabulary() -> None:
    external = ActorClass(login="m", kind="human", relation="external")
    member = ActorClass(login="a", kind="human", relation="org_member")
    unknown = ActorClass(login=None, kind=None, relation="unknown")
    assert [matches(external, f) for f in ("any", "external", "org", "bot")] == [
        True,
        True,
        False,
        False,
    ]
    assert [matches(member, f) for f in ("any", "external", "org", "bot")] == [
        True,
        False,
        True,
        False,
    ]
    assert [matches(unknown, f) for f in ("any", "external", "org", "bot")] == [
        True,
        False,
        False,
        False,
    ]


# -- the fake forge --------------------------------------------------------------


def _thread(
    thread: str, title: str, comment: str | None, updated: str
) -> dict[str, Any]:
    return {
        "id": thread,
        "reason": "comment",
        "unread": True,
        "updated_at": updated,
        "subject": {
            "title": title,
            "type": "Issue",
            "url": f"{API}/issues/{thread}",
            "latest_comment_url": None
            if comment is None
            else f"{API}/issues/comments/{comment}",
        },
    }


class Forge:
    """GitHub, as far as notifications, their authors and the org go."""

    def __init__(self) -> None:
        self.notifications = [
            _thread("1", "Outsider asks a question", "11", "2026-08-11T09:00:00Z"),
            _thread("2", "Member replies", "12", "2026-08-11T08:00:00Z"),
            _thread("3", "Dependabot bumps a pin", "13", "2026-08-11T07:00:00Z"),
            # No comment URL: the subject's author is the answer.
            _thread("4", "Collaborator opens an issue", None, "2026-08-11T06:00:00Z"),
            {
                "id": "5",
                "reason": "ci_activity",
                "unread": False,
                "updated_at": "2026-08-11T05:00:00Z",
                "subject": {
                    "title": "CI run failed",
                    "type": "CheckSuite",
                    "url": None,
                },
            },
        ]
        self.authors: dict[str, dict[str, Any]] = {
            f"{API}/issues/comments/11": {
                "user": {"login": "mallory", "type": "User"},
                "author_association": "NONE",
            },
            f"{API}/issues/comments/12": {
                "user": {"login": "alice", "type": "User"},
                "author_association": "MEMBER",
            },
            f"{API}/issues/comments/13": {
                "user": {"login": "dependabot[bot]", "type": "Bot"},
                "author_association": "NONE",
            },
            f"{API}/issues/4": {
                "user": {"login": "carol", "type": "User"},
                "author_association": "COLLABORATOR",
            },
        }
        self.members: list[dict[str, Any]] | None = [
            {"login": "alice"},
            {"login": "Bob"},
        ]
        self.requests: list[str] = []

    def author_reads(self) -> list[str]:
        return [
            url
            for url in self.requests
            if "/notifications" not in url and "/orgs/" not in url
        ]

    def __call__(
        self, url: str, headers: dict[str, str], body: bytes = b"", method: str = "GET"
    ) -> tuple[int, bytes]:
        del headers, body, method
        self.requests.append(url)
        if "/notifications" in url:
            return 200, json.dumps(self.notifications).encode()
        if "/orgs/acme/members" in url:
            if self.members is None:
                return 404, b""
            return 200, json.dumps(self.members).encode()
        bare = url.split("?", 1)[0]
        if bare in self.authors:
            return 200, json.dumps(self.authors[bare]).encode()
        return 404, b""


@pytest.fixture
def forge(instance: AppContext, monkeypatch: pytest.MonkeyPatch) -> Forge:
    fake = Forge()

    def configured(cls: Any, path: Any, *, transport: Any = None) -> GitHubClient:
        del cls, path, transport
        return GitHubClient(token="ghp_fake", transport=fake)

    monkeypatch.setattr(GitHubClient, "from_token_file", classmethod(configured))
    register_project(
        instance,
        RegisterProjectParams(
            name="widgets", root_path="/srv/widgets", repo_url=REPO, reason=WHY
        ),
    )
    return fake


def _sweep(ctx: AppContext) -> None:
    sweep(ctx, SweepParams(collectors=["forge-notifications"], reason=WHY))


def _by_title(ctx: AppContext, **params: Any) -> dict[str, Any]:
    result = list_inbox(ctx, InboxListParams(**params))
    return {entry.title: entry for entry in result.entries}


# -- capture at collect time -------------------------------------------------------


def test_each_notification_records_who_caused_it(
    instance: AppContext, forge: Forge
) -> None:
    _sweep(instance)
    entries = _by_title(instance)

    outsider = entries["Outsider asks a question"]
    assert (outsider.actor_login, outsider.actor_kind, outsider.actor_relation) == (
        "mallory",
        "human",
        "external",
    )
    member = entries["Member replies"]
    assert (member.actor_kind, member.actor_relation) == ("human", "org_member")
    assert entries["Dependabot bumps a pin"].actor_kind == "bot"
    # Org list wins over a COLLABORATOR association: an outside collaborator
    # is external (operator decision).
    collaborator = entries["Collaborator opens an issue"]
    assert (collaborator.actor_login, collaborator.actor_relation) == (
        "carol",
        "external",
    )
    # A workflow-run thread is GitHub Actions.
    ci = entries["CI run failed"]
    assert (ci.actor_login, ci.actor_kind) == ("github-actions[bot]", "bot")


def test_the_raw_facts_are_stored_on_the_observation(
    instance: AppContext, forge: Forge
) -> None:
    _sweep(instance)
    found = observations(instance, ObservationsParams(kind=KIND_NOTIFICATION))
    by_thread = {o.payload["thread"]: o.payload["actor"] for o in found.observations}
    assert by_thread["1"] == {
        "login": "mallory",
        "user_type": "User",
        "association": "NONE",
        "org_member": False,
        "resolved_from": f"{API}/issues/comments/11",
        "resolved_for": "2026-08-11T09:00:00Z",
    }


def test_an_unchanged_thread_is_not_resolved_twice(
    instance: AppContext, forge: Forge
) -> None:
    """The previous observation is the persistent cache: a second sweep over
    unchanged threads reads no author, even with the in-process cache gone
    (a restart)."""
    _sweep(instance)
    assert len(forge.author_reads()) == 4
    from vogt.adapters.forge.actors import RESOLVER

    RESOLVER.clear()
    forge.requests.clear()
    _sweep(instance)
    assert forge.author_reads() == []


def test_a_new_comment_on_a_thread_is_resolved_again(
    instance: AppContext, forge: Forge
) -> None:
    _sweep(instance)
    forge.requests.clear()
    forge.notifications[0] = _thread(
        "1", "Outsider asks a question", "12", "2026-08-12T09:00:00Z"
    )
    _sweep(instance)
    assert forge.author_reads() == [f"{API}/issues/comments/12"]
    entry = _by_title(instance)["Outsider asks a question"]
    assert entry.actor_login == "alice"


def test_a_resolved_author_does_not_resurrect_an_archived_entry(
    instance: AppContext, forge: Forge
) -> None:
    """The actor block is excluded from the occurrence digest: resolving an
    author on a later sweep must not turn an archived entry into a new one."""
    forge.authors.pop(f"{API}/issues/comments/11")
    _sweep(instance)
    first = _by_title(instance)["Outsider asks a question"]
    assert first.actor_relation == "unknown"
    archive_inbox(instance, InboxArchiveParams(entry_key=first.entry_key, reason=WHY))

    forge.authors[f"{API}/issues/comments/11"] = {
        "user": {"login": "mallory", "type": "User"},
        "author_association": "NONE",
    }
    _sweep(instance)
    archived = _by_title(instance, triage_states=["archived"])
    assert archived["Outsider asks a question"].entry_key == first.entry_key
    assert archived["Outsider asks a question"].actor_relation == "external"
    assert "Outsider asks a question" not in _by_title(instance)


def test_without_an_org_list_the_association_decides(
    instance: AppContext, forge: Forge
) -> None:
    forge.members = None  # a user-owned repository: no org to list
    _sweep(instance)
    entries = _by_title(instance)
    assert entries["Member replies"].actor_relation == "org_member"
    assert entries["Outsider asks a question"].actor_relation == "external"
    assert entries["Collaborator opens an issue"].actor_relation == "external"


def test_a_sweep_spends_a_bounded_number_of_author_reads() -> None:
    calls: list[str] = []

    def transport(
        url: str, headers: Any, body: bytes = b"", method: str = "GET"
    ) -> Any:
        del headers, body, method
        calls.append(url)
        return 200, json.dumps({"user": {"login": "x", "type": "User"}}).encode()

    provider = GitHubProvider(GitHubClient(token="t", transport=transport))
    resolver = ActorResolver()
    ref = RepoRef(host="github.com", owner="acme", repo="widgets")
    budget = [2]
    blocks = [
        resolver.actor_block(
            provider,
            ref,
            ForgeNotification(
                thread=str(n),
                repo=ref.slug,
                updated_at="2026-08-11T00:00:00Z",
                latest_comment_api_url=f"{API}/issues/comments/{n}",
            ),
            prior=None,
            budget=budget,
        )
        for n in range(5)
    ]
    assert sum(block is not None for block in blocks) == 2
    assert len([url for url in calls if "/comments/" in url]) == 2


def test_a_url_outside_the_forge_api_is_never_followed() -> None:
    calls: list[str] = []

    def transport(
        url: str, headers: Any, body: bytes = b"", method: str = "GET"
    ) -> Any:
        del headers, body, method
        calls.append(url)
        return 200, b"{}"

    provider = GitHubProvider(GitHubClient(token="t", transport=transport))
    assert provider.resolve_actor("https://evil.example/repos/a/b/issues/1") is None
    assert provider.resolve_actor("https://api.github.com/user") is None
    assert provider.resolve_actor(f"{API}/../../user") is None
    assert calls == []


# -- the filter --------------------------------------------------------------------


def test_external_people_only(instance: AppContext, forge: Forge) -> None:
    _sweep(instance)
    result = list_inbox(instance, InboxListParams(actor="external"))
    assert sorted(entry.title for entry in result.entries) == [
        "Collaborator opens an issue",
        "Outsider asks a question",
    ]
    assert result.counts["active"] == 2
    assert result.actor_unknown_hidden == 0

    assert sorted(_by_title(instance, actor="org")) == ["Member replies"]
    assert sorted(_by_title(instance, actor="bot")) == [
        "CI run failed",
        "Dependabot bumps a pin",
    ]


def test_unknown_authors_are_hidden_and_counted(
    instance: AppContext, forge: Forge
) -> None:
    forge.authors.pop(f"{API}/issues/comments/11")
    _sweep(instance)
    result = list_inbox(instance, InboxListParams(actor="external"))
    assert [entry.title for entry in result.entries] == ["Collaborator opens an issue"]
    assert result.actor_unknown_hidden == 1


def test_a_cursor_belongs_to_its_actor_filter(
    instance: AppContext, forge: Forge
) -> None:
    _sweep(instance)
    first = list_inbox(instance, InboxListParams(actor="bot", limit=1))
    assert first.next_cursor is not None
    from vogt.errors import InvalidCursor

    with pytest.raises(InvalidCursor):
        list_inbox(
            instance, InboxListParams(actor="any", limit=1, cursor=first.next_cursor)
        )
    second = list_inbox(
        instance, InboxListParams(actor="bot", limit=1, cursor=first.next_cursor)
    )
    assert [e.actor_kind for e in second.entries] == ["bot"]


# -- the per-account preference store ------------------------------------------


def test_a_preference_round_trips_with_a_version(instance: AppContext) -> None:
    assert get_preferences(instance, PreferenceGetParams()).preferences == []
    saved = set_preference(
        instance,
        PreferenceSetParams(
            key="inbox.filter",
            value={"actor": "external", "sources": ["github"]},
            expected_version=0,
            reason=WHY,
        ),
    ).preference
    assert saved.version == 1
    # Normalised to the full shape, defaults included.
    assert saved.value == {
        "actor": "external",
        "sources": ["github"],
        "triage_states": ["active"],
    }
    again = set_preference(
        instance,
        PreferenceSetParams(
            key="inbox.filter", value={}, expected_version=1, reason=WHY
        ),
    ).preference
    assert again.version == 2 and again.value == {}
    read = get_preferences(instance, PreferenceGetParams(key="inbox.filter"))
    assert [(p.key, p.version) for p in read.preferences] == [("inbox.filter", 2)]


def test_a_stale_version_is_refused(instance: AppContext) -> None:
    set_preference(
        instance, PreferenceSetParams(key="board.filters", value={"a": 1}, reason=WHY)
    )
    with pytest.raises(PreferenceVersionConflict):
        set_preference(
            instance,
            PreferenceSetParams(
                key="board.filters", value={"a": 2}, expected_version=0, reason=WHY
            ),
        )


def test_a_known_key_is_validated(instance: AppContext) -> None:
    with pytest.raises(InvalidPreference):
        set_preference(
            instance,
            PreferenceSetParams(
                key="inbox.filter", value={"actor": "aliens"}, reason=WHY
            ),
        )
    with pytest.raises(InvalidPreference):
        set_preference(
            instance,
            PreferenceSetParams(key="x.big", value={"blob": "x" * 20_000}, reason=WHY),
        )


def test_the_cli_form_of_a_value_is_json_text() -> None:
    params = PreferenceSetParams.model_validate(
        {"key": "inbox.filter", "value": '{"actor": "bot"}', "reason": WHY}
    )
    assert params.value == {"actor": "bot"}


def test_preferences_are_per_actor_and_audited(instance: AppContext) -> None:
    set_preference(
        instance,
        PreferenceSetParams(key="inbox.filter", value={"actor": "bot"}, reason=WHY),
    )
    other = replace(
        instance,
        principal=Principal(
            identity_ref="local:someone-else", kind="human", display_name="o"
        ),
    )
    assert get_preferences(other, PreferenceGetParams()).preferences == []
    with instance.declared.read() as view:
        audit = view.list_audit(limit=10, operation="preference.set")
    assert [row.reason for row in audit] == [WHY]


# -- the badge -------------------------------------------------------------------


@pytest.mark.parametrize(
    "saved",
    [
        {"actor": "external"},
        {"actor": "org"},
        {"actor": "bot", "sources": ["github"]},
        {"triage_states": ["archived"]},
        {"sources": ["drift"]},
    ],
)
def test_the_badge_counts_what_the_list_shows_under_the_saved_filter(
    instance: AppContext, forge: Forge, saved: dict[str, Any]
) -> None:
    _sweep(instance)
    set_preference(
        instance, PreferenceSetParams(key="inbox.filter", value=saved, reason=WHY)
    )
    parsed = InboxSavedFilter.model_validate(saved)
    listed = list_inbox(
        instance,
        InboxListParams(
            sources=parsed.sources,
            actor=parsed.actor,
            triage_states=parsed.triage_states,
            limit=100,
        ),
    )
    metrics = place_metrics(instance, PlaceMetricsParams())
    assert metrics.inbox_active == len(listed.entries)
    assert metrics.inbox_filter == parsed
    assert metrics.inbox_active_unfiltered == 5


def test_without_a_saved_filter_the_badge_is_every_active_entry(
    instance: AppContext, forge: Forge
) -> None:
    _sweep(instance)
    metrics = place_metrics(instance, PlaceMetricsParams())
    assert metrics.inbox_active == metrics.inbox_active_unfiltered == 5
    assert metrics.inbox_filter is None


def test_clearing_the_filter_restores_the_unfiltered_badge(
    instance: AppContext, forge: Forge
) -> None:
    _sweep(instance)
    set_preference(
        instance,
        PreferenceSetParams(
            key="inbox.filter", value={"actor": "external"}, reason=WHY
        ),
    )
    assert place_metrics(instance, PlaceMetricsParams()).inbox_active == 2
    set_preference(
        instance, PreferenceSetParams(key="inbox.filter", value={}, reason=WHY)
    )
    assert place_metrics(instance, PlaceMetricsParams()).inbox_active == 5


def test_the_badge_never_reaches_the_forge(instance: AppContext, forge: Forge) -> None:
    _sweep(instance)
    forge.requests.clear()
    inbox_badge(instance, InboxSavedFilter(actor="external"))
    place_metrics(instance, PlaceMetricsParams())
    assert forge.requests == []


def test_the_badge_shares_one_projection_until_something_changes(
    instance: AppContext, forge: Forge, monkeypatch: pytest.MonkeyPatch
) -> None:
    _sweep(instance)
    reads: list[int] = []
    inner = instance.observed.latest

    def counting(*args: Any, **kwargs: Any) -> Any:
        reads.append(1)
        return inner(*args, **kwargs)

    monkeypatch.setattr(instance.observed, "latest", counting)
    inbox_badge(instance, None)
    built = len(reads)
    assert built > 0
    inbox_badge(instance, InboxSavedFilter(actor="external"))
    inbox_badge(instance, InboxSavedFilter(actor="bot"))
    assert len(reads) == built
    # A declared write (here: a triage) moves the key; the next read rebuilds.
    entry = list_inbox(instance, InboxListParams(limit=1)).entries[0]
    archive_inbox(instance, InboxArchiveParams(entry_key=entry.entry_key, reason=WHY))
    reads.clear()
    assert inbox_badge(instance, None).active_total == 4
    assert reads


def test_issue_and_pull_observations_keep_who_the_author_is() -> None:
    """`user.type` and `author_association` used to be dropped (WI-837)."""
    item = {
        "number": 7,
        "title": "t",
        "state": "open",
        "user": {"login": "renovate[bot]", "type": "Bot"},
        "author_association": "CONTRIBUTOR",
    }

    def transport(
        url: str, headers: Any, body: bytes = b"", method: str = "GET"
    ) -> Any:
        del headers, body, method
        return 200, json.dumps([item]).encode()

    provider = GitHubProvider(GitHubClient(token="t", transport=transport))
    ref = RepoRef(host="github.com", owner="acme", repo="widgets")
    (issue,) = list(provider.issues_updated_since(ref, None))
    (pull,) = list(provider.pulls_updated_since(ref, None))
    for observed in (issue, pull):
        assert observed.author == "renovate[bot]"
        assert observed.author_type == "Bot"
        assert observed.author_association == "CONTRIBUTOR"
