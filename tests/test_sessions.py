"""Coding sessions (FR-E3–E5, FR-E8, FR-E9, FR-S10).

The engine is stood in for by a transport that records what was sent. That
is the only honest way to assert the thing these tests are actually about:
not that a session was created, but *where* it was told to open and *what
credential* it was given. A stub that returned a session id without keeping
the spec could not tell a session opened in a project's tree from one opened
wherever the engine felt like.
"""

from __future__ import annotations

import dataclasses
import json
from typing import Any

import pytest

from vogt.adapters.engine import EngineClient, EngineUnavailable
from vogt.application.context import AppContext
from vogt.application.models import (
    CreateWorkParams,
    GetWorkParams,
    ListSessionsParams,
    RegisterProjectParams,
    StartSessionParams,
    StopSessionParams,
)
from vogt.application.services import (
    create_work,
    get_work,
    list_sessions,
    register_project,
    start_session,
    stop_session,
)
from vogt.core.auth import hash_token
from vogt.errors import InvalidRequest, NotFound

WHY = "session test"
ROOT = "/srv/estate/vogt"


class StandInEngine:
    """An engine that answers, and remembers what it was asked."""

    def __init__(self) -> None:
        self.sent: list[dict[str, Any]] = []
        self.killed: list[str] = []
        self.alive: dict[str, str] = {}
        self.counter = 0

    def __call__(
        self,
        url: str,
        headers: dict[str, str],
        body: bytes = b"",
        method: str = "GET",
    ) -> tuple[int, bytes]:
        payload = json.loads(body.decode("utf-8")) if body else {}
        self.sent.append({"url": url, "method": method, "body": payload})

        if method == "POST" and url.endswith("/api/sessions"):
            self.counter += 1
            engine_id = f"eng-{self.counter}"
            self.alive[engine_id] = "running"
            return 200, json.dumps(
                {
                    "id": engine_id,
                    "name": payload.get("name", ""),
                    "activity": "running",
                    "cwd": payload.get("cwd", ""),
                    "exit_code": None,
                }
            ).encode()
        if method == "POST" and url.endswith("/kill"):
            engine_id = url.rsplit("/", 2)[-2]
            self.killed.append(engine_id)
            existed = self.alive.pop(engine_id, None) is not None
            return (200, b'{"ok":true}') if existed else (404, b"")
        if method == "GET" and url.endswith("/api/sessions"):
            return 200, json.dumps(
                [
                    {"id": key, "name": key, "activity": state, "cwd": ROOT}
                    for key, state in self.alive.items()
                ]
            ).encode()
        return 404, b""

    @property
    def last_spec(self) -> dict[str, Any]:
        creates = [
            row
            for row in self.sent
            if row["method"] == "POST" and "kill" not in row["url"]
        ]
        spec: dict[str, Any] = creates[-1]["body"]
        return spec

    def env_of_last_start(self) -> dict[str, str]:
        return dict(self.last_spec.get("env", []))


class DeadEngine:
    """An engine that is configured and not answering."""

    def __call__(
        self,
        url: str,
        headers: dict[str, str],
        body: bytes = b"",
        method: str = "GET",
    ) -> tuple[int, bytes]:
        raise EngineUnavailable("the engine is not answering: connection refused")


@pytest.fixture
def engine() -> StandInEngine:
    return StandInEngine()


@pytest.fixture
def wired(instance: AppContext, engine: StandInEngine) -> AppContext:
    """An instance with a project, a work item, and an engine to talk to."""
    ctx = dataclasses.replace(
        instance,
        engine=EngineClient(base_url="http://127.0.0.1:8910", transport=engine),
    )
    register_project(
        ctx, RegisterProjectParams(name="Vogt", root_path=ROOT, reason=WHY)
    )
    create_work(
        ctx,
        CreateWorkParams(
            kind="bug",
            title="A bug to open a terminal on",
            project="vogt",
            reason=WHY,
        ),
    )
    return ctx


# -- where the session opens (FR-E3) ---------------------------------------


def test_a_session_opens_in_the_path_the_registry_records(
    wired: AppContext, engine: StandInEngine
) -> None:
    """The one property FR-E3 exists for.

    The engine would default the working directory to its own workspace
    root. A session that opened there when Vogt meant this project's tree
    would look right in every list and be wrong about the only thing that
    matters.
    """
    start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    assert engine.last_spec["cwd"] == ROOT


def test_a_session_can_be_opened_on_a_project(
    wired: AppContext, engine: StandInEngine
) -> None:
    result = start_session(wired, StartSessionParams(project="vogt", reason=WHY))
    assert result.session.project == "vogt"
    assert result.session.work_item is None
    assert engine.last_spec["cwd"] == ROOT


def test_a_session_needs_exactly_one_subject(wired: AppContext) -> None:
    with pytest.raises(InvalidRequest):
        start_session(wired, StartSessionParams(reason=WHY))
    with pytest.raises(InvalidRequest):
        start_session(
            wired, StartSessionParams(work_item="WI-1", project="vogt", reason=WHY)
        )


def test_a_work_item_with_no_project_has_no_tree_to_open_in(
    wired: AppContext, engine: StandInEngine
) -> None:
    """Refused rather than guessed.

    Opening in the estate root, or in the first project that happened to
    match, is exactly the heuristic FR-E3 was written against.
    """
    create_work(wired, CreateWorkParams(kind="chore", title="Unassigned", reason=WHY))
    before = len(engine.sent)
    with pytest.raises(InvalidRequest) as raised:
        start_session(wired, StartSessionParams(work_item="WI-2", reason=WHY))
    assert "belongs to no project" in str(raised.value)
    assert len(engine.sent) == before, "nothing should have been started"


# -- who the session writes as (FR-E5, FR-S10) -----------------------------


def test_the_session_carries_its_own_token(
    wired: AppContext, engine: StandInEngine
) -> None:
    """A per-session actor, so its writes are distinguishable from every other."""
    result = start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    env = engine.env_of_last_start()
    assert env["VOGT_SESSION_ID"] == result.session.id
    assert env["VOGT_HTTP_TOKEN"].startswith("vogt_")
    assert result.session.actor == f"agent:session:{result.session.id}"

    with wired.declared.read() as view:
        stored = view.token_by_hash(hash_token(env["VOGT_HTTP_TOKEN"]))
    assert stored is not None, "the token the session was given must authenticate"
    assert stored.actor_identity_ref == result.session.actor
    assert set(stored.scopes) == {"read", "work.write"}, (
        "a terminal opened on one bug may read and record work, and nothing else"
    )


def test_the_secret_is_written_nowhere(
    wired: AppContext, engine: StandInEngine
) -> None:
    """A credential in a history row is a leak with a timestamp on it.

    Checked against the rows a reader can actually get at — the audit trail
    and the event stream — rather than against the code that writes them.
    The audit row stores a digest of its payload rather than the payload, so
    the failure this guards against would arrive through the *summary*: the
    one part of a write that is stored verbatim and is easy to widen.
    """
    start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    secret = engine.env_of_last_start()["VOGT_HTTP_TOKEN"]

    with wired.declared.read() as view:
        audit = view.list_audit(limit=50)
        events = view.list_events(after=0, limit=50)
    assert audit, "the start is an audited write"
    written = json.dumps([row.model_dump(mode="json") for row in (*audit, *events)])
    assert secret not in written


def test_stopping_a_session_revokes_what_it_held(
    wired: AppContext, engine: StandInEngine
) -> None:
    result = start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    secret = engine.env_of_last_start()["VOGT_HTTP_TOKEN"]

    stop_session(wired, StopSessionParams(id=result.session.id, reason=WHY))

    assert engine.killed == [result.session.engine_session_id]
    with wired.declared.read() as view:
        token = view.token_by_hash(hash_token(secret))
    assert token is not None
    assert token.revoked_at is not None, (
        "the session is over, so what it was running with must stop working"
    )


# -- the link, and what it is not (FR-E2, FR-E4) ---------------------------


def test_the_session_is_linked_to_its_work_item(wired: AppContext) -> None:
    result = start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    listed = list_sessions(wired, ListSessionsParams(work_item="WI-1"))
    assert [row.id for row in listed.sessions] == [result.session.id]
    assert listed.sessions[0].work_item == "WI-1"
    assert listed.sessions[0].reason == WHY


def test_activity_comes_from_the_engine_not_from_storage(
    wired: AppContext, engine: StandInEngine
) -> None:
    """Liveness is read at the moment of asking, never cached.

    The engine forgetting a session is how a session ends; a stored
    "running" would outlive the process it described.
    """
    result = start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    assert list_sessions(wired, ListSessionsParams()).sessions[0].activity == "running"

    engine.alive.clear()
    after = list_sessions(wired, ListSessionsParams()).sessions[0]
    assert after.activity is None
    assert after.alive is False
    assert after.id == result.session.id, "the link survives the process"


def test_a_stopped_session_leaves_the_list(wired: AppContext) -> None:
    result = start_session(wired, StartSessionParams(project="vogt", reason=WHY))
    stop_session(wired, StopSessionParams(id=result.session.id, reason=WHY))

    assert list_sessions(wired, ListSessionsParams()).sessions == []
    kept = list_sessions(wired, ListSessionsParams(include_stopped=True))
    assert [row.id for row in kept.sessions] == [result.session.id]
    assert kept.sessions[0].stopped_at is not None


def test_stopping_an_unknown_session_is_a_not_found(wired: AppContext) -> None:
    with pytest.raises(NotFound):
        stop_session(wired, StopSessionParams(id="ses_nope", reason=WHY))


# -- an absent engine costs sessions and nothing else (FR-E9) --------------


def test_with_no_engine_configured_starting_says_so(instance: AppContext) -> None:
    register_project(
        instance, RegisterProjectParams(name="Vogt", root_path=ROOT, reason=WHY)
    )
    with pytest.raises(EngineUnavailable) as raised:
        start_session(instance, StartSessionParams(project="vogt", reason=WHY))
    assert "VOGT_ENGINE_URL" in str(raised.value), (
        "a refusal names what is missing; 'unavailable' alone reads like an outage"
    )


def test_with_no_engine_the_links_still_list(wired: AppContext) -> None:
    """Vogt's record of what it started does not depend on the engine being up."""
    result = start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    without = dataclasses.replace(wired, engine=None)

    listed = list_sessions(without, ListSessionsParams())
    assert [row.id for row in listed.sessions] == [result.session.id]
    assert listed.engine is not None and "VOGT_ENGINE_URL" in listed.engine
    assert listed.sessions[0].alive is None, (
        "unasked is not the same answer as not running"
    )


def test_a_dead_engine_is_reported_not_rendered_as_stopped(
    wired: AppContext,
) -> None:
    dead = dataclasses.replace(
        wired,
        engine=EngineClient(base_url="http://127.0.0.1:8910", transport=DeadEngine()),
    )
    start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))

    listed = list_sessions(dead, ListSessionsParams())
    assert len(listed.sessions) == 1
    assert listed.engine is not None and "not answering" in listed.engine
    assert listed.sessions[0].alive is None


def test_a_session_can_be_closed_when_the_engine_is_gone(wired: AppContext) -> None:
    """Otherwise an engine outage would strand every open session in Vogt."""
    result = start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    dead = dataclasses.replace(
        wired,
        engine=EngineClient(base_url="http://127.0.0.1:8910", transport=DeadEngine()),
    )
    stopped = stop_session(dead, StopSessionParams(id=result.session.id, reason=WHY))
    assert stopped.session.stopped_at is not None


# -- the brief the agent is handed (FR-E4) ---------------------------------


def test_the_work_items_brief_travels_with_the_session(
    wired: AppContext, engine: StandInEngine
) -> None:
    """The agent is told what it is working on, from what Vogt records.

    Sent as text, not as a path: the file it will be read from lives on the
    engine's state directory, and the engine is the process that owns that
    filesystem even when both halves share a container.
    """
    create_work(
        wired,
        CreateWorkParams(
            kind="bug",
            title="Sweep drops a page",
            body="The second page of results never arrives.",
            project="vogt",
            reason=WHY,
        ),
    )
    result = start_session(wired, StartSessionParams(work_item="WI-2", reason=WHY))

    brief = engine.last_spec["prompt"]
    assert "WI-2 — Sweep drops a page" in brief
    assert "The second page of results never arrives." in brief
    assert result.session.id in brief, "the brief says which session it belongs to"
    assert "VOGT_HTTP_TOKEN" in brief, (
        "a brief that describes the work without saying how to record what was "
        "found leaves the write capability undiscovered"
    )


def test_the_brief_carries_no_credential(
    wired: AppContext, engine: StandInEngine
) -> None:
    start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))
    brief = engine.last_spec["prompt"]
    assert engine.env_of_last_start()["VOGT_HTTP_TOKEN"] not in brief, (
        "the token is passed in the environment; a copy in a file on disk is "
        "one more place it can be read from"
    )


def test_a_project_session_is_told_no_task_was_asked_for(
    wired: AppContext, engine: StandInEngine
) -> None:
    """Vogt does not invent work for a terminal nobody attached an item to.

    Suggesting "have a look at the backlog" would be the system deciding to
    start work, which is the half of the reversed non-goal that stayed
    refused (r9).
    """
    start_session(wired, StartSessionParams(project="vogt", reason=WHY))
    brief = engine.last_spec["prompt"]
    assert "No work item is attached" in brief
    assert "backlog" not in brief.lower()


def test_the_work_item_view_shows_what_is_running_for_it(
    wired: AppContext,
) -> None:
    """FR-E4's last clause, read through the operation a client actually calls."""
    started = start_session(wired, StartSessionParams(work_item="WI-1", reason=WHY))

    item = get_work(wired, GetWorkParams(ref="WI-1"))
    assert [row.id for row in item.sessions] == [started.session.id]
    assert item.sessions[0].activity == "running"

    stop_session(wired, StopSessionParams(id=started.session.id, reason=WHY))
    assert get_work(wired, GetWorkParams(ref="WI-1")).sessions == []
