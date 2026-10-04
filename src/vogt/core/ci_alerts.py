"""Which CI runs deserve attention on their own: watched refs and bound branches.

`core.checks` answers "what does CI say about a project's newest revision",
which is the right question for a pull request and the wrong one for a
release. A tag push starts `release` and `release-mobile`; a push to the
default branch starts `build`. None of them blocks a pull request, so when
one fails the newest-revision roll-up has usually moved on to some other
commit within the hour and the failure is never seen — `release-mobile`
failed on two consecutive tags before anyone noticed.

This module reads the same check observations by **lane** instead:

- a *watched ref* lane is one workflow on one default/prod branch, or one
  workflow across the release tags. The newest *decisive* run in the lane
  is the verdict: a failure stays an alert until a later run of the same
  workflow in the same lane succeeds. Pull-request runs are never in a lane.
- a *bound branch* is a branch a work item declared (`work.bind_branch`);
  its newest revision's runs say whether CI is still going, passed or failed
  — the event an agent would otherwise poll for.

Pure: observations in, verdicts out. Nothing here reads a store or a clock.
"""

from __future__ import annotations

import re
from collections.abc import Iterable, Mapping, Sequence
from dataclasses import dataclass
from datetime import datetime
from fnmatch import fnmatchcase
from typing import Literal

from vogt.core.entities import Observation

#: Runs a pull request started. They are the PR's own business — its checks
#: block its merge — so they never raise a watched-ref alert.
PULL_REQUEST_EVENTS: frozenset[str] = frozenset(
    {"pull_request", "pull_request_target", "merge_group"}
)

#: Runs GitHub starts from its own managed ("dynamic") workflows: Dependabot
#: updates, CodeQL default setup and similar. They are not the repository's
#: CI, they land on the default branch, and Dependabot's fail routinely when
#: an update can't be resolved, so alerting on them floods the Inbox with
#: "Update #N failed on main" noise.
GITHUB_MANAGED_EVENTS: frozenset[str] = frozenset({"dynamic"})

#: Where GitHub files its managed workflows (`dynamic/dependabot/...`).
GITHUB_MANAGED_PATH_PREFIX = "dynamic/"

#: A Dependabot update run's name: "npm_and_yarn in /web for undici - Update
#: #1606151494". Observations collected before the event and workflow path
#: were stored carry only the name, so this is how they are recognised.
_DEPENDABOT_RUN_NAME = re.compile(r" - Update #\d+$")


def github_managed(payload: Mapping[str, object]) -> bool:
    """Whether a check observation is a run of a GitHub-managed workflow.

    Its event is `dynamic`, its workflow path sits under `dynamic/`, or —
    for an observation stored before either was recorded — its name is a
    Dependabot update's. Reads only the stored payload.
    """
    event = payload.get("event")
    if isinstance(event, str) and event in GITHUB_MANAGED_EVENTS:
        return True
    path = payload.get("workflow_path")
    if isinstance(path, str) and path.startswith(GITHUB_MANAGED_PATH_PREFIX):
        return True
    if isinstance(event, str) and event:
        return False
    name = payload.get("check")
    return isinstance(name, str) and _DEPENDABOT_RUN_NAME.search(name) is not None


#: Conclusions that are a failure worth an alert.
FAILING_CONCLUSIONS: frozenset[str] = frozenset(
    {"failure", "timed_out", "startup_failure", "action_required", "error"}
)

#: Conclusions that settle a lane as healthy.
PASSING_CONCLUSIONS: frozenset[str] = frozenset({"success", "neutral", "skipped"})

LaneKind = Literal["branch", "tag"]
BranchState = Literal["running", "passed", "failed", "cancelled"]


@dataclass(frozen=True)
class WatchedRef:
    """Where a run sits: which lane, and the concrete ref it ran on."""

    kind: LaneKind
    #: The lane's name — the branch itself, or the tag pattern that matched.
    lane: str
    ref: str


@dataclass(frozen=True)
class RefFailure:
    """A lane whose newest decisive run failed."""

    observation: Observation
    workflow: str
    where: WatchedRef
    conclusion: str


@dataclass(frozen=True)
class BranchRun:
    """One workflow's newest run on a bound branch's head revision."""

    observation: Observation
    workflow: str
    conclusion: str | None


@dataclass(frozen=True)
class BranchCi:
    """CI on a bound branch's newest observed revision."""

    branch: str
    revision: str
    state: BranchState
    runs: tuple[BranchRun, ...]
    failing: tuple[str, ...]
    #: When the last run on the revision finished, as the forge said.
    concluded_at: str | None

    @property
    def settled(self) -> bool:
        return self.state != "running"


def watched_ref(
    branch: object,
    event: object,
    *,
    branches: Sequence[str],
    tags: Sequence[str],
) -> WatchedRef | None:
    """The lane a run belongs to, or `None` when it is not watched.

    A run with no ref, one a pull request started, or one from a
    GitHub-managed dynamic workflow (Dependabot updates) is never watched. A
    ref matching a branch pattern is a branch even if a tag pattern would also
    match it — a branch and a tag of the same name are vanishingly rare, and
    the branch lane is the stricter of the two.
    """
    if not isinstance(branch, str) or not branch:
        return None
    if isinstance(event, str) and (
        event in PULL_REQUEST_EVENTS or event in GITHUB_MANAGED_EVENTS
    ):
        return None
    for pattern in branches:
        if fnmatchcase(branch, pattern):
            return WatchedRef(kind="branch", lane=branch, ref=branch)
    for pattern in tags:
        if fnmatchcase(branch, pattern):
            return WatchedRef(kind="tag", lane=pattern, ref=branch)
    return None


def watched_failures(
    checks: Iterable[Observation],
    *,
    branches: Sequence[str],
    tags: Sequence[str],
) -> list[RefFailure]:
    """Every watched lane whose newest decisive run failed.

    *Decisive* means it concluded with a pass or a fail: a run still going, a
    cancelled run (often superseded by a newer push) and a stale one say
    nothing about whether the lane is healthy, so they neither raise nor
    clear an alert.
    """
    newest: dict[tuple[str | None, str, LaneKind, str], tuple[Observation, str]] = {}
    places: dict[tuple[str | None, str, LaneKind, str], WatchedRef] = {}
    for check in checks:
        payload = check.payload
        conclusion = payload.get("conclusion")
        if not isinstance(conclusion, str) or not (
            conclusion in FAILING_CONCLUSIONS or conclusion in PASSING_CONCLUSIONS
        ):
            continue
        where = watched_ref(
            payload.get("branch"),
            payload.get("event"),
            branches=branches,
            tags=tags,
        )
        if where is None or github_managed(payload):
            continue
        workflow = _workflow(check)
        key = (check.project_id, workflow, where.kind, where.lane)
        held = newest.get(key)
        if held is None or ran_at(check) > ran_at(held[0]):
            newest[key] = (check, conclusion)
            places[key] = where
    return [
        RefFailure(
            observation=check,
            workflow=key[1],
            where=places[key],
            conclusion=conclusion,
        )
        for key, (check, conclusion) in newest.items()
        if conclusion in FAILING_CONCLUSIONS
    ]


def branch_ci(checks: Iterable[Observation], branch: str) -> BranchCi | None:
    """What CI says about `branch`'s newest observed revision, or `None`
    when no run on the branch has been observed."""
    on_branch = [
        check
        for check in checks
        if check.payload.get("branch") == branch and _revision(check)
    ]
    if not on_branch:
        return None
    revision = _revision(max(on_branch, key=ran_at))
    latest: dict[str, Observation] = {}
    for check in on_branch:
        if _revision(check) != revision:
            continue
        workflow = _workflow(check)
        held = latest.get(workflow)
        if held is None or ran_at(check) > ran_at(held):
            latest[workflow] = check
    runs = tuple(
        BranchRun(
            observation=check,
            workflow=workflow,
            conclusion=_conclusion(check),
        )
        for workflow, check in sorted(latest.items())
    )
    failing = tuple(
        run.workflow for run in runs if run.conclusion in FAILING_CONCLUSIONS
    )
    state: BranchState
    if any(run.conclusion is None for run in runs):
        state = "running"
    elif failing:
        state = "failed"
    elif all(run.conclusion in ("cancelled", "stale") for run in runs):
        state = "cancelled"
    else:
        state = "passed"
    stamps = [
        stamp
        for stamp in (run.observation.payload.get("updated_at") for run in runs)
        if isinstance(stamp, str)
    ]
    return BranchCi(
        branch=branch,
        revision=revision,
        state=state,
        runs=runs,
        failing=failing,
        concluded_at=max(stamps) if stamps and state != "running" else None,
    )


def ran_at(check: Observation) -> tuple[str, int, datetime]:
    """When a run ran, as the forge reported it, for ordering runs.

    The run's own `updated_at` first (a re-run moves it forward), then its
    run number, then when Vogt observed it — the honest fallback.
    """
    stamp = check.payload.get("updated_at")
    number = check.payload.get("run_number")
    return (
        stamp if isinstance(stamp, str) else "",
        number if isinstance(number, int) else 0,
        check.observed_at,
    )


def _workflow(check: Observation) -> str:
    name = check.payload.get("check")
    return name if isinstance(name, str) and name else "workflow"


def _revision(check: Observation) -> str:
    revision = check.payload.get("revision")
    return revision if isinstance(revision, str) else ""


def _conclusion(check: Observation) -> str | None:
    """A run's conclusion, or `None` while it is still going."""
    conclusion = check.payload.get("conclusion")
    if isinstance(conclusion, str) and conclusion:
        return conclusion
    return None


__all__ = [
    "FAILING_CONCLUSIONS",
    "GITHUB_MANAGED_EVENTS",
    "PASSING_CONCLUSIONS",
    "PULL_REQUEST_EVENTS",
    "BranchCi",
    "BranchRun",
    "RefFailure",
    "WatchedRef",
    "branch_ci",
    "github_managed",
    "ran_at",
    "watched_failures",
    "watched_ref",
]
