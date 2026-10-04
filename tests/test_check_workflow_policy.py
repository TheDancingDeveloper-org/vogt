"""Tests for `scripts/check_workflow_policy.py`, against fixture workflows."""

from __future__ import annotations

from pathlib import Path

import pytest

from check_workflow_policy import (
    check_contexts,
    main,
    parse_workflow,
    producer_problems,
    required_checks,
    run,
    timeout_problems,
)

REPO_ROOT = Path(__file__).resolve().parents[1]

PR_WORKFLOW = """\
name: ci
on:
  pull_request:
  push:
    branches: [main]
jobs:
  # a comment at job level is not a job
  lint:
    name: lint
    runs-on: [self-hosted]
    timeout-minutes: 10
    steps:
      - run: |
          echo "name: not-a-job-property"
          timeout-minutes: 99
  analyze:
    name: analyze (${{ matrix.language }})
    runs-on: [self-hosted]
    timeout-minutes: 45
    strategy:
      matrix:
        language:
          - python
          - rust
  test:
    runs-on: [self-hosted]
    strategy:
      matrix:
        py: ["3.11", "3.13"]
    steps:
      - run: true
  call:
    uses: ./.github/workflows/other.yml
"""


def test_a_job_without_timeout_is_reported_and_a_reusable_call_is_exempt() -> None:
    workflow = parse_workflow("ci.yml", PR_WORKFLOW)
    problems = timeout_problems(workflow)
    assert [(p.line, "'test'" in p.message) for p in problems] == [(25, True)]
    assert problems[0].annotation().startswith("::error file=ci.yml,line=25::")


def test_run_block_contents_are_not_read_as_job_properties() -> None:
    lint = parse_workflow("ci.yml", PR_WORKFLOW).jobs[0]
    assert (lint.name, lint.timeout) == ("lint", "10")


@pytest.mark.parametrize("value", ["0", "361"])
def test_an_out_of_range_literal_timeout_is_reported(value: str) -> None:
    text = (
        f"on: pull_request\njobs:\n  a:\n    runs-on: x\n    timeout-minutes: {value}\n"
    )
    assert len(timeout_problems(parse_workflow("w.yml", text))) == 1


def test_an_expression_timeout_is_accepted() -> None:
    text = "on: push\njobs:\n  a:\n    timeout-minutes: ${{ inputs.t }}\n"
    assert timeout_problems(parse_workflow("w.yml", text)) == []


def test_matrix_names_expand_like_github_reports_them() -> None:
    jobs = {j.job_id: j for j in parse_workflow("ci.yml", PR_WORKFLOW).jobs}
    assert check_contexts(jobs["analyze"]) == {"analyze (python)", "analyze (rust)"}
    assert check_contexts(jobs["test"]) == {"test (3.11)", "test (3.13)"}
    assert check_contexts(jobs["lint"]) == {"lint"}


def test_a_dynamic_matrix_leaves_the_name_unresolved() -> None:
    text = (
        "on: push\njobs:\n  b:\n    name: ${{ matrix.v }}\n    strategy:\n"
        "      matrix:\n        v: ${{ fromJSON(needs.p.outputs.v) }}\n"
    )
    assert check_contexts(parse_workflow("w.yml", text).jobs[0]) == {"${{ matrix.v }}"}


@pytest.mark.parametrize(
    ("on", "events"),
    [
        ("on: pull_request", {"pull_request"}),
        ("on: [push, pull_request]", {"push", "pull_request"}),
        ("on:\n  - merge_group\n  - push", {"merge_group", "push"}),
        ("'on':\n  push:\n    tags: ['v*']", {"push"}),
    ],
)
def test_trigger_spellings(on: str, events: set[str]) -> None:
    text = f"{on}\njobs:\n  a:\n    timeout-minutes: 5\n"
    assert parse_workflow("w.yml", text).events == events


def test_a_required_check_with_no_producer_is_an_orphan() -> None:
    workflows = [parse_workflow("ci.yml", PR_WORKFLOW)]
    problems = producer_problems(
        workflows, ["lint", "analyze (rust)", "dependency review"], "SECURITY.md"
    )
    assert len(problems) == 1
    assert "'dependency review' is produced by no workflow job" in problems[0].message


def test_a_producer_that_never_runs_on_a_pr_does_not_count() -> None:
    push_only = parse_workflow(
        "r.yml",
        "on:\n  push:\njobs:\n  r:\n    name: release\n    timeout-minutes: 5\n",
    )
    problems = producer_problems([push_only], ["release"], "SECURITY.md")
    assert len(problems) == 1
    assert "do not run on pull_request" in problems[0].message


def test_required_checks_are_read_from_the_security_md_list() -> None:
    text = (
        "intro\n\nconfigure these required checks:\n\n- `ci`\n- `analyze (rust)`\n\n"
        "Also enable:\n- `not-this`\n"
    )
    assert required_checks(text) == ["ci", "analyze (rust)"]
    assert required_checks("nothing here") == []


def _tree(tmp_path: Path, workflow: str, security: str) -> Path:
    (tmp_path / ".github" / "workflows").mkdir(parents=True)
    (tmp_path / ".github" / "workflows" / "ci.yml").write_text(workflow)
    (tmp_path / "SECURITY.md").write_text(security)
    return tmp_path


def test_main_fails_on_a_fixture_tree_and_passes_once_fixed(
    tmp_path: Path, capsys: pytest.CaptureFixture[str]
) -> None:
    security = "required checks:\n\n- `lint`\n- `orphan`\n"
    root = _tree(tmp_path, PR_WORKFLOW, security)
    assert main(["--root", str(root)]) == 1
    out = capsys.readouterr().out
    assert "job 'test' has no timeout-minutes" in out
    assert "'orphan' is produced by no workflow job" in out

    fixed = PR_WORKFLOW.replace(
        "  test:\n    runs-on: [self-hosted]\n",
        "  test:\n    runs-on: [self-hosted]\n    timeout-minutes: 20\n",
    )
    (root / ".github" / "workflows" / "ci.yml").write_text(fixed)
    (root / "SECURITY.md").write_text("required checks:\n\n- `lint`\n")
    assert main(["--root", str(root)]) == 0


def test_a_missing_required_list_is_itself_a_problem(tmp_path: Path) -> None:
    root = _tree(tmp_path, "on: pull_request\njobs: {}\n", "no list\n")
    assert any("no 'required checks:'" in p.message for p in run(root))


def test_the_repository_itself_passes() -> None:
    assert run(REPO_ROOT) == []
