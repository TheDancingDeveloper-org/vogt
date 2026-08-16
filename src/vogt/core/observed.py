"""Reading observed subjects as work, without pretending they are declared.

Observed-first is the product's best idea and its biggest hazard (DESIGN
§3.6). A collected subject appears in ranked views immediately, but it has no
priority, no state and no assignee — nobody typed it in. This module holds
the guesses that fill those gaps, in one place, documented, so that they are
reviewable rather than scattered.

Every guess here is *best effort and correctable*: `adopt` promotes a subject
into a real work item where the guess can be overridden, and `suppress`
removes it from ranked views entirely. The guesses only have to be good
enough to order a list.
"""

from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime

from vogt.core.entities import Observation, Priority, TrustState, WorkItem, WorkKind

#: Observation kinds that represent work somebody might do. Everything else
#: a collector finds — checkouts, releases, dependency references, CI checks
#: — is context, not backlog, and never enters a ranked view.
WORKLIKE_KINDS: frozenset[str] = frozenset(
    {"forge.issue", "forge.pull_request", "marker"}
)

#: Labels that make a forge issue a bug. Matched case-insensitively.
BUG_LABELS: frozenset[str] = frozenset({"bug", "defect", "regression", "crash"})

#: Marker tags that read as defects rather than as chores.
BUG_TAGS: frozenset[str] = frozenset({"FIXME", "HACK", "XXX"})

DEFAULT_OBSERVED_PRIORITY: Priority = "p2"
MARKER_PRIORITY: Priority = "p3"

#: The state an observed subject is shown in. Not a workflow state: nothing
#: has transitioned it, and giving it `open` would imply it obeys a machine
#: it has never been through.
OBSERVED_STATE = "observed"

#: Lifecycle of an observed forge subject, read from the source rather than
#: guessed. Held separately from `OBSERVED_STATE` above, which deliberately
#: asserts no workflow state: "is this still outstanding" and "where is it in
#: a process" are different questions, and only the source can answer the
#: first.
LIFECYCLE_OPEN = "open"
LIFECYCLE_CLOSED = "closed"
LIFECYCLE_UNKNOWN = "unknown"


@dataclass(frozen=True)
class Rankable:
    """The minimum the scorer needs, from either store.

    Declared work items and observed subjects are ranked by the same
    function against the same weights — which is the point of observed-first.
    A separate scoring path for observed work would drift from the declared
    one and make the two orderings incomparable.
    """

    id: str
    ref: str
    priority: Priority
    updated_at: datetime
    trust_state: TrustState
    state: str
    has_initiative: bool = False

    @classmethod
    def from_work_item(cls, item: WorkItem) -> Rankable:
        return cls(
            id=item.id,
            ref=item.ref,
            priority=item.priority,
            updated_at=item.updated_at,
            trust_state=item.trust_state,
            state=item.state,
            has_initiative=item.initiative_id is not None,
        )


def work_kind_of(observation: Observation) -> WorkKind:
    """Map an observed subject onto a work kind, best effort.

    A wrong guess costs one row in the wrong filter and is fixed by adopting
    the subject. Guessing nothing would cost the bug view its whole observed
    half, which is the feature.
    """
    if observation.kind == "marker":
        tag = str(observation.payload.get("tag", "")).upper()
        return "bug" if tag in BUG_TAGS else "chore"
    if observation.kind == "forge.pull_request":
        # Work in flight: not a defect, not a new request. `chore` keeps it
        # out of the bug view while leaving it in the backlog.
        return "chore"
    labels = {str(label).lower() for label in _labels(observation)}
    return "bug" if labels & BUG_LABELS else "feature"


def lifecycle_of(observation: Observation) -> str:
    """Whether the source says this subject is still outstanding.

    `unknown` where the source did not say, and it must stay distinguishable
    from the other two: a subject nobody could read is not thereby open, and
    `bugs` treating it as such is how twenty-seven issues closed weeks earlier
    came to be listed as the estate's open defects, every one of them stamped
    `trust_state: verified`.

    Markers have no lifecycle — a TODO is in the source or it is not, and the
    sweep that stopped finding it is what closes it.
    """
    if observation.kind == "marker":
        return LIFECYCLE_OPEN
    state = observation.payload.get("state")
    if not isinstance(state, str):
        return LIFECYCLE_UNKNOWN
    normalised = state.strip().lower()
    if normalised in (LIFECYCLE_OPEN, LIFECYCLE_CLOSED):
        return normalised
    if normalised == "merged":
        return LIFECYCLE_CLOSED
    return LIFECYCLE_UNKNOWN


def is_classified(observation: Observation) -> bool:
    """Whether anything actually said what kind of work this is.

    `work_kind_of` has to return something, and for an unlabelled issue that
    something is a guess. Recording that it was a guess is what keeps a
    subject discoverable when it falls into no view: three of the estate's
    genuinely open issues carry no labels at all, and were absent from the bug
    view for that reason rather than because anyone judged them not to be bugs.
    """
    if observation.kind == "marker":
        return True
    if observation.kind == "forge.pull_request":
        return True
    return bool(_labels(observation))


def priority_of(observation: Observation) -> Priority:
    """Derive a priority so an observed subject can be ordered at all.

    An explicit `p0`–`p4` label wins, because somebody said it. Otherwise
    markers sit one band below issues: a marker is a note to self, an issue
    is something a person filed.
    """
    for label in _labels(observation):
        candidate = str(label).strip().lower()
        if candidate in ("p0", "p1", "p2", "p3", "p4"):
            return candidate  # type: ignore[return-value]
    if observation.kind == "marker":
        return MARKER_PRIORITY
    return DEFAULT_OBSERVED_PRIORITY


def title_of(observation: Observation) -> str:
    """A one-line description, whatever kind of subject this is."""
    payload = observation.payload
    if observation.kind == "marker":
        text = str(payload.get("text", "")).strip()
        location = f"{payload.get('path', '?')}:{payload.get('line', '?')}"
        marker = f"{payload.get('tag', 'TODO')} {location}"
        return f"{marker} — {text}" if text else marker
    title = str(payload.get("title", "")).strip()
    number = payload.get("number")
    if title and number is not None:
        return f"#{number} {title}"
    return title or observation.subject_key


def is_worklike(observation: Observation) -> bool:
    """Whether this subject belongs in a ranked view at all.

    Promotion is applied here for markers (FR-W11): an unpromoted marker is
    still observed, still queryable and still counted — it just does not
    claim to be work.
    """
    if observation.kind not in WORKLIKE_KINDS:
        return False
    if observation.kind == "marker":
        return observation.promoted
    return True


def _labels(observation: Observation) -> list[object]:
    labels = observation.payload.get("labels")
    return list(labels) if isinstance(labels, list) else []
