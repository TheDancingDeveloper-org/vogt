"""Reading the agent activity index: what agents did, and how it went.

The index is built by the `agent-activity` collector from Claude Code and
Codex transcripts (`collectors/agent_activity.py`) and lives in the observed
store. Two joins to the declared store happen here, at read time, because
both can change after a call was indexed and neither belongs in the index:

- **Project.** A call belongs to the registered project whose root holds its
  working directory (the longest such root, so a nested project wins). A
  project registered tomorrow claims yesterday's calls.
- **Vogt session.** Claude Code sessions Vogt starts use the engine session id
  as their conversation id, so a conversation id that names a Vogt session's
  engine id *is* that session. Nothing else is inferred: a Codex conversation,
  or a Claude one resumed under an older id, links to nothing rather than to
  a guess.

Answers are honest about absence: an instance that never configured a
transcript root, or has not swept since it did, says so in `detail` rather
than returning an empty list that reads as "agents did nothing".
"""

from __future__ import annotations

from collections.abc import Iterable
from datetime import UTC, datetime
from pathlib import Path

from vogt.application.context import AppContext
from vogt.application.models import (
    AgentActivityEvent,
    AgentActivitySearchParams,
    AgentActivitySearchResult,
    AgentActivitySessionSummary,
    AgentActivitySummaryParams,
    AgentActivitySummaryResult,
)
from vogt.application.services import _resolve
from vogt.collectors.agent_activity import COLLECTOR_NAME
from vogt.errors import NotFound
from vogt.storage.observed_types import ActivityQuery

NOT_CONFIGURED = (
    "agent activity is not collected: no transcript roots are configured "
    "(set `agent_activity_roots`)"
)
NOT_YET_INDEXED = (
    "agent activity is configured but has not been indexed yet: the next sweep "
    "will, or run `vogt sweep --collectors agent-activity`"
)


def search_agent_activity(
    ctx: AppContext, params: AgentActivitySearchParams
) -> AgentActivitySearchResult:
    """Tool calls agents made, newest first, narrowed by every given filter."""
    indexed_at, unavailable = _availability(ctx)
    if unavailable is not None:
        return AgentActivitySearchResult(
            events=[], total=0, indexed_at=indexed_at, detail=unavailable
        )
    projects = _project_roots(ctx)
    query = ActivityQuery(
        q=(params.q or "").strip() or None,
        service=(params.service or "").strip().lower() or None,
        tool=(params.tool or "").strip() or None,
        errors_only=params.errors_only,
        since=_aware(params.since),
        agent_session_ids=_session_ids(ctx, params.session),
        cwd_roots=_cwd_roots(ctx, params.project),
    )
    rows = ctx.observed.search_activity(query, limit=params.limit, offset=params.offset)
    links = _vogt_sessions(ctx, {row.agent_session_id for row in rows})
    events = [
        AgentActivityEvent(
            id=row.id,
            at=row.at,
            finished_at=row.finished_at,
            duration_ms=(
                None
                if row.finished_at is None
                else max(0, int((row.finished_at - row.at).total_seconds() * 1000))
            ),
            agent=row.agent,
            agent_session_id=row.agent_session_id,
            vogt_session_id=links.get(row.agent_session_id),
            project=_project_of(projects, row.cwd),
            cwd=row.cwd,
            tool=row.tool,
            summary=row.summary,
            services=list(row.services),
            error=row.error,
            excerpt=row.excerpt,
        )
        for row in rows
    ]
    return AgentActivitySearchResult(
        events=events,
        total=len(events),
        next_offset=(
            params.offset + len(events) if len(events) == params.limit else None
        ),
        indexed_at=indexed_at,
    )


def summarize_agent_activity(
    ctx: AppContext, params: AgentActivitySummaryParams
) -> AgentActivitySummaryResult:
    """Per agent conversation: tool counts, error rate, time waiting on tools."""
    indexed_at, unavailable = _availability(ctx)
    if unavailable is not None:
        return AgentActivitySummaryResult(
            sessions=[], total=0, indexed_at=indexed_at, detail=unavailable
        )
    projects = _project_roots(ctx)
    query = ActivityQuery(
        since=_aware(params.since),
        agent_session_ids=_session_ids(ctx, params.session),
        cwd_roots=_cwd_roots(ctx, params.project),
    )
    rows = ctx.observed.summarize_activity(
        query, limit=params.limit, offset=params.offset
    )
    links = _vogt_sessions(ctx, {row.agent_session_id for row in rows})
    sessions = [
        AgentActivitySessionSummary(
            agent=row.agent,
            agent_session_id=row.agent_session_id,
            vogt_session_id=links.get(row.agent_session_id),
            project=_project_of(projects, row.cwd),
            cwd=row.cwd,
            first_at=row.first_at,
            last_at=row.last_at,
            calls=row.calls,
            errors=row.errors,
            error_rate=round(row.errors / row.calls, 4) if row.calls else 0.0,
            tool_wait_ms=row.wait_ms,
            unfinished=row.calls - row.finished,
            tools=row.tools,
            services=row.services,
        )
        for row in rows
    ]
    return AgentActivitySummaryResult(
        sessions=sessions,
        total=len(sessions),
        next_offset=(
            params.offset + len(sessions) if len(sessions) == params.limit else None
        ),
        indexed_at=indexed_at,
    )


# -- helpers ---------------------------------------------------------------


def _availability(ctx: AppContext) -> tuple[datetime | None, str | None]:
    """When the index last finished a sweep, and why there is none if not."""
    configured = bool(ctx.config.agent_activity_roots)
    if not ctx.observed.has_evidence_tables():
        return None, NOT_CONFIGURED if not configured else NOT_YET_INDEXED
    sweep = ctx.observed.coverage().get(COLLECTOR_NAME)
    if sweep is None:
        return None, NOT_CONFIGURED if not configured else NOT_YET_INDEXED
    # Indexed before and since switched off: what was indexed is still
    # answerable, and is answered.
    return sweep.finished_at, None


def _aware(moment: datetime | None) -> datetime | None:
    if moment is None or moment.tzinfo is not None:
        return moment
    return moment.replace(tzinfo=UTC)


def _session_ids(ctx: AppContext, session: str | None) -> tuple[str, ...] | None:
    """The agent conversation ids a `session` filter means."""
    wanted = (session or "").strip()
    if not wanted:
        return None
    if not wanted.startswith("ses_"):
        # An engine session id or the agent's own conversation id: for a
        # Claude Code session Vogt started they are the same value.
        return (wanted,)
    with ctx.declared.read() as view:
        found = view.session_by_id(wanted)
    if found is None:
        msg = f"no session {wanted!r}"
        raise NotFound(msg)
    return (found.engine_session_id,)


def _cwd_roots(ctx: AppContext, project: str | None) -> tuple[str, ...] | None:
    slug = (project or "").strip()
    if not slug:
        return None
    with ctx.declared.read() as view:
        found = _resolve.project(view, slug)
    return (str(Path(found.root_path).expanduser()),)


def _project_roots(ctx: AppContext) -> list[tuple[str, str]]:
    """Registered project roots, longest first, so a nested project wins."""
    with ctx.declared.read() as view:
        projects = view.list_projects(limit=10_000, offset=0)
    roots = [
        (str(Path(project.root_path).expanduser()).rstrip("/"), project.slug)
        for project in projects
        if project.root_path
    ]
    return sorted(roots, key=lambda item: -len(item[0]))


def _project_of(roots: list[tuple[str, str]], cwd: str | None) -> str | None:
    if not cwd:
        return None
    for root, slug in roots:
        if root and (cwd == root or cwd.startswith(root + "/")):
            return slug
    return None


def _vogt_sessions(ctx: AppContext, agent_ids: Iterable[str]) -> dict[str, str]:
    """Agent conversation id → Vogt session id, where one is derivable."""
    links: dict[str, str] = {}
    with ctx.declared.read() as view:
        for agent_id in sorted(set(agent_ids)):
            session = view.session_by_engine_id(agent_id)
            if session is not None:
                links[agent_id] = session.id
    return links
