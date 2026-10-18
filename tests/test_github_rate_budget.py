"""WI-1838: the forge sweep must not spend the account's shared GitHub budget.

Three mechanisms, each tested here through the `Transport` seam:

- conditional requests (ETag / If-None-Match), so an unchanged resource is a
  free 304 replayed from cache;
- the rate budget, read from `X-RateLimit-*` headers, which pauses reads
  below a floor until the window resets;
- posture read once per interval per repository, from the git tree rather
  than eight `/contents/` probes.
"""

from __future__ import annotations

import json
from datetime import UTC, datetime, timedelta
from typing import Any

import pytest

from vogt.adapters.forge.collectors import ForgePostureCollector
from vogt.adapters.forge.github import GitHubProvider
from vogt.adapters.forge.models import RepoRef
from vogt.adapters.github.client import (
    RATE_BUDGET,
    GitHubClient,
    GitHubRateLimited,
    configure_rate_limit_floor,
)
from vogt.collectors.base import CollectorContext
from vogt.config import VogtConfig
from vogt.core.entities import Project

RESET = 2_000_000_000.0  # far future, so the budget stays blocked


class _Recorder:
    """A transport that answers from a route table and records each call."""

    def __init__(self, answer: Any) -> None:
        self.answer = answer
        self.calls: list[tuple[str, dict[str, str]]] = []

    def __call__(
        self, url: str, headers: dict[str, str], body: bytes = b"", method: str = "GET"
    ) -> Any:
        del body, method
        self.calls.append((url, dict(headers)))
        return self.answer(url, headers)


def _headers(remaining: int, **extra: str) -> dict[str, str]:
    return {
        "X-RateLimit-Remaining": str(remaining),
        "X-RateLimit-Reset": str(int(RESET)),
        "X-RateLimit-Resource": "core",
        **extra,
    }


# -- ETag / If-None-Match ---------------------------------------------------


def test_second_read_sends_if_none_match_and_replays_a_304() -> None:
    body = json.dumps([{"name": "bug"}]).encode()

    def answer(url: str, headers: dict[str, str]) -> Any:
        if headers.get("If-None-Match") == '"v1"':
            return 304, b"", _headers(4000, ETag='"v1"')
        return 200, body, _headers(4000, ETag='"v1"')

    transport = _Recorder(answer)
    client = GitHubClient(token="t", transport=transport)

    first = client.get("/repos/o/r/labels")
    second = client.get("/repos/o/r/labels")

    assert first == second == [{"name": "bug"}]
    assert "If-None-Match" not in transport.calls[0][1]
    assert transport.calls[1][1]["If-None-Match"] == '"v1"'


def test_etag_cache_is_per_token() -> None:
    def answer(url: str, headers: dict[str, str]) -> Any:
        return 200, b"[]", _headers(4000, ETag='"v1"')

    transport = _Recorder(answer)
    GitHubClient(token="a", transport=transport).get("/repos/o/r/labels")
    GitHubClient(token="b", transport=transport).get("/repos/o/r/labels")

    assert "If-None-Match" not in transport.calls[1][1]


def test_a_two_element_transport_gets_no_conditional_requests() -> None:
    transport = _Recorder(lambda url, headers: (200, b"[]"))
    client = GitHubClient(token="t", transport=transport)
    client.get("/x")
    client.get("/x")
    assert all("If-None-Match" not in h for _, h in transport.calls)


# -- rate budget -------------------------------------------------------------


def test_reads_pause_below_the_floor_without_touching_the_network() -> None:
    configure_rate_limit_floor(500)
    transport = _Recorder(lambda url, headers: (200, b"[]", _headers(499)))
    client = GitHubClient(token="t", transport=transport)

    client.get("/one")  # learns remaining=499
    with pytest.raises(GitHubRateLimited) as raised:
        client.get("/two")

    assert len(transport.calls) == 1, "the paused read must not be sent"
    assert raised.value.reset_at == RESET


def test_reads_continue_above_the_floor() -> None:
    configure_rate_limit_floor(500)
    transport = _Recorder(lambda url, headers: (200, b"[]", _headers(501)))
    client = GitHubClient(token="t", transport=transport)
    client.get("/one")
    client.get("/two")
    assert len(transport.calls) == 2


def test_reads_resume_after_the_reset(monkeypatch: pytest.MonkeyPatch) -> None:
    now = [RESET - 60]
    monkeypatch.setattr(RATE_BUDGET, "clock", lambda: now[0])
    transport = _Recorder(lambda url, headers: (200, b"[]", _headers(10)))
    client = GitHubClient(token="t", transport=transport)

    client.get("/one")
    with pytest.raises(GitHubRateLimited):
        client.get("/two")

    now[0] = RESET + 1
    client.get("/three")
    assert [url.rsplit("/", 1)[1] for url, _ in transport.calls] == ["one", "three"]


def test_an_exhausted_403_raises_rate_limited() -> None:
    transport = _Recorder(
        lambda url, headers: (403, b'{"message":"rate limit"}', _headers(0))
    )
    with pytest.raises(GitHubRateLimited):
        GitHubClient(token="t", transport=transport).get("/one")


def test_a_non_core_bucket_does_not_gate_core_reads() -> None:
    transport = _Recorder(
        lambda url, headers: (
            200,
            b"[]",
            _headers(1, **{"X-RateLimit-Resource": "search"}),
        )
    )
    client = GitHubClient(token="t", transport=transport)
    client.get("/one")
    client.get("/two")
    assert len(transport.calls) == 2


# -- posture: one tree read, once a day ---------------------------------------


def _tree(*paths: tuple[str, str]) -> bytes:
    return json.dumps({"tree": [{"path": p, "sha": s} for p, s in paths]}).encode()


def _posture_answer(url: str, headers: dict[str, str]) -> Any:
    del headers
    if url.endswith("/git/trees/HEAD"):
        return 200, _tree(("README.md", "r1"), (".github", "gh1"))
    if url.endswith("/git/trees/gh1"):
        return 200, _tree(("dependabot.yml", "d1"), ("workflows", "w1"))
    return 404, b""


def test_posture_reads_the_tree_not_eight_contents_probes() -> None:
    transport = _Recorder(_posture_answer)
    provider = GitHubProvider(GitHubClient(token="t", transport=transport))

    posture = provider.posture(RepoRef(host="github.com", owner="o", repo="r"))

    assert posture.version_updates_config == ".github/dependabot.yml"
    urls = [url for url, _ in transport.calls]
    assert not any("/contents/" in url for url in urls)
    # Two tree reads plus the two toggles.
    assert len(urls) == 4


def test_posture_without_a_github_dir_is_one_tree_read() -> None:
    def answer(url: str, headers: dict[str, str]) -> Any:
        if url.endswith("/git/trees/HEAD"):
            return 200, _tree(("renovate.json", "x"))
        return 404, b""

    transport = _Recorder(answer)
    provider = GitHubProvider(GitHubClient(token="t", transport=transport))
    posture = provider.posture(RepoRef(host="github.com", owner="o", repo="r"))

    assert posture.version_updates_config == "renovate.json"
    assert sum("/git/trees/" in url for url, _ in transport.calls) == 1


def _project() -> Project:
    return Project(
        id="prj_1",
        slug="r",
        name="r",
        root_path="/tmp/r",
        repo_url="https://github.com/o/r",
        created_at=datetime(2026, 10, 18, tzinfo=UTC),
        updated_at=datetime(2026, 10, 18, tzinfo=UTC),
    )


@pytest.mark.parametrize(
    ("interval", "later", "reads_again"),
    [
        (86400, timedelta(hours=1), False),
        (86400, timedelta(days=1), True),
        (0, timedelta(seconds=1), True),
    ],
)
def test_posture_is_read_once_per_interval(
    monkeypatch: pytest.MonkeyPatch,
    interval: int,
    later: timedelta,
    reads_again: bool,
) -> None:
    transport = _Recorder(_posture_answer)

    def provider_for(*_args: object, **_kwargs: object) -> GitHubProvider:
        return GitHubProvider(GitHubClient(token="t", transport=transport))

    monkeypatch.setattr("vogt.adapters.forge.collectors.provider_for", provider_for)
    config = VogtConfig(forge_posture_interval_seconds=interval)
    now = [datetime(2026, 10, 18, 12, tzinfo=UTC)]
    ctx = CollectorContext(config=config, clock=lambda: now[0])
    collector = ForgePostureCollector()

    first = list(collector.collect(ctx, _project()))
    calls_after_first = len(transport.calls)
    now[0] += later
    second = list(collector.collect(ctx, _project()))

    assert [f.kind for f in first].count("forge.posture") == 1
    read = len(transport.calls) > calls_after_first
    assert read is reads_again
    # A skipped sweep still records its receipt, with nothing read.
    assert len(second) == (2 if reads_again else 1)
