"""The `vogt-mcp-remote` stdio bridge.

The bridge hardcodes no tools: it discovers the remote's list at startup.
That is the property these tests are really protecting — cadastre's biggest
MCP duplication was twenty tool signatures hand-mirrored across a server, a
bridge and a registry, and every one of them a place to drift.
"""

from __future__ import annotations

import io
import json
from typing import Any

import pytest

from vogt.adapters.mcp.bridge import Bridge, BridgeReport, Transport, read_token

REMOTE = "https://winrarhost.example:18094"


class RecordingTransport:
    """A fake remote that remembers what it was asked."""

    def __init__(self, handler: Transport) -> None:
        self._handler = handler
        self.calls: list[dict[str, Any]] = []

    def __call__(
        self, url: str, headers: dict[str, str], body: bytes
    ) -> tuple[int, bytes]:
        self.calls.append({"url": url, "headers": headers, "body": body})
        return self._handler(url, headers, body)


def _remote(
    *,
    version: str = "0.1.0",
    tools: list[str] | None = None,
    protocol_versions: list[str] | None = None,
    status: int = 200,
    unreachable: bool = False,
) -> RecordingTransport:
    """A fake remote, so no test needs a network."""

    def transport(url: str, headers: dict[str, str], body: bytes) -> tuple[int, bytes]:
        if unreachable:
            msg = "connection refused"
            raise OSError(msg)
        if url.endswith("/connection-info"):
            return status, json.dumps(
                {
                    "version": version,
                    "supported_mcp_protocol_versions": (
                        protocol_versions
                        if protocol_versions is not None
                        else ["2025-06-18"]
                    ),
                }
            ).encode("utf-8")
        message = json.loads(body.decode("utf-8"))
        if message.get("method") == "tools/list":
            return 200, json.dumps(
                {
                    "jsonrpc": "2.0",
                    "id": message.get("id"),
                    "result": {
                        "tools": [{"name": name} for name in (tools or ["backlog"])]
                    },
                }
            ).encode("utf-8")
        return 200, json.dumps(
            {"jsonrpc": "2.0", "id": message.get("id"), "result": {"ok": True}}
        ).encode("utf-8")

    return RecordingTransport(transport)


def _run(
    transport: Transport, *messages: dict[str, Any], token: str | None = None
) -> tuple[BridgeReport, list[dict[str, Any]], str]:
    stdin = io.StringIO("\n".join(json.dumps(m) for m in messages) + "\n")
    stdout, stderr = io.StringIO(), io.StringIO()
    bridge = Bridge(
        REMOTE,
        token=token,
        stdin=stdin,
        stdout=stdout,
        stderr=stderr,
        transport=transport,
    )
    report = bridge.serve()
    responses = [json.loads(line) for line in stdout.getvalue().splitlines() if line]
    return report, responses, stderr.getvalue()


def test_the_tool_list_comes_from_the_remote() -> None:
    """Nothing is hardcoded: whatever the server offers is what is offered."""
    transport = _remote(tools=["backlog", "work_create", "something_new"])
    report, _, stderr = _run(transport, {"jsonrpc": "2.0", "id": 1, "method": "ping"})
    assert report.remote_tools == 3
    assert "3 tools available" in stderr


def test_messages_are_forwarded_verbatim() -> None:
    transport = _remote()
    _, responses, _ = _run(
        transport,
        {"jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": {"name": "x"}},
    )
    forwarded = json.loads(transport.calls[-1]["body"].decode("utf-8"))
    assert forwarded["id"] == 7
    assert forwarded["method"] == "tools/call"
    assert responses[-1]["id"] == 7


def test_the_token_is_sent_as_a_bearer_header() -> None:
    transport = _remote()
    _run(transport, {"jsonrpc": "2.0", "id": 1, "method": "ping"}, token="vogt_abc")
    assert transport.calls[0]["headers"]["Authorization"] == "Bearer vogt_abc"


def test_version_skew_warns_and_never_blocks() -> None:
    """DEPLOYMENT §4.5: one stderr line, and startup proceeds."""
    transport = _remote(version="9.9.9")
    report, responses, stderr = _run(
        transport, {"jsonrpc": "2.0", "id": 1, "method": "ping"}
    )
    assert "version skew" in stderr
    assert responses, "the session continued"
    assert report.messages_forwarded == 1


def test_no_common_protocol_version_warns_and_never_blocks() -> None:
    transport = _remote(protocol_versions=["1999-01-01"])
    _, responses, stderr = _run(
        transport, {"jsonrpc": "2.0", "id": 1, "method": "ping"}
    )
    assert "no MCP protocol version in common" in stderr
    assert responses


def test_an_unreachable_server_does_not_kill_the_bridge() -> None:
    """An agent that starts before its server should retry, not die."""
    _, responses, stderr = _run(
        _remote(unreachable=True), {"jsonrpc": "2.0", "id": 1, "method": "ping"}
    )
    assert "could not reach" in stderr
    assert responses[0]["error"]["code"] == -32000
    assert "unreachable" in responses[0]["error"]["message"]


def test_a_rejected_token_says_which_file_to_check() -> None:
    def transport(url: str, headers: dict[str, str], body: bytes) -> tuple[int, bytes]:
        if url.endswith("/connection-info"):
            return 200, json.dumps({"version": "0.1.0"}).encode("utf-8")
        return 401, b""

    _, responses, _ = _run(transport, {"jsonrpc": "2.0", "id": 1, "method": "ping"})
    assert "VOGT_TOKEN_FILE" in responses[-1]["error"]["message"]


def test_nothing_but_protocol_reaches_stdout() -> None:
    """A diagnostic on stdout corrupts framing and looks like a client bug."""
    _, responses, stderr = _run(
        _remote(version="9.9.9"), {"jsonrpc": "2.0", "id": 1, "method": "ping"}
    )
    for response in responses:
        assert response["jsonrpc"] == "2.0"
    assert stderr.strip(), "the warnings went somewhere"


def test_malformed_input_is_answered_not_fatal() -> None:
    stdin = io.StringIO('{"broken\n{"jsonrpc":"2.0","id":2,"method":"ping"}\n')
    stdout, stderr = io.StringIO(), io.StringIO()
    Bridge(
        REMOTE,
        stdin=stdin,
        stdout=stdout,
        stderr=stderr,
        transport=_remote(),
    ).serve()
    responses = [json.loads(line) for line in stdout.getvalue().splitlines() if line]
    assert responses[0]["error"]["code"] == -32700
    assert responses[1]["id"] == 2


def test_notifications_are_not_answered() -> None:
    def transport(url: str, headers: dict[str, str], body: bytes) -> tuple[int, bytes]:
        if url.endswith("/connection-info"):
            return 200, json.dumps({"version": "0.1.0"}).encode("utf-8")
        if b"tools/list" in body:
            return 200, json.dumps(
                {"jsonrpc": "2.0", "id": 0, "result": {"tools": []}}
            ).encode("utf-8")
        return 202, b""

    _, responses, _ = _run(
        transport, {"jsonrpc": "2.0", "method": "notifications/initialized"}
    )
    assert responses == []


def test_a_token_is_read_from_a_file_never_an_argument(tmp_path: Any) -> None:
    """FR-S7: a token on a command line ends up in `ps` and in history."""
    path = tmp_path / "token"
    path.write_text("vogt_secret\n", encoding="utf-8")
    assert read_token(str(path)) == "vogt_secret"
    assert read_token(None) is None
    assert read_token(str(tmp_path / "absent")) is None


def test_an_empty_token_file_reads_as_no_token(tmp_path: Any) -> None:
    path = tmp_path / "token"
    path.write_text("   \n", encoding="utf-8")
    assert read_token(str(path)) is None


@pytest.mark.parametrize("url", [REMOTE, REMOTE + "/"])
def test_a_trailing_slash_does_not_double_up(url: str) -> None:
    transport = _remote()
    message = {"jsonrpc": "2.0", "id": 1, "method": "ping"}
    stdin = io.StringIO(json.dumps(message) + "\n")
    Bridge(
        url,
        stdin=stdin,
        stdout=io.StringIO(),
        stderr=io.StringIO(),
        transport=transport,
    ).serve()
    assert all("//mcp" not in call["url"] for call in transport.calls)
