"""`vogt-mcp-remote` — a stdio bridge to a remote Vogt.

Some agent products can only spawn a local process; they cannot open an HTTP
MCP session. This bridge is the shim: stdio in, streamable HTTP out.

Cadastre's biggest MCP duplication was the same twenty tool signatures
hand-mirrored across its server, its remote bridge and its registry. This
bridge **hardcodes no tools at all** — it forwards everything, and learns the
tool count from the client's own `tools/list` as it passes. There is nothing
here to drift.

Two rules from `DEPLOYMENT.md` §4.5 and DESIGN §4.1:

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
DEFAULT_TIMEOUT_SECONDS = 30
#: The budget for the optional banner read. Discovery is a courtesy — a
#: version-skew warning and a tool count on stderr — and a client gives the
#: whole handshake about 30 s, so a pre-flight that may take 30 s of it is a
#: pre-flight that turns a slow core into "server failed to connect" (#582).
DISCOVERY_TIMEOUT_SECONDS = 3

#: (url, headers, body) -> (status, body). Injected in tests; urllib live.
Transport = Callable[[str, dict[str, str], bytes], "tuple[int, bytes]"]


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
        stdin: TextIO | None = None,
        stdout: TextIO | None = None,
        stderr: TextIO | None = None,
        transport: Transport | None = None,
    ) -> None:
        self._url = url.rstrip("/")
        self._token = token
        self._stdin = stdin if stdin is not None else sys.stdin
        self._stdout = stdout if stdout is not None else sys.stdout
        self._stderr = stderr if stderr is not None else sys.stderr
        self._transport = transport
        self._discovered = False
        self._announced = False
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
        # at 200 (#24), and this line raised straight out of `main()` — the
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
        spent the client's connect budget (#582). Not stored, not filtered,
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

    def _forward(self, message: dict[str, Any]) -> dict[str, Any] | None:
        try:
            status, body = self._post(f"{self._url}/mcp", message)
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
        return headers

    def _get(
        self, url: str, *, timeout: float = DEFAULT_TIMEOUT_SECONDS
    ) -> tuple[int, bytes]:
        if self._transport is not None:
            return self._transport(url, self._headers(), b"")
        request = urllib.request.Request(url, headers=self._headers())
        return self._open(request, timeout=timeout)

    def _post(self, url: str, message: dict[str, Any]) -> tuple[int, bytes]:
        body = json.dumps(message).encode("utf-8")
        if self._transport is not None:
            return self._transport(url, self._headers(), body)
        request = urllib.request.Request(
            url, data=body, headers=self._headers(), method="POST"
        )
        return self._open(request)

    def _open(  # pragma: no cover - the live path; tests inject a transport
        self,
        request: urllib.request.Request,
        *,
        timeout: float = DEFAULT_TIMEOUT_SECONDS,
    ) -> tuple[int, bytes]:
        try:
            with urllib.request.urlopen(  # the URL comes from configuration
                request, timeout=timeout
            ) as response:
                return int(response.status), bytes(response.read())
        except urllib.error.HTTPError as exc:
            return int(exc.code), bytes(exc.read())


def read_token(path: str | None) -> str | None:
    """Read a token from a file. Never from argv or a URL (FR-S7)."""
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
    (FR-S10).

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


def main() -> int:  # pragma: no cover - exercised via the console script
    """Console-script entry point (`vogt-mcp-remote`).

    Configured entirely by environment, because that is what an MCP client
    config can set — and because a token on a command line ends up in `ps`.
    """
    url = os.environ.get(URL_ENV)
    if not url:
        sys.stderr.write(
            f"vogt-mcp-remote: set {URL_ENV} to the server's base URL "
            f"(and {TOKEN_FILE_ENV} to a file holding a token)\n"
        )
        return 2
    Bridge(url, token=resolve_token(os.environ)).serve()
    return 0


if __name__ == "__main__":  # pragma: no cover
    raise SystemExit(main())
