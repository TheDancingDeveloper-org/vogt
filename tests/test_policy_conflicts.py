"""WI-1832: reading a task for contradictions of the landing rule."""

from __future__ import annotations

import pytest

from vogt.application.services._policy_conflicts import (
    overrides_section,
    task_policy_conflicts,
)
from vogt.core.entities import LandingPolicy


def _landing(**changes: str) -> LandingPolicy:
    fields = {
        "base_branch": "main",
        "worker_lands_by": "push_branch",
        "merge_by": "oversight",
        "merge_mode": "rebase",
        **changes,
    }
    return LandingPolicy.model_validate(fields)


@pytest.mark.parametrize(
    ("task", "axis"),
    [
        ("Open a PR against main.", "push only"),
        ("then `gh pr create --fill`", "push only"),
        ("Create a draft pull request.", "push only"),
        ("Merge it into main when green.", "do not merge"),
        ("Fix, then merge.", "do not merge"),
        ("Deploy to vogt-dev afterwards.", "do not deploy"),
        ("Cut a branch off develop.", "base `main`"),
    ],
)
def test_each_axis_is_detected(task: str, axis: str) -> None:
    lines = task_policy_conflicts(task, _landing())
    assert any(f"Policy overrides task: {axis}" in line for line in lines), lines


@pytest.mark.parametrize(
    "task",
    [
        "Open NO PR; push the branch only.",
        "Do not open a PR. Never merge, never deploy.",
        "Don't merge it into main.",
        "Resolve the merge conflicts and push.",
        "Rebase onto main and push.",
        "",
    ],
)
def test_agreement_and_negation_are_not_conflicts(task: str) -> None:
    assert task_policy_conflicts(task, _landing()) == []


def test_push_only_on_a_pr_project_is_overridden() -> None:
    lines = task_policy_conflicts(
        "Push the branch only.", _landing(worker_lands_by="open_pr")
    )
    assert lines == [
        "Policy overrides task: open a PR. The task says push only; this "
        "project lands by opening a pull request against `main`."
    ]


def test_a_worker_deployer_allows_deploy_and_no_landing_means_nothing() -> None:
    assert task_policy_conflicts("Deploy it.", _landing(deploy_by="worker")) == []
    assert task_policy_conflicts("Open a PR and merge it.", None) == []


def test_the_section_is_empty_without_lines() -> None:
    assert overrides_section([]) == ""
    assert overrides_section(["x"]).startswith("## Policy overrides task")


def test_a_known_long_lived_base_is_named_not_contradicted() -> None:
    """WI-1841."""
    from vogt.application.services._policy_conflicts import task_named_base

    landing = LandingPolicy.model_validate(
        {
            "base_branch": "main",
            "worker_lands_by": "push_branch",
            "merge_by": "oversight",
            "merge_mode": "rebase",
            "initiative_bases": {"rust-port": "rust-core"},
        }
    )
    assert task_named_base("Branch off `rust-core`.", landing) == "rust-core"
    assert task_named_base("Branch off develop.", landing) is None
    assert task_named_base("Rebase onto main; branch off rust-core.", landing) is None
    assert task_named_base("Never branch off rust-core.", landing) is None


def test_a_caller_that_merges_is_not_told_not_to_merge() -> None:
    lines = task_policy_conflicts("Then merge it.", _landing(), caller_merges=True)
    assert not any("do not merge" in line for line in lines)


_GRAFT_SOURCE = {
    "location": {"kind": "graft", "endpoint": "http://graft.test", "repo": "vogt"},
    "ref": "rust-2",
    "landing": "line",
}


def _policy_with(**extra: object) -> dict[str, object]:
    return {
        "base_branch": "main",
        "worker_lands_by": "push_branch",
        "merge_by": "oversight",
        "merge_mode": "rebase",
        **extra,
    }


def test_landing_policy_accepts_a_project_source() -> None:
    policy = LandingPolicy.model_validate(_policy_with(source=_GRAFT_SOURCE))

    assert policy.source == _GRAFT_SOURCE
    assert LandingPolicy.model_validate(_policy_with()).source is None


@pytest.mark.parametrize(
    "bad",
    [
        {"location": {"kind": "svn", "repo": "r"}, "ref": "main"},
        {"location": _GRAFT_SOURCE["location"], "ref": ""},
        {"location": _GRAFT_SOURCE["location"], "ref": "x", "landing": "merge"},
        {"ref": "main"},
    ],
)
def test_landing_policy_refuses_a_malformed_source(bad: dict[str, object]) -> None:
    with pytest.raises(ValueError):
        LandingPolicy.model_validate(_policy_with(source=bad))
