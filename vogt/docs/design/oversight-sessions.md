# Oversight sessions (WI-956)

Status: **design, with slices 0 and 1 implemented** (WI-957, the session role
and the rail order; WI-962, conversation capture, history identity and resume).
Slice 1 leaves out the core mirror event (step 4) and the role chooser in the
new-session dialog. Everything after slice 1 is a proposal. Where this note and
[`ENGINE.md`](../ENGINE.md) or [`API.md`](../API.md) disagree, those describe
what exists.

## Problem

An *oversight session* is a long-lived agent session that supervises the
others. It starts and drives worker sessions, checks on them, revives them
after a restart, and carries rollouts through. On 2026-10-07 one was lost to
a vogt-prod redeploy and was hard to find again:

- History showed it only as a generic shell. It had been started as a shell
  with `claude` typed into it, so the engine never knew there was an agent
  conversation in it.
- Its Claude conversation id was linked to nothing in Vogt. It was found by
  reading transcripts and recovered by hand with
  `session_start(template=claude, resume=<conversation id>)`.
- An unrelated new shell happened to be named "Oversight". A name is not a
  role.

The same thing happened on 2026-10-04 and 2026-10-05
([session-hibernation](session-hibernation.md#problem)). Hibernation and
`keep_awake` (WI-912) fix it for agent sessions the engine launched. They do
not fix it for an overseer the engine cannot see as an agent, and nothing in
Vogt says which session is the overseer, what it oversees, or where it had
got to.

A second requirement came from the operator during this work. **An overseer
spawns the work it drives as Vogt sessions** (`session_start` with a
template, project, work item, task and model). It does not spawn them as
subagents inside its own conversation. A Vogt session is visible in the GUI,
tracked, resumable and overseeable, and it outlives the overseer, for example
across a prod redeploy. A subagent dies with its parent and nobody else can
see it.

## What exists today

| Layer | What is there | Gap for oversight |
| --- | --- | --- |
| DB (`declared`) | `coding_sessions` (0007, 0010): project, work item, actor, cwd, template, model, effort, reason, started/stopped. Holds no live state, on purpose. The audit row of `session.start` records the *caller's* actor. | No role. No link to the session that started it, except by joining the audit row's actor `agent:session:ses_X` back to a session. No conversation id. |
| DB (`declared`) | `actor_preferences` (0018): a per-actor JSON value, versioned for compare-and-set and audited. `comments` on work items. | Keyed by actor. A new `session.start` mints a new actor, so a successor overseer could not read its predecessor's state. |
| Events | Core `events` hold audited writes only: `session.started/stopped/input/hibernated/woken/blocked/…`. The engine bus has `Activity`, `Killed`, `SessionHibernated`, `SessionBlocked`. The engine follows the core's feed (`vogt_core.rs` `spawn_event_follower`), but **nothing feeds engine activity back into the core**. | An overseer cannot ask "tell me when any of *my* sessions finishes, blocks or errors". It polls `session_sweep`, or waits on one session (`session_wait`). |
| Notifications | Inbox entries for sessions are projected live from the engine list (`inbox.py`): blocked, waiting, approval, errored. Web push goes to every device, filtered by kind. | Routed to the person only. Nothing routes to an overseer, and nothing is per-session. |
| Engine record | `state_dir/sessions/<uuid>.json`: command, cwd, env without secrets, model, effort, conversation, `keep_awake`, `autopilot`, and now `role`. It is written at spawn and survives a redeploy. `keep_awake` sessions are woken at boot through the core's `session.wake`. | `recover_records` **drops** a record that has no conversation and was not hibernated on purpose (a shell). The hand-typed overseer was exactly that. |
| Engine history | `history.sqlite` row: name, cwd, command, created/ended, exit code, end reason, FTS over output. | No template, conversation id or role. A lost overseer reads as `bash`. |
| Conversation id | Known only from the launch argv (`agent_cli::conversation`): a bare `claude` is pinned to the engine UUID, and `--resume <id>` is that id. opencode is captured afterwards from its store. | An agent typed into a shell is invisible. The core's transcript lookup can guess by cwd and mtime, but only `session_last_reply` uses the guess. |
| Driving | `session_start/input/wait/answer/sweep/last_reply/screen`; the WI-915 oversight board (`session.sweep`, `web/src/Oversight.tsx`). Any session token holds every scope except `admin`. | No "supervised by" concept. Nothing stops an overseer from using subagents instead, and nothing tells it not to. |
| Permissions | WI-926: `permission_mode=bypass` is a person's grant only, refused for every agent including the overseer. | Prod-changing work an overseer starts is denied under the default policy. The only path is a blocked report followed by a person acting. |
| Scheduling | Agent tasks: interval/daily/trigger runs, each a fresh PTY with no conversation. | Stateless, so not a host for an overseer. They could provide a periodic tick (see slice 3). |

## Recommendations

### 1. A first-class role (slice 0, implemented)

A session has a **role**: `worker` (the default) or `oversight`. It is an
engine-owned flag beside `keep_awake` and `autopilot`, for three reasons:

- It must survive exactly what the engine record survives: hibernation, a
  wake and a redeploy.
- A session started from the GUI has no `coding_sessions` row to hold it.
  The incident session was one of those.
- The PWA rail lists engine sessions, and the role is read where they are.

The core records the declared half durably. `session.start(role=…)` names the
role on its audit row, and `session.set_role` is an audited action. "Which
session was the overseer, and who said so" therefore has an answer after the
engine has forgotten the session.

Becoming oversight also **pins the session awake**. The overseer is the
session that brings the others back, so it has to come back by itself.
Removing oversight (back to worker) lifts that pin again, so the session
falls back under the ordinary idle/hibernate policy (WI-1091). A worker
pinned on its own with `session.keep_awake` keeps its pin when told it is a
worker.

In the PWA, the session menu offers **Make oversight** on a worker and
**Remove oversight** on an oversight session. That is the rail row's menu on
a desk, and the terminal's "⋯" menu on both a desk and a phone. Both call
the core's `session.set_role`, which only a person may do. Without it, the
only way to stop a session being the overseer was to kill it. The rail, the
phone's Sessions list and its pager list oversight sessions first and
reorder once the role changes; the phone terminal pager keeps attention order.

A name-based fallback ("a session called Oversight") was rejected. The
incident's decoy shell is the counter-example.

### 2. Durable conversation linkage (slice 1)

Make every agent conversation findable, whatever started it:

1. **Capture the conversation id from inside the agent.** Claude Code
   passes `session_id` to its `SessionStart` hook. The pod's user-level
   Claude settings (the same entrypoint step that already dismisses
   onboarding) add a `SessionStart` hook. The hook posts
   `{agent: "claude", id}` to `POST /api/sessions/$VOGT_SESSION_ID/conversation`,
   using the session's own credential, and the engine attaches it to the
   session and its record. This covers `claude` typed into a shell, `/clear`
   (a new id) and `--resume` (the id it actually resumed). The same route
   serves codex and opencode once their ids are readable. The id is
   validated with `agent_cli::is_conversation_id`.
   *Verified on vogt-dev (2026-10-07), with one correction:* the hook fires
   for a `claude` typed into an engine shell, but the shell has no
   `$VOGT_SESSION_ID` (only a session vogt-core started does). It has
   `$VOGT_ENGINE_SESSION_ID` and the session's broker token, so the hook as
   built posts to `POST /api/agent-auth/conversation` with the broker token,
   which names the session by itself, and falls back to
   `POST /api/sessions/$VOGT_ENGINE_SESSION_ID/conversation` with
   `VOGT_HTTP_TOKEN` where nothing is brokered. A `SessionEnd` hook unlinks the
   conversation again, and a `claude` whose stdin is not a terminal (a
   `claude -p` from a tool call) is ignored. See ENGINE.md, "A conversation
   reported from inside the session".
2. **History rows carry identity.** Add `template`, `conversation_agent`,
   `conversation_id` and `role` to the engine history row (a history schema
   migration), and show them in History and `session_history_list`. A lost
   overseer then reads as "oversight · claude · conversation 6c1f…", not
   `bash`, and the History row offers *Resume* (`session.start` with
   `resume` and the same role).
3. **Do not drop what can be resumed.** `recover_records` keeps a shell
   record once a conversation has been attached to it (step 1), since it is
   now resumable. It also keeps a shell record with `role = oversight`, as a
   non-resumable hibernated entry, so the overseer stays listed and
   findable. Boot wake skips non-resumable records, because waking them
   would only open an empty shell.
4. **Mirror the link in the core** (not built in slice 1; the engine logs
   `event=session.conversation` and keeps it in the record and History).
   Write a `session.conversation_linked`
   event: an audited action carrying the engine id, agent and conversation
   id. This leaves a durable trail in `vogt.sqlite3` even if the engine's
   `state_dir` is lost. A column on `coding_sessions` was considered and
   rejected. A conversation id can change within one session (`/clear`), and
   0007 keeps the engine's state out of that table.

### 3. Supervision linkage (slice 2)

- **Who supervises whom is derived from the principal, never a parameter.**
  When `session.start` is called by `agent:session:ses_X` or
  `agent:engine:<uuid>`, the new session's supervisor is that session. This
  follows the repository rule that a principal comes from the adapter
  context. Store it as `coding_sessions.supervisor_session_id` in a new
  migration (`0019_session_supervisor`), nullable, since a person-started
  session has none. Send it to the engine as `SessionSpec.supervisor` so the
  record and summary carry it too, including for a GUI-started overseer
  that has no core row.
- **Adopt and release.** `session.supervise(id, supervisor?)` (audited)
  hands a worker to another overseer, for example a successor after a lost
  overseer, or releases it.
- **Read paths.** `session.list` and `session.sweep` take
  `supervised_by=<id|me>`. Rows carry `supervisor`, and the sweep counts
  `needs_you` per overseer.
- **GUI.** The rail lists each oversight session first (slice 0), with the
  sessions it supervises nested under it (collapsible, with a count and the
  most urgent child's state). Unsupervised sessions follow. The Oversight
  board groups the same way.

### 4. Notifications to the overseer (slice 3)

The overseer should not have to poll. The engine already has every
transition on its bus. What is missing is routing.

1. **`session.wait_any`** (MCP `session_wait_any`, `read` scope). It blocks
   until any of a set of sessions, or `supervised_by=me`, reaches one of
   `until=[ready, blocked, awaiting-approval, exited, errored]`, and returns
   which session and why. It is implemented in the engine next to `wait.rs`
   (`GET /api/sessions/wait-any`), with the same subscribe-before-check
   order, the same 600 s cap and a cursor so nothing is missed between
   calls. This turns the overseer's loop into "wait_any, act, repeat".
2. **Idle injection, opt-in per overseer.** When a supervised session
   blocks, errors or exits while the overseer sits idle at its prompt, the
   engine types one line into the overseer. The line is clearly delimited
   and treated as untrusted data, for example
   `[vogt] WI-957 session 3f2a… blocked: needs the bot token`. This reuses
   the autopilot nudge machinery (ready detection, never when blocked or
   mid-turn). It wakes an overseer that is not inside a `wait_any`.
3. **Core events for engine transitions.** The core's engine follower
   records `session.exited`, `session.errored` and
   `session.awaiting_approval` as observed events (not audited writes) for
   linked sessions. Afterwards, `events_list(entity=…)` answers what
   happened to a session, and the Inbox no longer depends on the engine
   being up.
4. Push to the person is unchanged. The overseer's own `blocked` report is
   still how it asks a person for something.

### 5. Oversight state and checkpoints (slice 4)

An overseer's working memory (the rollout it is driving, what it last
checked, what it is waiting on) lives in its conversation. It is lost if the
conversation is lost and cannot be read by a successor or by the operator.
Persist it in Vogt:

- **An oversight charter**, a new declared entity `oversights` (migration
  `0020`): `id`, `name`, `scope` (projects, work items, or an initiative),
  `brief`, `current_session_id`, `state` (`active` / `paused` / `done`),
  `created_by` and `created_at`. The charter outlives any one session. A
  session *holds* a charter, and `session.set_role(oversight, charter=…)`
  attaches it, recording the hand-over in the audit log. This answers "what
  was the overseer doing" independently of which session or actor did it,
  which per-actor storage cannot do.
- **Checkpoints**, `oversight_checkpoints`: append-only, audited JSON
  entries, each with a short summary, `waiting_on` (session ids or work
  refs), `next_steps` and `version`. They are written by
  `oversight.checkpoint` and read with `oversight.get` (latest plus
  history). An overseer writes one after each meaningful step. A resumed or
  successor overseer reads the latest checkpoint first.
- **Work item comments stay the narrative.** Progress on a work item is
  still a `work_comment`. Checkpoints are the overseer's own state, not a
  second comment stream.
- **GUI.** An oversight session's pane gets a side panel with its charter,
  latest checkpoint and supervised sessions. The Oversight board shows one
  section per charter.

### 6. Auto-revive after a redeploy (slices 0 and 1)

- Slice 0: an oversight session is pinned, so the engine wakes it at boot
  through the core with a new token for the same actor (WI-912). This
  requires a known conversation, which slice 1 provides for hand-typed
  agents.
- Slice 1: after a boot wake, a resumed Claude sits idle at its prompt. For
  an oversight session, the engine types one line after `ready`. As built
  (WI-962), before checkpoints exist, it reads `[vogt] This oversight session
  was resumed after the engine restarted at <time>. Check on the sessions you
  oversee (session_sweep), wake the ones you need, and carry on where you
  left off.` Slice 4 adds "read your latest checkpoint (oversight_get)".
  Workers are not nudged. They wake on demand,
  and the overseer decides which to wake (`session_input` wakes one, and so
  does `session_wake`).
- Failure stays visible. If the core is unreachable, the overseer stays
  hibernated and is listed first, with *Wake* in its menu. This behaviour
  already exists.

### 7. Spawning work as sessions, with the right model (slice 1, guidance; slice 5, profiles)

- **Guidance now.** The `AGENT_GUIDE.md` driving section and the brief an
  oversight session gets say: start the work you drive with `session_start`
  (template, project, work item, task, model), never as subagents. A
  subagent is invisible to Vogt, cannot be overseen or resumed, and dies with
  you. Slice 0 adds this to `AGENT_GUIDE.md`; the oversight brief gets it
  in slice 1.
- **Model per session exists today.** `session_start(model=…, effort=…)`
  passes the model through to the CLI and refuses rather than ignoring it.
- **Purpose profiles (slice 5).** The deployment defines named profiles in
  the engine config, beside `session_templates`, for example
  `review → template claude, model <Fable id>` and
  `implement → template claude, model <Opus id>, autopilot`.
  `session_start(profile="review", work_item=…)` expands them, and an
  explicit `model` still wins. The operator's preference (Fable for review,
  Opus for implementation) then lives in configuration, not in every
  overseer's prompt. Profile names are the deployment's. Vogt ships none.

### 8. The permission gap (slice 6)

An agent cannot grant `permission_mode=bypass` (WI-926), so prod-changing
work started by an overseer is denied under the default policy. The
guardrail is right. What is missing is a fast, auditable way for a person to
say yes. Two options, in order:

*Generalised by [approved grants](oversight-grants.md) (WI-973): any one
scoped item — a named credential (milestone 1, built) or a capability such as
bypass (milestone 2) — for a live session, requested by an overseer and
approved by a person in the Inbox.*

1. **Ask instead of refuse (recommended first).** When an agent calls
   `session.start` with `permission_mode=bypass`, the core creates a
   **pending start** instead of returning `BypassRefused`. The pending start
   is a declared row with the full spec, the requesting session, its reason
   and an expiry (default 1 h). It appears in the Inbox (`session.bypass_request`)
   and as a push notification. A person approves it in the GUI. The core
   then starts the session **under the approving person's principal**
   (audited with both actors), and the overseer is told through `wait_any`
   or the returned request id (`session.start_request_get`). Declining or
   expiring tells the overseer too. No agent ever holds the grant, and the
   person sees exactly what will run.
2. **Standing delegation (later, an operator decision).** A person with
   `admin` creates a delegation grant: an oversight charter (or a session),
   the highest posture it may hand out, the projects or repositories it
   covers, a cap on concurrent children, an expiry and a reason. An agent's
   `bypass` start inside an active grant proceeds, carries the grant id on
   its audit row, and shows in the GUI as "bypass via delegation <id>". A
   grant is revocable at once, and revoking it does not kill running
   children, which the overseer or a person stops. This removes the human
   from the loop, which is why it is the second option and needs an explicit
   operator decision.

Either way, the worker still runs the WI-926 driven-session policy unless
bypass was granted, and a denial still becomes a `blocked` report that the
overseer sees through `wait_any`.

## MCP tools an overseer needs

| Tool | Status |
| --- | --- |
| `session_start(role, model, effort, task, work_item, autopilot, permission_mode)` | exists; `role` added in slice 0 |
| `session_set_role` | **slice 0** |
| `session_list` / `session_sweep` with `supervised_by` | slice 2 (the tools exist; the filter is new) |
| `session_supervise` (adopt or release) | slice 2 |
| `session_wait_any` | slice 3 |
| `session_wait`, `session_input`, `session_answer`, `session_last_reply`, `session_screen`, `session_wake`, `session_hibernate`, `session_keep_awake`, `session_stop` | exist |
| `oversight_create`, `oversight_get`, `oversight_checkpoint`, `oversight_list` | slice 4 |
| `session_history_list` with conversation, role and *resume* | slice 1 |
| `session_start_request_get` (pending bypass start) | slice 6 |

## Phases and effort

Estimates are for one engineer familiar with the codebase, including tests
and docs.

| Slice | Content | Layers | Effort |
| --- | --- | --- | --- |
| 0 (WI-957, done) | `role` on spec, summary and record; `POST /api/sessions/{id}/role`; `session.start(role)`, `session.set_role` (audited); oversight pins awake; rail sorts oversight first, with a badge and a menu toggle; guidance on spawning sessions | engine, core, MCP/CLI/REST, PWA, docs | ~1 day (S) |
| 1 (WI-962, done but for the last two) | Conversation capture via a `SessionStart` hook and `POST …/conversation`; history rows carry template, conversation and role, with *Resume* from History; keep oversight and resumable shell records at boot; resume nudge for a woken overseer; `session.conversation_linked` event; role chooser in the new-session dialog | engine (+ history migration), core, PWA, entrypoint | 3–4 days (M) |
| 2 | Supervisor derived from the principal (`0019`, `SessionSpec.supervisor`); `session.supervise`; `supervised_by` on list and sweep; rail nesting and board grouping | core, engine, PWA | ~3 days (M) |
| 3 | `session.wait_any` (engine route and core op); opt-in idle injection; core-recorded exit, error and approval events | engine, core, MCP | 4–5 days (M/L) |
| 4 | Oversight charters and checkpoints (`0020`), `oversight.*` ops, overseer side panel | core, PWA | 4–5 days (M/L) |
| 5 | Purpose profiles (`review`, `implement`, …) in engine config; `session_start(profile=…)` | engine config, core | 1–2 days (S) |
| 6a | Pending bypass starts approved in the Inbox | core, PWA, push | ~3 days (M) |
| 6b | Standing delegation grants (operator decision first) | core, PWA | 3–4 days (M) |

Recommended order: 0 → 1 → 3 → 2 → 6a → 4 → 5 → 6b. Slice 1 closes the
incident's failure mode. Slice 3 removes polling, which is the overseer's
largest cost. Slice 2 makes the GUI legible once there are many children.
Slice 6a unblocks prod work without weakening WI-926.

## Operator decisions

1. **Oversight pins awake.** It is on in slice 0. The alternative is a
   separate opt-in, which would reopen the incident.
2. **More than one overseer at a time.** It is allowed: the role is a flag,
   not a singleton. Charters (slice 4) are how two overseers avoid
   overlapping scope.
3. **Idle injection into the overseer** (slice 3) is off by default, per
   overseer.
4. **Pending bypass starts** (6a): who may approve (any person, or `admin`
   only) and the default expiry.
5. **Standing delegation** (6b): whether to build it at all.
