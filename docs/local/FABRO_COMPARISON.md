# Fabro vs Vogt — comparison and what to take

Operator-local working note (git-ignored under `docs/local/`). Written
2026-08-22 from a shallow clone of https://github.com/fabro-sh/fabro at
`v0.333.0-nightly.0` (HEAD 2026-08-22). Fabro moves fast; re-check before
acting on any specific claim below.

## One-line verdict

**Fabro is an execution engine with no work layer; Vogt is a work layer with a
thin execution surface.** They are complementary, not competitors. Take
Fabro's *orchestration* ideas (checkpoint branches, gates, steering, triggers,
typed outcomes, agent-executable onboarding); do not take its *agent loop*
(its own coding agent, 15 LLM providers, cloud sandboxes). Consider integrating
Fabro as an agent-task backend before re-building its execution features at
scale.

## What Fabro is

- Rust single binary (`fabro`), Axum server + React SPA embedded via
  `rust-embed`, ~45 crates. Storage: SlateDB event log for runs + SQLite for
  config/secrets/automations. No external DB.
- **Workflows are Graphviz DOT files** (`.fabro/workflows/<name>/workflow.fabro`)
  checked into the user's repo. Node types by shape: agent (multi-turn LLM with
  tools), prompt (single call), command (bash/python), human gate, conditional,
  parallel fan-out / fan-in, wait, manager-loop sub-workflow. Edge conditions,
  retries, loop-restart circuit breakers, `for_each` fan-out, imports.
- **Its own agent loop** (`lib/components/fabro-agent`): send → receive →
  execute tools in sandbox → repeat, with built-in `shell/read_file/
  write_file/edit_file/apply_patch/grep/glob/web_*` tools, compaction, loop
  detection, sub-agents, skills (`SKILL.md`), project memory (`AGENTS.md`).
  Per-provider tool profiles. External CLIs (Claude Code, Codex) are **not**
  first-class — `backend="cli"` is documented as legacy; the escape hatch is
  ACP (Agent Client Protocol over stdio), which gives up most Fabro features.
- **Providers:** anthropic, openai, gemini, deepseek, moonshot, venice,
  poolside, zai, minimax, inception, openrouter, modal, litellm, bedrock, any
  openai-compatible. CSS-like **model stylesheets** route nodes to models;
  **fallback chains** per requested model.
- **Sandboxes:** local, Docker (default), Daytona cloud VMs; SSH, preview
  URLs, VNC, a WebSocket xterm into the sandbox in the web UI.
- **Git checkpointing:** per run, `fabro/run/{id}` (one commit per completed
  node, real file changes) + orphan `fabro/meta/{id}` (`run.json`, the graph,
  per-stage `prompt.md`/`response.md`/diffs/command output), linked by a
  `Fabro-Checkpoint:` commit trailer. Resume, rewind, fork from any checkpoint.
- **Human-in-the-loop:** gates are graph nodes; options come from edge labels
  with accelerators; gates **fail closed** (interrupted/skipped ≠ approved,
  `--auto-approve` is the only bypass). Interviews delivered via console, web
  dock, or Slack. **Steering:** `POST /runs/{id}/steer` queues a user message
  drained between LLM rounds; `interrupt=true` cancels the round first.
  Pairing = live interactive session on an active agent stage.
- **Outcomes:** every node ends `succeeded | failed | partially_succeeded |
  skipped`; a terminal `Conclusion` carries timing, retries, billing
  (`usd_micros` per stage), final sha, diff. `output_schema` (JSON Schema)
  validated and repaired in-session.
- **Observability:** one event envelope everywhere (SSE, NDJSON, export);
  run board (status columns; rows carry PR link, ±lines, CI checks, cost);
  run detail tabs incl. waterfall, files diff, billing; DuckDB "insights" SQL
  over the event log; `fabro dump` → portable run directory.
- **Automations:** saved run config (repo + ref + workflow) + triggers (API,
  5-field cron), each trigger independently enabled. SQLite-backed, UI at
  `/automations`.
- **Onboarding:** `curl …/install.md | claude` — an imperative document
  (OBJECTIVE / DONE WHEN) the user's own agent executes; web install wizard
  (LLM → server → object store → sandbox → GitHub) with a test endpoint per
  step, server exits when done; Homebrew tap; `install.sh` (needs `gh`);
  SLSA build-provenance attestations on binaries and image.
- **Secrets:** SQLite vault; `{{ secrets.X }}` resolves only at sandbox start
  in env maps / prepare steps / provider headers, never persisted resolved.
- **GitHub:** `token` strategy (captures `gh auth token`; web UI disabled) or
  GitHub App (installation tokens, OAuth login, webhooks, auto-PR,
  auto-merge, checks). GitHub only. **Issues are not modelled** — agents run
  `gh issue view` in the sandbox. No backlog, initiatives, labels-as-streams,
  drift, or any planning entity. Run `labels` and `parent_id` are the closest
  primitives.
- **Evals:** one SWE-Bench-Lite harness with a checked-in scoreboard.

## Side by side

| | Fabro | Vogt |
|---|---|---|
| Unit of work | a **run** of a DOT graph | a **work item** (forge issue + overlay) |
| Planning layer | none | backlog, ranking, `why`, initiatives, relations, drift, audit |
| Forge | GitHub only; PRs first-class, issues not modelled | GitHub (Forgejo deferred); issues are the work item, PRs observed |
| Execution | own agent loop in sandboxes | PTY sessions driving `claude`/`codex`; agent tasks on schedules |
| Who writes code | Fabro's agent (vendor CLIs are legacy/ACP) | the vendor's CLI, in a PTY the user can attach to |
| In-house LLM loop | the whole product | the assistant only (Vogt ops as tools, on-screen approval) |
| Autonomy | graph runs unattended; gates pause it | tasks run unattended; no gates, no steer, no event triggers |
| Git story | checkpoint + meta branches, rewind/fork | none yet (#283–#287) |
| Sandboxing | Docker / Daytona / local | the engine pod, user's own box |
| Observability | event log, waterfall, billing, DuckDB | audit, events, provenance/age on every answer |
| Onboarding | agent-executable install.md, web wizard | `vogt init`, hand-issued tokens (#267) |
| UI | React SPA, no PWA | Solid PWA + Android shell |

## Decision: the agent loop

**Not taking Fabro's agent loop does not make Vogt user-driven.** Vogt has two
autonomous actors and keeps both:

1. **Agent tasks** run Claude Code / Codex in a PTY unattended, on a schedule
   or on demand, bound to a project / work item. That *is* an agent loop — the
   vendor's. Fabro re-implements what those CLIs already are (file tools,
   compaction, sub-agents, tool profiles per provider) and will not out-code
   the vendor harness; its "CLIs are legacy" stance is its bet. Vogt's bet is
   the opposite: users already have the CLIs and the subscriptions; Vogt's job
   is to drive them, observe them, and let the user attach.
2. **The assistant** is a small Vogt-owned tool-use loop (OpenRouter, Vogt
   operations as tools, approval on screen). That is the right scope for an
   in-house loop: Vogt-native actions, not editing source.

Autonomy grows in the **orchestration around those actors** — triggers, gates,
steering, outcomes, checkpoints — not by owning the model call. Fabro builds
the engine and the car; Vogt builds the road.

## Take (ranked)

1. **Checkpoint + metadata branches for agent work.** Adopt Fabro's shape for
   Vogt sessions/tasks: `vogt/<ref>/<session>` run branch, one commit per
   stage with trailers naming the work item and session, plus an orphan meta
   branch holding the transcript/diff per stage. Feeds #283 (branch binding —
   read trailers, not just names), #284 (PR edge), #285 (git story), and gives
   rewind/fork for free. → under #287.
2. **Fail-closed gates and steering on agent tasks.** A task step can require
   approval; interrupted ≠ approved; `steer` queues text between the CLI's
   turns (PTY input at a prompt boundary); `interrupt` first. → new issue.
3. **Event triggers on agent tasks.** Vogt has the event source Fabro lacks:
   work item transitions, new observations, drift, PR checks. "When WI enters
   `ready`, start task T bound to it." Keep manual/interval/daily. → new issue.
4. **Typed task outcomes + conclusion.** `succeeded / failed / partial /
   skipped`, timing, retries, cost where the CLI reports it, final sha, diff;
   `output_schema` for structured findings (#241 is the embryo). `why` can
   cite outcome and cost. → new issue.
5. **Agent-executable install document.** `curl …/install.md | claude` for
   Vogt's two-container stack; fits #288 and #267. → fold into #288.
6. **First-run wizard with per-step tests.** Issue the first token, link the
   forge PAT, register the first project, test each; replaces the hand-issued
   token path in #267. → new issue or #267 scope.
7. **Small steals:** SLSA attestations (#270), secrets never persisted
   resolved, `<untrusted-…>` fencing of fan-out items in prompts, `dump` a
   task/session as a portable directory, SQL over the event log.
8. **Model routing/fallbacks** — only relevant to the assistant today; low.

## Leave

- DOT as a workflow language — wrong centre of gravity; a process is an agent
  task template bound to a work item, not a second programming model.
- Own coding-agent loop, provider catalogue, tool profiles — see Decision.
- Daytona/cloud sandboxes, SSH, VNC, preview links — orthogonal to
  self-hosted-on-your-box. Docker-isolated sessions would be the one cheap
  slice, later.
- SWE-bench evals, Slack interviews, Railway template.

## Strategic option: integrate

Fabro exposes REST + SSE and is an MCP server (`fabro_run_create/get/
interact/pair/events`). An agent-task backend of kind `fabro` — "run this
workflow in Fabro against this repo, bound to WI-7" — would give Vogt
sandboxes, checkpoints, gates and billing for one adapter, and give Fabro the
work layer it lacks. Observed-first handles it: a Fabro run is an observation
source with provenance and age; its `fabro/run/*` branches are exactly what
#283/#284 collect. Sequence: after #287, before building items 1–4 natively at
scale. Items 5–6 are worth doing regardless.

## Status

Nothing filed from this note yet. Candidates for issues: items 2, 3, 4, 6 and
the integration option; item 1 folds into #287, item 5 into #288, item 7 into
#270.
