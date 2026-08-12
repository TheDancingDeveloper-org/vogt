"""The M1 demo, as an executable acceptance test.

From `ROADMAP.md`:

    From Claude Code via stdio MCP: create a bug, block it on another item,
    transition it, ask `backlog` and `why` — then show the same state from
    the CLI and `curl`, with identical answers, and the audit trail and event
    feed of everything the agent just did.

So the whole write half runs through the real stdio transport, exactly as an
agent would drive it, and the read half is then compared across all three
surfaces. A stage is done when its demo runs.
"""

from __future__ import annotations

import io
import json
from typing import Any

import pytest
from fastapi.testclient import TestClient

from vogt.adapters.cli.main import EXIT_OK, run
from vogt.adapters.http.app import API_PREFIX, build_app
from vogt.adapters.mcp.stdio import SUPPORTED_PROTOCOL_VERSIONS, StdioServer
from vogt.adapters.mcp.surface import McpSurface
from vogt.application.context import AppContext


class Agent:
    """A client that speaks MCP over stdio, one request at a time."""

    def __init__(self, instance: AppContext) -> None:
        self._instance = instance
        self._next_id = 0

    def call(self, method: str, /, **params: Any) -> dict[str, Any]:
        self._next_id += 1
        message = {
            "jsonrpc": "2.0",
            "id": self._next_id,
            "method": method,
            "params": params,
        }
        stdin = io.StringIO(json.dumps(message) + "\n")
        stdout = io.StringIO()
        StdioServer(
            McpSurface(context_factory=lambda: self._instance),
            stdin=stdin,
            stdout=stdout,
            stderr=io.StringIO(),
        ).serve()
        response: dict[str, Any] = json.loads(stdout.getvalue().splitlines()[0])
        assert "error" not in response, response["error"]
        result: dict[str, Any] = response["result"]
        return result

    def tool(self, tool_name: str, /, **arguments: Any) -> dict[str, Any]:
        """Positional-only name: several tools take an argument called `name`."""
        result = self.call("tools/call", name=tool_name, arguments=arguments)
        assert result["isError"] is False, result["content"]
        payload: dict[str, Any] = result["structuredContent"]
        return payload


@pytest.fixture
def agent(instance: AppContext) -> Agent:
    return Agent(instance)


def _cli(context: AppContext, *argv: str) -> Any:
    result = run(["--json", *argv], context=context)
    assert result.exit_code == EXIT_OK, result.stderr
    return json.loads(result.stdout)


def _comparable(value: Any) -> Any:
    """Round floats before comparing answers taken at different moments.

    Staleness is a function of wall-clock age, so three surfaces asked in
    succession legitimately disagree in the sixth decimal place. Rounding
    keeps the comparison about what the surfaces *say* rather than about how
    long the test took to get there.
    """
    if isinstance(value, dict):
        return {key: _comparable(item) for key, item in value.items()}
    if isinstance(value, list):
        return [_comparable(item) for item in value]
    if isinstance(value, float):
        return round(value, 3)
    return value


def test_m1_demo(instance: AppContext, agent: Agent) -> None:
    # The agent connects and discovers what it can do.
    handshake = agent.call("initialize", protocolVersion=SUPPORTED_PROTOCOL_VERSIONS[0])
    assert handshake["serverInfo"]["name"] == "vogt"
    tools = {tool["name"] for tool in agent.call("tools/list")["tools"]}
    assert {"work_create", "work_relate", "work_transition", "backlog", "why"} <= tools

    agent.tool(
        "project_register",
        name="Rust NZB",
        root_path="/srv/rustnzb",
        reason="the agent is tracking this repo",
    )

    # create a bug
    bug = agent.tool(
        "work_create",
        kind="bug",
        title="Segment fetch retries forever",
        project="rust-nzb",
        priority="p1",
        reason="observed while reading the retry loop",
    )["item"]
    assert bug["ref"] == "WI-1"
    assert bug["state"] == "open"

    # block it on another item
    blocker = agent.tool(
        "work_create",
        kind="feature",
        title="Add a retry budget",
        project="rust-nzb",
        priority="p2",
        reason="the bug cannot be fixed without this",
    )["item"]
    related = agent.tool(
        "work_relate",
        ref=bug["ref"],
        kind="depends_on",
        target=blocker["ref"],
        reason="the fix depends on the budget landing first",
    )["item"]
    assert [
        (relation["kind"], relation["related_ref"]) for relation in related["relations"]
    ] == [("depends_on", blocker["ref"])]

    # transition it
    moved = agent.tool(
        "work_transition",
        ref=bug["ref"],
        to_state="in_progress",
        reason="the agent picked this up",
    )["item"]
    assert moved["state"] == "in_progress"

    # and be refused when it tries to finish work that is still blocked
    refused = agent.call(
        "tools/call",
        name="work_transition",
        arguments={
            "ref": bug["ref"],
            "to_state": "review",
            "reason": "ready for review",
        },
    )
    assert refused["isError"] is False
    finish = agent.call(
        "tools/call",
        name="work_transition",
        arguments={
            "ref": bug["ref"],
            "to_state": "done",
            "reason": "trying to close it early",
        },
    )
    assert finish["isError"] is True
    assert "transition.blocked_by_dependency" in finish["content"][0]["text"]
    assert blocker["ref"] in finish["content"][0]["text"]

    # ask backlog and why
    ranked = agent.tool("backlog")
    assert [entry["item"]["ref"] for entry in ranked["items"]] == [
        bug["ref"],
        blocker["ref"],
    ], "the p1 bug outranks the p2 feature it depends on"

    explanation = agent.tool("why", ref=bug["ref"])
    assert explanation["total"] == pytest.approx(ranked["items"][0]["score"])
    assert [entry["input"] for entry in explanation["contributions"]] == [
        "priority",
        "staleness",
        "blocking_fan_out",
        "initiative_weight",
        "trust_penalty",
    ]

    # the same state from the CLI and from curl, with identical answers
    with TestClient(build_app(context_factory=lambda: instance)) as client:
        for argv, url, tool_name, params in (
            (["backlog"], f"{API_PREFIX}/backlog", "backlog", {}),
            (
                ["why", "--ref", bug["ref"]],
                f"{API_PREFIX}/why",
                "why",
                {"ref": bug["ref"]},
            ),
            (
                ["work", "get", "--ref", bug["ref"]],
                f"{API_PREFIX}/work/get",
                "work_get",
                {"ref": bug["ref"]},
            ),
            (
                ["project", "brief", "--slug", "rust-nzb"],
                f"{API_PREFIX}/projects/brief",
                "project_brief",
                {"slug": "rust-nzb"},
            ),
        ):
            from_cli = _comparable(_cli(instance, *argv))
            from_http = _comparable(client.get(url, params=params).json())
            from_mcp = _comparable(agent.tool(tool_name, **params))
            assert from_cli == from_http, f"CLI and REST disagree on {argv}"
            assert from_cli == from_mcp, f"CLI and MCP disagree on {argv}"

    # the audit trail and event feed of everything the agent just did
    audit = _cli(instance, "audit", "list", "--limit", "50")["records"]
    operations = [record["operation"] for record in reversed(audit)]
    assert operations == [
        "instance.init",
        "project.register",
        "work.create",
        "work.create",
        "work.relate",
        "work.transition",
        "work.transition",
    ], "every write the agent made is in the audit trail, in order"
    assert all(record["reason"].strip() for record in audit)
    assert {record["actor_identity_ref"] for record in audit} == {"local:test-user"}

    events = _cli(instance, "events", "list")["events"]
    assert [event["kind"] for event in events] == [
        "project.registered",
        "work.created",
        "work.created",
        "work.related",
        "work.transitioned",
        "work.transitioned",
    ]
    assert [event["seq"] for event in events] == list(range(1, len(events) + 1))
    # The refused transition wrote nothing: no audit row, no event, no revision.
    assert len(audit) == len(events) + 1  # +1 for the un-evented bootstrap row


def test_the_agents_own_writes_are_attributed_and_explained(
    instance: AppContext, agent: Agent
) -> None:
    """Every write records who and why — the point of an agent-first tracker."""
    agent.tool(
        "work_create", kind="chore", title="Tidy up", reason="a human asked for this"
    )
    record = _cli(instance, "audit", "list", "--limit", "1")["records"][0]
    assert record["operation"] == "work.create"
    assert record["reason"] == "a human asked for this"
    assert record["actor_identity_ref"] == "local:test-user"
    assert record["payload_digest"].startswith("sha256:")
