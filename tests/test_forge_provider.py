"""The forge-provider seam (#172, D2/D5/D8).

Phase 1 changes no observable behavior — those guarantees are asserted by the
existing GitHub and forge-module suites, which pass unchanged. What is *new*
here is the seam itself: the provider contract, its GitHub implementation, the
host→provider registry that replaced `repo_of()` at every call site, and the
`forge_token_files` config field that generalises `github_token_file`.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest

from vogt.adapters.forge import (
    ForgeProvider,
    GitHubProvider,
    RepoRef,
    provider_for,
    supported_hosts,
    token_file_for,
    unsupported_reason,
)
from vogt.adapters.github.client import GitHubClient
from vogt.config import VogtConfig

GITHUB = "https://github.com/TheDancingDeveloper-org/vogt"
GITHUB_SSH = "git@github.com:TheDancingDeveloper-org/vogt.git"
FORGEJO = "https://repo.indexarr.net/indexarr/Indexarr.git"


def _fake_transport(routes: dict[str, Any]) -> Any:
    def transport(
        url: str, headers: dict[str, str], body: bytes = b"", method: str = "GET"
    ) -> tuple[int, bytes]:
        del headers, body, method
        for fragment, payload in routes.items():
            if fragment in url:
                return 200, json.dumps(payload).encode("utf-8")
        return 200, b"[]"

    return transport


def _provider(routes: dict[str, Any] | None = None) -> GitHubProvider:
    return GitHubProvider(
        GitHubClient(token="x", transport=_fake_transport(routes or {}))
    )


# -- host selection: the table that replaced `repo_of` --------------------


@pytest.mark.parametrize(
    ("repo_url", "supported"),
    [
        (GITHUB, True),
        (GITHUB_SSH, True),
        ("https://github.com/owner/repo.git", True),
        (FORGEJO, False),  # WI-9's host: a real repository the adapter cannot read
        ("https://gitlab.com/group/project", False),
        ("https://example.com", False),
        (None, False),
        ("", False),
    ],
)
def test_host_selection_table(repo_url: str | None, supported: bool) -> None:
    """One provider recognises github.com and only github.com (D2)."""
    provider = _provider()
    ref = provider.parse(repo_url)
    assert (ref is not None) is supported
    # `unsupported_reason` is the mirror image: `None` exactly when readable.
    assert (unsupported_reason(repo_url) is None) is supported


def test_the_forgejo_host_names_itself_and_what_is_readable() -> None:
    """WI-9 again, at the seam: the reason carries its own cause."""
    reason = unsupported_reason(FORGEJO)
    assert reason is not None
    assert "repo.indexarr.net" in reason
    assert "github.com" in reason


def test_a_missing_url_is_not_an_unreadable_host() -> None:
    reason = unsupported_reason(None)
    assert reason is not None and "no repository URL" in reason


def test_supported_hosts_advertises_github_only_in_v1() -> None:
    assert supported_hosts() == ("github.com",)


# -- identity: parse, key scheme, capabilities ----------------------------


def test_parse_normalises_every_github_url_shape() -> None:
    provider = _provider()
    for url in (GITHUB, GITHUB_SSH, "https://github.com/owner/repo/"):
        ref = provider.parse(url)
        assert ref is not None
        assert ref.host == "github.com"
        assert ref.owner and ref.repo


def test_subject_key_and_number_round_trip() -> None:
    """The provider both builds `gh:owner/repo#n` and reads the n back (D5)."""
    provider = _provider()
    ref = RepoRef(host="github.com", owner="acme", repo="widgets")
    key = provider.subject_key(ref, 42)
    assert key == "gh:acme/widgets#42"
    assert provider.number_of(key) == 42


def test_number_of_declines_a_key_that_names_no_number() -> None:
    provider = _provider()
    assert provider.number_of("posture:acme/widgets") is None
    assert provider.number_of(None) is None
    assert provider.number_of("gh:acme/widgets#notanumber") is None


def test_capabilities_are_declared_not_discovered() -> None:
    caps = _provider().capabilities
    assert caps.hosts == ("github.com",)
    assert caps.supports_since is True
    assert caps.supports_posture is True
    assert caps.supports_notifications is True
    # Deferred by name in the v1 ceiling (D10).
    assert caps.supports_webhooks is False


def test_github_provider_satisfies_the_protocol() -> None:
    """The structural check Phase 5's Forgejo provider will have to pass too."""
    assert isinstance(_provider(), ForgeProvider)


def test_the_urls_come_from_the_provider_not_a_hardcoded_string() -> None:
    provider = _provider()
    ref = RepoRef(host="github.com", owner="acme", repo="widgets")
    assert provider.clone_url(ref) == "https://github.com/acme/widgets.git"
    assert provider.web_url(ref) == "https://github.com/acme/widgets"


# -- read surface: normalized models (wired into collectors in Phase 2) ---


def test_issues_updated_since_normalises_and_drops_pull_requests() -> None:
    provider = _provider(
        {
            "/issues": [
                {
                    "number": 7,
                    "title": "a real issue",
                    "state": "open",
                    "labels": [{"name": "bug"}],
                    "user": {"login": "alice"},
                    "html_url": f"{GITHUB}/issues/7",
                },
                {"number": 8, "pull_request": {}, "title": "a PR in disguise"},
            ]
        }
    )
    ref = provider.parse(GITHUB)
    assert ref is not None
    issues = list(provider.issues_updated_since(ref, since=None))
    assert [i.number for i in issues] == [7]
    assert issues[0].labels == ("bug",)
    assert issues[0].author == "alice"


def test_pulls_and_releases_and_checks_normalise() -> None:
    provider = _provider(
        {
            "/pulls": [
                {"number": 3, "title": "wip", "state": "open", "draft": True}
            ],
            "/releases": [{"tag_name": "v1.2.0", "name": "1.2.0"}],
            "/actions/runs": {
                "workflow_runs": [
                    {"head_sha": "abc123", "name": "ci", "conclusion": "success"}
                ]
            },
        }
    )
    ref = provider.parse(GITHUB)
    assert ref is not None
    pulls = list(provider.pulls_updated_since(ref, since=None))
    assert pulls[0].number == 3 and pulls[0].draft is True
    releases = list(provider.releases(ref))
    assert releases[0].tag == "v1.2.0"
    checks = list(provider.checks(ref))
    assert checks[0].revision == "abc123" and checks[0].conclusion == "success"


def test_a_read_against_an_empty_repository_is_empty_not_an_error() -> None:
    provider = _provider()  # every route returns []
    ref = provider.parse(GITHUB)
    assert ref is not None
    assert list(provider.issues_updated_since(ref, since=None)) == []
    assert list(provider.checks(ref)) == []


# -- write surface: append-only, and it delegates to the one mutator -------


class _Recorder:
    """A transport that records every request and answers writes with an id."""

    def __init__(self) -> None:
        self.requests: list[tuple[str, str]] = []
        self.bodies: list[dict[str, Any]] = []

    def __call__(
        self, url: str, headers: dict[str, str], body: bytes = b"", method: str = "GET"
    ) -> tuple[int, bytes]:
        del headers
        self.requests.append((method, url))
        if body:
            self.bodies.append(json.loads(body.decode("utf-8")))
        return 200, json.dumps(
            {"number": 12, "html_url": f"{GITHUB}/issues/12"}
        ).encode("utf-8")


def _writing_provider() -> tuple[GitHubProvider, _Recorder]:
    recorder = _Recorder()
    return GitHubProvider(GitHubClient(token="x", transport=recorder)), recorder


def test_comment_delegates_and_reports_the_provider_key() -> None:
    provider, recorder = _writing_provider()
    ref = RepoRef(host="github.com", owner="acme", repo="widgets")
    result = provider.comment(ref, 5, "hello")
    assert result.outcome == "succeeded"
    assert result.subject_key == "gh:acme/widgets#12"
    assert recorder.bodies == [{"body": "hello"}]
    assert all(method != "GET" for method, _ in recorder.requests)


def test_create_issue_and_add_labels_are_additive() -> None:
    provider, recorder = _writing_provider()
    ref = RepoRef(host="github.com", owner="acme", repo="widgets")
    provider.create_issue(ref, title="t", body="b", labels=["bug"])
    provider.add_labels(ref, 5, ["triage"])
    methods = {method for method, _ in recorder.requests}
    # POST appends; there is no PUT (replace) and no DELETE anywhere (FR-B4).
    assert methods == {"POST"}


def test_set_state_toggles_and_refuses_a_non_state() -> None:
    provider, recorder = _writing_provider()
    ref = RepoRef(host="github.com", owner="acme", repo="widgets")
    assert provider.set_state(ref, 5, "closed").outcome == "succeeded"
    assert recorder.bodies[-1] == {"state": "closed"}
    with pytest.raises(ValueError, match="not a state"):
        provider.set_state(ref, 5, "deleted")


# -- provider_for: host + token resolution together (D8) ------------------


def _config(**overrides: Any) -> VogtConfig:
    return VogtConfig(sqlite_synchronous="off", **overrides)


def test_provider_for_an_unsupported_host_is_none_even_with_a_token(
    tmp_path: Path,
) -> None:
    token = tmp_path / "token"
    token.write_text("ghp_x\n", encoding="utf-8")
    config = _config(github_token_file=token)
    assert provider_for(FORGEJO, config) is None


def test_provider_for_a_supported_host_without_a_token_is_none() -> None:
    """Absent ⇒ not collected: no token means no provider registered (D8)."""
    assert provider_for(GITHUB, _config()) is None


def test_provider_for_a_supported_host_with_a_token_resolves(tmp_path: Path) -> None:
    token = tmp_path / "token"
    token.write_text("ghp_x\n", encoding="utf-8")
    provider = provider_for(GITHUB, _config(github_token_file=token))
    assert isinstance(provider, GitHubProvider)


# -- forge_token_files: the four adapter-optional cases (D8) ---------------


def test_absent_from_the_map_falls_back_to_the_github_alias(tmp_path: Path) -> None:
    """`github_token_file` stays the name for github.com with no map entry."""
    token = tmp_path / "gh"
    token.write_text("ghp_x\n", encoding="utf-8")
    config = _config(github_token_file=token)
    assert token_file_for(config, "github.com") == token
    assert provider_for(GITHUB, config) is not None


def test_a_map_entry_pointing_at_a_missing_file_is_not_configured(
    tmp_path: Path,
) -> None:
    config = _config(forge_token_files={"github.com": tmp_path / "absent"})
    assert token_file_for(config, "github.com") == tmp_path / "absent"
    assert provider_for(GITHUB, config) is None


def test_a_map_entry_pointing_at_an_empty_file_is_not_configured(
    tmp_path: Path,
) -> None:
    token = tmp_path / "empty"
    token.write_text("   \n", encoding="utf-8")
    config = _config(forge_token_files={"github.com": token})
    assert provider_for(GITHUB, config) is None


def test_a_map_entry_present_wins_over_the_alias(tmp_path: Path) -> None:
    alias = tmp_path / "alias"
    alias.write_text("ghp_alias\n", encoding="utf-8")
    mapped = tmp_path / "mapped"
    mapped.write_text("ghp_mapped\n", encoding="utf-8")
    config = _config(
        github_token_file=alias, forge_token_files={"github.com": mapped}
    )
    assert token_file_for(config, "github.com") == mapped
    assert provider_for(GITHUB, config) is not None
