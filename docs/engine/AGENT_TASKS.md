# Agent Tasks

This note records the MyDevEnv2 implementation shape for long-lived or
recurring agents such as price monitors and recurring workspace checks.

## What Landed

- `GET/POST/PATCH/DELETE /api/agent-tasks` adds a durable scheduled-agent
  registry under `state_dir/agent-tasks.json`.
- `POST /api/agent-tasks/:id/run` launches a real PTY session through the
  existing `SessionRegistry`, so WebSocket attach, scrollback, history, auth,
  and push behavior stay on the existing path.
- The PWA exposes a dedicated Tasks tab with create/edit/pause/resume/run/delete
  actions, recent-run inspection, and open-session actions for task runs.
- Task runs persist explicit `running` / `completed` / `errored` status plus
  `completed_at`, `exit_code`, and a short summary derived from the linked
  session exit event.
- Every task run writes a prompt file under
  `state_dir/agent-task-prompts/<task-id>/<run-id>.md` plus a persistent
  `context.md` file. Agent commands receive those paths through environment
  variables and can also use `{prompt_file}` / `{context_file}` placeholders.
- Sessions created with a `prompt` (FR-E4's work-item brief) share that
  mechanism: their brief is written to
  `state_dir/agent-task-prompts/sessions/<session-id>.md` and exported as the
  same `MYDEVENV2_AGENT_TASK_PROMPT_FILE` variable, so one prompt root exists
  and `POST /api/agent-tasks/artifacts/cleanup` accounts for everything under
  it. See `engine/server/src/prompt_files.rs`.
- Tasks can schedule `manual`, `interval`, or UTC `daily` runs. The first useful
  product-monitor shape is `interval { minutes = 720 }` for twice daily.
- The default notification hook is output-driven: if an agent prints a line
  beginning with `MYDEVENV2_NOTIFY:`, the server fans out a push notification
  linking back to that run's session.

Example task payload for the Hisense PX3 monitor:

```json
{
  "name": "Hisense PX3 price monitor",
  "prompt": "In Australia, check current online prices for the Hisense PX3. Compare against prior context. If the best price is lower than the previous best, print exactly: MYDEVENV2_NOTIFY: Hisense PX3 dropped to <price> at <store>. Otherwise summarize quietly.",
  "schedule": { "kind": "interval", "minutes": 720 },
  "command": ["codex", "exec", "--prompt-file", "{prompt_file}"],
  "context": "Track best observed price, store, URL, and timestamp here."
}
```

The command is intentionally user-supplied. MyDevEnv2 should not depend on a
specific AI CLI because production deliberately leaves Codex, Claude, and other
agents user-managed.

## Odysseus Findings

Reviewed against `pewdiepie-archdaemon/odysseus` on the `dev` branch. The most
relevant pieces are:

- `src/task_scheduler.py`: persisted scheduled tasks, per-run records,
  next-run calculation, startup recovery for stale queued/running rows,
  single-slot model execution, task chaining, notification delivery, and
  check-in execution.
- `routes/task_routes.py`: task CRUD, pause/resume/run-now/stop, run history,
  output targets, webhook triggers, and natural-language-to-task drafting.
- `src/memory_provider.py` and `src/memory.py`: provider-neutral memory plus a
  local baseline store. Useful for recurring agent context, but heavier than
  MyDevEnv2 needs today.
- `src/tool_index.py`: RAG-selected tool exposure, including always-available
  tools for scheduled assistant/check-in tasks.
- `THREAT_MODEL.md`: strong reminder that web results, emails, memories, notes,
  skills, and other external content are untrusted context and must not become
  system instructions.

## Next Steps

1. Natural-language task draft endpoint: Odysseus has a good pattern, but in
   MyDevEnv2 it should produce a draft only. The user should review before a
   schedule goes active.
2. Context update workflow: let an agent append to `context.md` through a small
   authenticated helper or via a specific output marker, rather than requiring
   manual edits.
3. Webhook/event triggers: useful, but lower priority than interval/daily
   recurring tasks for price monitors.
4. Richer notification controls: per-task rules, quiet hours, and digesting
   fit naturally once the broader push surface is expanded.

Do not port wholesale:

- Odysseus' model endpoint registry, SQL chat schema, role system, email stack,
  calendar stack, or MCP manager. Those are app-defining systems in Odysseus;
  MyDevEnv2 should stay a centrally hosted terminal/workspace orchestrator.
- Automatic check-in seeding for every user. Odysseus removed that behavior
  after duplicate/intrusive tasks. MyDevEnv2 should make recurring tasks
  explicit.
