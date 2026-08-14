"""MCP over stdio (FR-A5).

The local transport: an agent spawns this process, talks JSON-RPC over
newline-delimited stdin/stdout, and reaches the same operations the CLI and
REST surfaces expose. No server is required and the data directory is the
one the CLI uses.

Three rules this file exists to keep:

- **stdout is the framing channel.** Nothing but protocol messages goes
  there — a stray `print` corrupts the stream and the failure looks like a
  client bug. Diagnostics go to stderr (DESIGN §4.1, `DEPLOYMENT.md` §4.5).
- **Unsupported protocol versions are refused with the supported list
  named** (FR-A6). A client that guesses wrong gets told what to guess.
- **Identity is never a message field.** The principal comes from the
  process's own authentication — here, the OS user — so no `initialize`
  parameter can claim to be somebody else (FR-S2).
"""

from __future__ import annotations

import json
import sys
from dataclasses import dataclass
from typing import Any, TextIO

from vogt import __version__
from vogt.adapters.mcp.surface import McpSurface
from vogt.errors import VogtError

#: A JSON-RPC id is a string, a number, or absent. Typing it precisely keeps
#: the error helpers from accepting anything at all.
MessageId = str | int | None

#: Newest first. The head is what the server offers when it gets to choose.
SUPPORTED_PROTOCOL_VERSIONS: tuple[str, ...] = (
    "2025-06-18",
    "2025-03-26",
    "2024-11-05",
)


def negotiate_protocol_version(requested: object) -> str:
    """Answer with a version both sides can speak (FR-A6, revised r8).

    The MCP handshake is a negotiation, not a gate: a server that does not
    recognise the requested version must answer with one it *does* support
    and let the client decide whether to continue. Refusing instead makes
    the server unusable by every client newer than itself, which is the
    normal direction of drift — and it is how Vogt became unreachable from
    Claude Code the day that client moved to 2025-11-25, while the estate's
    other MCP server carried on.

    The original reading of FR-A6 ("refuse with the supported list named")
    is kept where it belongs: the list still travels, in the response, as
    the version actually chosen.
    """
    if isinstance(requested, str) and requested in SUPPORTED_PROTOCOL_VERSIONS:
        return requested
    return SUPPORTED_PROTOCOL_VERSIONS[0]


SERVER_NAME = "vogt"

PARSE_ERROR = -32700
INVALID_REQUEST = -32600
METHOD_NOT_FOUND = -32601
INVALID_PARAMS = -32602


@dataclass
class ServeReport:
    """What one stdio session did."""

    messages_handled: int = 0
    protocol_version: str | None = None


class StdioServer:
    """A JSON-RPC 2.0 server speaking MCP over two text streams."""

    def __init__(
        self,
        surface: McpSurface | None = None,
        *,
        stdin: TextIO | None = None,
        stdout: TextIO | None = None,
        stderr: TextIO | None = None,
    ) -> None:
        self._surface = surface if surface is not None else McpSurface()
        self._stdin = stdin if stdin is not None else sys.stdin
        self._stdout = stdout if stdout is not None else sys.stdout
        self._stderr = stderr if stderr is not None else sys.stderr
        self.report = ServeReport()

    # -- loop --------------------------------------------------------------

    def serve(self) -> ServeReport:
        """Read messages until the stream closes."""
        for line in self._stdin:
            stripped = line.strip()
            if not stripped:
                continue
            self.report.messages_handled += 1
            response = self._handle_line(stripped)
            if response is not None:
                self._write(response)
        return self.report

    def _write(self, message: dict[str, Any]) -> None:
        self._stdout.write(json.dumps(message) + "\n")
        self._stdout.flush()

    def warn(self, text: str) -> None:
        """Diagnostics go to stderr, never to the framing channel."""
        self._stderr.write(f"vogt-mcp: {text}\n")
        self._stderr.flush()

    # -- dispatch ----------------------------------------------------------

    def _handle_line(self, line: str) -> dict[str, Any] | None:
        try:
            message = json.loads(line)
        except json.JSONDecodeError as exc:
            return _error(None, PARSE_ERROR, f"invalid JSON: {exc}")
        if not isinstance(message, dict):
            return _error(None, INVALID_REQUEST, "message must be a JSON object")
        return self.handle(message)

    def handle(self, message: dict[str, Any]) -> dict[str, Any] | None:
        """Handle one parsed message, returning a response or None."""
        raw_id = message.get("id")
        message_id: MessageId = raw_id if isinstance(raw_id, (str, int)) else None
        method = message.get("method")
        params = message.get("params") or {}
        if not isinstance(method, str):
            return _error(message_id, INVALID_REQUEST, "missing method")
        if not isinstance(params, dict):
            return _error(message_id, INVALID_PARAMS, "params must be an object")

        # Notifications carry no id and get no response, by protocol.
        if message_id is None:
            if method == "notifications/initialized":
                return None
            return None

        if method == "initialize":
            return self._initialize(message_id, params)
        if method == "ping":
            return _result(message_id, {})
        if method == "tools/list":
            return _result(
                message_id,
                {"tools": [tool.to_wire() for tool in self._surface.list_tools()]},
            )
        if method == "tools/call":
            return self._call(message_id, params)
        return _error(message_id, METHOD_NOT_FOUND, f"unknown method {method!r}")

    def _initialize(
        self, message_id: MessageId, params: dict[str, Any]
    ) -> dict[str, Any]:
        version = negotiate_protocol_version(params.get("protocolVersion"))
        self.report.protocol_version = version
        return _result(
            message_id,
            {
                "protocolVersion": version,
                "capabilities": {"tools": {"listChanged": False}},
                "serverInfo": {"name": SERVER_NAME, "version": __version__},
            },
        )

    def _call(self, message_id: MessageId, params: dict[str, Any]) -> dict[str, Any]:
        name = params.get("name")
        if not isinstance(name, str):
            return _error(message_id, INVALID_PARAMS, "tools/call needs a tool name")
        arguments = params.get("arguments") or {}
        if not isinstance(arguments, dict):
            return _error(message_id, INVALID_PARAMS, "arguments must be an object")
        try:
            payload = self._surface.call_tool(name, arguments)
        except VogtError as exc:
            # A failed tool call is a *result* with isError, not a protocol
            # error: the model is meant to see it and try something else.
            return _result(message_id, _tool_error(exc.code, str(exc)))
        except Exception as exc:  # a tool must not kill the session
            return _result(message_id, _tool_error("error", str(exc)))
        return _result(
            message_id,
            {
                "content": [{"type": "text", "text": json.dumps(payload, indent=2)}],
                "structuredContent": payload,
                "isError": False,
            },
        )


def _result(message_id: MessageId, result: dict[str, Any]) -> dict[str, Any]:
    return {"jsonrpc": "2.0", "id": message_id, "result": result}


def _error(
    message_id: MessageId,
    code: int,
    message: str,
    *,
    data: dict[str, Any] | None = None,
) -> dict[str, Any]:
    error: dict[str, Any] = {"code": code, "message": message}
    if data is not None:
        error["data"] = data
    return {"jsonrpc": "2.0", "id": message_id, "error": error}


def _tool_error(code: str, message: str) -> dict[str, Any]:
    return {
        "content": [{"type": "text", "text": f"{code}: {message}"}],
        "isError": True,
    }


def main() -> int:
    """Console-script entry point (`vogt-mcp`).

    This is the command an MCP client is configured to spawn. It is also
    reachable as `vogt mcp stdio`; both start the same server, and having the
    bare binary means a client config is one word rather than a quoted
    argument list.
    """
    StdioServer().serve()
    return 0


if __name__ == "__main__":  # pragma: no cover - exercised via the console script
    raise SystemExit(main())
