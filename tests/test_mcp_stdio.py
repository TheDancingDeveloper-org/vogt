"""MCP over stdio (FR-A5, FR-A6).

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


def test_an_unsupported_version_is_refused_with_the_supported_list(
    instance: AppContext,
) -> None:
    """FR-A6: a client that guesses wrong is told what to guess."""
    responses, _ = _session(
        instance, _request(1, "initialize", protocolVersion="1999-01-01")
    )
    error = responses[0]["error"]
    assert error["code"] == INVALID_PARAMS
    assert error["data"]["supported"] == list(SUPPORTED_PROTOCOL_VERSIONS)


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
    assert listed["items"][0]["item"]["title"] == "Raised over stdio"


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
