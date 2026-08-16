"""The optional GitHub adapter (FR-O5a, NFR-PO1, NFR-PO2).

Every test here injects a transport. Nothing in this suite touches the
network — a test that needs GitHub to be reachable is a test that fails for
reasons unrelated to the change.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest

from vogt.adapters.github import GitHubClient, GitHubUnavailable, github_collectors
from vogt.adapters.github.client import (
    NO_CONTENT,
    Transport,
    repo_of,
)
from vogt.adapters.github.collectors import (
    GitHubActionsCollector,
    GitHubIssueCollector,
    GitHubPullRequestCollector,
    GitHubReleaseCollector,
)
from vogt.adapters.github.posture import GitHubPostureCollector
from vogt.application.context import AppContext
from vogt.application.models import (
    BacklogParams,
    BugsParams,
    ObservationsParams,
    RegisterProjectParams,
    SweepParams,
)
from vogt.application.services import (
    backlog,
    bugs,
    observations,
    register_project,
    sweep,
)
from vogt.collectors import CollectorContext
from vogt.collectors.registry import CollectorRegistry
from vogt.core.entities import Project

WHY = "github adapter test"


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


def _project() -> Project:
    from datetime import UTC, datetime

    now = datetime(2026, 8, 12, tzinfo=UTC)
    return Project(
        id="prj_1",
        slug="rustnzb",
        name="rustnzb",
        root_path="/srv/rustnzb",
        repo_url="https://github.com/TheDancingDeveloper-org/rustnzb",
        created_at=now,
        updated_at=now,
    )


@pytest.fixture
def ctx(instance: AppContext) -> CollectorContext:
    return CollectorContext(config=instance.config, clock=instance.clock)


def _toggle_transport() -> Transport:
    """GitHub as it actually answers the repository toggles.

    204 with an empty body when the setting is on, 404 when it is off. Every
    fake transport in this file returned `json.dumps(...)` for everything,
    which is why a year of tests passed against a client that could not read a
    204 at all.
    """

    def transport(
        url: str, headers: dict[str, str], body: bytes = b"", method: str = "GET"
    ) -> tuple[int, bytes]:
        del headers, body, method
        if "vulnerability-alerts" in url:
            return 204, b""
        if "automated-security-fixes" in url:
            return 404, b""
        return 404, b"{}"

    return transport


def test_a_204_is_a_setting_that_is_on_not_a_parse_error(
    ctx: CollectorContext,
) -> None:
    """WI-4: `JSONDecodeError: Expecting value: line 1 column 1 (char 0)`.

    The empty body of a 204 was handed to a JSON parser, so `gh-posture`
    failed outright on any repository whose vulnerability alerts were
    switched on — and failed the whole collector for that project, not one
    field of it. It was reported as `partial` for `cadastre` and `rustnzb`
    for as long as anyone had been looking.
    """
    client = GitHubClient(token="x", transport=_toggle_transport())
    found = list(GitHubPostureCollector(client).collect(ctx, _project()))

    assert len(found) == 1, "one posture observation, not an exception"
    payload = found[0].payload
    assert payload["vulnerability_alerts"] is True, "204 means the setting is on"
    assert payload["automated_security_fixes"] is False, "404 means it is off"


def test_no_content_is_not_the_same_answer_as_not_found() -> None:
    """The distinction the whole fix rests on.

    Flattening 204 into `None` would have stopped the crash and reported
    every enabled security toggle as disabled, which is the more expensive
    wrong answer of the two.
    """
    client = GitHubClient(token="x", transport=_toggle_transport())
    assert client.get("/repos/o/r/vulnerability-alerts") is NO_CONTENT
    assert client.get("/repos/o/r/automated-security-fixes") is None
    assert not NO_CONTENT, "and it is falsy, so `if not payload` still reads right"


# -- the adapter is genuinely optional ------------------------------------


def test_no_token_file_means_no_collectors(instance: AppContext) -> None:
    """NFR-PO1: an absent adapter is the ordinary case, not a failure."""
    assert github_collectors(instance.config) == []


def test_a_missing_token_file_is_not_an_error(
    instance: AppContext, tmp_path: Path
) -> None:
    config = instance.config.model_copy(
        update={"github_token_file": tmp_path / "absent"}
    )
    assert github_collectors(config) == []


def test_an_empty_token_file_is_not_configured(
    instance: AppContext, tmp_path: Path
) -> None:
    token = tmp_path / "token"
    token.write_text("   \n", encoding="utf-8")
    config = instance.config.model_copy(update={"github_token_file": token})
    assert github_collectors(config) == []


def test_a_token_file_enables_the_forge_collectors(
    instance: AppContext, tmp_path: Path
) -> None:
    token = tmp_path / "token"
    token.write_text("ghp_example\n", encoding="utf-8")
    config = instance.config.model_copy(update={"github_token_file": token})
    assert {c.name for c in github_collectors(config)} == {
        "gh-issues",
        "gh-prs",
        "gh-actions",
        "gh-releases",
        "gh-posture",
        "gh-notifications",
    }


def test_the_token_is_read_from_the_file_not_an_argument(tmp_path: Path) -> None:
    """FR-S7: a token in argv shows up in `ps` and in shell history."""
    token = tmp_path / "token"
    token.write_text("ghp_secret\n", encoding="utf-8")
    client = GitHubClient.from_token_file(token)
    assert client is not None
    assert client.token == "ghp_secret"


def test_forge_collectors_declare_that_they_need_the_network(
    instance: AppContext, tmp_path: Path
) -> None:
    """NFR-PO2: this is what makes the forge-less test layer selectable."""
    token = tmp_path / "token"
    token.write_text("ghp_example\n", encoding="utf-8")
    config = instance.config.model_copy(update={"github_token_file": token})

    collectors = github_collectors(config)
    assert collectors, "the adapter is configured"
    for collector in collectors:
        assert collector.requires_network is True, collector.name

    registry = CollectorRegistry()
    registry.add(GitHubIssueCollector(GitHubClient(token="x")))
    assert "gh-issues" not in {c.name for c in registry.offline()}


# -- what the collectors produce ------------------------------------------


def test_issues_become_observations(ctx: CollectorContext) -> None:
    client = GitHubClient(
        token="x",
        transport=_fake_transport(
            {
                "/issues": [
                    {
                        "number": 42,
                        "title": "Segments never retry",
                        "state": "open",
                        "labels": [{"name": "bug"}, {"name": "p1"}],
                        "user": {"login": "someone"},
                        "assignees": [],
                        "comments": 3,
                        "updated_at": "2026-08-01T00:00:00Z",
                        "html_url": "https://github.com/o/r/issues/42",
                    }
                ]
            }
        ),
    )
    found = list(GitHubIssueCollector(client).collect(ctx, _project()))
    assert len(found) == 1
    assert found[0].subject_key == "gh:TheDancingDeveloper-org/rustnzb#42"
    assert found[0].promoted is True
    assert found[0].payload["labels"] == ["bug", "p1"]


def test_pull_requests_are_not_double_counted_as_issues(
    ctx: CollectorContext,
) -> None:
    """GitHub returns PRs from the issues endpoint; they have their own kind."""
    client = GitHubClient(
        token="x",
        transport=_fake_transport(
            {
                "/issues": [
                    {"number": 1, "title": "An issue", "labels": []},
                    {"number": 2, "title": "A PR", "pull_request": {}, "labels": []},
                ]
            }
        ),
    )
    found = list(GitHubIssueCollector(client).collect(ctx, _project()))
    assert [f.payload["number"] for f in found] == [1]


def test_actions_runs_become_generic_checks(ctx: CollectorContext) -> None:
    """FR-O6: CI is a per-revision check; Actions is one producer of those."""
    client = GitHubClient(
        token="x",
        transport=_fake_transport(
            {
                "/actions/runs": {
                    "workflow_runs": [
                        {
                            "name": "ci",
                            "head_sha": "abc123",
                            "status": "completed",
                            "conclusion": "failure",
                            "head_branch": "main",
                        }
                    ]
                }
            }
        ),
    )
    found = list(GitHubActionsCollector(client).collect(ctx, _project()))
    assert found[0].kind == "ci.check"
    assert found[0].subject_key.startswith("ci:TheDancingDeveloper-org/rustnzb@abc123")
    assert found[0].payload["revision"] == "abc123"
    assert found[0].payload["conclusion"] == "failure"


def test_releases_become_observed_versions(ctx: CollectorContext) -> None:
    client = GitHubClient(
        token="x",
        transport=_fake_transport(
            {"/releases": [{"tag_name": "v2.0.0", "name": "2.0"}]}
        ),
    )
    found = list(GitHubReleaseCollector(client).collect(ctx, _project()))
    assert found[0].payload["tag"] == "v2.0.0"


def test_a_project_that_is_not_on_github_is_skipped(ctx: CollectorContext) -> None:
    """Not an error: a project without a forge is a supported project."""
    local = _project().model_copy(update={"repo_url": None})
    client = GitHubClient(token="x", transport=_fake_transport({}))
    assert list(GitHubIssueCollector(client).collect(ctx, local)) == []


def test_a_404_is_absence_not_an_exception(ctx: CollectorContext) -> None:
    client = GitHubClient(token="x", transport=_fake_transport({}))
    assert list(GitHubIssueCollector(client).collect(ctx, _project())) == []


def test_rate_limiting_raises_so_the_sweep_records_it(ctx: CollectorContext) -> None:
    client = GitHubClient(
        token="x", transport=_fake_transport({"/issues": {}}, status=403)
    )
    with pytest.raises(GitHubUnavailable, match="rate limited"):
        list(GitHubIssueCollector(client).collect(ctx, _project()))


@pytest.mark.parametrize(
    ("url", "expected"),
    [
        ("https://github.com/o/r", ("o", "r")),
        ("https://github.com/o/r.git", ("o", "r")),
        ("git@github.com:o/r.git", ("o", "r")),
        ("git+ssh://github.com/o/r", ("o", "r")),
        ("https://gitlab.com/o/r", None),
        (None, None),
        ("https://github.com/o", None),
    ],
)
def test_repo_urls_are_recognised_in_the_forms_people_write_them(
    url: str | None, expected: tuple[str, str] | None
) -> None:
    assert repo_of(url) == expected


# -- end to end, with the adapter configured ------------------------------


def test_forge_issues_reach_the_bug_view(
    instance: AppContext, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The MVP claim: the global bug view shows GitHub issues too."""
    project_root = tmp_path / "rustnzb"
    project_root.mkdir()
    register_project(
        instance,
        RegisterProjectParams(
            name="rustnzb",
            root_path=str(project_root),
            repo_url="https://github.com/TheDancingDeveloper-org/rustnzb",
            reason=WHY,
        ),
    )

    transport = _fake_transport(
        {
            "/issues": [
                {
                    "number": 7,
                    "title": "Crashes on empty segment",
                    "state": "open",
                    "labels": [{"name": "bug"}],
                }
            ],
            "/pulls": [],
            "/actions/runs": {"workflow_runs": []},
            "/releases": [],
        }
    )

    from vogt.application.services import collect as collect_service

    def registry_with_github(ctx: AppContext) -> CollectorRegistry:
        registry = CollectorRegistry()
        client = GitHubClient(token="x", transport=transport)
        for collector in (
            GitHubIssueCollector(client),
            GitHubPullRequestCollector(client),
            GitHubActionsCollector(client),
            GitHubReleaseCollector(client),
        ):
            registry.add(collector)
        return registry

    monkeypatch.setattr(collect_service, "collector_registry", registry_with_github)

    sweep(instance, SweepParams(reason=WHY))

    found = bugs(instance, BugsParams(limit=50))
    titles = [entry.title for entry in found.items]
    assert any("Crashes on empty segment" in title for title in titles)
    assert found.items[0].origin == "observed"
    assert found.items[0].source_url is None or isinstance(
        found.items[0].source_url, str
    )


def test_the_whole_suite_runs_forge_less(instance: AppContext, tmp_path: Path) -> None:
    """NFR-PO1/PO2: unplug the network and everything except forge data works.

    This is the second half of the M2 demo, kept as a test because "it still
    works without GitHub" is a claim that rots silently.
    """
    project_root = tmp_path / "local-only"
    project_root.mkdir()
    (project_root / "notes.md").write_text(
        "TODO(vogt): still visible with no forge\n", encoding="utf-8"
    )
    register_project(
        instance,
        RegisterProjectParams(
            name="Local Only",
            root_path=str(project_root),
            repo_url="https://github.com/o/r",
            reason=WHY,
        ),
    )

    # No token file is configured, so no forge collector exists at all.
    assert github_collectors(instance.config) == []

    result = sweep(instance, SweepParams(reason=WHY))
    assert {report.collector for report in result.reports} == {
        "git-local",
        "source-markers",
        "dep-refs",
    }
    assert all(report.outcome == "ok" for report in result.reports)

    ranked = backlog(instance, BacklogParams(limit=50))
    assert any(entry.origin == "observed" for entry in ranked.items)

    forge = observations(instance, ObservationsParams(kind="forge.issue"))
    assert forge.observations == [], "not collected, and not pretended otherwise"
