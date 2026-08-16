"""Reading a project's CI state from check observations (FR-O6).

A check is *a check on a revision*, and the collector records it that way —
`gh-actions` stamps every workflow run with its `head_sha`. What was missing
was anyone reading it back that way. `project brief` and the
`ci_red_vs_healthy` drift rule both treated the retained window as one
population: any failure anywhere in it made the project `failing`, and the
revision they reported was whichever row happened to sort last by *sweep*
time.

The consequences were not subtle. `pingrag` read `failing` with a head commit
whose three workflow runs were all green — its twenty retained runs spanned
fourteen commits, and the two failures were four and six days old and long
fixed. `tfdrift` read `passing` at a revision two commits behind `HEAD`. And
because an old failure is retained until it ages out, a project could not
return to green by being green.

This module is pure: it takes the observations and answers which revision is
newest and what happened on it. Nothing here decides what to do about the
answer.
"""

from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime

from vogt.core.entities import Observation

#: Conclusions that are not a failure. `None` covers a run still going: it has
#: not failed, and reporting it as red would make every in-flight build a
#: defect.
PASSING_CONCLUSIONS: frozenset[str | None] = frozenset({None, "success", "skipped"})


@dataclass(frozen=True)
class CheckRollup:
    """What CI says about a project's newest observed revision.

    `revision` is the newest revision any retained check names, and `checks`
    and `failing` describe **only that revision**. `earlier_failures` carries
    what the old reading conflated with it — failures on revisions behind the
    newest — so the history is still there without being the verdict.
    """

    revision: str | None
    checks: tuple[Observation, ...]
    failing: tuple[str, ...]
    revisions_observed: int
    earlier_failures: int

    @property
    def status(self) -> str:
        return "failing" if self.failing else "passing"


def _ran_at(check: Observation) -> tuple[str, datetime]:
    """When the check ran, as the source reported it.

    `observed_at` is when *Vogt looked*, which is the same instant for every
    row in a sweep and therefore no ordering at all. The collector keeps the
    run's own `updated_at`; where it is missing, `observed_at` is the honest
    fallback and the empty string sorts it behind anything dated.
    """
    stamp = check.payload.get("updated_at")
    return (str(stamp) if isinstance(stamp, str) else "", check.observed_at)


def _revision_of(check: Observation) -> str:
    revision = check.payload.get("revision")
    return str(revision) if isinstance(revision, str) else ""


def _is_failing(check: Observation) -> bool:
    conclusion = check.payload.get("conclusion")
    return conclusion not in PASSING_CONCLUSIONS


def roll_up(checks: list[Observation]) -> CheckRollup | None:
    """Group checks by revision and answer for the newest one.

    Returns `None` for an empty input, which is "nobody has looked" — a
    different answer from "nothing failed", and the caller's to phrase.
    """
    dated = [check for check in checks if _revision_of(check)]
    if not dated:
        return None

    newest = max(dated, key=_ran_at)
    revision = _revision_of(newest)
    on_revision = tuple(check for check in dated if _revision_of(check) == revision)
    failing = sorted(
        {
            str(check.payload.get("check", "?"))
            for check in on_revision
            if _is_failing(check)
        }
    )
    return CheckRollup(
        revision=revision,
        checks=on_revision,
        failing=tuple(failing),
        revisions_observed=len({_revision_of(check) for check in dated}),
        earlier_failures=sum(
            1
            for check in dated
            if _revision_of(check) != revision and _is_failing(check)
        ),
    )
