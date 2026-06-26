# Agent Tasks And Daily Briefing

This note records the Odysseus review and the MyDevEnv2 implementation shape for
long-lived or recurring agents, price monitors, and weather-backed daily
briefing.

## What Landed

- `GET /api/briefing/daily` returns a compact daily briefing object. Today it
  includes active-session counts plus optional weather.
- `GET /api/weather` returns current + three-day weather from Open-Meteo.
  Weather can be passed per request with `latitude`/`longitude`, or configured
  with `weather_location` in `mydevenv2.toml` / `MYDEVENV2_WEATHER_*` env vars.
- `GET/POST/PATCH/DELETE /api/agent-tasks` adds a durable scheduled-agent
  registry under `state_dir/agent-tasks.json`.
- `POST /api/agent-tasks/:id/run` launches a real PTY session through the
  existing `SessionRegistry`, so WebSocket attach, scrollback, history, auth,
  and push behavior stay on the existing path.
- Every task run writes a prompt file under
  `state_dir/agent-task-prompts/<task-id>/<run-id>.md` plus a persistent
  `context.md` file. Agent commands receive those paths through environment
  variables and can also use `{prompt_file}` / `{context_file}` placeholders.
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
- `src/builtin_actions.py::action_daily_brief`: simple non-LLM daily digest of
  calendar, unread email, and todos.
- `src/task_scheduler.py::_execute_checkin`: richer assistant check-in flow that
  gathers raw data from calendar, notes, integrations, and MCP before asking the
  model to write the briefing.
- `src/memory_provider.py` and `src/memory.py`: provider-neutral memory plus a
  local baseline store. Useful for recurring agent context, but heavier than
  MyDevEnv2 needs today.
- `src/tool_index.py`: RAG-selected tool exposure, including always-available
  tools for scheduled assistant/check-in tasks.
- `THREAT_MODEL.md`: strong reminder that web results, emails, memories, notes,
  skills, and other external content are untrusted context and must not become
  system instructions.

## What To Port Next

Port in this order:

1. UI for agent tasks: list/create/edit/pause/resume/run-now, plus last-run
   links. Keep it dense and operational, like the current terminal/history
   surfaces.
2. Task run status: record started/completed/errored once session exit events
   are surfaced to the registry. Current runs are linked to sessions, but the
   registry does not yet update on process exit.
3. Natural-language task draft endpoint: Odysseus has a good pattern, but in
   MyDevEnv2 it should produce a draft only. The user should review before a
   schedule goes active.
4. Daily briefing scheduler: create a built-in agent task or non-agent push
   task that calls `/api/briefing/daily` at configured times.
5. Weather UI/config: add a Settings control for label/lat/lon/timezone and a
   daily-briefing panel.
6. Context update workflow: let an agent append to `context.md` through a small
   authenticated helper or via a specific output marker, rather than requiring
   manual edits.
7. Webhook/event triggers: useful, but lower priority than interval/daily
   recurring tasks for price monitors.

Do not port wholesale:

- Odysseus' model endpoint registry, SQL chat schema, role system, email stack,
  calendar stack, or MCP manager. Those are app-defining systems in Odysseus;
  MyDevEnv2 should stay a centrally hosted terminal/workspace orchestrator.
- Automatic check-in seeding for every user. Odysseus removed that behavior
  after duplicate/intrusive tasks. MyDevEnv2 should make recurring briefings
  explicit.

## Weather

Open-Meteo was chosen because it requires no API key and supports the current
and daily fields needed for a briefing: current temperature/apparent
temperature/weather code/wind speed, and daily min/max temperatures,
precipitation probability, precipitation sum, and weather code.

Config example:

```toml
[weather_location]
label = "Sydney"
latitude = -33.8688
longitude = 151.2093
timezone = "Australia/Sydney"
```

Environment equivalent:

```bash
MYDEVENV2_WEATHER_LABEL=Sydney
MYDEVENV2_WEATHER_LATITUDE=-33.8688
MYDEVENV2_WEATHER_LONGITUDE=151.2093
MYDEVENV2_WEATHER_TIMEZONE=Australia/Sydney
```
