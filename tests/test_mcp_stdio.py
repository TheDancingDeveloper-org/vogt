"""MCP over stdio.

The transport is exercised through real streams rather than by calling
handlers, because the things most likely to break — framing, what lands on
stdout, a tool error not killing the session — only exist at that level.
"""

from __future__ import annotations

import io
import json
from typing import Any

import pytest

from vogt.adapters.mcp.stdio import (
    INVALID_PARAMS,
    METHOD_NOT_FOUND,
    PARSE_ERROR,
    SUPPORTED_PROTOCOL_VERSIONS,
    StdioServer,
    tool_success,
)
from vogt.adapters.mcp.surface import McpSurface
from vogt.application.context import AppContext


def _session(
    instance: AppContext, *messages: dict[str, Any]
) -> tuple[list[dict[str, Any]], str]:
    """Run a session over in-memory streams and return responses and stderr."""
    stdin = io.StringIO("\n".join(json.dumps(message) for message in messages) + "\n")
    stdout = io.StringIO()
    stderr = io.StringIO()
    server = StdioServer(
        McpSurface(context_factory=lambda: instance),
        stdin=stdin,
        stdout=stdout,
        stderr=stderr,
    )
    server.serve()
    lines = [line for line in stdout.getvalue().splitlines() if line.strip()]
    return [json.loads(line) for line in lines], stderr.getvalue()


def _request(message_id: int, method: str, **params: Any) -> dict[str, Any]:
    return {"jsonrpc": "2.0", "id": message_id, "method": method, "params": params}


def test_initialize_agrees_on_a_protocol_version(instance: AppContext) -> None:
    responses, _ = _session(
        instance,
        _request(1, "initialize", protocolVersion=SUPPORTED_PROTOCOL_VERSIONS[0]),
    )
    result = responses[0]["result"]
    assert result["protocolVersion"] == SUPPORTED_PROTOCOL_VERSIONS[0]
    assert result["serverInfo"]["name"] == "vogt"
    assert "tools" in result["capabilities"]


def test_an_unknown_version_negotiates_down_rather_than_refusing(
    instance: AppContext,
) -> None:
    """The handshake negotiates, it does not gate.

    A server that refuses an unrecognised version is unusable by every
    client newer than itself, which is the direction clients actually move.
    Vogt disappeared from Claude Code the day it began asking for
    2025-11-25; answering with our own newest keeps the session alive and
    leaves the choice with the client, as the MCP spec requires.
    """
    responses, _ = _session(
        instance, _request(1, "initialize", protocolVersion="2999-01-01")
    )
    assert "error" not in responses[0], responses[0]
    assert responses[0]["result"]["protocolVersion"] == SUPPORTED_PROTOCOL_VERSIONS[0]


def test_a_version_we_do_speak_is_answered_with_itself(
    instance: AppContext,
) -> None:
    """Negotiating down must not mean ignoring what was asked for."""
    older = SUPPORTED_PROTOCOL_VERSIONS[-1]
    responses, _ = _session(instance, _request(1, "initialize", protocolVersion=older))
    assert responses[0]["result"]["protocolVersion"] == older


def test_a_client_that_states_no_version_gets_the_newest(
    instance: AppContext,
) -> None:
    responses, _ = _session(instance, _request(1, "initialize"))
    assert responses[0]["result"]["protocolVersion"] == SUPPORTED_PROTOCOL_VERSIONS[0]


def test_tools_list_matches_the_registry(instance: AppContext) -> None:
    responses, _ = _session(instance, _request(1, "tools/list"))
    tools = {tool["name"] for tool in responses[0]["result"]["tools"]}
    assert "work_create" in tools
    assert "backlog" in tools
    assert "init" not in tools, "local-only operations are not MCP tools"
    assert "mcp_stdio" not in tools, "the transport does not offer itself as a tool"


def test_a_tool_call_round_trips(instance: AppContext) -> None:
    responses, _ = _session(
        instance,
        _request(
            1,
            "tools/call",
            name="work_create",
            arguments={
                "kind": "bug",
                "title": "Raised over stdio",
                "reason": "an agent filed this",
            },
        ),
        _request(2, "tools/call", name="backlog", arguments={}),
    )
    created = responses[0]["result"]
    assert created["isError"] is False
    assert created["structuredContent"]["item"]["ref"] == "WI-1"

    listed = responses[1]["result"]["structuredContent"]
    assert listed["items"][0]["title"] == "Raised over stdio"


def test_a_failing_tool_is_a_result_not_a_dead_session(
    instance: AppContext,
) -> None:
    """The model has to see the failure and be able to try something else."""
    responses, _ = _session(
        instance,
        _request(1, "tools/call", name="work_get", arguments={"ref": "WI-404"}),
        _request(2, "tools/list"),
    )
    assert responses[0]["result"]["isError"] is True
    assert "not_found" in responses[0]["result"]["content"][0]["text"]
    assert "result" in responses[1], "the session survives a failed tool call"


def test_invalid_arguments_are_reported_as_a_tool_error(
    instance: AppContext,
) -> None:
    responses, _ = _session(
        instance,
        _request(1, "tools/call", name="work_create", arguments={"kind": "bug"}),
        _request(2, "tools/list"),
    )
    assert responses[0]["result"]["isError"] is True
    assert "result" in responses[1]


def test_notifications_get_no_response(instance: AppContext) -> None:
    responses, _ = _session(
        instance,
        {"jsonrpc": "2.0", "method": "notifications/initialized"},
        _request(1, "ping"),
    )
    assert len(responses) == 1
    assert responses[0]["id"] == 1


def test_unknown_methods_are_reported(instance: AppContext) -> None:
    responses, _ = _session(instance, _request(1, "does/not/exist"))
    assert responses[0]["error"]["code"] == METHOD_NOT_FOUND


def test_malformed_json_does_not_end_the_session(instance: AppContext) -> None:
    stdin = io.StringIO('{"not json\n' + json.dumps(_request(1, "ping")) + "\n")
    stdout, stderr = io.StringIO(), io.StringIO()
    server = StdioServer(
        McpSurface(context_factory=lambda: instance),
        stdin=stdin,
        stdout=stdout,
        stderr=stderr,
    )
    report = server.serve()

    responses = [json.loads(line) for line in stdout.getvalue().splitlines() if line]
    assert responses[0]["error"]["code"] == PARSE_ERROR
    assert responses[1]["id"] == 1
    assert report.messages_handled == 2


def test_a_tool_call_needs_a_name(instance: AppContext) -> None:
    responses, _ = _session(instance, _request(1, "tools/call", arguments={}))
    assert responses[0]["error"]["code"] == INVALID_PARAMS


def test_nothing_but_protocol_reaches_stdout(instance: AppContext) -> None:
    """A stray write to stdout corrupts framing and looks like a client bug."""
    stdin = io.StringIO(json.dumps(_request(1, "tools/list")) + "\n")
    stdout, stderr = io.StringIO(), io.StringIO()
    server = StdioServer(
        McpSurface(context_factory=lambda: instance),
        stdin=stdin,
        stdout=stdout,
        stderr=stderr,
    )
    server.warn("this is a diagnostic")
    server.serve()

    for line in stdout.getvalue().splitlines():
        assert json.loads(line)["jsonrpc"] == "2.0"
    assert "this is a diagnostic" in stderr.getvalue()


def test_blank_lines_are_ignored(instance: AppContext) -> None:
    stdin = io.StringIO("\n\n" + json.dumps(_request(1, "ping")) + "\n\n")
    stdout = io.StringIO()
    server = StdioServer(
        McpSurface(context_factory=lambda: instance), stdin=stdin, stdout=stdout
    )
    assert server.serve().messages_handled == 1


@pytest.mark.parametrize(
    "message", [[1, 2, 3], {"jsonrpc": "2.0", "id": 1}], ids=["array", "no-method"]
)
def test_structurally_invalid_messages_are_rejected(
    instance: AppContext, message: Any
) -> None:
    stdin = io.StringIO(json.dumps(message) + "\n")
    stdout = io.StringIO()
    StdioServer(
        McpSurface(context_factory=lambda: instance), stdin=stdin, stdout=stdout
    ).serve()
    response = json.loads(stdout.getvalue().splitlines()[0])
    assert "error" in response


def test_a_success_carries_the_whole_result_as_compact_text() -> None:
    """C1: the text is the full result (Klaudia reads only the text when
    there is any), compact and without null keys; structuredContent keeps
    the nulls so it matches CLI and REST."""
    payload = {
        "rows": [{"id": "ses_1", "note": None, "tail": ["x" * 300]}],
        "engine": None,
        "counts": {"total": 1},
    }
    result = tool_success(payload)
    text = result["content"][0]["text"]
    assert json.loads(text) == {
        "rows": [{"id": "ses_1", "tail": ["x" * 300]}],
        "counts": {"total": 1},
    }
    assert "\n" not in text
    assert result["structuredContent"] == payload
    assert result["isError"] is False


def test_work_writes_are_compact_text_unless_verbose_wi1525(
    instance: AppContext,
) -> None:
    """WI-1525: work_create and work_transition text is {ref, state, title,
    updated_at}, under 300 characters; structuredContent stays the full
    result; `verbose: true` puts the full result in the text too."""
    create = {"kind": "bug", "title": "Compact", "reason": "compact test"}
    responses, _ = _session(
        instance,
        _request(1, "tools/call", name="work_create", arguments=create),
        _request(
            2,
            "tools/call",
            name="work_create",
            arguments={**create, "verbose": True},
        ),
        _request(
            3,
            "tools/call",
            name="work_transition",
            arguments={"ref": "WI-1", "to_state": "in_progress", "reason": "go"},
        ),
    )
    compact = responses[0]["result"]
    text = compact["content"][0]["text"]
    assert len(text) <= 300
    assert set(json.loads(text)) == {"ref", "state", "title", "updated_at"}
    assert compact["structuredContent"]["item"]["ref"] == "WI-1"

    verbose = json.loads(responses[1]["result"]["content"][0]["text"])
    assert verbose["item"]["ref"] == "WI-2"

    moved = responses[2]["result"]
    assert moved["isError"] is False, moved
    assert json.loads(moved["content"][0]["text"])["ref"] == "WI-1"


def test_work_update_and_comment_are_compact_unless_verbose_wi1525(
    instance: AppContext,
) -> None:
    """WI-1525: work_update text is {ref, state, title, updated_at};
    work_comment text is the comment's id, kind, time and write-back, not its
    body. `verbose: true` returns the full result as text."""
    create = {"kind": "bug", "title": "Compact", "reason": "compact test"}
    update = {"ref": "WI-1", "title": "Renamed", "reason": "rename"}
    comment = {"ref": "WI-1", "body": "x" * 2000, "reason": "note"}
    responses, _ = _session(
        instance,
        _request(1, "tools/call", name="work_create", arguments=create),
        _request(2, "tools/call", name="work_update", arguments=update),
        _request(
            3, "tools/call", name="work_update", arguments={**update, "verbose": True}
        ),
        _request(4, "tools/call", name="work_comment", arguments=comment),
        _request(
            5,
            "tools/call",
            name="work_comment",
            arguments={**comment, "verbose": True},
        ),
    )
    updated = responses[1]["result"]
    assert updated["isError"] is False, updated
    text = updated["content"][0]["text"]
    assert len(text) <= 300
    assert json.loads(text)["title"] == "Renamed"
    assert set(json.loads(text)) == {"ref", "state", "title", "updated_at"}
    assert "item" in json.loads(responses[2]["result"]["content"][0]["text"])

    commented = responses[3]["result"]
    assert commented["isError"] is False, commented
    text = commented["content"][0]["text"]
    assert len(text) <= 300
    assert set(json.loads(text)) == {"id", "kind", "created_at", "write_back"}
    assert commented["structuredContent"]["comment"]["body"] == "x" * 2000
    full = json.loads(responses[4]["result"]["content"][0]["text"])
    assert full["comment"]["body"] == "x" * 2000
