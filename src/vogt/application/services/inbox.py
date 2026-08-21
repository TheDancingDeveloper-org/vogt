"""The normalized attention Inbox and its audited triage actions (FR-N4/N5).

The browser receives one server-owned projection. It never merges forge
notifications, drift, CI, or live engine state itself; this module does the
joins, ordering, coverage disclosure, and occurrence-scoped decisions.
"""

from __future__ import annotations

import base64
import binascii
import json
from collections.abc import Sequence
from datetime import UTC, datetime
from typing import Any, Literal, cast

from vogt.adapters.engine import EngineUnavailable
from vogt.adapters.forge import KIND_CHECK, KIND_NOTIFICATION
from vogt.adapters.forge.kinds import COLLECTOR_CHECKS, COLLECTOR_NOTIFICATIONS
from vogt.application.context import AppContext
from vogt.application.models import (
    InboxAction,
    InboxArchiveParams,
    InboxCoverage,
    InboxEntry,
    InboxListParams,
    InboxListResult,
    InboxRestoreParams,
    InboxSnoozeParams,
    InboxTriageResult,
)
from vogt.application.services import _resolve
from vogt.application.services.views import trust_for
from vogt.application.writes import WriteOutcome, audited_write
from vogt.collectors.session_outcomes import KIND_TASK_RUN
from vogt.core.checks import roll_up
from vogt.core.digest import digest_of
from vogt.core.entities import (
    Actor,
    CodingSession,
    DriftProposal,
    InboxTriage,
    Observation,
    Project,
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
MAX_SCAN = 10_000
Cursor = tuple[str, str]


def list_inbox(ctx: AppContext, params: InboxListParams) -> InboxListResult:
    snapshot_at = ctx.clock()
    with ctx.declared.read() as view:
        entries = _collect(ctx, view)
        project_ids = {
            p.slug: p.id for p in view.list_projects(limit=MAX_SCAN, offset=0)
        }
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
        coverage = _coverage(ctx, view, entries)
        engine_status, engine_detail = _engine_status(ctx)
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


def _collect(ctx: AppContext, view: ReadView) -> list[InboxEntry]:
    projects = {p.id: p for p in view.list_projects(limit=MAX_SCAN, offset=0)}
    entries: list[InboxEntry] = []

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
            )
        )

    for observation in checks_all:
        if _text(observation.payload.get("conclusion")) in (None, "success", "skipped"):
            continue
        project = projects.get(observation.project_id or "")
        if project is None:
            continue
        checks = ctx.observed.latest(
            kinds=(KIND_CHECK,), project_id=project.id, limit=MAX_SCAN
        )
        rolled = roll_up(checks)
        if rolled is None or observation not in rolled.checks:
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
            )
        )

    for proposal in view.list_drift(status="open", limit=MAX_SCAN):
        entries.append(_drift_entry(ctx, proposal, projects, view))

    if ctx.engine is not None:
        try:
            live = ctx.engine.list_sessions()
            for session in live:
                if session.activity not in ("waiting-for-input", "errored"):
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
                    )
                )
        except EngineUnavailable:
            pass
    return [_apply_triage(ctx, view, entry) for entry in entries]


def _observation_entry(
    ctx: AppContext,
    observation: Observation,
    projects: dict[str, Project],
    links: dict[str, str],
    source: Literal["github", "ci"],
    title: str,
    summary: str,
    occurred: datetime,
) -> InboxEntry:
    project = projects.get(observation.project_id or "")
    ref = links.get(observation.subject_key)
    material = {
        "subject_key": observation.subject_key,
        "title": title,
        "summary": summary,
        "source_url": observation.source_url,
        "payload": {
            key: value
            for key, value in observation.payload.items()
            if key not in {"unread", "last_read", "observed_at"}
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
    )


def _session_entry(
    ctx: AppContext,
    session_id: str,
    name: str,
    activity: str,
    activity_changed_at: str | None,
    declared: CodingSession | None,
    projects: dict[str, Project],
) -> InboxEntry:
    project = None if declared is None else projects.get(declared.project_id)
    return InboxEntry(
        entry_key=(
            f"agent:session:{session_id}:{activity}:{activity_changed_at or 'unknown'}"
        ),
        source=AGENT_KIND,
        kind="session.attention",
        occurred_at=_when(activity_changed_at) or ctx.clock(),
        observed_at=None,
        title=f"Session {name or session_id} needs attention",
        summary=f"Session is {activity}.",
        project_slug=None if project is None else project.slug,
        session_id=session_id,
        work_item_ref=None,
        source_subject_key=session_id,
        trust_state="unverified",
        freshness="live",
        provisional=True,
        action=InboxAction(kind="session", session_id=session_id),
    )


def _apply_triage(ctx: AppContext, view: ReadView, entry: InboxEntry) -> InboxEntry:
    triage = view.inbox_triage_by_key(entry.entry_key)
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
    return digest_of(
        {
            "sources": params.sources,
            "states": params.triage_states,
            "project": sorted(project_ids),
            "work_item": work_item_id,
        }
    )


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
    ctx: AppContext, view: ReadView, entries: list[InboxEntry]
) -> dict[InboxSource, InboxCoverage]:
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


def _engine_status(ctx: AppContext) -> tuple[EngineStatus, str | None]:
    if ctx.engine is None:
        return "not_configured", "no session engine is configured"
    try:
        ctx.engine.list_sessions()
    except EngineUnavailable as error:
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


__all__ = ["archive_inbox", "list_inbox", "restore_inbox", "snooze_inbox"]
