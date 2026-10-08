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
import difflib
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from datetime import UTC, datetime
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
        cmd.add_argument("--transport", default="cli", choices=["cli", "http"])
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
    steps = _load_steps(args.only)
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
    elif args.only:
        golden = [step for step in golden if step["operation"].startswith(args.only)]
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
    return [step for step in document["steps"] if step["operation"].startswith(prefix)]


def _run(binary: str, steps: list[dict[str, Any]], transport: str) -> list[dict[str, Any]]:
    if transport == "http":
        return _run_http(binary, steps)
    return _run_cli(binary, steps)


def _run_cli(binary: str, steps: list[dict[str, Any]]) -> list[dict[str, Any]]:
    root = Path(tempfile.mkdtemp(prefix="vogt-parity-"))
    data = root / "instance"
    env = {
        **os.environ,
        "VOGT_TEST_CLOCK_START": CLOCK_START,
        "VOGT_TEST_IDS": "sequential",
        # The local principal is `local:$USER`. Pinning it keeps a golden
        # recorded by one account comparable to a run under another, including
        # CI's `runner` user.
        "USER": "parity",
        "LOGNAME": "parity",
    }
    # A session may export VOGT_CORE_URL for the engine front door. The CLI
    # this harness spawns uses its own local store, and the hint that variable
    # produces is not part of the contract under test.
    env.pop("VOGT_CORE_URL", None)
    answers: list[dict[str, Any]] = []
    seen: dict[str, Any] = {}
    try:
        init = subprocess.run(
            [binary, "--json", "--data-dir", str(data), "init"],
            env=env,
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
            if "params" not in step:
                continue
            params = _substitute(step["params"], seen, root)
            argv = [binary, "--json", "--data-dir", str(data), *step["operation"].split("."), *_flags(params)]
            completed = subprocess.run(
                argv, env=env, capture_output=True, text=True, check=False
            )
            expect = step["expect"]
            if completed.returncode != expect.get("exit", 0):
                raise SystemExit(
                    f"{step['operation']} exited {completed.returncode}, "
                    f"wanted {expect.get('exit', 0)}: {completed.stderr.strip()}"
                )
            body = json.loads(completed.stdout) if completed.stdout.strip() else None
            seen[step["operation"]] = body
            answers.append(
                {
                    "operation": step["operation"],
                    "exit": completed.returncode,
                    "body": _normalise(body, root, data, step["operation"]),
                }
            )
    finally:
        shutil.rmtree(root, ignore_errors=True)
    return answers


def _dump(data: Path) -> dict[str, Any]:
    """Every non-empty table in both stores, after init.

    The data directory is blanked because the run owns a temporary root.
    Everything else compares verbatim: the step clock fixes `applied_at` the
    same way it fixes every other timestamp, so blanking it would hide a
    regression to wall-clock stamping.
    """
    import sqlite3

    stores: dict[str, Any] = {}
    for name in ("declared", "observed"):
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

    steps = [step for step in steps if "http" in step]
    recorded: list[dict[str, Any]] = []
    with tempfile.TemporaryDirectory(prefix="vogt-parity-http-") as scratch:
        data = Path(scratch) / "instance"
        env = {
            **os.environ,
            "VOGT_TEST_CLOCK_START": CLOCK_START,
            "VOGT_TEST_IDS": "sequential",
            "USER": "parity",
            "LOGNAME": "parity",
        }
        env.pop("VOGT_CORE_URL", None)
        init = subprocess.run(
            [binary, "--json", "--data-dir", str(data), "init"],
            env=env, capture_output=True, text=True, check=False,
        )
        if init.returncode != 0:
            raise SystemExit(f"init exited {init.returncode}: {init.stderr.strip()}")
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            port = sock.getsockname()[1]
        server = subprocess.Popen(
            [binary, "--data-dir", str(data), "serve", "--host", "127.0.0.1", "--port", str(port), "--no-auth"],
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        try:
            base = f"http://127.0.0.1:{port}"
            _wait_until(f"{base}/health/live", server)
            for step in steps:
                spec = step["http"]
                request = urllib.request.Request(
                    f"{base}{spec['path']}",
                    method=spec["method"],
                    headers=spec.get("headers") or {},
                )
                try:
                    with urllib.request.urlopen(request) as response:
                        status, payload = response.status, response.read()
                except urllib.error.HTTPError as error:
                    status, payload = error.code, error.read()
                result: Any = json.loads(payload) if payload else None
                recorded.append(
                    {
                        "operation": step["operation"],
                        "status": status,
                        "result": _normalise(
                            result, Path(scratch), data, step["operation"]
                        ),
                    }
                )
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
                raise SystemExit(f"server exited before answering {url}: {reason}")
            time.sleep(0.1)
    raise SystemExit(f"server never answered {url}")


def _normalise(value: Any, root: Path, data: Path, operation: str | None = None) -> Any:
    rules = _rules()
    return _walk(value, rules, {str(root): "<root>", str(data): "<data>"}, operation, True)


def _rules() -> dict[str, Any]:
    # The rule file is TOML but this tree's Python may predate tomllib's
    # availability only below 3.11, which the project already requires.
    import tomllib

    return tomllib.loads(RULES_PATH.read_text())


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
        walked = {
            key: (
                "<version>"
                if blank_version and key == version.get("key")
                else "<volatile>"
                if key in volatile
                else _walk(item, rules, paths, operation, False)
            )
            for key, item in value.items()
            if key not in drop
        }
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
    dumped[0]["body"]["declared"]["workflow_defs"][0]["definition"] = definition + "-mutated"
    if _diff(golden, _normalise(dumped, root, data), "") == 0:
        print("selftest: mutating a dumped workflow definition was not caught", file=sys.stderr)
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
        print("selftest: no 'instance_id' to mutate in the HTTP golden", file=sys.stderr)
        return 1
    if _diff(http_golden, _normalise(http, root, data), "") == 0:
        print("selftest: mutating 'instance_id' was not caught", file=sys.stderr)
        return 1
    print("selftest passed: id, dumped workflow definition, answer title and instance_id caught, schema title dropped")
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
