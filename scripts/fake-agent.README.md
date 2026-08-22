# `fake-agent` — a synthetic agent CLI for tests

An agent task drives a real coding-agent CLI — `claude`, `codex`, `opencode`,
usually wrapped as `mydevenv2-agent-auth run -- <cli>` — inside a PTY. CI has
none of those binaries and no way to type into a PTY, so the run-orchestration
behaviour in [`../engine/server/src/agent_tasks.rs`](../engine/server/src/agent_tasks.rs)
had no automated path. `fake-agent` is that path: it accepts the *same
invocation shape* the engine produces and plays a chosen **scenario** — no
network, no model, no wall-clock dependence beyond the small delays a scenario
opts into.

It exists so the tested-through-the-engine work can be tested: #241's findings,
and the future #289 (gates/steer), #290 (triggers), #291 (outcomes) and
#283–#285 (the git story).

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
  `MYDEVENV2_AGENT_TASK_PROMPT_FILE` (the prompt file),
  `MYDEVENV2_AGENT_TASK_CONTEXT_FILE`,
  `MYDEVENV2_AGENT_TASK_ID`,
  `MYDEVENV2_AGENT_TASK_RUN_ID`, and — when the task is Vogt-bound —
  `VOGT_PROJECT` / `VOGT_WORK_ITEM`.
- **prompt** — read from the prompt file named in the env (or `--prompt-file`).
- **findings** — a run reports a finding by printing a line beginning with the
  notify phrase (`VOGT_NOTIFY:` by default; the legacy `MYDEVENV2_NOTIFY:` is
  still matched by the engine). The engine's phrase watcher records the text
  after it on the run.
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
| `idle` | Print an idle prompt and block until a steer line arrives on stdin. |
| `outcome` | Exit with the chosen code. |
| `stall` | Sleep, to exercise the stall / idle-stall timeout. |

So `edit+commit` is "edit then commit", `findings+outcome` is "report then
exit". With no scenario the default is `outcome` (a clean exit `0`).

The scenario is chosen from, in order: `FAKE_AGENT_SCENARIO`, a bare positional
argument (`fake-agent edit+commit`), a `FAKE_AGENT_SCENARIO: <scenario>` marker
line in the prompt, then the `outcome` default.

### Checkpoint trailers

The `commit` step writes two git trailers onto every commit it makes:

```
Vogt-Task: <MYDEVENV2_AGENT_TASK_ID>
Vogt-Run: <MYDEVENV2_AGENT_TASK_RUN_ID>
```

There is no engine-side reader for these yet — the git story (#283–#285) is
future work — so this is the format that work can standardise on: a checkpoint
a run made, traceable back to the run that made it.

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
| `FAKE_AGENT_PYTHON` | `python3` | the shell entry |

Run `fake-agent --print-contract` for the same information as machine-readable
JSON.

## How it is wired for tests

- **As a session preset.** The engine's *test* config registers it as the
  `Fake Agent (test)` session template (in
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
MYDEVENV2_AGENT_TASK_RUN_ID=run-123 \
  scripts/fake-agent findings+outcome
```
