"""An in-process fake of the surfaces the acceptance suite drives.

It exists so the suite's own plumbing — redaction, refusal of production,
cleanup, MCP framing, report writing — can be proved without vogt-dev and
without a credential. It is deliberately *not* a second implementation of
Vogt: every answer here is a shape the live checks can consume, and the report
marks a fake run as such.

Run it as a library (``with FakeServer() as fake:``) or from ``run.py --fake``.
"""

from __future__ import annotations

import hashlib
import json
import secrets
import struct
import threading
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any, cast
from urllib.parse import urlsplit

from .harness import Token

GOLDEN_TOOLS = (
    Path(__file__).resolve().parents[1] / "parity" / "golden" / "mcp-tools.json"
)


class FakeState:
    """Everything the fake remembers for one run."""

    def __init__(self, *, leak_token: bool = True) -> None:
        self.admin_token = "fake-admin-" + secrets.token_hex(8)
        self.read_token = "fake-read-" + secrets.token_hex(8)
        self.leak_token = leak_token
        self.lock = threading.Lock()
        self.projects: dict[str, dict[str, Any]] = {
            "vogt": {
                "slug": "vogt",
                "name": "Vogt",
                "root_path": "/work/vogt",
                "lifecycle_state": "active",
                "write_back": "none",
                "link_state": "unlinked",
                "writable": False,
                "writable_reason": "not forge-linked",
            }
        }
        self.work: dict[str, dict[str, Any]] = {
            "WI-1": {
                "ref": "WI-1",
                "title": "Smoke",
                "kind": "chore",
                "state": "open",
                "priority": "p3",
                "project": "vogt",
                "labels": [],
                "comments": [],
                "relations": [],
                "body": "",
            }
        }
        self.next_ref = 9001
        self.sessions: dict[str, dict[str, Any]] = {}
        self.stops: list[str] = []
        self.calls: list[str] = []


def _error(message: str) -> dict[str, Any]:
    return {"isError": True, "content": [{"type": "text", "text": message}]}


def _ok(payload: dict[str, Any]) -> dict[str, Any]:
    return {
        "isError": False,
        "structuredContent": payload,
        "content": [
            {
                "type": "text",
                "text": json.dumps(payload, separators=(",", ":"), default=str),
            }
        ],
    }


def _work_public(item: dict[str, Any]) -> dict[str, Any]:
    """A WorkItem as the live surface returns it: comments are a sibling."""
    public = dict(item)
    public.pop("comments", None)
    return public


def _work_summary(item: dict[str, Any]) -> dict[str, Any]:
    """A write result as the live surface returns it: a compact summary.

    `work_create`, `work_update` and `work_transition` answer with these four
    fields, not the whole item; a caller that needs priority or labels reads
    the item back with `work_get`.
    """
    return {key: item.get(key) for key in ("ref", "state", "title", "updated_at")}


class FakeHandler(BaseHTTPRequestHandler):
    server_version = "vogt-acceptance-fake/1"
    protocol_version = "HTTP/1.1"

    state: FakeState  # set on the server instance

    # -- plumbing ------------------------------------------------------
    def log_message(self, _format: str, *args: object) -> None:
        return  # the suite's stdout is the report; the fake stays quiet

    @property
    def fake(self) -> FakeState:
        return cast(FakeState, self.server.state)  # type: ignore[attr-defined]

    def _body(self) -> bytes:
        length = int(self.headers.get("Content-Length") or 0)
        return self.rfile.read(length) if length else b""

    def _send(
        self, status: int, body: Any, *, content_type: str = "application/json"
    ) -> None:
        data = (
            body.encode("utf-8")
            if isinstance(body, str)
            else json.dumps(body).encode("utf-8")
        )
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(data)))
        self.send_header("X-Request-Id", "fake-" + secrets.token_hex(4))
        self.end_headers()
        self.wfile.write(data)

    def _bearer(self) -> str | None:
        value = self.headers.get("Authorization", "")
        return value[7:].strip() if value.startswith("Bearer ") else None

    def _scopes(self) -> str | None:
        token = self._bearer()
        if token == self.fake.admin_token:
            return "admin"
        if token == self.fake.read_token:
            return "read"
        return None

    def _auth(self) -> str | None:
        scopes = self._scopes()
        if scopes is None:
            self._send(401, {"error": "unauthorized"})
        return scopes

    def _path(self) -> str:
        return urlsplit(self.path).path

    # -- routes --------------------------------------------------------
    def do_GET(self) -> None:
        path = self._path()
        if path == "/healthz":
            self._send(200, {"ok": True})
            return
        if path == "/readyz":
            self._send(
                200,
                {
                    "ok": True,
                    "checks": [{"name": "workspace_root", "ok": True, "fatal": True}],
                },
            )
            return
        if path == "/api/config":
            self._send(
                200,
                {
                    "version": "0.7.22",
                    "product_version": "0.7.22",
                    "source_ref": "graft/batch-129",
                    "source_sha": "c481074b" * 8,
                    "features": {},
                    "session_templates": [],
                },
            )
            return
        if path == "/metrics":
            self._send(
                200, "# TYPE vogt_up gauge\nvogt_up 1\n", content_type="text/plain"
            )
            return
        if path == "/":
            self._send(
                200,
                "<!doctype html><html><head><title>Vogt</title>"
                '<link rel="manifest" href="/manifest.webmanifest"></head>'
                '<body><div id="root"></div>'
                '<script type="module" src="/assets/app.js"></script>'
                "</body></html>",
                content_type="text/html",
            )
            return
        if path == "/manifest.webmanifest":
            self._send(
                200,
                {
                    "name": "Vogt",
                    "short_name": "Vogt",
                    "icons": [{"src": "/icon-192.png", "sizes": "192x192"}],
                },
            )
            return
        if path == "/sw.js":
            self._send(
                200,
                "self.addEventListener('install', () => {});",
                content_type="text/javascript",
            )
            return
        if path == "/assets/app.js":
            self._send(200, "console.log('vogt');", content_type="text/javascript")
            return
        if path.startswith("/api/sessions/") and path.endswith("/attach"):
            self._websocket()
            return
        if path == "/api/status":
            if self._auth() is None:
                return
            self._send(200, self._status())
            return
        if path == "/api/auth/check":
            if self._auth() is None:
                return
            self._send(
                200,
                {
                    "ok": True,
                    "identity": {
                        "name": "human:fake",
                        "scopes": ["admin"],
                        "capabilities": [],
                    },
                },
            )
            return
        if path == "/api/vogt/instance/diagnostics":
            if self._auth() is None:
                return
            self._send(200, self._diagnostics())
            return
        if path == "/api/vogt/status":
            if self._auth() is None:
                return
            self._send(200, self._status())
            return
        if path.startswith("/api/"):
            if self._auth() is None:
                return
            self._send(200, {})
            return
        self._send(404, {"error": "not_found"})

    def do_POST(self) -> None:
        path = self._path()
        raw = self._body()
        if path in ("/mcp", "/mcp/"):
            if self._auth() is None:
                return
            try:
                message = json.loads(raw.decode("utf-8"))
            except (UnicodeDecodeError, json.JSONDecodeError):
                self._send(
                    200,
                    {
                        "jsonrpc": "2.0",
                        "id": None,
                        "error": {"code": -32600, "message": "invalid JSON"},
                    },
                )
                return
            self._send(200, self._mcp(message))
            return
        if path.startswith("/api/"):
            if self._auth() is None:
                return
            if self._scopes() != "admin":
                self._send(
                    403,
                    {
                        "error": "forbidden",
                        "detail": "a read-only credential cannot write",
                    },
                )
                return
            self._send(200, {"ok": True})
            return
        self._send(404, {"error": "not_found"})

    # -- payloads ------------------------------------------------------
    def _status(self) -> dict[str, Any]:
        return {
            "instance": {"id": "fake-dev-instance"},
            "source_ref": "graft/batch-129",
            "source_sha": "c481074b" * 8,
            "version": "0.7.22",
            "product_version": "0.7.22",
            "declared_schema_version": 1,
            "observed_schema_version": 1,
        }

    def _diagnostics(self) -> dict[str, Any]:
        detail = (
            f"authenticated with {self._bearer()}" if self.fake.leak_token else "ok"
        )
        return {
            "status": "ok",
            "vogt_version": "0.7.22",
            "image_digest": "sha256:" + "d" * 64,
            "instance_id": "fake-dev-instance",
            "started_at": "2026-11-12T00:00:00Z",
            "uptime_seconds": 120,
            "checks": [{"name": "engine", "status": "ok", "detail": detail}],
            "migrations": {},
            "recent_log": {"lines": []},
            "peer": {
                "configured": False,
                "url": None,
                "detail": None,
                "diagnostics": None,
            },
        }

    # -- websocket -----------------------------------------------------
    def _websocket(self) -> None:
        if self._auth() is None:
            return
        key = self.headers.get("Sec-WebSocket-Key", "")
        accept = self._ws_accept(key)
        self.send_response(101, "Switching Protocols")
        self.send_header("Upgrade", "websocket")
        self.send_header("Connection", "Upgrade")
        self.send_header("Sec-WebSocket-Accept", accept)
        self.end_headers()
        self.wfile.flush()
        try:
            opcode, _payload = self._ws_read()
            if opcode == 0x1:
                reply = json.dumps({"type": "ready", "id": "fake"}).encode("utf-8")
                self._ws_send(0x1, reply)
        finally:
            self.close_connection = True

    @staticmethod
    def _ws_accept(key: str) -> str:
        import base64

        digest = hashlib.sha1(
            (key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode("ascii")
        ).digest()
        return base64.b64encode(digest).decode("ascii")

    def _ws_read(self) -> tuple[int, bytes]:
        first = self.rfile.read(2)
        if len(first) < 2:
            return 0x8, b""
        opcode = first[0] & 0x0F
        masked = bool(first[1] & 0x80)
        length = first[1] & 0x7F
        if length == 126:
            length = struct.unpack("!H", self.rfile.read(2))[0]
        elif length == 127:
            length = struct.unpack("!Q", self.rfile.read(8))[0]
        mask = self.rfile.read(4) if masked else b""
        payload = self.rfile.read(length)
        if masked:
            payload = bytes(
                byte ^ mask[index % 4] for index, byte in enumerate(payload)
            )
        return opcode, payload

    def _ws_send(self, opcode: int, payload: bytes) -> None:
        header = bytearray([0x80 | opcode])
        if len(payload) < 126:
            header.append(len(payload))
        else:
            header.append(126)
            header.extend(struct.pack("!H", len(payload)))
        self.wfile.write(bytes(header) + payload)
        self.wfile.flush()

    # -- MCP -----------------------------------------------------------
    def _mcp(self, message: dict[str, Any]) -> dict[str, Any]:
        identifier = message.get("id")
        method = message.get("method")
        params = message.get("params") or {}
        self.fake.calls.append(str(method))
        if method == "initialize":
            return {
                "jsonrpc": "2.0",
                "id": identifier,
                "result": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {"tools": {"listChanged": False}},
                    "serverInfo": {"name": "vogt-fake", "version": "0.7.22"},
                },
            }
        if method == "ping":
            return {"jsonrpc": "2.0", "id": identifier, "result": {}}
        if method == "tools/list":
            tools = json.loads(GOLDEN_TOOLS.read_text("utf-8"))["tools"]
            scopes = self._scopes()
            if scopes != "admin":
                tools = [
                    tool
                    for tool in tools
                    if not _mutating_tool(str(tool.get("name", "")))
                ]
            return {"jsonrpc": "2.0", "id": identifier, "result": {"tools": tools}}
        if method == "tools/call":
            name = params.get("name")
            arguments = params.get("arguments") or {}
            if not isinstance(name, str):
                return _rpc_error(identifier, -32602, "tools/call needs a tool name")
            if name not in _KNOWN_TOOLS:
                return _rpc_error(identifier, -32601, f"unknown tool {name!r}")
            if _mutating_tool(name) and self._scopes() != "admin":
                return {
                    "jsonrpc": "2.0",
                    "id": identifier,
                    "result": _error("forbidden: read-only credential"),
                }
            try:
                payload = self._tool(name, arguments)
            except _ToolRefusal as exc:
                return {"jsonrpc": "2.0", "id": identifier, "result": _error(str(exc))}
            return {"jsonrpc": "2.0", "id": identifier, "result": _ok(payload)}
        return _rpc_error(identifier, -32601, f"unknown method {method!r}")

    def _tool(self, name: str, arguments: dict[str, Any]) -> dict[str, Any]:
        state = self.fake
        if name == "auth_whoami":
            return {
                "identity_ref": "human:fake",
                "kind": "human",
                "display_name": "Fake",
                "scopes": ["admin"],
            }
        if name == "status":
            return self._status()
        if name == "instance_diagnostics":
            return self._diagnostics()
        if name == "engine_status":
            return {
                "version": "0.7.22",
                "source_ref": "graft/batch-129",
                "workspace_root": "/work",
            }
        if name == "registry_dump":
            tools = json.loads(GOLDEN_TOOLS.read_text("utf-8"))["tools"]
            operations = [
                {"name": str(tool["name"]).replace("_", ".", 1)} for tool in tools
            ]
            return {"operations": operations, "count": len(operations)}
        if name == "project_list":
            return {
                "projects": list(state.projects.values()),
                "total": len(state.projects),
            }
        if name == "project_get":
            project = state.projects.get(str(arguments.get("slug")))
            if project is None:
                raise _ToolRefusal("not_found: no such project")
            return {"project": project}
        if name == "project_create":
            slug = str(arguments.get("name") or "acceptance").lower().replace(" ", "-")
            state.projects[slug] = {
                "slug": slug,
                "name": arguments.get("name"),
                "root_path": arguments.get("root_path"),
                "lifecycle_state": "incubating",
                "write_back": "none",
                "link_state": "unlinked",
                "writable": False,
                "writable_reason": "not forge-linked",
            }
            return {
                "project": state.projects[slug],
                "created_paths": [],
                "skipped_paths": [],
            }
        if name == "project_transition":
            slug = str(arguments.get("slug"))
            if slug in state.projects:
                state.projects[slug]["lifecycle_state"] = arguments.get("to_state")
                return {"project": state.projects[slug]}
            raise _ToolRefusal("not_found: no such project")
        if name == "label_create":
            return {"label": {"name": arguments.get("name")}}
        if name == "work_create":
            if arguments.get("upstream") is True:
                raise _ToolRefusal(
                    "upstream_write_refused: project is not forge-linked"
                )
            ref = f"WI-{state.next_ref}"
            state.next_ref += 1
            item = {
                "ref": ref,
                "title": arguments.get("title"),
                "kind": arguments.get("kind"),
                "state": "open",
                "priority": arguments.get("priority", "p2"),
                "project_slug": arguments.get("project"),
                "labels": list(arguments.get("labels") or []),
                "comments": [],
                "relations": [],
                "body": arguments.get("body", ""),
                "updated_at": "2026-01-01T00:00:00Z",
            }
            state.work[ref] = item
            return _work_summary(item)
        if name == "work_get":
            ref = str(arguments.get("ref") or arguments.get("id"))
            found = state.work.get(ref)
            if found is None:
                raise _ToolRefusal("not_found: no such work item")
            return {"item": _work_public(found), "comments": list(found["comments"])}
        if name == "work_list":
            query = str(arguments.get("query") or "").lower()
            items = [
                item
                for item in state.work.values()
                if not query
                or query in str(item["title"]).lower()
                or query in str(item["ref"]).lower()
            ]
            return {
                "items": [_work_public(item) for item in items],
                "total": len(items),
                "next_offset": None,
            }
        if name == "work_update":
            ref = str(arguments.get("ref"))
            found = state.work.get(ref)
            if found is None:
                raise _ToolRefusal("not_found: no such work item")
            if arguments.get("add_labels") or arguments.get("remove_labels"):
                raise _ToolRefusal(
                    "project_not_linked: work.update with labels needs a "
                    "forge-linked project"
                )
            for key in ("priority", "title", "body", "assignee"):
                if key in arguments and arguments[key] is not None:
                    found[key] = arguments[key]
            if arguments.get("add_labels"):
                found["labels"] = list(found["labels"]) + [
                    str(label) for label in arguments["add_labels"]
                ]
            return _work_summary(found)
        if name == "work_comment":
            ref = str(arguments.get("ref"))
            found = state.work.get(ref)
            if found is None:
                raise _ToolRefusal("not_found: no such work item")
            comment = {
                "kind": arguments.get("kind", "note"),
                "body": arguments.get("body"),
            }
            found["comments"].append(comment)
            return {"comment": comment}
        if name == "work_transition":
            ref = str(arguments.get("ref"))
            found = state.work.get(ref)
            if found is None:
                raise _ToolRefusal("not_found: no such work item")
            found["state"] = arguments.get("to_state")
            return _work_summary(found)
        if name == "work_relate":
            ref = str(arguments.get("ref"))
            found = state.work.get(ref)
            if found is None:
                raise _ToolRefusal("not_found: no such work item")
            found["relations"].append(
                {
                    "kind": arguments.get("kind"),
                    "related_ref": arguments.get("target"),
                }
            )
            return {"item": _work_public(found), "comments": list(found["comments"])}
        if name == "session_template_list":
            # SessionTemplateRow: name, agent, placements, default_placement.
            return {
                "templates": [
                    {
                        "name": "Codex (protected)",
                        "agent": "codex",
                        "placements": ["pod", "host"],
                        "default_placement": "pod",
                    },
                    {
                        "name": "Claude Code (protected)",
                        "agent": "claude",
                        "placements": ["pod", "host"],
                        "default_placement": "pod",
                    },
                    {
                        "name": "opencode (protected)",
                        "agent": "opencode",
                        "placements": ["pod", "host"],
                        "default_placement": "pod",
                    },
                ]
            }
        if name == "llm_provider_list":
            return {
                "providers": [
                    {
                        "name": "litellm-deepseek",
                        "base_url": "http://litellm:4000",
                        "wire_api": "responses",
                        "env_key": "DEEPSEEK_API_KEY",
                        "secret_project": "apps",
                        "secret_name": "DEEPSEEK_API_KEY",
                        "models": ["deepseek/deepseek-v4-flash"],
                        "state": "active",
                        "detail": None,
                    },
                    {
                        "name": "broken-anthropic",
                        "base_url": "http://litellm:4000",
                        "wire_api": "chat",
                        "env_key": "ANTHROPIC_API_KEY",
                        "secret_project": "apps",
                        "secret_name": "ANTHROPIC_API_KEY",
                        "models": ["claude-acceptance-probe"],
                        # Synthetic: the live engine emits active/dropped
                        # today. This state exercises P3's refusal branch.
                        "state": "secret_missing",
                        "detail": "the secret ANTHROPIC_API_KEY is not set",
                    },
                ],
                "engine": None,
            }
        if name == "usage_board":
            return {"accounts": [], "keys": []}
        if name == "forge_repos":
            return {"repos": [], "detail": "no forge credential"}
        if name == "forge_account_status":
            return {"accounts": []}
        if name == "session_start":
            session = self._start_session(arguments)
            return {"session": session}
        if name == "session_list":
            # The live result splits the modes: `digest` fills `digests` and
            # leaves `sessions` empty, `full` does the reverse. A caller that
            # reads only `sessions` therefore sees an empty live list, which
            # is exactly the mistake S18 exists to avoid.
            mode = str(arguments.get("mode") or "digest")
            if mode == "full":
                return {"sessions": list(state.sessions.values()), "digests": []}
            digests = [
                {
                    "id": session.get("id"),
                    "name": None,
                    "work_item": None,
                    "activity": session.get("activity"),
                    "attention": "unknown",
                    "ready": False,
                }
                for session in state.sessions.values()
            ]
            return {"sessions": [], "digests": digests, "total": len(digests)}
        if name == "session_sweep":
            return {
                # The live result nests the summary under `session`
                # (SessionSweepRow); mirror that so the sweep parser is
                # exercised against the shape it will really see.
                "rows": [
                    {"attention": "idle", "session": session}
                    for session in state.sessions.values()
                ],
                "counts": {"total": len(state.sessions)},
            }
        if name in ("session_wait", "session_wait_any"):
            return {"ready": True, "outcome": "ready", "matched": True}
        if name == "session_screen":
            session = self._session(arguments)
            return {
                "lines": session["screen"],
                "activity": session["activity"],
                "ready": True,
            }
        if name == "session_last_reply":
            session = self._session(arguments)
            return {"messages": [{"text": reply} for reply in session["replies"]]}
        if name == "session_input":
            session = self._session(arguments)
            text = str(arguments.get("text") or "")
            self._simulate(session, text)
            return {"delivery": "delivered", "submitted": True}
        if name == "session_hibernate":
            session = self._session(arguments)
            session["activity"] = "hibernated"
            return {"session": session}
        if name == "session_wake":
            session = self._session(arguments)
            session["activity"] = "ready"
            return {"session": session}
        if name == "session_stop":
            session = self._session(arguments)
            session["activity"] = "stopped"
            state.stops.append(str(session["id"]))
            return {"session": session}
        if name == "session_economics":
            session = self._session(arguments)
            return {
                "id": session["id"],
                "engine_session_id": session["engine_session_id"],
                "turns": len(session["replies"]),
                "context_tokens": 100,
                "usage_measured": True,
            }
        if name in _READ_TOOLS:
            return {}
        raise _ToolRefusal(f"unknown tool {name!r}")

    def _start_session(self, arguments: dict[str, Any]) -> dict[str, Any]:
        model = arguments.get("model")
        if model == "claude-acceptance-probe":
            raise _ToolRefusal("secret_missing: ANTHROPIC_API_KEY is not set")
        if model is not None and model != "deepseek/deepseek-v4-flash":
            raise _ToolRefusal(f"invalid_params: unknown model {model!r}")
        if arguments.get("placement") != "pod":
            raise _ToolRefusal("invalid placement: acceptance sessions are pod-only")
        if arguments.get("worktree") is True:
            raise _ToolRefusal("worktree=true is not the acceptance session shape")
        identifier = "ses_fake" + secrets.token_hex(4)
        session = {
            "id": identifier,
            "engine_session_id": str(uuid.uuid4()),
            "name": arguments.get("name") or "acceptance",
            "activity": "ready",
            "placement": "pod",
            "model": model,
            "worktree": False,
            "reason": arguments.get("reason"),
            "screen": ["codex: ready", "$"],
            "replies": [],
        }
        self._simulate(session, str(arguments.get("task") or ""))
        session["screen"] = (
            ["codex: ready", "$ echo PONG", "PONG"]
            if session["replies"]
            else ["codex: ready", "$"]
        )
        self.fake.sessions[identifier] = session
        return session

    def _simulate(self, session: dict[str, Any], text: str) -> None:
        lowered = text.lower()
        if "env | head" in lowered or "env|head" in lowered:
            session["replies"].append(
                "refusing: environment guard: `env` with no program would print the "
                "session's credentials; never print environment variables or tokens"
            )
            session["screen"] = ["refusing: environment guard: `env` with no program"]
            return
        if "sk-test" in lowered or "sk-acceptance" in lowered:
            session["replies"].append("output: sk-<redacted>")
            session["screen"] = ["output: sk-<redacted>"]
            return
        if "pong" in lowered:
            session["replies"].append("PONG")
            return
        if "queued" in lowered:
            session["replies"].append("QUEUED")
            return
        if "ws_scan_done" in lowered or "ws_agents" in lowered:
            session["replies"].append(
                "WS_AGENTS_OK\nWS_RULES_OK\n"
                "settings.local.json 0\nauth.json 0\n"
                ".git-credentials 0\n.netrc 0\nssh_keys 0\nWS_SCAN_DONE"
            )
            session["screen"] = ["WS_SCAN_DONE"]
            return
        if "vogt_mcp" in lowered:
            session["replies"].append("VOGT_MCP_OK")
            session["screen"] = ["VOGT_MCP_OK"]
            return
        if "graft_token" in lowered:
            session["replies"].append("GRAFT_TOKEN_MISSING")
            return
        if "cadastre" in lowered:
            session["replies"].append("CADASTRE_MISSING")
            return
        session["replies"].append("READY")

    def _session(self, arguments: dict[str, Any]) -> dict[str, Any]:
        identifier = str(arguments.get("id") or "")
        session = self.fake.sessions.get(identifier)
        if session is None:
            raise _ToolRefusal("not_found: no such session")
        return session


class _ToolRefusal(Exception):
    pass


def _rpc_error(identifier: Any, code: int, message: str) -> dict[str, Any]:
    return {
        "jsonrpc": "2.0",
        "id": identifier,
        "error": {"code": code, "message": message},
    }


def _mutating_tool(name: str) -> bool:
    return name in {
        "work_create",
        "work_update",
        "work_comment",
        "work_transition",
        "work_relate",
        "project_create",
        "project_transition",
        "project_update",
        "session_start",
        "session_input",
        "session_stop",
        "session_hibernate",
        "session_wake",
        "llm_provider_set",
    }


_KNOWN_TOOLS = {
    str(tool["name"]) for tool in json.loads(GOLDEN_TOOLS.read_text("utf-8"))["tools"]
}

_READ_TOOLS = {
    "project_list",
    "project_get",
    "work_list",
    "work_get",
    "work_get_many",
    "board_list",
    "backlog",
    "bugs",
    "label_list",
    "initiative_list",
    "actor_list",
    "workflow_list",
    "coverage",
    "observations_list",
    "drift_list",
    "inbox_list",
    "notifications",
    "deployed_versions",
    "session_list",
    "session_sweep",
    "session_template_list",
    "audit_list",
    "events_list",
    "session_changes",
    "place_metrics",
    "charter_get",
    "preference_get",
}


class FakeServer:
    """A threaded fake on an ephemeral loopback port."""

    def __init__(self, *, leak_token: bool = True) -> None:
        self.state = FakeState(leak_token=leak_token)
        self.httpd = ThreadingHTTPServer(("127.0.0.1", 0), FakeHandler)
        self.httpd.state = self.state  # type: ignore[attr-defined]
        self.thread = threading.Thread(target=self.httpd.serve_forever, daemon=True)

    @property
    def base_url(self) -> str:
        host = str(self.httpd.server_address[0])
        port = int(self.httpd.server_address[1])
        return f"http://{host}:{port}"

    @property
    def token(self) -> Token:
        return Token(self.state.admin_token)

    @property
    def read_token(self) -> Token:
        return Token(self.state.read_token)

    def __enter__(self) -> FakeServer:
        self.thread.start()
        return self

    def __exit__(self, *_exc: object) -> None:
        self.httpd.shutdown()
        self.httpd.server_close()
        self.thread.join(timeout=5)


def main() -> None:
    """Run the fake standalone, printing its URL and admin token.

    Printing a token here is deliberate and safe: it is generated for the
    process, ephemeral, and only for manual smoke-testing the suite against a
    local fake. The acceptance suite itself never prints its token.
    """
    with FakeServer() as fake:
        print(fake.base_url)
        print(fake.state.admin_token)
        try:
            threading.Event().wait()
        except KeyboardInterrupt:
            return


if __name__ == "__main__":
    main()
