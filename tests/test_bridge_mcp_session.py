"""`vogt-mcp-remote` against a streamable-HTTP server that issues sessions.

Cadastre's `/mcp` hands out an `Mcp-Session-Id` on `initialize` and answers
every later request without it with 400 "MCP request needs a valid
Mcp-Session-Id" (WI-4288). A hand-started claude behind `cadastre-mcp`
connected and then failed `tools/list`. These tests run the bridge's live
urllib path against a real HTTP server on loopback that behaves that way.
"""

from __future__ import annotations

import io
import json
import threading
from collections.abc import Iterator
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from typing import Any

import pytest

from vogt.adapters.mcp.bridge import Bridge

PROTOCOL = "2025-06-18"


class SessionServer(ThreadingHTTPServer):
    """A fake MCP server that requires the session id it issued."""

    def __init__(self) -> None:
        super().__init__(("127.0.0.1", 0), _Handler)
        self.issued: list[str] = []
        self.live: set[str] = set()
        self.seen: list[tuple[str, str | None]] = []

    def forget_sessions(self) -> None:
        """What a server restart does to every session it held."""
        self.live.clear()


class _Handler(BaseHTTPRequestHandler):
    server: SessionServer

    def log_message(self, format: str, *args: Any) -> None:
        del format, args

    def _reply(
        self, status: int, payload: Any | None, headers: dict[str, str] | None = None
    ) -> None:
        body = b"" if payload is None else json.dumps(payload).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        for name, value in (headers or {}).items():
            self.send_header(name, value)
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self) -> None:
        self._reply(404, {"error": "not found"})

    def do_POST(self) -> None:
        length = int(self.headers.get("Content-Length") or 0)
        message = json.loads(self.rfile.read(length).decode("utf-8"))
        session = self.headers.get("Mcp-Session-Id")
        self.server.seen.append((str(message.get("method")), session))
        if message.get("method") == "initialize":
            issued = f"sess-{len(self.server.issued) + 1}"
            self.server.issued.append(issued)
            self.server.live.add(issued)
            self._reply(
                200,
                {
                    "jsonrpc": "2.0",
                    "id": message.get("id"),
                    "result": {
                        "protocolVersion": PROTOCOL,
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "fake", "version": "0"},
                    },
                },
                {"Mcp-Session-Id": issued},
            )
            return
        if session not in self.server.live:
            self._reply(
                400,
                {
                    "jsonrpc": "2.0",
                    "id": message.get("id"),
                    "error": {
                        "code": -32602,
                        "message": "MCP request needs a valid Mcp-Session-Id",
                    },
                },
            )
            return
        if "id" not in message:
            self._reply(202, None)
            return
        self._reply(
            200,
            {
                "jsonrpc": "2.0",
                "id": message["id"],
                "result": {"tools": [{"name": "lookup"}, {"name": "brief"}]},
            },
        )


@pytest.fixture
def server() -> Iterator[SessionServer]:
    srv = SessionServer()
    thread = threading.Thread(target=srv.serve_forever, daemon=True)
    thread.start()
    try:
        yield srv
    finally:
        srv.shutdown()
        srv.server_close()


def _initialize(id_: int) -> dict[str, Any]:
    return {
        "jsonrpc": "2.0",
        "id": id_,
        "method": "initialize",
        "params": {
            "protocolVersion": PROTOCOL,
            "capabilities": {},
            "clientInfo": {"name": "claude-code", "version": "0"},
        },
    }


INITIALIZED = {"jsonrpc": "2.0", "method": "notifications/initialized"}


def _tools(id_: int) -> dict[str, Any]:
    return {"jsonrpc": "2.0", "id": id_, "method": "tools/list"}


class _Stdin:
    """Lines handed out one at a time, with a hook between them."""

    def __init__(self, steps: list[Any]) -> None:
        self._steps = steps

    def __iter__(self) -> Iterator[str]:
        for step in self._steps:
            if callable(step):
                step()
                continue
            yield json.dumps(step) + "\n"


def _serve(server: SessionServer, *steps: Any) -> list[dict[str, Any]]:
    stdout, stderr = io.StringIO(), io.StringIO()
    url = f"http://127.0.0.1:{server.server_address[1]}"
    Bridge(
        url,
        token="t",
        stdin=_Stdin(list(steps)),  # type: ignore[arg-type]
        stdout=stdout,
        stderr=stderr,
    ).serve()
    return [json.loads(line) for line in stdout.getvalue().splitlines() if line]


def test_the_session_id_from_initialize_is_sent_on_every_later_request(
    server: SessionServer,
) -> None:
    responses = _serve(server, _initialize(1), INITIALIZED, _tools(2))
    assert [r["id"] for r in responses] == [1, 2]
    assert "error" not in responses[1], responses[1]
    assert [t["name"] for t in responses[1]["result"]["tools"]] == ["lookup", "brief"]
    assert server.seen == [
        ("initialize", None),
        ("notifications/initialized", "sess-1"),
        ("tools/list", "sess-1"),
    ]


def test_a_client_reinitialize_starts_a_fresh_session(server: SessionServer) -> None:
    responses = _serve(
        server, _initialize(1), INITIALIZED, _initialize(2), INITIALIZED, _tools(3)
    )
    assert "error" not in responses[-1], responses[-1]
    assert server.seen[2] == ("initialize", None), (
        "a new initialize must not present the old session"
    )
    assert server.seen[-1] == ("tools/list", "sess-2")


def test_a_session_the_server_forgot_is_reestablished_once(
    server: SessionServer,
) -> None:
    responses = _serve(
        server,
        _initialize(1),
        INITIALIZED,
        _tools(2),
        server.forget_sessions,
        _tools(3),
    )
    assert responses[-1]["id"] == 3
    assert "error" not in responses[-1], responses[-1]
    assert [r["id"] for r in responses] == [1, 2, 3], (
        "the replayed initialize is the bridge's, not the client's; "
        "its answer must not reach stdout"
    )
    assert server.seen[3:] == [
        ("tools/list", "sess-1"),
        ("initialize", None),
        ("notifications/initialized", "sess-2"),
        ("tools/list", "sess-2"),
    ]


def test_a_server_that_issues_no_session_gets_no_header(
    server: SessionServer,
) -> None:
    """Vogt's own `/mcp` is stateless: nothing to capture, nothing to send."""
    stdout = io.StringIO()
    seen: list[dict[str, str]] = []

    def transport(url: str, headers: dict[str, str], body: bytes) -> tuple[int, bytes]:
        seen.append(dict(headers))
        message = json.loads(body or b"{}")
        return 200, json.dumps(
            {"jsonrpc": "2.0", "id": message.get("id"), "result": {}}
        ).encode("utf-8")

    Bridge(
        "http://unused",
        stdin=_Stdin([_initialize(1), _tools(2)]),  # type: ignore[arg-type]
        stdout=stdout,
        stderr=io.StringIO(),
        transport=transport,
    ).serve()
    assert all("Mcp-Session-Id" not in h for h in seen)
