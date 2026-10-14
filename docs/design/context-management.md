# Context management for oversight and worker sessions

Status: design, 2026-10-11, with most of the backlog shipped on main
(WI-1241, WI-1242, WI-1244 without the PreCompact hook, WI-1245 without the
auto-split, WI-1246 without the reply/commit kinds and `supervised_by`,
WI-1247, WI-1248). Tracked under initiative `ini_01M4M7ZE5221NJRCYYBZ788850`
(WI-1240). Remaining follow-ups go to the harness-review initiative
(WI-1517, H3/H4). Work items are listed at the end.
Scope: how Vogt keeps an overseer's context window small, keeps workers
inside a budget, hands work between sessions, applies project policy
without pasting it, and measures all of it.

Related designs: `oversight-sessions.md` (WI-956: supervisor linkage,
`wait_any`, charters and checkpoints, purpose profiles, all designed and
not built), WI-996 (escalation push, design only), WI-998 (session to
work-item binding, built), `session-hibernation.md`,
`driven-session-permissions.md`.

## 1. Evidence

Measured on this deployment on 2026-10-11 unless stated otherwise.

### 1.1 The overseer's context is eaten by list payloads

| Call | Size now | Why |
| --- | --- | --- |
| `session_sweep` (no params) | 388,637 chars, 155 rows | 122 of the rows are hibernated. The nested `session` object is 258k, `screen_tail` 110k. There is no row limit, no state filter, no `since`. |
| `session_list` (3 rows) | ~2,100 chars per row | 33 keys per row, nulls serialised, pretty-printed, and the whole payload is sent twice (`content[0].text` and `structuredContent`, `src/vogt/adapters/mcp/stdio.py:211-212`). The `command` field repeats the ~700-char launch prompt. |
| `initiative_list` | 78,187 chars, 36 rows | bodies always included, no `mode=summary` (`ListMode` exists only on `work.list`, `backlog`, `project.brief`). |
| `session_wait` | full screen every call | the result embeds the same object `session_screen` returns. |
| `session_log_tail` | 64 KiB default (~16k tokens) | |
| `session_last_reply n=20` | up to 400 KB | 20,000 chars per message (`src/vogt/adapters/transcripts.py:49`). |

Claude Code spills any tool result above its cap to a file, so the overseer
then reads the file back with `Read` or `jq`, paying twice. The two largest
Claude oversight transcripts on this box show exactly that pattern:

| Transcript | Assistant msgs | Compactions | Peak context | Tool-result bytes | Biggest contributor |
| --- | --- | --- | --- | --- | --- |
| `156a8a26` (oversight) | 4,961 | 3 | 967k tokens | 4.5 MB | `Bash` 1.58 MB (1,361 calls, mostly `jq` over spilled lists), `session_screen` 0.60 MB (116), `session_list` 0.35 MB (39), `session_last_reply` 0.31 MB (188), `session_start` + `session_hibernate` 0.46 MB (308 calls returning full rows) |
| `fd003f1d` (oversight) | 3,038 | 3 | 965k tokens | 16.3 MB | `Read` 14.1 MB in 55 calls (256 KB each: spilled list outputs read back whole) |

The same session issued 29 `CronCreate` calls with prompts up to 2.7k chars,
re-sent every 20 minutes. Every re-send is a full prompt plus a sweep.

### 1.2 Workers: the budget is implicit and nothing enforces it

| Source | Finding |
| --- | --- |
| Klaudia grok transcripts (top 8 by size) | 1,269 to 3,181 assistant messages each, 3 to 11 compactions each, 4 to 10 MB JSONL. These are the 120 to 270 turn, 4 to 11M token sessions in the brief. |
| Operator memory (`dlab-session-economics`) | 48k-context grok "went 30 to 56 minutes per turn and burned 3 to 4M tokens with zero file changes" in four sessions; at 128k with exact file and function pointers the same item committed in 5 minutes. |
| Operator memory | one-item workers with exact touch points finish in 20 to 70 turns; Opus integrate and review passes were efficient. |
| Engine | no turn counter, token counter, context estimate or compaction signal for any PTY session. The only counters are `autopilot_nudges` and `turn_started_at` (`engine/server/src/pty.rs:1382-1391`). The only token fields are on agent-task runs, self-reported through a `VOGT_COST:` line (`engine/server/src/agent_tasks.rs:50-60`). |
| Klaudia | writes `<session>.summary.md` on every compaction (75 on this box, 3 to 24 KB, median 7 KB) and a `compact_boundary` line in its JSONL. It records no token usage; `docs/compaction.md` in msp-klaudia uses a 4 chars/token estimate. |
| Claude Code | every assistant JSONL entry carries `message.usage` (`input_tokens`, `cache_read_input_tokens`, `cache_creation_input_tokens`, `output_tokens`); the sum is the live context size. Compaction is an entry with `isCompactSummary: true`. Nothing in Vogt reads either (`src/vogt/adapters/transcripts.py:394-430` reads only the model and reply text). |

### 1.3 Friction that costs context

- The brief boilerplate ("When you need a person", "Driving other
  sessions", "Autopilot") is 3,367 of this session's 6,649-char brief, and
  the same three blocks go to every session (`src/vogt/application/services/_brief.py:24-83`).
- Hard rules are pasted by the overseer into every task: the attribution
  ban for msp-agent, the docker-cp test recipe, "never merge: merge deploys
  prod", "CI waits in one command", "split at 150 turns". No per-project or
  per-template injection exists; the only policy injection is the
  deployment-wide Claude `--settings` file (`engine/server/src/agent_cli.rs:295-301`).
- The Klaudia gitguard (msp-klaudia `internal/gitguard`, outside this
  repo) blocks worker commits, so the overseer commits for them by a
  checklist held in operator memory (`committing-for-gitguard-blocked-workers`).
  The engine already has `POST /api/git/op` with a Commit variant
  (`engine/server/src/git.rs:393-421`), not exposed through MCP.
- `work_get` and the work-item brief return the **oldest** comments under
  the limit (`src/vogt/storage/sqlite/declared.py:784`,
  `_brief.py:157`), so a hand-off written as the latest comment is the
  first thing dropped on a long item.
- A `blocked` report lives only in engine memory; a restart or manual
  hibernate loses it (`engine/server/src/hibernation.rs:53-104` has no
  `blocked`). The audited core event survives.

## 2. Principles

1. **Vogt holds the state; the model holds a pointer.** Anything an
   overseer would otherwise keep in its conversation (what each worker is
   doing, what changed, what it is waiting on) is a Vogt row it can re-read
   in one small call.
2. **Small by default, full on request.** Every session tool returns a
   digest row unless asked for `mode=full`, and list tools never return
   hibernated or exited rows unless asked.
3. **Deltas, not snapshots.** The overseer asks "what changed since cursor
   X", or blocks on "wake me when any of mine needs me", instead of
   re-reading everything on a timer.
4. **Budgets are enforced by the engine, not promised in a brief.** The
   engine measures turns and context and acts at the threshold.
5. **Policy is data the engine applies.** A rule the overseer has to paste
   is a rule the engine should inject or check.
6. **A hand-off is an artefact with a schema**, written where the next
   session (and a person) will find it first.

## 3. Changes

Each change names the layers it touches and a size on Vogt's effort scale
(xs, s, m, l, xl). Priority is in section 4.

### Q1. What the overseer holds

**C1. Digest rows and filters on the session tools (engine + core + MCP, size m).**

- `session_list` and `session_sweep` gain `mode=digest|full` (default
  `digest`) and `fields=[...]`. A digest row is one line of state:
  `id`, `name`, `work_item`, `activity`, `attention`, `ready`,
  `blocked.reason` (first 120 chars), `turn_started_at`,
  `last_reply_excerpt` (120 chars), `budget` (see C7), `supervisor`.
  Target: under 300 chars per row, so 50 rows fit in 15k chars.
- Both tools gain `states=[...]` with a default that excludes
  `hibernated`, `stopped` and `exited`; `session_sweep` gains `limit`
  (default 50) and `attention=[needs_you, ...]`; `screen_tail` is off by
  default and `screen_lines` is honoured only when asked.
- `session_sweep` keeps `counts` always, so the overseer sees "19 need you,
  122 hibernated" without the rows.
- `session_wait` returns the digest row plus `screen_tail` (8 lines), not
  the full screen; `screen=true` restores today's shape.
- `session_start`, `session_hibernate`, `session_wake`, `session_input`
  return the digest row.
- `initiative_list`, `work_get` and `session_history_list` get
  `mode=summary` (initiative: no body; history: no `command`).
- MCP transport: drop the duplicate pretty-printed `text` payload when
  `structuredContent` is present (keep a one-line text summary), and
  serialise with `exclude_none`. This alone roughly halves every tool
  result. (`src/vogt/adapters/mcp/stdio.py:207-214`, `http.py:192-202`.)
- The `command` field on session rows is replaced by `template` plus
  `brief_file`; the launch argv stays available on `mode=full`.

Expected effect: a full sweep of the live sessions on this deployment
drops from 388k chars to under 15k, and an overseer sweep no longer spills
to a file.

**C2. "What changed since my last check" (engine + core + MCP, size l).**

- The engine assigns a monotonic `seq` to every session event it already
  publishes on `GET /api/events` (`engine/contract/src/lib.rs:755-959`)
  and keeps a bounded ring of them with `Last-Event-ID` replay.
- New `session_changes(since=<seq|time>, supervised_by=me)` returns a
  compact list of `(seq, session, kind, summary)` plus `next_cursor`.
  Kinds: `ready`, `blocked`, `unblocked`, `approval`, `exited`, `errored`,
  `hibernated`, `woken`, `reply` (new assistant reply, with the 120-char
  excerpt), `commit` (see C16), `budget` (see C7).
- `session_wait_any(until=[...], supervised_by=me, cursor)` as designed in
  `oversight-sessions.md` slice 3; it returns the first matching change and
  the cursor. Together these replace the 20-minute cron: the overseer's
  loop becomes `wait_any` then `session_changes` then act.
- Depends on WI-956 slice 2 (`supervisor` on the row) for the `me` filter;
  without it, the overseer passes an explicit id list.

**C3. Per-session digest stored in Vogt (engine + core + MCP, size m).**

- The engine maintains `session.digest`: `{last_reply_excerpt,
  turns, context_tokens, context_pct, compactions, commits_since_start,
  files_changed, last_commit_sha, blocked, updated_at}` from the transcript
  and the worktree (C16 supplies the numbers).
- `session_digest(id)` returns it, and the digest row (C1) carries the
  short form. The overseer reads one row instead of `session_screen` plus
  `session_last_reply` plus `git log`. `session_screen` stays for the
  cases where the terminal itself matters (menus, prompts).

**C4. Structured blocked reports.** WI-996 E0 already specifies
`session_report_blocked(needs, boundary, ask, items)` with a content-hash
Inbox key. Add here only: persist `blocked` in the hibernation record so
a restart does not lose it, and show `blocked.ask` in the digest row. (Size
xs on top of WI-996.)

**C5. Oversight checkpoints instead of a re-sent prompt (core + MCP, size m).**

- Build WI-956 slice 4 (`oversight_checkpoint`, `oversight_get`) in its
  minimal form: an append-only JSON checkpoint per oversight session with
  `summary`, `waiting_on`, `next_steps`, `cursor` (from C2).
- The engine's resume nudge for a woken overseer (`hibernate_policy.rs:316-345`)
  points at the latest checkpoint rather than the whole brief.
- Guidance for the overseer brief: the sweep is `wait_any`, then
  `session_changes(since=checkpoint.cursor)`, act, then write a
  checkpoint. The Claude cron is removed from the recipe.

### Q2. What a worker holds

**C7. Engine-enforced budgets with auto-split (engine + core + MCP, size l).**

- `session_start` takes `budget={turns, context_tokens, wall_clock_minutes}`
  and a purpose profile supplies defaults (C10). Defaults for
  `implement`: 120 turns, 80% of the model's window, 90 minutes per turn
  cap; `review`: 40 turns.
- The engine measures (C16) and at `soft` (80% of any budget) types one
  delimited line into the session, using the autopilot nudge path
  (`engine/server/src/autopilot.rs:36-67`): "[vogt] budget: 96 of 120
  turns. Commit WIP on your branch, write the hand-off comment (schema in
  C11), then stop." At `hard` (100%) it waits for the turn to end, then
  hibernates the session with `trigger=budget` and emits a `budget` change
  (C2) so the overseer, or the engine itself when `autosplit=true`, starts a
  successor from the hand-off (C11) with the same work item and a fresh
  context. Klaudia sessions get the same line (it reads prompts typed at
  the TUI; WI-1011 is already closed).
- The nudge text and thresholds live in engine config, not in briefs.
- A session whose turn exceeds the per-turn wall-clock cap with no file
  change (the grok "looping" signature) is treated as `hard`.

**C8. Briefs composed by the engine from policy plus task (core, size m).**

- The brief becomes `header + project policy (C13) + deployment policy +
  task + hand-off (C11)`. The three boilerplate blocks are shortened to a
  pointer each ("Blocked? `session_report_blocked`. Other sessions: see
  `AGENT_GUIDE.md` section 4.") and the full text moves to a file the agent
  can read on demand (`--add-dir` already covers the brief directory).
- `session_start.task` gains structured optional fields that the brief
  renders as a "Touch points" block: `files[]`, `symbols[]`, `done[]`,
  `gates[]`. Oversight fills them once; the engine formats them. This is
  the single strongest lever on grok turn counts in the evidence.

**C9. Stop the re-reading (brief + Klaudia, size s).**

- The brief's touch-points block carries line ranges, so a worker opens a
  file once at the right place.
- A `repo_map` option on `session_start` (default on for `implement`)
  appends a `tree -L 2` style outline of the touch-point directories,
  capped at 2k chars, so a worker does not `ls` and `find` its way in.
- Klaudia's microcompact already replaces old tool results with a
  placeholder that names a spill file; the brief tells the worker that a
  placeholder is recoverable with one `Read`, so it does not re-run the
  tool. No Vogt code.

**C10. Driver and model choice as profiles (engine config + core, size s).**

Implement WI-956 slice 5 with these profiles. They record the operator's
rules of 2026-10-07 to 2026-10-10. Klaudia is only used with grok in this
deployment; Claude models run through Claude Code.

| Profile | Template + model | Context | Budget | Use for |
| --- | --- | --- | --- | --- |
| `implement` | klaudia + grok-4.7 | 128k (never 48k) | 120 turns | one item, exact touch points, fresh worktree |
| `implement-careful` | claude + Opus | 1M | 150 turns | security boundaries, prod-touching, anything grok failed twice |
| `review` | claude + Fable | 1M | 40 turns | PR and design review; input is a diff plus the hand-off, never the worker's transcript |
| `oversight` | claude + Opus (Fable for design) | 1M | none; checkpoints (C5) | drives sessions; MCP-heavy, so never grok (WI-1030) |
| `mechanical` | claude + Haiku | 200k | 60 turns | long-file edits, renames; needs the allow-list from `claude-haiku-sessions-prompt` |

The profile sets template, model, budget, `repo_map`, and which policy
blocks are injected.

### Q3. Hand-offs

**C11. A hand-off artefact with a schema (core + engine, size m).**

- A work-item comment of kind `handoff` (the first comment kind; today
  `work_comment` takes only `body`, `models.py:1718-1721`) with fields:
  `branch`, `head_sha`, `base`, `done[]`, `remaining[]`, `touch_points[]`,
  `gates_run[]`, `blockers[]`, `session`. Free text is allowed under it.
- The engine also writes `state_dir/handoffs/<uuid>.md`: for Klaudia it is
  the latest `summary.md` plus the git facts; for Claude it is produced by
  a `PreCompact` and `SessionEnd` hook (the hook plumbing exists,
  `engine/deploy/claude-session-hook.sh`, only `SessionStart`/`SessionEnd`
  today) that reads `transcript_path` and posts a summary through
  `/api/agent-auth/conversation`'s sibling route.
- `work_get` returns the newest comments first (fix `declared.py:784`),
  with `comment_order` for the old behaviour, and the brief includes the
  latest `handoff` comment verbatim as its own section.
- `session_start(from_handoff=WI-n)` is sugar: fresh session, brief carries
  the hand-off, same work item, `supervisor` set.

**C12. Resume versus fresh (guidance in `AGENT_GUIDE.md`, size xs).**

- Workers: fresh session plus hand-off, always. Resume only to recover a
  session that died mid-turn and only once.
- Overseers and operator-driven investigations: resume (that is what WI-962
  built), with a checkpoint written before any planned restart.
- Review sessions: fresh, `review` profile, input is `gh pr diff` plus the
  hand-off comment. A reviewer never reads a worker's screen or transcript.

### Q4. Guardrails as data

**C13. Project policy entity (core + engine + MCP, size m).**

- New per-project field set `agent_policy` (JSON, edited with
  `project_update`, shown by `project_get`): `forbidden_text[]`
  (regex, checked in commit messages, authors, trailers, PR text and branch
  names), `merge_deploys_prod` (bool, with `main_branches[]`),
  `commit_identity` (for example "repo default, no model attribution"),
  `test_recipe` (a short named block), `rules[]` (free-text lines).
- The engine renders `agent_policy` into every brief for that project (C8)
  and applies the checkable parts: a Claude `PreToolUse` hook on `Bash`
  refuses `git commit`/`git push`/`gh pr create|merge` whose message or
  branch matches `forbidden_text`, and refuses `gh pr merge` on a
  `merge_deploys_prod` project unless the caller is a person. Klaudia has a
  `hooks.json`; the same check ships as a Klaudia hook. opencode gets a
  pattern deny.
- The current msp-agent rule becomes one row: `forbidden_text: ["(?i)grok"]`.

**C14. An "oversight commits for a worker" path (engine + MCP, size m).**

- Expose the engine's `POST /api/git/op` Commit variant as
  `session_commit_for(id, message, paths?)` behind `git-write`. The engine
  performs the checklist from operator memory before committing: the
  worker session is `idle`, `hibernated` or `blocked` (not mid-turn), the
  staged diff has no conflict markers, the message passes the project's
  `forbidden_text`, and the diff is attached to the result for the
  overseer's pin and `unsafe` review. It commits with the repo's identity.
- The result is a digest line plus the SHA; the overseer never pastes the
  diff into its own context unless it asks for it.

**C15. The permission classifier stays; denials become reports.** No new
rules here. Credential and deploy steps go through grants (WI-973) and a
denial becomes a structured blocked report (WI-996). The policy file remains
per deployment; per-project rules are the `agent_policy` above, not
classifier prose.

### Q5. Measurement

**C16. Session economics, recorded by the engine (engine + core + MCP + ops, size m).**

- The engine tails each agent's own transcript (it already resolves the
  path for Claude, Codex and Klaudia, `engine/server/src/transcripts.rs`)
  and keeps per session: `turns` (assistant entries), `context_tokens`
  (Claude: usage sum; Klaudia and Codex: 4 chars/token estimate of the live
  history), `peak_context_tokens`, `compactions`, `tool_result_bytes` by
  tool name, `reads_repeated` (same path read more than once), `wall_clock`
  per turn (max and mean), `commits` and `files_changed` on the worktree,
  `cost_usd` where a price table exists.
- Stored in `history.db` beside the session row, exposed as
  `session.economics` in `mode=full`, summarised in the digest row as
  `budget: 96/120 turns, 61% ctx, 2 compactions`, and exported as
  Prometheus histograms by profile and model (not per session) on the
  existing `/metrics` (`engine/server/src/metrics.rs`).
- `agent_activity_summary` gains the same numbers per conversation.
- This is the input for C7 thresholds, for the profile table in C10, and
  for answering "which driver finishes a p2 feature in fewer tokens" from
  data instead of memory notes.

## 4. Priority and sizing

| Order | Change | Size | Layers | Depends on |
| --- | --- | --- | --- | --- |
| 1 | C1 digest rows, filters, half-size transport | m | core, MCP, engine (fields) | none; immediate 10 to 25x cut on sweep and list |
| 2 | C16 session economics | m | engine, core, ops scrape | none; feeds everything below |
| 3 | C8 + C13 policy-composed briefs and project policy | m | core, engine hooks | none |
| 4 | C11 hand-off schema, newest-first comments, hand-off file | m | core, engine hook | C13 for the policy check on commit |
| 5 | C7 budgets and auto-split | l | engine, core | C16, C11 |
| 6 | C2 session_changes and wait_any | l | engine, core | WI-956 slice 2 for `me` |
| 7 | C3 per-session digest | m | engine, core | C16 |
| 8 | C14 oversight commit path | m | engine, MCP | C13 |
| 9 | C5 oversight checkpoints | m | core | C2 cursor |
| 10 | C10 profiles | s | engine config, core | C7 budgets |
| 11 | C9 repo map and touch points rendering | s | core | C8 |
| 12 | C12 guidance | xs | docs | C11 |

Items 1 and 2 together are about a week and remove the two failure modes
in the evidence: the overseer compacting on list output, and nothing
knowing a worker has looped until a person looks.

## 5. Operator decisions

All five decided 2026-10-11 as recommended (operator: "Accept all recommendations"). Also decided: no workers are started from this study; the operator hands the initiative on.

1. **Default budgets** for `implement`: DECIDED 120 turns, 80% of the
   window, 90 minutes per turn. `hard` hibernates the worker with
   `trigger=budget` and emits a change; the overseer starts the successor.
2. **Auto-split by the engine**: DECIDED off by default, on per profile.
3. **Policy hooks** (C13): DECIDED refuse for `forbidden_text` and
   `merge_deploys_prod` (unless a person), brief-only for the rest.
4. **MCP result size** (C1): DECIDED `structuredContent` plus a one-line
   text summary. WI-1241 verifies on dev that Klaudia and opencode read
   `structuredContent` before it lands.
5. **Comment ordering** (C11): DECIDED newest-first by default on
   `work_get` and in briefs, with `comment_order=oldest` for the old
   behaviour.

## 6. Work items

Initiative `context-management-for-oversight-and-worker-sessions` (weight
60), all local-only under project `vogt`:

| Item | Change | Size | Priority |
| --- | --- | --- | --- |
| WI-1240 | this design doc and backlog | xs | p1 |
| WI-1241 | C1 digest rows, filters, half-size MCP transport | m | p1 |
| WI-1242 | C16 session economics | m | p1 |
| WI-1243 | C8 + C13 project `agent_policy` and engine-composed briefs | m | p1 |
| WI-1244 | C11 hand-off artefact and newest-first comments | m | p1 |
| WI-1245 | C7 engine-enforced budgets and auto-split (depends on WI-1242, WI-1244) | l | p1 |
| WI-1246 | C2 `session_changes` and `session_wait_any` (relates to WI-996) | l | p2 |
| WI-1247 | C3 per-session digest (depends on WI-1242) | m | p2 |
| WI-1248 | C14 `session_commit_for` (depends on WI-1243) | m | p2 |

C5 (checkpoints), C10 (profiles), C9 (repo map) and C12 (guidance) are
folded into the items above or wait on WI-956 slices 4 and 5.
