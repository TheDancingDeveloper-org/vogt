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

from dataclasses import dataclass, field
from datetime import datetime
from typing import Literal

DriftStatus = Literal["open", "accepted", "rejected", "contested"]

#: Local drift kinds (M3). The forge kinds — `forge_state_mismatch`,
#: `vanished_upstream`, `ci_red_vs_healthy`, `update_automation_gap` —
#: arrive with the GitHub module at M5.
VERSION_MISMATCH = "version_mismatch"
UNRESOLVED_DEPENDENCY = "unresolved_dependency"

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
        """Whether the shipped default policy lets an agent accept this."""
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


# -- forge drift kinds (M5) ------------------------------------------------

FORGE_STATE_MISMATCH = "forge_state_mismatch"
VANISHED_UPSTREAM = "vanished_upstream"
CI_RED_VS_HEALTHY = "ci_red_vs_healthy"
UPDATE_AUTOMATION_GAP = "update_automation_gap"

#: `forge_state_mismatch` joins the auto-acceptable set: it is state-sync,
#: and the change it proposes is one somebody already made upstream.
#: The other three are not.
FORGE_AUTO_ACCEPTABLE: frozenset[str] = frozenset({FORGE_STATE_MISMATCH})

#: The forge kinds that are always human-gated, already listed in
#: HUMAN_GATED_REASON above. Kept as a set for the engine to check against.
FORGE_HUMAN_GATED: frozenset[str] = frozenset(
    {VANISHED_UPSTREAM, CI_RED_VS_HEALTHY, UPDATE_AUTOMATION_GAP}
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
            f"{subject_key} is {upstream_state} upstream, but {work_ref} is "
            f"{declared_state!r} here"
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
            collector="gh-issues",
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
