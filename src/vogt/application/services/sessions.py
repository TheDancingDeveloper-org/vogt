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

from vogt.adapters.engine import EngineClient, EngineSession, EngineUnavailable
from vogt.application import writes
from vogt.application.context import AppContext
from vogt.application.models import (
    ListSessionsParams,
    SessionListResult,
    SessionResult,
    SessionSummary,
    StartSessionParams,
    StopSessionParams,
)
from vogt.application.services import _resolve
from vogt.application.services._brief import (
    brief_for_project,
    brief_for_work_item,
)
from vogt.application.writes import WriteOutcome, audited_write
from vogt.core.auth import Scope, issue
from vogt.core.entities import Actor, CodingSession, Token, WorkItem
from vogt.errors import Conflict, InvalidRequest, NotFound
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
            reason=writes.validate_reason(params.reason),
            started_at=now,
            stopped_at=None,
        )
        txn.insert_session(session)
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
                "project": subject.project_slug,
                "cwd": subject.cwd,
            },
        )

    return audited_write(ctx, operation=SESSION_START, reason=params.reason, body=body)


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
    ) -> None:
        self.project_id = project_id
        self.project_slug = project_slug
        self.cwd = cwd
        self.work_item = work_item
        self.brief = brief

    @property
    def work_item_id(self) -> str | None:
        return None if self.work_item is None else self.work_item.id

    @property
    def work_item_ref(self) -> str | None:
        return None if self.work_item is None else self.work_item.ref

    @property
    def default_name(self) -> str:
        return self.work_item_ref or self.project_slug


def _subject(ctx: AppContext, params: StartSessionParams, session_id: str) -> _Subject:
    if (params.work_item is None) == (params.project is None):
        msg = "give exactly one of --work-item or --project"
        raise InvalidRequest(msg)

    with ctx.declared.read() as view:
        return _resolve_subject(view, params, session_id)


def _resolve_subject(
    view: ReadView, params: StartSessionParams, session_id: str
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
            brief=brief_for_work_item(view, item, session_id),
        )

    project = _resolve.project(view, params.project or "")
    return _Subject(
        project_id=project.id,
        project_slug=project.slug,
        cwd=project.root_path,
        work_item=None,
        brief=brief_for_project(view, project.slug, session_id),
    )


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
        reason=session.reason,
        started_at=session.started_at,
        stopped_at=session.stopped_at,
        activity=None if engine_session is None else engine_session.activity,
        alive=(engine_session is not None) if engine_asked else None,
    )


__all__ = ["list_sessions", "start_session", "stop_session"]
