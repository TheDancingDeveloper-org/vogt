"""Sweeping: running collectors and turning what they found into answers.

This is the one place both stores are open at once, which is deliberate — the
cross-store join lives in the application layer, never in SQL (`SCHEMA.md`
§1). Two of those joins happen here: resolving a dependency reference to a
registered project (FR-D3), and publishing the sweep-completion event into
the declared feed on the collectors' behalf (FR-N1).
"""

from __future__ import annotations

import re
from dataclasses import dataclass
from pathlib import Path

from vogt.application.context import AppContext
from vogt.application.models import (
    CoverageEntry,
    CoverageParams,
    CoverageResult,
    DepsParams,
    DepsResult,
    ObservationsParams,
    ObservationsResult,
    SweepParams,
    SweepReportView,
    SweepResult,
)
from vogt.application.services import _resolve
from vogt.application.services.views import freshness_of
from vogt.collectors import CollectorContext, CollectorRegistry, Sweeper
from vogt.collectors.dep_refs import KIND_DEP_REF
from vogt.collectors.session_outcomes import (
    SESSION_OBSERVATION_LIMIT,
    SessionOutcomeCollector,
    SessionRecord,
)
from vogt.core.entities import DepRef, Project
from vogt.errors import InvalidRequest
from vogt.storage.interface import ReadView
from vogt.storage.observed_types import DepRefRow

SWEEP_COMPLETED_EVENT = "sweep.completed"

_GIT_SUFFIX = re.compile(r"\.git$")
_SCP_STYLE = re.compile(r"^(?:git\+)?(?:ssh://)?git@(?P<host>[^:/]+)[:/](?P<path>.+)$")


def collector_registry(ctx: AppContext) -> CollectorRegistry:
    """Build the collectors this instance can run.

    The GitHub adapter registers itself only when it is configured. Its
    absence is not an error and not a degraded mode — it means forge
    subjects are "not collected", which is a different answer from "there
    are none" (NFR-PO1, FR-O4). `session-outcomes` follows the same rule
    against the session engine: with no engine configured there is nothing
    that could know how a session ended, and coverage says the collector has
    never run rather than reporting no outcomes.
    """
    registry = CollectorRegistry()
    from vogt.adapters.github import github_collectors

    for collector in github_collectors(ctx.config):
        registry.add(collector)
    if ctx.engine is not None:
        registry.add(SessionOutcomeCollector(ctx.engine, _DeclaredSessions(ctx)))
    return registry


class _DeclaredSessions:
    """The declared reads `session-outcomes` needs, done on its behalf.

    Here rather than in the collector because collectors never touch declared
    data (FR-O2, `collectors/__init__.py`). The collector is handed flat
    records; the join from a session to its work item's ref, and from a
    task's binding to a project slug, is application-layer work like every
    other cross-store join (`SCHEMA.md` §1).
    """

    def __init__(self, ctx: AppContext) -> None:
        self._ctx = ctx
        # A task binding is asked about once per project per sweep, and the
        # answer cannot change inside one sweep. Cached so that ten projects
        # and ten bound tasks are ten lookups rather than a hundred.
        self._work_item_projects: dict[str, str | None] = {}

    def for_project(self, project: Project) -> list[SessionRecord]:
        with self._ctx.declared.read() as view:
            sessions = view.list_sessions(
                project_id=project.id,
                include_stopped=True,
                limit=SESSION_OBSERVATION_LIMIT,
                offset=0,
            )
            refs: dict[str, str] = {}
            for session in sessions:
                if session.work_item_id is None or session.work_item_id in refs:
                    continue
                item = view.work_item_by_id(session.work_item_id)
                if item is not None:
                    refs[session.work_item_id] = item.ref
            return [
                SessionRecord(
                    id=session.id,
                    engine_session_id=session.engine_session_id,
                    cwd=session.cwd,
                    started_at=session.started_at,
                    stopped_at=session.stopped_at,
                    work_item=(
                        None
                        if session.work_item_id is None
                        else refs.get(session.work_item_id)
                    ),
                )
                for session in sessions
            ]

    def project_of_work_item(self, ref: str) -> str | None:
        if ref in self._work_item_projects:
            return self._work_item_projects[ref]
        slug: str | None = None
        with self._ctx.declared.read() as view:
            # `view.work_item_by_ref` rather than `_resolve.work_item`, which
            # exists to raise: a binding naming an item this instance does not
            # have is a fact about the engine's configuration, not an error
            # worth failing a sweep over. Those runs are bound to nothing
            # here, so nothing is collected for them.
            item = view.work_item_by_ref(ref)
            if item is not None and item.project_id is not None:
                project = view.project_by_id(item.project_id)
                slug = None if project is None else project.slug
        self._work_item_projects[ref] = slug
        return slug


def sweep(ctx: AppContext, params: SweepParams) -> SweepResult:
    """Collect over the registered projects, then rebuild the projections.

    Scope is always the registered project list (FR-G15) — optionally
    narrowed to one project, never widened by looking around.
    """
    with ctx.declared.read() as view:
        if params.project:
            projects = [_resolve.project(view, params.project)]
        else:
            projects = view.list_projects(limit=10_000, offset=0)

    registry = collector_registry(ctx)
    collectors = registry.select(
        tuple(params.collectors or ()), offline_only=params.offline_only
    )
    if not collectors:
        msg = (
            f"no collectors selected (available: {', '.join(registry.names) or 'none'})"
        )
        raise InvalidRequest(msg)

    sweeper = Sweeper(
        ctx.observed,
        CollectorContext(config=ctx.config, clock=ctx.clock),
    )
    reports = sweeper.run(collectors, projects)

    try:
        subjects = ctx.observed.rebuild_latest()
        dep_rows = _resolve_dep_refs(ctx)
        ctx.observed.replace_dep_refs(dep_rows)
    except Exception as exc:
        # Every collector above already committed its own sweep row — each
        # one genuinely ran and is, in isolation, `ok`. But nothing after
        # this point ran: no rebuilt projection, no `sweep.completed`
        # event. Left alone, `coverage` would keep reporting this batch
        # fresh and fine from a run nothing downstream ever heard complete
        # (FR-O4, #44). Overwrite it to `failed` before the exception
        # propagates, so absence stays honest even when the crash is here
        # rather than inside a collector.
        detail = f"sweep batch did not complete: {type(exc).__name__}: {exc}"
        ctx.observed.fail_sweeps([report.sweep_id for report in reports], detail=detail)
        raise

    for report in reports:
        ctx.declared.publish_event(
            kind=SWEEP_COMPLETED_EVENT,
            entity_kind="sweep",
            entity_id=report.sweep_id,
            summary={
                "collector": report.collector,
                "outcome": report.outcome,
                "projects": report.projects,
                "new": report.new,
                "unchanged": report.unchanged,
            },
            at=ctx.clock(),
        )

    return SweepResult(
        scope=params.project or "all registered projects",
        projects=len(projects),
        subjects=subjects,
        dep_refs=len(dep_rows),
        reports=[
            SweepReportView(
                collector=report.collector,
                sweep_id=report.sweep_id,
                outcome=report.outcome,
                projects=report.projects,
                new=report.new,
                unchanged=report.unchanged,
                failures=report.failures,
            )
            for report in reports
        ],
    )


# -- dependency resolution (FR-D3) ----------------------------------------


@dataclass(frozen=True)
class _ResolutionIndex:
    """How a raw reference target is matched to a registered project."""

    by_path: dict[str, str]
    by_repo: dict[str, str]
    roots: dict[str, Path]


def _index(projects: list[Project]) -> _ResolutionIndex:
    by_path: dict[str, str] = {}
    by_repo: dict[str, str] = {}
    roots: dict[str, Path] = {}
    for project in projects:
        root = Path(project.root_path).expanduser()
        roots[project.id] = root
        by_path[str(root.resolve() if root.exists() else root)] = project.id
        if project.repo_url:
            by_repo[_normalise_repo(project.repo_url)] = project.id
    return _ResolutionIndex(by_path=by_path, by_repo=by_repo, roots=roots)


def _normalise_repo(url: str) -> str:
    """Reduce a repository URL to `host/owner/repo`.

    The same repository is written half a dozen ways — `git@github.com:o/r`,
    `https://github.com/o/r.git`, `git+ssh://...` — and a reference that
    fails to resolve because of punctuation is worse than no graph at all.
    """
    candidate = url.strip()
    candidate = candidate.removeprefix("git+")
    scp = _SCP_STYLE.match(candidate)
    if scp:
        candidate = f"{scp.group('host')}/{scp.group('path')}"
    else:
        for prefix in ("https://", "http://", "ssh://", "github:"):
            if candidate.startswith(prefix):
                candidate = candidate[len(prefix) :]
                break
    candidate = _GIT_SUFFIX.sub("", candidate).strip("/").lower()
    return candidate


def _resolve_dep_refs(ctx: AppContext) -> list[DepRefRow]:
    """Turn dependency observations into the resolved projection."""
    with ctx.declared.read() as view:
        projects = view.list_projects(limit=10_000, offset=0)
    index = _index(projects)

    rows: list[DepRefRow] = []
    for observation in ctx.observed.latest(kinds=(KIND_DEP_REF,), limit=100_000):
        if observation.project_id is None:
            continue
        payload = observation.payload
        ref_kind = str(payload.get("ref_kind", "path"))
        raw_target = str(payload.get("raw_target", ""))
        manifest = payload.get("manifest")
        resolved = _resolve_target(
            index,
            from_project_id=observation.project_id,
            ref_kind=ref_kind,
            raw_target=raw_target,
            manifest=None if manifest is None else str(manifest),
        )
        rows.append(
            DepRefRow(
                subject_key=observation.subject_key,
                from_project_id=observation.project_id,
                ref_kind=ref_kind,
                raw_target=raw_target,
                manifest=None if manifest is None else str(manifest),
                to_project_id=resolved,
                observed_at=observation.observed_at,
            )
        )
    return rows


def _resolve_target(
    index: _ResolutionIndex,
    *,
    from_project_id: str,
    ref_kind: str,
    raw_target: str,
    manifest: str | None,
) -> str | None:
    """Resolve one reference, or leave it unresolved with its raw target.

    An internal-looking reference that resolves to nothing is kept as-is and
    reported as `unresolved_dependency` from M3 — it is usually a project
    nobody registered yet, which is exactly the thing worth surfacing.
    """
    if ref_kind == "git":
        return index.by_repo.get(_normalise_repo(raw_target))

    root = index.roots.get(from_project_id)
    if root is None:  # pragma: no cover - from_project always in scope
        return None
    base = root / Path(manifest).parent if manifest else root
    target = raw_target
    for prefix in ("file:", "link:", "portal:", "workspace:"):
        target = target.removeprefix(prefix)
    if not target or target == ".":
        return None
    candidate = (base / target).resolve()
    return index.by_path.get(str(candidate))


# -- reads -----------------------------------------------------------------


def coverage(ctx: AppContext, params: CoverageParams) -> CoverageResult:
    """What has looked at what, and how long ago (FR-O3).

    The answer to "has anything even looked at this repo lately", which is
    the question the whole observation layer exists to make answerable.
    """
    del params
    if not ctx.observed.has_evidence_tables():
        return CoverageResult(collectors=[], swept_project_ids=[])

    newest = ctx.observed.coverage()
    # Cumulative rather than last-sweep, because the question this operation
    # is named for — what has looked at what — cannot be answered from one
    # sweep's scope. With eight projects registered and the last sweep scoped
    # to one, every collector reported `projects: 1`, which reads as seven
    # unswept projects and was in fact seven projects swept an hour earlier.
    ever = ctx.observed.coverage_by_project()
    with ctx.declared.read() as view:
        registered = {
            project.id for project in view.list_projects(limit=10_000, offset=0)
        }
    registry = collector_registry(ctx)
    now = ctx.clock()

    entries: list[CoverageEntry] = []
    swept: set[str] = set()
    for name in registry.names:
        sweep_record = newest.get(name)
        seen = ever.get(name, {})
        swept.update(seen)
        if sweep_record is None:
            entries.append(
                CoverageEntry(
                    collector=name,
                    status="never_run",
                    detail="this collector has not completed a sweep",
                    registered=len(registered),
                )
            )
            continue
        finished = sweep_record.finished_at or sweep_record.started_at
        entries.append(
            CoverageEntry(
                collector=name,
                status=sweep_record.outcome,
                last_swept_at=finished,
                age_seconds=int((now - finished).total_seconds()),
                projects=len(seen),
                registered=len(registered),
                last_sweep_scope=len(sweep_record.scope),
                never_swept=len(registered - set(seen)),
                detail=sweep_record.detail,
            )
        )
    return CoverageResult(
        collectors=entries,
        swept_project_ids=sorted(swept),
        unswept_project_ids=sorted(registered - swept),
    )


def observations(ctx: AppContext, params: ObservationsParams) -> ObservationsResult:
    """Raw evidence, including subjects that ranked views filter out.

    Suppressed and unpromoted subjects are returnable here on purpose: the
    decision hides them from views, it does not delete evidence (DESIGN §3.6).
    """
    if not ctx.observed.has_evidence_tables():
        return ObservationsResult(observations=[], total=0)

    project_id: str | None = None
    if params.project:
        with ctx.declared.read() as view:
            project_id = _resolve.project(view, params.project).id

    if params.latest_only:
        found = ctx.observed.latest(
            kinds=(params.kind,) if params.kind else (),
            project_id=project_id,
            promoted_only=params.promoted_only,
            limit=params.limit,
        )
    else:
        found = ctx.observed.list_observations(
            kind=params.kind,
            project_id=project_id,
            subject_key=params.subject_key,
            limit=params.limit,
            offset=params.offset,
        )
    return ObservationsResult(observations=found, total=len(found))


def deps(ctx: AppContext, params: DepsParams) -> DepsResult:
    """References out of a project, and — reversed — into it (FR-D4)."""
    if not ctx.observed.has_evidence_tables():
        return DepsResult(
            project=params.project,
            references_out=[],
            referenced_by=[],
            freshness=freshness_of(ctx),
        )

    with ctx.declared.read() as view:
        project = _resolve.project(view, params.project)
        out = ctx.observed.dep_refs(from_project_id=project.id)
        incoming = ctx.observed.dep_refs(to_project_id=project.id)
        return DepsResult(
            project=project.slug,
            references_out=[_named(view, ref) for ref in out],
            referenced_by=[_named(view, ref) for ref in incoming],
            unresolved=sum(1 for ref in out if ref.to_project_id is None),
            freshness=freshness_of(ctx),
        )


def _named(view: ReadView, ref: DepRef) -> DepRef:
    """Attach slugs, so a reference reads without a second lookup."""
    from_project = view.project_by_id(ref.from_project_id)
    to_project = (
        None if ref.to_project_id is None else view.project_by_id(ref.to_project_id)
    )
    return ref.model_copy(
        update={
            "from_project_slug": None if from_project is None else from_project.slug,
            "to_project_slug": None if to_project is None else to_project.slug,
        }
    )
