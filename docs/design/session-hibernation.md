# Session hibernation (WI-912)

Status: **proposed**. Slices land behind this note. Where this note and
[`ENGINE.md`](../ENGINE.md) disagree, `ENGINE.md` describes what exists.

## Problem

An idle agent session costs about 250–290 MiB RSS: one `claude` process
(median ~200 MiB, up to 430) plus ~55 MiB of MCP servers. On 2026-10-05
vogt-prod held 30 live sessions. 22 of them were idle restores, about 6 GiB
in a pod with no memory limit, on a node whose swap was full
(`node-b-memory-audit-2026-10-05`, WI-893).

The sessions themselves are cheap to keep: the agent CLI's own transcript
under `~/.claude/projects` (or `~/.codex/sessions`) holds the conversation,
and `claude --resume <id>` continues it (WI-833, WI-871). What costs RAM is
the process tree. What a redeploy destroys is the *engine's memory* of which
sessions existed. The engine keeps sessions only in memory, so after the
2026-10-04 and 2026-10-05 redeploys an operator had to grep transcripts and
`session_start(resume=…)` each session by hand. One of those sessions was the
oversight runner that would otherwise have done the restore.

## Model

A session is in one of three states. Hibernation adds the middle one:

```text
            hibernate (manual, policy, shutdown, boot recovery)
  live  ───────────────────────────────────────────────▶  hibernated
   ▲                                                         │
   └──────── wake (session_input, session_wait, GUI tap) ────┘
   │
   └── kill / exit ──▶ exited            hibernated ── delete ──▶ gone
```

- **live**: a PTY with a running process tree, as today.
- **hibernated**: no process and no PTY. The engine still lists the session
  under the **same engine UUID**, with `alive: false`,
  `activity: "hibernated"` and a `hibernation` object (when, why, agent,
  conversation id). Attach and `/screen` serve the last screen. Input is
  refused with `409` and a hint to wake.
- **exited**: unchanged.

The engine UUID is kept on wake. This is the most important property: the
core's `coding_sessions.engine_session_id`, the PWA's open tabs, the history
row and log (`history/<uuid>.log`, appended to), and a fresh Claude
conversation's id (pinned with `--session-id <uuid>` since WI-833) all key on
it. Nothing has to be re-linked.

## Ownership: the engine owns the record

The engine owns the process, so it owns the hibernation record. It writes one
JSON file per session under `state_dir/sessions/<uuid>.json`. `state_dir`
lives on the persistent home volume in the stack (`engine-home`), next to the
brief files and the agent CLIs' transcripts that a resume needs anyway.

The core does **not** keep its own wake list. Its `coding_sessions` row
already records what Vogt asked for (project, item, actor, template, cwd).
The engine record adds what only the engine knows (the resolved command,
env, conversation id, and screen). A second copy in the core would be a
cached claim about another process's state, which
[0007_sessions](../../src/vogt/storage/sqlite/migrations/declared/0007_sessions.sql)
rejects on purpose.

### What is persisted

Written when the session is **spawned** (write-ahead) and updated on
hibernate:

| Field | Source | Why |
| --- | --- | --- |
| `id`, `name`, `created_at` | registry | identity, display |
| `template`, `command` (template-expanded, *before* the launch rewrite) | spec | re-launch the same CLI and wrapper |
| `cwd` | resolved spec | resume runs where the conversation is keyed |
| `env` with secrets removed (`pty::is_secret_env`) | spec | template and caller env, `VOGT_SESSION_ID`, prompt-file path |
| `model`, `effort` | spec | same model on wake |
| `agent`, `conversation_id` | resume id, or the pinned engine UUID for a bare Claude launch | what `--resume` takes |
| `brief_file` | `state_dir/agent-task-prompts/sessions/<uuid>.md` | kept, so the agent can re-read it; **not** re-sent as a first prompt |
| `hibernated_at`, `reason`, `trigger` (`manual`/`idle`/`shutdown`/`recovered`) | hibernate | audit, display |
| `screen`: the last ≤256 KiB of scrollback, rows, cols | scrollback ring at hibernate | last screen for attach and `/screen` |
| `last_input_at`, `last_output_at` | session | idle display |

**Never persisted:** any variable `is_secret_env` matches. That covers
`VOGT_HTTP_TOKEN`, the broker token, and anything with `TOKEN`, `SECRET`,
`PASSWORD` or `API_KEY` in its name. A woken session gets a freshly minted
broker grant from the engine. Its Vogt token comes from the core (see
[Wake](#wake)). The file is mode `0600`.

### Which sessions can hibernate

Only an agent session with a **known conversation id**:

- Claude Code started bare by the engine: the id is the engine UUID.
- Any agent started with `resume`: the id is the resume id. `claude --resume`
  keeps the id unless `--fork-session` is given, which the engine never adds.
- Codex and OpenCode started fresh: **not in v1**. Their id is minted by the
  CLI and is only discoverable by a cwd and time guess (the core's
  `transcripts` basis `cwd`). A wrong guess would resume somebody else's
  conversation, so these are refused with a reason. A follow-up can read the
  id from the transcript once it is unambiguous.
- A plain shell: refused unless the request sets `allow_shell`. A shell
  cannot be resumed; it reopens as a fresh shell in the same cwd with the
  last screen shown above it. This is opt-in per call, never by policy.

A refusal says why (`409`, e.g. "codex conversation id unknown"), so the
policy and the operator see the same reason.

## Hibernate

1. Snapshot the screen (scrollback tail, rows, cols) into the record and
   write it atomically (temp file + rename).
2. Stop the process tree: `SIGTERM` to the PTY child's process group, a
   grace period (default 5 s) so the CLI flushes its transcript, then
   `SIGKILL` to the group and to any descendant still found under `/proc`.
   Today's `kill` sends `SIGKILL` to the child pid alone, which can orphan
   MCP servers. Hibernation must free their RAM too.
3. Archive to history (`end_reason = "hibernated"`), revoke the broker
   grant, swap the live `Session` for the hibernated entry in the registry,
   and publish `session-hibernated` on `/api/events`.

## Wake

`POST /api/sessions/{id}/wake` with optional `env` (and `cols`/`rows`)
rebuilds the spec from the record: the same command, cwd, model and effort,
`resume = conversation_id`, **no** brief prompt, and the stored env plus the
request's `env` on top. It then spawns under the **same UUID**. Waking a live
session is a no-op that returns it. The history row and log continue.

**Through the core.** For a session Vogt linked, `session.wake` mints a new
token for the session's existing actor, revokes the old one, and calls the
engine wake with `VOGT_HTTP_TOKEN` in `env`. Attribution is unchanged: the
same actor, under a new credential. The core also wakes implicitly:

Hibernating through the core (`session.hibernate`) also revokes the
session's token. Nothing runs to hold it while the session sleeps.

| Call on a hibernated session | Behaviour |
| --- | --- |
| `session_input` | wake, wait for `ready` (bounded), then type |
| `session_wait` | **no wake**: it is a `read`-scope operation, and waking spawns a process and mints a token. It answers at once with `outcome: "hibernated"` and the kept screen |
| `session_screen`, `session_log_tail`, `session_last_reply` | **no wake**; served from the record or transcript, marked `hibernated` |
| `session_stop` | delete the record (and revoke the token, as today) |
| GUI attach | **no wake**; replays the stored screen, then a `{"type":"hibernated"}` frame; the pane shows *Hibernated — tap to wake*, which calls `session.wake` |

Opening a pane does not wake, because the PWA pre-warms panes and restores
tabs on reload. Waking on attach would bring back every hibernated session
the first time someone opened the GUI.

## Auto-hibernate policy

An engine-side watcher runs every minute, off unless configured:

- `ENGINE_HIBERNATE_IDLE_AFTER` (duration, e.g. `2h`; unset = off). A
  session is idle when `now − max(last_input_at, last_output_at, created_at)`
  is at least that long.
- Eligible: a hibernatable agent session (above) whose activity is `idle` or
  `waiting-for-input`.
- **Exempt**, each with a stated reason:
  - activity `running` (a turn is running) or `awaiting-approval` (a
    permission dialog is open);
  - a `blocked` report is set (blocked on the operator);
  - a shell process (`bash`/`sh`/`zsh`/`dash`/`fish`) is running below the
    agent CLI: a live background shell or a running tool;
  - `keep_awake` is set (`POST /api/sessions/{id}/keep-awake`);
  - the conversation id is unknown.
- **Memory-pressure trigger** (optional, off by default):
  `ENGINE_HIBERNATE_MEMAVAILABLE_BELOW` (e.g. `2GiB`). When the pod's
  `memory.max − memory.current`, or else the host's `MemAvailable`, falls
  below the threshold, hibernate the longest-idle eligible session, one per
  tick, ignoring `IDLE_AFTER` but keeping every exemption.

A `/goal`-style Stop hook keeps a turn running. That reads as `running`, so
it is exempt without special handling. A hook waiting on something else
invisible to the engine is not detected: pin the session with `keep_awake`.

## Redeploys

Three measures, so that losing the pod no longer loses the list:

1. **Write-ahead record at spawn.** Every hibernatable session's record exists
   from the moment it starts, so even a `SIGKILL`ed engine leaves it behind.
2. **Hibernate on shutdown.** On `SIGTERM`, the engine hibernates every
   hibernatable live session (snapshot, then stop) before the existing history
   drain. Hibernating is bounded by the shutdown grace, and the snapshots are
   written first, so a short grace loses only the stop, never the record.
3. **Recover at boot.** At startup, any record without a live process becomes
   a hibernated session, `trigger = "recovered"` (screen from the record when
   present, else from the history log tail). Then:
   - sessions with `keep_awake` are **woken at boot**. This is how the
     oversight runner survives a redeploy: pin it, and it comes back by
     itself with the core minting its token (see below);
   - everything else waits to be woken on demand. That avoids recreating the
     6 GiB spike the moment the pod starts.

A linked session woken at boot by the engine has no `VOGT_HTTP_TOKEN` (the
engine never stores it). The boot wake therefore asks the core:
`session.wake` with the stack secret. If the core is unreachable, the session
stays hibernated and the failure is logged. Waking it without credentials
would leave it unable to write to Vogt.

## Startup prompts on a woken session

A woken Claude session is driven by nobody until somebody types, so a modal
at startup reads as a hung session. Three prompts are possible:

| Prompt | Fix |
| --- | --- |
| Read outside the working directory (the brief under `state_dir`) | A wake never sends the brief prompt. A fresh start with a brief adds `--add-dir <brief dir>` so reading it needs no approval. |
| Folder trust ("Do you trust the files in this folder?") | Before spawning Claude in a cwd, the engine sets `projects[<cwd>].hasTrustDialogAccepted = true` in `~/.claude.json`. The write is atomic and best effort, and is skipped with `ENGINE_AGENT_QUIET_ONBOARDING=0`. The cwd is already inside `workspace_root`, which the operator trusts by deploying. |
| External `CLAUDE.md` import approval | In the same write, `hasClaudeMdExternalIncludesApproved = true` and `hasClaudeMdExternalIncludesWarningShown = true` for that cwd. |

The `~/.claude.json` write races with running Claude processes that rewrite
the file. It is done immediately before spawn, and a lost write only brings
back today's dialog. The existing "auto mode" dismissal in the entrypoint
stays.

## Surfaces

- Engine: `POST /api/sessions/{id}/hibernate` (`{reason, allow_shell?}`),
  `POST /api/sessions/{id}/wake` (`{env?, cols?, rows?}`),
  `POST /api/sessions/{id}/keep-awake` (`{keep_awake: bool}`); `activity:
  "hibernated"` and `hibernation`/`keep_awake` on `SessionSummary`; the
  `session-hibernated` and `session-woken` events. `DELETE` removes the
  record.
- Core (registry, so CLI/REST/MCP): `session.hibernate`, `session.wake`,
  `session.keep_awake`. Each is audited with a reason; hibernate and wake
  record the trigger. `session.list` rows carry `hibernated` details.
- PWA: hibernated rail badge, last screen with a *Tap to wake* bar, and a
  keep-awake toggle in the row menu.
- Docs: `ENGINE.md` §5, `API.md`, `AGENT_GUIDE.md`, `engine-openapi.yaml`.

## Slices

1. Engine mechanism: record, hibernate/wake routes, process-group stop,
   write-ahead record, boot recovery, hibernate-on-shutdown. Engine tests.
2. Core: `session.hibernate` / `session.wake` / `session.keep_awake`, token
   re-mint, implicit wake in `session_input`/`session_wait`, list fields,
   parity.
3. Policy: idle watcher, exemptions, keep-awake, memory-pressure trigger,
   boot wake of pinned sessions via the core.
4. Startup-prompt fixes.
5. PWA.

## Operator decisions

Defaults are chosen so that an unconfigured deployment behaves as it does
today (no auto-hibernation). Each item below is the recommendation; change it
before slice 3 lands.

1. **Idle threshold for vogt-prod.** Recommended: `ENGINE_HIBERNATE_IDLE_AFTER=2h`.
   The product default is off.
2. **Opening a pane does not wake** (tap to wake). The issue proposed wake on
   open; pre-warmed panes make that a mass wake.
3. **Reads do not wake.** `session_screen` and `session_wait` (both
   `read` scope) report the hibernated state; `session_input` and
   `session.wake` (`work.write`) wake.
4. **Blocked-on-operator sessions are exempt** indefinitely, as the issue
   says. Alternative: hibernate them after a longer threshold (e.g. 24 h),
   since a resume loses nothing.
5. **After a redeploy, only `keep_awake` sessions wake by themselves**; the
   rest wake on demand. Alternative: wake all (recreates the RAM spike).
6. **Codex/OpenCode fresh sessions are not hibernatable in v1** (the id is a
   guess).
7. **Memory-pressure trigger off by default**; suggested
   `ENGINE_HIBERNATE_MEMAVAILABLE_BELOW=2GiB` for vogt-prod once slice 3 has
   run on dev.
8. **Pre-accepting folder trust and external `CLAUDE.md` imports** for any
   cwd inside `workspace_root`. On by default, with
   `ENGINE_AGENT_QUIET_ONBOARDING=0` to opt out.
