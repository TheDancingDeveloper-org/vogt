"""The normalized attention Inbox and its audited triage actions.

The browser receives one server-owned projection. It never merges forge
notifications, drift, CI, or live engine state itself; this module does the
joins, ordering, coverage disclosure, and occurrence-scoped decisions.
"""

from __future__ import annotations

import base64
import binascii
import json
from collections.abc import Mapping, Sequence
from dataclasses import dataclass
from datetime import UTC, datetime, timedelta
from typing import Any, Literal, cast

from vogt.adapters.engine import EngineUnavailable
from vogt.adapters.engine.client import EngineApproval, EngineBlocked, EngineSession
from vogt.adapters.forge import KIND_CHECK, KIND_NOTIFICATION
from vogt.adapters.forge.kinds import (
    COLLECTOR_CHECKS,
    COLLECTOR_NOTIFICATIONS,
    KIND_DEPLOY_LANE,
)
from vogt.application.context import AppContext
from vogt.application.memo import StoreMemo
from vogt.application.models import (
    DecisionView,
    InboxAction,
    InboxArchiveParams,
    InboxCoverage,
    InboxEntry,
    InboxListParams,
    InboxListResult,
    InboxRestoreParams,
    InboxSavedFilter,
    InboxSnoozeParams,
    InboxTriageResult,
    SessionGrantView,
)
from vogt.application.services import _resolve
from vogt.application.services.ci_watch import (
    BoundBranch,
    bound_branches,
    hung_entity,
    hung_thresholds,
    index_by_branch,
)
from vogt.application.services.decisions import pending_decisions
from vogt.application.services.grants import pending_grants
from vogt.application.services.reports import one_line, view_of
from vogt.application.services.views import trust_for
from vogt.application.writes import WriteOutcome, audited_write
from vogt.collectors.session_outcomes import KIND_TASK_RUN
from vogt.core.actors import (
    SYSTEM_ACTOR,
    ActorClass,
    classify,
    facts_from_payload,
    matches,
    normalise_bot_logins,
)
from vogt.core.checks import roll_up
from vogt.core.ci_alerts import (
    HungRun,
    RefFailure,
    github_managed,
    watched_failures,
)
from vogt.core.digest import digest_of
from vogt.core.entities import (
    Actor,
    AuditRecord,
    CodingSession,
    DriftProposal,
    InboxTriage,
    Observation,
    Project,
    SessionReport,
    TrustState,
)
from vogt.errors import (
    InboxEntryNotFound,
    InvalidCursor,
    InvalidSnooze,
    InvalidTriageState,
)
from vogt.storage.interface import ReadView, WriteTxn

InboxSource = Literal["github", "drift", "ci", "agent"]
InboxState = Literal["active", "archived", "snoozed"]
EngineStatus = Literal["not_configured", "available", "unreachable"]
DRIFT_KIND: Literal["drift"] = "drift"
CI_KIND: Literal["ci"] = "ci"
GITHUB_KIND: Literal["github"] = "github"
AGENT_KIND: Literal["agent"] = "agent"
#: The `kind` of the CI entries beyond "the newest revision is red".
REF_FAILURE_KIND = "ci.ref_failure"
BRANCH_CONCLUDED_KIND = "ci.branch_concluded"
RUN_HUNG_KIND = "ci.run_hung"
DEPLOY_FAILED_KIND = "deploy.failed"
#: Beside `session.attention`: an oversight session started past a dispatch
#: rule with `override` (WI-1329). The audit operation of the same name.
DISPATCH_OVERRIDE_KIND = "dispatch.override"
MAX_SCAN = 10_000
#: Reports read for the Inbox; the newest per session is kept.
_REPORT_SCAN = 500
#: How long a session's latest report stays an Inbox entry.
_REPORT_HORIZON = timedelta(days=7)
Cursor = tuple[str, str]


def list_inbox(ctx: AppContext, params: InboxListParams) -> InboxListResult:
    snapshot_at = ctx.clock()
    with ctx.declared.read() as view:
        stored, live_entries, engine_error = _inbox_parts(ctx, view)
        # The stored half is already triaged inside the memo. Live entries
        # are triaged once here, the same way the badge does.
        entries = [*stored.entries, *_triaged(ctx, view, live_entries)]
        project_ids = {project.slug: project.id for project in stored.projects.values()}
        project_filter = params.project
        if project_filter is not None:
            project_ids = {project_filter: _resolve.project(view, project_filter).id}
        if params.work_item is not None:
            item = _resolve.work_item(view, params.work_item)
            work_item_id = item.id
        else:
            work_item_id = None

        fingerprint = _fingerprint(params, project_ids, work_item_id)
        entries = [
            entry
            for entry in entries
            if (params.sources is None or entry.source in params.sources)
            and (project_filter is None or entry.project_slug in project_ids)
            and (work_item_id is None or entry.work_item_ref == params.work_item)
            and _triage_matches(entry, params.triage_states, ctx.clock())
        ]
        unknown_hidden = (
            sum(1 for entry in entries if _actor_unknown(entry))
            if params.actor == "external"
            else 0
        )
        entries = [entry for entry in entries if actor_matches(entry, params.actor)]
        source_water = _high_water(entries)
        cursor_value = (
            _decode_cursor(params.cursor, fingerprint) if params.cursor else None
        )
        cursor_water = _cursor_high_water(cursor_value)
        if cursor_value is not None:
            snapshot_at = _cursor_snapshot(cursor_value)
            if cursor_water is not None:
                source_water = {
                    source: cursor_water.get(source) for source in source_water
                }
            entries = [entry for entry in entries if _within_water(entry, source_water)]
        entries.sort(key=_sort_key, reverse=True)
        start = _cursor_index(entries, cursor_value)
        page = entries[start : start + params.limit]
        next_cursor = None
        if start + params.limit < len(entries):
            next_cursor = _encode_cursor(
                fingerprint,
                page[-1],
                snapshot_at=snapshot_at,
                high_water=source_water,
            )
        coverage = _coverage(ctx, view, entries, registered=len(stored.projects))
        engine_status, engine_detail = _engine_status(
            ctx, probed=(live_entries, engine_error)
        )
        counts = {
            state: sum(1 for entry in entries if entry.triage_state == state)
            for state in ("active", "archived", "snoozed")
        }
        return InboxListResult(
            entries=page,
            next_cursor=next_cursor,
            snapshot_at=snapshot_at,
            high_water=source_water,
            coverage=coverage,
            counts=counts,
            github_scope="registered projects only",
            instance_scope="registered projects only",
            engine_status=engine_status,
            engine_detail=engine_detail,
            engine_available=engine_status == "available",
            actor_unknown_hidden=unknown_hidden,
        )


def archive_inbox(ctx: AppContext, params: InboxArchiveParams) -> InboxTriageResult:
    return _triage(
        ctx, entry_key=params.entry_key, state="archived", reason=params.reason
    )


def snooze_inbox(ctx: AppContext, params: InboxSnoozeParams) -> InboxTriageResult:
    if params.until <= ctx.clock():
        raise InvalidSnooze("snooze deadline must be in the future")
    return _triage(
        ctx,
        entry_key=params.entry_key,
        state="snoozed",
        reason=params.reason,
        until=params.until,
    )


def restore_inbox(ctx: AppContext, params: InboxRestoreParams) -> InboxTriageResult:
    return _triage(
        ctx, entry_key=params.entry_key, state="active", reason=params.reason
    )


def _triage(
    ctx: AppContext,
    *,
    entry_key: str,
    state: Literal["active", "archived", "snoozed"],
    reason: str,
    until: datetime | None = None,
) -> InboxTriageResult:
    with ctx.declared.read() as view:
        entry = _entry_by_key(ctx, view, entry_key)
        existing = view.inbox_triage_by_key(entry_key)
    if entry is None:
        raise InboxEntryNotFound(f"no current Inbox entry {entry_key!r}")
    if existing is not None and existing.state == state and state != "snoozed":
        raise InvalidTriageState(f"Inbox entry {entry_key!r} is already {state}")
    if state == "snoozed" and existing is not None and existing.state == "archived":
        raise InvalidTriageState(
            "an archived Inbox entry must be restored before snoozing"
        )

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[InboxTriageResult]:
        now = ctx.clock()
        triage = InboxTriage(
            entry_key=entry_key,
            state=state,
            snooze_until=until if state == "snoozed" else None,
            actor_id=actor.id,
            actor_identity_ref=actor.identity_ref,
            decided_at=now,
            occurrence_snapshot=entry.model_dump(mode="json"),
        )
        txn.upsert_inbox_triage(triage)
        updated = entry.model_copy(
            update={"triage_state": state, "snooze_until": triage.snooze_until}
        )
        return WriteOutcome(
            result=InboxTriageResult(entry=updated),
            entity_kind="inbox_triage",
            entity_id=entry_key,
            payload=triage.model_dump(mode="json"),
            event_kind="inbox.triaged",
            summary={"entry_key": entry_key, "state": state},
        )

    operation = f"inbox.{state if state != 'active' else 'restore'}"
    return audited_write(ctx, operation=operation, reason=reason, body=body)


def actor_matches(entry: InboxEntry, wanted: str) -> bool:
    """Whether an entry passes the `inbox.list` actor filter.

    Reads only the actor fields already on the entry — which came from the
    stored observation — so it is safe on the badge path."""
    actor = ActorClass(
        login=entry.actor_login, kind=entry.actor_kind, relation=entry.actor_relation
    )
    return matches(actor, cast(Any, wanted))


def _actor_unknown(entry: InboxEntry) -> bool:
    """A person-or-unresolved author the `external` filter cannot place."""
    return entry.actor_kind != "bot" and entry.actor_relation == "unknown"


def _actor_fields(actor: ActorClass) -> dict[str, Any]:
    return {
        "actor_login": actor.login,
        "actor_kind": actor.kind,
        "actor_relation": actor.relation,
    }


@dataclass(frozen=True)
class InboxBadge:
    """The sidebar count under a saved filter, and the unfiltered total."""

    filtered: int
    active_total: int


@dataclass(frozen=True)
class _StoredInbox:
    """The half of the Inbox that is a pure function of the two stores.

    Everything but the live engine half: notifications, CI, agent runs,
    bound branches, deploy lanes and drift, with their triage applied. The
    projects are kept beside them because the live half names its entries'
    projects too.
    """

    projects: dict[str, Project]
    entries: list[InboxEntry]


#: The stored half of the Inbox, shared between `inbox.list` and the badge
#: while the stores have not moved (WI-1224, WI-2691). `inbox.list` asks the
#: engine for sessions once per request and the reachability line reuses that
#: answer. Local seed (30 linked projects, 400 chores, 300 markers,
#: sqlite_synchronous=off, 2026-10-29): cold 3.7 ms, warm median 0.49 ms.
#: Prod before this memo was 1.4–1.9 s (WI-2690); the prod ≤100 ms check is
#: not this machine. Every open tab and device nudges its badge on the same
#: event; before, each one rebuilt the whole projection, and the cache that
#: was meant to share it was keyed on the store *objects*, which are built
#: per request, so it never hit across requests. The live half is not in it:
#: it is read from the engine on every badge read, and is cheap.
_stored_memo: StoreMemo[_StoredInbox] = StoreMemo()


def inbox_badge(ctx: AppContext, saved: InboxSavedFilter | None) -> InboxBadge:
    """Count the Inbox under a saved filter, for the shell's badge.

    Same answer as `inbox.list` under the same filter (`counts`/length), but
    cheaper: it skips the coverage and engine-status reads `inbox.list`
    reports, and it shares one projection of the stored half between every
    badge read that arrives while nothing has changed (`StoreMemo`). It
    filters only on fields already on the entry (the actor was resolved at
    collect time), so no forge call can happen here. A snooze that runs out
    is judged at count time, so a shared projection cannot keep it hidden.
    """
    stored = _stored_memo.get(
        ctx, lambda: _stored_triaged(ctx), expires=_next_snooze_end
    )
    with ctx.declared.read() as view:
        live_sessions, _error = _read_engine_sessions(ctx)
        live = _triaged(
            ctx, view, _collect_live(ctx, view, stored.projects, live_sessions)
        )
    entries = [*stored.entries, *live]
    moment = ctx.clock()
    active_total = sum(
        1 for entry in entries if _triage_matches(entry, ["active"], moment)
    )
    if saved is None:
        return InboxBadge(filtered=active_total, active_total=active_total)
    filtered = sum(
        1
        for entry in entries
        if (saved.sources is None or entry.source in saved.sources)
        and _triage_matches(entry, saved.triage_states, moment)
        and actor_matches(entry, saved.actor)
    )
    return InboxBadge(filtered=filtered, active_total=active_total)


def clear_badge_cache() -> None:
    """Drop the shared badge projection (tests, and a restore)."""
    _stored_memo.clear()


def _next_snooze_end(stored: _StoredInbox) -> datetime | None:
    """When the first snooze in the stored half runs out. The triaged
    entries carry their snooze state, so the memo must not outlive it."""
    return min(
        (
            entry.snooze_until
            for entry in stored.entries
            if entry.triage_state == "snoozed" and entry.snooze_until is not None
        ),
        default=None,
    )


def _stored_triaged(ctx: AppContext) -> _StoredInbox:
    with ctx.declared.read() as view:
        stored = _collect_stored(ctx, view)
        return _StoredInbox(
            projects=stored.projects,
            entries=_triaged(ctx, view, stored.entries),
        )


def _triaged(
    ctx: AppContext, view: ReadView, entries: list[InboxEntry]
) -> list[InboxEntry]:
    triage = view.inbox_triage_by_keys([entry.entry_key for entry in entries])
    return [_apply_triage(ctx, triage, entry) for entry in entries]


def _collect(ctx: AppContext, view: ReadView) -> list[InboxEntry]:
    stored, live, _error = _inbox_parts(ctx, view)
    return [*stored.entries, *live]


def _inbox_parts(
    ctx: AppContext, view: ReadView
) -> tuple[_StoredInbox, list[InboxEntry], EngineUnavailable | None]:
    """The stored half (shared while the stores have not moved) plus one
    live read of the engine, reused for the reachability line."""
    stored = _stored_memo.get(
        ctx, lambda: _stored_triaged(ctx), expires=_next_snooze_end
    )
    live, error = _read_engine_sessions(ctx)
    entries = _collect_live(ctx, view, stored.projects, live)
    return stored, entries, error


def _collect_stored(ctx: AppContext, view: ReadView) -> _StoredInbox:
    """The stored half, untriaged: a pure function of the two stores."""
    projects = {p.id: p for p in view.list_projects(limit=MAX_SCAN, offset=0)}
    entries: list[InboxEntry] = []
    bots = normalise_bot_logins(ctx.config.inbox_bot_logins)

    notifications = ctx.observed.latest(kinds=(KIND_NOTIFICATION,), limit=MAX_SCAN)
    checks_all = ctx.observed.latest(kinds=(KIND_CHECK,), limit=MAX_SCAN)
    task_runs = ctx.observed.latest(kinds=(KIND_TASK_RUN,), limit=MAX_SCAN)
    links = view.work_links_for_subjects(
        [
            observation.subject_key
            for observation in (*notifications, *checks_all, *task_runs)
        ]
    )

    for observation in notifications:
        payload = observation.payload
        title = _text(payload.get("title")) or "GitHub notification"
        occurred = _when(payload.get("updated_at")) or observation.observed_at
        facts = facts_from_payload(payload.get("actor"))
        entries.append(
            _observation_entry(
                ctx,
                observation,
                projects,
                links,
                GITHUB_KIND,
                title,
                _text(payload.get("reason")) or title,
                occurred,
                actor=(
                    ActorClass(login=None, kind=None, relation="unknown")
                    if facts is None
                    else classify(facts, bots=bots)
                ),
            )
        )

    for observation in task_runs:
        findings = observation.payload.get("findings")
        if not isinstance(findings, list) or not findings:
            continue
        project = projects.get(observation.project_id or "")
        title = _text(observation.payload.get("task")) or "Agent task finding"
        summary = _text(observation.payload.get("summary"))
        if summary is None:
            first = findings[0]
            summary = (
                _text(first.get("text")) if isinstance(first, dict) else None
            ) or "An agent task reported a finding."
        material = {
            "subject_key": observation.subject_key,
            "findings": findings,
            "summary": summary,
            "status": observation.payload.get("status"),
            "outcome": observation.payload.get("state"),
        }
        entries.append(
            InboxEntry(
                entry_key=f"agent:{observation.subject_key}:{digest_of(material)}",
                source=AGENT_KIND,
                kind=observation.kind,
                occurred_at=(
                    _when(observation.payload.get("completed_at"))
                    or _when(observation.payload.get("started_at"))
                    or observation.observed_at
                ),
                observed_at=observation.observed_at,
                title=f"Agent task: {title}",
                summary=summary,
                project_slug=None if project is None else project.slug,
                work_item_ref=links.get(observation.subject_key),
                source_subject_key=observation.subject_key,
                trust_state=cast(
                    TrustState, trust_for(ctx, observed_at=observation.observed_at)
                ),
                freshness="current"
                if trust_for(ctx, observed_at=observation.observed_at) == "verified"
                else "stale",
                action=InboxAction(
                    kind="observation", subject_key=observation.subject_key
                ),
                **_actor_fields(SYSTEM_ACTOR),
            )
        )

    # One roll-up per project, computed from the checks already in hand. The
    # previous shape re-read every latest check of the project and rolled it
    # up again *for each failing check*, so the projection cost
    # Σ failing × checks-per-project — 730 queries and 732k observation
    # objects for one page on a two-week-old estate, and the read crossed the
    # PWA's deadline, which turned every badge refresh into a retry.
    # GitHub-managed runs (Dependabot "Update #N") are not the repository's
    # CI and fail routinely; they never make a "CI failing" entry, nor move a
    # project's newest revision. Judged on the stored payload alone.
    repo_checks = [
        observation
        for observation in checks_all
        if not github_managed(observation.payload)
    ]
    checks_by_project: dict[str, list[Observation]] = {}
    for observation in repo_checks:
        if observation.project_id is not None:
            checks_by_project.setdefault(observation.project_id, []).append(observation)
    newest_revision_ids: dict[str, frozenset[str]] = {}

    # Watched refs first (WI-867): a failed run on the default branch, a
    # release tag or prod, judged by its own workflow+ref lane rather than by
    # the project's newest revision — which is how a tag's failed
    # `release-mobile` went unseen for two releases. One pass, O(checks).
    alerted: set[str] = set()
    for failure in watched_failures(
        checks_all,
        branches=ctx.config.ci_alert_branches,
        tags=ctx.config.ci_alert_tags,
    ):
        if failure.observation.project_id not in projects:
            continue
        alerted.add(failure.observation.id)
        entries.append(_ref_failure_entry(ctx, failure, projects, links))

    for observation in repo_checks:
        if _text(observation.payload.get("conclusion")) in (None, "success", "skipped"):
            continue
        if observation.id in alerted:
            continue
        project = projects.get(observation.project_id or "")
        if project is None:
            continue
        if project.id not in newest_revision_ids:
            rolled = roll_up(checks_by_project.get(project.id, []))
            newest_revision_ids[project.id] = frozenset(
                check.id for check in (rolled.checks if rolled is not None else ())
            )
        if observation.id not in newest_revision_ids[project.id]:
            continue
        check = _text(observation.payload.get("check")) or "CI check"
        revision = _text(observation.payload.get("revision")) or "unknown revision"
        entries.append(
            _observation_entry(
                ctx,
                observation,
                projects,
                links,
                CI_KIND,
                f"CI failing: {check}",
                f"{check} is failing on {revision}",
                observation.observed_at,
                actor=SYSTEM_ACTOR,
            )
        )

    # Bound branches (WI-855): the settled CI verdict on the newest revision
    # of each branch an open work item is bound to.
    for bound in bound_branches(
        view,
        index_by_branch(checks_all),
        now=ctx.clock(),
        hung_after=hung_thresholds(ctx.config),
    ):
        if bound.project_id not in projects:
            continue
        if bound.ci.settled:
            entries.append(_bound_branch_entry(ctx, bound, projects))
        # WI-1334: one entry per hung run; it goes when the run concludes.
        entries.extend(
            _hung_run_entry(ctx, bound, hung, projects) for hung in bound.ci.hung
        )

    # Read only when lanes are configured: an instance without any pays no
    # extra store read on every Inbox and badge projection.
    lanes = (
        ctx.observed.latest(kinds=(KIND_DEPLOY_LANE,), limit=MAX_SCAN)
        if ctx.config.deploy_lanes
        else []
    )
    for lane in lanes:
        entry = _deploy_lane_entry(ctx, lane, projects)
        if entry is not None:
            entries.append(entry)

    for proposal in view.list_drift(status="open", limit=MAX_SCAN):
        entries.append(_drift_entry(ctx, proposal, projects, view))
    return _StoredInbox(projects=projects, entries=entries)


def _read_engine_sessions(
    ctx: AppContext,
) -> tuple[list[EngineSession], EngineUnavailable | None]:
    """One `list_sessions` for the request. The error is the reachability
    probe: a second call would only repeat it."""
    if ctx.engine is None:
        return [], None
    try:
        live, _hidden = ctx.engine.list_sessions()
    except EngineUnavailable as error:
        return [], error
    return live, None


def _collect_live(
    ctx: AppContext,
    view: ReadView,
    projects: dict[str, Project],
    live: list[EngineSession] | None = None,
) -> list[InboxEntry]:
    """The live half, untriaged: grants waiting for a person and the engine's
    sessions that need one. Read on every call — the engine is its truth.

    `live` is a session list this request already fetched. Without it the
    engine is asked once here.
    """
    entries: list[InboxEntry] = []
    if live is None:
        live, _error = _read_engine_sessions(ctx)

    # Grants waiting for a person (WI-973): one entry per request, decided
    # with Approve/Deny on the entry itself. The target is named as the
    # person knows it (title, role, agent, project), because approving the
    # right secret for the wrong session is the mistake this entry must not
    # invite.
    live_by_id = {session.id: session for session in live}
    for grant in pending_grants(view, ctx):
        entries.append(_grant_entry(ctx, grant, view, projects, live_by_id))
    # Decisions waiting for a person (WI-1321): one entry per question,
    # answered by choosing one of its options on the entry itself.
    for decision in pending_decisions(view, ctx):
        entries.append(_decision_entry(decision, view))
    # Sessions' own reports (WI-2039): the newest one per session, so a
    # worker's result, question or blocker reaches oversight without a read
    # of its transcript. A later report replaces the entry (a new key).
    # A report stops being an entry once its session is stopped or the
    # report is older than `_REPORT_HORIZON`: a finished worker's last
    # `result` is history on its work item, not something to act on. Each
    # entry can also be archived or snoozed like any other.
    seen_sessions: set[str] = set()
    oldest = ctx.clock() - _REPORT_HORIZON
    for row in view.list_session_reports(limit=_REPORT_SCAN):
        key = row.engine_session_id or row.id
        if key in seen_sessions:
            continue
        seen_sessions.add(key)
        if row.created_at < oldest:
            continue
        declared = (
            None
            if row.engine_session_id is None
            else view.session_by_engine_id(row.engine_session_id)
        )
        if declared is not None and declared.stopped_at is not None:
            continue
        entries.append(_report_entry(view, row, projects))
    # Oversight overrides of the dispatch rules (WI-1329): one entry per
    # override, for as long as a report would stay, so a person sees every
    # start that went past the worker cap or the review rule.
    for record in view.list_audit(
        operation=DISPATCH_OVERRIDE_KIND, since=oldest, limit=_REPORT_SCAN
    ):
        entries.append(_override_entry(view, record, projects))

    if ctx.engine is not None:
        try:
            for session in live:
                if session.blocked is not None and session.alive:
                    entries.append(
                        _blocked_entry(
                            ctx,
                            session.id,
                            session.name,
                            session.blocked,
                            view.session_by_engine_id(session.id),
                            projects,
                            view=view,
                            work_label=session.work_item,
                        )
                    )
                if session.activity not in (
                    "waiting-for-input",
                    "awaiting-approval",
                    "errored",
                ):
                    continue
                declared = view.session_by_engine_id(session.id)
                entries.append(
                    _session_entry(
                        ctx,
                        session.id,
                        session.name,
                        session.activity,
                        session.activity_changed_at,
                        declared,
                        projects,
                        approval=session.approval,
                        view=view,
                        work_label=session.work_item,
                    )
                )
        except EngineUnavailable:
            pass
    return entries


def _freshness(
    ctx: AppContext, observed_at: datetime
) -> tuple[TrustState, Literal["current", "stale"]]:
    trust = cast(TrustState, trust_for(ctx, observed_at=observed_at))
    return trust, "current" if trust == "verified" else "stale"


def _ref_failure_entry(
    ctx: AppContext,
    failure: RefFailure,
    projects: dict[str, Project],
    links: dict[str, str],
) -> InboxEntry:
    """A failed run on a watched ref, naming the failed jobs and their log.

    Keyed by the run (id, attempt, conclusion) rather than by the whole
    payload: the job list arrives a sweep after the failure when the lookup
    budget is spent, and a key that moved then would resurrect an entry
    somebody had already archived."""
    observation = failure.observation
    payload = observation.payload
    project = projects.get(observation.project_id or "")
    where = failure.where
    listed = payload.get("failed_jobs")
    jobs = [
        job
        for job in (listed if isinstance(listed, list) else [])
        if isinstance(job, dict) and isinstance(job.get("name"), str)
    ]
    revision = _text(payload.get("revision")) or "unknown revision"
    lane = (
        f"a later run on {where.ref}"
        if where.kind == "branch"
        else f"a later run on a tag matching {where.lane}"
    )
    if jobs:
        named = ", ".join(str(job["name"]) for job in jobs[:3])
        more = f" (+{len(jobs) - 3} more)" if len(jobs) > 3 else ""
        what = f"Failed job: {named}{more}."
    else:
        what = f"The run concluded {failure.conclusion}."
    summary = (
        f"{what} {failure.workflow} on {where.ref} at {revision[:12]}. "
        f"Clears when {lane} of {failure.workflow} succeeds."
    )
    log_url = next(
        (
            job["url"]
            for job in jobs
            if isinstance(job.get("url"), str) and job.get("url")
        ),
        observation.source_url,
    )
    material = {
        "subject_key": observation.subject_key,
        "run_id": payload.get("run_id"),
        "run_attempt": payload.get("run_attempt"),
        "run_number": payload.get("run_number"),
        "conclusion": failure.conclusion,
    }
    trust, freshness = _freshness(ctx, observation.observed_at)
    return InboxEntry(
        entry_key=f"ci:ref:{observation.subject_key}:{digest_of(material)}",
        source=CI_KIND,
        kind=REF_FAILURE_KIND,
        occurred_at=_when(payload.get("updated_at")) or observation.observed_at,
        observed_at=observation.observed_at,
        title=f"{failure.workflow} failed on {where.ref}",
        summary=summary[:1000],
        project_slug=None if project is None else project.slug,
        work_item_ref=links.get(observation.subject_key),
        source_subject_key=observation.subject_key,
        source_url=log_url if isinstance(log_url, str) else observation.source_url,
        trust_state=trust,
        freshness=freshness,
        action=InboxAction(kind="observation", subject_key=observation.subject_key),
        **_actor_fields(SYSTEM_ACTOR),
    )


def _bound_branch_entry(
    ctx: AppContext, bound: BoundBranch, projects: dict[str, Project]
) -> InboxEntry:
    """CI settled on a branch a work item is bound to — pass or fail."""
    ci = bound.ci
    project = projects.get(bound.project_id)
    newest = max(ci.runs, key=lambda run: run.observation.observed_at)
    failing_url = next(
        (
            run.observation.source_url
            for run in ci.runs
            if run.workflow in ci.failing and run.observation.source_url
        ),
        None,
    )
    if ci.state == "failed":
        summary = f"Failing: {', '.join(ci.failing)}."
    elif ci.state == "passed":
        summary = f"All {len(ci.runs)} workflow(s) passed."
    else:
        summary = "Every run on the revision was cancelled."
    summary = (
        f"{summary} {bound.branch} @ {ci.revision[:12]} for {bound.work_item_ref}."
    )
    material = {
        "branch": bound.branch,
        "revision": ci.revision,
        "state": ci.state,
        "runs": [[run.workflow, run.conclusion] for run in ci.runs],
    }
    trust, freshness = _freshness(ctx, newest.observation.observed_at)
    subject = f"ci-branch:{bound.work_item_ref}:{bound.branch}"
    return InboxEntry(
        entry_key=f"ci:branch:{subject}:{digest_of(material)}",
        source=CI_KIND,
        kind=BRANCH_CONCLUDED_KIND,
        occurred_at=_when(ci.concluded_at) or newest.observation.observed_at,
        observed_at=newest.observation.observed_at,
        title=f"CI {ci.state} on {bound.branch}",
        summary=summary[:1000],
        project_slug=None if project is None else project.slug,
        work_item_ref=bound.work_item_ref,
        source_subject_key=newest.observation.subject_key,
        source_url=failing_url or newest.observation.source_url,
        trust_state=trust,
        freshness=freshness,
        action=InboxAction(
            kind="observation", subject_key=newest.observation.subject_key
        ),
        **_actor_fields(SYSTEM_ACTOR),
    )


def _hung_run_entry(
    ctx: AppContext, bound: BoundBranch, hung: HungRun, projects: dict[str, Project]
) -> InboxEntry:
    """A run on a bound branch still queued or running past its threshold."""
    project = projects.get(bound.project_id)
    observation = hung.observation
    run = str(hung.run_id) if hung.run_id is not None else "unknown run"
    state = "queued" if hung.phase == "queued" else "running"
    basis = (
        ""
        if hung.since_field == "run_started_at"
        else " (measured from the run's last update: the forge gave no start time)"
    )
    cancel = f" Suggested: `gh run cancel {hung.run_id}`." if hung.run_id else ""
    summary = (
        f"{hung.workflow} run {run} on {bound.branch} for {bound.work_item_ref} "
        f"has been {state} {hung.elapsed_minutes} min, since {hung.since}"
        f"{basis}.{cancel} Clears when the run concludes."
    )
    trust, freshness = _freshness(ctx, observation.observed_at)
    return InboxEntry(
        entry_key=f"ci:hung:{hung_entity(hung)}",
        source=CI_KIND,
        kind=RUN_HUNG_KIND,
        occurred_at=_when(hung.since) or observation.observed_at,
        observed_at=observation.observed_at,
        title=f"CI run may be hung: {hung.workflow} {state} {hung.elapsed_minutes} min",
        summary=summary[:1000],
        project_slug=None if project is None else project.slug,
        work_item_ref=bound.work_item_ref,
        source_subject_key=observation.subject_key,
        source_url=observation.source_url,
        trust_state=trust,
        freshness=freshness,
        action=InboxAction(kind="observation", subject_key=observation.subject_key),
        **_actor_fields(SYSTEM_ACTOR),
    )


def _deploy_lane_entry(
    ctx: AppContext, observation: Observation, projects: dict[str, Project]
) -> InboxEntry | None:
    """A deploy lane whose latest receipt says the deploy failed."""
    receipt = observation.payload.get("receipt")
    if not isinstance(receipt, dict) or receipt.get("status") != "failed":
        return None
    project = projects.get(observation.project_id or "")
    if project is None:
        return None
    lane = _text(observation.payload.get("lane")) or "deploy"
    sha = _text(receipt.get("source_sha")) or "unknown revision"
    tag = _text(receipt.get("source_tag"))
    material = {"subject_key": observation.subject_key, "receipt": receipt}
    trust, freshness = _freshness(ctx, observation.observed_at)
    return InboxEntry(
        entry_key=f"ci:deploy:{observation.subject_key}:{digest_of(material)}",
        source=CI_KIND,
        kind=DEPLOY_FAILED_KIND,
        occurred_at=_when(receipt.get("timestamp")) or observation.observed_at,
        observed_at=observation.observed_at,
        title=f"Deploy failed on lane {lane}",
        summary=(
            f"The {lane} receipt reports a failed deploy of "
            f"{tag or sha[:12]}. Clears when a later receipt reports success."
        ),
        project_slug=project.slug,
        source_subject_key=observation.subject_key,
        source_url=_text(receipt.get("url")) or observation.source_url,
        trust_state=trust,
        freshness=freshness,
        action=InboxAction(kind="observation", subject_key=observation.subject_key),
        **_actor_fields(SYSTEM_ACTOR),
    )


def _observation_entry(
    ctx: AppContext,
    observation: Observation,
    projects: dict[str, Project],
    links: dict[str, str],
    source: Literal["github", "ci"],
    title: str,
    summary: str,
    occurred: datetime,
    *,
    actor: ActorClass,
) -> InboxEntry:
    project = projects.get(observation.project_id or "")
    ref = links.get(observation.subject_key)
    material = {
        "subject_key": observation.subject_key,
        "title": title,
        "summary": summary,
        "source_url": observation.source_url,
        # `actor` is excluded like the read markers: it is who caused the
        # occurrence, resolved later than the occurrence itself, and an
        # author resolved on a later sweep must not turn an archived entry
        # back into a new one.
        "payload": {
            key: value
            for key, value in observation.payload.items()
            if key not in {"unread", "last_read", "observed_at", "actor"}
        },
    }
    return InboxEntry(
        entry_key=f"{source}:{observation.subject_key}:{digest_of(material)}",
        source=source,
        kind=observation.kind,
        occurred_at=occurred,
        observed_at=observation.observed_at,
        title=title,
        summary=summary,
        project_slug=None if project is None else project.slug,
        work_item_ref=ref,
        source_subject_key=observation.subject_key,
        source_url=observation.source_url,
        trust_state=cast(
            TrustState, trust_for(ctx, observed_at=observation.observed_at)
        ),
        freshness="current"
        if trust_for(ctx, observed_at=observation.observed_at) == "verified"
        else "stale",
        action=InboxAction(kind="observation", subject_key=observation.subject_key),
        **_actor_fields(actor),
    )


def _drift_entry(
    ctx: AppContext,
    proposal: DriftProposal,
    projects: dict[str, Project],
    view: ReadView,
) -> InboxEntry:
    project = projects.get(proposal.project_id or "")
    ref = None
    if proposal.subject_kind == "work_item":
        item = view.work_item_by_id(proposal.subject_id)
        ref = None if item is None else item.ref
    material = digest_of(
        {
            "evidence": proposal.evidence_snapshot,
            "proposed": proposal.proposed_change,
        }
    )
    return InboxEntry(
        entry_key=f"drift:{proposal.id}:{material}",
        source=DRIFT_KIND,
        kind=proposal.kind,
        occurred_at=proposal.opened_at,
        observed_at=proposal.opened_at,
        title=proposal.summary,
        summary=proposal.summary,
        project_slug=None if project is None else project.slug,
        work_item_ref=ref,
        source_subject_key=proposal.id,
        trust_state="disputed",
        freshness="current",
        evidence_snapshot=proposal.evidence_snapshot,
        proposed_change=proposal.proposed_change,
        action=InboxAction(kind="drift", drift_id=proposal.id),
        **_actor_fields(SYSTEM_ACTOR),
    )


#: Who a blocked report waits on, by its `needs` (WI-996). An unknown value
#: (a newer engine) reads as a person, as `_needs` does.
_BLOCKED_ON = {
    "person": "you",
    "overseer": "its overseer",
    "either": "you or its overseer",
}


def _blocked_entry(
    ctx: AppContext,
    session_id: str,
    name: str,
    blocked: EngineBlocked,
    declared: CodingSession | None,
    projects: dict[str, Project],
    *,
    view: ReadView,
    work_label: str | None = None,
) -> InboxEntry:
    """A session whose agent reported it cannot go on without a person.

    One occurrence per ask: its key is the report's content key (WI-996), so
    the same ask reported again stays one entry (archived stays archived) and
    a different ask surfaces anew. A report from an engine without content
    keys falls back to the report's time. The title names who it waits on
    (`needs`). The text is the agent's —
    untrusted, shown verbatim.
    """
    project = None if declared is None else projects.get(declared.project_id)
    ref = _bound_ref(view, declared, work_label)
    todo = "; ".join(blocked.items)
    summary = f"{blocked.reason}{' — to do: ' + todo if todo else ''}"[:1000]
    on = _BLOCKED_ON.get(blocked.needs, "you")
    return InboxEntry(
        entry_key=(
            f"agent:session:{session_id}:blocked:{blocked.key}"
            if blocked.key
            else f"agent:session:{session_id}:blocked:{blocked.since or 'unknown'}"
        ),
        source=AGENT_KIND,
        kind="session.blocked",
        occurred_at=_when(blocked.since) or ctx.clock(),
        observed_at=None,
        title=(
            f"{ref} session {name or session_id} is blocked on {on}"
            if ref
            else f"Session {name or session_id} is blocked on {on}"
        ),
        summary=summary,
        project_slug=None if project is None else project.slug,
        session_id=session_id,
        work_item_ref=ref,
        source_subject_key=session_id,
        trust_state="unverified",
        freshness="live",
        provisional=True,
        action=InboxAction(kind="session", session_id=session_id),
        **_actor_fields(SYSTEM_ACTOR),
    )


def _grant_entry(
    ctx: AppContext,
    grant: SessionGrantView,
    view: ReadView,
    projects: dict[str, Project],
    live: dict[str, EngineSession],
) -> InboxEntry:
    """A grant a session asked for and a person has yet to decide (WI-973).

    The reason is the asking agent's text — untrusted, shown verbatim, never
    acted on. What is asked for (secret and project), for whom (the target as
    the person knows it: title, role, agent, project) and by whom (the
    requester, and whether it is asking for itself) are spelled out, because
    that is what the person approves. The requester chose `target`, `var`
    and the reason; the names here come from the engine and the core, not
    from the request.
    """
    target = live.get(grant.target)
    declared = view.session_by_engine_id(grant.target)
    project = None if declared is None else projects.get(declared.project_id)
    label = grant.target if target is None else (target.name or grant.target)
    facts: list[str] = []
    if target is None:
        facts.append("not running now")
    else:
        facts.append(target.role)
        if target.conversation_agent:
            facts.append(target.conversation_agent)
        if target.permission_mode:
            facts.append(f"permission {target.permission_mode}")
    if project is not None:
        facts.append(f"project {project.slug}")
    if declared is not None:
        facts.append(declared.id)
    who = (
        "itself"
        if grant.requested_by == f"agent:engine:{grant.target}"
        or (
            declared is not None
            and grant.requested_by == f"agent:session:{declared.id}"
        )
        else f"session {label} ({', '.join(facts)})"
    )
    item = f"{grant.secret_name} (project {grant.project_id}) as {grant.var}"
    uses = "one fetch" if grant.uses == "once" else "any number of fetches"
    summary = (
        f"{grant.requested_by} asks for {item} for {who}: {uses}, for "
        f"{grant.ttl_seconds // 60} min once approved. Reason: {grant.reason}"
    )[:1000]
    requester = view.actor_by_identity(grant.requested_by)
    return InboxEntry(
        entry_key=f"agent:grant:{grant.id}",
        source=AGENT_KIND,
        kind="session.grant_request",
        occurred_at=grant.requested_at,
        observed_at=None,
        title=f"Grant request: {grant.secret_name} for session {label}",
        summary=summary,
        project_slug=None if project is None else project.slug,
        session_id=grant.target,
        source_subject_key=grant.id,
        trust_state="unverified",
        freshness="live",
        provisional=True,
        action=InboxAction(kind="grant", grant_id=grant.id, session_id=grant.target),
        evidence_snapshot={
            "grant_id": grant.id,
            "target": grant.target,
            "target_name": None if target is None else target.name,
            "target_role": None if target is None else target.role,
            "target_agent": None if target is None else target.conversation_agent,
            "target_permission_mode": (
                None if target is None else target.permission_mode
            ),
            "target_alive": target is not None,
            "target_session": None if declared is None else declared.id,
            "project": None if project is None else project.slug,
            "requested_by": grant.requested_by,
            "requester_is_target": who == "itself",
            "var": grant.var,
            "project_id": grant.project_id,
            "secret_name": grant.secret_name,
            "uses": grant.uses,
            "ttl_seconds": grant.ttl_seconds,
        },
        actor_login=grant.requested_by,
        actor_kind="human"
        if requester is not None and requester.kind == "human"
        else "bot",
        actor_relation="org_member",
    )


def _report_entry(
    view: ReadView, row: SessionReport, projects: dict[str, Project]
) -> InboxEntry:
    """A session's latest typed report (WI-2039). The agent's text: untrusted,
    shown verbatim, never acted on."""
    report = view_of(view, row)
    item = None if row.work_item_id is None else view.work_item_by_id(row.work_item_id)
    project = None if item is None else projects.get(item.project_id or "")
    who = report.reporter or row.engine_session_id or "a session"
    return InboxEntry(
        entry_key=f"agent:report:{row.id}",
        source=AGENT_KIND,
        kind=f"session.report.{row.kind}",
        occurred_at=row.created_at,
        observed_at=None,
        title=f"Report ({row.kind}) from {who}"[:200],
        summary=one_line(report)[:1000],
        project_slug=None if project is None else project.slug,
        work_item_ref=report.work_item,
        session_id=row.engine_session_id,
        source_subject_key=row.id,
        trust_state="unverified",
        freshness="live",
        provisional=True,
        action=None
        if row.engine_session_id is None
        else InboxAction(kind="session", session_id=row.engine_session_id),
        evidence_snapshot=report.model_dump(mode="json"),
        actor_login=report.reporter,
        actor_kind="bot",
        actor_relation="org_member",
    )


def _decision_entry(decision: DecisionView, view: ReadView) -> InboxEntry:
    """A question an agent asked and a person has yet to answer (WI-1321).

    The question, options and context are the asking agent's text —
    untrusted, shown verbatim, never acted on. Nothing is applied when the
    person answers; the asker reads the answer.
    """
    recommended = next(
        (o.label for o in decision.options if o.recommended), decision.options[0].label
    )
    options = " | ".join(
        o.label + (" (recommended)" if o.recommended else "") for o in decision.options
    )
    summary = (f"{decision.asker} asks: {decision.question} Options: {options}.")[:1000]
    item = (
        None
        if decision.work_item is None
        else view.work_item_by_ref(decision.work_item)
    )
    project = (
        None
        if item is None or item.project_id is None
        else view.project_by_id(item.project_id)
    )
    asker = view.actor_by_identity(decision.asker)
    return InboxEntry(
        entry_key=f"agent:decision:{decision.id}",
        source=AGENT_KIND,
        kind="decision.pending",
        occurred_at=decision.created_at,
        observed_at=None,
        title=f"Decision: {decision.question[:120]}",
        summary=summary,
        project_slug=None if project is None else project.slug,
        work_item_ref=decision.work_item,
        session_id=decision.asker_session,
        source_subject_key=decision.id,
        trust_state="unverified",
        freshness="live",
        provisional=True,
        action=InboxAction(
            kind="decision",
            decision_id=decision.id,
            session_id=decision.asker_session,
        ),
        evidence_snapshot={
            "decision_id": decision.id,
            "asker": decision.asker,
            "asker_session": decision.asker_session,
            "work_item": decision.work_item,
            "question": decision.question,
            "options": [o.model_dump(mode="json") for o in decision.options],
            "recommended": recommended,
            "context": decision.context,
            "expires_at": decision.expires_at.isoformat(),
        },
        actor_login=decision.asker,
        actor_kind="human" if asker is not None and asker.kind == "human" else "bot",
        actor_relation="org_member",
    )


def _override_entry(
    view: ReadView, record: AuditRecord, projects: dict[str, Project]
) -> InboxEntry:
    """A start oversight made past the dispatch rules (WI-1329).

    The reason is the overseer's override text — untrusted, shown verbatim.
    """
    declared = view.session_by_id(record.entity_id)
    project = None if declared is None else projects.get(declared.project_id)
    ref = _bound_ref(view, declared, None)
    label = f"{ref} session {record.entity_id}" if ref else record.entity_id
    return InboxEntry(
        entry_key=f"agent:dispatch.override:{record.id}",
        source=AGENT_KIND,
        kind=DISPATCH_OVERRIDE_KIND,
        occurred_at=record.at,
        observed_at=None,
        title=f"Dispatch override: {label} started by {record.actor_identity_ref}",
        summary=record.reason[:1000],
        project_slug=None if project is None else project.slug,
        session_id=(
            record.entity_id if declared is None else declared.engine_session_id
        ),
        work_item_ref=ref,
        source_subject_key=record.entity_id,
        trust_state="unverified",
        freshness="live",
        provisional=True,
        action=InboxAction(
            kind="session",
            session_id=(
                record.entity_id if declared is None else declared.engine_session_id
            ),
        ),
        **_actor_fields(SYSTEM_ACTOR),
    )


def _session_entry(
    ctx: AppContext,
    session_id: str,
    name: str,
    activity: str,
    activity_changed_at: str | None,
    declared: CodingSession | None,
    projects: dict[str, Project],
    *,
    approval: EngineApproval | None = None,
    view: ReadView,
    work_label: str | None = None,
) -> InboxEntry:
    project = None if declared is None else projects.get(declared.project_id)
    ref = _bound_ref(view, declared, work_label)
    label = f"{ref} {name or session_id}" if ref else (name or session_id)
    if activity == "awaiting-approval":
        # A permission dialog denies itself on a countdown: say what it asks
        # and how long is left, so it can be answered from the Inbox row.
        title = f"Session {label} is asking for approval"
        what = "" if approval is None else " ".join(approval.command_excerpt.split())
        left = (
            ""
            if approval is None or approval.deadline_seconds is None
            else f" Auto-deny in {approval.deadline_seconds}s."
        )
        summary = (
            f"{approval.question if approval else 'Permission dialog.'}{left} {what}"
        )[:1000].strip()
    else:
        title = f"Session {label} needs attention"
        summary = f"Session is {activity}."
    return InboxEntry(
        entry_key=(
            f"agent:session:{session_id}:{activity}:{activity_changed_at or 'unknown'}"
        ),
        source=AGENT_KIND,
        kind="session.attention",
        occurred_at=_when(activity_changed_at) or ctx.clock(),
        observed_at=None,
        title=title,
        summary=summary,
        project_slug=None if project is None else project.slug,
        session_id=session_id,
        work_item_ref=ref,
        source_subject_key=session_id,
        trust_state="unverified",
        freshness="live",
        provisional=True,
        action=InboxAction(kind="session", session_id=session_id),
        **_actor_fields(SYSTEM_ACTOR),
    )


def _bound_ref(
    view: ReadView, declared: CodingSession | None, label: str | None
) -> str | None:
    """The work item a session serves (WI-998): the core's row for a session
    Vogt started — it is the truth — and the engine's label for one the GUI
    started. Named on the entry so it reads "WI-983 session … is blocked"
    and the Inbox's work-item filter catches it."""
    if declared is not None:
        if declared.work_item_id is None:
            return None
        item = view.work_item_by_id(declared.work_item_id)
        return None if item is None else item.ref
    return label


def _apply_triage(
    ctx: AppContext, decisions: Mapping[str, InboxTriage], entry: InboxEntry
) -> InboxEntry:
    triage = decisions.get(entry.entry_key)
    if triage is None or (
        triage.state == "snoozed"
        and triage.snooze_until is not None
        and triage.snooze_until <= ctx.clock()
    ):
        return entry
    return entry.model_copy(
        update={"triage_state": triage.state, "snooze_until": triage.snooze_until}
    )


def _entry_by_key(ctx: AppContext, view: ReadView, key: str) -> InboxEntry | None:
    return next(
        (entry for entry in _collect(ctx, view) if entry.entry_key == key), None
    )


def _triage_matches(
    entry: InboxEntry, states: Sequence[InboxState], now: datetime
) -> bool:
    state = entry.triage_state
    if (
        state == "snoozed"
        and entry.snooze_until is not None
        and entry.snooze_until <= now
    ):
        state = "active"
    return state in states


def _sort_key(entry: InboxEntry) -> tuple[datetime, str]:
    return (
        entry.occurred_at or entry.observed_at or datetime.min.replace(tzinfo=UTC),
        entry.entry_key,
    )


def _fingerprint(
    params: InboxListParams, project_ids: dict[str, str], work_item_id: str | None
) -> str:
    material: dict[str, object] = {
        "sources": params.sources,
        "states": params.triage_states,
        "project": sorted(project_ids),
        "work_item": work_item_id,
    }
    # The actor filter joins the fingerprint only when it narrows, so a
    # cursor minted before the filter existed still pages an `any` read.
    if params.actor != "any":
        material["actor"] = params.actor
    return digest_of(material)


def _encode_cursor(
    fingerprint: str,
    entry: InboxEntry,
    *,
    snapshot_at: datetime,
    high_water: dict[InboxSource, datetime | None],
) -> str:
    moment = _entry_moment(entry)
    raw = json.dumps(
        {
            "fingerprint": fingerprint,
            "occurred_at": moment.isoformat(),
            "entry_key": entry.entry_key,
            "snapshot_at": snapshot_at.isoformat(),
            "high_water": {
                source: value.isoformat() if value is not None else None
                for source, value in high_water.items()
            },
        }
    ).encode()
    return base64.urlsafe_b64encode(raw).decode().rstrip("=")


def _cursor_index(entries: list[InboxEntry], cursor: dict[str, Any] | None) -> int:
    if cursor is None:
        return 0
    try:
        key = (datetime.fromisoformat(cursor["occurred_at"]), cursor["entry_key"])
        for index, entry in enumerate(entries):
            if _sort_key(entry) < key:
                return index
        return len(entries)
    except (ValueError, KeyError, TypeError, json.JSONDecodeError):
        raise InvalidCursor(
            "cursor is malformed or belongs to another Inbox query"
        ) from None


def _decode_cursor(cursor: str, fingerprint: str) -> dict[str, Any]:
    try:
        raw = base64.urlsafe_b64decode(cursor + "=" * (-len(cursor) % 4))
        value = json.loads(raw)
        if not isinstance(value, dict) or value.get("fingerprint") != fingerprint:
            raise ValueError
        if not isinstance(value.get("occurred_at"), str) or not isinstance(
            value.get("entry_key"), str
        ):
            raise ValueError
        datetime.fromisoformat(value["occurred_at"])
        if not isinstance(value.get("snapshot_at"), str):
            raise ValueError
        datetime.fromisoformat(value["snapshot_at"])
        return value
    except (binascii.Error, ValueError, KeyError, TypeError, json.JSONDecodeError):
        raise InvalidCursor(
            "cursor is malformed or belongs to another Inbox query"
        ) from None


def _cursor_high_water(
    cursor: dict[str, Any] | None,
) -> dict[InboxSource, datetime] | None:
    if cursor is None:
        return None
    raw = cursor.get("high_water")
    if not isinstance(raw, dict):
        raise InvalidCursor("cursor does not carry a high-water mark")
    result: dict[InboxSource, datetime] = {}
    for source in (GITHUB_KIND, DRIFT_KIND, CI_KIND, AGENT_KIND):
        value = raw.get(source)
        if value is not None:
            if not isinstance(value, str):
                raise InvalidCursor("cursor has an invalid high-water mark")
            try:
                result[source] = datetime.fromisoformat(value)
            except ValueError:
                raise InvalidCursor("cursor has an invalid high-water mark") from None
    return result


def _cursor_snapshot(cursor: dict[str, Any]) -> datetime:
    try:
        return datetime.fromisoformat(cursor["snapshot_at"])
    except (ValueError, KeyError, TypeError):
        raise InvalidCursor("cursor has an invalid snapshot time") from None


def _high_water(entries: list[InboxEntry]) -> dict[InboxSource, datetime | None]:
    result: dict[InboxSource, datetime | None] = dict.fromkeys(
        (GITHUB_KIND, DRIFT_KIND, CI_KIND, AGENT_KIND), None
    )
    for entry in entries:
        moment = entry.occurred_at or entry.observed_at
        if moment is None:
            continue
        previous = result[entry.source]
        if previous is None or moment > previous:
            result[entry.source] = moment
    return result


def _within_water(entry: InboxEntry, water: dict[InboxSource, datetime | None]) -> bool:
    high = water.get(entry.source)
    return high is not None and _entry_moment(entry) <= high


def _coverage(
    ctx: AppContext,
    view: ReadView,
    entries: list[InboxEntry],
    *,
    registered: int | None = None,
) -> dict[InboxSource, InboxCoverage]:
    if registered is None:
        registered = len(view.list_projects(limit=MAX_SCAN, offset=0))
    sweeps = ctx.observed.coverage()
    result: dict[InboxSource, InboxCoverage] = {}
    for source, collector in (
        (GITHUB_KIND, COLLECTOR_NOTIFICATIONS),
        (CI_KIND, COLLECTOR_CHECKS),
    ):
        sweep = sweeps.get(collector)
        relevant = [e for e in entries if e.source == source]
        result[source] = InboxCoverage(
            source=source,
            status="unswept" if sweep is None else sweep.outcome,
            count=len(relevant),
            observed_at=None if sweep is None else sweep.finished_at,
            registered=registered,
            detail=None
            if sweep is not None
            else "this collector has not completed a sweep",
        )
    result[DRIFT_KIND] = InboxCoverage(
        source=DRIFT_KIND,
        status="current",
        count=sum(e.source == DRIFT_KIND for e in entries),
        registered=registered,
        detail="open proposals in the declared store",
    )
    result[AGENT_KIND] = InboxCoverage(
        source=AGENT_KIND,
        status="current" if ctx.engine is not None else "unconfigured",
        count=sum(e.source == AGENT_KIND for e in entries),
        registered=registered,
        detail=None if ctx.engine is not None else "no session engine is configured",
    )
    return result


def _engine_status(
    ctx: AppContext,
    *,
    probed: tuple[object, EngineUnavailable | None] | None = None,
) -> tuple[EngineStatus, str | None]:
    """Whether the engine answered. `probed` is a `list_sessions` this
    request already made, so the status line does not make another."""
    if ctx.engine is None:
        return "not_configured", "no session engine is configured"
    error = _read_engine_sessions(ctx)[1] if probed is None else probed[1]
    if error is not None:
        return "unreachable", str(error)
    return "available", None


def _entry_moment(entry: InboxEntry) -> datetime:
    moment = entry.occurred_at or entry.observed_at
    if moment is None:
        raise InvalidCursor("Inbox entry has no timestamp")
    return moment


def _text(value: object) -> str | None:
    return value if isinstance(value, str) and value else None


def _when(value: object) -> datetime | None:
    if not isinstance(value, str) or not value:
        return None
    try:
        return datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None


__all__ = [
    "InboxBadge",
    "actor_matches",
    "archive_inbox",
    "clear_badge_cache",
    "inbox_badge",
    "list_inbox",
    "restore_inbox",
    "snooze_inbox",
]
