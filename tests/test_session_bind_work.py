"""Binding a running session to a work item (WI-998).

`session.bind_work` re-declares `coding_sessions.work_item_id` and copies it
to the engine as a `work_item` label. The engine is stood in for by a
transport that keeps the one piece of state these operations are about: each
session's label. What is asserted is what the core owns — the declared row,
its audit trail, the label it sends, the warning on close, and the reads
(`session.list`, `work.get`, the Inbox) that name the item.
"""

from __future__ import annotations

import dataclasses
import json
from typing import Any

import pytest

from vogt.adapters.engine import EngineClient
from vogt.application.context import AppContext
from vogt.application.models import (
    BindSessionWorkParams,
    CreateWorkParams,
    GetWorkParams,
    InboxListParams,
    ListAuditParams,
    ListEventsParams,
    ListSessionsParams,
    RegisterProjectParams,
    StartSessionParams,
    StopSessionParams,
    TransitionWorkParams,
)
from vogt.application.services import (
    bind_session_work,
    create_work,
    get_work,
    list_audit,
    list_events,
    list_inbox,
    list_sessions,
    register_project,
    start_session,
    stop_session,
    transition_work,
)
from vogt.core.principal import Principal
from vogt.errors import Conflict, InvalidRequest, NotFound

WHY = "binding test"
ROOT = "/srv/estate/vogt"
OPS_ROOT = "/srv/estate/ops"
UNLINKED = "0b0e5f0c-1111-4222-8333-944455556666"


class Engine:
    """Live sessions, their `work_item` labels, and every request made."""

    def __init__(self) -> None:
        self.sent: list[dict[str, Any]] = []
        self.live: set[str] = set()
        self.labels: dict[str, str] = {}
        self.blocked: set[str] = set()
        self.counter = 0
        #: When false, the engine predates the work-item route (404).
        self.knows_work_items = True

    def summary(self, engine_id: str) -> dict[str, Any]:
        return {
            "id": engine_id,
            "name": f"s{engine_id[-1]}",
            "activity": "idle",
            "alive": engine_id in self.live,
            "exit_code": None if engine_id in self.live else 0,
            "cwd": ROOT,
            "created_at": "2026-10-07T00:00:00Z",
            **(
                {"work_item": self.labels[engine_id]}
                if engine_id in self.labels
                else {}
            ),
            **(
                {
                    "blocked": {
                        "reason": "needs the bot token",
                        "items": ["store it"],
                        "since": "2026-10-07T01:00:00Z",
                    }
                }
                if engine_id in self.blocked
                else {}
            ),
        }

    def __call__(
        self,
        url: str,
        headers: dict[str, str],
        body: bytes = b"",
        method: str = "GET",
    ) -> tuple[int, bytes]:
        payload = json.loads(body.decode("utf-8")) if body else {}
        path = url.split("?", 1)[0].removeprefix("http://127.0.0.1:8910")
        self.sent.append({"path": path, "method": method, "body": payload})
        if method == "POST" and path == "/api/sessions":
            self.counter += 1
            engine_id = f"00000000-0000-4000-8000-00000000000{self.counter}"
            self.live.add(engine_id)
            if payload.get("work_item"):
                self.labels[engine_id] = payload["work_item"]
            return 200, json.dumps(self.summary(engine_id)).encode()
        if method == "GET" and path == "/api/sessions":
            return 200, json.dumps(
                [self.summary(i) for i in sorted(self.live)]
            ).encode()
        parts = path.split("/")
        if len(parts) < 4 or parts[2] != "sessions":
            return 404, b""
        engine_id = parts[3]
        verb = parts[4] if len(parts) > 4 else ""
        if engine_id not in self.live:
            return 404, b""
        if method == "GET" and verb == "":
            return 200, json.dumps({"summary": self.summary(engine_id)}).encode()
        if method == "POST" and verb == "work-item":
            if not self.knows_work_items:
                return 404, b""
            if payload.get("work_item"):
                self.labels[engine_id] = payload["work_item"]
            else:
                self.labels.pop(engine_id, None)
            return 200, json.dumps(self.summary(engine_id)).encode()
        if method == "POST" and verb == "kill":
            self.live.discard(engine_id)
            return 200, b'{"ok":true}'
        return 404, b""

    def label_writes(self, engine_id: str) -> list[Any]:
        return [
            row["body"].get("work_item")
            for row in self.sent
            if row["path"] == f"/api/sessions/{engine_id}/work-item"
        ]

    def start_spec(self) -> dict[str, Any]:
        starts = [
            r
            for r in self.sent
            if r["method"] == "POST" and r["path"] == "/api/sessions"
        ]
        spec: dict[str, Any] = starts[-1]["body"]
        return spec


@pytest.fixture
def engine() -> Engine:
    return Engine()


@pytest.fixture
def wired(instance: AppContext, engine: Engine) -> AppContext:
    ctx = dataclasses.replace(
        instance,
        engine=EngineClient(base_url="http://127.0.0.1:8910", transport=engine),
    )
    register_project(
        ctx, RegisterProjectParams(name="Vogt", root_path=ROOT, reason=WHY)
    )
    register_project(
        ctx, RegisterProjectParams(name="Ops", root_path=OPS_ROOT, reason=WHY)
    )
    for title, project in [("terminal render bug", "vogt"), ("ops item", "ops")]:
        create_work(
            ctx,
            CreateWorkParams(
                kind="bug", title=title, project=project, local_only=True, reason=WHY
            ),
        )
    return ctx


def _started(ctx: AppContext, **kwargs: Any) -> tuple[str, str]:
    params = {"project": "vogt", "template": "claude", "reason": WHY, **kwargs}
    result = start_session(ctx, StartSessionParams(**params))
    return result.session.id, result.session.engine_session_id


def _as_session(ctx: AppContext, ses_id: str) -> AppContext:
    return dataclasses.replace(
        ctx,
        principal=Principal(
            identity_ref=f"agent:session:{ses_id}",
            kind="agent",
            display_name=f"Session {ses_id}",
        ),
    )


def test_a_session_started_for_an_item_carries_it_in_env_and_label(
    wired: AppContext, engine: Engine
) -> None:
    _started(wired, project=None, work_item="WI-1")
    spec = engine.start_spec()
    assert dict(spec["env"])["VOGT_WORK_ITEM"] == "WI-1"
    assert spec["work_item"] == "WI-1"
    assert "bound to WI-1" in spec["prompt"]
    _started(wired)
    plain = engine.start_spec()
    assert "VOGT_WORK_ITEM" not in dict(plain["env"])
    assert "work_item" not in plain
    assert "session_bind_work" in plain["prompt"]


def test_bind_rebind_and_unbind_are_audited_and_labelled(
    wired: AppContext, engine: Engine
) -> None:
    ses_id, engine_id = _started(wired)

    bound = bind_session_work(
        wired, BindSessionWorkParams(id=ses_id, work_item="WI-1", reason=WHY)
    )
    assert bound.session.work_item == "WI-1"
    assert bound.session.work_item_title == "terminal render bug"
    assert bound.session.work_item_state == "open"
    assert bound.previous_work_item is None
    assert bound.engine_label == "written"
    assert bound.project_mismatch is False
    # Binding never moves the item (decision 4).
    assert get_work(wired, GetWorkParams(ref="WI-1")).item.state == "open"

    rebound = bind_session_work(
        wired, BindSessionWorkParams(id=engine_id, work_item="WI-2", reason=WHY)
    )
    assert rebound.previous_work_item == "WI-1"
    assert rebound.project_mismatch is True, "WI-2 is filed under ops"
    assert rebound.session.cwd == ROOT, "binding never moves the terminal"

    cleared = bind_session_work(
        wired, BindSessionWorkParams(id=ses_id, work_item=None, reason=WHY)
    )
    assert cleared.session.work_item is None
    assert cleared.previous_work_item == "WI-2"
    assert engine.label_writes(engine_id) == ["WI-1", "WI-2", None]
    assert engine_id not in engine.labels

    audit = list_audit(wired, ListAuditParams(limit=10)).records
    assert [a.operation for a in audit[:3]] == ["session.bind_work"] * 3
    assert all(a.entity_id == ses_id for a in audit[:3])
    events = list_events(wired, ListEventsParams(limit=10)).events
    kinds = [e.kind for e in events if e.kind.startswith("session.work_")]
    assert sorted(kinds) == [
        "session.work_bound",
        "session.work_bound",
        "session.work_unbound",
    ]


def test_bind_records_the_declared_branch_and_shows_on_the_item(
    wired: AppContext,
) -> None:
    ses_id, _ = _started(wired)
    bind_session_work(
        wired, BindSessionWorkParams(id=ses_id, work_item="WI-1", reason=WHY)
    )
    detail = get_work(wired, GetWorkParams(ref="WI-1"))
    assert [s.id for s in detail.sessions] == [ses_id]
    assert detail.sessions[0].alive is True
    assert any(b.source in ("declared", "both") for b in detail.branches)
    listed = list_sessions(wired, ListSessionsParams(work_item="WI-1")).sessions
    assert [s.id for s in listed] == [ses_id]


def test_a_session_binds_itself_without_an_id(wired: AppContext) -> None:
    ses_id, _ = _started(wired)
    own = _as_session(wired, ses_id)
    result = bind_session_work(own, BindSessionWorkParams(work_item="WI-1", reason=WHY))
    assert result.session.id == ses_id
    assert result.session.work_item == "WI-1"
    record = list_audit(wired, ListAuditParams(limit=1)).records[0]
    assert record.actor_identity_ref == f"agent:session:{ses_id}"


def test_another_session_may_bind_a_child(wired: AppContext) -> None:
    overseer, _ = _started(wired)
    child, _ = _started(wired)
    result = bind_session_work(
        _as_session(wired, overseer),
        BindSessionWorkParams(id=child, work_item="WI-1", reason=WHY),
    )
    assert result.session.id == child
    assert result.session.work_item == "WI-1"


def test_an_omitted_id_outside_a_session_is_refused(wired: AppContext) -> None:
    with pytest.raises(InvalidRequest):
        bind_session_work(wired, BindSessionWorkParams(work_item="WI-1", reason=WHY))


def test_an_unknown_item_or_a_stopped_session_is_refused(
    wired: AppContext,
) -> None:
    ses_id, _ = _started(wired)
    with pytest.raises(NotFound):
        bind_session_work(
            wired, BindSessionWorkParams(id=ses_id, work_item="WI-99", reason=WHY)
        )
    stop_session(wired, StopSessionParams(id=ses_id, reason=WHY))
    with pytest.raises(Conflict):
        bind_session_work(
            wired, BindSessionWorkParams(id=ses_id, work_item="WI-1", reason=WHY)
        )


def test_a_label_the_engine_cannot_take_is_reported_not_fatal(
    wired: AppContext, engine: Engine
) -> None:
    ses_id, _ = _started(wired)
    engine.knows_work_items = False
    result = bind_session_work(
        wired, BindSessionWorkParams(id=ses_id, work_item="WI-1", reason=WHY)
    )
    assert result.engine_label == "not_found"
    assert result.engine is not None
    assert result.session.work_item == "WI-1", "the core row is the truth"

    offline = dataclasses.replace(wired, engine=None)
    unbound = bind_session_work(
        offline, BindSessionWorkParams(id=ses_id, work_item=None, reason=WHY)
    )
    assert unbound.engine_label == "unavailable"
    assert unbound.session.work_item is None
    assert unbound.session.alive is None, "not asked is not dead"


def test_an_unlinked_session_is_bound_by_its_engine_label_only(
    wired: AppContext, engine: Engine
) -> None:
    engine.live.add(UNLINKED)
    result = bind_session_work(
        wired, BindSessionWorkParams(id=UNLINKED, work_item="WI-1", reason=WHY)
    )
    assert result.session.linked is False
    assert result.session.work_item == "WI-1"
    assert result.session.work_item_title == "terminal render bug"
    assert engine.labels[UNLINKED] == "WI-1"
    # No adoption (decision 5): the core has no row for it afterwards either.
    rows = list_sessions(wired, ListSessionsParams()).sessions
    unlinked = next(s for s in rows if s.engine_session_id == UNLINKED)
    assert unlinked.linked is False
    assert unlinked.work_item == "WI-1"
    record = list_audit(wired, ListAuditParams(limit=1)).records[0]
    assert record.operation == "session.bind_work"
    assert record.entity_id == UNLINKED

    engine.knows_work_items = False
    with pytest.raises(NotFound):
        bind_session_work(
            wired, BindSessionWorkParams(id=UNLINKED, work_item=None, reason=WHY)
        )


def test_closing_an_item_with_live_bound_sessions_warns_and_never_refuses(
    wired: AppContext, engine: Engine
) -> None:
    ses_id, engine_id = _started(wired)
    bind_session_work(
        wired, BindSessionWorkParams(id=ses_id, work_item="WI-1", reason=WHY)
    )
    moved = transition_work(
        wired, TransitionWorkParams(ref="WI-1", to_state="in_progress", reason=WHY)
    )
    assert moved.live_sessions == [], "only a finishing transition warns"

    closed = transition_work(
        wired, TransitionWorkParams(ref="WI-1", to_state="done", reason=WHY, walk=True)
    )
    assert closed.item.state == "done"
    assert [s.id for s in closed.live_sessions] == [ses_id]
    # Nothing was unbound (decision 3).
    assert engine.labels[engine_id] == "WI-1"
    still = get_work(wired, GetWorkParams(ref="WI-1")).sessions
    assert [s.work_item_state for s in still] == ["done"]

    # Binding to a finished item is allowed (a verification session).
    other, _ = _started(wired)
    late = bind_session_work(
        wired, BindSessionWorkParams(id=other, work_item="WI-1", reason=WHY)
    )
    assert late.session.work_item_state == "done"


def test_closing_with_no_live_session_carries_no_warning(
    wired: AppContext,
) -> None:
    ses_id, _ = _started(wired, project=None, work_item="WI-1")
    stop_session(wired, StopSessionParams(id=ses_id, reason=WHY))
    closed = transition_work(
        wired, TransitionWorkParams(ref="WI-1", to_state="wont_do", reason=WHY)
    )
    assert closed.live_sessions == []


def test_the_inbox_names_the_bound_item_on_a_blocked_session(
    wired: AppContext, engine: Engine
) -> None:
    ses_id, engine_id = _started(wired)
    bind_session_work(
        wired, BindSessionWorkParams(id=ses_id, work_item="WI-1", reason=WHY)
    )
    engine.live.add(UNLINKED)
    engine.labels[UNLINKED] = "WI-2"
    engine.blocked |= {engine_id, UNLINKED}
    entries = list_inbox(wired, InboxListParams()).entries
    blocked = {e.session_id: e for e in entries if e.kind == "session.blocked"}
    assert blocked[engine_id].work_item_ref == "WI-1"
    assert blocked[engine_id].title.startswith("WI-1 session")
    assert blocked[UNLINKED].work_item_ref == "WI-2", "the label, for an unlinked one"
