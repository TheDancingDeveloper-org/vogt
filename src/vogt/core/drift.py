"""Drift: where declared state and observation disagree.

Two rules shape everything here (DESIGN §3.2):

**Drift never silently mutates declared data** (FR-R2). It produces a
*proposal* — "the latest tag is 1.5 but this project declares 1.4; update
it?" — and a human or an authorised agent accepts, rejects, or leaves it
contested. Collector failure can therefore never corrupt anything: the worst
case is a wrong question.

**A proposal must never outlive its evidence** (FR-R5). Every proposal
carries a self-contained snapshot taken at raise time, so it still explains
itself after retention has pruned the history around it — and retention
additionally refuses to prune anything a proposal points at. Both, because
the two stores are pruned and restored independently.

Note what is deliberately *not* here: contract violations. They are a
computed status on the project (§5), not drift — there is no declared
counterpart to disagree with, so the proposal lifecycle would be ceremony
around a fact (r2 decision).
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field
from datetime import datetime
from typing import Literal

DriftStatus = Literal["open", "accepted", "rejected", "contested"]

#: Local drift kinds (M3). The forge kinds — `forge_state_mismatch`,
#: `vanished_upstream`, `ci_red_vs_healthy`, `update_automation_gap` —
#: arrive with the GitHub module at M5.
VERSION_MISMATCH = "version_mismatch"
UNRESOLVED_DEPENDENCY = "unresolved_dependency"
BROKEN_PATH_DEPENDENCY = "broken_path_dependency"

#: The shipped default policy: low-risk auto-accept (FR-R3, DESIGN §3.2).
#: State-sync kinds may be accepted by an agent without a human; anything
#: destructive or structural is always human-gated. The distinction is what
#: the acceptance *does*, not how confident the engine is.
AUTO_ACCEPTABLE_KINDS: frozenset[str] = frozenset(
    {VERSION_MISMATCH, "forge_state_mismatch"}
)

#: An unresolved reference is not auto-acceptable, and it is worth saying why:
#: accepting one means asserting that a project nobody registered does not
#: exist, and the usual truth is that somebody has not registered it yet.
HUMAN_GATED_REASON: dict[str, str] = {
    UNRESOLVED_DEPENDENCY: (
        "accepting this asserts the target is not a project; usually it is a "
        "project nobody has registered yet"
    ),
    "vanished_upstream": (
        "accepting this asserts an upstream object is gone for good; a repo "
        "transfer or a permissions change looks identical from here"
    ),
    "ci_red_vs_healthy": (
        "a red build is a fact about the build, not a decision about the "
        "project's lifecycle state — somebody has to say which is wrong"
    ),
    "update_automation_gap": (
        "turning on a security toggle is a change to the repository's "
        "settings, not to Vogt's data; accepting only records the judgement"
    ),
    BROKEN_PATH_DEPENDENCY: (
        "the target is inside this project and is not there; only somebody "
        "who knows whether it moved or was deleted can say what to do"
    ),
    "referenced_issue_state_mismatch": (
        "the reference was read out of the item's own text rather than "
        "adopted as a link, and only somebody who knows which register is "
        "right can say whether to close the issue or reopen the item"
    ),
    "initiative_checkbox_drift": (
        "a checkbox was ticked upstream and the member's workflow state was "
        "not; only somebody who knows which is right can say whether to move "
        "the item or let the next re-render restore the box"
    ),
    "initiative_tracking_close": (
        "the initiative is closed here; whether its tracking issue should be "
        "closed upstream is a person's call — Vogt proposes it, never writes it"
    ),
}


@dataclass(frozen=True)
class EvidenceSnapshot:
    """A self-contained copy of the evidence, as it stood at raise time."""

    subject_key: str
    content_digest: str
    observed_at: datetime
    collector: str
    payload: dict[str, object] = field(default_factory=dict)

    def to_json(self) -> dict[str, object]:
        return {
            "subject_key": self.subject_key,
            "content_digest": self.content_digest,
            "observed_at": self.observed_at.isoformat(),
            "collector": self.collector,
            "payload": self.payload,
        }


@dataclass(frozen=True)
class DriftFinding:
    """One disagreement the engine found, before it becomes a proposal."""

    kind: str
    subject_kind: str
    subject_id: str
    summary: str
    proposed_change: dict[str, object]
    project_id: str | None = None
    evidence_observation_id: str | None = None
    evidence: EvidenceSnapshot | None = None

    @property
    def auto_acceptable(self) -> bool:
        """Whether the shipped default policy lets an agent accept this.

        `forge_state_mismatch` is now auto-acceptable in **both** directions
        (#174). The old one-direction-only rule was about evidence, not risk:
        the open-only `gh-issues` scrape never observed a closure, so the
        newest observation of a closed issue said `open` indefinitely, and
        "upstream is open" was indistinguishable from "closed and never
        re-read" — auto-accepting a reopen would resurrect finished work from
        an absence nobody observed. Phase 2's `state=all` incremental sync
        makes closure a produced fact (`forge-issues`/`forge-prs`), so the
        distinction the asymmetry protected against no longer exists, and a
        reopen upstream is now as observable — and as safe to accept — as a
        close.
        """
        return self.kind in AUTO_ACCEPTABLE_KINDS


def normalise_version(value: str) -> str:
    """Compare versions the way people write them.

    `v1.4.0` and `1.4.0` are the same release; a drift proposal raised over
    a leading `v` is noise that teaches people to ignore drift proposals.
    """
    return value.strip().lstrip("vV")


def version_mismatch(
    *,
    project_id: str,
    project_slug: str,
    declared: str | None,
    observed: str,
    evidence: EvidenceSnapshot,
    evidence_observation_id: str | None,
) -> DriftFinding | None:
    """Declared version vs the newest tag or release actually seen (FR-P3)."""
    if declared is not None and normalise_version(declared) == normalise_version(
        observed
    ):
        return None
    stated = "nothing" if declared is None else repr(declared)
    return DriftFinding(
        kind=VERSION_MISMATCH,
        subject_kind="project",
        subject_id=project_id,
        project_id=project_id,
        summary=(
            f"{project_slug} declares {stated} but the newest observed "
            f"release is {observed!r}"
        ),
        proposed_change={
            "entity": "project",
            "field": "current_version",
            "from": declared,
            "to": observed,
        },
        evidence=evidence,
        evidence_observation_id=evidence_observation_id,
    )


def broken_path_dependency(
    *,
    subject_key: str,
    project_id: str,
    project_slug: str,
    raw_target: str,
    manifest: str | None,
    evidence: EvidenceSnapshot,
    evidence_observation_id: str | None,
) -> DriftFinding:
    """A path reference inside the project's own tree that resolves to nothing.

    Split from `unresolved_dependency` because the two ask different
    questions. An unresolved reference points somewhere Vogt has not been
    told about, and the answer is usually to register it. This one points
    inside the project, where there is nothing to register: the manifest is
    wrong, or the directory moved. Reporting both under one kind is how
    thirty proposals about a valid monorepo layout came to carry the gate
    text "usually it is a project nobody has registered yet".
    """
    where = f" in {manifest}" if manifest else ""
    return DriftFinding(
        kind=BROKEN_PATH_DEPENDENCY,
        subject_kind="dependency",
        subject_id=subject_key,
        project_id=project_id,
        summary=(
            f"{project_slug} references {raw_target!r}{where}, which is inside "
            "this project and does not exist"
        ),
        proposed_change={
            "action": "fix_manifest_or_restore_path",
            "raw_target": raw_target,
            "manifest": manifest,
        },
        evidence=evidence,
        evidence_observation_id=evidence_observation_id,
    )


def unresolved_dependency(
    *,
    subject_key: str,
    project_id: str,
    project_slug: str,
    raw_target: str,
    manifest: str | None,
    evidence: EvidenceSnapshot,
    evidence_observation_id: str | None,
) -> DriftFinding:
    """An internal-looking reference whose target is not a registered project.

    Reported rather than resolved (FR-D5). The reference keeps its raw
    target, and the useful response is usually to register the project it
    points at.
    """
    where = f" in {manifest}" if manifest else ""
    return DriftFinding(
        kind=UNRESOLVED_DEPENDENCY,
        subject_kind="dependency",
        subject_id=subject_key,
        project_id=project_id,
        summary=(
            f"{project_slug} references {raw_target!r}{where}, which is not a "
            "registered project"
        ),
        proposed_change={
            "action": "register_or_ignore",
            "raw_target": raw_target,
            "manifest": manifest,
        },
        evidence=evidence,
        evidence_observation_id=evidence_observation_id,
    )


def _last_observed_open(
    subject_key: str, work_ref: str, declared_state: str, evidence: EvidenceSnapshot
) -> str:
    """The reopen direction, said honestly.

    Once the incremental sync reads `state=all` (#173), an open observation is
    a produced fact, not the shadow a closed-and-unread subject cast under the
    old open-only scrape: `forge-issues`/`forge-prs` re-read every state each
    sweep, so a reopen upstream is observed like any other change and this
    disagreement is real rather than possibly-stale.
    """
    return (
        f"{subject_key} was open when last observed "
        f"({evidence.observed_at.isoformat()}), but {work_ref} is "
        f"{declared_state!r} here — the incremental sync reads all states, so "
        "this is an observed reopen, not a close it failed to see"
    )


# -- forge drift kinds (M5) ------------------------------------------------

FORGE_STATE_MISMATCH = "forge_state_mismatch"
VANISHED_UPSTREAM = "vanished_upstream"
CI_RED_VS_HEALTHY = "ci_red_vs_healthy"
UPDATE_AUTOMATION_GAP = "update_automation_gap"
#: A checkbox on an initiative tracking issue was ticked (or unticked) upstream
#: and no longer matches what the member's workflow state says (#286). The tick
#: is a human's edit inside Vogt's managed region; surfacing it as drift is what
#: keeps the projection from silently overwriting it on the next re-render.
INITIATIVE_CHECKBOX_DRIFT = "initiative_checkbox_drift"
#: Closing an initiative proposes closing its tracking issues; Vogt never
#: writes that close itself (#286 deliverable 4).
INITIATIVE_TRACKING_CLOSE = "initiative_tracking_close"

#: `forge_state_mismatch` joins the auto-acceptable set: it is state-sync,
#: and the change it proposes is one somebody already made upstream.
#: The other three are not.
FORGE_AUTO_ACCEPTABLE: frozenset[str] = frozenset({FORGE_STATE_MISMATCH})

#: The forge kinds that are always human-gated, already listed in
#: HUMAN_GATED_REASON above. Kept as a set for the engine to check against.
FORGE_HUMAN_GATED: frozenset[str] = frozenset(
    {
        VANISHED_UPSTREAM,
        CI_RED_VS_HEALTHY,
        UPDATE_AUTOMATION_GAP,
        INITIATIVE_CHECKBOX_DRIFT,
        INITIATIVE_TRACKING_CLOSE,
    }
)


def forge_state_mismatch(
    *,
    work_item_id: str,
    work_ref: str,
    declared_state: str,
    upstream_state: str,
    subject_key: str,
    project_id: str | None,
    evidence: EvidenceSnapshot,
    evidence_observation_id: str | None,
) -> DriftFinding:
    """A linked issue's state disagrees with the item's (DESIGN §3.2)."""
    return DriftFinding(
        kind=FORGE_STATE_MISMATCH,
        subject_kind="work_item",
        subject_id=work_item_id,
        project_id=project_id,
        summary=(
            f"{subject_key} is closed upstream, but {work_ref} is "
            f"{declared_state!r} here"
            if upstream_state == "closed"
            else _last_observed_open(subject_key, work_ref, declared_state, evidence)
        ),
        proposed_change={
            "entity": "work_item",
            "field": "state",
            "from": declared_state,
            "to": "done" if upstream_state == "closed" else "open",
            "work_ref": work_ref,
        },
        evidence=evidence,
        evidence_observation_id=evidence_observation_id,
    )


def initiative_checkbox_drift(
    *,
    initiative_id: str,
    initiative_slug: str,
    project_id: str | None,
    subject_key: str,
    number: int,
    work_ref: str,
    upstream_checked: bool,
    expected_checked: bool,
    evidence: EvidenceSnapshot,
    evidence_observation_id: str | None,
) -> DriftFinding:
    """A tracking-issue checkbox disagrees with the member's state (#286).

    Human-gated on purpose: the tick is somebody's edit, and only a person can
    say whether the item should move to match it or the box should be restored
    on the next re-render. Vogt neither auto-moves the item nor silently
    re-renders the box away — it raises the disagreement and waits.
    """
    ticked = "ticked" if upstream_checked else "unticked"
    ought = "done/terminal" if expected_checked else "still open"
    return DriftFinding(
        kind=INITIATIVE_CHECKBOX_DRIFT,
        subject_kind="initiative",
        # One proposal per member, not per initiative: two boxes ticked out of
        # step are two questions, and collapsing them by initiative id would
        # hide the second behind the first (the `open_drift_subjects` dedup).
        subject_id=f"{initiative_id}:{number}",
        project_id=project_id,
        summary=(
            f"initiative {initiative_slug!r}: {work_ref} (#{number}) is {ticked} "
            f"on the tracking issue but is {ought} here"
        ),
        proposed_change={
            "entity": "work_item",
            "initiative": initiative_slug,
            "work_ref": work_ref,
            "subject_key": subject_key,
            "number": number,
            "upstream_checked": upstream_checked,
            "expected_checked": expected_checked,
        },
        evidence=evidence,
        evidence_observation_id=evidence_observation_id,
    )


def vanished_upstream(
    *,
    work_item_id: str,
    work_ref: str,
    subject_key: str,
    project_id: str | None,
    swept_at: datetime,
) -> DriftFinding:
    """A linked forge object is absent *within provably swept scope*.

    Coverage-gated on purpose (FR-O4). Absence outside a completed sweep is
    "not collected", and most of cadastre's "missing" drift turned out to be
    exactly that mistake.
    """
    return DriftFinding(
        kind=VANISHED_UPSTREAM,
        subject_kind="work_item",
        subject_id=work_item_id,
        project_id=project_id,
        summary=(
            f"{work_ref} links {subject_key}, which a completed sweep at "
            f"{swept_at.isoformat()} did not find"
        ),
        proposed_change={
            "entity": "work_link",
            "action": "review",
            "subject_key": subject_key,
            "work_ref": work_ref,
        },
        # No observation to point at: the finding *is* the absence. The
        # snapshot instead records the sweep that establishes coverage.
        evidence=EvidenceSnapshot(
            subject_key=subject_key,
            content_digest="",
            observed_at=swept_at,
            collector="forge-issues",
            payload={"absent_in_completed_sweep": True},
        ),
        evidence_observation_id=None,
    )


def ci_red_vs_healthy(
    *,
    project_id: str,
    project_slug: str,
    lifecycle_state: str,
    failing: list[str],
    revision: str,
    evidence: EvidenceSnapshot,
    evidence_observation_id: str | None,
) -> DriftFinding:
    """CI is red on the default branch while the project claims to be fine."""
    return DriftFinding(
        kind=CI_RED_VS_HEALTHY,
        subject_kind="project",
        subject_id=project_id,
        project_id=project_id,
        summary=(
            f"{project_slug} is {lifecycle_state!r} but {len(failing)} check(s) "
            f"failed on {revision[:12]}: {', '.join(sorted(failing))}"
        ),
        proposed_change={
            "entity": "project",
            "action": "review",
            "failing": sorted(failing),
            "revision": revision,
        },
        evidence=evidence,
        evidence_observation_id=evidence_observation_id,
    )


REFERENCED_ISSUE_MISMATCH = "referenced_issue_state_mismatch"

#: `owner/name#123` — an unambiguous cross-repository reference.
_QUALIFIED_REF = re.compile(
    r"\b(?P<owner>[A-Za-z0-9][\w.-]*)/(?P<repo>[A-Za-z0-9][\w.-]*)#(?P<number>\d+)\b"
)

#: A full issue URL. `/pull/` is deliberately not matched: a pull request is
#: not the issue, and `WI-16`'s body names both.
_ISSUE_URL = re.compile(
    r"https?://(?:www\.)?github\.com/"
    r"(?P<owner>[A-Za-z0-9][\w.-]*)/(?P<repo>[A-Za-z0-9][\w.-]*)/issues/"
    r"(?P<number>\d+)"
)


def issue_references(text: str) -> list[str]:
    """Forge subject keys a work item's own text names (FR-R7).

    **Bare `#44` is deliberately not matched.** It is the commonest way to
    write a reference and the least decidable one: `WI-16`'s own title says
    "Regression from #43", and #43 is a pull request, while the issue it
    actually mirrors is named in the body as a full URL. Resolving `#n`
    against the item's project would have produced a proposal about a PR from
    a sentence describing history. A qualified reference or a URL is somebody
    naming a specific issue in a specific repository, which is what this is
    allowed to act on.

    Returns subject keys in the `gh:owner/repo#number` form the forge
    collectors already use, deduplicated, in first-seen order.
    """
    keys: list[str] = []
    for pattern in (_ISSUE_URL, _QUALIFIED_REF):
        for match in pattern.finditer(text):
            repo = match.group("repo").removesuffix(".git")
            key = f"gh:{match.group('owner')}/{repo}#{match.group('number')}"
            if key not in keys:
                keys.append(key)
    return keys


def referenced_issue_state_mismatch(
    *,
    work_item_id: str,
    work_ref: str,
    declared_state: str,
    upstream_state: str,
    subject_key: str,
    project_id: str | None,
    evidence: EvidenceSnapshot,
    evidence_observation_id: str | None,
) -> DriftFinding:
    """A work item and the issue its own text names disagree (FR-R7).

    Structurally the same disagreement as `forge_state_mismatch` and a
    different kind on purpose: that one is about a link somebody *adopted*,
    which is a deliberate act, and it may sync state automatically. This one
    is about a reference read out of an item's title or body, which is a
    weaker claim — so it proposes nothing, applies nothing, and is always
    human-gated.

    `WI-16` mirrored issue `#44`, named it in the first line of its body, was
    marked done when the fix deployed, and left `#44` open on GitHub for
    hours. Nothing inside Vogt noticed; an onboarding agent following the
    import playbook did, and refused to proceed (#49).
    """
    finished_here = "finished" if declared_state in ("done", "wont_do") else "open"
    return DriftFinding(
        kind=REFERENCED_ISSUE_MISMATCH,
        subject_kind="work_item",
        subject_id=work_item_id,
        project_id=project_id,
        summary=(
            f"{work_ref} references {subject_key}, which was {upstream_state} "
            f"when last observed ({evidence.observed_at.isoformat()}), while "
            f"{work_ref} is {declared_state!r} — {finished_here} here, "
            f"{upstream_state} there"
        ),
        proposed_change={
            "entity": "work_item",
            "action": "review",
            "subject_key": subject_key,
            "work_ref": work_ref,
            "declared_state": declared_state,
            "upstream_state": upstream_state,
        },
        evidence=evidence,
        evidence_observation_id=evidence_observation_id,
    )


def update_automation_gap(
    *,
    project_id: str,
    project_slug: str,
    missing: list[str],
    evidence: EvidenceSnapshot,
    evidence_observation_id: str | None,
) -> DriftFinding:
    """One of the three automation facts is off (FR-D6).

    Names *which*, because "automation is incomplete" is not actionable and
    the three toggles are set in three different places.
    """
    return DriftFinding(
        kind=UPDATE_AUTOMATION_GAP,
        subject_kind="project",
        subject_id=project_id,
        project_id=project_id,
        summary=(
            f"{project_slug} is missing {len(missing)} of the three update "
            f"automation facts: {', '.join(sorted(missing))}"
        ),
        proposed_change={
            "entity": "repository_settings",
            "action": "enable",
            "missing": sorted(missing),
        },
        evidence=evidence,
        evidence_observation_id=evidence_observation_id,
    )
