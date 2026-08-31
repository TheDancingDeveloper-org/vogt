"""Coding sessions — the work opening a terminal on itself (FR-E3–E5, E8).

A session is a PTY the engine runs, in a project's working tree, for a
project or for one work item. Vogt does not run it and does not watch it: it
decides *where* it opens and *who* it writes as, records the link, and asks
the engine for the live state whenever somebody looks.

Three rules this module exists to keep:

- **The working directory comes from the registry, never from a heuristic**
  (FR-E3). The engine would happily default to its workspace root, and a
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
  asked (FR-E2). A stored activity state would be a claim about a process
  this half of the product does not own.
"""

from __future__ import annotations

from datetime import datetime

from vogt.adapters.engine import EngineClient, EngineSession, EngineUnavailable
from vogt.application import writes
from vogt.application.context import AppContext
from vogt.application.models import (
    HistoryListParams,
    HistoryListResult,
    HistoryOutputMatch,
    HistorySessionRow,
    ListSessionsParams,
    LogTailParams,
    LogTailResult,
    SearchOutputParams,
    SearchOutputResult,
    SessionListResult,
    SessionResult,
    SessionSummary,
    StartSessionParams,
    StopSessionParams,
    WhyParams,
    WhyResult,
)
from vogt.application.services import _resolve
from vogt.application.services._brief import (
    brief_for_project,
    brief_for_work_item,
)
from vogt.application.services.views import why
from vogt.application.writes import WriteOutcome, audited_write
from vogt.core.auth import Scope, issue
from vogt.core.branches import default_branch_name
from vogt.core.entities import Actor, CodingSession, Token, WorkItem, WorkOverlay
from vogt.errors import Conflict, InvalidRequest, NotFound, VogtError
from vogt.storage.interface import ReadView, WriteTxn

SESSION_START = "session.start"
SESSION_STOP = "session.stop"
SESSION_STARTED_EVENT = "session.started"
SESSION_STOPPED_EVENT = "session.stopped"

#: What a session's own token may do. Read, so an agent can ask what it is
#: working on; `work.write`, so it can record what it did. Not `project.write`
#: and not `admin`: a terminal opened on one bug has no business registering
#: projects or issuing further tokens (FR-S10).
SESSION_SCOPES: tuple[Scope, ...] = ("read", "work.write")


def start_session(ctx: AppContext, params: StartSessionParams) -> SessionResult:
    """Open a terminal for a work item or a project (FR-E3, FR-E4, FR-E5)."""
    # Validate everything we can before crossing the process boundary. The
    # engine and SQLite cannot share a transaction, so rejecting the reason
    # only when ``audited_write`` begins would leave a running terminal that
    # Vogt never records. Keep the cleaned value for both the entity and the
    # audit row so they cannot disagree about whitespace.
    reason = writes.validate_reason(params.reason)
    engine = _engine(ctx)
    session_id = ctx.id_factory("ses")
    subject = _subject(ctx, params, session_id)
    actor_ref = f"agent:session:{session_id}"
    credential = issue(SESSION_SCOPES)

    started = _start_on_engine(
        engine,
        name=params.name or subject.default_name,
        template=params.template,
        cwd=subject.cwd,
        env=_session_env(ctx, session_id, credential.secret),
        brief=subject.brief,
        model=params.model,
        effort=params.effort,
    )

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
                scopes=list(SESSION_SCOPES),
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
            cwd=subject.cwd,
            template=params.template,
            model=params.model,
            effort=params.effort,
            reason=reason,
            started_at=now,
            stopped_at=None,
        )
        txn.insert_session(session)
        # The declared half of the branch binding (#283): a session opened for
        # a work item records, on that item's overlay, the branch it will use.
        # Additive and forward-only — this writes a name, never a branch: git
        # is not touched, and the row rides this session's audit like every
        # other change in the transaction (FR-B4).
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
                "cwd": subject.cwd,
                # Named in the audit summary because a spoken request that
                # resolved to the scratch project asked for neither, and a row
                # saying only which project it opened in would read as though
                # somebody chose it (FR-T11).
                "scratch": subject.is_scratch,
                "model": params.model,
                "effort": params.effort,
            },
        )

    return audited_write(ctx, operation=SESSION_START, reason=reason, body=body)


def stop_session(ctx: AppContext, params: StopSessionParams) -> SessionResult:
    """Stop a session and revoke the token it was running with (FR-S10).

    The kill is attempted first and its failure is not fatal: an engine that
    has already forgotten the session, or one that is down, must not leave
    Vogt unable to close its own record. A session Vogt believes is running
    when it is not is the worse of the two wrong answers.
    """
    session = _existing(ctx, params.id)
    engine = ctx.engine
    killed: bool | None = None
    if engine is not None:
        try:
            killed = engine.kill_session(session.engine_session_id)
        except EngineUnavailable:
            killed = None

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[SessionResult]:
        del actor
        current = txn.session_by_id(params.id)
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
        # property FR-S10 promises is that nothing the session held still
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
    """Vogt's session links, enriched with what the engine says right now.

    The links are returned whether or not the engine answers. Its absence
    costs the liveness columns and nothing else (FR-E9), and the reason it
    could not be asked is reported rather than rendered as "not running".
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
        return SessionListResult(
            sessions=[
                _summarize(
                    view,
                    session,
                    engine_session=live.get(session.engine_session_id),
                    engine_asked=detail is None,
                )
                for session in sessions
            ],
            engine=detail,
        )


# -- session history (#491) ------------------------------------------------
#
# Thin read pass-throughs to the engine's history surface. All three degrade
# the FR-E9 way `list_sessions` does: no engine, or an unreachable one, sets
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
    """Full-text search over session output, live sessions included (#491)."""
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
    show" is an ordinary answer.
    """
    if ctx.engine is None:
        return LogTailResult(engine=_NO_ENGINE)
    try:
        log = ctx.engine.history_log(
            params.id, tail_bytes=params.tail_bytes, strip_ansi=params.strip_ansi
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


# -- resolution ------------------------------------------------------------


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
        #: caller (FR-T11). Carried so the name and the audit row can say so.
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
        # FR-T11. The refusal names the setting because the alternative — a
        # default working directory — is the one failure FR-E3 is written
        # against, and it fails by succeeding somewhere plausible.
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
    work, which is the inversion FR-G13 spends its whole sentence on.
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
            # and guessing one would be exactly the heuristic FR-E3 forbids.
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


def _existing(ctx: AppContext, session_id: str) -> CodingSession:
    with ctx.declared.read() as view:
        session = view.session_by_id(session_id)
    if session is None:
        msg = f"no session {session_id!r}"
        raise NotFound(msg)
    return session


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
) -> EngineSession:
    return engine.create_session(
        prompt=brief,
        name=name,
        # A template names a command the *engine* knows; Vogt passes the name
        # through rather than resolving it, because the command a template
        # runs is that pod's configuration and not the estate's.
        command=None if template is None else [template],
        cwd=cwd,
        env=env,
        # Passed through for the same reason (FR-T11): *how* a model id
        # reaches a CLI is `claude --model` or `codex -m`, which is knowledge
        # about that pod's binaries. Vogt says which model; the engine knows
        # how to ask for it, and refuses by name when it cannot.
        model=model,
        effort=effort,
    )


def _session_env(ctx: AppContext, session_id: str, secret: str) -> dict[str, str]:
    """What an agent inside the session needs to reach Vogt (FR-E5).

    The same two variables the MCP bootstrap already uses in a MyDevEnv2
    container, so an agent started here is configured the way an agent
    started by hand is. The URL is the one the operator configured for
    clients; unset means the session still runs and simply has no Vogt to
    talk to, which is reported rather than guessed (FR-A8).
    """
    env = {"VOGT_HTTP_TOKEN": secret, "VOGT_SESSION_ID": session_id}
    if ctx.config.public_url:
        env["VOGT_URL"] = ctx.config.public_url
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
        alive=(engine_session is not None) if engine_asked else None,
    )


__all__ = [
    "history_list",
    "list_sessions",
    "log_tail",
    "search_output",
    "start_session",
    "stop_session",
]
