#!/usr/bin/env python3
"""Deterministic stand-in for the agent CLI an agent-task run drives (#296).

An agent task spawns a CLI — `claude`, `codex`, `opencode`, usually wrapped as
`mydevenv2-agent-auth run -- <cli>` — inside a PTY (see
`engine/server/src/agent_tasks.rs`). CI has none of those binaries and no way to
type into a PTY, so the run-orchestration behaviour (#241, and the future #289
gates/steer, #290 triggers, #291 outcomes, #283-#285 git story) has had no
automated path. This script is that path: it accepts the *same invocation shape*
the engine produces and plays a chosen **scenario**, with no network, no model,
and no wall-clock dependence beyond the small delays a scenario opts into.

Invocation shape it matches (all as the engine sends them):

* **argv** — the run's `command`, executed directly (argv[0] is the program).
  The prompt is **not** an argument; the engine delivers it as a file.
* **env** — `MYDEVENV2_AGENT_TASK_PROMPT_FILE` (the prompt),
  `MYDEVENV2_AGENT_TASK_CONTEXT_FILE`, `MYDEVENV2_AGENT_TASK_ID`,
  `MYDEVENV2_AGENT_TASK_RUN_ID`, and — when the task is Vogt-bound —
  `VOGT_PROJECT` / `VOGT_WORK_ITEM`.
* **prompt** — read from the prompt file named in the env (or `--prompt-file`).
* **findings** — a run reports a finding by printing a line beginning with the
  notify phrase (`VOGT_NOTIFY:` by default, `MYDEVENV2_NOTIFY:` legacy); the
  engine's phrase watcher records the text after it on the run.
* **exit** — the process exit code becomes the run outcome (0 completed, else
  errored).

The scenario is a `+`-separated list of steps run in order, so the named
scenarios compose from one vocabulary:

  edit      write to a file in the working tree
  commit    `git commit` the working tree with checkpoint trailers
  findings  print a `VOGT_NOTIFY:` line (and optionally dump findings JSON)
  idle      print an idle prompt and wait for a steer line on stdin
  outcome   exit with the chosen code
  stall     sleep, to exercise the stall / idle-stall timeout
  skip      print a `VOGT_SKIP:` line so the run concludes `skipped` (#291)
  cost      print a `VOGT_COST:` line so the conclusion records a cost (#291)
  schema    print a fenced ```json findings block, re-prompted until it passes
            the task's `output_schema` (#291)

`edit+commit` is therefore "edit then commit"; `findings+outcome` is "report
then exit". With no scenario the default is `outcome` (a clean exit 0).

Everything a scenario needs beyond the prompt is taken from `FAKE_AGENT_*`
environment variables so a caller sets behaviour without editing this file; see
`READ_ME` in `scripts/fake-agent.README.md` for the full table.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import time
from pathlib import Path

# The engine env the run is handed. Named here so the contract this script
# honours is one readable list rather than scattered os.environ lookups.
ENV_PROMPT_FILE = "MYDEVENV2_AGENT_TASK_PROMPT_FILE"
ENV_CONTEXT_FILE = "MYDEVENV2_AGENT_TASK_CONTEXT_FILE"
ENV_TASK_ID = "MYDEVENV2_AGENT_TASK_ID"
ENV_RUN_ID = "MYDEVENV2_AGENT_TASK_RUN_ID"
ENV_VOGT_PROJECT = "VOGT_PROJECT"
ENV_VOGT_WORK_ITEM = "VOGT_WORK_ITEM"

# The default the engine configures a task with, and the legacy prefix it still
# matches (#203). A scenario prints whichever phrase the caller asked for.
DEFAULT_NOTIFY_PHRASE = "VOGT_NOTIFY:"

# The sentinels the engine's conclusion path reads (#291): a run that prints
# SKIP_SENTINEL and exits cleanly concludes `skipped`, and the text after
# COST_SENTINEL (JSON or a bare dollar amount) is parsed into the run's cost.
SKIP_SENTINEL = "VOGT_SKIP:"
COST_SENTINEL = "VOGT_COST:"

# Checkpoint trailers the `commit` step writes. There is no engine-side reader
# for these yet — the git story (#283-#285) is future work — so this is the
# format that work can standardise on: two git trailers naming the task and run
# a commit belongs to, so a checkpoint made by a run is traceable back to it.
TRAILER_TASK = "Vogt-Task"
TRAILER_RUN = "Vogt-Run"

KNOWN_STEPS = (
    "edit",
    "commit",
    "findings",
    "idle",
    "outcome",
    "stall",
    "skip",
    "cost",
    "schema",
)


def _env(name: str, default: str = "") -> str:
    value = os.environ.get(name)
    return value if value is not None else default


def _emit(line: str) -> None:
    """Print one line and flush.

    The engine reads this over a PTY and its watchers scan line by line; an
    unflushed print would leave a finding sitting in a buffer until exit, which
    is exactly the race a test must not have.
    """
    sys.stdout.write(line + "\n")
    sys.stdout.flush()


def _read_prompt(prompt_file: str | None) -> str:
    path = prompt_file or _env(ENV_PROMPT_FILE) or None
    if not path:
        return ""
    try:
        return Path(path).read_text(encoding="utf-8")
    except OSError:
        return ""


def _scenario_from_prompt(prompt: str) -> str | None:
    """A `FAKE_AGENT_SCENARIO: <scenario>` marker line in the prompt, if any.

    The prompt is the one channel a task author always controls, so a scenario
    can be selected from the task definition alone when setting an env var on
    the run is inconvenient.
    """
    for raw in prompt.splitlines():
        line = raw.strip()
        if line.startswith("FAKE_AGENT_SCENARIO:"):
            value = line[len("FAKE_AGENT_SCENARIO:") :].strip()
            if value:
                return value
    return None


def _resolve_scenario(cli_scenario: str | None, prompt: str) -> str:
    """Env wins, then argv, then a prompt marker, then the `outcome` default."""
    for candidate in (
        _env("FAKE_AGENT_SCENARIO") or None,
        cli_scenario,
        _scenario_from_prompt(prompt),
    ):
        if candidate:
            return candidate
    return "outcome"


def _steps(scenario: str) -> list[str]:
    steps = [part.strip() for part in scenario.split("+") if part.strip()]
    unknown = [s for s in steps if s not in KNOWN_STEPS]
    if unknown:
        raise SystemExit(
            f"fake-agent: unknown scenario step(s) {unknown}; "
            f"known steps are {list(KNOWN_STEPS)}"
        )
    return steps or ["outcome"]


# --- steps ----------------------------------------------------------------


def step_edit() -> None:
    """Write deterministic text into a file in the working tree."""
    target = Path(_env("FAKE_AGENT_EDIT_FILE", "fake-agent-edit.txt"))
    run_id = _env(ENV_RUN_ID, "no-run")
    text = _env(
        "FAKE_AGENT_EDIT_TEXT",
        f"edited by fake-agent for run {run_id}\n",
    )
    if not text.endswith("\n"):
        text += "\n"
    # Append so a second run leaves evidence of both, matching how a real agent
    # accretes changes across checkpoints rather than clobbering.
    with target.open("a", encoding="utf-8") as handle:
        handle.write(text)
    _emit(f"fake-agent: edited {target}")


def _git(args: list[str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        ["git", *args],
        capture_output=True,
        text=True,
        check=False,
    )


def step_commit() -> None:
    """Commit the working tree with the checkpoint trailers.

    Identity is passed with `-c` rather than assumed from the environment so the
    commit succeeds in a bare CI checkout with no configured user. The trailers
    are appended to the message body directly (a blank line then `Key: value`),
    which is exactly what git parses back out as trailers.
    """
    task_id = _env(ENV_TASK_ID)
    run_id = _env(ENV_RUN_ID)
    subject = _env("FAKE_AGENT_COMMIT_MESSAGE", "chore: fake-agent checkpoint")

    trailers = []
    if task_id:
        trailers.append(f"{TRAILER_TASK}: {task_id}")
    if run_id:
        trailers.append(f"{TRAILER_RUN}: {run_id}")
    message = subject
    if trailers:
        message = subject + "\n\n" + "\n".join(trailers) + "\n"

    add = _git(["add", "-A"])
    if add.returncode != 0:
        _emit(f"fake-agent: git add failed: {add.stderr.strip()}")
        raise SystemExit(1)
    commit = _git(
        [
            "-c",
            "user.email=fake-agent@vogt.invalid",
            "-c",
            "user.name=fake-agent",
            "commit",
            "--no-gpg-sign",
            "--allow-empty",
            "-m",
            message,
        ]
    )
    if commit.returncode != 0:
        _emit(f"fake-agent: git commit failed: {commit.stderr.strip()}")
        raise SystemExit(1)
    head = _git(["rev-parse", "HEAD"])
    _emit(f"fake-agent: committed {head.stdout.strip()}")


def step_findings() -> None:
    """Report a finding the way the engine's phrase watcher reads it.

    The load-bearing half is the printed `<phrase> <text>` line: that is what
    `spawn_phrase_watcher` records on the run and pushes. The optional JSON dump
    is a forward-looking, structured mirror of the same finding for a consumer
    that wants one (there is no engine `output_schema` today).
    """
    phrase = _env("FAKE_AGENT_NOTIFY_PHRASE", DEFAULT_NOTIFY_PHRASE)
    text = _env("FAKE_AGENT_NOTIFY_TEXT", "fake-agent synthetic finding")

    # The engine's watcher subscribes just after the session spawns; a finding
    # printed before then would be missed. A short, opt-out delay makes the
    # scenario a test rather than a race.
    delay = float(_env("FAKE_AGENT_NOTIFY_DELAY", "0.3"))
    if delay > 0:
        time.sleep(delay)

    _emit(f"{phrase} {text}")

    dump = _env("FAKE_AGENT_FINDINGS_FILE")
    if dump:
        payload = {
            "task_id": _env(ENV_TASK_ID),
            "run_id": _env(ENV_RUN_ID),
            "vogt_project": _env(ENV_VOGT_PROJECT) or None,
            "vogt_work_item": _env(ENV_VOGT_WORK_ITEM) or None,
            "findings": [
                {"text": text, "source": "fake-agent", "phrase": phrase},
            ],
        }
        Path(dump).write_text(json.dumps(payload, indent=2) + "\n", encoding="utf-8")


def step_idle() -> None:
    """Print an idle prompt and block until a steer line arrives on stdin.

    This is the seam the future steer/gate work (#289) drives: the run pauses,
    the operator (or a test) writes a line into the PTY, and the run continues.
    EOF is a clean continue so a caller who never steers is not stuck forever.
    """
    prompt = _env("FAKE_AGENT_IDLE_PROMPT", "fake-agent idle> ")
    sys.stdout.write(prompt)
    sys.stdout.flush()
    line = sys.stdin.readline()
    steer = line.strip()
    if steer:
        _emit(f"fake-agent: steered with {steer!r}")
    else:
        _emit("fake-agent: idle continued (eof)")


def step_stall() -> None:
    """Sleep, to exercise the stall / idle-stall timeout path."""
    seconds = float(_env("FAKE_AGENT_STALL_SECONDS", "3600"))
    _emit(f"fake-agent: stalling for {seconds}s")
    time.sleep(seconds)


def step_outcome() -> None:
    """Exit with the chosen code (default 0)."""
    code = int(_env("FAKE_AGENT_EXIT_CODE", "0"))
    _emit(f"fake-agent: exiting {code}")
    raise SystemExit(code)


def step_skip() -> None:
    """Declare the run a deliberate no-op (#291).

    Prints the skip sentinel and returns; a `skip` scenario with no `outcome`
    step falls through to the clean exit 0 in `main`, which is exactly the
    "there was nothing to do" shape the engine concludes as `skipped`.
    """
    reason = _env("FAKE_AGENT_SKIP_REASON", "nothing to do")
    _emit(f"{SKIP_SENTINEL} {reason}")


def step_cost() -> None:
    """Report what the run cost (#291).

    The payload defaults to a small JSON object; a caller can override it with a
    bare dollar amount (`$0.40`) to exercise the other parse path.
    """
    payload = _env(
        "FAKE_AGENT_COST",
        '{"total_usd": 0.42, "input_tokens": 1200, "output_tokens": 340}',
    )
    _emit(f"{COST_SENTINEL} {payload}")


def step_schema() -> None:
    """Print a fenced ```json findings block, re-prompted until it passes (#291).

    The engine's schema watcher reads the block, validates it against the task's
    `output_schema`, and — on a mismatch with budget left — writes a correction
    line into the PTY. This step waits for that line on stdin after each *wrong*
    block, then prints the next one; on the attempt named by
    `FAKE_AGENT_SCHEMA_PASS_ON` (default 1, "pass first try") it prints a *good*
    block and exits 0. When the engine gives up instead of re-prompting, it kills
    the session and this readline never returns — which is the point.
    """
    pass_on = int(_env("FAKE_AGENT_SCHEMA_PASS_ON", "1"))
    delay = float(_env("FAKE_AGENT_SCHEMA_DELAY", "0.3"))
    good = _env(
        "FAKE_AGENT_SCHEMA_GOOD",
        '{"summary": "did the thing", "risk": "low"}',
    )
    bad = _env("FAKE_AGENT_SCHEMA_BAD", '{"summary": "did the thing"}')

    # The watcher subscribes just after the session spawns; a first block
    # printed before then would be missed, the same race the findings step opts
    # out of with a short delay.
    if delay > 0:
        time.sleep(delay)

    attempt = 1
    while True:
        payload = good if attempt >= pass_on else bad
        _emit("```json")
        _emit(payload)
        _emit("```")
        if attempt >= pass_on:
            raise SystemExit(0)
        # A wrong block: wait for the engine's correction before trying again.
        line = sys.stdin.readline()
        if not line:
            # No re-prompt arrived (eof); stop cleanly rather than spin.
            raise SystemExit(0)
        attempt += 1


STEP_FUNCS = {
    "edit": step_edit,
    "commit": step_commit,
    "findings": step_findings,
    "idle": step_idle,
    "stall": step_stall,
    "outcome": step_outcome,
    "skip": step_skip,
    "cost": step_cost,
    "schema": step_schema,
}


def _print_contract() -> None:
    """Emit the machine-readable contract, so a caller can discover it."""
    contract = {
        "scenario_default": "outcome",
        "steps": list(KNOWN_STEPS),
        "reads_env": [
            ENV_PROMPT_FILE,
            ENV_CONTEXT_FILE,
            ENV_TASK_ID,
            ENV_RUN_ID,
            ENV_VOGT_PROJECT,
            ENV_VOGT_WORK_ITEM,
        ],
        "config_env": {
            "FAKE_AGENT_SCENARIO": "scenario, '+'-separated steps",
            "FAKE_AGENT_NOTIFY_PHRASE": DEFAULT_NOTIFY_PHRASE,
            "FAKE_AGENT_NOTIFY_TEXT": "fake-agent synthetic finding",
            "FAKE_AGENT_NOTIFY_DELAY": "0.3",
            "FAKE_AGENT_FINDINGS_FILE": "optional path for a JSON dump",
            "FAKE_AGENT_EXIT_CODE": "0",
            "FAKE_AGENT_EDIT_FILE": "fake-agent-edit.txt",
            "FAKE_AGENT_EDIT_TEXT": "edited by fake-agent for run <run_id>",
            "FAKE_AGENT_COMMIT_MESSAGE": "chore: fake-agent checkpoint",
            "FAKE_AGENT_STALL_SECONDS": "3600",
            "FAKE_AGENT_IDLE_PROMPT": "fake-agent idle> ",
            "FAKE_AGENT_SKIP_REASON": "nothing to do",
            "FAKE_AGENT_COST": '{"total_usd": 0.42, ...}',
            "FAKE_AGENT_SCHEMA_PASS_ON": "1",
            "FAKE_AGENT_SCHEMA_DELAY": "0.3",
            "FAKE_AGENT_SCHEMA_GOOD": "a findings block that passes the schema",
            "FAKE_AGENT_SCHEMA_BAD": "a findings block that fails the schema",
        },
        "commit_trailers": [TRAILER_TASK, TRAILER_RUN],
        "notify_phrase_default": DEFAULT_NOTIFY_PHRASE,
        "skip_sentinel": SKIP_SENTINEL,
        "cost_sentinel": COST_SENTINEL,
    }
    _emit(json.dumps(contract, indent=2))


def main(argv: list[str]) -> int:
    cli_scenario: str | None = None
    prompt_file: str | None = None
    i = 0
    while i < len(argv):
        arg = argv[i]
        if arg == "--print-contract":
            _print_contract()
            return 0
        if arg == "--prompt-file":
            i += 1
            if i >= len(argv):
                raise SystemExit("fake-agent: --prompt-file needs a path")
            prompt_file = argv[i]
        elif arg.startswith("--prompt-file="):
            prompt_file = arg.split("=", 1)[1]
        elif arg == "--scenario":
            i += 1
            if i >= len(argv):
                raise SystemExit("fake-agent: --scenario needs a value")
            cli_scenario = argv[i]
        elif arg.startswith("--scenario="):
            cli_scenario = arg.split("=", 1)[1]
        elif not arg.startswith("-") and cli_scenario is None:
            # A bare positional is the scenario, so
            # `fake-agent edit+commit` reads naturally.
            cli_scenario = arg
        i += 1

    prompt = _read_prompt(prompt_file)
    scenario = _resolve_scenario(cli_scenario, prompt)
    steps = _steps(scenario)

    _emit(f"fake-agent: scenario {scenario} (steps: {'+'.join(steps)})")
    for step in steps:
        # `outcome` and `stall` do not return; every other step does and the
        # next one runs. A scenario that names neither still exits 0 below.
        STEP_FUNCS[step]()
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
