"""`vogt-mcp-remote` — a stdio bridge to a remote Vogt.

Some agent products can only spawn a local process; they cannot open an HTTP
MCP session. This bridge is the shim: stdio in, streamable HTTP out.

An earlier product's biggest MCP duplication was the same twenty tool signatures
hand-mirrored across its server, its remote bridge and its registry. This
bridge **hardcodes no tools at all** — it forwards everything, and learns the
tool count from the client's own `tools/list` as it passes. There is nothing
here to drift.

Two rules from `DEPLOYMENT.md` and ARCHITECTURE.md:

- **Version skew warns; it never blocks.** One line on stderr, and startup
  proceeds. A bridge that refuses to start because the server is a patch
  ahead is a bridge that turns a warning into an outage.
- **stdout is the MCP framing channel.** Every diagnostic goes to stderr.
  A warning printed to stdout corrupts the stream and looks like a client
  bug.
"""

from __future__ import annotations

import json
import os
import sys
import urllib.error
import urllib.request
from collections.abc import Callable, Mapping
from dataclasses import dataclass
from pathlib import Path
from typing import Any, TextIO

from vogt import __version__
from vogt.adapters.mcp.stdio import SUPPORTED_PROTOCOL_VERSIONS

URL_ENV = "VOGT_URL"
TOKEN_FILE_ENV = "VOGT_TOKEN_FILE"
#: Set by a coding session for the agent it starts, and by the container's
#: auth broker for everything else. See `resolve_token` for which wins.
HTTP_TOKEN_ENV = "VOGT_HTTP_TOKEN"
#: Set by the engine on every session's process (`pty.rs`).
ENGINE_SESSION_ENV = "VOGT_ENGINE_SESSION_ID"
#: `vogt.application.identity.HEADER_ENGINE_SESSION`, spelled out: the
#: bridge ships to clients and imports nothing from the application.
ENGINE_SESSION_HEADER = "X-Vogt-Engine-Session"
DEFAULT_TIMEOUT_SECONDS = 30
#: A tools/call that itself waits (`timeout_s` / `wake_timeout_s`) needs the
#: HTTP read to outlast that wait. The bridge adds the default on top and
#: never holds a connection longer than this.
MAX_CALL_TIMEOUT_SECONDS = 660
#: The budget for the optional banner read. Discovery is a courtesy — a
#: version-skew warning and a tool count on stderr — and a client gives the
#: whole handshake about 30 s, so a pre-flight that may take 30 s of it is a
#: pre-flight that turns a slow core into "server failed to connect".
DISCOVERY_TIMEOUT_SECONDS = 3

#: The streamable-HTTP session header. A server that issues one on
#: `initialize` refuses every later request that does not send it back.
MCP_SESSION_HEADER = "Mcp-Session-Id"

#: (url, headers, body) -> (status, body) or (status, body, response headers).
#: Injected in tests; urllib live.
Transport = Callable[
    [str, dict[str, str], bytes],
    "tuple[int, bytes] | tuple[int, bytes, Mapping[str, str]]",
]


@dataclass
class BridgeReport:
    """What one bridge session did."""

    messages_forwarded: int = 0
    remote_tools: int = 0
    remote_version: str | None = None
    warned: list[str] | None = None


class Bridge:
    """Forwards newline-delimited JSON-RPC between stdio and a remote."""

    def __init__(
        self,
        url: str,
        *,
        token: str | None = None,
        engine_session: str | None = None,
        stdin: TextIO | None = None,
        stdout: TextIO | None = None,
        stderr: TextIO | None = None,
        transport: Transport | None = None,
    ) -> None:
        self._url = url.rstrip("/")
        self._token = token
        self._engine_session = (engine_session or "").strip() or None
        self._stdin = stdin if stdin is not None else sys.stdin
        self._stdout = stdout if stdout is not None else sys.stdout
        self._stderr = stderr if stderr is not None else sys.stderr
        self._transport = transport
        self._discovered = False
        self._announced = False
        #: The session the server issued on `initialize`, if it issues one,
        #: and the client's `initialize` that got it, for re-establishing a
        #: session the server has forgotten.
        self._mcp_session: str | None = None
        self._initialize: dict[str, Any] | None = None
        self.report = BridgeReport(warned=[])

    # -- diagnostics -------------------------------------------------------

    def warn(self, text: str) -> None:
        """One line, on stderr, never fatal."""
        if self.report.warned is not None:
            self.report.warned.append(text)
        self._stderr.write(f"vogt-mcp-remote: {text}\n")
        self._stderr.flush()

    # -- startup -----------------------------------------------------------

    def discover(self) -> None:
        """Ask the remote what it is and what it offers.

        Failures here are warnings, not exits: an agent that starts before
        its server is up should reconnect on the first real call rather than
        die at launch.
        """
        try:
            status, body = self._get(
                f"{self._url}/connection-info", timeout=DISCOVERY_TIMEOUT_SECONDS
            )
        except OSError as exc:
            self.warn(f"could not reach {self._url}: {exc}")
            return
        if status != 200:
            self.warn(f"{self._url}/connection-info returned {status}")
            return

        # A 200 is not a promise that the body is ours. Behind the merged
        # front door, `/connection-info` is answered by the PWA's index.html
        # at 200, and this line raised straight out of `main()` — the
        # bridge died at launch over a banner it did not need, while the
        # `/mcp` endpoint two lines below was answering `initialize` and
        # `tools/list` perfectly (#25). Discovery is a pre-flight; the same
        # rule the two guards above follow applies here.
        try:
            info = json.loads(body.decode("utf-8"))
        except (UnicodeDecodeError, json.JSONDecodeError):
            self.warn(
                f"{self._url}/connection-info returned 200 but not JSON; "
                "skipping discovery and forwarding anyway"
            )
            return
        if not isinstance(info, dict):
            self.warn(
                f"{self._url}/connection-info returned JSON that is not an "
                "object; skipping discovery and forwarding anyway"
            )
            return

        self.report.remote_version = str(info.get("version", ""))
        if self.report.remote_version and self.report.remote_version != __version__:
            self.warn(
                f"version skew: bridge {__version__}, server "
                f"{self.report.remote_version}. Continuing."
            )
        remote_versions = info.get("supported_mcp_protocol_versions") or []
        if remote_versions and not set(remote_versions) & set(
            SUPPORTED_PROTOCOL_VERSIONS
        ):
            self.warn(
                "no MCP protocol version in common: bridge supports "
                f"{', '.join(SUPPORTED_PROTOCOL_VERSIONS)}, server supports "
                f"{', '.join(str(v) for v in remote_versions)}. Continuing."
            )

    def _note_tools(self, message: dict[str, Any], response: dict[str, Any]) -> None:
        """Learn the tool count from the client's own `tools/list`.

        The bridge used to ask for the list itself at startup, before it had
        read a byte of stdin. Every client sends `tools/list` right after
        `initialize` anyway, so the pre-flight was a second copy of a call
        that was about to happen — and, behind a slow core, the copy that
        spent the client's connect budget. Not stored, not filtered,
        not remembered: the bridge forwards whatever the server accepts.
        Caching the list here is exactly how a bridge starts lying about
        what a server can do.
        """
        if message.get("method") != "tools/list" or "result" not in response:
            return
        tools = response["result"].get("tools", [])
        self.report.remote_tools = len(tools)
        if not self._announced:
            self._announced = True
            self.warn(f"connected: {len(tools)} tools available")

    # -- the loop ----------------------------------------------------------

    def serve(self) -> BridgeReport:
        """Forward stdin to the remote until stdin closes.

        The client's first message — its `initialize` — is answered before
        discovery runs. Discovery is a courtesy on stderr; the handshake is
        the contract, and a bridge that pre-flights for 30 s before reading
        stdin is a bridge that fails to connect precisely when the core is
        slow, which is when an operator most needs the tools that say why.
        """
        for line in self._stdin:
            stripped = line.strip()
            if not stripped:
                continue
            self.report.messages_forwarded += 1
            try:
                message = json.loads(stripped)
            except json.JSONDecodeError as exc:
                self._write(
                    {
                        "jsonrpc": "2.0",
                        "id": None,
                        "error": {"code": -32700, "message": f"invalid JSON: {exc}"},
                    }
                )
                continue
            response = self._forward(message)
            if response is not None:
                self._note_tools(message, response)
                self._write(response)
            if not self._discovered:
                self._discovered = True
                self.discover()
        return self.report

    def _exchange(
        self, message: dict[str, Any]
    ) -> tuple[int, bytes, Mapping[str, str]]:
        """POST one message, keeping the server's MCP session (WI-4288).

        A streamable-HTTP server may issue an `Mcp-Session-Id` on
        `initialize` and refuse every later request without it (Cadastre
        does, with 400). Vogt's own `/mcp` issues none, and then nothing is
        sent. A client `initialize` starts over: the old session is not
        presented. A session the server no longer knows (404, or 400 naming
        the header — a restart) is re-established once by replaying the
        client's `initialize` and `notifications/initialized`; their answers
        are the bridge's and never reach stdout.
        """
        is_initialize = message.get("method") == "initialize"
        if is_initialize:
            self._mcp_session = None
            self._initialize = message
        sent_session = self._mcp_session
        status, body, headers = self._post(f"{self._url}/mcp", message)
        if is_initialize:
            self._capture_session(status, headers)
        elif (
            sent_session is not None
            and self._initialize is not None
            and _session_refused(status, body)
            and self._reinitialize(self._initialize)
            # A tool call is never replayed: the refusal is the server's
            # word, not proof the call did not run. The next request uses
            # the fresh session.
            and message.get("method") != "tools/call"
        ):
            status, body, headers = self._post(f"{self._url}/mcp", message)
        return status, body, headers

    def _capture_session(self, status: int, headers: Mapping[str, str]) -> str | None:
        """Keep the session an `initialize` answer issued, if it issued one."""
        if status != 200:
            return None
        for name, value in headers.items():
            if name.lower() == MCP_SESSION_HEADER.lower() and value.strip():
                self._mcp_session = value.strip()
                return self._mcp_session
        return None

    def _reinitialize(self, initialize: dict[str, Any]) -> bool:
        """Open a fresh server session for a client that already has one."""
        self._mcp_session = None
        status, _, headers = self._post(f"{self._url}/mcp", initialize)
        if self._capture_session(status, headers) is None:
            return False
        self.warn("the server forgot this MCP session; started a new one")
        self._post(
            f"{self._url}/mcp",
            {"jsonrpc": "2.0", "method": "notifications/initialized"},
        )
        return True

    def _forward(self, message: dict[str, Any]) -> dict[str, Any] | None:
        try:
            status, body, _ = self._exchange(message)
        except OSError as exc:
            if message.get("id") is None:
                return None
            return {
                "jsonrpc": "2.0",
                "id": message.get("id"),
                "error": {"code": -32000, "message": f"vogt unreachable: {exc}"},
            }
        # Checked before the empty-body case: a 401 often has no body, and
        # treating it as "no response" leaves the client waiting forever for
        # an answer to a question that was refused.
        if status in (401, 403):
            return {
                "jsonrpc": "2.0",
                "id": message.get("id"),
                "error": {
                    "code": -32001,
                    "message": (
                        "the server rejected this token; check VOGT_TOKEN_FILE"
                    ),
                },
            }
        if status == 202 or not body.strip():
            return None
        parsed: dict[str, Any] = json.loads(body.decode("utf-8"))
        return parsed

    def _write(self, message: dict[str, Any]) -> None:
        self._stdout.write(json.dumps(message) + "\n")
        self._stdout.flush()

    # -- transport ---------------------------------------------------------

    def _headers(self) -> dict[str, str]:
        headers = {"Content-Type": "application/json", "Accept": "application/json"}
        if self._token:
            headers["Authorization"] = f"Bearer {self._token}"
        if self._engine_session:
            # Which engine session this bridge runs in (WI-1852). Advisory:
            # the core uses it only to attribute a pod-token caller's
            # session.start, after the engine vouches for the session.
            headers[ENGINE_SESSION_HEADER] = self._engine_session
        if self._mcp_session:
            headers[MCP_SESSION_HEADER] = self._mcp_session
        return headers

    def _get(
        self, url: str, *, timeout: float = DEFAULT_TIMEOUT_SECONDS
    ) -> tuple[int, bytes]:
        if self._transport is not None:
            status, body, _ = _with_headers(self._transport(url, self._headers(), b""))
            return status, body
        request = urllib.request.Request(url, headers=self._headers())
        status, body, _ = _with_headers(self._open(request, timeout=timeout))
        return status, body

    def _post(
        self, url: str, message: dict[str, Any]
    ) -> tuple[int, bytes, Mapping[str, str]]:
        body = json.dumps(message).encode("utf-8")
        timeout = call_timeout(message)
        if self._transport is not None:
            return _with_headers(self._transport(url, self._headers(), body))
        request = urllib.request.Request(
            url, data=body, headers=self._headers(), method="POST"
        )
        return _with_headers(self._open(request, timeout=timeout))

    def _open(  # pragma: no cover - the live path; tests inject a transport
        self,
        request: urllib.request.Request,
        *,
        timeout: float = DEFAULT_TIMEOUT_SECONDS,
    ) -> tuple[int, bytes, Mapping[str, str]]:
        try:
            with urllib.request.urlopen(  # the URL comes from configuration
                request, timeout=timeout
            ) as response:
                return (
                    int(response.status),
                    bytes(response.read()),
                    dict(response.headers.items()),
                )
        except urllib.error.HTTPError as exc:
            return int(exc.code), bytes(exc.read()), dict(exc.headers.items())


def _with_headers(
    result: tuple[int, bytes] | tuple[int, bytes, Mapping[str, str]],
) -> tuple[int, bytes, Mapping[str, str]]:
    """A transport may answer without headers; that is no session."""
    if len(result) == 3:
        return result
    status, body = result[0], result[1]
    return status, body, {}


#: The JSON-RPC error Cadastre answers a request with no live session.
SESSION_REFUSED_CODE = -32602
SESSION_REFUSED_PREFIX = "mcp request needs a valid mcp-session-id"


def _session_refused(status: int, body: bytes) -> bool:
    """The server clearly says it no longer knows the session presented.

    The spec answers 404 for an expired session. Cadastre answers 400 with the
    JSON-RPC error -32602 "MCP request needs a valid Mcp-Session-Id". A 400
    whose body only mentions the header in free text is not that signal.
    """
    if status == 404:
        return True
    if status != 400:
        return False
    try:
        error = json.loads(body).get("error")
    except (ValueError, AttributeError):
        return False
    if not isinstance(error, dict) or error.get("code") != SESSION_REFUSED_CODE:
        return False
    message = error.get("message")
    return isinstance(message, str) and message.lower().startswith(
        SESSION_REFUSED_PREFIX
    )


def call_timeout(message: Mapping[str, Any]) -> float:
    """How long `_post` may wait for this JSON-RPC message.

    A `tools/call` whose arguments name `timeout_s` or `wake_timeout_s` as an
    int waits that long plus the default, capped at
    `MAX_CALL_TIMEOUT_SECONDS`. Anything else — including a non-int — keeps
    the default, and never raises.
    """
    if message.get("method") != "tools/call":
        return float(DEFAULT_TIMEOUT_SECONDS)
    params = message.get("params")
    arguments = params.get("arguments") if isinstance(params, Mapping) else None
    if not isinstance(arguments, Mapping):
        return float(DEFAULT_TIMEOUT_SECONDS)
    for key in ("timeout_s", "wake_timeout_s"):
        value = arguments.get(key)
        if isinstance(value, bool) or not isinstance(value, int):
            continue
        return float(min(value + DEFAULT_TIMEOUT_SECONDS, MAX_CALL_TIMEOUT_SECONDS))
    return float(DEFAULT_TIMEOUT_SECONDS)


def read_token(path: str | None) -> str | None:
    """Read a token from a file. Never from argv or a URL."""
    if not path:
        return None
    resolved = Path(path).expanduser()
    if not resolved.is_file():
        return None
    return resolved.read_text(encoding="utf-8").strip() or None


def resolve_token(env: Mapping[str, str]) -> str | None:
    """The token this bridge should present, and which one wins.

     Two sources, because two things provision one. A container brokers a
     shared token into a *file* and points `VOGT_TOKEN_FILE` at it; a coding
     session hands its own token to the process it starts, in
     `VOGT_HTTP_TOKEN`, bound to an actor that exists for that session alone
    .

     Inside a session the session's token wins. It has to: the whole point of
     minting one is that what the agent writes is attributable to *this*
     session, and falling back to the shared container token would file every
     session's work under one identity while looking like it worked. Outside
     a session there is no `VOGT_SESSION_ID`, the file is the only source,
     and nothing changes.
    """
    if env.get("VOGT_SESSION_ID") and env.get(HTTP_TOKEN_ENV):
        return env[HTTP_TOKEN_ENV].strip() or None
    from_file = read_token(env.get(TOKEN_FILE_ENV))
    if from_file:
        return from_file
    return (env.get(HTTP_TOKEN_ENV) or "").strip() or None


def startup_problem(env: Mapping[str, str]) -> str | None:
    """Why this bridge cannot work, or None when it can start.

    No URL or no token means every request would be refused. Serving anyway
    makes the client see a connected server whose `initialize` fails, or a
    silent close (gh#1426); refusing here puts the reason on stderr where
    the client's log shows it.
    """
    if not env.get(URL_ENV):
        return (
            f"set {URL_ENV} to the server's base URL "
            f"(and {TOKEN_FILE_ENV} to a file holding a token)"
        )
    if resolve_token(env) is None:
        return (
            f"no Vogt token: {HTTP_TOKEN_ENV} and {TOKEN_FILE_ENV} are unset or empty. "
            "Start the shell as a Vogt session, or set VOGT_TOKEN_FILE"
        )
    return None


def main() -> int:  # pragma: no cover - exercised via the console script
    """Console-script entry point (`vogt-mcp-remote`).

    Configured entirely by environment, because that is what an MCP client
    config can set — and because a token on a command line ends up in `ps`.
    """
    problem = startup_problem(os.environ)
    if problem:
        sys.stderr.write(f"vogt-mcp-remote: {problem}\n")
        return 2
    url = os.environ[URL_ENV]
    Bridge(
        url,
        token=resolve_token(os.environ),
        engine_session=os.environ.get(ENGINE_SESSION_ENV),
    ).serve()
    return 0


if __name__ == "__main__":  # pragma: no cover
    raise SystemExit(main())
