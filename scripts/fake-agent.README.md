# `fake-agent` — a synthetic agent CLI for tests

An agent task drives a real coding-agent CLI — `claude`, `codex`, `opencode`,
usually wrapped as `vogt-agent-auth run -- <cli>` — inside a PTY. CI has
none of those binaries and no way to type into a PTY, so the run-orchestration
behaviour in [`../engine/server/src/agent_tasks.rs`](../engine/server/src/agent_tasks.rs)
needs a stand-in. `fake-agent` is that stand-in: it accepts the *same
invocation shape* the engine produces and plays a chosen **scenario** — no
network, no model, no wall-clock dependence beyond the small delays a scenario
opts into.

## Files

- `fake-agent` — a POSIX-shell entry. It only finds a Python interpreter
  (`$FAKE_AGENT_PYTHON`, else `python3`) and execs the core beside it, passing
  argv and the environment through untouched — because the engine spawns
  `argv[0]` directly, the way it would a real CLI.
- `fake_agent_core.py` — the deterministic core that reads the prompt, selects
  a scenario, and plays it.

## The invocation shape it matches

Everything below is exactly what the engine sends a real agent CLI (see
`agent_tasks.rs`):

- **argv** — the run's `command`, executed directly. The prompt is **not** an
  argument; it is delivered as a file.
- **env** —
  `VOGT_ENGINE_AGENT_TASK_PROMPT_FILE` (the prompt file),
  `VOGT_ENGINE_AGENT_TASK_CONTEXT_FILE`,
  `VOGT_ENGINE_AGENT_TASK_ID`,
  `VOGT_ENGINE_AGENT_TASK_RUN_ID`, and — when the task is Vogt-bound —
  `VOGT_PROJECT` / `VOGT_WORK_ITEM`.
- **prompt** — read from the prompt file named in the env (or `--prompt-file`).
- **findings** — a run reports a finding by printing a line beginning with the
  notify phrase (`VOGT_NOTIFY:` by default). The engine's phrase watcher
  records the text after it on the run.
- **skip and cost** — a run that prints `VOGT_SKIP: <reason>` and exits
  cleanly concludes `skipped`; the text after `VOGT_COST:` (JSON or a bare
  dollar amount) is parsed into the run's cost.
- **structured findings** — a fenced ```` ```json ```` block is validated
  against the task's `output_schema`; on a mismatch with budget left, the
  engine writes a correction line into the PTY and the run tries again.
- **exit** — the process exit code becomes the run outcome: `0` completed,
  anything else errored.

## Scenarios

A scenario is a `+`-separated list of **steps**, run in order, so the named
scenarios compose from one small vocabulary:

| Step | What it does |
|---|---|
| `edit` | Append deterministic text to a file in the working tree. |
| `commit` | `git commit` the working tree with the checkpoint trailers (below). |
| `findings` | Print a `VOGT_NOTIFY:` line (and optionally dump a findings JSON). |
| `idle` | Print an idle prompt and block until a steer line arrives on stdin (EOF continues). |
| `outcome` | Exit with the chosen code. |
| `stall` | Sleep, to exercise the stall / idle-stall timeout. |
| `skip` | Print a `VOGT_SKIP:` line so the run concludes `skipped`. |
| `cost` | Print a `VOGT_COST:` line so the conclusion records a cost. |
| `schema` | Print a fenced JSON findings block, re-prompted until it passes the task's `output_schema`. |

So `edit+commit` is "edit then commit", `findings+outcome` is "report then
exit". `outcome` and `stall` do not return; every other step does, and a
scenario that names neither still exits `0`. With no scenario the default is
`outcome` (a clean exit `0`).

The scenario is chosen from, in order: `FAKE_AGENT_SCENARIO`, a bare positional
argument (`fake-agent edit+commit`) or `--scenario`, a
`FAKE_AGENT_SCENARIO: <scenario>` marker line in the prompt, then the
`outcome` default.

### Checkpoint trailers

The `commit` step writes two git trailers onto every commit it makes:

```
Vogt-Task: <VOGT_ENGINE_AGENT_TASK_ID>
Vogt-Run: <VOGT_ENGINE_AGENT_TASK_RUN_ID>
```

Nothing in the engine reads these; they make a checkpoint a run made traceable
back to the run that made it.

## Configuration knobs

Everything a scenario needs beyond the prompt comes from `FAKE_AGENT_*`
environment variables, so a caller sets behaviour without editing the script:

| Variable | Default | Used by |
|---|---|---|
| `FAKE_AGENT_SCENARIO` | `outcome` | scenario selection |
| `FAKE_AGENT_NOTIFY_PHRASE` | `VOGT_NOTIFY:` | `findings` |
| `FAKE_AGENT_NOTIFY_TEXT` | `fake-agent synthetic finding` | `findings` |
| `FAKE_AGENT_NOTIFY_DELAY` | `0.3` | `findings` (lets the watcher subscribe first) |
| `FAKE_AGENT_FINDINGS_FILE` | *(unset)* | `findings` — optional JSON dump path |
| `FAKE_AGENT_EXIT_CODE` | `0` | `outcome` |
| `FAKE_AGENT_EDIT_FILE` | `fake-agent-edit.txt` | `edit` |
| `FAKE_AGENT_EDIT_TEXT` | `edited by fake-agent for run <run_id>` | `edit` |
| `FAKE_AGENT_COMMIT_MESSAGE` | `chore: fake-agent checkpoint` | `commit` |
| `FAKE_AGENT_STALL_SECONDS` | `3600` | `stall` |
| `FAKE_AGENT_IDLE_PROMPT` | `fake-agent idle> ` | `idle` |
| `FAKE_AGENT_SKIP_REASON` | `nothing to do` | `skip` |
| `FAKE_AGENT_COST` | `{"total_usd": 0.42, "input_tokens": 1200, "output_tokens": 340}` | `cost` |
| `FAKE_AGENT_SCHEMA_PASS_ON` | `1` | `schema` — the attempt on which a passing block is printed |
| `FAKE_AGENT_SCHEMA_DELAY` | `0.3` | `schema` (lets the watcher subscribe first) |
| `FAKE_AGENT_SCHEMA_GOOD` | `{"summary": "did the thing", "risk": "low"}` | `schema` |
| `FAKE_AGENT_SCHEMA_BAD` | `{"summary": "did the thing"}` | `schema` |
| `FAKE_AGENT_PYTHON` | `python3` | the shell entry |

Run `fake-agent --print-contract` for the same information as machine-readable
JSON.

## How it is wired for tests

- **As a session preset.** The engine's *test* config registers it as a
  session template (in
  [`../engine/server/tests/integration.rs`](../engine/server/tests/integration.rs)),
  the same way a deployment registers a real agent CLI. It is deliberately kept
  out of the production `SessionTemplate::default_templates()`.
- **Through the engine.** `integration.rs` creates agent tasks whose `command`
  is the fake-agent and asserts, end to end: an `edit+commit` run leaves a
  commit whose `Vogt-Run` trailer equals the run the engine started; a
  `findings` run has its `VOGT_NOTIFY:` line recorded as a finding on the run;
  an `outcome` run surfaces its exit code as an errored run.
- **Against the contract.** [`../tests/test_fake_agent.py`](../tests/test_fake_agent.py)
  exercises every scenario directly, including the `idle`/steer and `stall`
  paths that need PTY input the HTTP suite cannot easily supply.

## Example

```sh
# Report a finding, then exit non-zero — as the engine would drive it.
FAKE_AGENT_NOTIFY_TEXT="the price dropped" \
FAKE_AGENT_EXIT_CODE=1 \
VOGT_ENGINE_AGENT_TASK_RUN_ID=run-123 \
  scripts/fake-agent findings+outcome
```
