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
        cmd.add_argument("--transport", default="cli", choices=["cli"])
        if name == "record":
            cmd.add_argument("--out", type=Path, required=True)
        else:
            cmd.add_argument("--golden", type=Path, required=True)
    args = parser.parse_args(argv)

    binary = _resolve(args.impl)
    steps = _load_steps(args.only)
    if not steps:
        print(f"no steps match --only {args.only!r}", file=sys.stderr)
        return 2
    recorded = _run(binary, steps)

    if args.command == "record":
        args.out.mkdir(parents=True, exist_ok=True)
        target = args.out / "cli.json"
        target.write_text(json.dumps(recorded, indent=2, sort_keys=True) + "\n")
        print(f"recorded {len(recorded)} steps to {target}")
        return 0

    golden = json.loads((args.golden / "cli.json").read_text())
    if args.only:
        golden = [step for step in golden if step["operation"].startswith(args.only)]
    return _diff(golden, recorded, args.only)


def _resolve(impl: str) -> str:
    env_name, fallback = BINS[impl]
    binary = os.environ.get(env_name) or shutil.which(fallback)
    if binary is None:
        raise SystemExit(f"{env_name} is unset and {fallback!r} is not on PATH")
    return binary


def _load_steps(prefix: str) -> list[dict[str, Any]]:
    document = json.loads(SCRIPT_PATH.read_text())
    return [step for step in document["steps"] if step["operation"].startswith(prefix)]


def _run(binary: str, steps: list[dict[str, Any]]) -> list[dict[str, Any]]:
    root = Path(tempfile.mkdtemp(prefix="vogt-parity-"))
    data = root / "instance"
    env = {
        **os.environ,
        "VOGT_DATA_DIR": str(data),
        "VOGT_TEST_CLOCK_START": CLOCK_START,
        "VOGT_TEST_IDS": "sequential",
    }
    # A session may export VOGT_CORE_URL for the engine front door. The CLI
    # this harness spawns uses its own local store, and the hint that variable
    # produces is not part of the contract under test.
    env.pop("VOGT_CORE_URL", None)
    answers: list[dict[str, Any]] = []
    seen: dict[str, Any] = {}
    try:
        init = subprocess.run(
            [binary, "--json", "init"],
            env=env,
            capture_output=True,
            text=True,
            check=False,
        )
        if init.returncode != 0:
            raise SystemExit(f"init exited {init.returncode}: {init.stderr.strip()}")
        for step in steps:
            params = _substitute(step["params"], seen, root)
            argv = [binary, "--json", *step["operation"].split("."), *_flags(params)]
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
                    "body": _normalise(body, root, data),
                }
            )
    finally:
        shutil.rmtree(root, ignore_errors=True)
    return answers


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


def _normalise(value: Any, root: Path, data: Path) -> Any:
    rules = _rules()
    return _walk(value, rules, {str(root): "<root>", str(data): "<data>"})


def _rules() -> dict[str, Any]:
    # The rule file is TOML but this tree's Python may predate tomllib's
    # availability only below 3.11, which the project already requires.
    import tomllib

    return tomllib.loads(RULES_PATH.read_text())


def _walk(value: Any, rules: dict[str, Any], paths: dict[str, str]) -> Any:
    volatile = set(rules["volatile"]["keys"])
    drop = set(rules["schema"]["drop_keys"])
    if isinstance(value, dict):
        walked = {
            key: ("<volatile>" if key in volatile else _walk(item, rules, paths))
            for key, item in value.items()
            if key not in drop
        }
        if rules["schema"]["sort_any_of"] and isinstance(walked.get("anyOf"), list):
            walked["anyOf"] = sorted(
                walked["anyOf"], key=lambda item: json.dumps(item, sort_keys=True)
            )
        return walked
    if isinstance(value, list):
        return [_walk(item, rules, paths) for item in value]
    if isinstance(value, str):
        for needle, token in paths.items():
            value = value.replace(needle, token)
        return value
    if isinstance(value, float):
        return round(value, 3)
    return value


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
