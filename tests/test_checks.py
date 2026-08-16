"""CI state is the state of a revision, not of a retention window (FR-O6).

Both scenarios below are real. `pingrag` and `tfdrift` were onboarded on
2026-08-16 and the brief was wrong about both, in opposite directions, from
one cause: nothing grouped the checks by the revision the collector had
already stamped on them.
"""

from __future__ import annotations

from datetime import UTC, datetime

from vogt.core.checks import roll_up
from vogt.core.entities import Observation

SWEPT_AT = datetime(2026, 8, 16, 7, 14, tzinfo=UTC)


def _check(
    revision: str, name: str, conclusion: str | None, ran_at: str, *, index: int = 0
) -> Observation:
    """One workflow run, shaped as `gh-actions` records it.

    `observed_at` is deliberately identical across every row: that is what a
    sweep produces, and treating it as an ordering is the bug under test.
    """
    return Observation(
        id=f"obs_{revision}_{name}_{index}",
        sweep_id="swp_1",
        collector="gh-actions",
        kind="ci.check",
        project_id="prj_1",
        subject_key=f"ci:owner/repo@{revision}:{name}",
        payload={
            "revision": revision,
            "check": name,
            "conclusion": conclusion,
            "updated_at": ran_at,
        },
        content_digest="sha256:x",
        observed_at=SWEPT_AT,
    )


def test_nothing_observed_is_not_a_verdict() -> None:
    assert roll_up([]) is None


def test_a_check_with_no_revision_cannot_be_placed() -> None:
    """A check that does not name a commit is not evidence about one."""
    orphan = _check("", "build", "failure", "2026-08-16T04:00:00Z")
    assert roll_up([orphan]) is None


def test_a_green_head_reads_green_however_red_its_history() -> None:
    """The `pingrag` case: 20 runs, 14 commits, two old failures, green head.

    It read `failing`, and `ci_red_vs_healthy` was raised against it. The
    failures were on `004f5f3` and `0291aff`, two and four days before the
    head commit, and both were fixed by the time anyone looked.
    """
    rollup = roll_up(
        [
            _check("0291aff", "build image", "failure", "2026-08-09T22:41:46Z"),
            _check("004f5f3", "build image", "failure", "2026-08-11T03:51:48Z"),
            _check("9fe53d8", "Runner policy", "success", "2026-08-13T09:36:23Z"),
            _check("9fe53d8", "build and deploy", "success", "2026-08-13T09:36:25Z"),
            _check("9fe53d8", "build image", "success", "2026-08-13T09:36:27Z"),
        ]
    )
    assert rollup is not None
    assert rollup.status == "passing"
    assert rollup.revision == "9fe53d8"
    assert len(rollup.checks) == 3, "three checks on the head, not twenty"
    assert rollup.failing == ()
    assert rollup.earlier_failures == 2, "the history survives, as history"
    assert rollup.revisions_observed == 3


def test_the_newest_revision_is_the_newest_run_not_the_last_row() -> None:
    """The `tfdrift` case: three green runs, and it reported the oldest.

    Every row carries the same `observed_at` because one sweep collected them
    all, so `max(observed_at)` returned an arbitrary member — in practice the
    first. The run's own timestamp is the only ordering in the data.
    """
    rollup = roll_up(
        [
            _check("6875f6d", "build and deploy", "success", "2026-08-16T04:50:53Z"),
            _check("ca6a2ca", "build and deploy", "success", "2026-08-16T04:55:22Z"),
            _check("6a55741", "build and deploy", "success", "2026-08-16T04:56:39Z"),
        ]
    )
    assert rollup is not None
    assert rollup.revision == "6a55741", "the commit GitHub built last"


def test_a_failure_on_the_newest_revision_is_still_a_failure() -> None:
    rollup = roll_up(
        [
            _check("aaa1111", "ci", "success", "2026-08-15T10:00:00Z"),
            _check("bbb2222", "ci", "failure", "2026-08-16T10:00:00Z"),
            _check("bbb2222", "lint", "success", "2026-08-16T10:00:01Z"),
        ]
    )
    assert rollup is not None
    assert rollup.status == "failing"
    assert rollup.failing == ("ci",)
    assert rollup.earlier_failures == 0


def test_a_run_still_going_is_not_a_failure() -> None:
    """A null conclusion is an in-flight build, not a defect."""
    rollup = roll_up([_check("aaa1111", "ci", None, "2026-08-16T10:00:00Z")])
    assert rollup is not None
    assert rollup.status == "passing"


def test_one_workflow_cannot_appear_twice_for_one_revision() -> None:
    """Two runs of one workflow on one commit is a re-run, not two checks.

    The old code deduplicated in `brief` and not in the drift summary, which
    is why a proposal once read "2 check(s) failed: build image, build image".
    """
    rollup = roll_up(
        [
            _check("aaa1111", "build", "failure", "2026-08-16T10:00:00Z", index=1),
            _check("aaa1111", "build", "failure", "2026-08-16T11:00:00Z", index=2),
        ]
    )
    assert rollup is not None
    assert rollup.failing == ("build",)


def test_fixing_the_build_goes_green_without_waiting_for_retention() -> None:
    """The property the old shape could not have: red → green on a new commit.

    Previously the failing observation stayed in the window and kept the
    project `failing` until it aged out, so being green was not enough.
    """
    red = _check("aaa1111", "ci", "failure", "2026-08-16T10:00:00Z")
    fixed = _check("bbb2222", "ci", "success", "2026-08-16T12:00:00Z")

    assert (before := roll_up([red])) is not None and before.status == "failing"
    assert (after := roll_up([red, fixed])) is not None and after.status == "passing"
