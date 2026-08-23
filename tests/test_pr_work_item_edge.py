"""The PR ↔ work-item edge, observed from closing keywords and branch names (#284).

Observed-first, additive, forward-only (#287): the `implemented_by` edge is
*read* from a pull request — never typed in — and it reports rather than
enforces. So the assertions here are about reading it faithfully (every closing
form, cross-repo, title and body, the branch name), carrying its provenance,
*not* blocking completion the way `depends_on` does, and collapsing the PR
under the item it implements so one stream is counted once.
"""

from __future__ import annotations

import json
from pathlib import Path
from typing import Any

import pytest

from vogt.adapters.forge.edges import (
    FROM_BODY,
    FROM_BRANCH,
    FROM_TITLE,
    parse_edges,
)
from vogt.adapters.github.client import GitHubClient
from vogt.application.context import AppContext
from vogt.application.models import (
    BacklogParams,
    CreateWorkParams,
    RegisterProjectParams,
    RelateWorkParams,
    SweepParams,
    TransitionWorkParams,
)
from vogt.application.services import (
    backlog,
    create_work,
    register_project,
    relate_work,
    sweep,
    transition_work,
)
from vogt.core.workflow import TransitionRejected
from vogt.errors import InvalidRequest

WHY = "pr-work-item-edge test"
REPO = "https://github.com/TheDancingDeveloper-org/rustnzb"
SLUG = "TheDancingDeveloper-org/rustnzb"


# -- parsing: closing keywords (body and title) ----------------------------


@pytest.mark.parametrize(
    ("text", "number"),
    [
        ("Closes #12", 12),
        ("closes #12", 12),
        ("Closed #12", 12),
        ("Fixes #7", 7),
        ("fix #7", 7),
        ("Fixed #7", 7),
        ("Resolves #3", 3),
        ("resolve #3", 3),
        ("Resolved #3", 3),
        ("Closes: #9", 9),
        ("nothing before it, closes #4 at the end", 4),
    ],
)
def test_each_closing_form_is_read_from_the_body(text: str, number: int) -> None:
    edges = parse_edges(title="", body=text, branch=None)
    assert [e.number for e in edges] == [number]
    assert edges[0].provenance == FROM_BODY
    assert edges[0].owner is None and edges[0].repo is None


def test_a_closing_keyword_in_the_title_is_read_and_marked_as_such() -> None:
    edges = parse_edges(title="Fixes #5: the retry loop", body="", branch=None)
    assert [(e.number, e.provenance) for e in edges] == [(5, FROM_TITLE)]


def test_a_cross_repo_closing_reference_keeps_its_owner_and_repo() -> None:
    edges = parse_edges(title="", body="Closes octocat/hello-world#42", branch=None)
    assert len(edges) == 1
    assert edges[0].owner == "octocat"
    assert edges[0].repo == "hello-world"
    assert edges[0].number == 42


def test_multiple_closing_references_all_parse() -> None:
    edges = parse_edges(
        title="", body="Closes #1, fixes #2 and resolves #3", branch=None
    )
    assert sorted(e.number for e in edges) == [1, 2, 3]


def test_a_pr_that_names_no_work_yields_no_edge() -> None:
    edges = parse_edges(
        title="Refactor the transport",
        body="An ordinary open PR. Relates to #2.",  # 'relates' is not closing
        branch="main",
    )
    assert edges == []


def test_encloses_is_not_a_false_positive() -> None:
    # The keyword is anchored, so a substring in a larger word does not match.
    assert parse_edges(title="", body="This encloses #9 in quotes", branch=None) == []


# -- parsing: branch names (coordinates with #283's pattern) ---------------


@pytest.mark.parametrize(
    ("branch", "number"),
    [
        ("gh-1-open-bug", 1),
        ("gh-2-pagination", 2),
        ("feature/gh-4-cache", 4),
        ("wi-3/readme-typo", 3),
    ],
)
def test_a_branch_name_binds_its_issue_number(branch: str, number: int) -> None:
    edges = parse_edges(title="", body="", branch=branch)
    assert [(e.number, e.provenance) for e in edges] == [(number, FROM_BRANCH)]


def test_a_branch_that_matches_no_pattern_yields_no_edge() -> None:
    assert parse_edges(title="", body="", branch="dependabot/pip/requests") == []


def test_body_provenance_wins_when_the_branch_names_the_same_issue() -> None:
    # `Closes #1` and branch `gh-1-…` are the same target; the most deliberate
    # provenance (the body) is the one recorded, and the target is not doubled.
    edges = parse_edges(title="", body="Closes #1", branch="gh-1-open-bug")
    assert len(edges) == 1
    assert edges[0].number == 1
    assert edges[0].provenance == FROM_BODY


# -- the observation: state carried, edge recorded with provenance ---------


class FakeForge:
    """A GitHub whose issues and pulls a test sets, transport-level."""

    def __init__(self) -> None:
        self.issues: list[dict[str, Any]] = []
        self.pulls: list[dict[str, Any]] = []

    def __call__(
        self, url: str, headers: dict[str, str], body: bytes = b"", method: str = "GET"
    ) -> tuple[int, bytes]:
        del headers, body, method
        if "/issues" in url and "/comments" not in url:
            return 200, json.dumps(self.issues).encode("utf-8")
        if "/pulls" in url:
            return 200, json.dumps(self.pulls).encode("utf-8")
        if "/actions/runs" in url:
            return 200, json.dumps({"workflow_runs": []}).encode("utf-8")
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


def _pr_observation(instance: AppContext, number: int) -> Any:
    latest = instance.observed.latest_by_subject(f"gh:{SLUG}#{number}")
    assert latest is not None
    return latest


def test_the_pr_observation_records_the_edge_with_provenance(
    instance: AppContext, forge: FakeForge
) -> None:
    forge.pulls = [
        {
            "number": 20,
            "title": "Fix the retry loop",
            "state": "open",
            "body": "Closes #1.",
            "head": {"sha": "deadbeef", "ref": "gh-1-open-bug"},
            "updated_at": "2026-08-01T00:00:00Z",
        }
    ]
    sweep(instance, SweepParams(reason=WHY))
    implements = _pr_observation(instance, 20).payload["implements"]
    # One target — body and branch name the same issue, deduplicated — and the
    # body's provenance wins.
    assert len(implements) == 1
    assert implements[0]["subject"] == f"gh:{SLUG}#1"
    assert implements[0]["provenance"] == FROM_BODY


def test_a_cross_repo_close_keys_against_the_other_repo(
    instance: AppContext, forge: FakeForge
) -> None:
    forge.pulls = [
        {
            "number": 21,
            "title": "Upstream fix",
            "state": "open",
            "body": "Fixes octocat/hello-world#9",
            "head": {"sha": "abc", "ref": "topic"},
            "updated_at": "2026-08-01T00:00:00Z",
        }
    ]
    sweep(instance, SweepParams(reason=WHY))
    implements = _pr_observation(instance, 21).payload["implements"]
    assert implements[0]["subject"] == "gh:octocat/hello-world#9"
    assert implements[0]["provenance"] == FROM_BODY


def test_a_merged_pr_reads_merged_and_the_branch_ref_is_carried(
    instance: AppContext, forge: FakeForge
) -> None:
    forge.pulls = [
        {
            "number": 22,
            "title": "Shipped",
            "state": "closed",
            "merged_at": "2026-08-02T00:00:00Z",
            "body": "Resolves #2",
            "head": {"sha": "abc", "ref": "gh-2-pagination"},
            "updated_at": "2026-08-02T00:00:00Z",
        }
    ]
    sweep(instance, SweepParams(reason=WHY))
    payload = _pr_observation(instance, 22).payload
    assert payload["state"] == "merged"
    assert payload["head_ref"] == "gh-2-pagination"


# -- backlog collapse: the PR is counted under the item, not beside it ------


def _backlog_subjects(instance: AppContext) -> list[str]:
    result = backlog(instance, BacklogParams(limit=100))
    return [item.ref for item in result.items]


def test_a_pr_collapses_under_the_issue_it_implements(
    instance: AppContext, forge: FakeForge
) -> None:
    forge.issues = [
        {
            "number": 1,
            "title": "Crash on empty config",
            "state": "open",
            "labels": [{"name": "bug"}],
            "updated_at": "2026-08-01T00:00:00Z",
        }
    ]
    forge.pulls = [
        {
            "number": 20,
            "title": "Fix the crash",
            "state": "open",
            "body": "Closes #1.",
            "head": {"sha": "abc", "ref": "gh-1-open-bug"},
            "updated_at": "2026-08-01T00:00:00Z",
        }
    ]
    sweep(instance, SweepParams(reason=WHY))
    subjects = _backlog_subjects(instance)
    # The issue is on the board; the PR that implements it is not a sibling.
    assert f"gh:{SLUG}#1" in subjects
    assert f"gh:{SLUG}#20" not in subjects


def test_a_pr_whose_target_is_absent_still_lists_on_its_own(
    instance: AppContext, forge: FakeForge
) -> None:
    # No issue #99 is observed, so there is nothing to collapse under — the PR
    # is not silently dropped, it lists as its own subject.
    forge.pulls = [
        {
            "number": 30,
            "title": "Fix something untracked",
            "state": "open",
            "body": "Closes #99.",
            "head": {"sha": "abc", "ref": "topic"},
            "updated_at": "2026-08-01T00:00:00Z",
        }
    ]
    sweep(instance, SweepParams(reason=WHY))
    assert f"gh:{SLUG}#30" in _backlog_subjects(instance)


# -- non-blocking: implemented_by never gates completion -------------------


def test_implemented_by_cannot_be_declared_by_hand(instance: AppContext) -> None:
    """It is observed from a PR, never typed in (#284) — so it can never become
    a declared edge, and therefore can never gate a transition the way a
    hand-declared `depends_on` does."""
    a = create_work(
        instance, CreateWorkParams(kind="feature", title="A", reason=WHY)
    ).item
    b = create_work(
        instance, CreateWorkParams(kind="feature", title="B", reason=WHY)
    ).item
    with pytest.raises(InvalidRequest, match="observed edge"):
        relate_work(
            instance,
            RelateWorkParams(
                ref=a.ref, kind="implemented_by", target=b.ref, reason=WHY
            ),
        )


def _to_done(instance: AppContext, ref: str) -> str:
    item_state = ""
    for to_state in ("in_progress", "review", "done"):
        item_state = transition_work(
            instance,
            TransitionWorkParams(ref=ref, to_state=to_state, reason=WHY),
        ).item.state
    return item_state


def test_only_depends_on_gates_completion_never_implemented_by(
    instance: AppContext,
) -> None:
    """The completion gate is `depends_on`-specific (FR-W8). `implemented_by`
    is an observed PR edge that never enters the relations table at all, so it
    is structurally incapable of blocking a transition — an item it points at
    completes freely, unlike one held by an unfinished dependency."""
    blocker = create_work(
        instance, CreateWorkParams(kind="feature", title="Blocker", reason=WHY)
    ).item
    dependent = create_work(
        instance, CreateWorkParams(kind="feature", title="Dependent", reason=WHY)
    ).item
    relate_work(
        instance,
        RelateWorkParams(
            ref=dependent.ref, kind="depends_on", target=blocker.ref, reason=WHY
        ),
    )
    # depends_on on an unfinished target blocks completion...
    transition_work(
        instance,
        TransitionWorkParams(ref=dependent.ref, to_state="in_progress", reason=WHY),
    )
    transition_work(
        instance,
        TransitionWorkParams(ref=dependent.ref, to_state="review", reason=WHY),
    )
    with pytest.raises(TransitionRejected) as caught:
        transition_work(
            instance,
            TransitionWorkParams(ref=dependent.ref, to_state="done", reason=WHY),
        )
    assert caught.value.rule == "transition.blocked_by_dependency"
    # ...while a plain item — the shape an implemented_by target has, since that
    # edge is never a declared blocker — completes with nothing in its way.
    free = create_work(
        instance, CreateWorkParams(kind="feature", title="Free", reason=WHY)
    ).item
    assert _to_done(instance, free.ref) == "done"
