"""Rendering an initiative as a forge tracking issue with a task list (#286).

An initiative is vogt-local by decision (#178 dec.3: no milestones, no Projects
v2), which makes an epic invisible to anyone with only the forge open. The
supported workaround is a *tracking issue*: one issue per forge-linked repo the
initiative spans, carrying a checkbox task list of its member work items. This
module is the pure half of that projection — the string shapes and nothing
else, so the rules can be pinned by test without a forge in the loop.

## The managed region is the whole contract

Vogt owns exactly one span of the issue body: the block between
``MANAGED_START`` and ``MANAGED_END``. Everything a human writes *outside* that
span is theirs and survives every re-render (`splice_managed_region`), and the
span itself is re-rendered wholesale each time. That single boundary is what
makes the projection additive and forward-only at the same time: Vogt never
edits a human's prose, and a human never has to fight Vogt's task list.

A hidden per-initiative marker (`marker_for`) lives inside the region so the
publish verb can recognise its own tracking issue on a later run and *adopt*
it — update in place — rather than open a second one.
"""

from __future__ import annotations

import re
from dataclasses import dataclass

from vogt.core.workflow import TERMINAL_STATES

#: The two markers that fence the region Vogt re-renders. Chosen as HTML
#: comments so they are invisible in a rendered issue body but unambiguous in
#: the raw markdown the splice reads back.
MANAGED_START = "<!-- vogt:initiative:start -->"
MANAGED_END = "<!-- vogt:initiative:end -->"

#: What the task list says when an initiative has no forge-numbered members
#: yet — an empty span would read as a rendering bug, not an empty backlog.
EMPTY_TASK_LIST = "_No linked work items yet._"

_CHECKBOX_RE = re.compile(r"^\s*- \[([ xX])\]\s+#(\d+)\b", re.MULTILINE)


def marker_for(slug: str) -> str:
    """The hidden marker that identifies *this* initiative's tracking issue.

    Placed inside the managed region, so `body_has_marker` can pick the issue
    Vogt owns out of a repository full of issues (adopt-by-marker) without a
    label lookup or a stored issue number.
    """
    return f"<!-- vogt:initiative:{slug} -->"


@dataclass(frozen=True)
class TaskLine:
    """One member work item, as it appears in the task list."""

    number: int
    title: str
    #: True when the member is in a terminal workflow state (done / won't-do).
    #: Mirrored from the workflow, never toggled by hand — a human tick upstream
    #: is drift, not truth (#286, FR-O2).
    checked: bool

    @classmethod
    def from_state(cls, *, number: int, title: str, state: str) -> TaskLine:
        return cls(number=number, title=title, checked=state in TERMINAL_STATES)


def render_task_line(line: TaskLine) -> str:
    """`- [ ] #12 Title`, or `- [x] …` when the member is terminal."""
    box = "x" if line.checked else " "
    return f"- [{box}] #{line.number} {line.title}".rstrip()


def render_task_list(lines: list[TaskLine]) -> str:
    """The whole checkbox list, or a placeholder when there is nothing to list."""
    if not lines:
        return EMPTY_TASK_LIST
    return "\n".join(render_task_line(line) for line in lines)


def render_managed_region(
    *,
    slug: str,
    body: str,
    tasks: list[TaskLine],
    siblings: list[tuple[str, str]] = [],  # noqa: B006 (read-only default)
) -> str:
    """Build the block Vogt owns: marker, the initiative prose, the task list.

    `siblings` is the cross-project link set (#286 deliverable 6): the other
    repos' tracking issues, as ``(project_slug, url)`` pairs, so a reader on any
    one repo can reach the rest. It is rendered only when non-empty, so a
    single-repo initiative carries no dangling "tracked across" heading.
    """
    parts = [MANAGED_START, marker_for(slug), ""]
    trimmed = body.strip()
    if trimmed:
        parts.extend([trimmed, ""])
    parts.extend(["### Work items", "", render_task_list(tasks)])
    if siblings:
        parts.extend(["", "### Tracked across other repositories", ""])
        parts.extend(f"- {sib_slug}: {url}" for sib_slug, url in siblings)
    parts.extend(["", MANAGED_END])
    return "\n".join(parts)


def splice_managed_region(existing: str | None, region: str) -> str:
    """Replace the managed region in `existing`, preserving everything else.

    The forward-only guarantee in one function: text a human wrote before
    ``MANAGED_START`` or after ``MANAGED_END`` is copied through untouched, and
    only the span between the markers is rewritten. When the markers are absent
    — a freshly created issue, or one a human has not seeded — the region is
    appended after whatever body is already there.
    """
    if existing is None or MANAGED_START not in existing or MANAGED_END not in existing:
        base = (existing or "").rstrip()
        return f"{base}\n\n{region}" if base else region
    start = existing.index(MANAGED_START)
    end = existing.index(MANAGED_END) + len(MANAGED_END)
    return existing[:start] + region + existing[end:]


def body_has_marker(body: str | None, slug: str) -> bool:
    """Whether `body` is the tracking issue for the initiative named `slug`."""
    return body is not None and marker_for(slug) in body


def _managed_span(body: str) -> str:
    """The text between the markers, or `""` when the region is not present."""
    if MANAGED_START not in body or MANAGED_END not in body:
        return ""
    start = body.index(MANAGED_START) + len(MANAGED_START)
    end = body.index(MANAGED_END)
    return body[start:end]


def parse_checkbox_states(body: str) -> dict[int, bool]:
    """Read the ``#<n> → checked?`` map back out of a tracking issue body.

    Only the managed region is read, so a checkbox a human wrote in their own
    prose above the markers can never be mistaken for a member's state. This is
    what lets a sweep tell a human's upstream tick apart from what Vogt last
    rendered, and surface the disagreement as drift (#286 deliverable 5).
    """
    span = _managed_span(body)
    if not span:
        return {}
    return {
        int(number): box.lower() == "x" for box, number in _CHECKBOX_RE.findall(span)
    }


__all__ = [
    "EMPTY_TASK_LIST",
    "MANAGED_END",
    "MANAGED_START",
    "TaskLine",
    "body_has_marker",
    "marker_for",
    "parse_checkbox_states",
    "render_managed_region",
    "render_task_line",
    "render_task_list",
    "splice_managed_region",
]
