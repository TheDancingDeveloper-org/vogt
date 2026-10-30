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


_GRAFT = {
    "worker_lands_by": "graft",
    "graft": {
        "line": "backlog-1",
        "mirror": "vogt",
        "endpoint": "http://graft-dev:17888",
    },
}


def test_graft_landing_needs_its_block_and_only_with_graft() -> None:
    """#1240: `graft` names line, mirror and endpoint; other modes refuse it."""
    landing = LandingPolicy.model_validate(
        {"base_branch": "main", "merge_by": "oversight", "merge_mode": "rebase"}
        | _GRAFT
    )
    assert landing.graft is not None and landing.graft.line == "backlog-1"
    with pytest.raises(ValueError, match="needs graft"):
        _landing(worker_lands_by="graft")
    with pytest.raises(ValueError, match="only when worker_lands_by is graft"):
        LandingPolicy.model_validate(
            {"base_branch": "main", "merge_by": "oversight", "merge_mode": "rebase"}
            | _GRAFT
            | {"worker_lands_by": "push_branch"}
        )


@pytest.mark.parametrize("task", ["Open a PR against main.", "Push the branch only."])
def test_graft_overrides_a_task_asking_for_push_or_pr(task: str) -> None:
    landing = LandingPolicy.model_validate(
        {"base_branch": "main", "merge_by": "oversight", "merge_mode": "rebase"}
        | _GRAFT
    )
    lines = task_policy_conflicts(task, landing)
    assert any("land through graft" in line and "backlog-1" in line for line in lines)
    assert task_policy_conflicts("Land it; open NO PR.", landing) == []
