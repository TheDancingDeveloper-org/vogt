#!/usr/bin/env python3
"""Black-box parity between two implementations of the Vogt core.

Drives a recorded script through a real `vogt` (or `vogt-core`) process —
CLI today, HTTP and MCP once those drivers are asked for — and either writes
the normalised answers (`record`) or diffs them against a previous recording
(`check`).

    scripts/parity.py record --impl python --out tests/parity/golden/<sha>
    scripts/parity.py check  --impl python --golden tests/parity/golden/<sha>
    scripts/parity.py check  --impl python --golden <dir> --only actor

`--impl python` resolves `VOGT_PARITY_PYTHON_BIN` (a `vogt` executable) and
`--impl rust` resolves `VOGT_PARITY_RUST_BIN` (a `vogt-core` executable).
Each run sets `VOGT_TEST_CLOCK_START` and `VOGT_TEST_IDS=sequential` so two
fresh instances given the same script produce the same identifiers and
timestamps. Exit status is non-zero when any step differs.
"""

from __future__ import annotations

import argparse
import contextlib
import difflib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from collections.abc import Iterator
from datetime import UTC, datetime
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any

REPO = Path(__file__).resolve().parents[1]
SCRIPT_PATH = REPO / "tests" / "parity" / "script.json"
RULES_PATH = REPO / "tests" / "parity" / "normalise.toml"
CLOCK_START = "2026-01-02T03:04:05+00:00"

BINS = {
    "python": ("VOGT_PARITY_PYTHON_BIN", "vogt"),
    "rust": ("VOGT_PARITY_RUST_BIN", "vogt-core"),
}


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)
    for name in ("record", "check"):
        cmd = sub.add_parser(name)
        cmd.add_argument("--impl", choices=sorted(BINS), required=True)
        cmd.add_argument("--only", default="", help="operation prefix, e.g. actor")
        cmd.add_argument("--transport", default="cli", choices=["cli", "http", "mcp"])
        if name == "record":
            cmd.add_argument("--out", type=Path, required=True)
        else:
            cmd.add_argument("--golden", type=Path, required=True)
    probe = sub.add_parser("selftest")
    probe.add_argument("--golden", type=Path, required=True)
    args = parser.parse_args(argv)

    if args.command == "selftest":
        return _selftest(args.golden)

    binary = _resolve(args.impl)
    # `--only dump` runs the whole script and keeps only the dumps: a dump
    # step reads what the sweeps wrote, and a script cut down to the dumps
    # would dump an empty observed store.
    steps = _load_steps("" if args.only == "dump" else args.only)
    if not steps and args.only != "dump":
        print(f"no steps match --only {args.only!r}", file=sys.stderr)
        return 2
    recorded = _run(binary, steps, args.transport)

    if args.command == "record":
        args.out.mkdir(parents=True, exist_ok=True)
        target = args.out / f"{args.transport}.json"
        target.write_text(json.dumps(recorded, indent=2, sort_keys=True) + "\n")
        meta = {
            "main_sha": _merge_base(),
            "recorded_at": datetime.now(UTC).strftime("%Y-%m-%dT%H:%M:%SZ"),
            "recorder": "scripts/parity.py",
        }
        (args.out / "meta.json").write_text(
            json.dumps(meta, indent=2, sort_keys=True) + "\n"
        )
        print(f"recorded {len(recorded)} steps to {target}")
        return 0

    golden = json.loads((args.golden / f"{args.transport}.json").read_text())
    if args.only == "dump":
        golden = [step for step in golden if step["operation"].startswith("dump.")]
        # A dump run executes the whole script so each mid-script dump sees the
        # state the steps before it left; only the dumps are compared.
        recorded = [step for step in recorded if step["operation"].startswith("dump.")]
    elif args.only:
        golden = [step for step in golden if step["operation"].startswith(args.only)]
        # A CLI run always records dump.after_init first, even when the script
        # was filtered. The prefix applies to the script, so the baseline dump
        # is not part of what --only asks to compare.
        recorded = [
            step for step in recorded if step["operation"].startswith(args.only)
        ]
    return _diff(golden, recorded, args.only)


def _merge_base() -> str:
    """The main SHA this tree was rebased onto, which names the golden."""
    completed = subprocess.run(
        ["git", "merge-base", "HEAD", "origin/main"],
        cwd=REPO,
        capture_output=True,
        text=True,
        check=False,
    )
    if completed.returncode != 0:
        return "unknown"
    return completed.stdout.strip()


def _resolve(impl: str) -> str:
    env_name, fallback = BINS[impl]
    binary = os.environ.get(env_name) or shutil.which(fallback)
    if binary is None:
        raise SystemExit(f"{env_name} is unset and {fallback!r} is not on PATH")
    return binary


def _load_steps(prefix: str) -> list[dict[str, Any]]:
    document = json.loads(SCRIPT_PATH.read_text())
    if prefix == "dump":
        # The dumps sit between the steps that write the tables they show, so
        # a dump-only run has to execute the script up to them.
        return list(document["steps"])
    return [step for step in document["steps"] if step["operation"].startswith(prefix)]


def _prepare_init(root: Path, env: dict[str, str]) -> dict[str, str]:
    """Write the init pre-step's files and return the env `init` runs with.

    The script's `init` block names the bootstrap token files, and any
    fixture a step needs before it runs (a repository whose `.git` is broken,
    for one). A name may sit in a subdirectory, which is created. Every
    transport runs `init` with the same block, so the adoption shows up in the
    after-init dump of each one.
    """
    spec = json.loads(SCRIPT_PATH.read_text()).get("init", {})
    for name, content in spec.get("files", {}).items():
        path = root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(content.replace("{root}", str(root)), encoding="utf-8")
    init_env = dict(env)
    for key, value in spec.get("env", {}).items():
        init_env[key] = value.replace("{root}", str(root))
    return init_env


def _run(
    binary: str, steps: list[dict[str, Any]], transport: str
) -> list[dict[str, Any]]:
    if transport == "http":
        return _run_http(binary, steps)
    if transport == "mcp":
        return _run_mcp(binary, steps)
    return _run_cli(binary, steps)


@contextlib.contextmanager
def _fixed_scratch(name: str) -> Iterator[Path]:
    """A scratch directory at a fixed path, cleared before and after use."""
    path = Path(tempfile.gettempdir()) / name
    shutil.rmtree(path, ignore_errors=True)
    path.mkdir(parents=True)
    try:
        yield path
    finally:
        shutil.rmtree(path, ignore_errors=True)


class _McpStdio:
    """One `mcp stdio` process, initialised, with its JSON-RPC call counter.

    The environment is fixed per process. A step that names a stand-in engine
    needs `VOGT_ENGINE_URL` in the child's environment, so the runner starts a
    fresh process for it. The data directory is shared, so the state carries.
    """

    def __init__(self, binary: str, data: Path, env: dict[str, str]) -> None:
        self.server = subprocess.Popen(
            [binary, "--data-dir", str(data), "mcp", "stdio"],
            env=env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        self.call_id = 0
        initialised = self.request(
            "initialize",
            {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "parity", "version": "0"},
            },
        )
        if "error" in initialised:
            raise SystemExit(f"mcp initialize failed: {initialised['error']}")
        assert self.server.stdin is not None
        self.server.stdin.write(
            json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n"
        )
        self.server.stdin.flush()

    def request(self, method: str, params: dict[str, Any] | None = None) -> Any:
        self.call_id += 1
        message: dict[str, Any] = {
            "jsonrpc": "2.0",
            "id": self.call_id,
            "method": method,
        }
        if params is not None:
            message["params"] = params
        assert self.server.stdin is not None and self.server.stdout is not None
        self.server.stdin.write(json.dumps(message) + "\n")
        self.server.stdin.flush()
        line = self.server.stdout.readline()
        if not line:
            reason = (
                self.server.stderr.read() if self.server.stderr else ""
            ) or "closed"
            raise SystemExit(f"mcp stdio closed on {method}: {reason}")
        reply = json.loads(line)
        if reply.get("id") != self.call_id:
            raise SystemExit(f"mcp reply id {reply.get('id')} for call {self.call_id}")
        return reply

    def close(self) -> None:
        if self.server.stdin is not None:
            self.server.stdin.close()
        self.server.terminate()
        self.server.wait(timeout=10)


def _run_mcp(binary: str, steps: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Drive the steps that take parameters over MCP stdio.

    The binary serves `mcp stdio` on a fresh instance, and each step with
    `params` is one `tools/call`. The tool is the operation with dots as
    underscores, which is the registry's `mcp_tool` for every operation here.
    The arguments are the step's params, the same model the CLI reads. Steps
    without params are CLI-only or HTTP-only and are skipped.

    A step with `"engine": true` runs against the same loopback stand-in engine
    the CLI run uses, through `VOGT_ENGINE_URL` in the server's environment.
    Every other step runs with it unset, so the runner restarts the server when
    the flag changes. A `{seen:...}` reference reads an earlier answer, as the
    CLI run does, so a grant decision names the grant the request made.

    Every answer is recorded, including an `isError` result, so a refusal the
    two cores word differently fails the comparison. The after-init dump is
    recorded first, as the CLI run does. No final dump: the session token that
    the HTTP login mints is random, and nothing in this transport captures it.
    """
    steps = [step for step in steps if "params" in step or "dump" in step]
    # The scratch root is fixed, not random: the audit payload digest covers the
    # absolute root_path a project was registered at, so a random directory
    # made every MCP digest differ from run to run. The output still shows it as
    # <root>. Runs are sequential, so one fixed path is safe to clear and reuse.
    with _fixed_scratch("vogt-parity-mcp-fixed") as scratch:
        root = Path(scratch)
        data = root / "instance"
        env = _child_env()
        init = subprocess.run(
            [binary, "--json", "--data-dir", str(data), "init"],
            env=_prepare_init(root, env),
            capture_output=True,
            text=True,
            check=False,
        )
        if init.returncode != 0:
            raise SystemExit(f"init exited {init.returncode}: {init.stderr.strip()}")
        recorded: list[dict[str, Any]] = [
            {
                "operation": "dump.after_init",
                "body": _normalise(_dump(data), root, data),
            }
        ]
        seen: dict[str, Any] = {}
        engine = _StandInEngine()
        engine_url = engine.__enter__()
        server: _McpStdio | None = None
        on_engine = False
        try:
            for step in steps:
                if "dump" in step:
                    recorded.append(
                        {
                            "operation": step["operation"],
                            "body": _dump(data, _dump_names(step)),
                        }
                    )
                    continue
                wants_engine = bool(step.get("engine"))
                if server is None or wants_engine != on_engine:
                    if server is not None:
                        server.close()
                    step_env = (
                        {**env, "VOGT_ENGINE_URL": engine_url} if wants_engine else env
                    )
                    server = _McpStdio(binary, data, step_env)
                    on_engine = wants_engine
                arguments = _substitute(step["params"], seen, root)
                tool = step.get("mcp_tool", step["operation"].replace(".", "_"))
                reply = server.request(
                    "tools/call", {"name": tool, "arguments": arguments}
                )
                if "error" in reply:
                    answer: dict[str, Any] = {
                        "operation": step["operation"],
                        "rpc_error": _normalise(
                            reply["error"], root, data, step["operation"]
                        ),
                    }
                else:
                    result = reply["result"]
                    text = "".join(
                        item.get("text", "") for item in result.get("content", [])
                    )
                    try:
                        body: Any = json.loads(text)
                    except json.JSONDecodeError:
                        body = {"text": text}
                    seen[step["operation"]] = body
                    answer = {
                        "operation": step["operation"],
                        "is_error": bool(result.get("isError", False)),
                        "body": _normalise(body, root, data, step["operation"]),
                    }
                recorded.append(answer)
        finally:
            if server is not None:
                server.close()
            engine.__exit__(None, None, None)
    return recorded


def _child_env() -> dict[str, str]:
    """The environment a recorded binary runs in, the same on every machine.

    Every inherited `VOGT_*` variable is dropped. A session or a pod exports
    several (`VOGT_CORE_URL`, `VOGT_ENGINE_URL`, `VOGT_DATA_DIR`, ...), and
    each one changes what the binary answers: an engine URL that resolves on
    one host and not on another turned `engine_detail` into a statement about
    the runner's DNS. With none of them set, both cores read the defaults the
    contract is about, and the hooks below are the only `VOGT_*` they see.
    """
    env = {
        key: value for key, value in os.environ.items() if not key.startswith("VOGT_")
    }
    env.update(
        {
            "VOGT_TEST_CLOCK_START": CLOCK_START,
            "VOGT_TEST_IDS": "sequential",
            # The local principal is `local:$USER`. Pinning it keeps a golden
            # recorded by one account comparable to a run under another,
            # including CI's `runner` user.
            "USER": "parity",
            "LOGNAME": "parity",
        }
    )
    return env


class _StandInHandler(BaseHTTPRequestHandler):
    """The engine routes the grant steps reach: every session answers alive as
    a worker, a grant apply is echoed back, and a revoke is acknowledged."""

    def _reply(self, status: int, payload: Any) -> None:
        body = json.dumps(payload).encode() if payload is not None else b""
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _parts(self) -> list[str]:
        return self.path.split("?", 1)[0].strip("/").split("/")

    def do_GET(self) -> None:
        parts = self._parts()
        if parts[:2] == ["api", "sessions"] and len(parts) == 3:
            return self._reply(
                200,
                {
                    "id": parts[2],
                    "name": parts[2],
                    "activity": "idle",
                    "alive": True,
                    "exit_code": None,
                    "role": "worker",
                    "cwd": "/work",
                },
            )
        self._reply(404, None)

    def do_POST(self) -> None:
        parts = self._parts()
        length = int(self.headers.get("Content-Length") or 0)
        payload = json.loads(self.rfile.read(length) or b"{}")
        if (
            len(parts) == 4
            and parts[:2] == ["api", "sessions"]
            and parts[3] == "grants"
        ):
            return self._reply(200, payload)
        self._reply(404, None)

    def do_DELETE(self) -> None:
        parts = self._parts()
        if (
            len(parts) == 5
            and parts[:2] == ["api", "sessions"]
            and parts[3] == "grants"
        ):
            return self._reply(200, {"revoked": True})
        self._reply(404, None)

    def log_message(self, *args: Any) -> None:
        return


class _StandInEngine:
    """A loopback stand-in for the session engine, for the steps that set
    `"engine": true`. Every other step runs with VOGT_ENGINE_URL unset."""

    def __enter__(self) -> str:
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), _StandInHandler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        return f"http://127.0.0.1:{self.server.server_address[1]}"

    def __exit__(self, *exc: object) -> None:
        self.server.shutdown()
        self.server.server_close()


def _run_cli(binary: str, steps: list[dict[str, Any]]) -> list[dict[str, Any]]:
    root = Path(tempfile.mkdtemp(prefix="vogt-parity-"))
    data = root / "instance"
    env = _child_env()
    answers: list[dict[str, Any]] = []
    seen: dict[str, Any] = {}
    engine = _StandInEngine()
    engine_url = engine.__enter__()
    try:
        root.mkdir(parents=True, exist_ok=True)
        init = subprocess.run(
            [binary, "--json", "--data-dir", str(data), "init"],
            env=_prepare_init(root, env),
            capture_output=True,
            text=True,
            check=False,
        )
        if init.returncode != 0:
            raise SystemExit(f"init exited {init.returncode}: {init.stderr.strip()}")
        answers.append(
            {
                "operation": "dump.after_init",
                "exit": 0,
                "body": _dump(data),
            }
        )
        for step in steps:
            # A dump step records tables at this point in the script, so a
            # later step cannot hide a change an earlier one wrote. `true`
            # dumps both stores; a store name dumps just that one.
            if step.get("dump"):
                body = _dump(data, _dump_names(step))
                _blank_payload_digests(body)
                answers.append(
                    {"operation": step["operation"], "exit": 0, "body": body}
                )
                continue
            if "params" not in step:
                continue
            # A step may give the CLI its own params where the CLI reads a
            # secret from a file, which MCP and HTTP take as a plain field.
            params = _substitute(step.get("cli_params", step["params"]), seen, root)
            # A step may name its CLI words explicitly when the operation's
            # dotted name does not match the command path, as `session.log_tail`
            # is the `session log` subcommand.
            words = step["cli"] if "cli" in step else step["operation"].split(".")
            argv = [binary, "--json", "--data-dir", str(data), *words, *_flags(params)]
            step_env = (
                {**env, "VOGT_ENGINE_URL": engine_url} if step.get("engine") else env
            )
            completed = subprocess.run(
                argv, env=step_env, capture_output=True, text=True, check=False
            )
            expect = step["expect"]
            if completed.returncode != expect.get("exit", 0):
                raise SystemExit(
                    f"{step['operation']} exited {completed.returncode}, "
                    f"wanted {expect.get('exit', 0)}: {completed.stderr.strip()}"
                )
            body = json.loads(completed.stdout) if completed.stdout.strip() else None
            seen[step["operation"]] = body
            answer = {
                "operation": step["operation"],
                "exit": completed.returncode,
                "body": _normalise(body, root, data, step["operation"]),
            }
            # A failure writes nothing to stdout, so the exit alone cannot say
            # which refusal it was. Keep the error line, with the data
            # directory blanked the way every other path is.
            if completed.returncode != 0 and expect.get("capture_error"):
                # The hooks banner is printed on every line of a parity run;
                # the error line is the one the step is there to pin down.
                line = next(
                    (
                        item
                        for item in reversed(completed.stderr.strip().splitlines())
                        if item.startswith("error:")
                    ),
                    completed.stderr.strip(),
                )
                answer["error"] = _normalise(line, root, data, step["operation"])
            answers.append(answer)
    finally:
        engine.__exit__(None, None, None)
        shutil.rmtree(root, ignore_errors=True)
    return answers


def _dump_names(step: dict[str, Any]) -> tuple[str, ...]:
    """The stores a dump step names: both for `true`, else the one named."""
    if step["dump"] is True:
        return ("declared", "observed")
    return (step["dump"],)


def _blank_payload_digests(dumped: dict[str, Any]) -> None:
    """Blank the audit digests of a mid-script dump, in place.

    An audit row's `payload_digest` covers the whole payload, and a project's
    payload names its root path, which is the run's temporary root. The digest
    therefore differs from one run to the next even though the rest of the row
    is fixed. The row's other fields still compare verbatim, so a changed
    operation, actor, revision or reason is still caught.
    """
    for tables in dumped.values():
        for row in (tables or {}).get("audit") or []:
            if "payload_digest" in row:
                row["payload_digest"] = "<payload-digest>"


def _dump(
    data: Path, names: tuple[str, ...] = ("declared", "observed")
) -> dict[str, Any]:
    """Every non-empty table in the named stores.

    Both stores after init by default. A dump step names the one it wants:
    the observed store after a sweep holds the findings and the coverage rows
    that the sweep wrote, and those are the evidence the sweep is judged on.

    The data directory is blanked because the run owns a temporary root.
    Everything else compares verbatim: the step clock fixes `applied_at` the
    same way it fixes every other timestamp, so blanking it would hide a
    regression to wall-clock stamping.
    """
    import sqlite3

    stores: dict[str, Any] = {}
    for name in names:
        path = data / f"{name}.sqlite3"
        conn = sqlite3.connect(path)
        conn.row_factory = sqlite3.Row
        tables = [
            row[0]
            for row in conn.execute(
                "SELECT name FROM sqlite_master WHERE type = 'table' "
                "AND name NOT LIKE 'sqlite_%' ORDER BY name"
            )
        ]
        dumped: dict[str, Any] = {}
        for table in tables:
            rows = [
                _normalise(dict(row), data.parent, data)
                for row in conn.execute(f"SELECT * FROM {table}")
            ]
            if rows:
                dumped[table] = rows
        conn.close()
        stores[name] = dumped
    return stores


def _flags(params: dict[str, Any]) -> list[str]:
    argv: list[str] = []
    for key, value in params.items():
        flag = f"--{key.replace('_', '-')}"
        if isinstance(value, bool):
            argv.append(flag if value else f"--no-{key.replace('_', '-')}")
        elif isinstance(value, list):
            for entry in value:
                argv += [
                    flag,
                    json.dumps(entry) if isinstance(entry, dict) else str(entry),
                ]
        else:
            argv += [flag, str(value)]
    return argv


def _substitute(
    params: dict[str, Any], seen: dict[str, Any], root: Path
) -> dict[str, Any]:
    def one(value: Any) -> Any:
        if not isinstance(value, str):
            return value
        # `{seen:<operation>#<dotted.key>}` is a field of an earlier step's
        # answer, so a step can name what an earlier one created.
        match = re.fullmatch(r"\{seen:([^#}]+)#([^}]+)\}", value)
        if match:
            found: Any = seen[match.group(1)]
            for key in match.group(2).split("."):
                found = found[key]
            return found
        value = value.replace("{root}", str(root))
        return value

    return {key: one(value) for key, value in params.items()}


def _run_http(binary: str, steps: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Drive the steps that name a route over HTTP against a served instance.

    Steps without an ``http`` block are CLI steps and are skipped, so a run
    without ``--only`` does not fail on them. The data directory goes on
    ``--data-dir``, which both binaries take before the subcommand.
    """
    import socket
    import urllib.request

    steps = [step for step in steps if "http" in step or "dump" in step]
    recorded: list[dict[str, Any]] = []
    with tempfile.TemporaryDirectory(prefix="vogt-parity-http-") as scratch:
        data = Path(scratch) / "instance"
        env = _child_env()
        init = subprocess.run(
            [binary, "--json", "--data-dir", str(data), "init"],
            env=_prepare_init(Path(scratch), env),
            capture_output=True,
            text=True,
            check=False,
        )
        if init.returncode != 0:
            raise SystemExit(f"init exited {init.returncode}: {init.stderr.strip()}")
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        # `--no-schedule` on both binaries. A background sweep between two
        # declared writes consumes AUTOINCREMENT `events.seq` values that are
        # never reused, so later sequences shift, and it races `auth.logout`
        # into a 500 (WI-1152). With the scheduler off, no sweep row is written
        # and the declared dump compares verbatim.
        server = subprocess.Popen(
            [
                binary,
                "--data-dir",
                str(data),
                "serve",
                "--host",
                "127.0.0.1",
                "--port",
                str(port),
                "--no-schedule",
            ],
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        try:
            base = f"http://127.0.0.1:{port}"
            _wait_until(f"{base}/health/live", server)
            credentials: dict[str, str] = {}
            secrets: list[str] = []
            for step in steps:
                if "dump" in step:
                    # The store is read while the server runs: sqlite serves
                    # the committed rows, and nothing writes between steps.
                    recorded.append(
                        {
                            "operation": step["operation"],
                            "result": _normalise(
                                _dump(data, _dump_names(step)), Path(scratch), data
                            ),
                        }
                    )
                    continue
                spec = step["http"]
                headers = dict(spec.get("headers") or {})
                if "auth" in spec:
                    kind = spec["auth"]
                    if kind not in credentials:
                        raise SystemExit(
                            f"{step['operation']} needs a {kind} credential"
                        )
                    headers["Authorization"] = f"Bearer {credentials[kind]}"
                body = (
                    _substitute(spec["body"], {}, Path(scratch))
                    if spec.get("body") is not None
                    else None
                )
                payload_bytes = json.dumps(body).encode() if body is not None else None
                if payload_bytes is not None:
                    headers.setdefault("Content-Type", "application/json")
                request = urllib.request.Request(
                    f"{base}{spec['path'].replace('{root}', scratch)}",
                    data=payload_bytes,
                    method=spec["method"],
                    headers=headers,
                )
                try:
                    with urllib.request.urlopen(request) as response:
                        status, payload = response.status, response.read()
                except urllib.error.HTTPError as error:
                    status, payload = error.code, error.read()
                result: Any = None
                if payload:
                    try:
                        result = json.loads(payload)
                    except json.JSONDecodeError:
                        result = {"unparseable": payload.decode(errors="replace")[:300]}
                recorded.append(
                    {
                        "operation": step["operation"],
                        "status": status,
                        "result": _normalise(
                            result, Path(scratch), data, step["operation"]
                        ),
                    }
                )
                if "capture" in spec:
                    # `admin` is the install's operator token, `token` the one
                    # token.issue mints, and `session` the login's session. The
                    # steps name which one they present.
                    if "secret" not in result:
                        raise SystemExit(
                            f"{step['operation']} answered {status} without a secret: "
                            f"{result!r}"
                        )
                    credentials[spec["capture"]] = result["secret"]
                    secrets.append(result["secret"])
            # The declared tables only. The server was started with
            # `--no-schedule`, so no background sweep writes into them.
            dumped = _normalise(_dump(data), Path(scratch), data)["declared"]
            _mark_token_hash(dumped, secrets)
            recorded.append({"operation": "dump", "result": dumped})
        finally:
            server.terminate()
            server.wait(timeout=10)
    return recorded


def _wait_until(url: str, server: subprocess.Popen[str], attempts: int = 50) -> None:
    import urllib.request

    for _ in range(attempts):
        try:
            with urllib.request.urlopen(url):
                return
        except (urllib.error.URLError, ConnectionError):
            if server.poll() is not None:
                reason = (server.stderr.read() if server.stderr else "") or "exited"
                raise SystemExit(
                    f"server exited before answering {url}: {reason}"
                ) from None
            time.sleep(0.1)
    raise SystemExit(f"server never answered {url}")


def _mark_token_hash(dumped: dict[str, Any], secrets: list[str]) -> None:
    """Replace a token hash that is the SHA-256 of a captured secret.

    `hash_token` is deterministic, so a matching hash is the scheme agreeing,
    not a value that has to be compared. A hash that matches none of the
    captured secrets stays as it was, which is how a scheme divergence shows
    up. Scoped to the tokens table of this dump: `token_hash` is not a global
    volatile key, because blanking it on the CLI would hide the same
    divergence there. Every captured secret is kept, not just the last one,
    because the bootstrap token and the issued token are different rows.
    """
    import hashlib

    expected = {
        hashlib.sha256(secret.encode("utf-8")).hexdigest() for secret in secrets
    }
    for row in dumped.get("tokens") or []:
        if isinstance(row, dict) and row.get("token_hash") in expected:
            row["token_hash"] = "<hash-of-secret>"


def _normalise(value: Any, root: Path, data: Path, operation: str | None = None) -> Any:
    rules = _rules()
    paths = {str(root): "<root>", str(data): "<data>"}
    return _walk(value, rules, paths, operation, True)


def _rules() -> dict[str, Any]:
    # The rule file is TOML but this tree's Python may predate tomllib's
    # availability only below 3.11, which the project already requires.
    import tomllib

    return tomllib.loads(RULES_PATH.read_text())


def _product_versions() -> set[str]:
    """The version a build reports: the release in ``pyproject.toml``.

    Both sides report it. Python reads ``vogt.__version__`` and the Rust core
    falls back to the pinned release (``check_product_version.py`` enforces the
    two agree), so neither ever says ``local/dev``. Accepting that stamp here
    would hide a Rust build that had fallen back to it, which is the divergence
    this rule exists to keep visible.
    """
    import tomllib

    project = tomllib.loads((REPO / "pyproject.toml").read_text())["project"]
    return {str(project["version"])}


def _walk(
    value: Any,
    rules: dict[str, Any],
    paths: dict[str, str],
    operation: str | None = None,
    top: bool = False,
) -> Any:
    if isinstance(value, dict):
        volatile = set(rules["volatile"]["keys"])
        schema = rules["schema"]
        in_schema = any(marker in value for marker in schema["schema_markers"])
        drop = set(schema["drop_keys"]) if in_schema else set()
        version = rules.get("version", {})
        blank_version = top and operation in set(version.get("operations", []))
        version_key = version.get("key")
        accepted = _product_versions() if blank_version else set()
        digests = rules.get("digests", {})
        path_bearing = set(digests.get("path_bearing_operations", []))
        walked = {
            key: (
                "<version>"
                if blank_version and key == version_key and item in accepted
                else "<volatile>"
                if key in volatile
                else _walk(item, rules, paths, operation, False)
            )
            for key, item in value.items()
            if key not in drop
        }
        if walked.get("operation") in path_bearing and "payload_digest" in walked:
            walked["payload_digest"] = "<path-bearing>"
        if (
            in_schema
            and schema["sort_any_of"]
            and isinstance(walked.get("anyOf"), list)
        ):
            walked["anyOf"] = sorted(
                walked["anyOf"], key=lambda item: json.dumps(item, sort_keys=True)
            )
        return walked
    if isinstance(value, list):
        return [_walk(item, rules, paths, operation, False) for item in value]
    if isinstance(value, str):
        for needle, token in paths.items():
            value = value.replace(needle, token)
        return value
    # Floats are rounded to 3 decimal places because ranking totals carry more
    # precision than two runs agree on once staleness is involved. Integers
    # and everything else compare verbatim.
    if isinstance(value, float):
        return round(value, 3)
    return value


def _selftest(golden_dir: Path) -> int:
    """A mutated recording must fail the check.

    The id mutation runs against the committed golden. The title scoping cannot:
    none of the recorded steps returns a titled entity. A synthetic fixture
    carries both shapes — an answer whose `title` must survive normalisation,
    and a schema object (it has `properties`) whose `title` must be dropped.
    Mutating the first must fail the diff; mutating the second must not.
    """
    root, data = Path("/parity/root"), Path("/parity/data")
    golden = _normalise(json.loads((golden_dir / "cli.json").read_text()), root, data)
    if _diff(golden, golden, "") != 0:
        print("selftest: the golden disagrees with itself", file=sys.stderr)
        return 1
    mutated = json.loads((golden_dir / "cli.json").read_text())
    if not _mutate(mutated, "id"):
        print("selftest: no 'id' to mutate in the golden", file=sys.stderr)
        return 1
    if _diff(golden, _normalise(mutated, root, data), "") == 0:
        print("selftest: mutating 'id' was not caught", file=sys.stderr)
        return 1

    # The dump is a row store, not an answer, so a change to one cell must fail
    # the check the same way a changed id does. Mutating the seeded workflow
    # definition is the case that would otherwise pass silently.
    dumped = json.loads((golden_dir / "cli.json").read_text())
    definition = dumped[0]["body"]["declared"]["workflow_defs"][0]["definition"]
    dumped[0]["body"]["declared"]["workflow_defs"][0]["definition"] = (
        definition + "-mutated"
    )
    if _diff(golden, _normalise(dumped, root, data), "") == 0:
        print(
            "selftest: mutating a dumped workflow definition was not caught",
            file=sys.stderr,
        )
        return 1

    fixture = {
        "answer": {"id": "wrk_0001", "title": "the real title"},
        "schema": {"properties": {"name": {"type": "string"}}, "title": "ModelName"},
    }
    normalised = _normalise(fixture, root, data)
    if normalised["schema"].get("title") is not None:
        print("selftest: a schema title survived normalisation", file=sys.stderr)
        return 1
    changed = json.loads(json.dumps(fixture))
    changed["answer"]["title"] += "-mutated"
    if _diff(normalised, _normalise(changed, root, data), "") == 0:
        print("selftest: mutating an answer title was not caught", file=sys.stderr)
        return 1
    dropped = json.loads(json.dumps(fixture))
    dropped["schema"]["title"] += "-mutated"
    if _diff(normalised, _normalise(dropped, root, data), "") != 0:
        print("selftest: mutating a schema title was not dropped", file=sys.stderr)
        return 1

    http = json.loads((golden_dir / "http.json").read_text())
    http_golden = _normalise(http, root, data)
    if not _mutate(http, "instance_id"):
        print(
            "selftest: no 'instance_id' to mutate in the HTTP golden",
            file=sys.stderr,
        )
        return 1
    if _diff(http_golden, _normalise(http, root, data), "") == 0:
        print("selftest: mutating 'instance_id' was not caught", file=sys.stderr)
        return 1
    print(
        "selftest passed: id, dumped workflow definition, answer title and "
        "instance_id caught, schema title dropped"
    )
    return 0


def _mutate(value: Any, key: str) -> bool:
    if isinstance(value, dict):
        if isinstance(value.get(key), str):
            value[key] = value[key] + "-mutated"
            return True
        return any(_mutate(item, key) for item in value.values())
    if isinstance(value, list):
        return any(_mutate(item, key) for item in value)
    return False


def _diff(
    golden: list[dict[str, Any]], recorded: list[dict[str, Any]], prefix: str
) -> int:
    left = json.dumps(golden, indent=2, sort_keys=True).splitlines(keepends=True)
    right = json.dumps(recorded, indent=2, sort_keys=True).splitlines(keepends=True)
    if left == right:
        label = f" --only {prefix}" if prefix else ""
        print(f"check passed: {len(recorded)} steps agree{label}")
        return 0
    sys.stdout.writelines(
        difflib.unified_diff(left, right, fromfile="golden", tofile="recorded")
    )
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
