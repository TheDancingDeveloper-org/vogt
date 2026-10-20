"""When a watch fires, and what the woken session is told (WI-2036).

A watch lets a session wait on an event instead of polling for it: "wake me
when PR #N merges", "when CI on this revision concludes", "when WI-7 is
done", "when that session ends". The application layer gathers the evidence
for each active watch (observations, a work item, an engine session); this
module decides, from that evidence alone, whether the watch has fired and
what the outcome is.

Pure: evidence in, an outcome or `None` out. Nothing here reads a store or
a clock.
"""

from __future__ import annotations

from collections.abc import Iterable, Mapping, Sequence
from typing import Any

from vogt.core.ci_alerts import FAILING_CONCLUSIONS, ran_at
from vogt.core.entities import Observation

KIND_PR = "pr"
KIND_CI = "ci"
KIND_WORK_ITEM = "work_item"
KIND_SESSION = "session"
KINDS: tuple[str, ...] = (KIND_PR, KIND_CI, KIND_WORK_ITEM, KIND_SESSION)

#: PR states a watch can wait for, as the forge observation records them:
#: `merged` is distinct from a bare `closed` (`adapters/forge/sync.py`).
PR_EVENTS: frozenset[str] = frozenset({"merged", "closed"})

#: Engine activities that mean a session has stopped working.
ENDED_ACTIVITIES: frozenset[str] = frozenset({"exited", "hibernated", "stopped"})

NOTICE_MAX_CHARS = 500

Outcome = dict[str, Any]


def pr_outcome(
    payload: Mapping[str, object] | None, on: Sequence[str], url: str | None = None
) -> Outcome | None:
    """Fired when the observed pull request reads one of `on`."""
    if payload is None:
        return None
    state = payload.get("state")
    if not isinstance(state, str) or state not in on:
        return None
    return {
        "state": state,
        "number": payload.get("number"),
        "title": payload.get("title"),
        "head": payload.get("head"),
        "merged_by": payload.get("merged_by"),
        "url": url,
    }


def ci_outcome(
    checks: Iterable[Observation], revision: str, workflows: Sequence[str] = ()
) -> Outcome | None:
    """Fired when every run on `revision` has concluded.

    `workflows`, when given, are the checks that count — the required ones;
    the watch waits until each has been observed and concluded, and ignores
    every other run. Without it, every observed run on the revision counts,
    and at least one must have been observed.
    """
    latest: dict[str, Observation] = {}
    for check in checks:
        if not _same_revision(check.payload.get("revision"), revision):
            continue
        name = check.payload.get("check")
        if not isinstance(name, str) or not name:
            continue
        if workflows and name not in workflows:
            continue
        held = latest.get(name)
        if held is None or ran_at(check) > ran_at(held):
            latest[name] = check
    if not latest:
        return None
    if workflows and any(name not in latest for name in workflows):
        return None
    conclusions = {name: _conclusion(check) for name, check in latest.items()}
    if any(conclusion is None for conclusion in conclusions.values()):
        return None
    failing = sorted(n for n, c in conclusions.items() if c in FAILING_CONCLUSIONS)
    if failing:
        state = "failed"
    elif all(c in ("cancelled", "stale") for c in conclusions.values()):
        state = "cancelled"
    else:
        state = "passed"
    url = next(
        (latest[name].source_url for name in failing if latest[name].source_url),
        None,
    )
    return {
        "state": state,
        "revision": revision,
        "checks": sorted(latest),
        "failing": failing,
        "url": url,
    }


def work_item_outcome(state: str | None, states: Sequence[str]) -> Outcome | None:
    """Fired when the work item is in one of `states`."""
    if state is None or state not in states:
        return None
    return {"state": state}


def session_outcome(
    *,
    present: bool,
    activity: str | None,
    alive: bool,
    done_at: str | None,
    until: str,
) -> Outcome | None:
    """Fired when the watched session has ended (or, `until="done"`, has
    also declared itself done under autopilot)."""
    if not present:
        return {"state": "gone"}
    if activity in ENDED_ACTIVITIES or not alive:
        return {"state": activity or "exited"}
    if until == "done" and done_at:
        return {"state": "done", "done_at": done_at}
    return None


def notice_text(
    kind: str, trigger: Mapping[str, Any], outcome: Outcome, note: str
) -> str:
    """The one line typed into the woken session. Plain text with every
    control character stripped: forge titles and check names are data, and
    an Esc or carriage return would be a keystroke."""
    line = f"[vogt] watch fired: {describe(kind, trigger)} -> {_verdict(kind, outcome)}"
    if note:
        line += f". {note}"
    return "".join(ch for ch in line if ch.isprintable())[:NOTICE_MAX_CHARS]


def describe(kind: str, trigger: Mapping[str, Any]) -> str:
    """What a watch waits for, in a few words."""
    if kind == KIND_PR:
        return f"PR #{trigger.get('number')} in {trigger.get('project')}"
    if kind == KIND_CI:
        target = (
            f"PR #{trigger['pr']}"
            if trigger.get("pr") is not None
            else str(trigger.get("sha", ""))[:12]
        )
        return f"CI on {target} in {trigger.get('project')}"
    if kind == KIND_WORK_ITEM:
        return f"{trigger.get('ref')} state"
    if kind == KIND_SESSION:
        return f"session {trigger.get('session')}"
    return kind


def _verdict(kind: str, outcome: Outcome) -> str:
    state = str(outcome.get("state", "")).upper()
    if kind == KIND_CI:
        detail = (
            f"; failing: {', '.join(outcome['failing'])}"
            if outcome.get("failing")
            else f"; {len(outcome.get('checks', []))} check(s)"
        )
        url = f" — {outcome['url']}" if outcome.get("url") else ""
        return f"CI {state} @ {str(outcome.get('revision', ''))[:12]}{detail}{url}"
    if kind == KIND_PR:
        title = f" ({outcome['title']})" if outcome.get("title") else ""
        return f"{state}{title}"
    return state


def _same_revision(observed: object, wanted: str) -> bool:
    if not isinstance(observed, str) or not observed or len(wanted) < 7:
        return False
    return observed.startswith(wanted) or wanted.startswith(observed)


def _conclusion(check: Observation) -> str | None:
    conclusion = check.payload.get("conclusion")
    if isinstance(conclusion, str) and conclusion:
        return conclusion
    return None


__all__ = [
    "ENDED_ACTIVITIES",
    "KINDS",
    "KIND_CI",
    "KIND_PR",
    "KIND_SESSION",
    "KIND_WORK_ITEM",
    "PR_EVENTS",
    "ci_outcome",
    "describe",
    "notice_text",
    "pr_outcome",
    "session_outcome",
    "work_item_outcome",
]
