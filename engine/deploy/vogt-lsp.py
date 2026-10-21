#!/usr/bin/env python3
"""vogt-lsp: one entry point for the agents' language servers (WI-887).

    vogt-lsp <python|rust|java>        stdio language server, for Claude Code's
                                       native LSP tool (the vogt-lsp plugin)
    vogt-lsp mcp <python|rust|java>    an MCP server over stdio for Codex, which
                                       has no LSP client of its own

Both resolve the project root from the nearest marker (docs/design/
shared-language-servers.md §4), never above the git toplevel, and start the
server with the session's leaked Python environment stripped. Each process
spawns its own servers until the hub (LSP-2) lands, so Java is off unless
VOGT_LSP_JAVA=1.

The MCP tool shapes are msp-klaudia's internal/tools/lsp_tools.go: a file plus a
1-based line and character, answers as plain text. Stdlib only: this runs from
/usr/local/bin with whatever python3 the image has.
"""

from __future__ import annotations

import hashlib
import json
import os
import queue
import shutil
import subprocess
import sys
import threading
import time
from pathlib import Path
from typing import Any
from urllib.parse import unquote, urlparse

LANGS = ("python", "rust", "java")

MARKERS: dict[str, tuple[str, ...]] = {
    "python": ("pyproject.toml", "uv.lock", "setup.py", "pyrightconfig.json"),
    "rust": ("Cargo.toml",),
    "java": (
        "settings.gradle",
        "settings.gradle.kts",
        "pom.xml",
        "build.gradle",
        "build.gradle.kts",
    ),
}

EXTENSIONS: dict[str, dict[str, str]] = {
    "python": {".py": "python", ".pyi": "python"},
    "rust": {".rs": "rust"},
    "java": {".java": "java"},
}

# Settings answered to workspace/configuration and sent as initializationOptions.
# rust-analyzer checks into target/rust-analyzer so it never holds the agent's
# build lock; basedpyright reports at "standard" unless the project configures it.
SETTINGS: dict[str, dict[str, Any]] = {
    "python": {"basedpyright": {"analysis": {"typeCheckingMode": "standard"}}},
    "rust": {
        "rust-analyzer": {
            "cargo": {"targetDir": True},
            "cachePriming": {"enable": False},
        }
    },
    "java": {},
}

SYMBOL_KINDS = [
    "",
    "file",
    "module",
    "namespace",
    "package",
    "class",
    "method",
    "property",
    "field",
    "constructor",
    "enum",
    "interface",
    "function",
    "variable",
    "constant",
    "string",
    "number",
    "boolean",
    "array",
    "object",
    "key",
    "null",
    "enum member",
    "struct",
    "event",
    "operator",
    "type parameter",
]
SEVERITIES = ["", "error", "warning", "info", "hint"]

REQUEST_TIMEOUT = float(os.environ.get("VOGT_LSP_REQUEST_TIMEOUT", "60"))
DIAGNOSTICS_WAIT = float(os.environ.get("VOGT_LSP_DIAGNOSTICS_WAIT", "10"))


def die(message: str, code: int = 2) -> None:
    print(f"vogt-lsp: {message}", file=sys.stderr)
    sys.exit(code)


# --- root and command resolution -------------------------------------------


def git_toplevel(start: Path) -> Path | None:
    try:
        out = subprocess.run(
            ["git", "-C", str(start), "rev-parse", "--show-toplevel"],
            capture_output=True,
            text=True,
            timeout=5,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    return (
        Path(out.stdout.strip()) if out.returncode == 0 and out.stdout.strip() else None
    )


def find_root(lang: str, start: Path) -> Path:
    """The nearest marker directory at or above `start`, bounded by the git
    toplevel. Rust prefers the outermost Cargo.toml that declares [workspace]."""
    start = start.resolve()
    if start.is_file():
        start = start.parent
    top = git_toplevel(start)
    found: Path | None = None
    workspace: Path | None = None
    for directory in (start, *start.parents):
        for marker in MARKERS[lang]:
            if (directory / marker).is_file():
                found = found or directory
                if lang == "rust" and "[workspace]" in (directory / marker).read_text(
                    errors="replace"
                ):
                    workspace = directory
        if top is not None and directory == top:
            break
    return workspace or found or top or start


def sanitized_env() -> dict[str, str]:
    env = dict(os.environ)
    for key in list(env):
        if key in ("VIRTUAL_ENV", "PYTHONPATH", "PYTHONHOME") or key.startswith("UV_"):
            del env[key]
    return env


def server_command(lang: str, root: Path) -> list[str]:
    if lang == "python":
        if os.environ.get("VOGT_LSP_PYTHON") == "ty" and shutil.which("ty"):
            return ["ty", "server"]
        for name in ("basedpyright-langserver", "pyright-langserver"):
            if shutil.which(name):
                return [name, "--stdio"]
        die(
            "no Python language server on PATH (basedpyright-langserver or "
            "pyright-langserver)"
        )
    if lang == "rust":
        pinned = Path("/opt/rust-analyzer/bin/rust-analyzer")
        if pinned.is_file() and os.access(pinned, os.X_OK):
            return [str(pinned)]
        if shutil.which("rust-analyzer"):
            return ["rust-analyzer"]
        die("rust-analyzer is not installed")
    if lang == "java":
        if os.environ.get("VOGT_LSP_JAVA") != "1":
            die(
                "Java is disabled until the shared hub lands (set VOGT_LSP_JAVA=1 "
                "to opt in)"
            )
        if not shutil.which("jdtls"):
            die("jdtls is not installed")
        digest = hashlib.sha256(str(root).encode()).hexdigest()
        data = Path.home() / ".cache" / "vogt-lsp" / "jdtls" / digest
        data.mkdir(parents=True, exist_ok=True)
        return ["jdtls", "-data", str(data)]
    die(f"unknown language {lang!r}")
    raise AssertionError


def run_stdio(lang: str) -> None:
    root = find_root(lang, Path.cwd())
    command = server_command(lang, root)
    env = sanitized_env()
    if lang == "java":
        env["GRADLE_OPTS"] = (
            env.get("GRADLE_OPTS", "") + " -Dorg.gradle.daemon.idletimeout=600000"
        ).strip()
    os.chdir(root)
    try:
        os.execvpe(command[0], command, env)
    except OSError as exc:
        die(f"cannot start {command[0]}: {exc}")


# --- a minimal LSP client ---------------------------------------------------


def path_uri(path: Path) -> str:
    return path.resolve().as_uri()


def uri_path(uri: str) -> str:
    return unquote(urlparse(uri).path)


class LspClient:
    def __init__(self, lang: str, root: Path, command: list[str]) -> None:
        self.lang = lang
        self.root = root
        env = sanitized_env()
        self.proc = subprocess.Popen(
            command,
            cwd=root,
            env=env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
        )
        self.lock = threading.Lock()
        self.next_id = 0
        self.pending: dict[int, queue.Queue[dict[str, Any]]] = {}
        self.diagnostics: dict[str, list[dict[str, Any]]] = {}
        self.diag_event = threading.Condition()
        self.opened: dict[str, int] = {}
        self.capabilities: dict[str, Any] = {}
        threading.Thread(target=self._read_loop, daemon=True).start()
        result = self.request(
            "initialize",
            {
                "processId": os.getpid(),
                "rootUri": path_uri(root),
                "workspaceFolders": [{"uri": path_uri(root), "name": root.name}],
                "initializationOptions": SETTINGS[lang].get(
                    next(iter(SETTINGS[lang]), ""), None
                ),
                "capabilities": {
                    "workspace": {"configuration": True, "workspaceFolders": True},
                    "textDocument": {
                        "hover": {"contentFormat": ["markdown", "plaintext"]},
                        "documentSymbol": {"hierarchicalDocumentSymbolSupport": True},
                        "publishDiagnostics": {},
                        "synchronization": {"didSave": True},
                    },
                },
            },
        )
        self.capabilities = (result or {}).get("capabilities", {})
        self.notify("initialized", {})

    def _send(self, message: dict[str, Any]) -> None:
        body = json.dumps(message).encode()
        assert self.proc.stdin is not None
        with self.lock:
            self.proc.stdin.write(b"Content-Length: %d\r\n\r\n" % len(body) + body)
            self.proc.stdin.flush()

    def _read_loop(self) -> None:
        stream = self.proc.stdout
        assert stream is not None
        while True:
            length = 0
            while True:
                line = stream.readline()
                if not line:
                    for waiter in list(self.pending.values()):
                        waiter.put({"error": {"message": "language server exited"}})
                    return
                line = line.strip()
                if not line:
                    break
                if line.lower().startswith(b"content-length:"):
                    length = int(line.split(b":", 1)[1])
            message = json.loads(stream.read(length))
            self._dispatch(message)

    def _dispatch(self, message: dict[str, Any]) -> None:
        if "method" in message and "id" in message:
            self._send(
                {"jsonrpc": "2.0", "id": message["id"], "result": self._answer(message)}
            )
        elif "method" in message:
            if message["method"] == "textDocument/publishDiagnostics":
                params = message.get("params", {})
                with self.diag_event:
                    self.diagnostics[params.get("uri", "")] = params.get(
                        "diagnostics", []
                    )
                    self.diag_event.notify_all()
        elif "id" in message:
            waiter = self.pending.pop(message["id"], None)
            if waiter is not None:
                waiter.put(message)

    def _answer(self, message: dict[str, Any]) -> Any:
        # The client answers what servers ask of it from static settings, so
        # nothing stalls waiting on an editor that is not there.
        if message["method"] == "workspace/configuration":
            out = []
            for item in message.get("params", {}).get("items", []):
                node: Any = SETTINGS[self.lang]
                for part in (item.get("section") or "").split("."):
                    if part:
                        node = node.get(part) if isinstance(node, dict) else None
                out.append(node)
            return out
        if message["method"] == "workspace/workspaceFolders":
            return [{"uri": path_uri(self.root), "name": self.root.name}]
        return None

    def request(self, method: str, params: Any) -> Any:
        with self.lock:
            self.next_id += 1
            rid = self.next_id
        waiter: queue.Queue[dict[str, Any]] = queue.Queue()
        self.pending[rid] = waiter
        self._send({"jsonrpc": "2.0", "id": rid, "method": method, "params": params})
        try:
            reply = waiter.get(timeout=REQUEST_TIMEOUT)
        except queue.Empty:
            self.pending.pop(rid, None)
            raise RuntimeError(
                f"{method} timed out after {REQUEST_TIMEOUT:g}s"
            ) from None
        if "error" in reply:
            raise RuntimeError(f"{method}: {reply['error'].get('message', 'error')}")
        return reply.get("result")

    def notify(self, method: str, params: Any) -> None:
        self._send({"jsonrpc": "2.0", "method": method, "params": params})

    def open(self, path: Path) -> str:
        """didOpen (or didChange, if already open) with the file's current text."""
        uri = path_uri(path)
        text = path.read_text(errors="replace")
        version = self.opened.get(uri, 0) + 1
        if uri in self.opened:
            self.notify(
                "textDocument/didChange",
                {
                    "textDocument": {"uri": uri, "version": version},
                    "contentChanges": [{"text": text}],
                },
            )
        else:
            self.notify(
                "textDocument/didOpen",
                {
                    "textDocument": {
                        "uri": uri,
                        "languageId": self.lang,
                        "version": version,
                        "text": text,
                    },
                },
            )
        self.opened[uri] = version
        return uri

    def close(self) -> None:
        try:
            self.request("shutdown", None)
            self.notify("exit", None)
        except (RuntimeError, OSError):
            pass
        try:
            self.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.proc.kill()


# --- the MCP bridge ---------------------------------------------------------


def text_of(contents: Any) -> str:
    if isinstance(contents, str):
        return contents
    if isinstance(contents, list):
        return "\n\n".join(text_of(c) for c in contents if text_of(c))
    if isinstance(contents, dict):
        return str(contents.get("value", ""))
    return ""


def source_line(uri: str, line: int) -> str:
    try:
        with Path(uri_path(uri)).open(encoding="utf-8", errors="replace") as fh:
            for index, row in enumerate(fh):
                if index == line:
                    return row.strip()
    except OSError:
        pass
    return ""


def locations(raw: Any) -> list[dict[str, Any]]:
    if raw is None:
        return []
    items = raw if isinstance(raw, list) else [raw]
    out = []
    for item in items:
        if "targetUri" in item:
            out.append(
                {
                    "uri": item["targetUri"],
                    "range": item.get("targetSelectionRange")
                    or item.get("targetRange"),
                }
            )
        elif "uri" in item:
            out.append(item)
    return out


def format_locations(locs: list[dict[str, Any]], what: str) -> str:
    if not locs:
        return f"No {what} found."
    rows = []
    for loc in locs:
        start = (loc.get("range") or {}).get("start", {"line": 0, "character": 0})
        row = f"{uri_path(loc['uri'])}:{start['line'] + 1}:{start['character'] + 1}"
        line = source_line(loc["uri"], start["line"])
        rows.append(f"{row}  {line}" if line else row)
    return f"{len(locs)} {what}:\n" + "\n".join(rows)


def flatten_symbols(
    items: list[dict[str, Any]], uri: str, container: str = ""
) -> list[dict[str, Any]]:
    out: list[dict[str, Any]] = []
    for item in items or []:
        if "location" in item:  # SymbolInformation
            out.append(item)
            continue
        out.append(
            {
                "name": item.get("name", ""),
                "kind": item.get("kind", 0),
                "detail": item.get("detail", ""),
                "containerName": container,
                "location": {
                    "uri": uri,
                    "range": item.get("selectionRange") or item.get("range"),
                },
            }
        )
        out.extend(flatten_symbols(item.get("children", []), uri, item.get("name", "")))
    return out


def format_symbols(symbols: list[dict[str, Any]]) -> str:
    if not symbols:
        return "No symbols found."
    rows = []
    for sym in symbols:
        kind = sym.get("kind", 0)
        label = SYMBOL_KINDS[kind] if 0 < kind < len(SYMBOL_KINDS) else "symbol"
        loc = sym.get("location") or {}
        start = (loc.get("range") or {}).get("start")
        where = uri_path(loc.get("uri", ""))
        if start:
            where += f":{start['line'] + 1}:{start['character'] + 1}"
        row = f"[{label}] {sym.get('name', '')}"
        if sym.get("detail"):
            row += f" {sym['detail']}"
        if sym.get("containerName"):
            row += f" (in {sym['containerName']})"
        rows.append(f"{row}  {where}")
    return "\n".join(rows)


FILE_PROP = {
    "type": "string",
    "description": "Path to the source file (relative to the working directory or "
    "absolute)",
}
LINE_PROP = {
    "type": "integer",
    "minimum": 1,
    "description": "1-based line number of the symbol",
}
CHAR_PROP = {
    "type": "integer",
    "minimum": 1,
    "description": "1-based column of the symbol on that line",
}
POSITION_SCHEMA = {
    "type": "object",
    "properties": {"file": FILE_PROP, "line": LINE_PROP, "character": CHAR_PROP},
    "required": ["file", "line"],
}

TOOLS = [
    {
        "name": "definition",
        "description": "Go to the definition of the symbol at a position. Line and "
        "character are 1-based.",
        "inputSchema": POSITION_SCHEMA,
    },
    {
        "name": "references",
        "description": "Find every usage of the symbol at a position. Line and "
        "character are 1-based.",
        "inputSchema": POSITION_SCHEMA,
    },
    {
        "name": "hover",
        "description": "Type, signature and documentation of the symbol at a "
        "position. Line and character are 1-based.",
        "inputSchema": POSITION_SCHEMA,
    },
    {
        "name": "document_symbols",
        "description": "List the symbols (classes, functions, fields) defined in a "
        "file.",
        "inputSchema": {
            "type": "object",
            "properties": {"file": FILE_PROP},
            "required": ["file"],
        },
    },
    {
        "name": "workspace_symbol",
        "description": "Search the project for symbols by name (servers match "
        "fuzzily).",
        "inputSchema": {
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Symbol name or fragment to search for",
                },
                "file": {
                    **FILE_PROP,
                    "description": "Optional file or directory that picks the "
                    "project; defaults to the working directory",
                },
            },
            "required": ["query"],
        },
    },
    {
        "name": "diagnostics",
        "description": "Type-check a file and list the errors and warnings the "
        "language server reports.",
        "inputSchema": {
            "type": "object",
            "properties": {"file": FILE_PROP},
            "required": ["file"],
        },
    },
]


class Bridge:
    def __init__(self, lang: str) -> None:
        self.lang = lang
        self.clients: dict[Path, LspClient] = {}

    def client_for(self, start: Path) -> LspClient:
        root = find_root(self.lang, start)
        client = self.clients.get(root)
        if client is None or client.proc.poll() is not None:
            client = LspClient(self.lang, root, server_command(self.lang, root))
            self.clients[root] = client
        return client

    def resolve_file(self, raw: Any) -> Path:
        if not isinstance(raw, str) or not raw.strip():
            raise ValueError("file is required")
        path = Path(raw).expanduser()
        path = (Path.cwd() / path) if not path.is_absolute() else path
        if not path.is_file():
            raise ValueError(f"{raw}: no such file")
        if path.suffix not in EXTENSIONS[self.lang]:
            raise ValueError(
                f"{raw}: not a {self.lang} file ({', '.join(EXTENSIONS[self.lang])})"
            )
        return path.resolve()

    def position(self, args: dict[str, Any]) -> tuple[LspClient, dict[str, Any]]:
        path = self.resolve_file(args.get("file"))
        line = args.get("line")
        if not isinstance(line, int) or line < 1:
            raise ValueError("file and a 1-based line are required")
        character = args.get("character") or 1
        client = self.client_for(path)
        uri = client.open(path)
        return client, {
            "textDocument": {"uri": uri},
            "position": {"line": line - 1, "character": int(character) - 1},
        }

    def call(self, name: str, args: dict[str, Any]) -> str:
        if name in ("definition", "references", "hover"):
            client, params = self.position(args)
            if name == "hover":
                result = client.request("textDocument/hover", params)
                text = text_of((result or {}).get("contents")).strip()
                return text or "No hover information at that position."
            if name == "references":
                params["context"] = {"includeDeclaration": True}
                return format_locations(
                    locations(client.request("textDocument/references", params)),
                    "references",
                )
            return format_locations(
                locations(client.request("textDocument/definition", params)),
                "definitions",
            )
        if name == "document_symbols":
            path = self.resolve_file(args.get("file"))
            client = self.client_for(path)
            uri = client.open(path)
            raw = client.request(
                "textDocument/documentSymbol", {"textDocument": {"uri": uri}}
            )
            return format_symbols(flatten_symbols(raw or [], uri))
        if name == "workspace_symbol":
            query = args.get("query")
            if not isinstance(query, str) or not query.strip():
                raise ValueError("query is required")
            start = Path(args["file"]).expanduser() if args.get("file") else Path.cwd()
            client = self.client_for(
                start if start.is_absolute() else Path.cwd() / start
            )
            raw = client.request("workspace/symbol", {"query": query})
            return format_symbols(flatten_symbols(raw or [], ""))
        if name == "diagnostics":
            return self.diagnostics(args)
        raise ValueError(f"unknown tool {name!r}")

    def diagnostics(self, args: dict[str, Any]) -> str:
        path = self.resolve_file(args.get("file"))
        client = self.client_for(path)
        uri = path_uri(path)
        with client.diag_event:
            client.diagnostics.pop(uri, None)
        # Reopen rather than change, so every server publishes afresh.
        if client.opened.pop(uri, None) is not None:
            client.notify("textDocument/didClose", {"textDocument": {"uri": uri}})
        client.open(path)
        deadline = time.monotonic() + DIAGNOSTICS_WAIT
        with client.diag_event:
            while uri not in client.diagnostics:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    break
                client.diag_event.wait(remaining)
            diags = client.diagnostics.get(uri)
        if diags is None:
            return f"No diagnostics reported for {path} within {DIAGNOSTICS_WAIT:g}s."
        if not diags:
            return f"No problems in {path}."
        rows = []
        for diag in diags:
            sev = diag.get("severity", 1)
            start = diag.get("range", {}).get("start", {"line": 0, "character": 0})
            label = SEVERITIES[sev] if 0 < sev < len(SEVERITIES) else "error"
            message = " ".join(str(diag.get("message", "")).split())
            rows.append(
                f"  [{label}] {start['line'] + 1}:{start['character'] + 1}  {message}"
            )
        return f"{len(diags)} problem(s) in {path}:\n" + "\n".join(rows)

    def handle(self, message: dict[str, Any]) -> dict[str, Any] | None:
        method = message.get("method")
        rid = message.get("id")
        if rid is None:
            return None  # notifications (initialized, cancelled) need no answer
        if method == "initialize":
            requested = (message.get("params") or {}).get(
                "protocolVersion", "2024-11-05"
            )
            result: Any = {
                "protocolVersion": requested,
                "capabilities": {"tools": {}},
                "serverInfo": {"name": f"vogt-lsp-{self.lang}", "version": "1"},
            }
        elif method == "ping":
            result = {}
        elif method == "tools/list":
            result = {"tools": TOOLS}
        elif method == "tools/call":
            params = message.get("params") or {}
            try:
                text = self.call(params.get("name", ""), params.get("arguments") or {})
                result = {"content": [{"type": "text", "text": text}]}
            except (ValueError, RuntimeError, OSError) as exc:
                result = {
                    "content": [{"type": "text", "text": str(exc)}],
                    "isError": True,
                }
            except SystemExit:
                result = {
                    "content": [
                        {
                            "type": "text",
                            "text": f"the {self.lang} language server is not available",
                        }
                    ],
                    "isError": True,
                }
        else:
            return {
                "jsonrpc": "2.0",
                "id": rid,
                "error": {"code": -32601, "message": f"method not found: {method}"},
            }
        return {"jsonrpc": "2.0", "id": rid, "result": result}

    def serve(self) -> None:
        # MCP stdio framing: one JSON message per line, nothing else on stdout.
        for line in sys.stdin:
            line = line.strip()
            if not line:
                continue
            try:
                message = json.loads(line)
            except json.JSONDecodeError:
                continue
            reply = self.handle(message)
            if reply is not None:
                sys.stdout.write(json.dumps(reply) + "\n")
                sys.stdout.flush()
        for client in self.clients.values():
            client.close()


def main(argv: list[str]) -> None:
    if len(argv) == 2 and argv[0] == "check" and argv[1] in LANGS:
        # Exit 0 when this language's server can start here; mcp-bootstrap
        # registers a Codex bridge only for those.
        print(" ".join(server_command(argv[1], Path.cwd())))
    elif len(argv) == 2 and argv[0] == "mcp" and argv[1] in LANGS:
        Bridge(argv[1]).serve()
    elif len(argv) == 1 and argv[0] in LANGS:
        run_stdio(argv[0])
    else:
        die(
            "usage: vogt-lsp <lang> | vogt-lsp mcp <lang> | vogt-lsp check <lang>  "
            "(lang: python, rust, java)"
        )


if __name__ == "__main__":
    main(sys.argv[1:])
