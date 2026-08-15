"""Ranked views over both stores (FR-V1–V3, FR-W4).

Observed-first means declared work and collected subjects appear in one list,
ordered by one set of weights. Three mechanisms stand between the evidence
and the ranked view, and all three are required before observed-first is safe
to switch on (DESIGN §3.6):

1. **Promotion by convention** — only markers matching a configured pattern
   claim to be work. Applied in `core/observed.py`.
2. **Suppression** — an audited decision, keyed on a subject or a pattern,
   that survives re-observation. Applied here.
3. **Per-project exclusions** — applied before collection, so excluded paths
   never become observations at all.

Adopted subjects are folded into their declared work item rather than listed
twice: adopting is how an observed subject becomes real, not how it gets a
duplicate.
"""

from __future__ import annotations

import fnmatch
from collections.abc import Sequence
from dataclasses import dataclass
from datetime import datetime

from vogt.application.context import AppContext
from vogt.application.models import (
    BacklogParams,
    BacklogResult,
    BugsParams,
    ContributionView,
    Freshness,
    RankedItem,
    WhyParams,
    WhyResult,
)
from vogt.application.services import _resolve
from vogt.core.entities import Observation, Project, Suppression, WorkItem
from vogt.core.observed import (
    OBSERVED_STATE,
    Rankable,
    is_worklike,
    priority_of,
    title_of,
    work_kind_of,
)
from vogt.core.ranking import PENDING_INPUTS, RankingInputs, rank, score_item
from vogt.core.workflow import TERMINAL_STATES
from vogt.errors import NotFound
from vogt.storage.interface import ReadView, WorkFilter

#: Ranked views read a wide slice and score it in the application layer,
#: because the score is a documented function of several inputs rather than
#: something SQL should be asked to express. The cap keeps that honest: at
#: the NFR-S1 envelope this is the query the benchmark fixture watches.
RANKING_CANDIDATE_LIMIT = 1000


# -- suppression -----------------------------------------------------------


@dataclass(frozen=True)
class SuppressionFilter:
    """The live suppressions, ready to test subjects against."""

    exact: frozenset[str]
    patterns: tuple[tuple[str, str | None], ...]
    scoped_exact: frozenset[tuple[str, str]]

    @classmethod
    def build(cls, suppressions: Sequence[Suppression]) -> SuppressionFilter:
        exact: set[str] = set()
        scoped: set[tuple[str, str]] = set()
        patterns: list[tuple[str, str | None]] = []
        for entry in suppressions:
            if not entry.active:
                continue
            if entry.match_kind == "pattern":
                patterns.append((entry.subject_key_or_pattern, entry.scope_project_id))
            elif entry.scope_project_id:
                scoped.add((entry.scope_project_id, entry.subject_key_or_pattern))
            else:
                exact.add(entry.subject_key_or_pattern)
        return cls(
            exact=frozenset(exact),
            patterns=tuple(patterns),
            scoped_exact=frozenset(scoped),
        )

    def hides(self, subject_key: str, project_id: str | None) -> bool:
        if subject_key in self.exact:
            return True
        if project_id and (project_id, subject_key) in self.scoped_exact:
            return True
        return any(
            fnmatch.fnmatch(subject_key, pattern)
            and (scope is None or scope == project_id)
            for pattern, scope in self.patterns
        )


# -- freshness -------------------------------------------------------------


def freshness_of(ctx: AppContext) -> Freshness:
    """How old the evidence behind an aggregating answer is (FR-V4).

    Reported as the *oldest* relevant sweep, because an answer is exactly as
    fresh as the least fresh thing it depends on. A collector that has never
    completed makes the whole answer `partial` rather than quietly not
    counting.
    """
    if not ctx.observed.has_evidence_tables():
        return Freshness(
            status="never_swept",
            detail="no sweep has run; observed subjects are not collected",
        )
    newest = ctx.observed.coverage()
    if not newest:
        return Freshness(
            status="never_swept",
            detail="no collector has completed a sweep yet",
        )
    now = ctx.clock()
    ages: dict[str, str] = {}
    oldest: datetime | None = None
    for name, sweep in sorted(newest.items()):
        finished = sweep.finished_at or sweep.started_at
        ages[name] = f"{int((now - finished).total_seconds())}s ago ({sweep.outcome})"
        if oldest is None or finished < oldest:
            oldest = finished
    partial = any(sweep.outcome != "ok" for sweep in newest.values())
    return Freshness(
        status="partial" if partial else "fresh",
        oldest_relevant_sweep=oldest,
        age_seconds=None if oldest is None else int((now - oldest).total_seconds()),
        collectors=ages,
        detail=(
            "at least one collector reported a partial or failed sweep"
            if partial
            else None
        ),
    )


# -- trust -----------------------------------------------------------------


def trust_for(
    ctx: AppContext, *, observed_at: datetime | None, disputed: bool = False
) -> str:
    """Compute a trust state; never hand-set (FR-R4, SCHEMA §4).

    `verified` means a sweep newer than the verify horizon confirmed the
    subject. `stale` means the last confirmation aged out. `unverified` means
    nothing has ever confirmed it — which is the honest answer for declared
    work nobody has linked to anything.
    """
    if disputed:
        return "disputed"
    if observed_at is None:
        return "unverified"
    age = (ctx.clock() - observed_at).total_seconds()
    horizon = ctx.config.verify_horizon_hours * 3600
    return "verified" if age <= horizon else "stale"


# -- assembling the ranked set --------------------------------------------


@dataclass(frozen=True)
class _Candidate:
    """One thing that might appear in a ranked view, with its inputs.

    Carrying the ranking inputs alongside the entry — rather than looking
    them up again at scoring time — keeps scoring a pure function of what was
    gathered, and keeps the two halves of the set gathered the same way.
    """

    rankable: Rankable
    entry: RankedItem
    fan_out: int = 0
    initiative_weight: int = 0


def _declared_candidates(view: ReadView, items: list[WorkItem]) -> list[_Candidate]:
    fan_out = view.blocking_fan_out([item.id for item in items])
    weights: dict[str, int] = {}
    for item in items:
        if item.initiative_id and item.initiative_id not in weights:
            initiative = view.initiative_by_id(item.initiative_id)
            weights[item.initiative_id] = 0 if initiative is None else initiative.weight

    return [
        _Candidate(
            rankable=Rankable.from_work_item(item),
            entry=RankedItem(
                origin="declared",
                ref=item.ref,
                title=item.title,
                kind=item.kind,
                state=item.state,
                priority=item.priority,
                project_slug=item.project_slug,
                trust_state=item.trust_state,
                labels=list(item.labels),
                score=0.0,
                updated_at=item.updated_at,
                item=item,
            ),
            fan_out=fan_out.get(item.id, 0),
            initiative_weight=(
                weights.get(item.initiative_id, 0) if item.initiative_id else 0
            ),
        )
        for item in items
    ]


def _observed_candidates(
    ctx: AppContext,
    observations: list[Observation],
    *,
    projects: dict[str, Project],
    adopted: dict[str, str],
) -> list[_Candidate]:
    candidates: list[_Candidate] = []
    for observation in observations:
        priority = priority_of(observation)
        trust = trust_for(ctx, observed_at=observation.observed_at)
        project = (
            projects.get(observation.project_id) if observation.project_id else None
        )
        raw_labels = observation.payload.get("labels")
        labels = (
            [str(label) for label in raw_labels] if isinstance(raw_labels, list) else []
        )
        candidates.append(
            _Candidate(
                rankable=Rankable(
                    id=observation.subject_key,
                    ref=observation.subject_key,
                    priority=priority,
                    updated_at=observation.observed_at,
                    trust_state=trust,  # type: ignore[arg-type]
                    state=OBSERVED_STATE,
                ),
                entry=RankedItem(
                    origin="observed",
                    ref=observation.subject_key,
                    title=title_of(observation),
                    kind=work_kind_of(observation),
                    state=OBSERVED_STATE,
                    priority=priority,
                    project_slug=None if project is None else project.slug,
                    trust_state=trust,  # type: ignore[arg-type]
                    labels=labels,
                    score=0.0,
                    updated_at=observation.observed_at,
                    observation_kind=observation.kind,
                    source_url=observation.source_url,
                    observed_at=observation.observed_at,
                    adopted_as=adopted.get(observation.subject_key),
                ),
            )
        )
    return candidates


def _score_all(candidates: list[_Candidate], *, now: datetime) -> list[RankedItem]:
    scores = [
        score_item(
            candidate.rankable,
            RankingInputs(
                now=now,
                blocking_fan_out=candidate.fan_out,
                initiative_weight=candidate.initiative_weight,
                is_terminal=candidate.rankable.state in TERMINAL_STATES,
            ),
        )
        for candidate in candidates
    ]
    by_id = {candidate.rankable.id: candidate.entry for candidate in candidates}
    return [
        by_id[score.subject_id].model_copy(update={"score": score.total})
        for score in rank(scores)
    ]


def rank_items(
    view: ReadView, items: list[WorkItem], *, now: datetime
) -> list[RankedItem]:
    """Score and order declared work items only (used by the brief)."""
    if not items:
        return []
    return _score_all(_declared_candidates(view, items), now=now)


def _gather(
    ctx: AppContext,
    *,
    project: str | None,
    kinds: Sequence[str] | None,
    priorities: Sequence[str] | None,
    assignee: str | None,
    initiative: str | None,
    label: str | None,
    trust_states: Sequence[str] | None = None,
    include_observed: bool = True,
) -> tuple[list[RankedItem], int, int, int]:
    """Build the ranked set across both stores."""
    with ctx.declared.read() as view:
        project_row = None if project is None else _resolve.project(view, project)
        work_filter = WorkFilter(
            project_id=None if project_row is None else project_row.id,
            kinds=tuple(kinds or ()),
            priorities=tuple(priorities or ()),
            assignee_actor_id=(
                None if assignee is None else _resolve.actor(view, assignee).id
            ),
            initiative_id=(
                None if initiative is None else _resolve.initiative(view, initiative).id
            ),
            label=label,
            trust_states=tuple(trust_states or ()),
            exclude_terminal=True,
            limit=RANKING_CANDIDATE_LIMIT,
        )
        declared_items = view.list_work_items(work_filter)
        candidates = _declared_candidates(view, declared_items)
        projects = {p.id: p for p in view.list_projects(limit=10_000, offset=0)}
        suppressions = SuppressionFilter.build(view.list_suppressions(limit=1000))

        observed: list[Observation] = []
        suppressed = 0
        if include_observed and ctx.observed.has_evidence_tables():
            worklike = [
                observation
                for observation in ctx.observed.latest(
                    project_id=None if project_row is None else project_row.id,
                    limit=RANKING_CANDIDATE_LIMIT,
                )
                if is_worklike(observation)
            ]
            kept: list[Observation] = []
            for observation in worklike:
                if suppressions.hides(observation.subject_key, observation.project_id):
                    suppressed += 1
                    continue
                kept.append(observation)
            adopted = view.work_links_for_subjects(
                [observation.subject_key for observation in kept]
            )
            # An adopted subject is already in the declared half; listing it
            # twice would double-count the work it represents.
            observed = [
                observation
                for observation in kept
                if observation.subject_key not in adopted
            ]
            candidates += _observed_candidates(
                ctx, observed, projects=projects, adopted=adopted
            )

    ranked = _score_all(candidates, now=ctx.clock())
    if kinds:
        ranked = [entry for entry in ranked if entry.kind in set(kinds)]
    if priorities:
        ranked = [entry for entry in ranked if entry.priority in set(priorities)]
    return ranked, len(declared_items), len(observed), suppressed


def backlog(ctx: AppContext, params: BacklogParams) -> BacklogResult:
    """The ranked backlog, globally or for one project (FR-V1, FR-V2, FR-W4).

    Paged with `limit`/`offset` against `total_considered` (FR-V5). Ranking is
    computed over the whole candidate set before the slice, so page two is the
    next rows of one ordering rather than a fresh ranking of what was left —
    which is the only version that pages honestly, and is why the offset is
    applied here rather than pushed into the query.
    """
    ranked, declared, observed, suppressed = _gather(
        ctx,
        project=params.project,
        kinds=params.kinds,
        priorities=params.priorities,
        assignee=params.assignee,
        initiative=params.initiative,
        label=params.label,
        trust_states=params.trust_states,
    )
    return BacklogResult(
        items=ranked[params.offset : params.offset + params.limit],
        total_considered=len(ranked),
        declared=declared,
        observed=observed,
        suppressed=suppressed,
        scope=params.project or "global",
        freshness=freshness_of(ctx),
    )


def bugs(ctx: AppContext, params: BugsParams) -> BacklogResult:
    """Open bugs across every project, declared and observed alike.

    Paged like `backlog`, and for the same reason (FR-V5).
    """
    ranked, declared, observed, suppressed = _gather(
        ctx,
        project=params.project,
        kinds=["bug"],
        priorities=params.priorities,
        assignee=params.assignee,
        initiative=None,
        label=params.label,
    )
    return BacklogResult(
        items=ranked[params.offset : params.offset + params.limit],
        total_considered=len(ranked),
        declared=declared,
        observed=observed,
        suppressed=suppressed,
        scope=params.project or "global",
        freshness=freshness_of(ctx),
    )


def why(ctx: AppContext, params: WhyParams) -> WhyResult:
    """Per-input score contributions for one ranked entry (FR-V3).

    Answers for observed subjects as well as declared work: "why is this
    GitHub issue above my bug" is exactly the question an explainable
    ranking has to be able to take.
    """
    with ctx.declared.read() as view:
        item = view.work_item_by_ref(params.ref)
        if item is not None:
            fan_out = view.blocking_fan_out([item.id])
            weight = 0
            if item.initiative_id:
                initiative = view.initiative_by_id(item.initiative_id)
                weight = 0 if initiative is None else initiative.weight
            rankable = Rankable.from_work_item(item)
            title = item.title
            inputs = RankingInputs(
                now=ctx.clock(),
                blocking_fan_out=fan_out.get(item.id, 0),
                initiative_weight=weight,
                is_terminal=item.state in TERMINAL_STATES,
            )
        else:
            observation = _observation_for(ctx, params.ref)
            title = title_of(observation)
            rankable = Rankable(
                id=observation.subject_key,
                ref=observation.subject_key,
                priority=priority_of(observation),
                updated_at=observation.observed_at,
                trust_state=trust_for(  # type: ignore[arg-type]
                    ctx, observed_at=observation.observed_at
                ),
                state=OBSERVED_STATE,
            )
            inputs = RankingInputs(now=ctx.clock())

    score = score_item(rankable, inputs)
    return WhyResult(
        ref=rankable.ref,
        title=title,
        total=score.total,
        contributions=[
            ContributionView(
                input=entry.input,
                detail=entry.detail,
                value=entry.value,
                weight=entry.weight,
                contribution=entry.contribution,
            )
            for entry in score.contributions
        ],
        inputs_not_yet_available=dict(PENDING_INPUTS),
    )


def _observation_for(ctx: AppContext, ref: str) -> Observation:
    if ctx.observed.has_evidence_tables():
        found = ctx.observed.list_observations(subject_key=ref, limit=1)
        if found:
            return found[0]
    msg = f"no work item or observed subject {ref!r}"
    raise NotFound(msg)
