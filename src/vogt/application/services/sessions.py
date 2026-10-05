"""Coding sessions — the work opening a terminal on itself.

A session is a PTY the engine runs, in a project's working tree, for a
project or for one work item. Vogt does not run it and does not watch it: it
decides *where* it opens and *who* it writes as, records the link, and asks
the engine for the live state whenever somebody looks.

Three rules this module exists to keep:

- **The working directory comes from the registry, never from a heuristic**
 . The engine would happily default to its workspace root, and a
  session that opened there when Vogt meant a project's tree would be
  plausible and wrong.
- **The terminal starts before the declared write.** `project.import` orders
  its clone the same way and says why: the failure mode is then a directory
  nobody registered rather than a project pointing at nothing. Here it is a
  terminal nobody recorded rather than a work item claiming a session that
  never started. The token minted for the session is worthless until that
  write lands — it is only a hash in a row that does not exist yet — so a
  half-failed start leaves nothing that can act.
- **Nothing about a running process is cached.** Activity comes from the
  engine at the moment of asking, and is `None` when the engine cannot be
  asked. A stored activity state would be a claim about a process
  this half of the product does not own.
"""

from __future__ import annotations

import time
from datetime import datetime, timedelta
from typing import Any

from vogt.adapters import transcripts
from vogt.adapters.engine import EngineClient, EngineSession, EngineUnavailable
from vogt.adapters.engine.client import (
    EngineApproval,
    EngineBlocked,
    EngineHibernation,
    EngineResources,
    EngineScreen,
    EngineSweepEntry,
)
from vogt.application import writes
from vogt.application.context import AppContext
from vogt.application.models import (
    SESSION_INPUT_MAX_BYTES,
    AnswerSessionParams,
    HibernateSessionParams,
    HistoryListParams,
    HistoryListResult,
    HistoryOutputMatch,
    HistorySessionRow,
    KeepSessionAwakeParams,
    ListSessionsParams,
    LogTailParams,
    LogTailResult,
    ReportBlockedParams,
    ReportUnblockedParams,
    SearchOutputParams,
    SearchOutputResult,
    SessionAnswerResult,
    SessionApproval,
    SessionApprovalOption,
    SessionBlocked,
    SessionBlockedResult,
    SessionHibernation,
    SessionInputParams,
    SessionInputResult,
    SessionKey,
    SessionLastReplyParams,
    SessionLastReplyResult,
    SessionListResult,
    SessionReply,
    SessionResources,
    SessionResult,
    SessionRuntime,
    SessionScreenCursor,
    SessionScreenParams,
    SessionScreenResult,
    SessionSummary,
    SessionSweepResult,
    SessionSweepRow,
    SessionWaitParams,
    SessionWaitResult,
    StartSessionParams,
    StopSessionParams,
    SweepSessionsParams,
    WakeSessionParams,
    WhyParams,
    WhyResult,
)
from vogt.application.services import _resolve
from vogt.application.services._brief import (
    AUTOPILOT,
    brief_for_project,
    brief_for_work_item,
)
from vogt.application.services.views import why
from vogt.application.writes import WriteOutcome, audited_action, audited_write
from vogt.core import delivery, oversight, runtime
from vogt.core.auth import Scope, issue, parse_scopes
from vogt.core.branches import default_branch_name
from vogt.core.entities import Actor, CodingSession, Token, WorkItem, WorkOverlay
from vogt.errors import BypassRefused, Conflict, InvalidRequest, NotFound, VogtError
from vogt.storage.interface import ReadView, WriteTxn

SESSION_START = "session.start"
SESSION_STOP = "session.stop"
SESSION_STARTED_EVENT = "session.started"
SESSION_STOPPED_EVENT = "session.stopped"
SESSION_INPUT = "session.input"
SESSION_INPUT_EVENT = "session.input"
SESSION_HIBERNATE = "session.hibernate"
SESSION_HIBERNATED_EVENT = "session.hibernated"
SESSION_WAKE = "session.wake"
SESSION_WOKEN_EVENT = "session.woken"
SESSION_KEEP_AWAKE = "session.keep_awake"
SESSION_KEEP_AWAKE_EVENT = "session.keep_awake"

#: The byte sequence an xterm-compatible terminal sends for each named key.
#: Arrows are the normal-mode CSI forms (`ESC [ A`), which every TUI this
#: product drives (shells, Claude Code, Codex) reads.
SESSION_KEYS: dict[SessionKey, str] = {
    "enter": "\r",
    "esc": "\x1b",
    "tab": "\t",
    "up": "\x1b[A",
    "down": "\x1b[B",
    "right": "\x1b[C",
    "left": "\x1b[D",
    "ctrl-c": "\x03",
    "ctrl-d": "\x04",
    "backspace": "\x7f",
}


#: What a session's own token may do is one deployment decision, set by
#: `agent_session_scopes` (env `VOGT_AGENT_SESSION_SCOPES`) and applied to every
#: session however it was launched — the default is everything except `admin`.
#: Per-session *attribution* is unchanged: each `session.start` still mints its
#: own actor-bound token; only the scope set is deployment-chosen (FR-S10).
def _session_scopes(ctx: AppContext) -> tuple[Scope, ...]:
    return parse_scopes(ctx.config.agent_session_scopes)


def start_session(ctx: AppContext, params: StartSessionParams) -> SessionResult:
    """Open a terminal for a work item or a project."""
    # Validate everything we can before crossing the process boundary. The
    # engine and SQLite cannot share a transaction, so rejecting the reason
    # only when ``audited_write`` begins would leave a running terminal that
    # Vogt never records. Keep the cleaned value for both the entity and the
    # audit row so they cannot disagree about whitespace.
    reason = writes.validate_reason(params.reason)
    if params.permission_mode == "bypass" and ctx.principal.kind == "agent":
        # Full bypass is a person's grant for one trusted task (WI-926). An
        # agent — the overseeing runner included — that could start a child
        # without permission checks would escape its own guardrails by proxy.
        msg = (
            "permission_mode=bypass can only be granted by a person, not by "
            f"an agent ({ctx.principal.identity_ref}); start the session from "
            "the GUI or the CLI, or use the default posture and report the "
            "denied action as blocked"
        )
        raise BypassRefused(msg)
    engine = _engine(ctx)
    session_id = ctx.id_factory("ses")
    subject = _subject(ctx, params, session_id)
    actor_ref = f"agent:session:{session_id}"
    session_scopes = _session_scopes(ctx)
    credential = issue(session_scopes)

    started = _start_on_engine(
        engine,
        name=params.name or subject.default_name,
        template=params.template,
        cwd=subject.cwd,
        env=_session_env(ctx, session_id, credential.secret),
        brief=_brief_with_task(subject.brief, params.task, autopilot=params.autopilot),
        model=params.model,
        effort=params.effort,
        resume=params.resume,
        permission_mode=params.permission_mode,
    )

    # A resumed conversation is started by the engine in the directory its
    # transcript records, which may be below or beside the project root
    # (WI-871). Record where it actually runs; every other session runs at
    # the registry's root, which is what the engine reports back anyway.
    cwd = started.cwd if params.resume and started.cwd else subject.cwd

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[SessionResult]:
        del actor
        now = ctx.clock()
        holder = Actor(
            id=ctx.id_factory("act"),
            identity_ref=actor_ref,
            kind="agent",
            display_name=f"Session {session_id}",
            created_at=now,
        )
        txn.insert_actor(holder)
        txn.insert_token(
            Token(
                id=ctx.id_factory("tok"),
                actor_id=holder.id,
                actor_identity_ref=holder.identity_ref,
                name=f"session {session_id}",
                scopes=list(session_scopes),
                created_at=now,
                expires_at=None,
            ),
            token_hash=credential.token_hash,
        )
        session = CodingSession(
            id=session_id,
            engine_session_id=started.id,
            project_id=subject.project_id,
            work_item_id=subject.work_item_id,
            actor_id=holder.id,
            cwd=cwd,
            template=params.template,
            model=params.model,
            effort=params.effort,
            reason=reason,
            started_at=now,
            stopped_at=None,
        )
        txn.insert_session(session)
        # The declared half of the branch binding: a session opened for
        # a work item records, on that item's overlay, the branch it will use.
        # Additive and forward-only — this writes a name, never a branch: git
        # is not touched, and the row rides this session's audit like every
        # other change in the transaction.
        declared_branch = (
            None
            if subject.work_item is None
            else _record_declared_branch(
                txn,
                work_ref=subject.work_item.ref,
                project_id=subject.project_id,
                at=now,
                template=ctx.config.branch_binding_template,
            )
        )
        return WriteOutcome(
            result=SessionResult(
                session=_summarize(txn, session, engine_session=started)
            ),
            entity_kind="session",
            entity_id=session.id,
            payload=_audited_payload(session),
            event_kind=SESSION_STARTED_EVENT,
            summary={
                "work_item": subject.work_item_ref,
                "branch": declared_branch,
                "project": subject.project_slug,
                "cwd": cwd,
                # Named in the audit summary because a spoken request that
                # resolved to the scratch project asked for neither, and a row
                # saying only which project it opened in would read as though
                # somebody chose it.
                "scratch": subject.is_scratch,
                "model": params.model,
                "effort": params.effort,
                # The agent conversation this session continues, when it was
                # asked to. Recorded on the audit row rather than the session:
                # it is what the session was *asked* to resume, the same
                # standing as `model`.
                "resume": params.resume,
                "autopilot": params.autopilot,
                # Named on the audit row: who started an unguarded session,
                # and why, is what an operator looks for afterwards.
                "permission_mode": params.permission_mode,
            },
        )

    return audited_write(ctx, operation=SESSION_START, reason=reason, body=body)


def stop_session(ctx: AppContext, params: StopSessionParams) -> SessionResult:
    """Stop a session and revoke the token it was running with.

    The kill is attempted first and its failure is not fatal: an engine that
    has already forgotten the session, or one that is down, must not leave
    Vogt unable to close its own record. A session Vogt believes is running
    when it is not is the worse of the two wrong answers.

    Either id form is accepted. A session Vogt never linked (started from the
    GUI) has no record or token to close, so stopping it is the kill alone,
    still audited against the engine's id.
    """
    target = _target(ctx, params.id)
    if target.session is None:
        return _stop_unlinked(ctx, target.engine_session_id, params.reason)
    session = target.session
    engine = ctx.engine
    killed: bool | None = None
    if engine is not None:
        try:
            killed = engine.kill_session(
                session.engine_session_id,
                reason=params.reason,
                by=ctx.principal.identity_ref,
            )
        except EngineUnavailable:
            killed = None

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[SessionResult]:
        del actor
        current = txn.session_by_id(session.id)
        if current is None:
            msg = f"no session {params.id!r}"
            raise NotFound(msg)
        if current.stopped_at is not None:
            # Said rather than absorbed, as `token.revoke` says it: a caller
            # who stops a session twice has a different picture of the world
            # from the store, and a silent success leaves them with it.
            msg = f"session {params.id!r} was already stopped"
            raise Conflict(msg)
        now = ctx.clock()
        txn.mark_session_stopped(current.id, at=now)
        # A session mints exactly one token, but revoking is written as a
        # loop over the actor's tokens rather than a lookup of that one: the
        # property promised is that nothing the session held still
        # works afterwards, and "the one we think we minted" is a weaker
        # claim than "everything this actor has".
        for token in txn.tokens_for_actor(current.actor_id):
            txn.revoke_token(token.id, reason=params.reason, at=now)
        stopped = txn.session_by_id(current.id)
        assert stopped is not None  # just written in this transaction
        return WriteOutcome(
            result=SessionResult(session=_summarize(txn, stopped, engine_session=None)),
            entity_kind="session",
            entity_id=stopped.id,
            payload=_audited_payload(stopped),
            event_kind=SESSION_STOPPED_EVENT,
            summary={"engine_killed": killed},
        )

    return audited_write(ctx, operation=SESSION_STOP, reason=params.reason, body=body)


def list_sessions(ctx: AppContext, params: ListSessionsParams) -> SessionListResult:
    """Vogt's session links, enriched with what the engine says right now,
    plus any live session the engine holds that Vogt never linked.

    The links are returned whether or not the engine answers. Its absence
    costs the liveness columns and nothing else, and the reason it
    could not be asked is reported rather than rendered as "not running".

    A session started outside Vogt — or one whose link Vogt lost — is still
    running, and an agent asking "what sessions are here" must be able to see
    it: reporting only Vogt's own links is why one session could not see
    another. Such unlinked sessions are appended (with `linked=False` and null
    declared fields) after the declared page. They carry no project or work
    item, so a `project`/`work_item` filter excludes them; and because they
    have no stable page position, they are appended only on the first page.
    """
    live: dict[str, EngineSession] = {}
    detail: str | None = None
    if ctx.engine is None:
        detail = "no session engine is configured (VOGT_ENGINE_URL is unset)"
    else:
        try:
            live = {row.id: row for row in ctx.engine.list_sessions()}
        except EngineUnavailable as exc:
            detail = str(exc)
    summaries = _rows(ctx, params, live, detail)
    if params.order == "rss":
        # Stable: sessions with no sample keep their order, after the
        # measured ones.
        summaries.sort(
            key=lambda s: (1, 0) if s.resources is None else (0, -s.resources.rss_bytes)
        )
    return SessionListResult(sessions=summaries, engine=detail)


def sweep_sessions(ctx: AppContext, params: SweepSessionsParams) -> SessionSweepResult:
    """Every live and hibernated session at once, for a driver overseeing
    them: the `session.list` row (activity, turn timing, last reply excerpt,
    blocked, approval), the tail of its screen, and where it belongs in an
    attention order — one call instead of a `session_screen` per session.

    Two round trips at most whatever the session count: one to the engine
    (which renders every screen concurrently) and the transcript reads the
    list already makes. With no engine, or one that cannot be asked, the
    table is empty and `engine` says why: a session table built from Vogt's
    links alone would present stale liveness as current.
    """
    now = ctx.clock()
    stall_after = timedelta(minutes=params.stall_after_minutes)
    if ctx.engine is None:
        return SessionSweepResult(
            rows=[],
            counts={"total": 0, "needs_you": 0},
            swept_at=now,
            engine="no session engine is configured (VOGT_ENGINE_URL is unset)",
        )
    try:
        entries = ctx.engine.sweep_sessions(screen_lines=params.screen_lines)
        if entries is None:
            # An engine that predates the sweep route: the same table, from
            # the list, without screens.
            entries = [
                EngineSweepEntry(session=row)
                for row in ctx.engine.list_sessions()
                if row.alive or row.hibernated
            ]
    except EngineUnavailable as exc:
        return SessionSweepResult(
            rows=[], counts={"total": 0, "needs_you": 0}, swept_at=now, engine=str(exc)
        )
    live = {entry.session.id: entry.session for entry in entries}
    tails = {entry.session.id: entry for entry in entries}
    summaries = _rows(
        ctx,
        ListSessionsParams(project=params.project, limit=500),
        live,
        None,
    )
    rows: list[SessionSweepRow] = []
    for summary in summaries:
        entry = tails.get(summary.engine_session_id)
        if entry is None:
            # Vogt's link to a session the engine no longer runs: not a row
            # of a table of what is running.
            continue
        verdict = oversight.classify(
            activity=summary.activity,
            alive=summary.alive,
            ready=entry.ready,
            approval_question=None
            if summary.approval is None
            else summary.approval.question,
            blocker=None if summary.blocked is None else summary.blocked.blocker,
            approval_kind=None if summary.approval is None else summary.approval.kind,
            last_output_at=summary.last_output_at,
            now=now,
            stall_after=stall_after,
        )
        rows.append(
            SessionSweepRow(
                attention=verdict.attention,
                attention_reason=verdict.reason,
                session=summary,
                screen_tail=list(entry.screen_tail),
                ready=entry.ready,
            )
        )
    rows.sort(
        key=lambda row: (
            oversight.ORDER.get(row.attention, 99),  # type: ignore[call-overload]
            -(row.session.last_output_at or now).timestamp(),
        )
    )
    counts: dict[str, int] = {"total": len(rows), "needs_you": 0}
    for row in rows:
        counts[row.attention] = counts.get(row.attention, 0) + 1
        if row.attention in oversight.NEEDS_YOU:
            counts["needs_you"] += 1
    return SessionSweepResult(rows=rows, counts=counts, swept_at=now, engine=None)


def _rows(
    ctx: AppContext,
    params: ListSessionsParams,
    live: dict[str, EngineSession],
    detail: str | None,
) -> list[SessionSummary]:
    """The rows `session.list` and `session.sweep` share: Vogt's links joined
    with what the engine reported (`live`, empty when `detail` says why it
    could not be asked), unlinked engine sessions appended, and reply
    excerpts read from the agents' transcripts."""
    with ctx.declared.read() as view:
        project_id = (
            None
            if params.project is None
            else _resolve.project(view, params.project).id
        )
        work_item_id = (
            None
            if params.work_item is None
            else _resolve.work_item(view, params.work_item).id
        )
        sessions = view.list_sessions(
            project_id=project_id,
            work_item_id=work_item_id,
            include_stopped=params.include_stopped,
            limit=params.limit,
            offset=params.offset,
        )
        summaries = [
            _summarize(
                view,
                session,
                engine_session=live.get(session.engine_session_id),
                engine_asked=detail is None,
            )
            for session in sessions
        ]

        # Append live engine sessions Vogt never linked. A project/work-item
        # filter is a filter on the declared half, which these do not have, so
        # they surface only when neither filter is set; and they are appended
        # only on the first page, having no stable position to paginate by.
        # An unlinked session whose process has exited is the engine's
        # leftover, not a running terminal: it is listed only when stopped
        # sessions were asked for, as a linked one Vogt stopped would be.
        unfiltered = project_id is None and work_item_id is None
        if unfiltered and params.offset == 0:
            for engine_session in live.values():
                # A hibernated session is asleep, not stopped: listed always.
                if (
                    not engine_session.alive
                    and not engine_session.hibernated
                    and not params.include_stopped
                ):
                    continue
                if view.session_by_engine_id(engine_session.id) is None:
                    summaries.append(_summarize_engine_only(engine_session))
            summaries = summaries[: params.limit]

        templates = {
            summary.engine_session_id: summary.template for summary in summaries
        }

    # Outside the read transaction: this is file I/O, and an excerpt that
    # cannot be had is a null, never an error.
    for index, summary in enumerate(summaries):
        live_session = live.get(summary.engine_session_id)
        if live_session is None or not (live_session.alive or live_session.hibernated):
            continue
        template = templates.get(summary.engine_session_id) or live_session.template
        found = _transcript(ctx, live_session, template)
        update: dict[str, Any] = {
            "running": _running(found, live_session, summary),
        }
        if summary.template is None and live_session.template:
            # An unlinked session's template is the engine's to report.
            update["template"] = live_session.template
        if found is not None:
            excerpt = transcripts.last_reply_excerpt(found)
            if excerpt is not None:
                update["last_reply_excerpt"] = excerpt
        summaries[index] = summary.model_copy(update=update)
    return summaries


def _transcript(
    ctx: AppContext, engine_session: EngineSession, template: str | None
) -> transcripts.Transcript | None:
    """The transcript a session's conversation is in, when its id is known.

    Only the id-based lookups — a list must stay cheap and must not guess
    which of two agents in one directory a session is.
    """
    roots = ctx.config.session_transcript_roots
    if not roots:
        return None
    return transcripts.find(
        roots,
        engine_session_id=engine_session.id,
        command=engine_session.command,
        template=template,
        cwd=engine_session.cwd,
        started_at=None,
        allow_cwd_guess=False,
    )


def _running(
    found: transcripts.Transcript | None,
    engine_session: EngineSession,
    summary: SessionSummary,
) -> SessionRuntime | None:
    """What the session is actually running (WI-919): the transcript's latest
    turn, else the command's flags, else what was asked."""
    model, effort = (None, None) if found is None else transcripts.runtime(found)
    resolved = runtime.resolve(
        command=engine_session.command,
        conversation_agent=engine_session.conversation_agent,
        transcript_model=model,
        transcript_effort=effort,
        asked_model=summary.model,
        asked_effort=summary.effort,
    )
    if resolved.agent is None and resolved.model is None and resolved.effort is None:
        return None
    return SessionRuntime(
        agent=resolved.agent,
        model=resolved.model,
        model_basis=resolved.model_basis,
        effort=resolved.effort,
        effort_basis=resolved.effort_basis,
    )


def last_reply(
    ctx: AppContext, params: SessionLastReplyParams
) -> SessionLastReplyResult:
    """The last `n` assistant messages of the session's agent conversation."""
    target = _target(ctx, params.id)
    engine_id = target.engine_session_id
    engine_session: EngineSession | None = None
    if ctx.engine is not None:
        try:
            engine_session = ctx.engine.get_session(engine_id)
        except EngineUnavailable:
            engine_session = None
    declared = target.session
    if engine_session is None and declared is None:
        msg = f"no session {params.id!r}"
        raise NotFound(msg)
    roots = ctx.config.session_transcript_roots
    result = SessionLastReplyResult(id=params.id, engine_session_id=engine_id)
    if not roots:
        return result.model_copy(
            update={"detail": "session_transcript_roots is empty: reading is off"}
        )
    started = (
        declared.started_at
        if declared is not None
        else _parse_engine_timestamp(
            None if engine_session is None else engine_session.created_at
        )
    )
    found = transcripts.find(
        roots,
        engine_session_id=engine_id,
        command=None if engine_session is None else engine_session.command,
        template=None if declared is None else declared.template,
        cwd=(engine_session.cwd if engine_session is not None else None)
        or (declared.cwd if declared is not None else None),
        started_at=started,
        allow_cwd_guess=True,
    )
    if found is None:
        return result.model_copy(
            update={
                "detail": (
                    "no agent transcript found for this session (a plain shell, "
                    "an agent started by hand in another directory, or "
                    "transcripts this process cannot read)"
                )
            }
        )
    replies = transcripts.last_replies(found, params.n)
    return result.model_copy(
        update={
            "agent": found.agent,
            "conversation_id": found.conversation_id,
            "basis": found.basis,
            "transcript": str(found.path),
            "messages": [SessionReply(text=r.text, at=r.at) for r in replies],
            "detail": None if replies else "the transcript has no assistant reply yet",
        }
    )


# -- session history ------------------------------------------------
#
# Thin read pass-throughs to the engine's history surface. All three degrade
# the engine-optional way `list_sessions` does: no engine, or an unreachable
# one, sets
# the `engine` field and returns an empty view — never an error that reads as
# "no history". History lives entirely engine-side, so there is no declared
# store to consult.

_NO_ENGINE = "no session engine is configured (VOGT_ENGINE_URL is unset)"


def history_list(ctx: AppContext, params: HistoryListParams) -> HistoryListResult:
    """The engine's archived-session listing, newest-first, paginated."""
    if ctx.engine is None:
        return HistoryListResult(engine=_NO_ENGINE)
    try:
        rows = ctx.engine.history_sessions(limit=params.limit, offset=params.offset)
    except EngineUnavailable as exc:
        return HistoryListResult(engine=str(exc))
    return HistoryListResult(
        sessions=[
            HistorySessionRow(
                id=row.id,
                name=row.name,
                created_at=row.created_at,
                ended_at=row.ended_at,
                exit_code=row.exit_code,
                cwd=row.cwd,
                command=row.command,
                scrollback_bytes=row.scrollback_bytes,
            )
            for row in rows
        ]
    )


def search_output(ctx: AppContext, params: SearchOutputParams) -> SearchOutputResult:
    """Full-text search over session output, live sessions included."""
    if ctx.engine is None:
        return SearchOutputResult(engine=_NO_ENGINE)
    try:
        hits = ctx.engine.search_history(
            params.q, limit=params.limit, include_live=params.include_live
        )
    except EngineUnavailable as exc:
        return SearchOutputResult(engine=str(exc))
    return SearchOutputResult(
        matches=[
            HistoryOutputMatch(
                session_id=hit.session_id,
                session_name=hit.session_name,
                created_at=hit.created_at,
                match_snippet=hit.match_snippet,
                rank=hit.rank,
                live=hit.live,
            )
            for hit in hits
        ]
    )


def log_tail(ctx: AppContext, params: LogTailParams) -> LogTailResult:
    """The tail of one session's output log, readable (ANSI-stripped) by default.

    A missing log — the id is unknown, or history is off — is an empty result
    (`session_id` null, `engine` null), not an error: "there is no output to
    show" is an ordinary answer. Either id form is accepted; a `ses_…` id
    Vogt has no record of is `NotFound`, because that is a wrong id rather
    than a missing log.
    """
    if ctx.engine is None:
        return LogTailResult(engine=_NO_ENGINE)
    engine_id = _target(ctx, params.id).engine_session_id
    try:
        log = ctx.engine.history_log(
            engine_id, tail_bytes=params.tail_bytes, strip_ansi=params.strip_ansi
        )
    except EngineUnavailable as exc:
        return LogTailResult(engine=str(exc))
    if log is None:
        return LogTailResult()
    return LogTailResult(
        session_id=log.session_id,
        text=log.text,
        bytes=log.bytes,
        total_bytes=log.total_bytes,
        truncated=log.truncated,
    )


# -- driving a session -----------------------------------------------------


def session_input(ctx: AppContext, params: SessionInputParams) -> SessionInputResult:
    """Type into a session: the text, then each named key, then Enter.

    Each part is its own engine write, in that order, so a terminal reading
    an Esc does not take the bytes after it as an Alt-chord. Audited after
    the effect (the `audited_action` ordering) with the byte count and key
    names only: what was typed may be a password, and an audit row is the
    wrong place to keep one.
    """
    reason = writes.validate_reason(params.reason)
    text = params.text or ""
    keys = list(params.keys or [])
    if not text and not keys and not params.submit:
        msg = "nothing to send: give text, keys, or submit"
        raise InvalidRequest(msg)
    size = len(text.encode("utf-8"))
    if size > SESSION_INPUT_MAX_BYTES:
        msg = (
            f"text is {size} bytes; the engine accepts at most "
            f"{SESSION_INPUT_MAX_BYTES} bytes per write"
        )
        raise InvalidRequest(msg)
    engine = _engine(ctx)
    target = _target(ctx, params.id)
    engine_id = target.engine_session_id

    writes_in_order: list[tuple[str, bool]] = []
    if text:
        writes_in_order.append((text, False))
    writes_in_order.extend((SESSION_KEYS[key], False) for key in keys)
    if params.submit:
        writes_in_order.append(("", True))
    # Enter by either route is a submit: `keys: ["enter"]` used to report
    # `submitted: false` (WI-918).
    submitted = params.submit or "enter" in keys
    confirming = submitted and params.confirm
    before: str | None = None
    if confirming:
        # What the agent was doing as the input arrived decides between
        # "starts a turn" and "waits behind one". A failed read only costs
        # the judgement its first clue.
        try:
            seen = engine.get_session(engine_id)
        except EngineUnavailable:
            seen = None
        before = None if seen is None else seen.activity
    woke = False
    try:
        _send_all(engine, engine_id, params.id, writes_in_order)
    except Conflict:
        # The engine refuses input to a hibernated session. Typing into one
        # is asking for it back: wake it — resuming its conversation, with a
        # new token — wait until it is at its prompt, then type. Anything
        # else the engine refused stays refused.
        current = engine.get_session(engine_id)
        if current is None or not current.hibernated:
            raise
        _wake(ctx, engine, target, f"woken to deliver input: {reason}")
        woke = True
        waited = engine.wait_session(
            engine_id, until="ready", timeout_s=params.wake_timeout_s
        )
        if waited is None or waited.outcome != "ready":
            outcome = "no answer" if waited is None else waited.outcome
            msg = (
                f"woke session {params.id!r}, but it was not ready for input "
                f"within {params.wake_timeout_s}s ({outcome}); nothing was "
                "typed. Read session_screen — it may be showing a dialog — "
                "then retry"
            )
            raise Conflict(msg) from None
        # Freshly woken and at its prompt: nothing was running.
        before = "waiting-for-input"
        _send_all(engine, engine_id, params.id, writes_in_order)

    verdict = (
        _confirm_delivery(engine, engine_id, before)
        if confirming
        else delivery.judge(submitted=submitted, before=None, after=[])
    )

    linked = target.session is not None
    audited_action(
        ctx,
        operation=SESSION_INPUT,
        reason=reason,
        entity_kind="session",
        entity_id=target.session.id if target.session is not None else engine_id,
        # Never the text: only how much, and which keys.
        outcome={
            "engine_session_id": engine_id,
            "linked": linked,
            "bytes": size,
            "keys": list(keys),
            "submit": params.submit,
            "submitted": submitted,
            "delivery": verdict.delivery,
            "woke": woke,
        },
        event_kind=SESSION_INPUT_EVENT,
    )
    return SessionInputResult(
        id=params.id,
        engine_session_id=engine_id,
        linked=linked,
        bytes=size,
        keys=keys,
        submitted=submitted,
        delivery=verdict.delivery,
        delivery_evidence=verdict.evidence,
        woke=woke,
    )


#: How long `session.input` watches for what became of a submitted input:
#: up to `_CONFIRM_READS` screen reads, `_CONFIRM_PAUSE_S` apart. A module
#: value so tests can make it instant.
_CONFIRM_READS = 10
_CONFIRM_PAUSE_S = 0.2


def _pause(seconds: float) -> None:
    time.sleep(seconds)


def _confirm_delivery(
    engine: EngineClient, engine_id: str, before: str | None
) -> delivery.Verdict:
    """Read the session a few times after a submit and judge, stopping at
    the first decisive read. An engine that cannot be read leaves the
    judgement to what was seen before."""
    observed: list[delivery.Observation] = []
    verdict = delivery.judge(submitted=True, before=before, after=observed)
    if verdict.delivery != "unconfirmed":
        return verdict
    for _ in range(_CONFIRM_READS):
        _pause(_CONFIRM_PAUSE_S)
        try:
            screen = engine.session_screen(engine_id)
        except EngineUnavailable:
            break
        if screen is None:
            break
        observed.append(
            delivery.Observation(activity=screen.activity, lines=screen.lines)
        )
        verdict = delivery.judge(submitted=True, before=before, after=observed)
        if verdict.delivery != "unconfirmed":
            break
    return verdict


def _send_all(
    engine: EngineClient, engine_id: str, named: str, chunks: list[tuple[str, bool]]
) -> None:
    for chunk, submit in chunks:
        if not engine.send_input(engine_id, chunk, submit=submit):
            msg = f"the engine has no live session {named!r}"
            raise NotFound(msg)


def session_screen(ctx: AppContext, params: SessionScreenParams) -> SessionScreenResult:
    """What the session's terminal shows right now, as text lines.

    Needs an engine with the `/screen` route. An engine that predates it
    answers 404 for a session it does hold; that is reported as the engine
    lacking the feature — never papered over with the output log, which is a
    different thing (`session.log_tail`).
    """
    engine = _engine(ctx)
    target = _target(ctx, params.id)
    engine_id = target.engine_session_id
    screen = engine.session_screen(engine_id, scrollback_lines=params.scrollback_lines)
    if screen is None:
        if engine.get_session(engine_id) is not None:
            msg = (
                "the session engine does not support screen yet (it has no "
                "GET /api/sessions/{id}/screen route); upgrade the engine, or "
                "read the output with session.log_tail"
            )
            raise EngineUnavailable(msg)
        msg = f"the engine has no live session {params.id!r}"
        raise NotFound(msg)
    return _screen_result(params.id, engine_id, screen)


def _screen_result(
    named: str, engine_id: str, screen: EngineScreen
) -> SessionScreenResult:
    cursor = (
        None
        if screen.cursor_row is None or screen.cursor_col is None
        else SessionScreenCursor(row=screen.cursor_row, col=screen.cursor_col)
    )
    return SessionScreenResult(
        id=named,
        engine_session_id=engine_id,
        cols=screen.cols,
        rows=screen.rows,
        lines=list(screen.lines),
        cursor=cursor,
        title=screen.title,
        activity=screen.activity,
        alive=screen.alive,
        ready=screen.ready,
        scrollback=list(screen.scrollback),
        turn_started_at=_parse_engine_timestamp(screen.turn_started_at),
        last_output_at=_parse_engine_timestamp(screen.last_output_at),
        approval=_approval(screen.approval),
        blocked=_blocked(screen.blocked),
    )


def session_wait(ctx: AppContext, params: SessionWaitParams) -> SessionWaitResult:
    """Block until the session is ready (or needs a person, or exits), exits,
    or changes — or the timeout passes — on the engine's own event bus.

    One call replaces a polling loop. The engine does the waiting; this is a
    read, held open for at most `timeout_s`.
    """
    engine = _engine(ctx)
    target = _target(ctx, params.id)
    engine_id = target.engine_session_id
    try:
        waited = engine.wait_session(
            engine_id,
            until=params.until.replace("_", "-"),
            timeout_s=params.timeout_s,
        )
    except Conflict:
        # Nothing happens in a hibernated session until somebody wakes it,
        # and a wait is a read: it does not wake it. Say so at once, with
        # the screen it kept, rather than holding the caller to a timeout.
        screen = engine.session_screen(engine_id)
        if screen is None or screen.activity != "hibernated":
            raise
        return SessionWaitResult(
            id=params.id,
            engine_session_id=engine_id,
            outcome="hibernated",
            matched=False,
            waited_ms=0,
            screen=_screen_result(params.id, engine_id, screen),
        )
    if waited is None:
        if engine.get_session(engine_id) is not None:
            msg = (
                "the session engine does not support wait yet (it has no "
                "GET /api/sessions/{id}/wait route); upgrade the engine, or "
                "poll session.screen"
            )
            raise EngineUnavailable(msg)
        msg = f"the engine has no live session {params.id!r}"
        raise NotFound(msg)
    return SessionWaitResult(
        id=params.id,
        engine_session_id=engine_id,
        outcome=waited.outcome,
        matched=waited.matched,
        waited_ms=waited.waited_ms,
        screen=_screen_result(params.id, engine_id, waited.screen),
    )


# -- hibernation -----------------------------------------------------------


def hibernate_session(ctx: AppContext, params: HibernateSessionParams) -> SessionResult:
    """Stop a session's processes to free their memory, keeping it listed.

    The engine keeps what it needs to start the session again by resuming its
    agent conversation under the same id (`session.wake`). The session's
    token is revoked: nothing runs to hold it, and a wake mints a new one.
    Audited after the effect, the `audited_action` ordering, because the
    effect is the engine's.
    """
    reason = writes.validate_reason(params.reason)
    engine = _engine(ctx)
    target = _target(ctx, params.id)
    engine_id = target.engine_session_id
    if target.session is not None and target.session.stopped_at is not None:
        msg = f"session {params.id!r} was stopped; there is nothing to hibernate"
        raise Conflict(msg)
    hibernated = engine.hibernate_session(
        engine_id, reason=reason, allow_shell=params.allow_shell
    )
    if hibernated is None:
        msg = (
            f"the engine has no session {params.id!r}, or predates hibernation "
            "(no POST /api/sessions/{id}/hibernate)"
        )
        raise NotFound(msg)
    outcome: dict[str, Any] = {
        "engine_session_id": engine_id,
        "linked": target.session is not None,
        "allow_shell": params.allow_shell,
        "resumable": None
        if hibernated.hibernation is None
        else hibernated.hibernation.resumable,
    }
    if target.session is None:
        audited_action(
            ctx,
            operation=SESSION_HIBERNATE,
            reason=reason,
            entity_kind="session",
            entity_id=engine_id,
            outcome=outcome,
            event_kind=SESSION_HIBERNATED_EVENT,
        )
        return SessionResult(session=_summarize_engine_only(hibernated))
    session = target.session

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[SessionResult]:
        del actor
        current = txn.session_by_id(session.id)
        if current is None:
            msg = f"no session {params.id!r}"
            raise NotFound(msg)
        now = ctx.clock()
        revoked = [
            token.id
            for token in txn.tokens_for_actor(current.actor_id)
            if txn.revoke_token(token.id, reason=f"hibernated: {reason}", at=now)
        ]
        return WriteOutcome(
            result=SessionResult(
                session=_summarize(txn, current, engine_session=hibernated)
            ),
            entity_kind="session",
            entity_id=current.id,
            payload=_audited_payload(current),
            event_kind=SESSION_HIBERNATED_EVENT,
            summary={**outcome, "tokens_revoked": len(revoked)},
        )

    return audited_write(ctx, operation=SESSION_HIBERNATE, reason=reason, body=body)


def wake_session(ctx: AppContext, params: WakeSessionParams) -> SessionResult:
    """Start a hibernated session again: the same id, the same conversation.

    A live session is returned as it is — waking is idempotent, so a caller
    that is not sure need not check first.
    """
    reason = writes.validate_reason(params.reason)
    engine = _engine(ctx)
    target = _target(ctx, params.id)
    return _wake(ctx, engine, target, reason)


def _wake(
    ctx: AppContext, engine: EngineClient, target: _Target, reason: str
) -> SessionResult:
    """Wake `target` if it is hibernated; its summary either way.

    For a session Vogt started, a new token is minted for the session's own
    actor and handed to the woken process — the engine never stores one —
    and every older token of that actor is revoked in the same write. The
    attribution is unchanged: the same actor, a new credential. As at start,
    the process starts before the declared write: the new token is only a
    hash in a row that does not exist yet until the write lands.
    """
    engine_id = target.engine_session_id
    current = engine.get_session(engine_id)
    if current is None:
        msg = f"the engine has no session {engine_id!r}"
        raise NotFound(msg)
    session = target.session
    if not current.hibernated:
        if not current.alive:
            msg = (
                f"session {engine_id!r} has exited, not hibernated; start a new "
                "one (session_start, with resume to continue its conversation)"
            )
            raise Conflict(msg)
        if session is None:
            return SessionResult(session=_summarize_engine_only(current))
        with ctx.declared.read() as view:
            return SessionResult(
                session=_summarize(view, session, engine_session=current)
            )

    if session is None:
        woken = engine.wake_session(engine_id)
        if woken is None:
            msg = f"the engine has no session {engine_id!r}"
            raise NotFound(msg)
        audited_action(
            ctx,
            operation=SESSION_WAKE,
            reason=reason,
            entity_kind="session",
            entity_id=engine_id,
            outcome={"engine_session_id": engine_id, "linked": False},
            event_kind=SESSION_WOKEN_EVENT,
        )
        return SessionResult(session=_summarize_engine_only(woken))

    if session.stopped_at is not None:
        msg = f"session {session.id!r} was stopped; start a new one instead"
        raise Conflict(msg)
    scopes = _session_scopes(ctx)
    credential = issue(scopes)
    woken = engine.wake_session(
        engine_id, env=_session_env(ctx, session.id, credential.secret)
    )
    if woken is None:
        msg = f"the engine has no session {engine_id!r}"
        raise NotFound(msg)

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[SessionResult]:
        del actor
        row = txn.session_by_id(session.id)
        holder = None if row is None else txn.actor_by_id(row.actor_id)
        if row is None or holder is None:
            msg = f"no session {session.id!r}"
            raise NotFound(msg)
        now = ctx.clock()
        revoked = [
            token.id
            for token in txn.tokens_for_actor(row.actor_id)
            if txn.revoke_token(
                token.id, reason=f"superseded on wake: {reason}", at=now
            )
        ]
        txn.insert_token(
            Token(
                id=ctx.id_factory("tok"),
                actor_id=holder.id,
                actor_identity_ref=holder.identity_ref,
                name=f"session {row.id}",
                scopes=list(scopes),
                created_at=now,
                expires_at=None,
            ),
            token_hash=credential.token_hash,
        )
        return WriteOutcome(
            result=SessionResult(session=_summarize(txn, row, engine_session=woken)),
            entity_kind="session",
            entity_id=row.id,
            payload=_audited_payload(row),
            event_kind=SESSION_WOKEN_EVENT,
            summary={
                "engine_session_id": engine_id,
                "conversation_id": current.conversation_id,
                "tokens_revoked": len(revoked),
            },
        )

    return audited_write(ctx, operation=SESSION_WAKE, reason=reason, body=body)


def keep_session_awake(
    ctx: AppContext, params: KeepSessionAwakeParams
) -> SessionResult:
    """Pin a session awake, so policy never hibernates it — or unpin it."""
    reason = writes.validate_reason(params.reason)
    engine = _engine(ctx)
    target = _target(ctx, params.id)
    engine_id = target.engine_session_id
    updated = engine.keep_awake(engine_id, keep_awake=params.keep_awake)
    if updated is None:
        msg = f"the engine has no session {params.id!r}, or predates hibernation"
        raise NotFound(msg)
    audited_action(
        ctx,
        operation=SESSION_KEEP_AWAKE,
        reason=reason,
        entity_kind="session",
        entity_id=target.session.id if target.session is not None else engine_id,
        outcome={
            "engine_session_id": engine_id,
            "linked": target.session is not None,
            "keep_awake": params.keep_awake,
        },
        event_kind=SESSION_KEEP_AWAKE_EVENT,
    )
    if target.session is None:
        return SessionResult(session=_summarize_engine_only(updated))
    with ctx.declared.read() as view:
        return SessionResult(
            session=_summarize(view, target.session, engine_session=updated)
        )


SESSION_ANSWER = "session.answer"
SESSION_ANSWERED_EVENT = "session.answered"


def answer_session(ctx: AppContext, params: AnswerSessionParams) -> SessionAnswerResult:
    """Answer the dialog a session shows — a permission request or a
    startup gate (folder trust, external CLAUDE.md imports, reading outside
    the working directory) — by option, not by keystrokes.

    The engine reads the menu as it is at that moment and moves the highlight
    itself, so nothing here counts rows. Audited after the effect with the
    question, the kind and the option chosen.
    """
    reason = writes.validate_reason(params.reason)
    if (params.option is None) == (params.label is None or not params.label.strip()):
        msg = "give exactly one of option (a number) or label"
        raise InvalidRequest(msg)
    engine = _engine(ctx)
    target = _target(ctx, params.id)
    engine_id = target.engine_session_id
    answered = engine.answer_session(
        engine_id,
        option=params.option,
        label=None if params.label is None else params.label.strip(),
        expect_question=params.expect_question,
    )
    if answered is None:
        msg = (
            f"the engine has no session {params.id!r}, or predates answering "
            "(no POST /api/sessions/{id}/answer)"
        )
        raise NotFound(msg)
    chosen = answered.get("chosen")
    chosen = chosen if isinstance(chosen, dict) else {}
    option = SessionApprovalOption(
        number=int(chosen.get("number", 0)),
        label=str(chosen.get("label", "")),
        selected=True,
    )
    question = str(answered.get("question", ""))
    kind = str(answered.get("kind", "permission"))
    dismissed = answered.get("dismissed") is True
    audited_action(
        ctx,
        operation=SESSION_ANSWER,
        reason=reason,
        entity_kind="session",
        entity_id=target.session.id if target.session is not None else engine_id,
        outcome={
            "engine_session_id": engine_id,
            "linked": target.session is not None,
            "question": question[:300],
            "kind": kind,
            "option": option.number,
            "label": option.label[:200],
            "dismissed": dismissed,
        },
        event_kind=SESSION_ANSWERED_EVENT,
    )
    return SessionAnswerResult(
        id=params.id,
        engine_session_id=engine_id,
        question=question,
        kind=kind,
        chosen=option,
        dismissed=dismissed,
    )


SESSION_REPORT_BLOCKED = "session.report_blocked"
SESSION_REPORT_UNBLOCKED = "session.report_unblocked"
SESSION_BLOCKED_EVENT = "session.blocked"
SESSION_UNBLOCKED_EVENT = "session.unblocked"


def report_blocked(
    ctx: AppContext, params: ReportBlockedParams
) -> SessionBlockedResult:
    """An agent says it cannot go on without a person, and what they must do.

    The report lives on the engine's session (it is live state, gone when the
    session is), where `session.list`, the screen, `session.wait`, the Inbox
    and a push all see it. Audited here, after the effect, with the text: the
    report is meant to be read.
    """
    reason = writes.validate_reason(params.reason)
    items = [item.strip() for item in params.items if item.strip()]
    return _set_blocked(
        ctx,
        params.id,
        reason=reason,
        blocker=params.blocker.strip(),
        items=items,
    )


def report_unblocked(
    ctx: AppContext, params: ReportUnblockedParams
) -> SessionBlockedResult:
    """Clear a session's blocked report: the person acted, or it found a way."""
    reason = writes.validate_reason(params.reason)
    return _set_blocked(ctx, params.id, reason=reason, blocker=None, items=[])


def _set_blocked(
    ctx: AppContext,
    session_id: str | None,
    *,
    reason: str,
    blocker: str | None,
    items: list[str],
) -> SessionBlockedResult:
    engine = _engine(ctx)
    target = _target(ctx, session_id or _own_session(ctx))
    engine_id = target.engine_session_id
    updated = engine.set_blocked(
        engine_id, blocked=blocker is not None, reason=blocker, items=items
    )
    if updated is None:
        msg = f"the engine has no live session {engine_id!r}"
        raise NotFound(msg)
    audited_action(
        ctx,
        operation=SESSION_REPORT_BLOCKED
        if blocker is not None
        else SESSION_REPORT_UNBLOCKED,
        reason=reason,
        entity_kind="session",
        entity_id=target.session.id if target.session is not None else engine_id,
        outcome={
            "engine_session_id": engine_id,
            "linked": target.session is not None,
            "blocker": blocker,
            "items": items,
        },
        event_kind=SESSION_BLOCKED_EVENT
        if blocker is not None
        else SESSION_UNBLOCKED_EVENT,
    )
    return SessionBlockedResult(
        id=target.session.id if target.session is not None else engine_id,
        engine_session_id=engine_id,
        blocked=_blocked(updated.blocked),
    )


def _own_session(ctx: AppContext) -> str:
    """The session a session's own token belongs to, for an omitted `id`.

    A session Vogt started holds a token bound to the actor
    `agent:session:<ses_…>`, so that actor names it. Any other caller must
    say which session it means.
    """
    ref = ctx.principal.identity_ref
    prefix = "agent:session:"
    if ref.startswith(prefix):
        return ref.removeprefix(prefix)
    msg = (
        "give `id`: this caller is not a session Vogt started, so it does not "
        "name one (inside a session, pass $VOGT_ENGINE_SESSION_ID)"
    )
    raise InvalidRequest(msg)


def _stop_unlinked(ctx: AppContext, engine_id: str, reason: str) -> SessionResult:
    """Stop a session Vogt never linked: the engine kill, audited by its id."""
    cleaned = writes.validate_reason(reason)
    engine = _engine(ctx)
    live = engine.get_session(engine_id)
    if live is None:
        msg = f"no session {engine_id!r}"
        raise NotFound(msg)
    killed = engine.kill_session(
        engine_id, reason=cleaned, by=ctx.principal.identity_ref
    )
    audited_action(
        ctx,
        operation=SESSION_STOP,
        reason=cleaned,
        entity_kind="session",
        entity_id=engine_id,
        outcome={"engine_session_id": engine_id, "linked": False},
        event_kind=SESSION_STOPPED_EVENT,
        summary={"engine_killed": killed, "linked": False},
    )
    summary = _summarize_engine_only(live).model_copy(
        update={"alive": False if killed else None, "stopped_at": ctx.clock()}
    )
    return SessionResult(session=summary)


# -- resolution ------------------------------------------------------------


class _Target:
    """A session named by either id form, resolved to the engine's id.

    `session` is Vogt's record when there is one; `None` means the engine id
    names a session Vogt never linked (started from the GUI), which is still
    a session the caller may read or drive.
    """

    def __init__(
        self, *, engine_session_id: str, session: CodingSession | None
    ) -> None:
        self.engine_session_id = engine_session_id
        self.session = session


def _target(ctx: AppContext, session_id: str) -> _Target:
    """Resolve a `ses_…` id or an engine UUID to the engine's session id.

    A `ses_…` id must be one Vogt recorded — otherwise it is a wrong id and
    says so. Anything else is the engine's own id, passed through as given
    and linked to Vogt's record when one exists.
    """
    wanted = session_id.strip()
    if not wanted:
        msg = "a session id is required (ses_… or the engine's session UUID)"
        raise InvalidRequest(msg)
    with ctx.declared.read() as view:
        if wanted.startswith("ses_"):
            session = view.session_by_id(wanted)
            if session is None:
                msg = f"no session {wanted!r}"
                raise NotFound(msg)
            return _Target(engine_session_id=session.engine_session_id, session=session)
        return _Target(
            engine_session_id=wanted, session=view.session_by_engine_id(wanted)
        )


class _Subject:
    """What a session is being opened for, resolved to a path."""

    def __init__(
        self,
        *,
        project_id: str,
        project_slug: str,
        cwd: str,
        work_item: WorkItem | None,
        brief: str,
        is_scratch: bool = False,
    ) -> None:
        self.project_id = project_id
        self.project_slug = project_slug
        self.cwd = cwd
        self.work_item = work_item
        self.brief = brief
        #: Resolved from `session_scratch_project` rather than named by the
        #: caller. Carried so the name and the audit row can say so.
        self.is_scratch = is_scratch

    @property
    def work_item_id(self) -> str | None:
        return None if self.work_item is None else self.work_item.id

    @property
    def work_item_ref(self) -> str | None:
        return None if self.work_item is None else self.work_item.ref

    @property
    def default_name(self) -> str:
        # A scratch session says so in its own name. Otherwise a list of
        # sessions shows several called `scratch` and nothing distinguishes
        # the one that was asked for from the ones that fell back to it.
        if self.is_scratch:
            return f"scratch/{self.project_slug}"
        return self.work_item_ref or self.project_slug


def _subject(ctx: AppContext, params: StartSessionParams, session_id: str) -> _Subject:
    if params.work_item is not None and params.project is not None:
        msg = "give at most one of --work-item or --project"
        raise InvalidRequest(msg)
    if params.work_item is None and params.project is None:
        # The refusal names the setting because the alternative — a default
        # working directory — is the one failure the registry-owned `cwd`
        # rule is written against, and it fails by succeeding somewhere
        # plausible.
        scratch = ctx.config.session_scratch_project
        if not scratch:
            msg = (
                "a session needs a work item or a project, and no scratch "
                "project is configured for requests that name neither (set "
                "session_scratch_project to a registered project slug)"
            )
            raise InvalidRequest(msg)
        params = params.model_copy(update={"project": scratch})
        with ctx.declared.read() as view:
            subject = _resolve_subject(view, params, session_id, ctx)
        subject.is_scratch = True
        return subject

    with ctx.declared.read() as view:
        return _resolve_subject(view, params, session_id, ctx)


def _ranking(ctx: AppContext, ref: str) -> WhyResult | None:
    """The item's score explanation, or nothing if it cannot be had.

    Optional on purpose: a brief that refused to be written because a score
    could not be computed would make the ranking a precondition for starting
    work, which is the inversion the contract-as-value rule spends its whole
    sentence on.
    """
    try:
        return why(ctx, WhyParams(ref=ref))
    except VogtError:
        return None


def _resolve_subject(
    view: ReadView, params: StartSessionParams, session_id: str, ctx: AppContext
) -> _Subject:
    if params.work_item is not None:
        item = _resolve.work_item(view, params.work_item)
        if item.project_id is None:
            # Not a lookup failure: an unassigned item has no tree to open in,
            # and guessing one would be exactly the heuristic the registry-owned
            # `cwd` rule forbids.
            msg = (
                f"{item.ref} belongs to no project, so there is no working tree "
                "to open a session in"
            )
            raise InvalidRequest(msg)
        project = view.project_by_id(item.project_id)
        if project is None:  # pragma: no cover - foreign key guarantees this
            msg = f"work item {item.ref} references a project that is gone"
            raise NotFound(msg)
        return _Subject(
            project_id=project.id,
            project_slug=project.slug,
            cwd=project.root_path,
            work_item=item,
            brief=brief_for_work_item(view, item, session_id, _ranking(ctx, item.ref)),
        )

    project = _resolve.project(view, params.project or "")
    return _Subject(
        project_id=project.id,
        project_slug=project.slug,
        cwd=project.root_path,
        work_item=None,
        brief=brief_for_project(view, project.slug, session_id),
    )


def _record_declared_branch(
    txn: WriteTxn, *, work_ref: str, project_id: str, at: datetime, template: str
) -> str:
    """Add the branch a session will use to the item's overlay, idempotently.

    Read-modify-write on the `branches` list rather than a blind append, so
    starting a second session on an item that already declared its branch adds
    nothing and re-starting after a stop does not accumulate duplicates. Keyed
    by the work-item ref, which is `WI-7` for a native item and the subject key
    for an upstream one — the same key `work.get` reads the overlay back under.
    """
    branch = default_branch_name(work_ref, template=template)
    existing = txn.work_overlay(work_ref)
    current = list(existing.branches) if existing is not None else []
    if branch in current:
        return branch
    current.append(branch)
    overlay = (
        existing.model_copy(update={"branches": current, "updated_at": at})
        if existing is not None
        else WorkOverlay(
            subject_key=work_ref,
            project_id=project_id,
            branches=current,
            created_at=at,
            updated_at=at,
        )
    )
    txn.upsert_work_overlay(overlay)
    return branch


def _engine(ctx: AppContext) -> EngineClient:
    if ctx.engine is None:
        msg = (
            "no session engine is configured, so there is nothing to open a "
            "terminal on (set VOGT_ENGINE_URL)"
        )
        raise EngineUnavailable(msg)
    return ctx.engine


def _brief_with_task(brief: str, task: str | None, *, autopilot: bool = False) -> str:
    """Fold an explicit task into the session's brief.

    The brief is context — the project, or the work item and why it ranks —
    and by itself it asks the agent to do nothing (a project brief says so
    in as many words). A spoken "start a session on komodo and check the
    containers" carries the *doing* part separately; without it the agent
    opens and waits, and the person has to type what they just said.
    Appended rather than replacing so the agent keeps the context under
    the task.
    """
    task = (task or "").strip()
    if autopilot:
        # Said in the brief, not enforced: the engine cannot make an agent
        # keep going, but an agent that was told to will (WI-878).
        brief = f"{brief.rstrip()}\n\n{AUTOPILOT}"
    if not task:
        return brief
    return f"{brief.rstrip()}\n\n## Task\n\n{task}\n"


def _start_on_engine(
    engine: EngineClient,
    *,
    name: str,
    template: str | None,
    cwd: str,
    env: dict[str, str],
    brief: str,
    model: str | None = None,
    effort: str | None = None,
    resume: str | None = None,
    permission_mode: str | None = None,
) -> EngineSession:
    return engine.create_session(
        prompt=brief,
        name=name,
        # A template names a command the *engine* knows; Vogt sends the
        # name and the engine expands it against its own `session_templates`,
        # because the command a template runs — a `vogt-agent-auth run --
        # claude` wrapper, say — is that pod's configuration and not the
        # estate's. Sending the bare name is what lets "start an agent on
        # this" reach the deployment's protected agent template rather than
        # an unwrapped binary.
        template=template,
        cwd=cwd,
        env=env,
        # Passed through for the same reason: *how* a model id
        # reaches a CLI is `claude --model` or `codex -m`, which is knowledge
        # about that pod's binaries. Vogt says which model; the engine knows
        # how to ask for it, and refuses by name when it cannot.
        model=model,
        effort=effort,
        resume=resume,
        permission_mode=permission_mode,
    )


def _session_env(ctx: AppContext, session_id: str, secret: str) -> dict[str, str]:
    """What an agent inside the session needs to reach Vogt.

    The same two variables the MCP bootstrap already uses in an engine
    container, so an agent started here is configured the way an agent
    started by hand is. The URL is the one the operator configured for
    clients; unset means the session still runs and simply has no Vogt to
    talk to, which is reported rather than guessed.

    `VOGT_ENGINE_URL` is where this core reaches the engine — in the merged
    stack the engine on loopback, the same process the session runs under —
    so an agent that needs a route Vogt does not wrap yet can find it without
    reading engine source. The core's operations (`session_input`,
    `session_screen`) remain the recommended way to drive another session.
    """
    env = {"VOGT_HTTP_TOKEN": secret, "VOGT_SESSION_ID": session_id}
    if ctx.config.public_url:
        env["VOGT_URL"] = ctx.config.public_url
    if ctx.engine is not None:
        env["VOGT_ENGINE_URL"] = ctx.engine.base_url
    return env


def _audited_payload(session: CodingSession) -> dict[str, object]:
    """What the audit row records about a session.

    Everything except the credential: an audit row that carries the token is
    a token leak with a timestamp on it. The actor id is enough to find
    every write the session made.
    """
    return {
        "id": session.id,
        "engine_session_id": session.engine_session_id,
        "project_id": session.project_id,
        "work_item_id": session.work_item_id,
        "actor_id": session.actor_id,
        "cwd": session.cwd,
        "template": session.template,
        "model": session.model,
        "effort": session.effort,
        "started_at": session.started_at.isoformat(),
        "stopped_at": None
        if session.stopped_at is None
        else session.stopped_at.isoformat(),
    }


def _summarize(
    view: ReadView,
    session: CodingSession,
    *,
    engine_session: EngineSession | None,
    engine_asked: bool = True,
) -> SessionSummary:
    """Name what the ids point at, so a caller reads WI-7 rather than wrk_01J8.

    Takes a `ReadView` and is called with both a read view and an open
    transaction — `WriteTxn` is one — so a session summarised inside the
    write that created it reads the rows that write just made.
    """
    project = view.project_by_id(session.project_id)
    work_item = (
        None
        if session.work_item_id is None
        else view.work_item_by_id(session.work_item_id)
    )
    actor = view.actor_by_id(session.actor_id)

    return SessionSummary(
        id=session.id,
        engine_session_id=session.engine_session_id,
        project=None if project is None else project.slug,
        work_item=None if work_item is None else work_item.ref,
        actor=session.actor_id if actor is None else actor.identity_ref,
        cwd=session.cwd,
        template=session.template,
        model=session.model,
        effort=session.effort,
        reason=session.reason,
        started_at=session.started_at,
        stopped_at=session.stopped_at,
        activity=None if engine_session is None else engine_session.activity,
        # Listed by the engine is not alive: an exited session stays in its
        # list, with an exit code, until it is deleted.
        alive=(engine_session is not None and engine_session.alive)
        if engine_asked
        else None,
        **_live_fields(engine_session),
    )


def _approval(approval: EngineApproval | None) -> SessionApproval | None:
    if approval is None:
        return None
    return SessionApproval(
        question=approval.question,
        command_excerpt=approval.command_excerpt,
        kind=approval.kind,
        options=[
            SessionApprovalOption(number=number, label=label, selected=selected)
            for number, label, selected in approval.options
        ],
        deadline_seconds=approval.deadline_seconds,
        deadline_at=_parse_engine_timestamp(approval.deadline_at),
        detected_at=_parse_engine_timestamp(approval.detected_at),
    )


def _live_fields(engine_session: EngineSession | None) -> dict[str, Any]:
    """The engine's live turn timing and permission dialog, for a summary."""
    if engine_session is None:
        return {}
    return {
        "turn_started_at": _parse_engine_timestamp(engine_session.turn_started_at),
        "last_output_at": _parse_engine_timestamp(engine_session.last_output_at),
        "approval": _approval(engine_session.approval),
        "blocked": _blocked(engine_session.blocked),
        "hibernation": _hibernation(engine_session.hibernation),
        "keep_awake": engine_session.keep_awake,
        "conversation_id": engine_session.conversation_id,
        "resources": _resources(engine_session.resources),
        "permission_mode": engine_session.permission_mode,
        "stopped_by": engine_session.stopped_by,
        "stop_reason": engine_session.stop_reason,
    }


def _resources(resources: EngineResources | None) -> SessionResources | None:
    if resources is None:
        return None
    return SessionResources(
        rss_bytes=resources.rss_bytes,
        cpu_pct=resources.cpu_pct,
        processes=resources.processes,
        sampled_at=_parse_engine_timestamp(resources.sampled_at),
        over_threshold=resources.over_threshold,
    )


def _hibernation(hibernation: EngineHibernation | None) -> SessionHibernation | None:
    if hibernation is None:
        return None
    return SessionHibernation(
        at=_parse_engine_timestamp(hibernation.at),
        trigger=hibernation.trigger,
        reason=hibernation.reason,
        resumable=hibernation.resumable,
    )


def _blocked(blocked: EngineBlocked | None) -> SessionBlocked | None:
    if blocked is None:
        return None
    return SessionBlocked(
        blocker=blocked.reason,
        items=list(blocked.items),
        since=_parse_engine_timestamp(blocked.since),
    )


def _summarize_engine_only(engine_session: EngineSession) -> SessionSummary:
    """A session the engine is running but Vogt never linked.

    Vogt has no declared row for it, so every audited field is null: it is
    not Vogt's to name a project, work item, actor or reason for something it
    did not start. What can be reported truthfully is reported — the engine's
    id, where it runs, its live activity, and the engine's own start time.
    """
    return SessionSummary(
        linked=False,
        id=engine_session.id,
        engine_session_id=engine_session.id,
        project=None,
        work_item=None,
        actor=None,
        cwd=engine_session.cwd,
        template=None,
        model=None,
        effort=None,
        reason=None,
        started_at=_parse_engine_timestamp(engine_session.created_at),
        stopped_at=None,
        activity=engine_session.activity,
        # The engine has it and was asked; whether its process still runs is
        # the engine's own answer.
        alive=engine_session.alive,
        **_live_fields(engine_session),
    )


def _parse_engine_timestamp(value: str | None) -> datetime | None:
    """The engine's RFC3339 string as a datetime, or None if unreadable.

    Better to drop the field than to guess: an unlinked session with no
    trustworthy start time reports none rather than a fabricated one.
    """
    if not value:
        return None
    try:
        return datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None


__all__ = [
    "answer_session",
    "hibernate_session",
    "history_list",
    "keep_session_awake",
    "last_reply",
    "list_sessions",
    "log_tail",
    "report_blocked",
    "report_unblocked",
    "search_output",
    "session_input",
    "session_screen",
    "session_wait",
    "start_session",
    "stop_session",
    "sweep_sessions",
    "wake_session",
]
