"""Incremental all-state forge sync (#173) — and #169's regression.

The defect #169 named: the collectors asked only for *open* issues, so a
closure upstream was never observed and the ranked backlog showed closed work
as open forever. The fix is here — `state=all` incremental sync with a
watermark — and the exit criterion is a test: observe an issue open, close it
upstream, sweep, and watch it leave the backlog.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest

from vogt.adapters.github.client import GitHubClient
from vogt.application.context import AppContext
from vogt.application.models import (
    BacklogParams,
    RegisterProjectParams,
    SweepParams,
)
from vogt.application.services import backlog, register_project, sweep
from vogt.core.observed import LIFECYCLE_CLOSED, lifecycle_of

WHY = "forge sync test"
REPO = "https://github.com/TheDancingDeveloper-org/rustnzb"


class FakeForge:
    """A GitHub whose issue/PR state a test can change between sweeps."""

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
        if "/actions/runs" in url:
            return 200, json.dumps({"workflow_runs": []}).encode("utf-8")
        if "/releases" in url:
            return 200, b"[]"
        if "/contents/" in url or "-alerts" in url or "-fixes" in url:
            return 404, b""
        return 200, b"[]"


@pytest.fixture
def forge(
    instance: AppContext, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> FakeForge:
    fake = FakeForge()

    def configured(path: Any, *, transport: Any = None) -> GitHubClient:
        del path, transport
        return GitHubClient(token="ghp_fake", transport=fake)

    monkeypatch.setattr(GitHubClient, "from_token_file", staticmethod(configured))

    root = tmp_path / "rustnzb"
    root.mkdir()
    register_project(
        instance,
        RegisterProjectParams(
            name="rustnzb", root_path=str(root), repo_url=REPO, reason=WHY
        ),
    )
    return fake


def _backlog_numbers(instance: AppContext) -> list[int]:
    result = backlog(instance, BacklogParams(limit=100))
    numbers: list[int] = []
    for item in result.items:
        if item.origin == "observed" and item.title.startswith("#"):
            numbers.append(int(item.title.split()[0].lstrip("#")))
    return numbers


# -- #169: a closure upstream leaves the backlog --------------------------


def test_a_closed_issue_leaves_the_backlog_after_a_sweep(
    instance: AppContext, forge: FakeForge
) -> None:
    """#169's reproduction, as the regression test the fix owes it."""
    forge.issues = [
        {
            "number": 42,
            "title": "Segments never retry",
            "state": "open",
            "labels": [{"name": "bug"}],
            "updated_at": "2026-08-01T00:00:00Z",
        }
    ]
    sweep(instance, SweepParams(reason=WHY))
    assert 42 in _backlog_numbers(instance), "an open issue is backlog"

    # Closed upstream, and — as a real closure does — its `updated_at` moves.
    forge.issues = [
        {
            "number": 42,
            "title": "Segments never retry",
            "state": "closed",
            "labels": [{"name": "bug"}],
            "updated_at": "2026-08-05T00:00:00Z",
            "closed_at": "2026-08-05T00:00:00Z",
        }
    ]
    sweep(instance, SweepParams(reason=WHY))

    assert 42 not in _backlog_numbers(instance), "a closed issue is not backlog"
    result = backlog(instance, BacklogParams(limit=100))
    assert result.closed_upstream >= 1, "and it is counted as closed upstream"


def test_the_closure_is_recorded_as_an_observation(
    instance: AppContext, forge: FakeForge
) -> None:
    """The projection learns `closed` because a `closed` observation was written.

    #169's root cause was that no closed observation was ever written; the
    open-only scrape could only ever drop the subject from its result set."""
    forge.issues = [
        {
            "number": 7,
            "title": "x",
            "state": "open",
            "updated_at": "2026-08-01T00:00:00Z",
        }
    ]
    sweep(instance, SweepParams(reason=WHY))
    forge.issues = [
        {
            "number": 7,
            "title": "x",
            "state": "closed",
            "updated_at": "2026-08-02T00:00:00Z",
        }
    ]
    sweep(instance, SweepParams(reason=WHY))
    latest = instance.observed.latest_by_subject(
        "gh:TheDancingDeveloper-org/rustnzb#7"
    )
    assert latest is not None
    assert lifecycle_of(latest) == LIFECYCLE_CLOSED


# -- the watermark and dedup ----------------------------------------------


def test_the_watermark_advances_to_the_newest_update(
    instance: AppContext, forge: FakeForge
) -> None:
    forge.issues = [
        {"number": 1, "state": "open", "updated_at": "2026-08-01T00:00:00Z"},
        {"number": 2, "state": "open", "updated_at": "2026-08-03T00:00:00Z"},
    ]
    sweep(instance, SweepParams(reason=WHY))
    with instance.declared.read() as view:
        project = view.project_by_slug("rustnzb")
    assert project is not None
    watermark = instance.observed.get_watermark(
        collector="forge-issues", project_id=project.id
    )
    assert watermark is not None and "2026-08-03" in watermark


def test_an_unchanged_subject_is_still_confirmed(
    instance: AppContext, forge: FakeForge
) -> None:
    """subject_seen is touched every sweep, even when nothing about it changed."""
    forge.issues = [
        {
            "number": 5,
            "title": "steady",
            "state": "open",
            "updated_at": "2026-08-01T00:00:00Z",
        }
    ]
    sweep(instance, SweepParams(reason=WHY))
    sweep(instance, SweepParams(reason=WHY))
    seen = instance.observed.last_confirmed(["gh:TheDancingDeveloper-org/rustnzb#5"])
    assert "gh:TheDancingDeveloper-org/rustnzb#5" in seen


# -- receipts distinguish 0 from not-collected (FR-O4/O11) ----------------


def test_a_non_github_project_gets_a_not_supported_receipt(
    instance: AppContext, forge: FakeForge, tmp_path: Path
) -> None:
    other = tmp_path / "forgejo"
    other.mkdir()
    register_project(
        instance,
        RegisterProjectParams(
            name="Elsewhere",
            root_path=str(other),
            repo_url="https://repo.indexarr.net/indexarr/Indexarr.git",
            reason=WHY,
        ),
    )
    sweep(instance, SweepParams(reason=WHY))
    receipts = instance.observed.latest(kinds=("forge.sync",))
    elsewhere = [
        r
        for r in receipts
        if r.payload.get("supported") is False and "indexarr" in str(r.payload)
    ]
    assert elsewhere, "the unreadable host reports itself, not a silent zero"
    assert elsewhere[0].payload.get("detail")


def test_pull_requests_sync_and_self_heal(
    instance: AppContext, forge: FakeForge
) -> None:
    forge.pulls = [
        {
            "number": 100,
            "title": "wip",
            "state": "open",
            "updated_at": "2026-08-01T00:00:00Z",
        }
    ]
    sweep(instance, SweepParams(reason=WHY))
    assert 100 in _backlog_numbers(instance)
    forge.pulls = [
        {
            "number": 100,
            "title": "wip",
            "state": "closed",
            "updated_at": "2026-08-02T00:00:00Z",
        }
    ]
    sweep(instance, SweepParams(reason=WHY))
    assert 100 not in _backlog_numbers(instance)
