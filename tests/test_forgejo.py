"""The Forgejo provider (#176) — the seam's acceptance test.

Phase 5's claim is structural: a second forge is `adapters/forge/forgejo.py`
plus one `_Spec` entry in the registry, and nothing outside the seam changes.
These tests hold the provider to the same contract the GitHub suite holds
GitHubProvider to — host selection, the adapter-optional cases, normalized
reads, the append-only write surface — and then re-run #169's regression
shape end to end against a Forgejo host, because "the interface holds" is a
claim about behavior, not signatures.

Every test injects a transport. Nothing here touches the network, and nothing
here speaks to the estate's real `repo.indexarr.net`.
"""

from __future__ import annotations

import dataclasses
import json
from pathlib import Path
from typing import Any

import pytest

from vogt.adapters.forge import (
    ForgejoClient,
    ForgejoProvider,
    ForgeProvider,
    GitHubProvider,
    RepoRef,
    has_configured_forge,
    provider_for,
    supported_hosts,
    unsupported_reason,
)
from vogt.adapters.forge.collectors import ForgePostureCollector
from vogt.adapters.github.client import Transport
from vogt.application.context import AppContext
from vogt.application.models import (
    BacklogParams,
    RegisterProjectParams,
    SweepParams,
)
from vogt.application.services import backlog, register_project, sweep
from vogt.collectors import CollectorContext
from vogt.config import VogtConfig
from vogt.core.entities import Project
from vogt.core.observed import LIFECYCLE_CLOSED, lifecycle_of
from vogt.errors import RemoteRepoExists

WHY = "forgejo provider test"
HOST = "repo.indexarr.net"
REPO = f"https://{HOST}/indexarr/Indexarr"


def _fake_transport(routes: dict[str, Any], *, status: int = 200) -> Transport:
    def transport(
        url: str, headers: dict[str, str], body: bytes = b"", method: str = "GET"
    ) -> tuple[int, bytes]:
        del headers, body, method
        for fragment, payload in routes.items():
            if fragment in url:
                return status, json.dumps(payload).encode("utf-8")
        return 404, b"{}"

    return transport


def _provider(routes: dict[str, Any] | None = None) -> ForgejoProvider:
    return ForgejoProvider(
        ForgejoClient(host=HOST, token="x", transport=_fake_transport(routes or {}))
    )


def _ref() -> RepoRef:
    return RepoRef(host=HOST, owner="indexarr", repo="Indexarr")


def _config(tmp_path: Path, **overrides: Any) -> VogtConfig:
    token = tmp_path / "forgejo-token"
    token.write_text("frg_x\n", encoding="utf-8")
    return VogtConfig(
        sqlite_synchronous="off",
        forge_token_files={HOST: token},
        **overrides,
    )


# -- host selection: a Forgejo host is data, not a constant (D8) -----------


@pytest.mark.parametrize(
    ("repo_url", "supported"),
    [
        (REPO, True),
        (f"{REPO}.git", True),
        (f"git@{HOST}:indexarr/Indexarr.git", True),
        (f"git+https://{HOST}/indexarr/Indexarr", True),
        ("https://github.com/o/r", True),  # still github's, not forgejo's
        ("https://gitlab.com/group/project", False),
        (f"https://{HOST}/only-owner", False),
        (None, False),
        ("", False),
    ],
)
def test_host_selection_table(
    tmp_path: Path, repo_url: str | None, supported: bool
) -> None:
    """With the host under `[forge_token_files]`, its URLs resolve (D2/D8)."""
    config = _config(tmp_path)
    assert (unsupported_reason(repo_url, config) is None) is supported


def test_a_configured_forgejo_url_resolves_to_the_forgejo_provider(
    tmp_path: Path,
) -> None:
    provider = provider_for(REPO, _config(tmp_path))
    assert isinstance(provider, ForgejoProvider)


def test_a_github_entry_in_the_map_is_a_token_override_not_a_forgejo(
    tmp_path: Path,
) -> None:
    """github.com under `[forge_token_files]` keeps its own provider (D8)."""
    token = tmp_path / "gh"
    token.write_text("ghp_x\n", encoding="utf-8")
    config = VogtConfig(
        sqlite_synchronous="off", forge_token_files={"github.com": token}
    )
    assert isinstance(provider_for("https://github.com/o/r", config), GitHubProvider)
    assert supported_hosts(config) == ("github.com",)


def test_without_config_the_forgejo_host_is_not_claimed(tmp_path: Path) -> None:
    """Config-less resolution answers for the constant hosts only."""
    del tmp_path
    assert unsupported_reason(REPO) is not None
    assert supported_hosts() == ("github.com",)


def test_supported_hosts_names_the_configured_forgejo(tmp_path: Path) -> None:
    assert supported_hosts(_config(tmp_path)) == ("github.com", HOST)


# -- the adapter is genuinely optional (the four cases, D8) ----------------


def test_a_host_absent_from_the_map_is_unsupported(tmp_path: Path) -> None:
    """No `[forge_token_files]` entry means no forge reads the host at all."""
    config = VogtConfig(sqlite_synchronous="off")
    assert provider_for(REPO, config) is None
    reason = unsupported_reason(REPO, config)
    assert reason is not None and HOST in reason


def test_a_map_entry_pointing_at_a_missing_file_is_not_configured(
    tmp_path: Path,
) -> None:
    """Supported host, unusable token: "not collected", not "unsupported"."""
    config = VogtConfig(
        sqlite_synchronous="off", forge_token_files={HOST: tmp_path / "absent"}
    )
    assert provider_for(REPO, config) is None
    # The host is *supported* — the map names it — so the reason is None and
    # the caller reports "not configured" rather than "no forge reads this".
    assert unsupported_reason(REPO, config) is None


def test_a_map_entry_pointing_at_an_empty_file_is_not_configured(
    tmp_path: Path,
) -> None:
    empty = tmp_path / "empty"
    empty.write_text("   \n", encoding="utf-8")
    config = VogtConfig(sqlite_synchronous="off", forge_token_files={HOST: empty})
    assert provider_for(REPO, config) is None
    assert has_configured_forge(config) is False


def test_a_token_file_enables_the_forgejo_provider(tmp_path: Path) -> None:
    config = _config(tmp_path)
    assert isinstance(provider_for(REPO, config), ForgejoProvider)
    assert has_configured_forge(config) is True


# -- identity: protocol, keys, capabilities --------------------------------


def test_forgejo_provider_satisfies_the_protocol() -> None:
    """The structural check #172 promised Phase 5 would have to pass."""
    assert isinstance(_provider(), ForgeProvider)


def test_capabilities_are_declared_per_what_the_api_offers() -> None:
    caps = _provider().capabilities
    assert caps.hosts == (HOST,)
    assert caps.supports_since is True, "the issues endpoint takes `since`"
    assert caps.supports_posture is False, "no Dependabot-style posture surface"
    assert caps.supports_notifications is True, "per-repo notifications exist"
    assert caps.supports_webhooks is False, "deferred by name (D10), like GitHub"


def test_subject_keys_are_host_qualified_and_round_trip() -> None:
    """`forge:{host}/{owner}/{repo}#{n}` — the #171 scheme (D5)."""
    provider = _provider()
    key = provider.subject_key(_ref(), 42)
    assert key == f"forge:{HOST}/indexarr/Indexarr#42"
    assert provider.number_of(key) == 42
    # And it cannot collide with github.com's legacy `gh:` keys.
    assert not key.startswith("gh:")


def test_number_of_declines_a_key_that_names_no_number() -> None:
    provider = _provider()
    assert provider.number_of(f"posture:{HOST}/indexarr/Indexarr") is None
    assert provider.number_of(None) is None
    assert provider.number_of(f"forge:{HOST}/o/r#notanumber") is None


def test_the_urls_come_from_the_provider_not_a_hardcoded_string() -> None:
    provider = _provider()
    assert provider.clone_url(_ref()) == f"https://{HOST}/indexarr/Indexarr.git"
    assert provider.web_url(_ref()) == f"https://{HOST}/indexarr/Indexarr"
    assert provider.clone_token() == "x", "the clone authenticates as the token"


def test_the_auth_header_is_forgejos_token_scheme() -> None:
    """`Authorization: token …`, not GitHub's `Bearer …`."""
    seen: list[dict[str, str]] = []

    def transport(
        url: str, headers: dict[str, str], body: bytes = b"", method: str = "GET"
    ) -> tuple[int, bytes]:
        del url, body, method
        seen.append(dict(headers))
        return 200, b"[]"

    client = ForgejoClient(host=HOST, token="frg_secret", transport=transport)
    client.get("/repos/indexarr/Indexarr/labels")
    assert seen[0]["Authorization"] == "token frg_secret"
    assert f"https://{HOST}/api/v1" == client.api_root


# -- read surface: normalized models ---------------------------------------


def test_issues_normalise_ascending_and_drop_disguised_pulls() -> None:
    provider = _provider(
        {
            "/issues": [
                {
                    "number": 9,
                    "title": "newer",
                    "state": "open",
                    "labels": [{"name": "bug"}],
                    "user": {"login": "alice"},
                    "updated_at": "2026-08-10T00:00:00Z",
                    "html_url": f"{REPO}/issues/9",
                },
                {
                    "number": 7,
                    "title": "older",
                    "state": "closed",
                    "labels": [],
                    "user": {"username": "bob"},
                    "updated_at": "2026-08-01T00:00:00Z",
                    "closed_at": "2026-08-01T00:00:00Z",
                },
                {"number": 8, "title": "a PR in disguise", "pull_request": {}},
            ]
        }
    )
    issues = list(provider.issues_updated_since(_ref(), since=None))
    assert [i.number for i in issues] == [7, 9], "ascending by update, PRs dropped"
    assert issues[1].labels == ("bug",)
    assert issues[1].author == "alice"
    assert issues[0].author == "bob", "`username` satisfies when `login` is absent"
    assert issues[0].state == "closed" and issues[0].closed_at is not None


def test_issues_ask_for_issues_only_and_carry_since_upstream() -> None:
    urls: list[str] = []

    def transport(
        url: str, headers: dict[str, str], body: bytes = b"", method: str = "GET"
    ) -> tuple[int, bytes]:
        del headers, body, method
        urls.append(url)
        return 200, b"[]"

    provider = ForgejoProvider(ForgejoClient(host=HOST, token="x", transport=transport))
    list(provider.issues_updated_since(_ref(), since="2026-08-01T00:00:00Z"))
    assert "type=issues" in urls[0], "PRs are excluded server-side"
    assert "since=2026-08-01" in urls[0], "supports_since is real, not declared"
    assert "state=all" in urls[0], "closed history is part of the answer (#169)"


def test_pulls_filter_by_since_locally_and_yield_ascending() -> None:
    provider = _provider(
        {
            "/pulls": [
                {
                    "number": 3,
                    "title": "new",
                    "state": "open",
                    "draft": True,
                    "head": {"sha": "abc"},
                    "base": {"ref": "main"},
                    "updated_at": "2026-08-10T00:00:00Z",
                },
                {
                    "number": 1,
                    "title": "old",
                    "state": "closed",
                    "updated_at": "2026-07-01T00:00:00Z",
                },
            ]
        }
    )
    pulls = list(provider.pulls_updated_since(_ref(), since="2026-08-01T00:00:00Z"))
    assert [p.number for p in pulls] == [3], "the stale PR is filtered locally"
    assert pulls[0].draft is True and pulls[0].head == "abc" and pulls[0].base == "main"


def test_releases_and_labels_normalise() -> None:
    provider = _provider(
        {
            "/releases": [{"tag_name": "v1.2.0", "name": "1.2.0", "prerelease": True}],
            "/labels": [{"name": "bug", "color": "ee0701", "description": "broken"}],
        }
    )
    releases = list(provider.releases(_ref()))
    assert releases[0].tag == "v1.2.0" and releases[0].prerelease is True
    labels = list(provider.labels(_ref()))
    assert labels[0].name == "bug" and labels[0].color == "ee0701"


def test_action_tasks_become_generic_checks() -> None:
    """FR-O6 through Forgejo Actions: same `workflow_runs` envelope, but one
    `status` field carrying what GitHub splits into status and conclusion."""
    provider = _provider(
        {
            "/actions/tasks": {
                "total_count": 2,
                "workflow_runs": [
                    {
                        "name": "ci",
                        "head_sha": "abc123",
                        "head_branch": "main",
                        "status": "failure",
                        "run_number": 8,
                    },
                    {"name": "ci", "head_sha": "def456", "status": "running"},
                ],
            }
        }
    )
    checks = list(provider.checks(_ref()))
    assert checks[0].revision == "abc123" and checks[0].conclusion == "failure"
    assert checks[1].status == "running"
    assert checks[1].conclusion is None, "in flight honestly has no conclusion yet"


def test_an_install_without_actions_reports_no_checks_not_an_error() -> None:
    provider = _provider()  # /actions/tasks routes to 404
    assert list(provider.checks(_ref())) == []


def test_notifications_are_repo_scoped_and_honest_about_missing_fields() -> None:
    provider = _provider(
        {
            "/notifications": [
                {
                    "id": 55,
                    "unread": True,
                    "updated_at": "2026-08-10T00:00:00Z",
                    "subject": {
                        "title": "Crashes on empty segment",
                        "type": "Issue",
                        "url": f"https://{HOST}/api/v1/repos/indexarr/Indexarr/issues/9",
                    },
                }
            ]
        }
    )
    notes = list(provider.notifications(_ref()))
    assert notes[0].thread == "55" and notes[0].unread is True
    assert notes[0].source_url == f"https://{HOST}/indexarr/Indexarr/issues/9"
    # Forgejo's thread model has no reason and no per-thread read timestamp:
    # `None`, never a guess (FR-O11 applied to fields).
    assert notes[0].reason is None and notes[0].last_read_at is None


def test_the_posture_gap_reports_itself_as_not_supported(
    instance: AppContext, tmp_path: Path
) -> None:
    """FR-O11: the declared `supports_posture=False` becomes a receipt, not a
    silent zero and not three fabricated facts."""
    project_root = tmp_path / "indexarr"
    project_root.mkdir()
    from datetime import UTC, datetime

    now = datetime(2026, 8, 21, tzinfo=UTC)
    project = Project(
        id="prj_forgejo",
        slug="indexarr",
        name="indexarr",
        root_path=str(project_root),
        repo_url=REPO,
        created_at=now,
        updated_at=now,
    )
    config = _config(tmp_path, data_dir=instance.config.data_dir)
    ctx = CollectorContext(config=config, clock=instance.clock)
    collector = ForgePostureCollector(transport=_fake_transport({}))
    found = list(collector.collect(ctx, project))
    assert len(found) == 1, "one receipt, no posture observation"
    assert found[0].kind == "forge.sync"
    assert found[0].payload["supported"] is False
    assert "posture" in str(found[0].payload["detail"])


# -- list_repos: the import picker's enumeration (#180) --------------------


def test_list_repos_enumerates_what_the_credential_sees() -> None:
    provider = _provider(
        {
            "/user/repos": [
                {
                    "name": "Indexarr",
                    "owner": {"login": "indexarr"},
                    "private": True,
                    "default_branch": "main",
                    "html_url": f"https://{HOST}/indexarr/Indexarr",
                },
                {"name": "orphan"},  # no owner: skipped, not invented
            ]
        }
    )
    repos = list(provider.list_repos())
    assert [r.slug for r in repos] == ["indexarr/Indexarr"]
    assert repos[0].visibility == "private"
    assert repos[0].web_url == f"https://{HOST}/indexarr/Indexarr"


# -- write surface: append-only, and it reports the ledger's shape ----------


class _Recorder:
    """A transport that records every request and answers writes with an id."""

    def __init__(self, *, write_status: int = 200) -> None:
        self.write_status = write_status
        self.requests: list[tuple[str, str]] = []
        self.bodies: list[dict[str, Any]] = []

    def __call__(
        self, url: str, headers: dict[str, str], body: bytes = b"", method: str = "GET"
    ) -> tuple[int, bytes]:
        del headers
        self.requests.append((method, url))
        if body:
            self.bodies.append(json.loads(body.decode("utf-8")))
        if method != "GET" and self.write_status >= 400:
            return self.write_status, b"{}"
        return 200, json.dumps(
            {
                "number": 12,
                "html_url": f"{REPO}/issues/12",
                "name": "widgets",
                "owner": {"login": "indexarr"},
                "private": True,
                "default_branch": "main",
            }
        ).encode("utf-8")


def _writing_provider(**kwargs: Any) -> tuple[ForgejoProvider, _Recorder]:
    recorder = _Recorder(**kwargs)
    return (
        ForgejoProvider(ForgejoClient(host=HOST, token="x", transport=recorder)),
        recorder,
    )


def test_comment_posts_and_reports_the_host_qualified_key() -> None:
    provider, recorder = _writing_provider()
    result = provider.comment(_ref(), 5, "hello")
    assert result.outcome == "succeeded"
    assert result.subject_key == f"forge:{HOST}/indexarr/Indexarr#12"
    assert recorder.bodies == [{"body": "hello"}]
    assert all(method == "POST" for method, _ in recorder.requests)


def test_create_issue_appends_twice_because_labels_ride_their_own_endpoint() -> None:
    """Forgejo's create-issue option takes label ids; names go to the labels
    endpoint after the create. Both verbs are appends (FR-B4)."""
    provider, recorder = _writing_provider()
    result = provider.create_issue(_ref(), title="t", body="b", labels=["bug"])
    assert result.outcome == "succeeded"
    methods_and_urls = recorder.requests
    assert [m for m, _ in methods_and_urls] == ["POST", "POST"]
    assert methods_and_urls[1][1].endswith("/issues/12/labels")
    assert recorder.bodies == [{"title": "t", "body": "b"}, {"labels": ["bug"]}]


def test_add_labels_appends_and_never_replaces() -> None:
    provider, recorder = _writing_provider()
    provider.add_labels(_ref(), 5, ["triage"])
    methods = {method for method, _ in recorder.requests}
    assert methods == {"POST"}, "POST appends; no PUT (replace), no DELETE (FR-B4)"


def test_set_state_toggles_and_refuses_a_non_state() -> None:
    provider, recorder = _writing_provider()
    assert provider.set_state(_ref(), 5, "closed").outcome == "succeeded"
    assert recorder.requests[-1][0] == "PATCH"
    assert recorder.bodies[-1] == {"state": "closed"}
    with pytest.raises(ValueError, match="not a state"):
        provider.set_state(_ref(), 5, "deleted")


def test_an_upstream_refusal_is_a_failed_result_not_an_exception() -> None:
    """Never fatal to the declared write: the ledger records the miss."""
    provider, _ = _writing_provider(write_status=500)
    result = provider.comment(_ref(), 5, "hello")
    assert result.outcome == "failed"
    assert result.detail is not None and "500" in result.detail


def test_create_repo_lands_under_the_credential() -> None:
    provider, recorder = _writing_provider()
    repo = provider.create_repo("widgets", private=True, description="d")
    assert repo.slug == "indexarr/widgets"
    assert repo.visibility == "private"
    assert recorder.requests == [("POST", f"https://{HOST}/api/v1/user/repos")]
    assert recorder.bodies[0]["auto_init"] is False, "the first push must be clean"


def test_an_existing_name_is_a_typed_refusal_never_a_clobber() -> None:
    """Forgejo answers a duplicate with 409; that is `RemoteRepoExists` (#182)."""
    provider, _ = _writing_provider(write_status=409)
    with pytest.raises(RemoteRepoExists, match="widgets"):
        provider.create_repo("widgets", private=True)


# -- end to end: #169's regression shape, against a Forgejo host ------------


class FakeForgejo:
    """A Forgejo whose issue/PR state a test can change between sweeps."""

    def __init__(self) -> None:
        self.issues: list[dict[str, Any]] = []
        self.pulls: list[dict[str, Any]] = []
        self.requests: list[tuple[str, str]] = []

    def __call__(
        self, url: str, headers: dict[str, str], body: bytes = b"", method: str = "GET"
    ) -> tuple[int, bytes]:
        del headers, body
        self.requests.append((method, url))
        if "/issues" in url and "/comments" not in url:
            return 200, json.dumps(self.issues).encode("utf-8")
        if "/pulls" in url:
            return 200, json.dumps(self.pulls).encode("utf-8")
        if "/actions/tasks" in url:
            return 200, json.dumps({"workflow_runs": []}).encode("utf-8")
        return 200, b"[]"

    @property
    def mutations(self) -> list[tuple[str, str]]:
        return [(m, u) for m, u in self.requests if m != "GET"]


@pytest.fixture
def forgejo_instance(
    instance: AppContext, tmp_path: Path
) -> tuple[AppContext, FakeForgejo]:
    """A registered Forgejo-hosted project on an instance that reads its host.

    Configured the deployed way — a host under `[forge_token_files]` and a
    token in a file (D8) — with only the transport substituted. No factory is
    monkeypatched: the same resolution a production sweep walks is the one
    under test, which is the point of Phase 5.
    """
    fake = FakeForgejo()
    token = tmp_path / "forgejo-token"
    token.write_text("frg_fake\n", encoding="utf-8")
    config = instance.config.model_copy(update={"forge_token_files": {HOST: token}})
    ctx = dataclasses.replace(instance, config=config, forge_transport=fake)
    root = tmp_path / "indexarr"
    root.mkdir()
    register_project(
        ctx,
        RegisterProjectParams(
            name="indexarr", root_path=str(root), repo_url=f"{REPO}.git", reason=WHY
        ),
    )
    return ctx, fake


def _backlog_numbers(ctx: AppContext) -> list[int]:
    result = backlog(ctx, BacklogParams(limit=100))
    numbers: list[int] = []
    for item in result.items:
        if item.origin == "observed" and item.title.startswith("#"):
            numbers.append(int(item.title.split()[0].lstrip("#")))
    return numbers


def test_forgejo_issues_and_prs_reach_the_backlog(
    forgejo_instance: tuple[AppContext, FakeForgejo],
) -> None:
    """The inversion of WI-9: the Forgejo project's history is *read*."""
    ctx, fake = forgejo_instance
    fake.issues = [
        {
            "number": 9,
            "title": "Crashes on empty segment",
            "state": "open",
            "labels": [{"name": "bug"}],
            "updated_at": "2026-08-01T00:00:00Z",
        }
    ]
    fake.pulls = [
        {
            "number": 100,
            "title": "wip",
            "state": "open",
            "updated_at": "2026-08-02T00:00:00Z",
        }
    ]
    sweep(ctx, SweepParams(reason=WHY))
    numbers = _backlog_numbers(ctx)
    assert 9 in numbers, "a Forgejo issue is backlog"
    assert 100 in numbers, "a Forgejo PR is backlog"
    assert fake.mutations == [], "and reading asked nobody anything (FR-B3)"


def test_forgejo_sync_is_incremental_via_watermarks(
    forgejo_instance: tuple[AppContext, FakeForgejo],
) -> None:
    ctx, fake = forgejo_instance
    fake.issues = [{"number": 1, "state": "open", "updated_at": "2026-08-03T00:00:00Z"}]
    sweep(ctx, SweepParams(reason=WHY))
    with ctx.declared.read() as view:
        project = view.project_by_slug("indexarr")
    assert project is not None
    watermark = ctx.observed.get_watermark(
        collector="forge-issues", project_id=project.id
    )
    assert watermark is not None and "2026-08-03" in watermark

    sweep(ctx, SweepParams(reason=WHY))
    issue_reads = [
        url for method, url in fake.requests if method == "GET" and "/issues?" in url
    ]
    assert "since=" in issue_reads[-1], "the second sweep asks what moved since"


def test_a_closure_upstream_leaves_the_backlog(
    forgejo_instance: tuple[AppContext, FakeForgejo],
) -> None:
    """#169's exit criterion, re-run through the second provider: observe an
    issue open, close it upstream, sweep, and watch the ranked view heal."""
    ctx, fake = forgejo_instance
    fake.issues = [
        {
            "number": 42,
            "title": "Segments never retry",
            "state": "open",
            "labels": [{"name": "bug"}],
            "updated_at": "2026-08-01T00:00:00Z",
        }
    ]
    sweep(ctx, SweepParams(reason=WHY))
    assert 42 in _backlog_numbers(ctx)

    fake.issues = [
        {
            "number": 42,
            "title": "Segments never retry",
            "state": "closed",
            "labels": [{"name": "bug"}],
            "updated_at": "2026-08-05T00:00:00Z",
            "closed_at": "2026-08-05T00:00:00Z",
        }
    ]
    sweep(ctx, SweepParams(reason=WHY))
    assert 42 not in _backlog_numbers(ctx), "a closed issue is not backlog"

    latest = ctx.observed.latest_by_subject(f"forge:{HOST}/indexarr/Indexarr#42")
    assert latest is not None, "and the key scheme is the provider's own (#171)"
    assert lifecycle_of(latest) == LIFECYCLE_CLOSED
