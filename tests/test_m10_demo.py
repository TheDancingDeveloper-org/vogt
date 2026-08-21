"""The M10 demo — the merge-MVP acceptance test.

From `ROADMAP.md`:

    Import a GitHub repo, create a work item, start an agent session on it,
    watch the agent update the work item over MCP, and read the write in the
    audit log attributed to the session's actor.

Driven end to end through the surfaces a client actually uses: the operation
registry for the human half, and the MCP tool surface — authenticated with
the token the session was handed and nothing else — for the agent half. That
second detail is the whole point of the stage. A test that called
`comment_work` directly would prove the comment lands; it would prove
nothing about whether the credential a session hands its agent can reach
Vogt, or whether what the agent then writes is attributable to that session
rather than to whoever started it.

The engine is stood in for. Its own tests cover PTYs; what is being asserted
here is the seam between the two halves.
"""

from __future__ import annotations

import dataclasses
import json
from typing import Any

import pytest

from vogt.adapters.engine import EngineClient
from vogt.adapters.mcp.surface import McpSurface
from vogt.application.context import AppContext
from vogt.application.models import (
    ForgeLinkParams,
    GetWorkParams,
    RegisterProjectParams,
    StartSessionParams,
    StopSessionParams,
)
from vogt.application.services import (
    get_work,
    link_project,
    register_project,
    start_session,
    stop_session,
)
from vogt.application.services.auth import (
    Authenticated,
    Forbidden,
    authenticate,
    authorize,
)
from vogt.core.auth import hash_token
from vogt.registry import default_registry

from tests.conftest import native_work_item

WHY = "the M10 demo"


class StandInEngine:
    """An engine that starts sessions and remembers their specs."""

    def __init__(self) -> None:
        self.specs: list[dict[str, Any]] = []

    def __call__(
        self,
        url: str,
        headers: dict[str, str],
        body: bytes = b"",
        method: str = "GET",
    ) -> tuple[int, bytes]:
        spec = json.loads(body.decode("utf-8")) if body else {}
        if method == "POST" and url.endswith("/api/sessions"):
            self.specs.append(spec)
            return 200, json.dumps(
                {
                    "id": f"eng-{len(self.specs)}",
                    "name": spec.get("name", ""),
                    "activity": "running",
                    "cwd": spec.get("cwd", ""),
                    "exit_code": None,
                }
            ).encode()
        if method == "POST" and url.endswith("/kill"):
            return 200, b'{"ok":true}'
        if method == "GET" and url.endswith("/api/sessions"):
            return 200, json.dumps(
                [
                    {
                        "id": f"eng-{index + 1}",
                        "name": spec.get("name", ""),
                        "activity": "running",
                        "cwd": spec.get("cwd", ""),
                    }
                    for index, spec in enumerate(self.specs)
                ]
            ).encode()
        return 404, b""


@pytest.fixture
def engine() -> StandInEngine:
    return StandInEngine()


@pytest.fixture
def estate(instance: AppContext, engine: StandInEngine) -> AppContext:
    return dataclasses.replace(
        instance,
        engine=EngineClient(base_url="http://127.0.0.1:8910", transport=engine),
    )


def _as_session(ctx: AppContext, secret: str) -> tuple[AppContext, Authenticated]:
    """The context an agent inside the session gets, and no more.

    Resolved through `authenticate`, the same call an adapter makes on a real
    request, rather than by looking the token up: a test that read the row
    directly would still pass if the session's token could not actually
    authenticate — which is precisely the failure worth catching.
    """
    caller = authenticate(ctx, bearer=secret)
    return dataclasses.replace(ctx, principal=caller.principal), caller


def test_the_m10_demo(
    estate: AppContext,
    engine: StandInEngine,
    tmp_path: object,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    del tmp_path
    # 1. A project, and a bug worth opening a terminal on. (`project.import`
    #    is exercised by the M7 demo against a recording cloner; what M10
    #    adds starts at the work item, so this registers the tree directly.)
    #    Registered with its repository and linked (#181): the write verbs
    #    the session drives below refuse on an unlinked project since
    #    decision 10, and the demo's estate is a forge-linked one.
    from vogt.adapters.github.client import GitHubClient

    def configured(path: Any, *, transport: Any = None) -> GitHubClient:
        del path, transport
        return GitHubClient(token="ghp_fake", transport=lambda *a, **k: (200, b"[]"))

    monkeypatch.setattr(GitHubClient, "from_token_file", staticmethod(configured))
    register_project(
        estate,
        RegisterProjectParams(
            name="Rustnzb",
            root_path="/srv/estate/rustnzb",
            repo_url="https://github.com/TheDancingDeveloper-org/rustnzb",
            reason=WHY,
        ),
    )
    link_project(estate, ForgeLinkParams(project="rustnzb", reason=WHY))
    # The item itself is the adopted/native declared shape (#183 owns their
    # migration); `work.create` on the linked project would write through
    # and hand back a subject key, and sessions open on declared refs.
    created = native_work_item(
        estate,
        kind="bug",
        title="Retries are not backing off",
        body="Second attempt fires immediately after the first.",
        project="rustnzb",
    )
    ref = created.ref

    # 2. Start a session on it. The terminal opens in the tree the registry
    #    records — not in the engine's workspace root (FR-E3).
    started = start_session(estate, StartSessionParams(work_item=ref, reason=WHY))
    spec = engine.specs[-1]
    assert spec["cwd"] == "/srv/estate/rustnzb"
    assert ref in spec["prompt"], "the agent is told which item it is here for"

    session_token = dict(spec["env"])["VOGT_HTTP_TOKEN"]

    # 3. The agent works, and records what it found — over MCP, with the
    #    token the session gave it, through the tool surface a real client
    #    calls.
    agent, caller = _as_session(estate, session_token)
    assert caller.grant.scopes == frozenset({"read", "work.write"}), (
        "a session may read and record work, and nothing else (FR-S10)"
    )
    with pytest.raises(Forbidden):
        authorize(
            agent,
            caller,
            operation="project.register",
            scope="project.write",
            mutating=True,
            transport="mcp",
        )
    surface = McpSurface(registry=default_registry(), context_factory=lambda: agent)
    answer = surface.call_tool(
        default_registry().get("work.comment").mcp_tool_name,
        {
            "ref": ref,
            "body": "Reproduced: the backoff timer is never armed.",
            "reason": "recording what the session found",
        },
    )
    assert answer["comment"]["body"].startswith("Reproduced")

    # 4. The work item shows the session that is running for it (FR-E4), and
    #    the comment the agent left.
    item = get_work(estate, GetWorkParams(ref=ref))
    assert [row.id for row in item.sessions] == [started.session.id]
    assert item.sessions[0].activity == "running"
    assert any("Reproduced" in comment.body for comment in item.comments)

    # 5. The audit log attributes the write to *this session's* actor, not to
    #    the person who started it and not to a shared agent identity. This
    #    is the assertion the stage exists for (FR-S10).
    with estate.declared.read() as view:
        trail = view.list_audit(limit=50)
    comment_rows = [row for row in trail if row.operation == "work.comment"]
    assert len(comment_rows) == 1
    assert comment_rows[0].actor_identity_ref == f"agent:session:{started.session.id}"
    assert comment_rows[0].reason == "recording what the session found"

    starter = [row for row in trail if row.operation == "session.start"]
    assert (
        starter and starter[0].actor_identity_ref != comment_rows[0].actor_identity_ref
    ), (
        "the session's writes must be distinguishable from the writes of "
        "whoever started it — that is what per-session actors are for"
    )

    # 6. Stopping the session revokes what it held: the same token that
    #    worked in step 3 no longer resolves to anything.
    stop_session(
        estate, StopSessionParams(id=started.session.id, reason="the demo is over")
    )
    with estate.declared.read() as view:
        after = view.token_by_hash(hash_token(session_token))
    assert after is not None and after.revoked_at is not None
    assert get_work(estate, GetWorkParams(ref=ref)).sessions == []
