"""Deterministic ranking, and the explanation that comes with it.

FR-V2/V3 and DESIGN §3.4: ordering is computed from constant, documented
weights, and `why <item>` returns the per-input contributions. No ML, no
hidden state, and — decided 2026-08-12 — no `rank_order` column, so nothing
hand-set competes with the score. `priority` and initiative weight are the
hand-set inputs, and they are scored inputs like any other.

Every input below is a pure function of data the caller can also see, which
is what makes `why` an explanation rather than a summary.
"""

from __future__ import annotations

from dataclasses import dataclass
from datetime import datetime

from vogt.core.entities import Priority, TrustState, WorkItem

#: Points per priority band. The gap between p0 and p1 is deliberately larger
#: than between the rest: p0 should outrank a large amount of accumulated
#: staleness, and p3/p4 should not fight each other.
PRIORITY_POINTS: dict[Priority, float] = {
    "p0": 100.0,
    "p1": 55.0,
    "p2": 25.0,
    "p3": 8.0,
    "p4": 0.0,
}

#: Points per day since the item last changed, capped, so that an ignored p2
#: eventually rises past a fresh p3 but never past a p0.
STALENESS_POINTS_PER_DAY = 0.5
STALENESS_CAP_DAYS = 60

#: Points per item that declares `depends_on` this one. Unblocking work is
#: worth more than doing work, which is the whole reason the edge is typed.
BLOCKING_FAN_OUT_POINTS = 8.0

#: Initiative weight is 0–100 and contributes a fraction of itself, so a
#: heavily weighted initiative lifts its items about one priority band.
INITIATIVE_WEIGHT_FACTOR = 0.25

#: Penalty for items whose declaration nothing has confirmed. Zero until M2
#: gives trust states something to be computed from, and negative-only: trust
#: never *promotes* an item, it only declines to vouch for one.
TRUST_PENALTY: dict[TrustState, float] = {
    "verified": 0.0,
    "stale": -2.0,
    "unverified": -3.0,
    "disputed": -8.0,
}

#: Terminal work does not appear in ranked views at all; this exists so that
#: a caller who asks `why` about a done item gets an honest answer.
TERMINAL_PENALTY = -1000.0


#: Scores are rounded before they leave the scorer. Staleness is a function
#: of wall-clock age, so an unrounded contribution carries float noise like
#: `1.1574074074074073e-05` — which is not information, reads as spurious
#: precision in `why`, and makes two reads of the same item look different.
#: Four places is well below any ordering the weights can express.
SCORE_PRECISION = 4


@dataclass(frozen=True)
class Contribution:
    """One scored input, with the evidence that produced it."""

    input: str
    detail: str
    value: float
    weight: float
    contribution: float

    @classmethod
    def of(
        cls,
        *,
        input: str,
        detail: str,
        value: float,
        weight: float,
        contribution: float,
    ) -> Contribution:
        return cls(
            input=input,
            detail=detail,
            value=round(value, SCORE_PRECISION),
            weight=weight,
            contribution=round(contribution, SCORE_PRECISION),
        )


@dataclass(frozen=True)
class Score:
    """A work item's score and the full derivation of it."""

    work_item_id: str
    ref: str
    total: float
    contributions: tuple[Contribution, ...]


@dataclass(frozen=True)
class RankingInputs:
    """Everything outside the work item that the score depends on.

    Passed in rather than fetched, so scoring stays a pure function and the
    ranked views can compute their inputs once for the whole set.
    """

    now: datetime
    blocking_fan_out: int = 0
    initiative_weight: int = 0
    is_terminal: bool = False


def score_item(item: WorkItem, inputs: RankingInputs) -> Score:
    """Score one item and record how each input contributed."""
    contributions: list[Contribution] = []

    priority_points = PRIORITY_POINTS[item.priority]
    contributions.append(
        Contribution.of(
            input="priority",
            detail=item.priority,
            value=1.0,
            weight=priority_points,
            contribution=priority_points,
        )
    )

    age_days = max(0.0, (inputs.now - item.updated_at).total_seconds() / 86_400.0)
    capped_days = min(age_days, float(STALENESS_CAP_DAYS))
    staleness = capped_days * STALENESS_POINTS_PER_DAY
    contributions.append(
        Contribution.of(
            input="staleness",
            detail=(
                f"{age_days:.1f} days since last change"
                + (
                    f", capped at {STALENESS_CAP_DAYS}"
                    if age_days > capped_days
                    else ""
                )
            ),
            value=round(capped_days, 3),
            weight=STALENESS_POINTS_PER_DAY,
            contribution=staleness,
        )
    )

    fan_out = float(inputs.blocking_fan_out) * BLOCKING_FAN_OUT_POINTS
    contributions.append(
        Contribution.of(
            input="blocking_fan_out",
            detail=f"{inputs.blocking_fan_out} item(s) depend on this one",
            value=float(inputs.blocking_fan_out),
            weight=BLOCKING_FAN_OUT_POINTS,
            contribution=fan_out,
        )
    )

    initiative = float(inputs.initiative_weight) * INITIATIVE_WEIGHT_FACTOR
    contributions.append(
        Contribution.of(
            input="initiative_weight",
            detail=(
                f"initiative weight {inputs.initiative_weight}"
                if item.initiative_id
                else "no initiative"
            ),
            value=float(inputs.initiative_weight),
            weight=INITIATIVE_WEIGHT_FACTOR,
            contribution=initiative,
        )
    )

    trust = TRUST_PENALTY[item.trust_state]
    contributions.append(
        Contribution.of(
            input="trust_penalty",
            detail=f"trust state {item.trust_state}",
            value=1.0,
            weight=trust,
            contribution=trust,
        )
    )

    if inputs.is_terminal:
        contributions.append(
            Contribution.of(
                input="terminal_state",
                detail=f"state {item.state} is terminal; excluded from ranked views",
                value=1.0,
                weight=TERMINAL_PENALTY,
                contribution=TERMINAL_PENALTY,
            )
        )

    total = sum(entry.contribution for entry in contributions)
    return Score(
        work_item_id=item.id,
        ref=item.ref,
        total=round(total, SCORE_PRECISION),
        contributions=tuple(contributions),
    )


def rank(scores: list[Score]) -> list[Score]:
    """Order by score, breaking ties by ref so the order is total.

    A stable, total order matters more than it looks: two runs of `backlog`
    that disagree about equal-scoring items read as a system that changed its
    mind.
    """
    return sorted(scores, key=lambda score: (-score.total, score.ref))


#: Inputs documented in DESIGN §3.4 that cannot fire yet, recorded so that
#: `why`'s input list is honest about what it is *not* considering rather
#: than silently omitting it.
PENDING_INPUTS: tuple[tuple[str, str], ...] = (
    (
        "ci_red_boost",
        "arrives at M2 with the CI check observations it reads (FR-O6)",
    ),
)
