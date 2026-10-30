"""Where a caller's task contradicts the project's landing rule (WI-1832).

A task is free text written by oversight or a person; the landing rule is the
project's recorded policy. When the two disagree on a known axis — open a PR
or push only, who merges, which base, who deploys — a worker that reads both
stops to ask. The policy wins, and the brief says so in as many words, so the
worker has one instruction instead of two.

Detection is a reading of the text, not a parse: each axis is a small set of
phrasings, and a match preceded by a negation ("open no PR", "never merge")
is agreement, not conflict. A miss leaves the brief as it was; a false hit
adds a line restating the policy the brief already carries.
"""

from __future__ import annotations

import re

from vogt.core.entities import LandingPolicy

#: Words that, just before a phrase, turn an instruction into its opposite.
_NEGATION = re.compile(
    r"\b(?:no|not|never|don'?t|do\s+not|without|nor|instead\s+of)\b[^.;\n]{0,24}$",
    re.IGNORECASE,
)

_OPEN_PR = re.compile(
    r"\b(?:open|create|raise|file|submit|send)\s+(?:a\s+|an\s+|the\s+)?"
    r"(?:draft\s+)?(?:PR|pull[\s-]request)\b|\bgh\s+pr\s+create\b",
    re.IGNORECASE,
)
_PUSH_ONLY = re.compile(
    r"\b(?:open|create|raise)\s+no\s+(?:PR|pull[\s-]request)\b"
    r"|\bpush(?:\s+the)?(?:\s+branch)?\s+only\b"
    r"|\bno\s+(?:PR|pull[\s-]request)\b",
    re.IGNORECASE,
)
_PUSH_BRANCH = re.compile(r"\bpush(?:\s+the)?(?:\s+branch)?\s+only\b", re.IGNORECASE)
_MERGE = re.compile(
    r"\bgh\s+pr\s+merge\b"
    r"|\bmerge\s+(?:it|this|the\s+(?:PR|pull[\s-]request|branch|change))\b"
    r"|\bmerge\s+(?:it\s+)?(?:to|into|onto)\s+[`'\"]?[\w./-]+"
    r"|\band\s+merge\b|\bthen\s+merge\b|\bself[\s-]merge\b",
    re.IGNORECASE,
)
_DEPLOY = re.compile(
    r"\b(?:deploy|ship|roll\s+out)\s+(?:it|this|the\s+\w+|to\s+[`'\"]?[\w./-]+)"
    r"|\band\s+deploy\b|\bthen\s+deploy\b",
    re.IGNORECASE,
)
_BASE = re.compile(
    r"\b(?:PR|pull[\s-]request|branch|rebase|base(?:d)?)\s+"
    r"(?:against|onto|into|off(?:\s+of)?|from|on)\s+"
    r"[`'\"]?(?:origin/)?(?P<branch>[A-Za-z0-9][\w./-]*[\w])[`'\"]?",
    re.IGNORECASE,
)

#: Who may deploy without the policy being contradicted.
_SELF_DEPLOYERS = frozenset({"worker", "agent", "session"})


def _asserted(pattern: re.Pattern[str], task: str) -> list[re.Match[str]]:
    """Every match of `pattern` that is not negated by the words before it."""
    hits = []
    for match in pattern.finditer(task):
        before = task[max(0, match.start() - 40) : match.start()]
        if not _NEGATION.search(before):
            hits.append(match)
    return hits


def task_named_base(task: str, landing: LandingPolicy | None) -> str | None:
    """The known long-lived branch `task` names as its base, if exactly one.

    A task that says "branch from `rust-core`" on a project that lists
    `rust-core` among its long-lived branches means it (WI-1841): it is the
    base, not a contradiction. An unknown branch, or two known ones, is not
    taken — the first is the conflict check's to report, the second is
    ambiguous.
    """
    if landing is None or not task.strip():
        return None
    known = landing.known_bases()
    named = {
        match.group("branch")
        for match in _asserted(_BASE, task)
        if match.group("branch") in known
    }
    return next(iter(named)) if len(named) == 1 else None


def task_policy_conflicts(
    task: str, landing: LandingPolicy | None, *, caller_merges: bool = False
) -> list[str]:
    """The override lines for each axis where `task` contradicts `landing`.

    Empty when there is no task, no landing rule, or no conflict. Each line
    names the axis, what the task asked, and what the policy requires.
    `caller_merges` is an oversight session handing out its own landing work
    on a project where oversight merges: a merge there is the policy, not
    against it (WI-1841).
    """
    if landing is None or not task.strip():
        return []
    lines: list[str] = []
    base = landing.base_branch
    if landing.worker_lands_by == "push_branch" and _asserted(_OPEN_PR, task):
        lines.append(
            "Policy overrides task: push only. The task asks for a pull "
            "request; this project lands by pushing the branch. Open NO PR."
        )
    if landing.worker_lands_by == "open_pr" and _PUSH_ONLY.search(task):
        lines.append(
            "Policy overrides task: open a PR. The task says push only; this "
            f"project lands by opening a pull request against `{base}`."
        )
    if landing.graft is not None and (
        _asserted(_OPEN_PR, task) or _PUSH_BRANCH.search(task)
    ):
        lines.append(
            "Policy overrides task: land through graft. The task asks for a "
            "push or a pull request; this project lands on graft line "
            f"`{landing.graft.line}`. No git push, no PR."
        )
    if not caller_merges and _asserted(_MERGE, task):
        lines.append(
            "Policy overrides task: do not merge. The task asks for a merge; "
            f"on this project {landing.merge_by} merges ({landing.merge_mode})."
        )
    if landing.deploy_by not in _SELF_DEPLOYERS and _asserted(_DEPLOY, task):
        lines.append(
            "Policy overrides task: do not deploy. The task asks for a "
            f"deploy; on this project {landing.deploy_by} deploys."
        )
    others = sorted(
        {
            match.group("branch")
            for match in _asserted(_BASE, task)
            if match.group("branch") != base
        }
    )
    if others:
        named = ", ".join(f"`{branch}`" for branch in others)
        lines.append(
            f"Policy overrides task: base `{base}`. The task names {named}; "
            f"this project's workers cut from and land onto `{base}`."
        )
    return lines


def overrides_section(lines: list[str]) -> str:
    """The brief section that states the overrides, or "" when there are none."""
    if not lines:
        return ""
    rows = "\n".join(f"- {line}" for line in lines)
    return (
        "## Policy overrides task\n\n"
        "The task below contradicts this project's landing rule. The policy "
        "wins; follow it and do not stop to ask.\n\n"
        f"{rows}\n"
    )
