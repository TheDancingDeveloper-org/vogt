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
AUTO_ACCEPTABLE_KINDS: frozenset[str] = frozenset({VERSION_MISMATCH})

#: An unresolved reference is not auto-acceptable, and it is worth saying why:
#: accepting one means asserting that a project nobody registered does not
#: exist, and the usual truth is that somebody has not registered it yet.
HUMAN_GATED_REASON: dict[str, str] = {
    UNRESOLVED_DEPENDENCY: (
        "accepting this asserts the target is not a project; usually it is a "
        "project nobody has registered yet"
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
