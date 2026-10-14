# Harness review — oversight cost, completion signalling, ACP, prompt audit

Date: 2026-10-14. Base: `origin/main` 28058089 (v0.7.15). Work item: WI-1517.
Backlog: initiative `event-driven-oversight-and-structured-worker-results-harness-review-2026-10-14`, items WI-1518..WI-1530.
Scope: investigation and design only. No code on main changed.

## 1. Summary

- **Where an oversight sweep's tokens go.** About 80% of an oversight sweep's cost is the overseer re-reading its own conversation history. The sweep payload is not the problem. A live `session_sweep(limit 60)` is 8.3 KB (~2.5k tokens). The live Opus overseer's context sits at a median of 530k tokens and peaks near 900k. Each sweep makes about 5 API calls, and each call re-reads the whole context. Mean cost is 279k input-equivalent tokens per sweep, median 189k, p90 517k. That matches the operator's 100–300k measurement.
- **Push signalling.** A push path exists but has gaps.
  - WI-1246 shipped a sequenced engine event bus with `session_changes` and `session_wait_any` (blocks up to 600 s).
  - `AUTOPILOT: DONE` never reaches the bus.
  - `wait_any` does not fire for a worker resting `idle` at its prompt.
  - Nothing links a worker to its overseer.
  - The sweep prompt doesn't use any of it.
  - There is no outbound webhook.
- **Recommendation for (a).**
  - Add a structured `session_report` op that workers call (done / blocked / needs_review, with SHA, PR, gates and summary).
  - Have the engine emit a `done` change on `AUTOPILOT: DONE` as a fallback.
  - Fix the idle→`ready` gap.
  - Link workers to their overseer, and let the engine type one coalesced wake line into an idle overseer.
  - Move hibernate-on-done and stale-unblock into engine policy.
  - Bound the overseer's context to about 200k tokens with an automatic `/compact`.
  - Projected oversight cost: about −90% per day.
- **Do we need a different model?** Not first. Cache reads cost the same on Opus 5.5 and Sonnet 5.5 ($0.20/MTok), so switching models while the context is still 0.5M tokens saves only about 10%. Fix the context and the wake pattern first. Then run the event loop on **Opus 5.5 at effort `low`**, with Sonnet 5.5 as a measured trial, and keep reviews and design on a separate Opus session as today. Haiku only fits as a pre-filter.
- **(b) ACP: pilot, narrowly.**
  - ACP gives structured turns, `stopReason`, permission requests and session load/resume. Klaudia (`--input-format acp`) and xAI's Grok Build speak it natively.
  - It does not support a human attaching to a live session, which is a core Vogt feature.
  - Claude Code over ACP needs the Agent SDK adapter, which raises a subscription-login policy question.
  - Its `end_turn` means "turn ended", not "task done", so it does not remove the need for `session_report`.
  - Pilot it for headless grok workers only.
- **(c) Prompt audit.**
  - aidevenv: the audit's 8 findings are all confirmed, plus about 10 more. The fix is on the local branch `docs/agents-md-current` (commit 1eff3743, not pushed).
  - Vogt's prompt surface:
    - The sweep prompt re-sends a ~1.7k-char block 267 times into one conversation, carrying a hand-kept follow-up list and an operator name list.
    - The brief states the wrong token scope.
    - The brief carries filler and duplicated rules.
    - Shipped work items are still open.

## 2. Measurements

Source: the live overseer's Claude Code transcript (conversation `60c479d4…`, Opus 5.5). It covers 268 sweep prompts between 2026-10-12 09:21Z and 2026-10-14 23:00Z, about 104 sweeps a day. A second overseer (`156a8a26…`, 2026-10-08..12) shows the same shape. Token counts come from each API call's `usage`, deduplicated by message id. "Input-equivalent" weighs cache reads at 0.1×, cache writes at 1.25× and output at 5×. Dollar figures use Opus 5.5 list prices ($4 in / $20 out / $0.20 cache read / $5 cache write per MTok) as an API-equivalent. The operator runs on a subscription, so read the dollars as relative weight.

| Metric | Value |
|---|---|
| Live `session_sweep(limit 60)` response | 8,355 bytes, 9 rows (50 hibernated excluded by default), ~2.5k tokens |
| Sweep result per sweep in the transcript | 3.9k chars (~1.1k tokens) |
| API calls per sweep | mean 5.0 |
| Context at a sweep's first call | median 530k, peaks 870–910k; 3 compactions in 2.6 days |
| Fixed baseline at a session's first call | 40–53k (Claude Code system prompt, tool list, CLAUDE.md/AGENTS.md, MEMORY.md) |
| Input-equivalent per sweep | mean 279k, median 189k, p90 517k |
| Quiet sweeps (no write action) | 35% of sweeps, 17% of cost, median 141k each |
| API-equivalent cost | ≈ $0.62 per sweep, ≈ $65/day |
| Whole overseer session, all turns | 114M input-equivalent over 2.6 days; sweeps 74.7M of that |

### 2.1 Where a sweep's tokens go (mean over 268 sweeps)

| Component | Input-equiv / sweep | Share | What it is |
|---|---|---|---|
| Re-reading conversation history | 224.6k | **80.4%** | Every earlier sweep prompt, tool result, reply and thinking block, re-read on each of the ~5 calls |
| Re-reading the fixed baseline | 22.4k | 8.0% | ~45k system prompt, tool schemas, CLAUDE.md, MEMORY.md × 5 calls × 0.1 |
| New content (cache writes) | 19.9k | 7.1% | This sweep's prompt, tool results and reply, written once |
| Output (incl. thinking) | 12.3k | 4.4% | ~2.5k output tokens |

Content a sweep adds to the context, on average, in characters: `session_sweep` 3,914; `session_hibernate` 1,798; the sweep prompt 1,570; `session_start` 1,537; Bash 1,352; `session_last_reply` 887; `work_get` 691; `work_transition` 472. That is about 12k characters (~3.5k tokens) of results plus ~2.5k output tokens. All of it is then re-read on every later call until the next compaction.

What this means for each item in the brief:

- **The sweep payload** is ~1.1k tokens, 0.4% of a sweep's cost when first read. It matters only because it accumulates.
- **Last-reply reads** add ~0.9k characters per sweep, which is minor.
- **The repeated prompt** adds ~0.4k tokens per sweep. Over 70 sweeps between compactions that is ~30k tokens of near-identical text in the context.
- **Re-read conversation context** is 80%. This is the cost.
- **Tool schemas** are part of the 8% baseline. Claude Code defers MCP schemas behind ToolSearch, so the ~150 vogt tools cost names only until one is loaded.

## 3. (a) Agent management harness

### 3.1 What exists today for completion and blocked signalling

| Signal | Mechanism | Works | Missing |
|---|---|---|---|
| `AUTOPILOT: DONE` | Engine autopilot watcher matches the marker in the last 10 screen lines (`engine/server/src/autopilot.rs:65,80-88`) | Stops nudging; `autopilot` reads false on the summary | `Step::Done` only calls `set_autopilot(false)` and logs `autopilot.done` (`autopilot.rs:206-214`). No bus event, no core event, no change kind. **Invisible to `wait_any`.** |
| Trailed-off worker | Autopilot nudge after 60 s quiet at the prompt, cap 100 (`autopilot.rs:113-142,234-247`); the cap records a blocked report (`autopilot.rs:215-233`) | Re-drives Claude, Codex, opencode and Klaudia without a model turn | — |
| Blocked | `session_report_blocked` → core `_set_blocked` (`src/vogt/application/services/sessions.py:2846-2914`) → engine `POST /api/sessions/{id}/blocked` (`engine/server/src/api.rs:128-163`) → `ServerEvent::SessionBlocked` (`engine/contract/src/lib.rs:1023-1027`) | Inbox entry (`inbox.py:859-890`), Web Push/FCM push (`push_api.rs:207-247`), wakes `session_wait` and `wait_any` (`changes.rs:72-75`), survives hibernation | No `needs`/`boundary`/`ask` fields (WI-996 design, not built). No check that the caller is the session it reports on (`sessions.py:3015-3035`). |
| Event bus | Engine `EventBus` with a 4096-entry sequenced ring (`engine/server/src/events.rs:10,102-125`); SSE `GET /api/events` with `Last-Event-ID` replay (`api.rs:1060-1118`) | Real push for an HTTP client holding a bearer | Not reachable over MCP: MCP-over-HTTP is POST-only, tools only, `listChanged: false` (`src/vogt/adapters/mcp/http.py:56,112`) |
| `session_changes` / `session_wait_any` (WI-1246, c57fc5dc) | `GET /api/sessions/changes` and `GET /api/sessions/wait-any`; long-poll on the bus, 600 s cap (`engine/server/src/changes.rs:27,152-273`) | ready, approval, exited, errored, blocked, unblocked, hibernated, woken, budget | **`Idle` maps to no change** (`changes.rs:66-68`), and `ready` is computed on demand from a screen render (`screen.rs:490-534`), never published. So a Claude worker resting at its boxed prompt, or a Klaudia/Codex worker forced to `idle` by its title (`pty.rs:1733-1742`), does not wake `wait_any`. `session_wait` does treat idle+ready as ready (`wait.rs:62-83`). No `done`/`reply`/`commit` kinds; `reply`/`commit` are "reserved" (`changes.rs:12-13`). No `supervised_by` filter. |
| `events_list` | Core `events` table, cursor-polled (`operations.py:2030-2040`); the engine republishes rows as `VogtChanged` every 5 s (`vogt_core.rs:768-846`) | Live: ~59k events, including `session.blocked`, `session.hibernated`, `work.commented` | Polling only. `VogtChanged` is not a change kind (`changes.rs:99`), so a `work_comment` hand-off wakes nothing. |
| Hand-off comment | `work_comment(kind=handoff)` with `HandoffFields` (`models.py:1929-1957`, `services/work.py:1390-1460`) | Structured branch, sha, gates, remaining | No event to the overseer and no push; the overseer must poll the item. |
| Overseer link | `supervisor` field | — | Reserved and always null (`models.py:1092-1093`); no "supervised by me" filter (`models.py:5459,5485`) |
| Engine typing into a session without a model turn | Autopilot nudge; budget soft line (`budget.rs:92`); boot-wake resume prompt (`hibernate_policy.rs:367-420`); CI watch `[vogt] CI PASSED/FAILED` into bound sessions (`services/ci_watch.py:118-146,209-233`) | Proven pattern | No "worker finished → tell its overseer". Idle injection is design-only (`docs/design/oversight-sessions.md:172-178`, "off by default" decision at line 334). |
| Outbound webhook | — | — | None anywhere. `ENGINE_ESCALATION_WEBHOOK_URL` exists only in the WI-996 design. |

The operator's memory is right: the push mechanism for sub-agents was never built. Its building blocks (the bus, `wait_any`, blocked reports, the nudge typing path) were built, and they are about two small fixes and one new op away from being useful.

### 3.2 Design: workers report results, and the engine turns completion into events

**Feasible, and cheap to reach.** Every worker already has the vogt MCP server and a `work.write` token. Claude uses `vogt-mcp` and Codex uses a bearer env (`engine/deploy/mcp-bootstrap.sh:79-146`); opencode uses `opencode mcp add`, and Klaudia uses `vogt-klaudia-mcp`. A new core op at `work.write` scope is therefore callable by all four CLIs with no new plumbing.

**API: `session.report`** (MCP `session_report`; REST `POST /sessions/report`; engine `POST /api/sessions/{id}/report`). Item WI-1520.

```json
{
  "status": "done | blocked | needs_review | progress",
  "summary": "≤2000 chars",
  "work_item": "WI-n (default: the bound item)",
  "branch": "wi-1520-report", "head_sha": "…", "base": "origin/main",
  "pr_url": "https://…",
  "gates": [{"name": "cargo clippy", "result": "pass|fail|skipped", "detail": "≤200"}],
  "remaining": ["…"], "blockers": ["…"],
  "needs": "person | overseer",
  "idempotency_key": "optional; default sha256(canonical body)"
}
```

**Auth.**
- Scope `work.write`.
- **Self-only:** the caller must be the session it reports on. `_caller_engine_session_id` (`sessions.py:2668-2685`) resolves both `agent:session:` and `agent:engine:` principals.
- An overseer cannot report for a worker; it has `work_comment` for that.
- Fixing the same gap in `report_blocked` (no caller check today) is part of the item.

**Idempotency.**
- A repeat of the same `(session, idempotency_key)` within 24 h returns the first result with `duplicate: true` and emits nothing.
- An agent that retries after a timeout, or that both calls the tool and prints the marker, produces one event.

**Effects.**
1. Write a `handoff`-kind work comment on the item; the fields map 1:1 to `HandoffFields`.
2. Store `last_report` on the session summary and the hibernation record.
3. `blocked` delegates to the existing `_set_blocked`, so Inbox, push and wait keep working unchanged.
4. `done` also turns autopilot off.
5. The engine publishes `ServerEvent::SessionReported`, which `change_of` maps to change kind `reported` (status plus a 120-char summary).

**The engine detects `AUTOPILOT: DONE` itself (WI-1518).** Yes. The detection already exists; it just isn't published. In `Step::Done`, publish `ServerEvent::SessionDone { source: "marker" }`, map it to a new `done` change kind, add it to `DEFAULT_UNTIL`, and store `done_at`. A worker that called `session_report(status=done)` gets the same event with `source: "report"`, deduplicated per turn. The marker stays as the fallback for agents without MCP. The brief changes to "call `session_report`, then print `AUTOPILOT: DONE`".

**Fix the idle→ready gap (WI-1519).** When a session goes running → idle, render once. If `screen::is_ready` holds, publish one `ready` change per turn. That makes `wait_any(until=[ready])` agree with `session_wait(until=ready)`.

### 3.3 How it reaches oversight (WI-1521)

Instead of a 10-minute poll:
1. **Supervisor linkage.** `session_start` from a session records `supervisor`. `session_changes`, `session_wait_any` and `session_sweep` take `supervised_by=me`.
2. **Wake-on-event.**
   - An overseer opts in with `wake_on: [done, reported, blocked, approval, errored, budget]`.
   - When a supervised worker emits one of these and the overseer is at its prompt, the engine waits a 60 s coalescing window. It then types one line: `[vogt] 3 workers changed (done: vogt-1513, blocked: vogt-1514 …) — session_changes(since=4812)`.
   - This reuses the nudge typing path (`autopilot.rs:254-258`).
   - Rate limits: at most one wake per 60 s, and none while a person typed in the last 30 s.
   - A hibernated overseer is not woken; Inbox and push cover the person.
3. **Without opting in**, an overseer can still block on `session_wait_any(supervised_by=me, cursor=…, timeout_s=600)`. That is one cheap call that returns only on a real change. The 30 s MCP bridge timeout was already raised for this (c57fc5dc).
4. **External push (WI-996 E3)**, an `ENGINE_ESCALATION_WEBHOOK_URL` to Alertmanager to Telegram, stays the person's channel for blocked-on-person. It is not needed for the overseer.

**How each worker calls it.**

| Worker | Path |
|---|---|
| Claude Code (Opus/Haiku/Sonnet) | MCP tool `session_report` via `vogt-mcp` |
| Codex | MCP tool via the bearer-token HTTP server |
| opencode | MCP tool via `opencode mcp add vogt` |
| Klaudia/grok | MCP tool via `~/.klaudia/.mcp.json`; grok calls MCP tools reliably for small flat schemas, so keep the shape flat |
| Anything else / scripts | `curl -sf -H "Authorization: Bearer $VOGT_HTTP_TOKEN" -H 'Content-Type: application/json' "$VOGT_URL/sessions/report" -d @report.json` |

## 4. Cutting the sweep's token cost

Estimates are relative to today's mean of 279k input-equivalent per sweep at about 104 sweeps a day (29M a day, ≈ $65/day API-equivalent). Levers interact, so the combined row is computed directly rather than summed.

| # | Option | Mechanism | Estimated saving | Item |
|---|---|---|---|---|
| O1 | **Bound the overseer's context** | Default context budget for role=oversight (200k). The soft action types `/compact <focus>` instead of hibernating. | Mean re-read context drops from ~494k to ~120k: **−60 to −70%** alone | WI-1522 |
| O2 | **Event-driven overseer** | Wake only on done/reported/blocked/approval/errored/budget for its own workers; hourly `needs_you` sweep. | Removes quiet sweeps (17% of cost) and most polling wakes. ~40–60 wakes a day instead of 104: **−40 to −60%** alone | WI-1518, WI-1519, WI-1521, WI-1524 |
| O3 | Delta-since-cursor | `session_changes(since)` instead of a full sweep; `session_sweep(since=…)` | Small per wake (the sweep is ~1.1k tokens); stops ~3.9k chars a sweep accumulating: **−3 to −5%** | WI-1525 |
| O4 | **Routine actions as engine policy** | Hibernate-on-done after 10 min; auto-clear stale blocked after input + turn; nudge is already engine-side | ~1 hibernate call per sweep is one full context re-read each. Calls per sweep go from 5.0 to ~3.5: **−25%** of re-read | WI-1523 |
| O5 | Leave operator sessions out | `exclude_roles` / owner attribute instead of a name list in the prompt | 5 of 9 live rows are the operator's: about half the sweep payload, **~−2%** | WI-1525 |
| O6 | Shorter digests and lean write results | Drop empty fields; return digests from hibernate, start, bind, transition and create | ~6k characters a sweep less accumulation: **−5 to −8%** | WI-1525 |
| O7 | Cheaper model for routine sweeps | See §5 | Alone, today: Sonnet **−10%**; Haiku ~−80% but on a 0.5M context with judgment calls | WI-1526 |
| | **O1 + O2 + O4 (+O5/O6)** | ~50 wakes a day × ~3 calls × ~80k average context | ≈ 42k input-equivalent per wake (24k re-read, 10k new content, 7.5k output), ≈ 2.1M a day vs 29M: **≈ −93%** (≈ $6/day vs $65) | |

Order: O1 first. It needs no new API and is one engine-default change on top of `budget.rs`. Then H1/H2 (small engine fixes), then O4 and the v2 prompt, then the supervisor wake.

## 5. Model setup for oversight

Prices are API list prices, cached 2026-10-06:

| Model | $ in / out per MTok | Cache read |
|---|---|---|
| Opus 5.5 | 4 / 20 | $0.20 |
| Sonnet 5.5 | 2 / 10 | $0.20 |
| Haiku 5.5 | 0.10 / 0.50 (≤100k prompt; $0.50 / $2.50 beyond) | not listed; assumed 0.1× input |

| Scenario | Opus 5.5 | Sonnet 5.5 | Haiku 5.5 |
|---|---|---|---|
| Today's shape (≈5 calls × 494k context per sweep) | ≈ $0.62/sweep | ≈ $0.56/sweep | ≈ $0.13/sweep, long-context rate |
| After O1+O2+O4 (≈3 calls × 80k per wake) | ≈ $0.12/wake | ≈ $0.08/wake | ≈ $0.004/wake |

**Answer: no different model is needed to fix the cost; the context and the wake pattern are the cost.**

Recommended setup:
1. The overseer runs on **Opus 5.5 at effort `low`**. It has a bounded context (WI-1522) and wakes on events (WI-1524).
2. Reviews, design and security keep going to a separate Opus session. This is already policy, and it keeps the overseer's context free of review material.
3. Trial Sonnet 5.5 at effort `medium` for 3 days against the Opus-low baseline (WI-1526). Switch if quality incidents stay at zero; the saving is ~30% of an already-small number.
4. Haiku 5.5 only as a pre-filter that decides "nothing to do" without waking Opus, and only if wakes exceed ~150 a day. With event-driven wakes almost every wake has something to do, so a pre-filter has little to filter.

## 6. Oversight loop v2

This replaces the 10-minute prompt. It is item WI-1524 and needs WI-1518/WI-1519, plus WI-1521 for push. Most of the content moves out of the per-sweep prompt and into the overseer's brief or `agent_policy`, which load once.

### 6.1 The per-wake prompt

```text
Oversight wake. session_changes(since=<cursor>, supervised_by=me); act only on the sessions listed:
done/reported → check the report against the item's Done-means, queue review or hibernate;
blocked → answer if you can, else AskUserQuestion with a recommendation; approval → leave it for the person;
budget/errored → read session_last_reply(n=1) then decide. Reply in ≤5 lines, changes only.
```

### 6.2 What leaves the per-sweep prompt

Things that should not be in the per-sweep prompt:

- **The model policy and merge rules.** Put them in the overseer's brief.
- **The follow-up list** (WI-1291, WI-1314…). Use a Vogt query (`work_list` by initiative/state) or the item graph instead.
- **The operator-session list** ("oversight 7, msp-agent, … overssight2"). Mark operator sessions with a role or owner attribute and exclude them in the sweep.
- **Disk at ≥85%.** This is a Grafana/Alertmanager rule, not a model check every 10 minutes.
- **CI on release PRs.** `ci_watch` already types results into bound sessions.

## 7. (b) Agent Client Protocol

Sources, fetched 2026-10-14 and treated as data:
- agentclientprotocol.com (protocol pages, registry)
- github.com/leepokai/Codync
- agentclientprotocol/claude-agent-acp, agentclientprotocol/codex-acp
- gemini-cli docs/cli/acp-mode.md
- opencode.ai/docs/acp
- the msp-klaudia README
- code.claude.com/docs/en/agent-sdk/overview

**What ACP gives.**
- **Transport:** JSON-RPC 2.0 over stdio. The protocol version is 1; schema v1.25.0 was released 2026-10-09.
- **Turns:** `session/prompt` returns a `stopReason` of `end_turn`, `max_tokens`, `max_turn_requests`, `refusal` or `cancelled`, plus an unstable `error`.
- **Streaming:** `session/update` streams `agent_message_chunk`, `agent_thought_chunk`, `tool_call` / `tool_call_update` (with kind, status and diff/terminal content), `plan`, `usage_update` and `compaction_update`.
- **Permissions:** `session/request_permission` is a blocking agent→client request with `allow_once` / `allow_always` / `reject_once` / `reject_always`. `elicitation/create` covers other questions.
- **Sessions:**
  - `session/new` passes MCP servers through.
  - `session/load` replays history and needs the `loadSession` capability.
  - `session/resume` does not replay.
  - Model and mode go through session config options.
  - `session/cancel` covers cancellation.

**Compared with Vogt's PTY approach.**

| Concern | PTY + screen (today) | ACP |
|---|---|---|
| Working / idle / ready | Heuristics over the screen and title (`activity.rs:12-30`, `screen.rs:260-318,490-534`, `pty.rs:1710-1790`); fragile per CLI version | Typed: updates stream while working; the prompt response ends the turn |
| Permission prompts | Detected on screen, answered by typing keys | Typed request with options; the answer is a JSON response |
| Completion | Sentinel text `AUTOPILOT: DONE` | `stopReason: end_turn` = turn ended, **not** task done; still needs `session_report` |
| Tool activity / economics | Transcript tailing (`session_economics`) | `tool_call` / `usage_update` events |
| Resume after hibernate | Agent-specific `--resume` | `session/load` (Claude adapter, Codex adapter, opencode, Klaudia) |
| Human live attach (web/phone) | **Yes**, a core Vogt feature | **No**: one client drives the agent; a human cannot co-drive. Codex even refuses a second writer on a thread. |
| Slash commands, TUI-only features | All | Advertised subset; the Claude adapter filters some |
| Auth on subscription logins | The official CLI with Max/ChatGPT login | Claude: via `claude-agent-acp` on the Agent SDK. Anthropic's Agent SDK page says third-party products may not offer claude.ai login unless approved, which is a **policy question**. Codex adapter: ChatGPT login supported. |

**Support today.**
- **Claude Code:** adapter `@agentclientprotocol/claude-agent-acp` v0.90.0, built on the Agent SDK. It loads CLAUDE.md, settings and hooks.
- **Codex:** adapter `codex-acp` v2.2.2, built on the Codex App Server. Vogt already drives Codex quick chats over app-server (d6c2a43f).
- **Gemini CLI:** native (`gemini --acp`).
- **opencode:** native (`opencode acp`). It spawns its own server and cannot attach to a running TUI.
- **Klaudia:** native (`klaudia --input-format acp`). Session ids are its transcript ids; it supports load and list.
- **xAI Grok Build:** native (`grok agent stdio`).
- **Codync** is a working reference. It is a Rust ACP host driving ~25 agents. It uses `stopReason` for "done" alerts and permission requests for "needs input" cards. It keeps its own transcript with a global revision, and it uses an injected MCP `send_message` tool for the semantic result. PTYs are used only for login.

**Migration cost and risk.**
- **Cost:** a new engine driver (`acp.rs`) behind the same `ServerEvent`s, so `session_changes`, the GUI and the Inbox stay unchanged. Rebuilding attach as a structured transcript UI is a large GUI change.
- **Risk:**
  - Losing live human attach.
  - Adapter churn (the Claude adapter is at release 0.90 and moves weekly).
  - The Claude subscription policy question.
  - Two driver paths to maintain.

**Does it solve (a) natively?** Partly. It removes turn-end and permission detection, but not the task-level verdict. Codync itself adds an MCP tool for the result, which is the same move as `session_report`.

**Recommendation: pilot, not adopt.** Phased plan:
- **P0 (WI-1527).** A `klaudia-acp` template for headless grok workers that nobody attaches to. It maps ACP events onto existing `ServerEvent`s, with `session/load` on wake. Run ≥10 real tasks and compare reliability against PTY Klaudia.
- **P1.** If P0 wins, add an engine `AgentDriver` trait (PTY | ACP) and make ACP the default for headless grok and Codex workers. Codex goes via `codex-acp`/app-server, which the engine already speaks.
- **P2.** Claude via `claude-agent-acp` only after the subscription-login question is answered in writing, and only for headless workers. Interactive and oversight sessions stay on PTY while live attach matters.
- **Not now:** replacing the PTY path for attached sessions.

## 8. (c) Prompt audit follow-up

### 8.1 aidevenv AGENTS.md (part 1)

The `/claude-api prompt-audit` report (2026-10-14 20:46Z, run inside the overseer session) had 8 findings and 1 flag. **All are confirmed** against the repo, the remote, the Komodo stacks and the ops repo. About 10 more stale facts were found and fixed:

- the harness store moved to `/var/lib/aidevenv`;
- the agent-auth broker is now generic (no Forgejo/Woodpecker/Komodo checks);
- the proxy forwards GET/POST/**PUT**;
- the mobile WebView uses `AIDEVENV_ANDROID_SERVER_URL`;
- opencode ships only in the devpod stage;
- the secrets are named `INFISICAL_CLIENT_*` / `TAILSCALE_AUTH_KEY`;
- the §6 mutagen procedure described the predecessor's layout;
- the API list was incomplete.

The commit removes dated narrative: the June 2026 incident, "July 7 2026", and "pipeline #17".

- Branch: `docs/agents-md-current` in aidevenv, worktree `/home/sprooty/Working/Active/aidevenv-agentsmd`.
- Commit `1eff3743`, on base `origin/feat` b5b2ad2 (aidevenv has no `main`).
- AGENTS.md: 99 insertions, 187 deletions (19.5 KB → 15.1 KB). **Not pushed.**
- Out of scope and recorded on WI-1530: `TOOLING.md:3`, compose and `.env.example` comments referencing missing files, and `deploy-internal.yml` targeting a nonexistent `main`.

### 8.2 Vogt oversight and worker prompt surface (part 2)

| Finding | Where | Pattern | Fix |
|---|---|---|---|
| Sweep prompt re-sent every 10 min into one conversation (267×, 1.0–2.3k chars each) | Operator's loop prompt | Per-turn repetition of static policy; grows the context | Static parts into the brief; per-wake prompt ≤600 chars (WI-1524) |
| Follow-up list hard-coded in the prompt (WI-1291, WI-1314, …) | Sweep prompt §5 | Hand-maintained state in a prompt goes stale between edits | Query Vogt (WI-1524) |
| Operator sessions by name, including a typo ("overssight2") | Sweep prompt §6 | Fragile identity by display name | Role/owner attribute + `exclude_roles` (WI-1525) |
| Disk and CI checks every sweep | Sweep prompt §1 | Model doing a monitor's job | Alert rule; `ci_watch` |
| Sweep never uses `wait_any`/`changes` | Sweep prompt §1 | Predates WI-1246 | v2 loop (WI-1524) |
| Brief says the token "may write work items and comments — nothing else" | `src/vogt/application/services/_brief.py:199-201` | **Stale fact**: default scopes are `read,work.write,project.write,writeback` (`src/vogt/config.py:629-640`) | Correct the sentence or narrow the default (WI-1528, D7) |
| "## Deployment policy … the settings file … not this brief" | `_brief.py:29`, rendered at 450/553/584 | Filler heading in every brief | Drop or fold (WI-1528) |
| "Budget: 120 turns. At the soft warning…" and "Never end a turn by announcing the next read" | vogt `agent_policy` rows (rendered at `_brief.py:152`) | Duplicates the engine budget and the Autopilot block; the second is a patch for an older model's habit | Remove from `agent_policy` (operator data change) |
| "Land by pushing your branch only. Open NO PR" vs task texts saying "Open a PR --base …" | vogt `agent_policy` vs oversight task texts (e.g. the WI-1075 brief) | Contradiction resolved only by the reader | State the precedence once (WI-1528) |
| Kick-off "Read that file now…" | `engine/server/src/agent_cli.rs:236` | One extra tool round trip per session start; harmless | Keep; the brief file is also the hand-off record |
| `session_bind_work`, `work_create`, `session_hibernate`, `session_start` return full objects (1.5–4 KB) | `services/sessions.py`, `services/work.py` | Write results sized for a GUI, landing in a model's context | Digest returns by default (WI-1525) |
| `AGENTS.md` (19 KB), `engine/AGENTS.md` (14 KB) and `docs/AGENT_GUIDE.md` (49 KB) are read on demand via the CLAUDE.md pointers | repo | Fine: not auto-loaded; workers read only what their task touches | None |

### 8.3 Tracker, design-doc and memory hygiene

| Finding | Where | Pattern | Fix |
|---|---|---|---|
| Shipped items still `open` (WI-1241, 1242, 1244–1248); `docs/design/context-management.md:3` says "No code" | Tracker / docs | Stale status misleads planners and overseers | WI-1529 |
| vogt project `MEMORY.md` is 17 KB (~4.5k tokens), mostly "MERGED/DEPLOYED/DONE" lines | `~/.claude/projects/-home-sprooty-Working-Active-vogt/memory/MEMORY.md` | Status log in an always-loaded index | Prune to ≤8 KB (WI-1529) |

## 9. Operator decisions

| # | Decision | Recommendation |
|---|---|---|
| D1 | Bound the overseer context at 200k with automatic `/compact` (WI-1522), before anything else | **Yes**: biggest saving, smallest change |
| D2 | Overseer wake: engine types a coalesced line into an idle overseer (`wake_on`, opt-in per overseer) vs a blocking `wait_any` loop only | **Engine wake, opt-in**. It reverses the earlier "idle injection off by default" only for overseers that opt in |
| D3 | Overseer model after D1/D2 | **Opus 5.5 effort `low`**, with a Sonnet 5.5 trial (WI-1526); Haiku only as a pre-filter |
| D4 | `ENGINE_HIBERNATE_DONE_AFTER` default on dev | **10 minutes** on dev, then prod after a week |
| D5 | ACP | **Pilot** headless Klaudia/grok only (WI-1527); no Claude-over-ACP until the subscription-login policy is confirmed |
| D6 | Push `docs/agents-md-current` in aidevenv and merge to `feat` | **Yes**, after reading the diff |
| D7 | Session token scope: fix the brief's sentence, or narrow the default to `read,work.write` | **Fix the sentence now**; narrowing breaks `project.write` users and needs its own review |
| D8 | Close the shipped context-management items (WI-1241..1248) | **Yes**, with the commits as reasons (WI-1529) |

## 10. Backlog

Initiative `event-driven-oversight-and-structured-worker-results-harness-review-2026-10-14`, parent WI-1517. No workers have been started.

| Ref | Title | P | Effort | Model | Depends |
|---|---|---|---|---|---|
| WI-1518 | H1 Engine emits a `done` change on AUTOPILOT: DONE | p1 | s | grok, Opus-low review | — |
| WI-1519 | H2 `ready` change for agents resting idle at their prompt | p1 | s | grok, Opus-low review | — |
| WI-1520 | H3 `session_report` structured results | p1 | l | Opus low | relates WI-996 |
| WI-1521 | H4 Supervisor linkage + overseer wake-on-event | p1 | l | Opus low | 1518, 1519 |
| WI-1522 | H5 Bounded overseer context (`/compact` on budget) | p1 | m | grok, Opus-low review | — |
| WI-1523 | H6 Engine policy: hibernate-on-done, stale-blocked clearing | p1 | m | grok, Opus-low review | 1518 |
| WI-1524 | H7 Oversight loop v2 prompt + doc | p1 | s | Haiku, Opus-low review | 1518, 1519 |
| WI-1525 | H8 Compact sweep and lean write results | p2 | m | grok, Opus-low review | — |
| WI-1526 | H9 Oversight model trial (Opus-low vs Sonnet) | p2 | s | Opus low | 1522, 1524 |
| WI-1527 | H10 ACP pilot for headless Klaudia/grok | p3 | l | Opus low | decision D5 |
| WI-1528 | H11 Brief and guide cleanup | p3 | s | Haiku | — |
| WI-1529 | H12 Tracker/doc/memory hygiene | p3 | xs | Haiku | — |
| WI-1530 | H13 aidevenv AGENTS.md branch + remaining stale refs | p3 | xs | Haiku | — |

Each item's body names its files, acceptance criteria and gates.
