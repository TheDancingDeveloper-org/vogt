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

import contextlib
import logging
import os
import re
import subprocess
import threading
import time
from datetime import datetime, timedelta
from pathlib import Path
from typing import Any, Literal

from vogt.adapters import transcripts
from vogt.adapters.engine import EngineClient, EngineSession, EngineUnavailable
from vogt.adapters.engine.client import (
    EngineApproval,
    EngineBlocked,
    EngineChange,
    EngineHibernation,
    EngineResources,
    EngineScreen,
    EngineSweepEntry,
)
from vogt.application import writes
from vogt.application.context import AppContext
from vogt.application.models import (
    DIGEST_LIVE_STATES,
    SESSION_INPUT_MAX_BYTES,
    AnswerSessionParams,
    BindSessionWorkParams,
    BindSessionWorkResult,
    CancelWatchParams,
    CommitForSessionParams,
    CommitForSessionResult,
    EffectivePolicy,
    EngineSessionTokenParams,
    EngineSessionTokenResult,
    HibernateSessionParams,
    HistoryListParams,
    HistoryListResult,
    HistoryOutputMatch,
    HistorySessionRow,
    KeepSessionAwakeParams,
    ListSessionsParams,
    LogTailParams,
    LogTailResult,
    RemoveSessionParams,
    RenameSessionParams,
    ReportBlockedParams,
    ReportUnblockedParams,
    SearchOutputParams,
    SearchOutputResult,
    SessionAnswerResult,
    SessionApproval,
    SessionApprovalOption,
    SessionBlocked,
    SessionBlockedResult,
    SessionBudgetParams,
    SessionChange,
    SessionChangesParams,
    SessionChangesResult,
    SessionDigest,
    SessionDigestParams,
    SessionDigestResult,
    SessionEconomicsParams,
    SessionEconomicsResult,
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
    SessionWaitAnyParams,
    SessionWaitAnyResult,
    SessionWaitParams,
    SessionWaitResult,
    SetSessionRoleParams,
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
    ComposedBrief,
    brief_for_project,
    brief_for_work_item,
    review_brief_for_work_item,
)
from vogt.application.services._policy_conflicts import (
    overrides_section,
    task_named_base,
    task_policy_conflicts,
)
from vogt.application.services.claims import check_claim, record_claim_check
from vogt.application.services.preferences import dispatch_settings
from vogt.application.services.reports import latest_reports, one_line
from vogt.application.services.views import why
from vogt.application.writes import WriteOutcome, audited_action, audited_write
from vogt.collectors.base import CollectorError
from vogt.core import delivery, oversight, runtime
from vogt.core.auth import Scope, hash_token, issue, parse_scopes
from vogt.core.branches import default_branch_name
from vogt.core.entities import (
    Actor,
    AgentPolicy,
    CodingSession,
    LandingPolicy,
    Project,
    Token,
    WorkItem,
    WorkOverlay,
)
from vogt.core.session_economics import SessionEconomics, meter
from vogt.errors import (
    BypassRefused,
    Conflict,
    EngineOnly,
    InvalidRequest,
    NotFound,
    PolicyRefused,
    RoleRefused,
    VogtError,
)
from vogt.observability import logger
from vogt.storage.interface import ReadView, WriteTxn

_LOG = logger("sessions")

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
SESSION_COMMIT_FOR = "session.commit_for"
SESSION_COMMITTED_EVENT = "session.committed"
SESSION_SET_ROLE = "session.set_role"
SESSION_ROLE_EVENT = "session.role_set"
SESSION_RENAME = "session.rename"
SESSION_RENAMED_EVENT = "session.renamed"
SESSION_REMOVE = "session.remove"
SESSION_REMOVED_EVENT = "session.removed"
SESSION_BIND_WORK = "session.bind_work"
SESSION_WORK_BOUND_EVENT = "session.work_bound"
SESSION_WORK_UNBOUND_EVENT = "session.work_unbound"

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


def _as_digest(result: SessionResult) -> SessionResult:
    """C1: start, hibernate and wake carry the digest row as well as the summary.

    The summary stays. Parity, the CLI and the PWA read `session`; the digest
    is what an MCP caller keeps. `exclude_none` does not drop it.
    """
    return result.model_copy(update={"digest": _digest_of(result.session)})


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
    if (
        params.role != "worker"
        and ctx.principal.kind == "agent"
        and not _is_engine_credential(ctx)
    ):
        # The oversight role is a person's nomination (WI-957), and it is what
        # lets a session commit for others (C14) and ask for grants on their
        # behalf (WI-973). An agent that could start an overseer would hand
        # itself that standing by proxy: start one, drive its terminal
        # (WI-1255). The engine's own credential is not an agent here — it
        # relays the break-glass operator.
        msg = (
            f"only a person starts an oversight session ({ctx.principal.identity_ref} "
            "is an agent); start a worker, or ask a person to start the "
            "overseer from the GUI"
        )
        raise RoleRefused(msg)
    engine = _engine(ctx)
    # Resolved only after both guards above, and a profile carries neither a
    # role nor a permission posture, so naming a purpose can never raise a
    # start past what the caller could ask for directly (WI-1323).
    # Before the profile fills unset fields (WI-1671). Only a worktree the
    # caller asked for conflicts with `cwd`: the implement profile sets
    # `worktree` itself, and that is the case `cwd` exists for.
    if params.cwd and "worktree" in params.model_fields_set and params.worktree:
        msg = (
            "cwd and worktree cannot be combined: worktree cuts a new "
            "checkout, cwd opens one that already exists"
        )
        raise InvalidRequest(msg)
    params, overrides, read_only = _resolve_purpose(ctx, params)
    if params.cwd and params.worktree:
        params = params.model_copy(update={"worktree": None})
    # After the profile fills the model, before anything reaches the engine:
    # a project's review policy is a rule, and a start that would run a model
    # it forbids is refused rather than spawned (WI-1590).
    _refuse_forbidden_model(ctx, params)
    started_by_session, started_by_kind = _started_by(ctx, engine)
    session_id = ctx.id_factory("ses")
    subject = _subject(ctx, params, session_id)
    if params.cwd:
        subject.cwd = _existing_worktree_cwd(params.cwd, subject.cwd)
    caller_merges = _caller_merges(subject.policy, params, started_by_kind)
    actor_ref = f"agent:session:{session_id}"
    session_scopes = _session_scopes(ctx)
    credential = issue(session_scopes)

    # How long the engine took to accept the session (WI-927): on the audit
    # row, and logged, so a slow or refused start is attributable afterwards.
    # A refusal raises before any audit write, so it is logged here; the
    # engine audits its own side (`event=session.start`) either way.
    autopilot = _autopilot(params)
    budget = _budget(params, subject.policy)
    spawn_started = time.monotonic()
    try:
        started = _start_on_engine(
            engine,
            name=params.name or subject.default_name,
            template=params.template,
            cwd=subject.cwd,
            env=_session_env(
                ctx, session_id, credential.secret, work_item=subject.work_item_ref
            ),
            brief=_brief_with_task(
                subject.brief,
                params,
                cwd=subject.cwd,
                autopilot=autopilot,
                read_only=read_only,
                landing=subject.policy.landing,
                caller_merges=caller_merges,
            ),
            companion=subject.brief.companion,
            charter=subject.brief.charter,
            model=params.model,
            effort=params.effort,
            resume=params.resume,
            permission_mode=params.permission_mode,
            account=params.account,
            autopilot=autopilot,
            role=params.role,
            work_item=subject.work_item_ref,
            policy=subject.policy,
            budget=budget,
            worktree=(
                {
                    "branch": _worktree_branch(subject, session_id),
                    "base_ref": _worktree_base(subject.policy),
                }
                if params.worktree
                else None
            ),
        )
    except VogtError as error:
        _LOG.warning(
            "session.start failed session_id=%s template=%s "
            "engine_spawn_ms=%d error=%s",
            session_id,
            params.template,
            int((time.monotonic() - spawn_started) * 1000),
            error.code,
        )
        raise
    engine_spawn_ms = int((time.monotonic() - spawn_started) * 1000)
    _LOG.info(
        "session.start session_id=%s engine_session_id=%s template=%s "
        "engine_spawn_ms=%d",
        session_id,
        started.id,
        params.template,
        engine_spawn_ms,
    )

    # A resumed conversation is started by the engine in the directory its
    # transcript records, which may be below or beside the project root
    # (WI-871). A session given its own worktree runs in that worktree, which
    # the engine reports back as its cwd (WI-1298). Every other session runs
    # at the registry's root, which is what the engine reports back anyway.
    cwd = (
        started.cwd
        if started.cwd and (params.resume or params.worktree or params.cwd)
        else subject.cwd
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
            purpose=params.purpose,
            started_by_session=started_by_session,
            started_by_kind=started_by_kind,
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
                session=_summarize(txn, session, engine_session=started),
                effective_policy=_effective_policy(
                    subject.policy, params.task, caller_merges=caller_merges
                ),
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
                "autopilot": autopilot,
                # Recorded so that "which session was the overseer?" has an
                # answer after the engine has forgotten it (WI-957).
                "role": params.role,
                # Whether this session was started to review rather than to
                # do the work, so the audit trail says which brief it got.
                "review_of": params.review_of,
                # Named on the audit row: who started an unguarded session,
                # and why, is what an operator looks for afterwards.
                "permission_mode": params.permission_mode,
                # Which Claude Code account it was asked to run under.
                "account": params.account,
                # The dispatch purpose and what it resolved (WI-1323): the
                # start fields the caller set over the profile are listed in
                # `overrides`, so a reader sees which defaults were not used.
                "purpose": params.purpose,
                "overrides": list(overrides),
                "budget": None if budget is None else _budget_wire(budget),
                "worktree": params.worktree,
                "repo_map": params.repo_map,
                "started_by_session": started_by_session,
                "started_by_kind": started_by_kind,
                # How long the engine took to start it (WI-927); the launch
                # inside it is timed by the engine's own launch report.
                "engine_spawn_ms": engine_spawn_ms,
            },
        )

    return _as_digest(
        audited_write(ctx, operation=SESSION_START, reason=reason, body=body)
    )


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


_EXCLUDED_STATES = frozenset({"hibernated", "stopped", "exited"})
_DIGEST_CLIP = 120


def _clip(text: str | None, limit: int = _DIGEST_CLIP) -> str | None:
    if text is None:
        return None
    return text if len(text) <= limit else text[:limit]


def _digest_of(
    summary: SessionSummary,
    *,
    attention: str | None = None,
    ready: bool | None = None,
    fields: list[str] | None = None,
    budget: str | None = None,
) -> SessionDigest:
    """One line of state from a full summary (design C1).

    `command` stays off the digest; `template` is the replacement, and
    `brief_file` is null until the engine reports a path. `budget` is the
    engine's economics line when the caller has one. `supervisor` stays null
    until WI-956 exists.
    """
    extra: dict[str, Any] = {}
    for name in fields or []:
        if name in {"command"} or not hasattr(summary, name):
            continue
        value = getattr(summary, name)
        if hasattr(value, "model_dump"):
            value = value.model_dump(mode="json")
        extra[name] = value
    blocked = None if summary.blocked is None else summary.blocked.blocker
    approval = summary.approval
    report = summary.latest_report
    return SessionDigest(
        id=summary.id,
        name=summary.name,
        work_item=summary.work_item,
        activity=summary.activity,
        attention=attention,
        ready=ready,
        blocked_reason=_clip(blocked),
        blocked_detail=(
            None if blocked is None or len(blocked) <= _DIGEST_CLIP else blocked
        ),
        turn_started_at=summary.turn_started_at,
        done_at=summary.done_at,
        # A session's own report says what it did better than a clipped
        # reply does, and in fewer tokens (WI-2039): it replaces the excerpt.
        last_reply_excerpt=None
        if report is not None
        else _clip(summary.last_reply_excerpt),
        report=None if report is None else one_line(report),
        template=summary.template,
        question=None if approval is None else approval.question,
        question_kind=None if approval is None else approval.kind,
        options=None
        if approval is None or not approval.options
        else [option.label for option in approval.options],
        budget=budget,
        extra=extra or None,
    )


def _state_of(summary: SessionSummary, attention: str | None) -> str | None:
    if summary.activity == "stopped" or attention == "exited":
        return "exited" if attention == "exited" else (summary.activity or attention)
    return summary.activity or attention


def _kept(
    summary: SessionSummary, attention: str | None, states: list[str] | None
) -> bool:
    """Default listings drop hibernated, stopped and exited.

    An empty list means the caller asked for every state.
    """
    if states is not None and len(states) == 0:
        return True
    allowed = DIGEST_LIVE_STATES if states is None else frozenset(states)
    state = _state_of(summary, attention)
    if state is None:
        return "unknown" in allowed
    if state in allowed or (attention is not None and attention in allowed):
        return True
    return state not in _EXCLUDED_STATES and state in allowed


def _attention_of(
    summary: SessionSummary, *, now: datetime, stall_after: timedelta
) -> oversight.Verdict:
    """The same attention class a sweep row would carry, without a screen."""
    return oversight.classify(
        activity=summary.activity,
        alive=summary.alive,
        ready=summary.activity == "waiting-for-input",
        approval_question=None
        if summary.approval is None
        else summary.approval.question,
        blocker=None if summary.blocked is None else summary.blocked.blocker,
        approval_kind=None if summary.approval is None else summary.approval.kind,
        last_output_at=summary.last_output_at,
        now=now,
        stall_after=stall_after,
        done_at=summary.done_at,
    )


def _attention_kept(attention: str, wanted: list[str] | None) -> bool:
    if not wanted:
        return True
    names = set(wanted)
    if "needs_you" in names:
        names.discard("needs_you")
        names.update(oversight.NEEDS_YOU)
    return attention in names


def list_sessions(ctx: AppContext, params: ListSessionsParams) -> SessionListResult:
    """Vogt's session links, enriched with what the engine says right now,
    plus any live session the engine holds that Vogt never linked.

    The links are returned whether or not the engine answers. Its absence
    costs the liveness columns and nothing else, and the reason it
    could not be asked is reported rather than rendered as "not running".

    A session started outside Vogt — or one whose link Vogt lost — is still
    running, and an agent asking "what sessions are here" must be able to see
    it: reporting only Vogt's own links is why one session could not see
    another. Such unlinked sessions are joined (with `linked=False` and null
    declared fields) before any page is cut. They carry no project or work
    item, so a `project`/`work_item` filter excludes them.

    The page is cut last. A state filter applied after `limit` rows of
    hibernated links have already filled the page hides every live session
    past it and returns a near-empty list with no sign that more existed
    (WI-1266). The joined set is filtered (digest mode; full mode returns
    every state, as before), then ordered, then sliced, and
    `total`/`truncated` say whether the page is the whole answer.
    """
    live: dict[str, EngineSession] = {}
    detail: str | None = None
    # The engine's count of rows it hid before this list (WI-1289). Zero when
    # the caller asked for them, when no engine answered, or when the engine
    # is older than the object response and says nothing about what it hid.
    hidden_count = 0
    if ctx.engine is None:
        detail = "no session engine is configured (VOGT_ENGINE_URL is unset)"
    else:
        try:
            rows, hidden_count = ctx.engine.list_sessions(
                include_hidden=params.include_hidden
            )
            live = {row.id: row for row in rows}
        except EngineUnavailable as exc:
            detail = str(exc)
    joined, declared_overflow = _rows(ctx, params, live, detail)
    now = ctx.clock()
    stall_after = timedelta(minutes=params.stall_after_minutes)
    kept = [
        summary
        for summary in joined
        if params.mode == "full"
        or _kept(
            summary,
            _attention_of(summary, now=now, stall_after=stall_after).attention,
            params.states,
        )
    ]
    if params.order == "rss":
        # Stable: sessions with no sample keep their order, after the
        # measured ones.
        kept.sort(
            key=lambda s: (1, 0) if s.resources is None else (0, -s.resources.rss_bytes)
        )
    total = len(kept)
    page = kept[params.offset : params.offset + params.limit]
    # The declared half is fetched with a ceiling one past the operation's own
    # maximum page (`_DECLARED_FETCH`). When that ceiling is hit, more declared
    # links match than were joined, so the counted `total` is a floor and the
    # list is partial even when this page holds everything that was fetched
    # (WI-1266 review).
    truncated = params.offset + len(page) < total or declared_overflow
    if declared_overflow:
        total += 1
    if params.mode == "full":
        return SessionListResult(
            sessions=page,
            engine=detail,
            total=total,
            truncated=truncated,
            hidden_count=hidden_count,
        )
    digests = []
    for summary in page:
        attention = _attention_of(summary, now=now, stall_after=stall_after).attention
        digests.append(
            _digest_of(
                summary,
                attention=attention,
                ready=summary.activity == "waiting-for-input",
                fields=params.fields,
                budget=_budget_of(
                    None
                    if (row := live.get(summary.engine_session_id)) is None
                    else row.economics
                ),
            )
        )
    return SessionListResult(
        digests=digests,
        engine=detail,
        total=total,
        truncated=truncated,
        hidden_count=hidden_count,
    )


#: Screen lines the sweep always reads, to find an input box (WI-1870).
_SWEEP_INPUT_LINES = 8


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
        # Enough of every screen to see an input box holding unsent text
        # (WI-1870), whatever the caller asked to be shown.
        entries = ctx.engine.sweep_sessions(
            screen_lines=max(params.screen_lines, _SWEEP_INPUT_LINES)
        )
        if entries is None:
            # An engine that predates the sweep route: the same table, from
            # the list, without screens.
            live_rows, _hidden = ctx.engine.list_sessions()
            entries = [
                EngineSweepEntry(session=row)
                for row in live_rows
                if row.alive or row.hibernated
            ]
    except EngineUnavailable as exc:
        return SessionSweepResult(
            rows=[], counts={"total": 0, "needs_you": 0}, swept_at=now, engine=str(exc)
        )
    live = {entry.session.id: entry.session for entry in entries}
    tails = {entry.session.id: entry for entry in entries}
    summaries, _declared_overflow = _rows(
        ctx,
        # Unstopped links only. The declared half is capped, and a history of
        # stopped links would fill that cap with rows the sweep then discards
        # (it keeps only sessions the engine still runs), pushing a live link
        # older than the newest 500 starts out of the table entirely (WI-1266
        # review).
        ListSessionsParams(project=params.project, limit=500, offset=0),
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
        stale = _block_is_stale(summary)
        verdict = oversight.classify(
            activity=summary.activity,
            alive=summary.alive,
            ready=entry.ready,
            approval_question=None
            if summary.approval is None
            else summary.approval.question,
            blocker=None
            if summary.blocked is None or stale
            else summary.blocked.blocker,
            approval_kind=None if summary.approval is None else summary.approval.kind,
            last_output_at=summary.last_output_at,
            now=now,
            stall_after=stall_after,
            done_at=summary.done_at,
            turns=_turns_of(live.get(summary.engine_session_id)),
        )
        if verdict.attention in {"waiting", "done", "idle"} and (
            delivery.input_pending(tuple(entry.screen_tail)) is not None
        ):
            verdict = oversight.Verdict("waiting", oversight.INPUT_PENDING)
        if verdict.attention in {"waiting", "done", "idle"} and (
            delivery.codex_error_wedge(tuple(entry.screen_tail)) is not None
        ):
            verdict = oversight.Verdict("waiting", oversight.WEDGED)
        if (
            verdict.attention == "done"
            and ctx.config.autopilot_done_requires_result_report
            and not oversight.result_reported(
                report_kind=None
                if summary.latest_report is None
                else summary.latest_report.kind,
                report_at=None
                if summary.latest_report is None
                else summary.latest_report.created_at,
                turn_started_at=summary.turn_started_at,
            )
        ):
            # WI-2039: a DONE line without a result report is not taken as
            # done; oversight asks for the report instead.
            verdict = oversight.Verdict("waiting", oversight.DONE_WITHOUT_REPORT)
        reason = verdict.reason
        claim_check = None
        if verdict.attention == "done" and summary.work_item is not None:
            # WI-1835: a DONE claim is checked against git and the forge, and a
            # mismatch is said in the row's reason so the digest carries it.
            claim_check = check_claim(
                ctx,
                summary,
                claim="autopilot-done",
                text="\n".join([summary.last_reply_excerpt or "", *entry.screen_tail]),
            )
            # Once per DONE: the key is the session and the moment it said so.
            if summary.done_at is not None:
                record_claim_check(
                    ctx,
                    claim_check,
                    once_key=f"{summary.id}@{summary.done_at.isoformat()}",
                )
            if claim_check.mismatches:
                reason = f"{reason}; claim check: " + "; ".join(claim_check.mismatches)
        rows.append(
            SessionSweepRow(
                attention=verdict.attention,
                attention_reason=reason,
                session=summary,
                screen_tail=list(entry.screen_tail)[-params.screen_lines :]
                if params.screen_lines
                else [],
                ready=entry.ready,
                **_block_age(summary, now, stale=stale),
                claim_check=claim_check,
            )
        )
    rows.sort(
        key=lambda row: (
            oversight.ORDER.get(row.attention, 99),  # type: ignore[call-overload]
            -(
                (row.session.last_output_at if row.session is not None else None) or now
            ).timestamp(),
        )
    )
    counts: dict[str, int] = {"total": len(rows), "needs_you": 0}
    for row in rows:
        counts[row.attention] = counts.get(row.attention, 0) + 1
        if row.attention in oversight.NEEDS_YOU:
            counts["needs_you"] += 1
    if params.mode == "digest":
        kept: list[SessionSweepRow] = []
        for row in rows:
            if row.session is None:
                continue
            if not _kept(row.session, row.attention, params.states):
                continue
            if not _attention_kept(row.attention, params.attention):
                continue
            kept.append(
                SessionSweepRow(
                    attention=row.attention,
                    attention_reason=row.attention_reason,
                    digest=_digest_of(
                        row.session,
                        attention=row.attention,
                        ready=row.ready,
                        fields=params.fields,
                        budget=_budget_of(
                            None
                            if (live_row := live.get(row.session.engine_session_id))
                            is None
                            else live_row.economics
                        ),
                    ),
                    screen_tail=row.screen_tail,
                    ready=row.ready,
                    blocked_since=row.blocked_since,
                    blocked_age_s=row.blocked_age_s,
                    blocked_stale=row.blocked_stale,
                    claim_check=row.claim_check,
                )
            )
        rows = kept[: params.limit]
    return SessionSweepResult(rows=rows, counts=counts, swept_at=now, engine=None)


def _block_is_stale(summary: SessionSummary) -> bool:
    """Whether the session's blocked report predates its current turn.

    A report is made mid-turn, just before the agent stops. A turn that
    began after it means the session went back to work — it was typed into,
    woken, or nudged on — so the report no longer says what it waits on
    (WI-1834). So does a done declaration (`AUTOPILOT: DONE`) made after
    the report, even in the same turn: the agent says nothing is left for it.
    The report stays on the row, flagged stale, so it is not lost.
    """
    blocked = summary.blocked
    if blocked is None or blocked.since is None:
        return False
    return any(
        at is not None and at > blocked.since
        for at in (summary.turn_started_at, summary.done_at)
    )


def _block_age(
    summary: SessionSummary | None, now: datetime, *, stale: bool
) -> dict[str, Any]:
    """The sweep row's blocked_since / blocked_age_s / blocked_stale."""
    since = None
    if summary is not None and summary.blocked is not None:
        since = summary.blocked.since
    if since is None:
        return {"blocked_stale": stale}
    return {
        "blocked_since": since,
        "blocked_age_s": max(0, int((now - since).total_seconds())),
        "blocked_stale": stale,
    }


def _starter_engine_id(view: ReadView, named: str | None) -> str | None:
    """The engine id a `started_by_session` filter matches, or None for no filter.

    The column stores the starter's engine session id. A caller may name
    either that id or the starter's Vogt `ses_…` id.
    """
    if named is None:
        return None
    session = view.session_by_id(named) or view.session_by_engine_id(named)
    if session is not None:
        return session.engine_session_id
    return named


def _rows(
    ctx: AppContext,
    params: ListSessionsParams,
    live: dict[str, EngineSession],
    detail: str | None,
) -> tuple[list[SessionSummary], bool]:
    """The rows `session.list` and `session.sweep` share: Vogt's links joined
    with what the engine reported (`live`, empty when `detail` says why it
    could not be asked), unlinked engine sessions appended, and reply
    excerpts read from the agents' transcripts.

    The second element is true when more declared links match than were
    fetched: the declared half is capped, so past the cap the joined set is
    not the whole answer and the caller must say so.
    """
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
        starter = _starter_engine_id(view, params.started_by_session)
        sessions = view.list_sessions(
            project_id=project_id,
            work_item_id=work_item_id,
            started_by_session=starter,
            include_stopped=params.include_stopped,
            # The page is cut after the state filter, so the declared half
            # has to be the whole filtered set. One past the operation's own
            # ceiling (`ListSessionsParams.limit` max 500) so a match beyond
            # it is visible as overflow instead of a silently complete list.
            limit=_DECLARED_FETCH,
            offset=0,
        )
        declared_overflow = len(sessions) == _DECLARED_FETCH
        if declared_overflow:
            sessions = sessions[: _DECLARED_FETCH - 1]
        summaries = [
            _summarize(
                view,
                session,
                engine_session=live.get(session.engine_session_id),
                engine_asked=detail is None,
            )
            for session in sessions
        ]

        # Append engine sessions Vogt never linked. A project/work-item
        # filter is a filter on the declared half, which these do not have, so
        # they surface only when neither filter is set. The caller pages the
        # joined set afterwards, so they are not sliced here: slicing to the
        # page size before the state filter drops a live unlinked session
        # behind a page of hibernated links (WI-1266).
        # An unlinked session whose process has exited is the engine's
        # leftover, not a running terminal: it is listed only when stopped
        # sessions were asked for, as a linked one Vogt stopped would be.
        unfiltered = project_id is None and work_item_id is None and starter is None
        if unfiltered:
            for engine_session in live.values():
                # A hibernated session is asleep, not stopped: listed always.
                if (
                    not engine_session.alive
                    and not engine_session.hibernated
                    and not params.include_stopped
                ):
                    continue
                if view.session_by_engine_id(engine_session.id) is None:
                    summaries.append(_summarize_engine_only(engine_session, view))

        templates = {
            summary.engine_session_id: summary.template for summary in summaries
        }
        # Each row's newest report (WI-2039): one indexed read for the page.
        reports = latest_reports(
            view, [summary.engine_session_id for summary in summaries]
        )
        summaries = [
            summary
            if (reported := reports.get(summary.engine_session_id)) is None
            else summary.model_copy(update={"latest_report": reported})
            for summary in summaries
        ]

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
        elif live_session.conversation_agent == "opencode":
            excerpt = _opencode_excerpt(ctx, live_session.id)
            if excerpt is not None:
                update["last_reply_excerpt"] = excerpt
        summaries[index] = summary.model_copy(update=update)
    return summaries, declared_overflow


def _opencode_replies(
    ctx: AppContext, engine_session_id: str, n: int
) -> tuple[str | None, list[transcripts.Reply]] | None:
    """An opencode session's replies, from the engine (WI-931): opencode keeps
    no transcript file, and its store holds credentials the core is not
    given. Redacted here, as a transcript's are."""
    if ctx.engine is None:
        return None
    try:
        found = ctx.engine.session_replies(engine_session_id, n=n)
    except EngineUnavailable:
        return None
    if found is None:
        return None
    conversation_id, replies = found
    return conversation_id, [
        transcripts.reply(text, _parse_engine_timestamp(at)) for text, at in replies
    ]


def _opencode_excerpt(ctx: AppContext, engine_session_id: str) -> str | None:
    found = _opencode_replies(ctx, engine_session_id, 1)
    if not found or not found[1]:
        return None
    return transcripts.excerpt(found[1][-1].text)


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
    if engine_session is not None and engine_session.conversation_agent == "opencode":
        opencode = _opencode_replies(ctx, engine_id, params.n)
        if opencode is not None:
            conversation_id, replies = opencode
            return result.model_copy(
                update={
                    "agent": "opencode",
                    "conversation_id": conversation_id,
                    "basis": "engine-id",
                    "messages": [SessionReply(text=r.text, at=r.at) for r in replies],
                    "detail": None
                    if replies
                    else "opencode has no assistant reply yet",
                }
            )
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


def _budget_of(payload: dict[str, Any] | None) -> str | None:
    """The digest line from an engine economics reading, or None when the
    engine has not measured the session."""
    if not payload:
        return None
    tools = payload.get("tool_result_bytes")
    reading = SessionEconomics(
        turns=_int_of(payload.get("turns")),
        context_tokens=_int_of(payload.get("context_tokens")),
        peak_context_tokens=_int_of(payload.get("peak_context_tokens")),
        compactions=_int_of(payload.get("compactions")),
        tool_result_bytes={
            str(name): _int_of(value)
            for name, value in tools.items()
            if isinstance(name, str)
        }
        if isinstance(tools, dict)
        else {},
        reads_repeated=_int_of(payload.get("reads_repeated")),
        output_tokens=_int_of(payload.get("output_tokens")),
        usage_measured=payload.get("usage_measured") is True,
    )
    return reading.budget_line(None, None)


def _int_of(value: object) -> int:
    return value if isinstance(value, int) and not isinstance(value, bool) else 0


def economics(
    ctx: AppContext, params: SessionEconomicsParams
) -> SessionEconomicsResult:
    """What the session's agent transcript cost, read whole.

    The same finding as `last_reply`, but the file is metered rather than
    tailed: context, compactions and tool bytes live anywhere in it, not in
    the last few megabytes. Cost stays null (no price table) and the worktree
    is not measured here, so commits and files_changed stay null rather than
    claiming a clean tree.
    """
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
    result = SessionEconomicsResult(
        id=params.id,
        engine_session_id=engine_id,
        account=None if engine_session is None else engine_session.account,
    )
    reported = None if engine_session is None else engine_session.economics
    if reported:
        # The engine's tailer already metered the transcript. Re-reading the
        # file here would repeat its work and could disagree with the number
        # the session list shows.
        return result.model_copy(
            update={
                "agent": engine_session.conversation_agent if engine_session else None,
                "conversation_id": engine_session.conversation_id
                if engine_session
                else None,
                "basis": "engine",
                "turns": _int_of(reported.get("turns")),
                "context_tokens": _int_of(reported.get("context_tokens")),
                "peak_context_tokens": _int_of(reported.get("peak_context_tokens")),
                "compactions": _int_of(reported.get("compactions")),
                "tool_result_bytes": {
                    str(name): _int_of(value)
                    for name, value in reported["tool_result_bytes"].items()
                }
                if isinstance(reported.get("tool_result_bytes"), dict)
                else {},
                "reads_repeated": _int_of(reported.get("reads_repeated")),
                "wall_clock_count": (
                    _int_of(reported["wall_clock"].get("count"))
                    if isinstance(reported.get("wall_clock"), dict)
                    else 0
                ),
                "wall_clock_max_ms": _int_of(
                    (reported.get("wall_clock") or {}).get("max_ms")
                )
                if isinstance(reported.get("wall_clock"), dict)
                else 0,
                "wall_clock_mean_ms": _int_of(
                    (reported.get("wall_clock") or {}).get("mean_ms")
                )
                if isinstance(reported.get("wall_clock"), dict)
                else 0,
                "output_tokens": _int_of(reported.get("output_tokens")),
                "usage_measured": reported.get("usage_measured") is True,
                "budget_line": _budget_of(reported) or "",
                "detail": None,
            }
        )
    if engine_session is not None and engine_session.conversation_agent == "opencode":
        return result.model_copy(
            update={
                "agent": "opencode",
                "basis": "engine-id",
                "detail": "opencode transcripts are not metered",
            }
        )
    roots = ctx.config.session_transcript_roots
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
    try:
        text = found.path.read_text(encoding="utf-8", errors="replace")
    except OSError:
        return result.model_copy(
            update={
                "agent": found.agent,
                "conversation_id": found.conversation_id,
                "basis": found.basis,
                "transcript": str(found.path),
                "detail": "the transcript could not be read",
            }
        )
    reading = meter(text.splitlines(), agent=found.agent)
    return result.model_copy(
        update={
            "agent": found.agent,
            "conversation_id": found.conversation_id,
            "basis": found.basis,
            "transcript": str(found.path),
            "turns": reading.turns,
            "context_tokens": reading.context_tokens,
            "peak_context_tokens": reading.peak_context_tokens,
            "compactions": reading.compactions,
            "tool_result_bytes": dict(reading.tool_result_bytes),
            "reads_repeated": reading.reads_repeated,
            "wall_clock_count": reading.wall_clock.count,
            "wall_clock_max_ms": reading.wall_clock.max_ms,
            "wall_clock_mean_ms": reading.wall_clock.mean_ms,
            "output_tokens": reading.output_tokens,
            "usage_measured": reading.usage_measured,
            "budget_line": reading.budget_line(None, None),
            "detail": None,
        }
    )


_EXCERPT_LIMIT = 120


def _turns_of(engine_session: EngineSession | None) -> int | None:
    """The engine's metered turn count, or None when it has not metered one."""
    if engine_session is None or not engine_session.economics:
        return None
    return _int_or_none(engine_session.economics.get("turns"))


def _int_or_none(value: object) -> int | None:
    """An economics counter the engine sent, or None when it did not.

    A bool is an int in Python and is not a count. A float is not a count
    either. Anything else is absent rather than zero: zero would say the
    session was measured and did nothing.
    """
    if isinstance(value, bool) or not isinstance(value, int):
        return None
    return value


def digest(ctx: AppContext, params: SessionDigestParams) -> SessionDigestResult:
    """One session's digest, from a single engine row plus a transcript tail.

    Numbers come from the row's `economics` when the engine already computed
    them, and stay null when it did not. This read does not meter a transcript.
    The excerpt is the same tail read the list digest uses, clipped to 120
    characters, and null when the transcript cannot be read.
    """
    target = _target(ctx, params.id)
    engine_id = target.engine_session_id
    engine_session: EngineSession | None = None
    if ctx.engine is not None:
        try:
            engine_session = ctx.engine.get_session(engine_id)
        except EngineUnavailable:
            engine_session = None
    if engine_session is None and target.session is None:
        msg = f"no session {params.id!r}"
        raise NotFound(msg)
    if engine_session is None:
        return SessionDigestResult(id=params.id)
    numbers = engine_session.economics
    turns = context = peak = compactions = output = None
    usage_measured: bool | None = None
    context_pct: int | None = None
    budget: str | None = None
    if numbers is not None:
        turns = _int_or_none(numbers.get("turns"))
        context = _int_or_none(numbers.get("context_tokens"))
        peak = _int_or_none(numbers.get("peak_context_tokens"))
        compactions = _int_or_none(numbers.get("compactions"))
        output = _int_or_none(numbers.get("output_tokens"))
        measured = numbers.get("usage_measured")
        usage_measured = measured if isinstance(measured, bool) else None
        reading = SessionEconomics(
            turns=turns or 0,
            context_tokens=context or 0,
            peak_context_tokens=peak or 0,
            compactions=compactions or 0,
            output_tokens=output or 0,
            usage_measured=bool(usage_measured),
        )
        budget = reading.budget_line(None, params.context_window)
        if params.context_window and context is not None:
            context_pct = min(100, round(100 * context / params.context_window))
    excerpt = _digest_excerpt(
        ctx,
        engine_session,
        None if target.session is None else target.session.template,
    )
    with ctx.declared.read() as view:
        report = latest_reports(view, [engine_session.id]).get(engine_session.id)
    return SessionDigestResult(
        id=params.id,
        name=engine_session.name or None,
        work_item=engine_session.work_item,
        activity=engine_session.activity,
        blocked=_blocked(engine_session.blocked),
        last_reply_excerpt=excerpt,
        latest_report=report,
        turns=turns,
        context_tokens=context,
        peak_context_tokens=peak,
        compactions=compactions,
        output_tokens=output,
        usage_measured=usage_measured,
        context_pct=context_pct,
        budget_line=budget,
        updated_at=_parse_engine_timestamp(engine_session.activity_changed_at),
    )


def _digest_excerpt(
    ctx: AppContext, engine_session: EngineSession, template: str | None
) -> str | None:
    """The list digest's tail read, clipped, and null on any failure.

    A transcript that cannot be found or read is an absent excerpt, never an
    error that fails the digest.
    """
    found = _transcript(ctx, engine_session, template)
    if found is None:
        return None
    excerpt = transcripts.last_reply_excerpt(found)
    if excerpt is None or len(excerpt) <= _EXCERPT_LIMIT:
        return excerpt
    return excerpt[:_EXCERPT_LIMIT]


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
                command=None if params.mode == "summary" else row.command,
                scrollback_bytes=row.scrollback_bytes,
                template=row.template,
                role=row.role,
                conversation_agent=row.conversation_agent,
                conversation_id=row.conversation_id,
                resume_template=row.resume_template,
                work_item=row.work_item,
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

    While the session shows a permission prompt, every keystroke answers it,
    so the engine refuses input from anyone but a person (`PersonRequired`,
    WI-983) or an oversight session (WI-2028); this says which the caller is
    (`_answers_as_person`, `_is_oversight_caller`).
    """
    reason = writes.validate_reason(params.reason)
    text = params.text or ""
    keys = list(params.keys or [])
    # `submit` defaults to pressing Enter after text. `draft` is the opt-out
    # for typing without sending, and an explicit `submit=false` means the
    # same thing. A call that names only keys keeps its old behaviour: the
    # keys are pressed and nothing else, unless `submit` is given explicitly.
    # An "enter" among the keys is already the submit, so the default adds no
    # second, blind Enter after it.
    explicit_submit = "submit" in params.model_fields_set
    submit = (
        params.submit
        and not params.draft
        and (explicit_submit or (bool(text) and "enter" not in keys))
    )
    if not text and not keys and not submit:
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
    person = _answers_as_person(ctx)
    # An oversight session is relayed by its engine id: the engine lets it
    # answer a worker's permission prompt and audits it as that session
    # (WI-2028). Anyone else still gets `PersonRequired`.
    overseer = (
        _caller_engine_session_id(ctx)
        if not person and _is_oversight_caller(ctx, engine)
        else None
    )

    writes_in_order: list[tuple[str, bool]] = []
    if text:
        writes_in_order.append((text, False))
    writes_in_order.extend((SESSION_KEYS[key], False) for key in keys)
    if submit:
        writes_in_order.append(("", True))
    # Enter by either route is a submit: `keys: ["enter"]` used to report
    # `submitted: false` (WI-918).
    submitted = submit or "enter" in keys
    confirming = submitted and params.confirm
    before: str | None = None
    seen = None
    if confirming:
        # What the agent was doing as the input arrived decides between
        # "starts a turn" and "waits behind one". A failed read only costs
        # the judgement its first clue.
        try:
            seen = engine.get_session(engine_id)
        except EngineUnavailable:
            seen = None
        before = None if seen is None else seen.activity
    # Text and Enter to an agent whose input box Vogt can read go one step
    # at a time, each checked on the screen (WI-2025).
    stepwise = confirming and submit and bool(text) and not keys
    woke = False
    woken_verdict: delivery.Verdict | None = None
    try:
        if stepwise and seen is not None and seen.hibernated:
            msg = "hibernated"
            raise Conflict(msg)
        if stepwise and _has_input_box(seen):
            woken_verdict = _type_then_submit(
                engine, engine_id, params.id, text, before=before, person=person
            )
        else:
            _send_all(
                engine,
                engine_id,
                params.id,
                writes_in_order,
                person=person,
                overseer=overseer,
            )
    except Conflict:
        # The engine refuses input to a hibernated session. Typing into one
        # is asking for it back: wake it — resuming its conversation, with a
        # new token — wait until it is at its prompt, then type. Anything
        # else the engine refused stays refused.
        current = engine.get_session(engine_id)
        if current is None or not current.hibernated:
            raise
        # Never forced. A session whose work was handed on, or whose work item
        # is already live in the same directory, stays asleep and the engine's
        # refusal (409) is the answer.
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
        # A resumed agent can take the text but not the Enter: a resumed
        # Codex swallows it while settling (WI-1669), a just-woken Claude
        # left it in its box, or read it as a newline (WI-2025). Stepwise,
        # checked on the screen, on the agents whose box Vogt can read.
        if stepwise and _has_input_box(current):
            woken_verdict = _type_then_submit(
                engine, engine_id, params.id, text, before=before, person=person
            )
        else:
            _send_all(
                engine,
                engine_id,
                params.id,
                writes_in_order,
                person=person,
                overseer=overseer,
            )

    if woken_verdict is not None or confirming:
        verdict = woken_verdict or _confirm_delivery(
            engine, engine_id, before, text or None
        )
        if verdict.delivery == "queued" and text and submit:
            # Behind a running turn. The CLI should take it when the turn
            # ends, but a Claude Code turn can end with the text left in
            # the box, unsent (WI-1870): watch for that and press Enter.
            _follow_queued(engine, engine_id, params.id, text, person=person)
    else:
        verdict = delivery.judge(submitted=submitted, before=None, after=[])

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
            "submit": submit,
            "draft": params.draft,
            "submitted": submitted,
            "delivery": verdict.delivery,
            "woke": woke,
            "person": person,
            "oversight": overseer is not None,
        },
        event_kind=SESSION_INPUT_EVENT,
    )
    digest = _input_digest(ctx, target, submitted=submitted, reason=reason)
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
        digest=digest,
    )


#: How long `session.input` watches for what became of a submitted input:
#: up to `_CONFIRM_READS` screen reads, `_CONFIRM_PAUSE_S` apart. A module
#: value so tests can make it instant.
_CONFIRM_READS = 10
_CONFIRM_PAUSE_S = 0.2


def _pause(seconds: float) -> None:
    time.sleep(seconds)


#: A submitted text gets at most this many Enters, the first included, each
#: after the agent has settled for up to `_SETTLE_S` (WI-1669, WI-2025).
_MAX_ENTERS = 3
_SETTLE_S = 10

#: Before typing into an agent whose input box is not drawn yet — still
#: starting or resuming — wait up to this long for its ready prompt; then
#: watch for the typed text to show in the box for up to `_TYPED_READS`
#: reads, `_CONFIRM_PAUSE_S` apart, before pressing Enter (WI-2025).
_READY_GATE_S = 30
_TYPED_READS = 10


def _has_input_box(session: object) -> bool:
    """Whether a session the engine reported runs an agent whose input box
    `delivery.composer_text` can read: Claude Code or Codex."""
    agent = getattr(session, "conversation_agent", None)
    return agent in {"claude", "codex"}


def _read(engine: EngineClient, engine_id: str) -> delivery.Observation | None:
    """One read of the session's screen, or None when it cannot be read."""
    try:
        screen = engine.session_screen(engine_id)
    except EngineUnavailable:
        return None
    if screen is None:
        return None
    return delivery.Observation(activity=screen.activity, lines=screen.lines)


def _box_drawn(engine: EngineClient, engine_id: str) -> bool:
    seen = _read(engine, engine_id)
    return seen is not None and delivery.composer_text(seen.lines) is not None


def _type_then_submit(
    engine: EngineClient,
    engine_id: str,
    named: str,
    text: str,
    *,
    before: str | None,
    person: bool,
) -> delivery.Verdict:
    """Type `text`, then press Enter, each step checked on the screen
    (WI-2025).

    Written together, a just-woken Claude Code took the text and the Enter
    as one burst: the Enter landed as a newline, or not at all, while the
    resume's own redraw read as a turn starting. So: an agent with no input
    box drawn and no turn running is still starting — wait for its ready
    prompt. Then type, and wait for the text to show in the box before the
    Enter, so the Enter arrives as a key of its own. Then judge, pressing
    Enter again while the box still holds the text (`_submit_until_taken`).
    """
    boxed = _box_drawn(engine, engine_id)
    if not boxed and before not in {"running", "awaiting-approval"}:
        with contextlib.suppress(EngineUnavailable):
            engine.wait_session(engine_id, until="ready", timeout_s=_READY_GATE_S)
        boxed = _box_drawn(engine, engine_id)
    _send_all(engine, engine_id, named, [(text, False)], person=person)
    if boxed:
        for _ in range(_TYPED_READS):
            seen = _read(engine, engine_id)
            if seen is None or delivery.composer_holds(seen.lines, text):
                break
            _pause(_CONFIRM_PAUSE_S)
    _send_all(engine, engine_id, named, [("", True)], person=person)
    return _submit_until_taken(engine, engine_id, named, before, text, person=person)


def _submit_until_taken(
    engine: EngineClient,
    engine_id: str,
    named: str,
    before: str | None,
    text: str,
    *,
    person: bool,
) -> delivery.Verdict:
    """Judge an Enter already pressed, pressing it again while the input box
    still holds the text (WI-1669, WI-2025).

    Bounded to `_MAX_ENTERS`. A "delivered" judged from a turn that started
    is checked once more: a resume's own turn reads as running too, and the
    text left in the box after it is the sign the Enter did not land. Before
    each further Enter it waits for the prompt to settle and reads the
    screen once more; it presses only when the box is drawn, still holds
    the text and no dialog or approval shows. It stops as soon as the box
    clears.
    """
    verdict = _settled(
        engine, engine_id, text, _confirm_delivery(engine, engine_id, before, text)
    )
    for _ in range(_MAX_ENTERS - 1):
        if verdict.delivery not in {"unconfirmed", "stuck"}:
            break
        try:
            engine.wait_session(engine_id, until="ready", timeout_s=_SETTLE_S)
        except EngineUnavailable:
            break
        seen = _read(engine, engine_id)
        if seen is None:
            break
        if not delivery.safe_to_resubmit(seen, text):
            if delivery.composer_text(seen.lines) is not None:
                # The box is drawn and no longer holds the text: taken.
                verdict = delivery.Verdict(
                    "delivered", "the input box cleared after the Enter"
                )
            break
        _send_all(engine, engine_id, named, [("", True)], person=person)
        verdict = _settled(
            engine, engine_id, text, _confirm_delivery(engine, engine_id, before, text)
        )
    return verdict


def _settled(
    engine: EngineClient, engine_id: str, text: str, verdict: delivery.Verdict
) -> delivery.Verdict:
    """A "delivered" verdict, re-checked once after a pause: still the text
    in the box, with no turn running, is `stuck` (WI-2025)."""
    if verdict.delivery != "delivered":
        return verdict
    _pause(_CONFIRM_PAUSE_S)
    seen = _read(engine, engine_id)
    if seen is None or seen.activity == "running":
        return verdict
    return delivery.judge(submitted=True, before=None, after=[seen], pending=text)


#: How long a queued input is followed for its turn to end, and how many
#: Enters it gets once the agent is at rest with the text still in its box.
_QUEUED_FOLLOW_S = 3600
_QUEUED_MAX_ENTERS = 2

#: The longest single wait the engine accepts (its `/wait` caps at 600 s
#: and refuses more); a longer follow-up waits in chunks of this.
_ENGINE_WAIT_MAX_S = 600

_log = logging.getLogger(__name__)


def _follow_queued(
    engine: EngineClient, engine_id: str, named: str, text: str, *, person: bool
) -> None:
    """Follow a queued input in the background (WI-1870). A module value so
    tests can run it inline."""
    threading.Thread(
        target=_submit_queued_logged,
        args=(engine, engine_id, named, text),
        kwargs={"person": person},
        name=f"vogt-queued-input-{engine_id}",
        daemon=True,
    ).start()


def _submit_queued_logged(
    engine: EngineClient, engine_id: str, named: str, text: str, *, person: bool
) -> None:
    """The follow-up thread's body: nothing it raises may die silently."""
    try:
        _submit_queued(engine, engine_id, named, text, person=person)
    except Exception:
        _log.exception("following queued input to %s failed", named)


def _wait_turn_end(engine: EngineClient, engine_id: str) -> bool:
    """Wait up to `_QUEUED_FOLLOW_S` for the session to be ready, in waits
    of at most `_ENGINE_WAIT_MAX_S` each. False when it never was, or the
    engine no longer holds it."""
    chunks = -(-_QUEUED_FOLLOW_S // _ENGINE_WAIT_MAX_S)
    for _ in range(chunks):
        waited = engine.wait_session(
            engine_id, until="ready", timeout_s=_ENGINE_WAIT_MAX_S
        )
        if waited is None:
            return False
        if waited.outcome == "ready":
            return True
        if waited.outcome != "timeout":
            # Exited, hibernated, refused: nothing left to submit to.
            return False
    return False


def _submit_queued(
    engine: EngineClient, engine_id: str, named: str, text: str, *, person: bool
) -> delivery.Delivery:
    """Once the running turn ends, make sure the queued text was taken
    (WI-1870).

    Waits for the agent to be ready, then reads its screen: a box that no
    longer holds the text means the CLI took it. A box still holding it,
    with no dialog or approval showing, gets an Enter, at most
    `_QUEUED_MAX_ENTERS` times. Still held after that is `stuck`; the sweep
    shows such a session as "input pending" from its screen.
    """
    try:
        if not _wait_turn_end(engine, engine_id):
            return "queued"
        for _ in range(_QUEUED_MAX_ENTERS):
            screen = engine.session_screen(engine_id)
            if screen is None:
                return "unconfirmed"
            seen = delivery.Observation(activity=screen.activity, lines=screen.lines)
            if not delivery.safe_to_resubmit(seen, text):
                return "delivered"
            _send_all(engine, engine_id, named, [("", True)], person=person)
            verdict = _confirm_delivery(engine, engine_id, "waiting-for-input", text)
            if verdict.delivery == "delivered":
                _log.info("queued input to %s submitted by a follow-up Enter", named)
                return "delivered"
    except (EngineUnavailable, NotFound):
        return "unconfirmed"
    _log.warning("queued input to %s is stuck in its input box", named)
    return "stuck"


def _confirm_delivery(
    engine: EngineClient,
    engine_id: str,
    before: str | None,
    pending: str | None = None,
) -> delivery.Verdict:
    """Read the session a few times after a submit and judge, stopping at
    the first decisive read. An engine that cannot be read leaves the
    judgement to what was seen before. `pending` is the submitted text
    when the input box should be checked for it."""
    observed: list[delivery.Observation] = []
    verdict = delivery.judge(
        submitted=True, before=before, after=observed, pending=pending
    )
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
        verdict = delivery.judge(
            submitted=True, before=before, after=observed, pending=pending
        )
        # A held box can be the CLI not yet having read the Enter: `stuck`
        # stands only if it is still held at the last read.
        if verdict.delivery not in {"unconfirmed", "stuck"}:
            break
    return verdict


def _send_all(
    engine: EngineClient,
    engine_id: str,
    named: str,
    chunks: list[tuple[str, bool]],
    *,
    person: bool,
    overseer: str | None = None,
) -> None:
    for chunk, submit in chunks:
        if not engine.send_input(
            engine_id, chunk, submit=submit, person=person, overseer=overseer
        ):
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
        shown = _screen_result(params.id, engine_id, screen)
        return SessionWaitResult(
            id=params.id,
            engine_session_id=engine_id,
            outcome="hibernated",
            matched=False,
            waited_ms=0,
            screen=shown if params.screen else None,
            digest=None if params.screen else _wait_digest(ctx, params.id, engine_id),
            screen_tail=[] if params.screen else _tail_of(shown),
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
    shown = _screen_result(params.id, engine_id, waited.screen)
    return SessionWaitResult(
        id=params.id,
        engine_session_id=engine_id,
        outcome=waited.outcome,
        matched=waited.matched,
        waited_ms=waited.waited_ms,
        screen=shown if params.screen else None,
        digest=None if params.screen else _wait_digest(ctx, params.id, engine_id),
        screen_tail=[] if params.screen else _tail_of(shown),
    )


def _session_labels(ctx: AppContext, engine_ids: list[str]) -> dict[str, str]:
    """Vogt's `ses_…` id for each linked engine session, else the uuid itself.

    One read for the page: opening the store per change is a query per row.
    """
    labels: dict[str, str] = {}
    with ctx.declared.read() as view:
        for engine_id in dict.fromkeys(engine_ids):
            linked = view.session_by_engine_id(engine_id)
            labels[engine_id] = linked.id if linked is not None else engine_id
    return labels


def _change(change: EngineChange, labels: dict[str, str]) -> SessionChange:
    return SessionChange(
        seq=change.seq,
        at=_parse_engine_timestamp(change.at),
        session=labels.get(change.session, change.session),
        kind=change.kind,
        summary=change.summary,
    )


def _engine_ids(ctx: AppContext, ids: list[str]) -> list[str] | None:
    """Resolve named sessions to engine ids; `None` means every session."""
    named = [one.strip() for one in ids if one.strip()]
    if not named:
        return None
    return [_target(ctx, one).engine_session_id for one in named]


def session_changes(
    ctx: AppContext, params: SessionChangesParams
) -> SessionChangesResult:
    """Read session deltas published after `since`.

    One call replaces re-sweeping every session. The engine keeps a bounded
    ring of what it published; this reads a page of it.
    """
    engine = _engine(ctx)
    ids = _engine_ids(ctx, params.ids)
    try:
        page = engine.session_changes(since=params.since, ids=ids, limit=params.limit)
    except Conflict:
        page = None
    if page is None:
        msg = (
            "the session engine does not support changes yet (it has no "
            "GET /api/sessions/changes route); upgrade the engine, or poll "
            "session.sweep"
        )
        raise EngineUnavailable(msg)
    labels = _session_labels(ctx, [one.session for one in page.changes])
    return SessionChangesResult(
        changes=[_change(one, labels) for one in page.changes],
        next_cursor=page.next_cursor,
        truncated=page.truncated,
    )


def session_wait_any(
    ctx: AppContext, params: SessionWaitAnyParams
) -> SessionWaitAnyResult:
    """Block until one named session publishes a kind in `until`.

    The overseer's loop: wait, then read `session.changes` from the cursor
    this returns. The engine does the waiting; this is a read, held open for
    at most `timeout_s`.
    """
    engine = _engine(ctx)
    ids = _engine_ids(ctx, params.ids)
    kinds = [one.strip() for one in params.until if one.strip()] or None
    try:
        waited = engine.wait_any(
            ids=ids,
            until=kinds,
            cursor=params.cursor,
            timeout_s=params.timeout_s,
        )
    except Conflict:
        waited = None
    if waited is None:
        msg = (
            "the session engine does not support wait-any yet (it has no "
            "GET /api/sessions/wait-any route); upgrade the engine, or poll "
            "session.sweep"
        )
        raise EngineUnavailable(msg)
    labels = (
        {} if waited.change is None else _session_labels(ctx, [waited.change.session])
    )
    return SessionWaitAnyResult(
        outcome=waited.outcome,
        change=None if waited.change is None else _change(waited.change, labels),
        next_cursor=waited.next_cursor,
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
    successor: str | None = None
    if params.superseded_by:
        successor = _target(ctx, params.superseded_by).engine_session_id
    if target.session is not None and target.session.stopped_at is not None:
        msg = f"session {params.id!r} was stopped; there is nothing to hibernate"
        raise Conflict(msg)
    # Imported here: `watches` reaches this module through `grants`.
    from vogt.application.services.watches import arm as arm_watch

    # The watch is armed before the session sleeps: a session hibernating
    # itself is gone once the engine acts, and must not sleep unwatched.
    watch = (
        None
        if params.wake_on is None
        else arm_watch(
            ctx,
            params.wake_on,
            target_session=engine_id,
            note="",
            ttl_seconds=params.wake_on_ttl_seconds,
            reason=reason,
        )
    )
    try:
        hibernated = engine.hibernate_session(
            engine_id,
            reason=reason,
            allow_shell=params.allow_shell,
            superseded_by=successor,
        )
    except Exception:
        if watch is not None:
            _disarm(ctx, watch.id, reason)
        raise
    if hibernated is None and watch is not None:
        _disarm(ctx, watch.id, reason)
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
        "superseded_by": successor,
        "watch": None if watch is None else watch.id,
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
        return _as_digest(SessionResult(session=_summarize_engine_only(hibernated)))
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

    return _as_digest(
        audited_write(ctx, operation=SESSION_HIBERNATE, reason=reason, body=body)
    )


def _disarm(ctx: AppContext, watch_id: str, reason: str) -> None:
    """Cancel the watch a failed hibernate armed."""
    from vogt.application.services.watches import cancel_watch

    cancel_watch(
        ctx, CancelWatchParams(id=watch_id, reason=f"hibernate failed: {reason}")
    )


def wake_session(ctx: AppContext, params: WakeSessionParams) -> SessionResult:
    """Start a hibernated session again: the same id, the same conversation.

    A live session is returned as it is — waking is idempotent, so a caller
    that is not sure need not check first. A session superseded by a successor,
    or whose work item another live session holds in the same directory, is
    refused unless `params.force`.
    """
    reason = writes.validate_reason(params.reason)
    engine = _engine(ctx)
    if params.force and not _may_force_wake(ctx, engine):
        raise RoleRefused("only a person or an oversight session may force a wake")
    target = _target(ctx, params.id)
    result = _wake(ctx, engine, target, reason, force=params.force)
    # Woken on purpose: whoever woke it is acting on it, so the report it
    # slept with no longer stands (WI-1834). A session that is still blocked
    # reports again.
    if _clear_block_on(ctx, engine, target, cause="wake", reason=reason):
        result = _wake(ctx, engine, target, reason, force=params.force)
    return result


def _named_session(ctx: AppContext, engine_session_id: str) -> str:
    """The ses_… id of the session linked to `engine_session_id`, or the uuid
    itself when none is. One declared read."""
    with ctx.declared.read() as view:
        row = view.session_by_engine_id(engine_session_id)
    return row.id if row is not None else engine_session_id


_SUCCESSOR = re.compile(
    r"(?P<head>session [0-9a-fA-F-]{36} was superseded by )(?P<uuid>[0-9a-fA-F-]{36})"
)


def _in_session_terms(ctx: AppContext, message: str) -> str:
    """Say a refused wake in ses_… terms when the successor is a linked session.
    An unlinked successor stays a uuid, and so does any other refusal."""
    found = _SUCCESSOR.search(message)
    if found is None:
        return message
    named = _named_session(ctx, found.group("uuid"))
    if named == found.group("uuid"):
        return message
    return message.replace(found.group("uuid"), named, 1)


def _wake(
    ctx: AppContext,
    engine: EngineClient,
    target: _Target,
    reason: str,
    *,
    force: bool = False,
) -> SessionResult:
    try:
        return _wake_inner(ctx, engine, target, reason, force=force)
    except Conflict as refused:
        raise Conflict(_in_session_terms(ctx, str(refused))) from None


def _wake_inner(
    ctx: AppContext,
    engine: EngineClient,
    target: _Target,
    reason: str,
    *,
    force: bool = False,
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
            return _as_digest(SessionResult(session=_summarize_engine_only(current)))
        with ctx.declared.read() as view:
            return _as_digest(
                SessionResult(session=_summarize(view, session, engine_session=current))
            )

    if session is None:
        woken = engine.wake_session(engine_id, force=force)
        if woken is None:
            msg = f"the engine has no session {engine_id!r}"
            raise NotFound(msg)
        audited_action(
            ctx,
            operation=SESSION_WAKE,
            reason=reason,
            entity_kind="session",
            entity_id=engine_id,
            outcome={
                "engine_session_id": engine_id,
                "linked": False,
                "forced": force,
            },
            event_kind=SESSION_WOKEN_EVENT,
        )
        return _as_digest(SessionResult(session=_summarize_engine_only(woken)))

    if session.stopped_at is not None:
        msg = f"session {session.id!r} was stopped; start a new one instead"
        raise Conflict(msg)
    scopes = _session_scopes(ctx)
    credential = issue(scopes)
    woken = engine.wake_session(
        engine_id, env=_session_env(ctx, session.id, credential.secret), force=force
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
                "forced": force,
            },
        )

    return _as_digest(
        audited_write(ctx, operation=SESSION_WAKE, reason=reason, body=body)
    )


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


def commit_for_session(
    ctx: AppContext, params: CommitForSessionParams
) -> CommitForSessionResult:
    """Commit a worker session's staged work (C14).

    The engine owns the checklist: the worker must be idle, hibernated or
    blocked, and the staged diff must carry no conflict marker, secret or
    intent-to-add. The commit uses the repo's own identity. The result is a
    digest and the SHA; the diff itself only when the caller asked for it.
    """
    reason = writes.validate_reason(params.reason)
    message = params.message.strip()
    if not message:
        msg = "message must not be empty: say what the commit does"
        raise InvalidRequest(msg)
    engine = _engine(ctx)
    target = _target(ctx, params.id)
    _require_commit_authority(ctx, engine, target.engine_session_id)
    forbidden_text = _forbidden_text_for(ctx, target)
    committed = engine.git_commit(
        session_id=target.engine_session_id,
        message=message,
        paths=params.paths,
        full_diff=params.full_diff,
        forbidden_text=forbidden_text,
        worktree=params.worktree,
    )
    sha = str(committed.get("commit") or "")
    if not sha:
        msg = f"the engine committed nothing for session {params.id!r}"
        raise InvalidRequest(msg)
    audited_action(
        ctx,
        operation=SESSION_COMMIT_FOR,
        reason=reason,
        entity_kind="session",
        entity_id=(
            target.session.id
            if target.session is not None
            else target.engine_session_id
        ),
        outcome={
            "engine_session_id": target.engine_session_id,
            "commit": sha,
            "digest": str(committed.get("digest") or ""),
            "paths": len(params.paths),
        },
        event_kind=SESSION_COMMITTED_EVENT,
    )
    return CommitForSessionResult(
        commit=sha,
        digest=str(committed.get("digest") or ""),
        diff_stat=str(committed.get("diff_stat") or ""),
        diff=committed.get("diff"),
        review=[str(flag) for flag in committed.get("review") or []],
    )


def rename_session(ctx: AppContext, params: RenameSessionParams) -> SessionResult:
    """Rename a session, live or hibernated — the GUI's rename, audited.

    The name is the engine's: the core keeps no copy, so a linked session and
    one the GUI started are renamed the same way and read back the same way.
    """
    reason = writes.validate_reason(params.reason)
    name = params.name.strip()
    if not name:
        msg = "name must not be empty: give the session a name to show"
        raise InvalidRequest(msg)
    engine = _engine(ctx)
    target = _target(ctx, params.id)
    engine_id = target.engine_session_id
    if not engine.rename_session(engine_id, name=name):
        msg = f"the engine has no session {params.id!r}"
        raise NotFound(msg)
    audited_action(
        ctx,
        operation=SESSION_RENAME,
        reason=reason,
        entity_kind="session",
        entity_id=target.session.id if target.session is not None else engine_id,
        outcome={
            "engine_session_id": engine_id,
            "linked": target.session is not None,
            "name": name,
        },
        event_kind=SESSION_RENAMED_EVENT,
    )
    renamed = engine.get_session(engine_id)
    if target.session is None:
        if renamed is None:
            msg = f"the engine has no session {params.id!r}"
            raise NotFound(msg)
        return SessionResult(session=_summarize_engine_only(renamed))
    with ctx.declared.read() as view:
        return SessionResult(
            session=_summarize(view, target.session, engine_session=renamed)
        )


def remove_session(ctx: AppContext, params: RemoveSessionParams) -> SessionResult:
    """Kill a session if it still runs and have the engine forget it — the
    GUI's Remove, audited.

    Where `session.stop` keeps the session listed so its output stays
    readable, this drops the engine's record, its kept screen and its brief.
    A linked session's own record is closed and its token revoked exactly as
    a stop closes them, unless a stop already did; Vogt's record and the
    audit trail stay. An engine that had already forgotten a linked session
    is not an error: the record is closed all the same.

    A live session is killed through the stop path first, carrying the
    reason and who asked, so its exit reads `stopped` with both rather than
    `errored` (WI-913) — the engine's DELETE kills without them.
    """
    reason = writes.validate_reason(params.reason)
    engine = _engine(ctx)
    target = _target(ctx, params.id)
    engine_id = target.engine_session_id
    before = engine.get_session(engine_id)
    if before is not None and before.alive:
        engine.kill_session(engine_id, reason=reason, by=ctx.principal.identity_ref)
    if target.session is None:
        if before is None or not engine.remove_session(engine_id):
            msg = f"no session {engine_id!r}"
            raise NotFound(msg)
        audited_action(
            ctx,
            operation=SESSION_REMOVE,
            reason=reason,
            entity_kind="session",
            entity_id=engine_id,
            outcome={"engine_session_id": engine_id, "linked": False},
            event_kind=SESSION_REMOVED_EVENT,
            summary={"engine_removed": True, "linked": False},
        )
        summary = _summarize_engine_only(before).model_copy(
            update={"alive": False, "stopped_at": ctx.clock()}
        )
        return SessionResult(session=summary)

    session = target.session
    removed = engine.remove_session(engine_id)

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[SessionResult]:
        del actor
        current = txn.session_by_id(session.id)
        if current is None:  # pragma: no cover - resolved just above
            msg = f"no session {params.id!r}"
            raise NotFound(msg)
        now = ctx.clock()
        closed = current.stopped_at is None
        if closed:
            txn.mark_session_stopped(current.id, at=now)
            for token in txn.tokens_for_actor(current.actor_id):
                txn.revoke_token(token.id, reason=reason, at=now)
        after = txn.session_by_id(current.id)
        assert after is not None  # read in this transaction
        return WriteOutcome(
            result=SessionResult(session=_summarize(txn, after, engine_session=None)),
            entity_kind="session",
            entity_id=after.id,
            payload=_audited_payload(after),
            event_kind=SESSION_REMOVED_EVENT,
            summary={"engine_removed": removed, "record_closed": closed},
        )

    return audited_write(ctx, operation=SESSION_REMOVE, reason=reason, body=body)


def set_session_role(ctx: AppContext, params: SetSessionRoleParams) -> SessionResult:
    """Nominate a session as oversight, or make it a worker again (WI-957).

    The role lives with the engine, beside `keep_awake`, because it has to
    survive exactly what the engine's record survives — hibernation and a
    redeploy — and because a session started from the GUI has no row here to
    hold it. The audit row is the core's durable answer to "which session was
    the overseer, and who said so".
    """
    reason = writes.validate_reason(params.reason)
    if ctx.principal.kind == "agent":
        # The role is a person's nomination. An overseer may ask for grants on
        # another session's behalf (WI-973), so an agent that could nominate
        # itself would hand itself that standing; and a worker demoting its
        # overseer is no better. Refused by who is asking, like `bypass`.
        msg = (
            "only a person nominates an oversight session, or makes one a worker "
            f"again ({ctx.principal.identity_ref} is an agent); ask a person to "
            "set the role from the GUI"
        )
        raise RoleRefused(msg)
    engine = _engine(ctx)
    target = _target(ctx, params.id)
    engine_id = target.engine_session_id
    updated = engine.set_role(engine_id, role=params.role)
    if updated is None:
        msg = f"the engine has no session {params.id!r}, or predates session roles"
        raise NotFound(msg)
    audited_action(
        ctx,
        operation=SESSION_SET_ROLE,
        reason=reason,
        entity_kind="session",
        entity_id=target.session.id if target.session is not None else engine_id,
        outcome={
            "engine_session_id": engine_id,
            "linked": target.session is not None,
            "role": params.role,
            "keep_awake": updated.keep_awake,
        },
        event_kind=SESSION_ROLE_EVENT,
    )
    if target.session is None:
        return SessionResult(session=_summarize_engine_only(updated))
    with ctx.declared.read() as view:
        return SessionResult(
            session=_summarize(view, target.session, engine_session=updated)
        )


def bind_session_work(
    ctx: AppContext, params: BindSessionWorkParams
) -> BindSessionWorkResult:
    """Declare which work item a session serves, or that it serves none
    (WI-998).

    The core's `coding_sessions.work_item_id` is the truth for a session Vogt
    started; this re-declares it. One current item per session: rebinding
    from WI-A to WI-B is one call, and the sequence is the audit log
    (`audit.list(entity_id=<ses_…>)`), not a second table. Any `work.write`
    principal may bind — a person, the session itself, its overseer: a
    binding grants nothing, it says what the session is doing.

    Binding never moves the terminal (its project and cwd stay), never moves
    the item to `in_progress`, and is allowed on a finished item (a
    verification session is legitimate). An item in another project is bound
    with `project_mismatch` set. A bind records the item's declared branch,
    as a start does; an unbind leaves it, forward-only, as a stop does.

    The engine carries a copy as its `work_item` label, so the rail and a
    session the GUI started (no core row — the label is all it has) show it.
    The core row is written first and wins: a label the engine could not take
    is reported in `engine_label`, not fatal.
    """
    reason = writes.validate_reason(params.reason)
    target = _target(ctx, params.id or _own_session(ctx))
    engine_id = target.engine_session_id
    wanted = (params.work_item or "").strip() or None
    with ctx.declared.read() as view:
        item = None if wanted is None else _resolve.work_item(view, wanted)
    ref = None if item is None else item.ref
    entity_id = target.session.id if target.session is not None else engine_id

    if target.session is None:
        # A session the GUI started: no row to declare on, and adopting one
        # would invent an actor for writes it never made (decision 5). The
        # engine's label is the binding.
        engine = _engine(ctx)
        current = engine.get_session(engine_id)
        if current is None:
            msg = f"no session {engine_id!r}"
            raise NotFound(msg)
        updated = engine.set_work_item(engine_id, work_item=ref)
        if updated is None:
            msg = (
                f"the engine cannot label session {engine_id!r} with a work "
                "item (it predates session work items)"
            )
            raise NotFound(msg)
        audited_action(
            ctx,
            operation=SESSION_BIND_WORK,
            reason=reason,
            entity_kind="session",
            entity_id=entity_id,
            outcome={
                "engine_session_id": engine_id,
                "linked": False,
                "work_item_id": None if item is None else item.id,
                "previous_work_item": current.work_item,
            },
            event_kind=SESSION_WORK_UNBOUND_EVENT
            if item is None
            else SESSION_WORK_BOUND_EVENT,
            summary={
                "engine_session_id": engine_id,
                "linked": False,
                "work_item": ref,
                "previous_work_item": current.work_item,
            },
        )
        with ctx.declared.read() as view:
            summary = _summarize_engine_only(updated, view)
        return BindSessionWorkResult(
            session=summary,
            previous_work_item=current.work_item,
            engine_label="written",
        )

    session = target.session

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[BindSessionWorkResult]:
        del actor
        current = txn.session_by_id(session.id)
        if current is None:  # pragma: no cover - resolved a moment ago
            msg = f"no session {session.id!r}"
            raise NotFound(msg)
        if current.stopped_at is not None:
            # A stopped session's binding is history: what it was on when it
            # ended. Rewriting it would rewrite that.
            msg = (
                f"session {session.id} was stopped; its work item is a "
                "record of what it was on, not something to re-declare"
            )
            raise Conflict(msg)
        previous = (
            None
            if current.work_item_id is None
            else txn.work_item_by_id(current.work_item_id)
        )
        txn.set_session_work_item(current.id, None if item is None else item.id)
        declared_branch = (
            None
            if item is None
            else _record_declared_branch(
                txn,
                work_ref=item.ref,
                project_id=item.project_id or current.project_id,
                at=ctx.clock(),
                template=ctx.config.branch_binding_template,
            )
        )
        updated = txn.session_by_id(current.id)
        assert updated is not None  # written in this transaction
        mismatch = item is not None and item.project_id != current.project_id
        previous_ref = None if previous is None else previous.ref
        return WriteOutcome(
            result=BindSessionWorkResult(
                session=_summarize(txn, updated, engine_session=None),
                previous_work_item=previous_ref,
                project_mismatch=mismatch,
                engine_label="unavailable",
            ),
            entity_kind="session",
            entity_id=updated.id,
            payload={
                **_audited_payload(updated),
                "previous_work_item_id": current.work_item_id,
            },
            event_kind=SESSION_WORK_UNBOUND_EVENT
            if item is None
            else SESSION_WORK_BOUND_EVENT,
            summary={
                "engine_session_id": updated.engine_session_id,
                "work_item": ref,
                "previous_work_item": previous_ref,
                "branch": declared_branch,
                "project_mismatch": mismatch,
            },
        )

    result = audited_write(ctx, operation=SESSION_BIND_WORK, reason=reason, body=body)

    # After the declared write, never inside it: the engine and SQLite cannot
    # share a transaction, and the row is the truth the label copies.
    live: EngineSession | None = None
    label: Literal["written", "not_found", "unavailable"] = "unavailable"
    detail: str | None = None
    if ctx.engine is None:
        detail = "no session engine is configured (VOGT_ENGINE_URL is unset)"
    else:
        try:
            live = ctx.engine.set_work_item(engine_id, work_item=ref)
            label = "written" if live is not None else "not_found"
            if live is None:
                detail = (
                    "the engine has no such session, or predates session work "
                    "items; the binding is recorded in Vogt"
                )
                live = ctx.engine.get_session(engine_id)
        except EngineUnavailable as exc:
            detail = str(exc)
    with ctx.declared.read() as view:
        bound = view.session_by_id(session.id)
        assert bound is not None
        summary = _summarize(
            view, bound, engine_session=live, engine_asked=label != "unavailable"
        )
    return result.model_copy(
        update={"session": summary, "engine_label": label, "engine": detail}
    )


SESSION_ANSWER = "session.answer"
SESSION_ANSWERED_EVENT = "session.answered"


def answer_session(ctx: AppContext, params: AnswerSessionParams) -> SessionAnswerResult:
    """Answer the dialog a session shows — a permission request or a
    startup gate (folder trust, external CLAUDE.md imports, reading outside
    the working directory) — by option, not by keystrokes.

    The engine reads the menu as it is at that moment and moves the highlight
    itself, so nothing here counts rows. Audited after the effect with the
    question, the kind, the option chosen and whether a person chose it.

    A permission prompt (not a startup gate) is a person's to answer: the
    engine refuses it for anyone else with `PersonRequired` (WI-983), and
    this decides which the caller is (`_answers_as_person`). The exception
    is an oversight session, which may answer a worker's permission prompt;
    the engine checks its role and audits it with the command (WI-2028).
    """
    reason = writes.validate_reason(params.reason)
    if (params.option is None) == (params.label is None or not params.label.strip()):
        msg = "give exactly one of option (a number) or label"
        raise InvalidRequest(msg)
    engine = _engine(ctx)
    target = _target(ctx, params.id)
    engine_id = target.engine_session_id
    # A person answers anything. An oversight session is relayed by its
    # engine id, not as a person: the engine lets it answer a permission
    # prompt and refuses other kinds with `PersonRequired` (WI-2028). The
    # audit line records which answered.
    by_person = _answers_as_person(ctx)
    by_oversight = not by_person and _is_oversight_caller(ctx, engine)
    answered = engine.answer_session(
        engine_id,
        option=params.option,
        label=None if params.label is None else params.label.strip(),
        expect_question=params.expect_question,
        person=by_person,
        overseer=_caller_engine_session_id(ctx) if by_oversight else None,
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
            "person": by_person,
            "oversight": by_oversight,
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


SESSION_TOKEN = "session.token"
#: The actor of a session the engine started itself (from the GUI, a
#: template, an agent task), as opposed to `agent:session:<ses_…>` for one
#: vogt-core started.
ENGINE_SESSION_ACTOR_PREFIX = "agent:engine:"


def _may_force_wake(ctx: AppContext, engine: EngineClient) -> bool:
    """`force` overrides the handoff guard, so only a person, or a session the
    engine has marked `oversight`, may use it. Any other agent is refused."""
    if _answers_as_person(ctx):
        return True
    return _is_oversight_caller(ctx, engine)


def _is_oversight_caller(ctx: AppContext, engine: EngineClient) -> bool:
    """Whether the caller is a session the engine has marked `oversight`."""
    caller_id = _caller_engine_session_id(ctx)
    if caller_id is None:
        return False
    caller = engine.get_session(caller_id)
    return caller is not None and caller.role == "oversight"


def _answers_as_person(ctx: AppContext) -> bool:
    """Whether this caller answers a permission prompt as a person (WI-983).

    The rule `session.grant_decide` applies (WI-973): an agent principal —
    a session's token, an `agent:engine:` token, the pod token — never
    does. Nor does the engine's own credential, whatever actor it is bound
    to: on a deployment whose stack secret was first issued to a person, its
    principal is that person, but what presents it is the engine, and any
    process that can read it.
    """
    return ctx.principal.kind == "human" and not _is_engine_credential(ctx)


def _forbidden_text_for(ctx: AppContext, target: _Target) -> list[str]:
    """The target session's project policy patterns (C13), read here, never
    taken from the caller. A session Vogt never linked has no project, so
    nothing is forbidden for it beyond what the engine checks anyway."""
    if target.session is None:
        return []
    with ctx.declared.read() as view:
        project = view.project_by_id(target.session.project_id)
    return [] if project is None else list(project.agent_policy.forbidden_text)


def _caller_engine_session_id(ctx: AppContext) -> str | None:
    """The engine session id this caller is, when the caller is a session.

    A session Vogt started is `agent:session:<ses_…>`, resolved through its
    row. One the engine started is `agent:engine:<uuid>`, and that uuid is
    the engine's id. Anyone else is not a session.
    """
    ref = ctx.principal.identity_ref
    if ref.startswith("agent:session:"):
        own = _own_session(ctx)
        with ctx.declared.read() as view:
            session = view.session_by_id(own)
        if session is None:
            return None
        return session.engine_session_id
    if ref.startswith(ENGINE_SESSION_ACTOR_PREFIX):
        return ref.removeprefix(ENGINE_SESSION_ACTOR_PREFIX)
    return None


def _require_commit_authority(
    ctx: AppContext, engine: EngineClient, target_engine_id: str
) -> None:
    """Who may commit for a session (C14).

    A person always may. A session may commit only for another session, and
    only when its own engine role is `oversight`: a worker committing its own
    tree, or another worker's, is the thing this refuses.
    """
    if _answers_as_person(ctx):
        return
    caller_id = _caller_engine_session_id(ctx)
    if caller_id is None:
        msg = (
            "only a person, or an oversight session, commits for a session "
            f"({ctx.principal.identity_ref} is neither)"
        )
        raise RoleRefused(msg)
    if caller_id == target_engine_id:
        msg = (
            "a session does not commit its own work; an oversight session, or "
            f"a person, commits for it ({caller_id})"
        )
        raise RoleRefused(msg)
    caller = engine.get_session(caller_id)
    if caller is None or caller.role != "oversight":
        role = "unknown" if caller is None else caller.role
        msg = (
            "only an oversight session commits for another session "
            f"({caller_id} is {role})"
        )
        raise RoleRefused(msg)


def _is_engine_credential(ctx: AppContext) -> bool:
    """Whether the caller is the session engine: the actor its credential is
    bound to by configuration, or the very token it shares with this core
    (the stack secret, `bootstrap_core_token_file`).

    The second matters because a stack secret first issued by hand keeps
    whatever actor it was issued to: adoption is idempotent on the secret's
    hash and does not rebind it. On the estate that actor is a person, so a
    check on the actor alone refused the engine (found validating WI-926 on
    vogt-dev).
    """
    if ctx.principal.identity_ref == ctx.config.bootstrap_core_token_actor:
        return True
    configured = ctx.config.bootstrap_core_token_file
    if ctx.token is None or configured is None:
        return False
    try:
        secret = Path(configured).read_text(encoding="utf-8").strip()
    except OSError:
        return False
    if not secret:
        return False
    with ctx.declared.read() as view:
        row = view.token_by_hash(hash_token(secret))
    return row is not None and row.revoked_at is None and row.id == ctx.token.id


def engine_session_token(
    ctx: AppContext, params: EngineSessionTokenParams
) -> EngineSessionTokenResult:
    """Mint (or revoke) the credential of an agent session the engine started.

    WI-926: a session vogt-core starts runs with its own agent token, so the
    core knows an agent from a person. One the engine started (the GUI, a
    protected template) ran with the pod's brokered token, bound to a person,
    so its agent counted as that person: it could grant `bypass`, and its
    writes were attributed to them. The engine now asks for a token like
    this one for every agent it launches.

    Only the engine's own credential may ask, and the token carries the same
    scopes a session `session.start` mints does, so this grants nothing
    `session.start` does not already grant any `work.write` caller.
    """
    reason = writes.validate_reason(params.reason)
    if not _is_engine_credential(ctx):
        msg = (
            "session.token is called by the session engine for the agents it "
            f"starts, not by {ctx.principal.identity_ref}"
        )
        raise EngineOnly(msg)
    identity_ref = f"{ENGINE_SESSION_ACTOR_PREFIX}{params.engine_session_id}"
    scopes = _session_scopes(ctx)
    credential = None if params.revoke else issue(scopes)

    def body(txn: WriteTxn, actor: Actor) -> WriteOutcome[EngineSessionTokenResult]:
        del actor
        now = ctx.clock()
        holder = txn.actor_by_identity(identity_ref)
        if holder is None:
            holder = Actor(
                id=ctx.id_factory("act"),
                identity_ref=identity_ref,
                kind="agent",
                display_name=f"Engine session {params.engine_session_id[:8]}",
                created_at=now,
            )
            txn.insert_actor(holder)
        why = "session ended" if params.revoke else f"superseded: {reason}"
        revoked = [
            token.id
            for token in txn.tokens_for_actor(holder.id)
            if txn.revoke_token(token.id, reason=why, at=now)
        ]
        if credential is not None:
            txn.insert_token(
                Token(
                    id=ctx.id_factory("tok"),
                    actor_id=holder.id,
                    actor_identity_ref=identity_ref,
                    name=f"engine session {params.engine_session_id}",
                    scopes=list(scopes),
                    created_at=now,
                    expires_at=None,
                ),
                token_hash=credential.token_hash,
            )
        return WriteOutcome(
            result=EngineSessionTokenResult(
                engine_session_id=params.engine_session_id,
                actor=identity_ref,
                token=None if credential is None else credential.secret,
                revoked=len(revoked),
            ),
            entity_kind="actor",
            entity_id=holder.id,
            payload={"identity_ref": identity_ref, "kind": "agent"},
            event_kind=(
                "session.token_revoked"
                if credential is None
                else "session.token_issued"
            ),
            # Never the secret: the audit row says what happened, not with what.
            summary={
                "engine_session_id": params.engine_session_id,
                "minted": credential is not None,
                "revoked": len(revoked),
            },
        )

    return audited_write(ctx, operation=SESSION_TOKEN, reason=reason, body=body)


SESSION_REPORT_BLOCKED = "session.report_blocked"
SESSION_REPORT_UNBLOCKED = "session.report_unblocked"
SESSION_BLOCKED_EVENT = "session.blocked"
SESSION_UNBLOCKED_EVENT = "session.unblocked"

# One past the largest page `session.list` will return. Fetching exactly the
# ceiling hides a match beyond it: the response would report the capped count
# as complete. The extra row is dropped before the join; it only marks the
# overflow so the caller can keep `truncated` true.
_DECLARED_FETCH = 501


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


def _clear_block_on(
    ctx: AppContext,
    engine: EngineClient,
    target: _Target,
    *,
    cause: str,
    reason: str,
    current: EngineSession | None = None,
) -> EngineSession | None:
    """Clear `target`'s blocked report because someone acted on the session
    (WI-1834): it was given new input, or woken. The report it replaces stays
    in the audit, as an unblock naming the cause and what it said.

    A session typing into or waking itself clears nothing, and a report that
    cannot be read or cleared is left for the sweep's staleness rule: this
    never fails the input or wake it rides on. `current` is a read of the
    session the caller already holds, to spare a second one. The session as
    cleared when one was, else None.
    """
    if ctx.principal.identity_ref in {
        f"agent:session:{target.engine_session_id}",
        *(() if target.session is None else (f"agent:session:{target.session.id}",)),
    }:
        return None
    engine_id = target.engine_session_id
    try:
        if current is None:
            current = engine.get_session(engine_id)
        if current is None or current.blocked is None:
            return None
        was = current.blocked
        cleared = engine.set_blocked(engine_id, blocked=False, reason=None, items=[])
        if cleared is None:
            return None
    except (EngineUnavailable, Conflict, NotFound):
        return None
    audited_action(
        ctx,
        operation=SESSION_REPORT_UNBLOCKED,
        reason=f"cleared by {cause}: {reason}",
        entity_kind="session",
        entity_id=target.session.id if target.session is not None else engine_id,
        outcome={
            "engine_session_id": engine_id,
            "linked": target.session is not None,
            "blocker": None,
            "items": [],
            "cleared_by": cause,
            "was_blocker": was.reason,
            "was_items": list(was.items),
            "was_since": was.since,
        },
        event_kind=SESSION_UNBLOCKED_EVENT,
    )
    return cleared


def _input_digest(
    ctx: AppContext, target: _Target, *, submitted: bool, reason: str
) -> SessionDigest | None:
    """The digest of the session just typed into, when it can be read.

    A submitted input also clears the session's blocked report (WI-1834),
    from the same read: a new instruction answers what it was blocked on, or
    supersedes it. A draft is not sent yet.
    """
    engine = _engine(ctx)
    try:
        live = engine.get_session(target.engine_session_id)
    except (EngineUnavailable, NotFound):
        return None
    if submitted and live is not None:
        live = (
            _clear_block_on(
                ctx, engine, target, cause="input", reason=reason, current=live
            )
            or live
        )
    if target.session is None:
        if live is None:
            return None
        return _digest_of(_summarize_engine_only(live))
    with ctx.declared.read() as view:
        return _digest_of(_summarize(view, target.session, engine_session=live))


def _wait_digest(
    ctx: AppContext, params_id: str, engine_id: str
) -> SessionDigest | None:
    try:
        live = _engine(ctx).get_session(engine_id)
    except (EngineUnavailable, NotFound):
        return None
    if live is None:
        return None
    target = _target(ctx, params_id)
    if target.session is None:
        return _digest_of(_summarize_engine_only(live))
    with ctx.declared.read() as view:
        return _digest_of(_summarize(view, target.session, engine_session=live))


def _tail_of(screen: SessionScreenResult, lines: int = 8) -> list[str]:
    kept = [line for line in screen.lines if line.strip()]
    return kept[-lines:]


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
        brief: ComposedBrief,
        policy: AgentPolicy,
        is_scratch: bool = False,
    ) -> None:
        self.project_id = project_id
        self.project_slug = project_slug
        self.cwd = cwd
        self.work_item = work_item
        self.brief = brief
        self.policy = policy
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


def _git_probe(root: Path, flag: str) -> str:
    """One `git rev-parse` question, with nothing the repo can run.

    `rev-parse` runs no hooks. `core.fsmonitor` and `core.hooksPath` are
    overridden so a repo's monitor and hooks are not consulted, and
    `GIT_CONFIG_NOSYSTEM` drops the system gitconfig. Inherited `GIT_DIR`,
    `GIT_WORK_TREE` and `GIT_COMMON_DIR` are removed so the engine's own
    environment cannot point the probe at a different repository.
    """

    env = {
        key: value
        for key, value in os.environ.items()
        if key not in {"GIT_DIR", "GIT_WORK_TREE", "GIT_COMMON_DIR", "GIT_CONFIG_COUNT"}
    }
    env["GIT_CONFIG_NOSYSTEM"] = "1"
    try:
        completed = subprocess.run(  # fixed argv, never a shell
            [
                "git",
                "-C",
                str(root),
                "-c",
                "core.fsmonitor=",
                "-c",
                "core.hooksPath=/dev/null",
                "rev-parse",
                flag,
            ],
            capture_output=True,
            text=True,
            timeout=20,
            check=False,
            env=env,
        )
    except (OSError, subprocess.SubprocessError) as exc:
        msg = f"git could not be run against {root}: {exc}"
        raise CollectorError(msg) from exc
    if completed.returncode != 0:
        detail = completed.stderr.strip() or f"exit {completed.returncode}"
        msg = f"git rev-parse could not be read in {root}: {detail}"
        raise CollectorError(msg)
    return completed.stdout.strip()


def _existing_worktree_cwd(raw: str, project_root: str) -> str:
    """The directory a `cwd` start opens in, or a refusal (WI-1671).

    Four checks, in the order a wrong path fails them: it exists, it sits
    under the workspace's real `Active/` directory (the parent of the
    project's registered checkout, not any path component named `Active`),
    it is not inside that checkout (its git toplevel differs, so a linked
    worktree nested under the checkout still passes), and it is a git
    worktree of the same repository. The comparison is git's `commondir`,
    which a linked worktree and the main checkout of one repository share
    and two repositories do not.
    """
    path = Path(raw).expanduser()
    if not path.is_absolute():
        msg = f"cwd {raw!r} must be an absolute path"
        raise InvalidRequest(msg)
    if not path.is_dir():
        msg = f"cwd {raw!r} does not exist"
        raise InvalidRequest(msg)
    try:
        resolved = path.resolve(strict=True)
    except OSError as exc:
        msg = f"cwd {raw!r} cannot be resolved: {exc}"
        raise InvalidRequest(msg) from exc
    checkout = Path(project_root)
    if not checkout.is_dir():
        msg = (
            f"the project's checkout {project_root!r} does not exist, so "
            "cwd cannot be checked against it"
        )
        raise InvalidRequest(msg)
    checkout_resolved = checkout.resolve()
    active = checkout_resolved.parent
    if active.name != "Active" or not resolved.is_relative_to(active):
        msg = f"cwd {resolved} is not under the workspace Active/ directory ({active})"
        raise InvalidRequest(msg)
    try:
        asked = _git_probe(resolved, "--git-common-dir")
        owned = _git_probe(checkout_resolved, "--git-common-dir")
        # A subdirectory of the shared checkout shares its commondir and is
        # under Active/, so the path comparison above misses it. Its
        # toplevel is the checkout, which a linked worktree's is not.
        toplevel = _git_probe(resolved, "--show-toplevel")
    except CollectorError as exc:
        msg = (
            f"cwd {resolved} is not a git worktree of the project's "
            f"repository ({project_root}): {exc}"
        )
        raise InvalidRequest(msg) from exc
    asked_dir = Path(asked)
    owned_dir = Path(owned)
    if not asked_dir.is_absolute():
        asked_dir = resolved / asked_dir
    if not owned_dir.is_absolute():
        owned_dir = checkout / owned_dir
    try:
        same = asked_dir.resolve() == owned_dir.resolve()
    except OSError as exc:
        msg = f"cwd {raw!r} is not a git worktree of the project's repository: {exc}"
        raise InvalidRequest(msg) from exc
    if not same:
        msg = (
            f"cwd {resolved} is not a git worktree of the project's "
            f"repository ({project_root})"
        )
        raise InvalidRequest(msg)
    try:
        top = Path(toplevel).resolve()
    except OSError as exc:
        msg = f"cwd {resolved} is not a git worktree: {exc}"
        raise InvalidRequest(msg) from exc
    if top == checkout_resolved:
        msg = (
            f"cwd {resolved} is inside the project's shared checkout; name a "
            "worktree of it instead"
        )
        raise InvalidRequest(msg)
    return str(resolved)


def _worktree_branch(subject: _Subject, session_id: str) -> str:
    """The branch a session's own worktree is cut onto (WI-1298).

    `<work item>-<session>` when the session serves an item, otherwise
    `<project>-<session>`. The session id's tail keeps two sessions on the
    same item apart, and a ref or slug that is not itself a legal branch
    name is flattened so git is handed one path component.
    """
    head = subject.work_item_ref or subject.project_slug
    flat = re.sub(r"[^A-Za-z0-9._-]+", "-", head).strip("-") or "session"
    tail = session_id.removeprefix("ses_")
    return f"{flat}-{tail}"


def _worktree_base(policy: AgentPolicy) -> str:
    """What the worktree's branch is cut from: the project's integration
    branch when it states one, otherwise `main`."""
    stated = policy.base_branch.strip()
    return stated or "main"


def _review_subject(ctx: AppContext, params: StartSessionParams) -> StartSessionParams:
    """Fold `review_of` into the subject, or refuse a review of nothing.

    A review is of one work item. Naming it is enough — the same sugar as
    `from_handoff` — and naming a second, different item is a contradiction,
    refused before a terminal exists. Whether the item has a hand-off is the
    brief's to say: a review of an item that names no diff still starts, and
    reports that there is nothing to diff.
    """
    if params.review_of is None:
        return params
    if params.work_item is not None and params.work_item != params.review_of:
        msg = (
            f"review_of names {params.review_of}, but work_item names "
            f"{params.work_item}"
        )
        raise InvalidRequest(msg)
    if params.from_handoff is not None and params.from_handoff != params.review_of:
        msg = (
            f"review_of names {params.review_of}, but from_handoff names "
            f"{params.from_handoff}"
        )
        raise InvalidRequest(msg)
    with ctx.declared.read() as view:
        item = _resolve.work_item(view, params.review_of)
    return params.model_copy(update={"work_item": item.ref, "review_of": item.ref})


def _subject(ctx: AppContext, params: StartSessionParams, session_id: str) -> _Subject:
    params = _review_subject(ctx, params)
    if params.from_handoff is not None:
        # Sugar for "fresh session on the item the hand-off belongs to".
        # The brief carries the hand-off because the brief always does,
        # once the item is the subject.
        if params.work_item is not None and params.work_item != params.from_handoff:
            msg = (
                "from_handoff names "
                f"{params.from_handoff}, but work_item names {params.work_item}"
            )
            raise InvalidRequest(msg)
        with ctx.declared.read() as view:
            item = _resolve.work_item(view, params.from_handoff)
            handoffs = [
                one
                for one in view.comments_for(item.id, limit=200, order="newest")
                if one.kind == "handoff"
            ]
        if not handoffs:
            msg = f"{item.ref} has no hand-off comment to start a session from"
            raise InvalidRequest(msg)
        params = params.model_copy(update={"work_item": item.ref})
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
        project = _with_item_base(view, project, item, params.task)
        return _Subject(
            project_id=project.id,
            project_slug=project.slug,
            cwd=project.root_path,
            work_item=item,
            brief=(
                review_brief_for_work_item(view, item, session_id, project=project)
                if params.review_of is not None
                else brief_for_work_item(
                    view, item, session_id, _ranking(ctx, item.ref), project=project
                )
            ),
            policy=project.agent_policy,
        )

    project = _resolve.project(view, params.project or "")
    project = _with_item_base(view, project, None, params.task)
    return _Subject(
        project_id=project.id,
        project_slug=project.slug,
        cwd=project.root_path,
        work_item=None,
        brief=brief_for_project(view, project.slug, session_id, project=project),
        policy=project.agent_policy,
    )


def _with_item_base(
    view: ReadView, project: Project, item: WorkItem | None, task: str | None
) -> Project:
    """The project as this start sees it: its policy on the item's base.

    A project's landing rule names one base, but a long-lived integration
    line — the Rust port on `rust-core` — has its own (WI-1841). The
    initiative's base applies to its items, and a task that names a known
    long-lived branch as its base is taken at its word. The brief, the
    worktree and the conflict check all read the result, so they agree.
    """
    policy = project.agent_policy
    landing = policy.landing
    if landing is None:
        return project
    base = landing.base_branch
    if item is not None and item.initiative_id is not None:
        keys = [item.initiative_id]
        initiative = view.initiative_by_id(item.initiative_id)
        if initiative is not None:
            keys.append(initiative.slug)
        base = next(
            (
                landing.initiative_bases[key]
                for key in keys
                if key in landing.initiative_bases
            ),
            base,
        )
    base = task_named_base(task or "", landing) or base
    resolved = policy.with_base(base)
    if resolved is policy:
        return project
    return project.model_copy(update={"agent_policy": resolved})


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


def _caller_merges(
    policy: AgentPolicy,
    params: StartSessionParams,
    started_by_kind: str | None,
) -> bool:
    """Whether this start is oversight's own landing work (WI-1841).

    On a project where oversight merges, an oversight session that starts a
    mechanical session to land a branch is doing what the policy says; its
    "merge" is not overridden to "do not merge".
    """
    landing = policy.landing
    return (
        landing is not None
        and landing.merge_by == "oversight"
        and started_by_kind == "oversight"
        and params.purpose == "mechanical"
    )


def _effective_policy(
    policy: AgentPolicy, task: str | None, *, caller_merges: bool = False
) -> EffectivePolicy | None:
    """The landing rule a started worker runs under, and what it overrode.

    Returned on `session.start` so the caller sees, before the worker runs,
    that its task said one thing and the policy another (WI-1832).
    """
    landing = policy.landing
    if landing is None:
        return None
    return EffectivePolicy(
        base_branch=landing.base_branch,
        lands_by=landing.worker_lands_by,
        merge_by=landing.merge_by,
        merge_mode=landing.merge_mode,
        deploy_by=landing.deploy_by,
        overrides=task_policy_conflicts(
            task or "", landing, caller_merges=caller_merges
        ),
    )


def _brief_with_task(
    brief: ComposedBrief,
    params: StartSessionParams,
    *,
    cwd: str,
    autopilot: bool = False,
    read_only: bool = False,
    landing: LandingPolicy | None = None,
    caller_merges: bool = False,
) -> str:
    """Fold an explicit task, and its touch points, into the session's brief.

    The brief is context — the project, or the work item and why it ranks —
    and by itself it asks the agent to do nothing (a project brief says so
    in as many words). A spoken "start a session on komodo and check the
    containers" carries the *doing* part separately; without it the agent
    opens and waits, and the person has to type what they just said.
    Appended rather than replacing so the agent keeps the context under
    the task.
    """
    text = brief.text
    if autopilot:
        # Said in the brief, not enforced: the engine cannot make an agent
        # keep going, but an agent that was told to will (WI-878).
        text = f"{text.rstrip()}\n\n{AUTOPILOT}"
    if read_only:
        text = f"{text.rstrip()}\n\n{READ_ONLY}"
    task = (params.task or "").strip()
    touch = _touch_points(params, cwd=cwd)
    if params.review_of is not None:
        # The agent's launch prompt looks for a Task section and otherwise
        # waits, so a reviewer gets one even when the caller named no task
        # (WI-1854). The caller's own task stays under "Also asked".
        text = f"{text.rstrip()}\n\n## Task\n\n{_REVIEW_TASK}\n"
    elif not task:
        task = _default_task(params)
    if task:
        # A reviewer already has its task — check the diff — and a caller's
        # task would read as new work to go and do. It is recorded under the
        # review instead, as what the review was asked to look at.
        heading = "## Also asked" if params.review_of is not None else "## Task"
        # Where the task contradicts the landing rule the policy wins, and
        # the brief says so above the task rather than leaving the worker
        # two instructions to choose between (WI-1832).
        overrides = overrides_section(
            task_policy_conflicts(task, landing, caller_merges=caller_merges)
        )
        if overrides:
            text = f"{text.rstrip()}\n\n{overrides}"
        text = f"{text.rstrip()}\n\n{heading}\n\n{task}\n"
    if params.purpose == "implement":
        # Every agent template, Claude and Codex alike: a worker that polls
        # CI or picks up a second item re-reads its whole context each turn
        # (WI-2035).
        text = f"{text.rstrip()}\n\n{IMPLEMENT_FOOTER}\n"
    if touch:
        text = f"{text.rstrip()}\n\n{touch}"
    return text


#: The Task section a review start gets (WI-1854): what the review brief
#: above it already asks, said under the heading the launch prompt looks for.
_REVIEW_TASK = (
    "Review the branch this item's latest hand-off names, as the Review "
    "section above says, and post the verdict as one `work_comment`: "
    "`no blockers (head <sha>)` or `blockers (head <sha>):` with the list."
)

#: How a reason says it is the task (WI-1854): `Task: do the thing`.
_TASK_PREFIX = re.compile(r"^\s*task\s*:\s*", re.IGNORECASE)


#: Said at the foot of every implement brief (WI-2035).
IMPLEMENT_FOOTER = (
    "Stop when your branch is pushed: do not wait for or poll CI, "
    "oversight lands it. One work item per session."
)


def _default_task(params: StartSessionParams) -> str:
    """The task a start implies when it names none (WI-1854).

    An agent is launched to "carry out the Task section, or summarise and
    wait", so a start that leaves `task` out opens an agent that idles.
    Callers often put the task in `reason` instead; an agent start takes
    it from there. A purpose of `review` gets the review task. A plain
    shell gets nothing: it has no agent to read a task.
    """
    if params.purpose == "review":
        target = params.work_item or "the bound work item"
        return (
            f"Review the pull request or branch bound to {target} and "
            "report blockers: post one `work_comment` whose first line is "
            "`no blockers (head <sha>)` or `blockers (head <sha>):` followed "
            "by a numbered list. Make no edits, commits or pushes."
        )
    if params.template is None:
        return ""
    return _TASK_PREFIX.sub("", params.reason or "", count=1).strip()


def _touch_points(params: StartSessionParams, *, cwd: str) -> str:
    """The structured half of a task: files, symbols, done, gates, map.

    Empty when the caller named none of them, so a plain `task` brief is
    unchanged. The map is a two-level outline of the directories the files
    sit in, drawn from the working tree and capped — a map, not a listing.
    """
    rows: list[str] = []
    if params.files:
        rows.append("**Files**")
        rows.extend(f"- `{one}`" for one in params.files)
    if params.symbols:
        rows.append("**Symbols** " + ", ".join(f"`{one}`" for one in params.symbols))
    if params.done:
        rows.append("**Done when**")
        rows.extend(f"- {one}" for one in params.done)
    if params.gates:
        rows.append("**Gates**")
        rows.extend(f"- {one}" for one in params.gates)
    outline = _repo_map(params, cwd=cwd)
    if outline:
        rows += ["**Repo map**", "```", outline, "```"]
    if not rows:
        return ""
    return "## Touch points\n\n" + "\n".join(rows) + "\n"


_REPO_MAP_CAP = 2000


def _repo_map(params: StartSessionParams, *, cwd: str) -> str:
    """A two-level outline of the touch-point directories, or nothing.

    Drawn only when the caller asked (`repo_map`) or named files. Each file's
    directory is listed one level down, and only when it sits inside `cwd`:
    a path that escapes the working tree is a hint that was wrong, not a
    directory to go and read. A missing tree is an empty map, never a failed
    start. Capped, because a map that outgrows the brief has stopped being one.
    """
    wanted = params.repo_map
    if wanted is False or (wanted is None and not params.files):
        return ""
    root = Path(cwd).resolve()
    if not root.is_dir():
        return ""
    dirs = {_touch_dir(root, spec) for spec in params.files}
    dirs.discard(None)
    lines: list[str] = []
    for directory in sorted(dirs, key=lambda one: str(one)):
        assert directory is not None
        rel = directory.relative_to(root)
        lines.append(f"{rel}/" if str(rel) != "." else "./")
        try:
            children = sorted(directory.iterdir(), key=lambda one: one.name)
        except OSError:
            continue
        for child in children:
            if child.name.startswith("."):
                continue
            mark = "/" if child.is_dir() else ""
            lines.append(f"  {child.name}{mark}")
            if sum(len(line) + 1 for line in lines) > _REPO_MAP_CAP:
                lines.append("  …")
                return "\n".join(lines)
    return "\n".join(lines)


def _touch_dir(root: Path, spec: str) -> Path | None:
    """The directory a touch-point file sits in, if it is inside `root`."""
    raw = spec.split(":", 1)[0].strip()
    if not raw:
        return None
    candidate = Path(raw)
    if not candidate.is_absolute():
        candidate = root / candidate
    try:
        resolved = candidate.resolve()
    except OSError:
        return None
    if resolved.is_file():
        resolved = resolved.parent
    try:
        resolved.relative_to(root)
    except ValueError:
        return None
    return resolved if resolved.is_dir() else None


def _enforced_policy(policy: AgentPolicy | None) -> dict[str, Any] | None:
    """The slice of a project's agent policy the engine can refuse on.

    Guidance (`commit_identity`, `test_recipe`, `rules`) stays in the brief
    and never reaches the session. With nothing to refuse — no forbidden
    text and merges that do not deploy — the spec omits `policy` entirely,
    so a start is byte-identical to one from before enforcement existed.
    """
    if policy is None:
        return None
    forbidden = [str(pattern) for pattern in policy.forbidden_text]
    merges = policy.merge_deploys_prod
    if not forbidden and not merges:
        return None
    branches = [str(branch) for branch in policy.main_branches]
    enforced: dict[str, Any] = {
        "forbidden_text": forbidden,
        "merge_deploys_prod": merges,
    }
    if branches:
        enforced["main_branches"] = branches
    return enforced


def _budget(
    params: StartSessionParams, policy: AgentPolicy | None = None
) -> SessionBudgetParams | None:
    """The limits a start carries (C7, WI-1245).

    Asked explicitly, that object is the answer — even an empty one, which is
    how a caller declines the default rather than inheriting it. Asked nothing,
    an agent template given a task in the worker role gets the default (120
    turns, 90 minutes of wall clock per turn, no context limit), plus the
    project's whole-session wall clock when its `agent_policy` sets one
    (WI-1281; unset by default). An oversight session and a shell or no-task
    start get none: there is nothing to bound.
    """
    if params.budget is not None:
        return params.budget
    if (
        params.template
        and params.task
        and params.task.strip()
        and params.role == "worker"
    ):
        return SessionBudgetParams(
            turns=120,
            wall_clock_minutes=90,
            session_wall_clock_minutes=(
                policy.session_wall_clock_minutes if policy else None
            ),
        )
    return None


def _budget_wire(budget: SessionBudgetParams | None) -> dict[str, int] | None:
    """The engine's `budget` object, or None when the start sets no budget.

    An empty object is sent as `{}`: it means "no limits", which the engine
    reads differently from a field that was never sent.
    """
    if budget is None:
        return None
    return {
        key: value
        for key, value in (
            ("turns", budget.turns),
            ("context_tokens", budget.context_tokens),
            ("wall_clock_minutes", budget.wall_clock_minutes),
            ("session_wall_clock_minutes", budget.session_wall_clock_minutes),
        )
        if value is not None
    }


def _budget_params(raw: dict[str, Any] | None) -> SessionBudgetParams | None:
    """The engine's budget object as the summary carries it.

    Only positive limits survive: zero is not a limit, and a row that yields
    nothing is no budget rather than an empty object.
    """
    if not isinstance(raw, dict):
        return None
    limits = {
        key: value
        for key in (
            "turns",
            "context_tokens",
            "wall_clock_minutes",
            "session_wall_clock_minutes",
        )
        if isinstance(value := raw.get(key), int) and value > 0
    }
    return SessionBudgetParams(**limits) if limits else None


#: The start fields a dispatch profile fills, in the order `overrides` lists
#: them. No `role` and no `permission_mode`: those are never defaults.
_PROFILE_FIELDS = (
    "template",
    "model",
    "effort",
    "budget",
    "worktree",
    "repo_map",
    "autopilot",
)

#: The review profile's hint (WI-1323). Advisory, like autopilot's text.
READ_ONLY = (
    "## Read only\n\n"
    "This session was started to review. Read, run the checks and report; do "
    "not edit, commit or push."
)


def _resolve_purpose(
    ctx: AppContext, params: StartSessionParams
) -> tuple[StartSessionParams, tuple[str, ...], bool]:
    """Fill a start's unset fields from its purpose's dispatch profile.

    Returns the filled params, the profile fields the caller set explicitly
    (the overrides), and whether the profile asks for a read-only session.
    The profile is the deployment's (`dispatch.profiles`, written by a
    person, over the code defaults), never the caller's own preference. A
    start with no purpose is returned unchanged.
    """
    if params.purpose is None:
        return params, (), False
    with ctx.declared.read() as view:
        profile = dispatch_settings(view).profiles[params.purpose]
    given = params.model_fields_set
    # Sized by the model the start will run, the caller's or the profile's.
    turns = profile.turns_for(params.model if "model" in given else profile.model)
    supplied: dict[str, object] = {
        "template": profile.template,
        "model": profile.model,
        "effort": profile.effort,
        "budget": SessionBudgetParams(
            turns=turns,
            wall_clock_minutes=profile.budget_wall_clock_minutes,
        )
        if turns or profile.budget_wall_clock_minutes
        else None,
        "worktree": profile.worktree,
        "repo_map": profile.repo_map,
        "autopilot": profile.autopilot,
    }
    if (
        "model" in given
        and "template" not in given
        and params.model is not None
        and _CLAUDE_MODEL.match(params.model)
        and _resolved_template_tag(profile.template) != "claude"
    ):
        # A Claude model on the profile's other agent never runs: klaudia
        # answers `opus` with 400 "Invalid model name" on every turn
        # (WI-1852). The caller named the model, so the agent follows it.
        supplied["template"] = "claude"
    filled = {field: value for field, value in supplied.items() if field not in given}
    overrides = tuple(field for field in _PROFILE_FIELDS if field in given)
    return params.model_copy(update=filled), overrides, profile.read_only


#: Model ids Claude Code itself accepts. Anything else on the `claude`
#: template is a non-Anthropic id handed to Claude Code, which the engine
#: would pass through as `claude --model <id>` and start anyway (WI-1590).
#: `opus`, `sonnet`, `haiku` and `fable` are the aliases Claude Code resolves
#: itself; a full id is `claude-…`.
_CLAUDE_MODEL = re.compile(r"^(claude-.+|opus|sonnet|haiku|fable)$")
#: The bare aliases among those: names only Claude Code resolves.
_CLAUDE_ALIAS = re.compile(r"^(opus|sonnet|haiku|fable)$")
#: Agent templates that are not Claude Code.
_NON_CLAUDE_AGENTS = ("codex", "opencode", "klaudia")


def _resolved_template_tag(template: str | None) -> str | None:
    """The agent a template name means, after the engine's alias and tag rules.

    The engine matches a template by name first and, failing that, by tag
    (`resolve_template_in`), so `claude` reaches the template tagged `claude`
    and `Claude Code (protected)` is the same template by name. The core never
    sees the deployment's template list, so the resolution mirrored here is
    the built-in set those names come from: a name or tag of `claude`,
    `codex`, `opencode` or `klaudia`, compared case-insensitively the way the
    engine compares them. A name that names none of them is not an agent
    template and is returned as given, lower-cased, so a deployment's own
    template still has a stable id to judge.
    """
    if template is None:
        return None
    folded = template.strip().lower()
    for agent in ("claude", "codex", "opencode", "klaudia"):
        if folded == agent or folded.endswith(f"({agent})"):
            return agent
    # The built-in template's own name, "Claude Code (protected)": the engine
    # matches a template by its full name before it tries a tag.
    if folded.startswith("claude code"):
        return "claude"
    return folded


def _model_matches_forbidden_text(model: str, patterns: list[str]) -> str | None:
    """The first `forbidden_text` pattern the model id matches, or None.

    The same patterns the engine applies to a command, matched here against
    the model id the start will actually run. A pattern that does not compile
    is skipped rather than refusing every start: the write path already
    refuses one, and a project stored before that rule must still be readable.
    """
    for pattern in patterns:
        try:
            if re.search(pattern, model):
                return pattern
        except re.error:
            continue
    return None


def _refuse_forbidden_model(ctx: AppContext, params: StartSessionParams) -> None:
    """Refuse a start whose model the subject's project forbids (WI-1590).

    `agent_policy.review.never_models` is documented as a hard rule and was
    only ever written into the brief as text, so a dispatch profile could
    assign exactly the model the project forbids and the session still
    spawned. The model judged is the one the start will actually run — the
    profile's, unless the caller named one — and the comparison is on the
    model id, not the display name.

    Two more rules on the same model. The project's `forbidden_text` patterns
    are matched against it, so a project that forbids `(?i)grok` refuses
    `grok-4.7` even when `never_models` does not list it. And a template that
    resolves to Claude Code is refused with a model id Claude Code does not
    speak: the engine would otherwise launch `claude --model grok-4.7`.

    A start that names no model has nothing to judge. A project with no
    policy still has the Claude Code check, which does not depend on one.
    """
    template = _resolved_template_tag(params.template)
    not_claude = params.model is None or _CLAUDE_MODEL.match(params.model) is None
    if template == "claude" and params.model is not None and not_claude:
        msg = (
            f"template 'claude' runs Claude Code, which does not take model "
            f"{params.model!r}; name a claude model (claude-*, opus, sonnet, "
            "haiku, fable) or a different template"
        )
        raise PolicyRefused(msg)
    if (
        template in _NON_CLAUDE_AGENTS
        and params.model is not None
        and _CLAUDE_ALIAS.match(params.model)
    ):
        # The mirror image (WI-1852): `opus` is a name only Claude Code
        # resolves, and the other agents refuse it on every turn.
        msg = (
            f"template {params.template!r} runs {template}, which does not "
            f"resolve the Claude Code alias {params.model!r}; use template "
            "'claude', or name a model that agent takes"
        )
        raise PolicyRefused(msg)
    if params.model is None:
        return
    with ctx.declared.read() as view:
        project = _project_of(view, params)
        if project is None or project.agent_policy is None:
            return
        policy = project.agent_policy
        review = policy.review
        forbidden = [] if review is None else review.never_models
        patterns = list(policy.forbidden_text)
    where = f"project {project.slug!r}"
    if params.model in forbidden:
        msg = (
            f"model {params.model!r} is on {where}'s review policy never_models, "
            "so a session may not start on it"
        )
        raise PolicyRefused(msg)
    matched = _model_matches_forbidden_text(params.model, patterns)
    if matched is not None:
        msg = (
            f"model {params.model!r} matches {where}'s forbidden_text pattern "
            f"{matched!r}, so a session may not start on it"
        )
        raise PolicyRefused(msg)


def _project_of(view: ReadView, params: StartSessionParams) -> Project | None:
    """The project a start is about, before its checkout is resolved.

    A work item's project is the one whose policy applies; a start that names
    a project directly is about that project; a start that names neither has
    no subject yet and so no policy to apply.
    """
    if params.work_item is not None:
        project_id = _resolve.work_item(view, params.work_item).project_id
        return view.project_by_id(project_id) if project_id is not None else None
    if params.project is not None:
        return _resolve.project(view, params.project)
    return None


def _started_by(
    ctx: AppContext, engine: EngineClient
) -> tuple[str | None, Literal["person", "oversight", "worker"] | None]:
    """Who is starting this session, from the caller's verified identity.

    A person (the check `session.grant_decide` makes) is `person` with no
    session. A session is named by its token's actor, and its kind is its
    engine role right now; None when the engine cannot say. Nothing here is
    read from the start's parameters.
    """
    if ctx.principal.kind != "agent":
        return None, "person"
    caller = _caller_engine_session_id(ctx)
    if caller is None:
        return _claimed_oversight(ctx, engine)
    try:
        live = engine.get_session(caller)
    except EngineUnavailable:
        live = None
    if live is None or live.role not in ("oversight", "worker"):
        return caller, None
    return caller, "oversight" if live.role == "oversight" else "worker"


#: The shared pod token's actor (`agent:pod:<stack>`).
_POD_ACTOR_PREFIX = "agent:pod:"


def _claimed_oversight(
    ctx: AppContext, engine: EngineClient
) -> tuple[str | None, Literal["person", "oversight", "worker"] | None]:
    """A pod-token caller's starter, from the session its bridge says it runs in.

    An oversight session resumed by hand (`claude --resume` in a shell) runs
    with the pod's token, not its own, so it has no session identity and its
    starts were recorded with no starter — and its ownership filter lost them
    (WI-1852). The MCP bridge names the engine session it runs in; that claim
    counts only from the pod token, which already outranks any session, and
    only when the engine says that session is live and oversight. It sets
    attribution and nothing else.
    """
    claimed = ctx.claimed_engine_session
    if claimed is None or not ctx.principal.identity_ref.startswith(_POD_ACTOR_PREFIX):
        return None, None
    try:
        live = engine.get_session(claimed)
    except EngineUnavailable:
        return None, None
    if live is None or not live.alive or live.role != "oversight":
        return None, None
    return claimed, "oversight"


def _autopilot(params: StartSessionParams) -> bool:
    """Whether a start runs on autopilot (WI-949): as asked, or — asked
    nothing — on for an agent template given a task, the shape of a session
    started to work through something, and off for anything else."""
    if params.autopilot is not None:
        return params.autopilot
    return bool(params.template and params.task and params.task.strip())


def _start_on_engine(
    engine: EngineClient,
    *,
    name: str,
    template: str | None,
    cwd: str,
    env: dict[str, str],
    brief: str,
    companion: str = "",
    charter: str = "",
    model: str | None = None,
    effort: str | None = None,
    resume: str | None = None,
    permission_mode: str | None = None,
    account: str | None = None,
    autopilot: bool = False,
    role: str = "worker",
    work_item: str | None = None,
    policy: AgentPolicy | None = None,
    budget: SessionBudgetParams | None = None,
    worktree: dict[str, str] | None = None,
) -> EngineSession:
    enforced = _enforced_policy(policy)
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
        account=account,
        autopilot=autopilot,
        role=role,
        work_item=work_item,
        companion=companion or None,
        charter=charter or None,
        policy=enforced,
        budget=_budget_wire(budget),
        worktree=worktree,
    )


def _session_env(
    ctx: AppContext, session_id: str, secret: str, *, work_item: str | None = None
) -> dict[str, str]:
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
    if work_item:
        # The item it was started for (WI-998), readable without a call — the
        # same variable an agent task run gets. A later session.bind_work is
        # not reflected here: the env is the start's, the binding is the
        # core's (`session.list`, `auth.whoami`).
        env["VOGT_WORK_ITEM"] = work_item
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
        "purpose": session.purpose,
        "started_by_session": session.started_by_session,
        "started_by_kind": session.started_by_kind,
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
        work_item_title=None if work_item is None else work_item.title,
        work_item_state=None if work_item is None else work_item.state,
        actor=session.actor_id if actor is None else actor.identity_ref,
        cwd=session.cwd,
        template=session.template,
        model=session.model,
        effort=session.effort,
        reason=session.reason,
        purpose=session.purpose,
        started_by_session=session.started_by_session,
        started_by_kind=session.started_by_kind,
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
        "name": engine_session.name or None,
        "command": engine_session.command,
        "exit_code": engine_session.exit_code,
        "activity_changed_at": _parse_engine_timestamp(
            engine_session.activity_changed_at
        ),
        "conversation_agent": engine_session.conversation_agent,
        "turn_started_at": _parse_engine_timestamp(engine_session.turn_started_at),
        "last_output_at": _parse_engine_timestamp(engine_session.last_output_at),
        "done_at": _parse_engine_timestamp(engine_session.done_at),
        "approval": _approval(engine_session.approval),
        "blocked": _blocked(engine_session.blocked),
        "hibernation": _hibernation(engine_session.hibernation),
        "keep_awake": engine_session.keep_awake,
        "autopilot": engine_session.autopilot,
        "autopilot_nudges": engine_session.autopilot_nudges,
        "role": engine_session.role,
        "conversation_id": engine_session.conversation_id,
        "resources": _resources(engine_session.resources),
        "permission_mode": engine_session.permission_mode,
        "account": engine_session.account,
        "stopped_by": engine_session.stopped_by,
        "stop_reason": engine_session.stop_reason,
        "budget": _budget_params(engine_session.budget),
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
        superseded_by=hibernation.superseded_by,
    )


def _blocked(blocked: EngineBlocked | None) -> SessionBlocked | None:
    if blocked is None:
        return None
    return SessionBlocked(
        blocker=blocked.reason,
        items=list(blocked.items),
        since=_parse_engine_timestamp(blocked.since),
    )


def _summarize_engine_only(
    engine_session: EngineSession, view: ReadView | None = None
) -> SessionSummary:
    """A session the engine is running but Vogt never linked.

    Vogt has no declared row for it, so every audited field is null: it is
    not Vogt's to name a project, actor or reason for something it did not
    start. What can be reported truthfully is reported — the engine's id,
    where it runs, its live activity, the engine's own start time, and the
    `work_item` label a session.bind_work put on it (WI-998), joined to the
    item's title and state when `view` is given and the ref still resolves.
    """
    label = engine_session.work_item
    item = None if label is None or view is None else view.work_item_by_ref(label)
    return SessionSummary(
        linked=False,
        id=engine_session.id,
        engine_session_id=engine_session.id,
        project=None,
        work_item=label,
        work_item_title=None if item is None else item.title,
        work_item_state=None if item is None else item.state,
        actor=None,
        cwd=engine_session.cwd,
        # The engine's own record of the template, not an audited claim: the
        # name it was started from, as the GUI's rail shows it.
        template=engine_session.template,
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
    "bind_session_work",
    "digest",
    "economics",
    "hibernate_session",
    "history_list",
    "keep_session_awake",
    "last_reply",
    "list_sessions",
    "log_tail",
    "remove_session",
    "rename_session",
    "report_blocked",
    "report_unblocked",
    "search_output",
    "session_changes",
    "session_input",
    "session_screen",
    "session_wait",
    "session_wait_any",
    "start_session",
    "stop_session",
    "sweep_sessions",
    "wake_session",
]
