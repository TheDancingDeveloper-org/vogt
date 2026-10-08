"""Quick chats (WI-1097), reached through the session engine.

A chat is the engine's: a persistent text conversation with an agent CLI
(Klaudia) driven over its stream-json protocol, stored in the engine's own
state and kept until deleted. These use-cases are the MCP/CLI/REST face of
the engine's `/api/chats` routes, so an agent can do what the PWA's Chat
panel does — with one difference that is the point of the feature: only a
person answers a chat's approvals (`chat.decide`), exactly as only a person
answers a session's permission prompt (WI-983).

Every write is audited after its effect (`audited_action`), because the
effect lands in the engine, not the declared store. What was typed is not in
the audit row — its byte count is.
"""

from __future__ import annotations

import time
import uuid
from typing import Any

from vogt.adapters.engine import EngineClient, EngineUnavailable
from vogt.application import writes
from vogt.application.context import AppContext
from vogt.application.models import (
    ChatApprovalView,
    ChatArchiveParams,
    ChatCreateParams,
    ChatDecideParams,
    ChatDetailResult,
    ChatEntryView,
    ChatGetParams,
    ChatIdParams,
    ChatListParams,
    ChatListResult,
    ChatPromoteParams,
    ChatPromoteResult,
    ChatSendParams,
    ChatSendResult,
    ChatSetModelParams,
    ChatSummaryView,
)
from vogt.application.services.sessions import _answers_as_person
from vogt.application.writes import audited_action
from vogt.errors import InvalidRequest, NotFound

CHAT_CREATE = "chat.create"
CHAT_SEND = "chat.send"
CHAT_DECIDE = "chat.decide"
CHAT_SET_MODEL = "chat.set_model"
CHAT_INTERRUPT = "chat.interrupt"
CHAT_ARCHIVE = "chat.archive"
CHAT_PROMOTE = "chat.promote"


def _engine(ctx: AppContext) -> EngineClient:
    if ctx.engine is None:
        msg = (
            "no session engine is configured, and chats run on it (set VOGT_ENGINE_URL)"
        )
        raise EngineUnavailable(msg)
    return ctx.engine


def _chat_id(raw: str) -> str:
    """A chat id is the engine's UUID; anything else is refused here, by
    name, rather than becoming a path segment."""
    try:
        return str(uuid.UUID(raw.strip()))
    except ValueError:
        msg = f"{raw!r} is not a chat id (a UUID, from chat.list or chat.create)"
        raise InvalidRequest(msg) from None


def _missing(chat_id: str) -> NotFound:
    return NotFound(
        f"the engine has no chat {chat_id}, or has chats turned off "
        "(ENGINE_CHAT_ENABLED, or no Klaudia launch configured)"
    )


def _summary(payload: object) -> ChatSummaryView:
    data = payload if isinstance(payload, dict) else {}
    return ChatSummaryView.model_validate(
        {k: v for k, v in data.items() if k in ChatSummaryView.model_fields}
    )


def _entries(payload: object) -> list[ChatEntryView]:
    rows = payload if isinstance(payload, list) else []
    return [
        ChatEntryView.model_validate(
            {k: v for k, v in row.items() if k in ChatEntryView.model_fields}
        )
        for row in rows
        if isinstance(row, dict)
    ]


def _approval(payload: object) -> ChatApprovalView:
    data = payload if isinstance(payload, dict) else {}
    return ChatApprovalView.model_validate(
        {k: v for k, v in data.items() if k in ChatApprovalView.model_fields}
    )


def _actor(ctx: AppContext) -> str:
    return ctx.principal.identity_ref


def chat_list(ctx: AppContext, params: ChatListParams) -> ChatListResult:
    """List chats, newest first, optionally by what was said in them."""
    rows = _engine(ctx).chat_list(
        q=params.q, archived=params.archived, limit=params.limit
    )
    if rows is None:
        return ChatListResult(chats=[], available=False)
    return ChatListResult(chats=[_summary(r) for r in rows], available=True)


def chat_get(ctx: AppContext, params: ChatGetParams) -> ChatDetailResult:
    """A chat with its newest entries and pending approvals."""
    chat_id = _chat_id(params.id)
    detail = _engine(ctx).chat_get(chat_id, tail=params.tail)
    if detail is None:
        raise _missing(chat_id)
    return ChatDetailResult(
        chat=_summary(detail),
        entries=_entries(detail.get("entries")),
        approvals=[_approval(a) for a in detail.get("approvals") or []],
    )


def _send_result(payload: dict[str, Any]) -> ChatSendResult:
    return ChatSendResult(
        chat=_summary(payload.get("chat")),
        entries=_entries(payload.get("entries")),
        finished=bool(payload.get("finished")),
    )


def chat_create(ctx: AppContext, params: ChatCreateParams) -> ChatSendResult:
    """Start a chat, optionally with its first message. The engine records
    the caller as its creator."""
    reason = writes.validate_reason(params.reason)
    body: dict[str, Any] = {"creator": _actor(ctx)}
    for key in ("title", "model", "message", "work_item"):
        value = getattr(params, key)
        if value:
            body[key] = value
    engine = _engine(ctx)
    created = engine.chat_create(body, wait_s=0)
    if created is None:
        raise _missing("(new)")
    result = _send_result(created)
    if params.message and params.wait_s:
        # The engine's create returns at once; waiting for the first reply
        # is a read of the same turn.
        result = _wait_for_turn(engine, result, params.wait_s)
    audited_action(
        ctx,
        operation=CHAT_CREATE,
        reason=reason,
        entity_kind="chat",
        entity_id=result.chat.id,
        outcome={
            "chat": result.chat.id,
            "model": result.chat.model,
            "work_item": result.chat.work_item,
            "message_bytes": len((params.message or "").encode("utf-8")),
        },
        event_kind="chat.created",
    )
    return result


def _wait_for_turn(
    engine: EngineClient, result: ChatSendResult, wait_s: int
) -> ChatSendResult:
    deadline = time.monotonic() + wait_s
    first_seq = min((e.seq for e in result.entries), default=1)
    while time.monotonic() < deadline:
        detail = engine.chat_get(result.chat.id, tail=500)
        if detail is None:
            break
        summary = _summary(detail)
        if summary.state == "idle":
            entries = [e for e in _entries(detail.get("entries")) if e.seq >= first_seq]
            return ChatSendResult(chat=summary, entries=entries, finished=True)
        time.sleep(0.5)
    return result


def chat_send(ctx: AppContext, params: ChatSendParams) -> ChatSendResult:
    """Send a message and, by default, wait for the reply."""
    reason = writes.validate_reason(params.reason)
    chat_id = _chat_id(params.id)
    sent = _engine(ctx).chat_send(
        chat_id, {"text": params.text, "wait_secs": params.wait_s, "by": _actor(ctx)}
    )
    if sent is None:
        raise _missing(chat_id)
    result = _send_result(sent)
    audited_action(
        ctx,
        operation=CHAT_SEND,
        reason=reason,
        entity_kind="chat",
        entity_id=chat_id,
        outcome={
            "chat": chat_id,
            "bytes": len(params.text.encode("utf-8")),
            "finished": result.finished,
        },
        event_kind="chat.message_sent",
    )
    return result


def chat_decide(ctx: AppContext, params: ChatDecideParams) -> ChatApprovalView:
    """A person's answer to a chat's approval. An agent is refused
    (`PersonRequired`): a chat's approvals are the only gate on what its
    agent may change, so they are a person's to give."""
    reason = writes.validate_reason(params.reason)
    chat_id = _chat_id(params.id)
    person = _answers_as_person(ctx)
    decided = _engine(ctx).chat_decide(
        chat_id,
        params.approval_id,
        allow=params.allow,
        message=params.message,
        person=person,
    )
    if decided is None:
        raise _missing(chat_id)
    approval = _approval(decided)
    audited_action(
        ctx,
        operation=CHAT_DECIDE,
        reason=reason,
        entity_kind="chat",
        entity_id=chat_id,
        outcome={
            "chat": chat_id,
            "approval": approval.id,
            "tool": approval.tool_name,
            "summary": approval.summary[:300],
            "allowed": params.allow,
            "person": person,
        },
        event_kind="chat.approval_decided",
    )
    return approval


def chat_set_model(ctx: AppContext, params: ChatSetModelParams) -> ChatSummaryView:
    """Switch the model the chat's next turn runs on."""
    reason = writes.validate_reason(params.reason)
    chat_id = _chat_id(params.id)
    changed = _engine(ctx).chat_set_model(chat_id, params.model)
    if changed is None:
        raise _missing(chat_id)
    summary = _summary(changed)
    audited_action(
        ctx,
        operation=CHAT_SET_MODEL,
        reason=reason,
        entity_kind="chat",
        entity_id=chat_id,
        outcome={"chat": chat_id, "model": summary.model},
        event_kind="chat.model_set",
    )
    return summary


def chat_interrupt(ctx: AppContext, params: ChatIdParams) -> ChatSummaryView:
    """Stop the turn the chat is running."""
    reason = writes.validate_reason(params.reason)
    chat_id = _chat_id(params.id)
    stopped = _engine(ctx).chat_interrupt(chat_id)
    if stopped is None:
        raise _missing(chat_id)
    audited_action(
        ctx,
        operation=CHAT_INTERRUPT,
        reason=reason,
        entity_kind="chat",
        entity_id=chat_id,
        outcome={"chat": chat_id},
        event_kind="chat.interrupted",
    )
    return _summary(stopped)


def chat_archive(ctx: AppContext, params: ChatArchiveParams) -> ChatSummaryView:
    """Hide a chat from the default list, or bring it back. Nothing is deleted."""
    reason = writes.validate_reason(params.reason)
    chat_id = _chat_id(params.id)
    changed = _engine(ctx).chat_archive(chat_id, archived=params.archived)
    if changed is None:
        raise _missing(chat_id)
    audited_action(
        ctx,
        operation=CHAT_ARCHIVE,
        reason=reason,
        entity_kind="chat",
        entity_id=chat_id,
        outcome={"chat": chat_id, "archived": params.archived},
        event_kind="chat.archived" if params.archived else "chat.unarchived",
    )
    return _summary(changed)


def chat_promote(ctx: AppContext, params: ChatPromoteParams) -> ChatPromoteResult:
    """Continue the chat's conversation in a terminal session. The chat then
    takes no more messages."""
    reason = writes.validate_reason(params.reason)
    chat_id = _chat_id(params.id)
    body: dict[str, Any] = {}
    for key in ("cwd", "name", "work_item"):
        value = getattr(params, key)
        if value:
            body[key] = value
    promoted = _engine(ctx).chat_promote(chat_id, body)
    if promoted is None:
        raise _missing(chat_id)
    session = promoted.get("session")
    session = session if isinstance(session, dict) else {}
    result = ChatPromoteResult(
        chat=_summary(promoted.get("chat")),
        engine_session_id=str(session.get("id", "")),
        session_name=str(session.get("name", "")),
    )
    audited_action(
        ctx,
        operation=CHAT_PROMOTE,
        reason=reason,
        entity_kind="chat",
        entity_id=chat_id,
        outcome={"chat": chat_id, "engine_session_id": result.engine_session_id},
        event_kind="chat.promoted",
    )
    return result
