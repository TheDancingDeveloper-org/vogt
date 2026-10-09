"""The GUI's session actions, on every surface: rename, remove, engine status.

Each was an engine route the PWA used with no core operation over it, so an
agent could do it only by calling the engine directly (WI-1094). These cover
what the parity harness does not: unlinked sessions, an engine that has
forgotten a session, and an engine that does not answer.
"""

from __future__ import annotations

import dataclasses
import json
from typing import Any

import pytest

from vogt.adapters.engine import EngineClient
from vogt.application.context import AppContext
from vogt.application.models import (
    EngineStatusParams,
    ListAuditParams,
    ListSessionsParams,
    RegisterProjectParams,
    RemoveSessionParams,
    RenameSessionParams,
    StartSessionParams,
    StopSessionParams,
)
from vogt.application.services import (
    engine_status,
    list_audit,
    list_sessions,
    register_project,
    remove_session,
    rename_session,
    start_session,
    stop_session,
)
from vogt.errors import InvalidRequest, NotFound

from tests.test_session_hibernation import ROOT, UNLINKED, Engine, _token_revoked

WHY = "rename/remove test"


class RenamingEngine(Engine):
    """The hibernation stand-in, plus rename, delete and the status report."""

    def __init__(self) -> None:
        super().__init__()
        self.names: dict[str, str] = {}
        #: Killed but still listed, as the engine keeps them until a DELETE.
        self.exited: set[str] = set()
        self.status: tuple[int, bytes] = (
            200,
            json.dumps(
                {
                    "version": "0.8.0",
                    "product_version": "0.8.0",
                    "source_sha": "abc",
                    "session_count": 2,
                    "history": {"enabled": True, "archived_session_count": 4},
                    "agent_tasks": {"task_count": 1},
                    "auth_broker": {"auto_agent_auth": True, "helper": "h"},
                    "storage": {"state_dir": "/s", "workspace_root": "/w"},
                    "event_lag": {"push": {"episodes": 2, "events_skipped": 9}},
                }
            ).encode(),
        )

    def summary(self, engine_id: str) -> dict[str, Any]:
        row = super().summary(engine_id)
        row["name"] = self.names.get(engine_id, row["name"])
        row["template"] = "claude"
        row["command"] = "claude"
        if engine_id in self.exited:
            row.update(activity="stopped", alive=False, exit_code=-9)
        return row

    def __call__(
        self,
        url: str,
        headers: dict[str, str],
        body: bytes = b"",
        method: str = "GET",
    ) -> tuple[int, bytes]:
        path = url.split("?", 1)[0].removeprefix("http://127.0.0.1:8910")
        if path == "/api/status":
            return self.status
        parts = path.split("/")
        known = self.live | self.hibernated | self.exited
        if method == "POST" and path.endswith("/kill") and parts[3] in known:
            self.sent.append({"path": path, "method": method, "body": json.loads(body)})
            self.live.discard(parts[3])
            self.hibernated.discard(parts[3])
            self.exited.add(parts[3])
            return 200, b'{"ok":true}'
        if method == "GET" and len(parts) == 4 and parts[3] in self.exited:
            return 200, json.dumps({"summary": self.summary(parts[3])}).encode()
        if method in ("PATCH", "DELETE") and len(parts) == 4:
            payload = json.loads(body.decode("utf-8")) if body else {}
            self.sent.append({"path": path, "method": method, "body": payload})
            engine_id = parts[3]
            if engine_id not in known:
                return 404, b""
            if method == "PATCH":
                name = str(payload.get("name", "")).strip()
                if not name:
                    return 400, json.dumps({"error": "name must not be empty"}).encode()
                self.names[engine_id] = name
            else:
                self.live.discard(engine_id)
                self.hibernated.discard(engine_id)
                self.exited.discard(engine_id)
            return 200, b'{"ok":true}'
        return super().__call__(url, headers, body, method)


@pytest.fixture
def engine() -> RenamingEngine:
    return RenamingEngine()


@pytest.fixture
def wired(instance: AppContext, engine: RenamingEngine) -> AppContext:
    ctx = dataclasses.replace(
        instance,
        engine=EngineClient(base_url="http://127.0.0.1:8910", transport=engine),
    )
    register_project(
        ctx, RegisterProjectParams(name="Vogt", root_path=ROOT, reason=WHY)
    )
    return ctx


def _started(ctx: AppContext) -> tuple[str, str]:
    result = start_session(
        ctx, StartSessionParams(project="vogt", template="claude", reason=WHY)
    )
    return result.session.id, result.session.engine_session_id


def test_rename_reaches_the_engine_and_reads_back_as_name(
    wired: AppContext, engine: RenamingEngine
) -> None:
    ses_id, engine_id = _started(wired)
    result = rename_session(
        wired, RenameSessionParams(id=ses_id, name="  driver  ", reason=WHY)
    )
    assert engine.names[engine_id] == "driver"
    assert result.session.name == "driver"
    listed = list_sessions(wired, ListSessionsParams()).sessions
    assert [s.name for s in listed if s.id == ses_id] == ["driver"]
    audit = list_audit(wired, ListAuditParams(limit=5)).records
    assert audit[0].operation == "session.rename"
    assert audit[0].reason == WHY


def test_rename_works_on_a_session_the_gui_started(
    wired: AppContext, engine: RenamingEngine
) -> None:
    engine.live.add(UNLINKED)
    result = rename_session(
        wired, RenameSessionParams(id=UNLINKED, name="gui one", reason=WHY)
    )
    assert result.session.linked is False
    assert result.session.name == "gui one"
    assert result.session.template == "claude", "the engine's template is shown"


def test_rename_refusals_are_said(wired: AppContext, engine: RenamingEngine) -> None:
    with pytest.raises(NotFound):
        rename_session(wired, RenameSessionParams(id=UNLINKED, name="x", reason=WHY))
    ses_id, _ = _started(wired)
    with pytest.raises(InvalidRequest, match="must not be empty"):
        rename_session(wired, RenameSessionParams(id=ses_id, name="   ", reason=WHY))


def test_remove_forgets_a_live_linked_session_and_closes_its_record(
    wired: AppContext, engine: RenamingEngine
) -> None:
    ses_id, engine_id = _started(wired)
    secret = engine.start_env()["VOGT_HTTP_TOKEN"]
    result = remove_session(wired, RemoveSessionParams(id=ses_id, reason=WHY))
    assert engine_id not in engine.live | engine.exited
    # Killed with who and why first, so the exit reads `stopped` (WI-913);
    # only then forgotten.
    calls = engine.calls(engine_id)
    assert calls.index("POST kill") < calls.index("DELETE ")
    kill = next(r for r in engine.sent if r["path"].endswith(f"{engine_id}/kill"))
    assert kill["body"] == {"reason": WHY, "by": wired.principal.identity_ref}
    assert result.session.stopped_at is not None
    assert _token_revoked(wired, secret)
    audit = list_audit(wired, ListAuditParams(limit=5)).records
    assert audit[0].operation == "session.remove"


def test_remove_after_stop_only_forgets_the_engine_copy(
    wired: AppContext, engine: RenamingEngine
) -> None:
    ses_id, engine_id = _started(wired)
    stop_session(wired, StopSessionParams(id=ses_id, reason=WHY))
    assert engine_id in engine.exited, "a killed session stays listed"
    kills = engine.calls(engine_id).count("POST kill")
    result = remove_session(wired, RemoveSessionParams(id=ses_id, reason=WHY))
    assert engine_id not in engine.exited
    assert engine.calls(engine_id).count("POST kill") == kills, "not killed twice"
    assert result.session.id == ses_id


def test_remove_closes_a_linked_record_the_engine_already_forgot(
    wired: AppContext, engine: RenamingEngine
) -> None:
    ses_id, engine_id = _started(wired)
    engine.live.discard(engine_id)
    result = remove_session(wired, RemoveSessionParams(id=ses_id, reason=WHY))
    assert result.session.stopped_at is not None


def test_remove_an_unlinked_session(wired: AppContext, engine: RenamingEngine) -> None:
    engine.live.add(UNLINKED)
    result = remove_session(wired, RemoveSessionParams(id=UNLINKED, reason=WHY))
    assert UNLINKED not in engine.live | engine.exited
    calls = engine.calls(UNLINKED)
    assert calls.index("POST kill") < calls.index("DELETE ")
    assert result.session.linked is False
    assert result.session.alive is False
    with pytest.raises(NotFound):
        remove_session(wired, RemoveSessionParams(id=UNLINKED, reason=WHY))


def test_engine_status_reports_what_the_engine_says(
    wired: AppContext, engine: RenamingEngine
) -> None:
    status = engine_status(wired, EngineStatusParams())
    assert status.engine is None
    assert status.version == "0.8.0"
    assert status.session_count == 2
    assert status.workspace_root == "/w"
    assert status.auto_agent_auth is True
    assert status.history is not None
    assert status.history.archived_session_count == 4
    assert status.event_lag["push"].events_skipped == 9


def test_engine_status_says_an_outage_rather_than_reporting_zeros(
    wired: AppContext, engine: RenamingEngine, instance: AppContext
) -> None:
    engine.status = (503, b"")
    down = engine_status(wired, EngineStatusParams())
    assert down.engine is not None
    assert down.session_count is None
    absent = engine_status(
        dataclasses.replace(instance, engine=None), EngineStatusParams()
    )
    assert absent.engine is not None
    assert "not configured" in absent.engine or "unset" in absent.engine
