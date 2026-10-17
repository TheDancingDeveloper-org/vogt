# Session rail: grouping, staleness and "needs you" (design, 2026-10-17)

Status: proposal. Design only. No app code changes yet.

## Problem

The operator's complaint: the places rail (`.places-rail-session-area`) is
mostly stale sessions. When work is active, the relevant live sessions are
hard to find.

## How it works today

**Data.** The rail renders `sessionsStore.order` (engine `GET /api/sessions`,
kept live over SSE). Each row is a `SessionSummary`
(`engine/contract/src/lib.rs:221`): `activity` (`idle | running |
waiting-for-input | awaiting-approval | errored | exited | hibernated |
stopped`), `alive`, `exit_code`, `blocked`, `approval`, `hibernation`,
`keep_awake`, `autopilot`, `role` (`oversight` or worker), `work_item`, and
the clocks `created_at`, `activity_changed_at`, `last_output_at`,
`last_event_at` and `last_person_input_at`. The Vogt-side digest
(`session_list`) adds `attention`, `ready`, `blocked_reason`,
`started_by_session`, `project`, `role`, `purpose` and `work_item`. **The
rail sees none of the digest fields.** It has no project, parent or
ready information.

**Filter.** In the engine, `session_is_hidden` (`engine/server/src/sessions.rs:2126`,
WI-1289) drops a session from the default list when all of these are true:
it is not oversight and not kept awake; it is not running, waiting,
awaiting approval or blocked; it is not alive and idle; no person typed in
the window; and its last event is older than the window. The window is
`ENGINE_SESSION_LIST_RETAIN_AFTER`, six hours by default.

**Order.** The engine's `session_list_order` puts oversight first, then
`last_event_at` descending. In the web app, `sortSessionsForRail`
(`web/src/sessionRowModel.ts:167`) lifts bookmarked sessions between
oversight and the rest. `holdRailOrder` freezes the order while a row menu
is open (#686). Attention is shown only as a row class (`waiting`). It does
not move the row. The phone pager uses `sortSessionsByAttention` instead.

**Shape.** The rail is one flat list under a single collapsible header
labelled **"Running"**. The header count is every listed session
(`sessionsStore.order.length`), live or not. Mobile is the same SolidJS app
inside Capacitor, so the phone gets the same rail in its drawer.

## The data (live instance, 2026-10-17 03:10Z, read-only `session_list`)

I made one call: `include_hidden`, `include_stopped`, `states=[]`,
`limit=500`. It returned 500 of 509 rows; the remaining 9 are older rows
and do not change the result.

| Measure | Count |
|---|---|
| Sessions known to Vogt | 509 |
| Hidden by the engine's 6 h rule (`hidden_count`) | 480 |
| **Listed in the rail by default** | **~29** |
| Live: running | 10 |
| Live: idle (alive) | 2 |
| Hibernated | 141 |
| Stopped | 47 |
| Engine record gone (Vogt link only) | 300 |
| Reporting `ready` | 0 |
| Reporting `blocked` | 1 |
| Role oversight / worker / unknown | 2 / 198 / 300 |
| Bound to a work item | 453 |
| Has `started_by_session` (has a parent) | 342 |

Age by start time:

| Started | Live | Not live |
|---|---|---|
| < 1 h | 11 | 18 |
| 1–6 h | 0 | 56 |
| 6–24 h | 1 | 105 |
| 1–7 d | 0 | 309 |

Reading:

- About 29 rows show by default, and only 12 of them are live. **About 60 %
  of the rail is finished work.** Oversight sessions stay pinned to the top
  even when they are hibernated.
- Two in three sessions have a parent (`started_by_session`). Under active
  oversight the workers are the bulk of the rail, but they sit in one flat
  list, unrelated to their oversight session.
- `ready` was 0 and only one session was `blocked`, yet operators do get
  stuck sessions. The "needs you" signal is under-reported. WI-1706 is the
  concrete case: a Claude `AskUserQuestion` picker reported plain `idle` for
  2 h 20 m.
- The header says "Running (29)" when 10 are running.

## Design

### 1. Sections, ordered by what the operator must do

```
NEEDS YOU      awaiting-approval · waiting-for-input · picker open · blocked · errored
RUNNING        activity = running (incl. stalled, flagged)
READY / IDLE   alive and idle; `ready` = at its prompt and finished a turn
DONE RECENTLY  exited / stopped / hibernated, last event < N h (default 6 h)
OLDER (n)      collapsed by default; a link that opens the full History view
```

- Each section header shows a count, and an empty section renders nothing.
  Rename the current single "Running" header and fix its count.
- Within a section, sort by `last_event_at` descending. Needs you sorts
  oldest wait first, because the longest wait is the most urgent.
- A section move is the one reshuffle we allow. `holdRailOrder` still
  freezes order while a menu is open, and is extended to freeze it while the
  pointer is over the rail. A row that changes section while frozen gets a
  small "moved" ghost until the pointer leaves.
- Oversight sessions are no longer pinned to the very top regardless of
  state. They are parents (see §3), so they appear wherever their state
  puts them. They never fall into Older while they have live children.

### 2. Stale handling

- **Done recently** collects finished sessions inside the retain window. It
  is collapsible, and its collapse state is remembered per device.
- **Older** holds anything finished and past the window. It is collapsed and
  lazy: the engine already returns `hidden_count`, so the header can say
  "Older (480)" without fetching the rows. Expanding it pages through
  `include_hidden=true`.
- **Archive**: a person can archive a session (row menu or swipe). An
  archived session never appears in the rail until it is woken or gets new
  input. Auto-archive: a hibernated or stopped session with no event for
  `ENGINE_SESSION_ARCHIVE_AFTER` (default 72 h) is archived by the engine's
  sweep. Archiving is a list flag only: the session can still be woken and
  stays in History.
- A hibernated oversight session is shown as one dimmed line, not a full row.

### 3. Hierarchy: oversight → workers/reviewers

- Group rows under their parent when `started_by_session` resolves to a
  listed session. The parent row shows a caret and rolled-up badges, for
  example `▸ ovs-main  3▶ 1⚠ 2✓` (3 running, 1 needs you, 2 done).
- **A child that needs you is also hoisted into Needs you**, shown with a
  "↳ ovs-main" breadcrumb. Hierarchy must never hide a request for
  attention.
- An optional "Group by" control offers `state` (default), `project`,
  `work item` or `parent`. In project or work-item mode, the section
  headers become project or WI headers, each with its own state sort
  inside.

### 4. Pin, search, compact rows, badges

- **Pin** (the existing bookmarks) shows as a separate "Pinned" strip above
  Needs you. A pinned session that is finished stays visible, but dimmed.
- **Search** is a filter field at the top of the rail (`/` focuses it). It
  matches name, WI ref, project and branch, and searches hidden sessions
  too, through a server-side `q=`.
- **Compact row**, one line:
  `[state glyph] name · WI-123 · 4m [badges]`. The project chip shows only
  when grouping is not by project. Density setting: comfortable or compact.
- **Badges**: `?` picker or question, `⚠` approval, `⛔` blocked, `✗`
  errored, `AP` autopilot, `☾` hibernated, `✓` ready. The section counts
  show in the Places link too ("Sessions 2!/10▶").

### 5. Option pickers count as "needs you" (WI-1706, WI-1519)

- WI-1519 (done, 0f8aa25c) already publishes one `SessionReady` per turn
  when an agent rests at its prompt. Use it for the `✓` ready badge and to
  move the row into READY.
- WI-1706: the engine should detect a Claude `AskUserQuestion` or select
  list on screen (numbered options with a `❯` cursor) and the Codex
  equivalent. It reports this as a new `ActivityState::AwaitingChoice`,
  or as `awaiting-approval` with `approval.kind = "choice"` and the options
  parsed. Either way it gets attention rank -1, and the digest
  `attention` becomes `needs-answer`.
- The rail puts that row in Needs you with a `?` badge. Expanding the row
  (or long-pressing it on the phone) shows the options as buttons that send
  the option's key through the existing input path. This mirrors the
  approval card.

### 6. API / server changes

| Change | Where |
|---|---|
| `GET /api/sessions?group=state` returns `section` per row (`needs_you / running / ready / done / older`), computed in one place in the engine, so web, phone pager and MCP agree | engine `sessions.rs`, contract |
| `parent_session` (engine id) on `SessionSummary`, copied from the spec's starter. The rail cannot see the Vogt digest today | engine contract + spawn path |
| `project` on `SessionSummary` (or derived from `cwd`/the work item by Vogt and pushed into the spec) | engine contract |
| `ready: bool` on the summary (latched from `SessionReady`, cleared on running) | engine `pty.rs` |
| `AwaitingChoice` / `approval.kind=choice` with parsed options (WI-1706) | engine `screen.rs`, `pty.rs` |
| `archived_at` + `POST /api/sessions/{id}/archive` / `unarchive`; auto-archive sweep with `ENGINE_SESSION_ARCHIVE_AFTER` | engine, Vogt MCP `session_archive` |
| List params: `section=`, `q=`, `parent=`, `include_archived`, `sort=attention|event` | engine API, MCP `session_list` parity (test_engine_parity) |
| `counts` object on the list response: per section + `hidden` + `archived` | engine API |

## Mockups

Desktop (rail ~260 px):

```
┌ Places ─────────────────────────┐
│ Sessions  2! 10▶                 │
│ [ / filter sessions…          ]  │
│ Group: State ▾       ≡ compact   │
│ ★ PINNED                         │
│   ▶ vogt-rust-core · WI-1042  2m │
│ ▾ NEEDS YOU (2)                  │
│   ? msp-agent5 · WI-1706 · 2h ↳ovs│
│   ⚠ rustnzb-fix · WI-1650 · 40s  │
│ ▾ RUNNING (10)                   │
│   ▾ ◆ ovs-main         3▶ 1? 2✓  │
│       ▶ wi-1669-codex    · 1m    │
│       ▶ wi-1682-files    · 3m    │
│       ▶ session-rail-des · 0s    │
│   ▶ fluent2-egui-perf · WI-1601  │
│   …                              │
│ ▾ READY / IDLE (2)               │
│   ✓ pork-barrel-ui · WI-1590  12m│
│ ▸ DONE RECENTLY (17)             │
│ ▸ OLDER (480)       View history │
└──────────────────────────────────┘
```

Mobile drawer (~360 px); a row is 44 px high and swipe left archives it:

```
┌──────────────────────────────┐
│ ☰ Sessions        2!  10▶  🔍│
├──────────────────────────────┤
│ NEEDS YOU · 2                │
│ ? msp-agent5          2h  ›  │
│   [1 Yes] [2 No] [3 Other]   │
│ ⚠ rustnzb-fix        40s  ›  │
│ RUNNING · 10                 │
│ ◆ ovs-main  3▶ 1? 2✓      ▸  │
│ ▶ fluent2-egui-perf   1m  ›  │
│ READY · 2                    │
│ ✓ pork-barrel-ui     12m  ›  │
│ ▸ Done recently · 17         │
│ ▸ Older · 480                │
└──────────────────────────────┘
```

On the phone, the terminal pager follows the same section order: Needs you
first, then Running, then Ready.

## Phased plan (work items)

| Phase | Item | Size | Depends |
|---|---|---|---|
| 1 Web only | R1 Sectioned rail using existing fields (activity, approval, blocked, exit_code, hibernation); fix the header count; Done recently/Older collapse; Older uses `hidden_count` | M | – |
| 1 | R2 Compact row + badge set + section counts on the Places link | S | R1 |
| 1 | R3 Search box (client-side over the listed rows) | S | R1 |
| 2 Engine | R4 `parent_session`, `project`, latched `ready` on `SessionSummary` + MCP parity | M | – |
| 2 | R5 Engine-computed `section` + `counts`; `section=`/`q=`/`parent=` params; web uses them (one source for web, pager, MCP) | M | R4 |
| 2 | R6 Parent grouping with rollup badges + hoisting needs-you children | M | R4, R1 |
| 3 Pickers | R7 = WI-1706 engine: detect option pickers → `awaiting choice` + parsed options; digest `needs-answer` | M | – |
| 3 | R8 Answer the picker inline in the rail row/phone card | S | R7, R1 |
| 4 Stale | R9 Archive state + endpoints + MCP `session_archive`; auto-archive sweep (`ENGINE_SESSION_ARCHIVE_AFTER`) | M | – |
| 4 | R10 Swipe/menu archive, Older paging via `include_hidden`, server-side search over hidden | S | R9, R5 |
| 4 | R11 Group-by control (state/project/work item/parent), per-device persisted | S | R5, R6 |

Phase 1 alone answers most of the complaint (60 % stale, the misleading
"Running" header) without any engine change. Every phase keeps
`holdRailOrder`'s no-reshuffle-under-the-pointer guarantee (#686), and the
Playwright rail specs need updating for the section headers.

## Open questions for the operator

1. Default retain window for "Done recently" (6 h today) and for
   auto-archive (72 h proposed).
2. Should oversight lose its unconditional pin to the top (this proposal
   says yes: it is a parent row placed by state)?
3. For pickers, a new `AwaitingChoice` state or reuse `awaiting-approval`
   with a `kind`? This proposal leans towards reuse, because it is a smaller
   contract change.

## Decisions

Recorded from the operator's answers (WI-1736, 2026-10-17). They settle the
open questions above.

1. **Retain windows.** "Done recently" covers the last 6 hours. Auto-archive
   applies after 72 hours (`ENGINE_SESSION_ARCHIVE_AFTER`). Archiving is a
   later phase, so Phase 1 does not archive anything.
2. **Oversight is not pinned to the top.** An oversight session is a parent
   row placed by its state, like any other row. Its children nest under it.
   A child that needs you is lifted into "Needs you" with a breadcrumb.
3. **Pickers reuse `awaiting-approval`.** An option picker is
   `awaiting-approval` with `kind = picker`. The web side is ready to render
   it now. The engine side (WI-1706) follows in a later phase.
