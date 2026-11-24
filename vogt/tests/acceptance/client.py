"""Standard-library HTTP, MCP-over-HTTP and WebSocket clients.

The suite must run as ``python3 tests/acceptance/run.py`` with no virtualenv,
so this file uses ``urllib`` rather than httpx and a hand-rolled RFC 6455
client rather than a websocket package. It is deliberately small: the suite
drives documented routes, not a general-purpose transport.
"""

from __future__ import annotations

import base64
import contextlib
import hashlib
import json
import os
import socket
import ssl
import struct
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass
from typing import Any

from .harness import HttpStatusError, RpcError, Token, ToolError, TransportError


@dataclass(frozen=True)
class Response:
    status: int
    headers: dict[str, str]
    body: bytes

    def json(self) -> Any:
        try:
            return json.loads(self.body.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError) as exc:
            raise TransportError(f"response is not JSON: {exc}") from None

    def text(self) -> str:
        return self.body.decode("utf-8", "replace")


class HttpClient:
    """Bearer-authenticated HTTP against one base URL."""

    def __init__(
        self, base_url: str, *, token: Token | None, timeout: float = 30.0
    ) -> None:
        self.base_url = base_url.rstrip("/")
        self.token = token
        self.timeout = timeout

    @property
    def secrets(self) -> tuple[str, ...]:
        return (self.token.value,) if self.token is not None else ()

    def request(
        self,
        method: str,
        path: str,
        *,
        body: Any | None = None,
        headers: dict[str, str] | None = None,
        token: Token | bool | None = True,
        timeout: float | None = None,
    ) -> Response:
        """One request. ``token=True`` uses the run token, ``False`` sends none."""
        url = self.base_url + (path if path.startswith("/") else "/" + path)
        data: bytes | None = None
        request_headers = dict(headers or {})
        if body is not None:
            data = json.dumps(body).encode("utf-8")
            request_headers.setdefault("Content-Type", "application/json")
        if token is True and self.token is not None:
            request_headers["Authorization"] = self.token.header()
        elif isinstance(token, Token):
            request_headers["Authorization"] = token.header()
        request = urllib.request.Request(
            url, data=data, method=method, headers=request_headers
        )
        try:
            with urllib.request.urlopen(
                request, timeout=timeout or self.timeout
            ) as answer:
                return Response(
                    status=answer.status,
                    headers={k.lower(): v for k, v in answer.headers.items()},
                    body=answer.read(),
                )
        except urllib.error.HTTPError as exc:
            return Response(
                status=exc.code,
                headers={k.lower(): v for k, v in (exc.headers or {}).items()},
                body=exc.read(),
            )
        except urllib.error.URLError as exc:
            reason = getattr(exc, "reason", exc)
            raise TransportError(f"{type(reason).__name__}: {reason}") from None
        except TimeoutError as exc:
            raise TransportError(f"timeout: {type(exc).__name__}") from None


class McpClient:
    """JSON-RPC over the streamable-HTTP MCP endpoint.

    The core answers a POST with one JSON object; the engine proxies it. Tool
    failures are a 200 with ``isError: true`` (a refusal the model is meant to
    read), while a bad credential is a 401 and an unavailable core a 502/503.
    Those are kept distinct here because the acceptance checks assert on them.
    """

    def __init__(self, http: HttpClient) -> None:
        self.http = http
        self._id = 0

    @property
    def secrets(self) -> tuple[str, ...]:
        return self.http.secrets

    def call(
        self,
        method: str,
        params: dict[str, Any] | None = None,
        *,
        token: Token | bool | None = True,
    ) -> Any:
        self._id += 1
        message = {
            "jsonrpc": "2.0",
            "id": self._id,
            "method": method,
            "params": params or {},
        }
        response = self.http.request(
            "POST",
            "/mcp",
            body=message,
            headers={"Accept": "application/json, text/event-stream"},
            token=token,
        )
        if response.status == 401:
            raise HttpStatusError(401, "POST", "/mcp", "unauthorized")
        if response.status in (502, 503, 504):
            raise HttpStatusError(
                response.status, "POST", "/mcp", response.text()[:300]
            )
        if response.status != 200:
            raise HttpStatusError(
                response.status, "POST", "/mcp", response.text()[:300]
            )
        document = response.json()
        if not isinstance(document, dict):
            raise TransportError("MCP answer is not a JSON object")
        error = document.get("error")
        if isinstance(error, dict):
            raise RpcError(
                int(error.get("code") or 0), str(error.get("message") or "error")
            )
        return document.get("result")

    def initialize(self) -> dict[str, Any]:
        result = self.call(
            "initialize",
            {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "vogt-acceptance", "version": "1"},
            },
        )
        return result if isinstance(result, dict) else {}

    def list_tools(self) -> list[dict[str, Any]]:
        result = self.call("tools/list")
        tools = result.get("tools", []) if isinstance(result, dict) else []
        return [tool for tool in tools if isinstance(tool, dict)]

    def tool(
        self, name: str, arguments: dict[str, Any] | None = None
    ) -> dict[str, Any]:
        result = self.call("tools/call", {"name": name, "arguments": arguments or {}})
        if not isinstance(result, dict):
            raise TransportError(f"{name} returned a {type(result).__name__}")
        if result.get("isError"):
            raise ToolError(_result_text(result))
        structured = result.get("structuredContent")
        if isinstance(structured, dict):
            return structured
        text = _result_text(result)
        try:
            parsed = json.loads(text)
        except json.JSONDecodeError:
            raise TransportError(f"{name} returned non-JSON text") from None
        if not isinstance(parsed, dict):
            raise TransportError(f"{name} returned a JSON {type(parsed).__name__}")
        return parsed

    def try_tool(
        self, name: str, arguments: dict[str, Any] | None = None
    ) -> tuple[dict[str, Any] | None, str | None]:
        """``(result, error)`` where a domain refusal is the error, not a raise."""
        try:
            return self.tool(name, arguments), None
        except (ToolError, RpcError, HttpStatusError) as exc:
            return None, str(exc)


def _result_text(result: dict[str, Any]) -> str:
    content = result.get("content")
    if isinstance(content, list):
        parts = [
            str(item.get("text", ""))
            for item in content
            if isinstance(item, dict) and item.get("type") == "text"
        ]
        return "\n".join(parts)
    return ""


class WebSocketClient:
    """A minimal RFC 6455 client: enough for one authenticated attach."""

    def __init__(self, sock: socket.socket) -> None:
        self.sock = sock

    @classmethod
    def connect(
        cls, url: str, *, token: Token, timeout: float = 10.0
    ) -> WebSocketClient:
        parts = urllib.parse.urlsplit(url)
        if parts.scheme not in ("ws", "wss"):
            raise TransportError(f"not a websocket URL: {parts.scheme}")
        host = parts.hostname or "127.0.0.1"
        port = parts.port or (443 if parts.scheme == "wss" else 80)
        raw = socket.create_connection((host, port), timeout=timeout)
        if parts.scheme == "wss":
            context = ssl.create_default_context()
            raw = context.wrap_socket(raw, server_hostname=host)
        key = base64.b64encode(os.urandom(16)).decode("ascii")
        path = parts.path or "/"
        if parts.query:
            path += "?" + parts.query
        request = (
            f"GET {path} HTTP/1.1\r\n"
            f"Host: {host}:{port}\r\n"
            "Upgrade: websocket\r\n"
            "Connection: Upgrade\r\n"
            f"Sec-WebSocket-Key: {key}\r\n"
            "Sec-WebSocket-Version: 13\r\n"
            f"Origin: {parts.scheme.replace('ws', 'http')}://{host}\r\n"
            f"Authorization: {token.header()}\r\n"
            "\r\n"
        )
        raw.sendall(request.encode("ascii"))
        head = b""
        while b"\r\n\r\n" not in head:
            chunk = raw.recv(4096)
            if not chunk:
                raise TransportError("websocket handshake closed")
            head += chunk
        header_text = head.split(b"\r\n\r\n", 1)[0].decode("latin-1")
        status_line = header_text.splitlines()[0] if header_text else ""
        if " 101 " not in status_line:
            raw.close()
            raise HttpStatusError(
                _status_from(status_line), "GET", path, "websocket upgrade refused"
            )
        expected = base64.b64encode(
            hashlib.sha1(
                (key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode("ascii")
            ).digest()
        ).decode("ascii")
        if f"sec-websocket-accept: {expected}".lower() not in header_text.lower():
            raw.close()
            raise TransportError("websocket accept key mismatch")
        return cls(raw)

    def send_text(self, text: str) -> None:
        payload = text.encode("utf-8")
        mask = os.urandom(4)
        header = bytearray([0x81])
        length = len(payload)
        if length < 126:
            header.append(0x80 | length)
        elif length < 65536:
            header.append(0x80 | 126)
            header.extend(struct.pack("!H", length))
        else:
            header.append(0x80 | 127)
            header.extend(struct.pack("!Q", length))
        header.extend(mask)
        masked = bytes(byte ^ mask[index % 4] for index, byte in enumerate(payload))
        self.sock.sendall(bytes(header) + masked)

    def recv_frame(self, timeout: float = 10.0) -> tuple[int, bytes]:
        self.sock.settimeout(timeout)
        first = _read_exact(self.sock, 2)
        opcode = first[0] & 0x0F
        masked = bool(first[1] & 0x80)
        length = first[1] & 0x7F
        if length == 126:
            length = struct.unpack("!H", _read_exact(self.sock, 2))[0]
        elif length == 127:
            length = struct.unpack("!Q", _read_exact(self.sock, 8))[0]
        mask = _read_exact(self.sock, 4) if masked else b""
        payload = _read_exact(self.sock, length)
        if masked:
            payload = bytes(
                byte ^ mask[index % 4] for index, byte in enumerate(payload)
            )
        return opcode, payload

    def recv_text(self, timeout: float = 10.0) -> str:
        while True:
            opcode, payload = self.recv_frame(timeout)
            if opcode == 0x8:
                raise TransportError("websocket closed")
            if opcode == 0x9:
                continue
            if opcode in (0x1, 0x2):
                return payload.decode("utf-8", "replace")
            if opcode == 0xA:
                continue

    def close(self) -> None:
        with contextlib.suppress(OSError):
            self.sock.close()

    def __enter__(self) -> WebSocketClient:
        return self

    def __exit__(self, *_exc: object) -> None:
        self.close()


def _read_exact(sock: socket.socket, count: int) -> bytes:
    data = b""
    while len(data) < count:
        chunk = sock.recv(count - len(data))
        if not chunk:
            raise TransportError("websocket closed mid-frame")
        data += chunk
    return data


def _status_from(status_line: str) -> int:
    parts = status_line.split()
    if len(parts) >= 2 and parts[1].isdigit():
        return int(parts[1])
    return 0
