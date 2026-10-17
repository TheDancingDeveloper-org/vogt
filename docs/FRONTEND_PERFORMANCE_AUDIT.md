# Frontend performance audit

Date: 2026-08-26  
Branch: `audit/frontend-performance` (based on `dev`)

## Conclusion

The PWA's route splitting is healthy for the large editor/terminal payloads.
The bundle-budget script's static graph is 47.9 kB of JavaScript (147.7 kB CSS),
under the existing 400/200 kB budgets, and editor/terminal code is deferred.
However, `index.tsx` immediately awaits a dynamic `App` import before rendering;
the startup-critical awaited union is about 282 kB raw (about 99 kB gzip),
including the 140 kB App chunk and shell/FileTree dependencies. That is still
far from downloading Monaco or all terminal code, but it is a separate startup
waterfall the budget does not currently measure. The 10–15 second resume pause
is more likely a lifecycle/network fan-out problem than the browser downloading
the whole application.

The highest-probability path is:

1. A suspended network leaves SSE/WebSocket/fetch connections half-open.
2. Returning to the foreground deliberately starts a new SSE connection and
   every mounted live view starts its own reconciliation request.
3. The shell also refetches the workspace tree and Git status on both `focus`
   and `visibilitychange`.
4. Open terminal tabs are retained forever. Each terminal pane owns a WebSocket,
   xterm buffer, cache restore and replay; split panes multiply this cost.
5. None of the ordinary API reads has a deadline. A half-open request can sit
   until the browser/TCP stack gives up, which matches a 10–15 second apparent
   hang.

## Evidence

### Startup and resume

- [`web/src/store.ts`](../web/src/store.ts) force-closes/reopens SSE on every
  visible transition (and Capacitor resume), even if the current stream is
  healthy. The reconnect itself is useful after mobile backgrounding, but it is
  not coalesced with view reconciliation.
- [`web/src/viewAge.tsx`](../web/src/viewAge.tsx) invokes each mounted surface's
  handler on the same visibility transition. The active Board, Inbox, Projects,
  Audit, and Work Item views have different read sets; retained tool tabs can
  also remain mounted.
- [`web/src/FileTree.tsx`](../web/src/FileTree.tsx) is mounted in the shell for
  authenticated users and calls both `tree` and `git/status` on `focus` and on
  `visibilitychange`. A single wake can fire both browser events.
- [`web/src/placeMetrics.ts`](../web/src/placeMetrics.ts) refreshes five
  independent Vogt reads (Inbox, Projects, Board, Backlog, Drift) on login and
  after a coalesced live nudge. This is bounded, but it is still five requests
  before the user asks to open those places.
- [`web/src/Sessions.tsx`](../web/src/Sessions.tsx) reads assistant history on
  every Sessions mount, including when the assistant is not configured.
- [`web/src/index.tsx`](../web/src/index.tsx) awaits the runtime demo probe and
  the App/push/theme dynamic imports before rendering. The demo probe has no
  timeout. In a normal build it still probes `/demo-manifest.json`; on the
  engine's SPA fallback that unknown asset becomes an HTML response which is
  parsed and rejected. This is small, but it can delay *all* first paint when
  the network is unhealthy. The awaited App import also means the 140 kB App
  chunk is startup-critical even though it is represented as a Vite dynamic
  entry.
- [`web/src/App.tsx`](../web/src/App.tsx) waits for `/api/status` and then
  `/api/sessions` before switching out of the login-loading state. There is no
  timeout on either request.
- [`web/src/api.ts`](../web/src/api.ts) uses a direct, non-retrying,
  non-deadlined fetch for credential validation. The general GET retry helper
  also has no request deadline, so retries do not help a request that is stuck
  rather than rejected.

A browser interception run against the built app (token seeded, API responses
stubbed) observed this Board landing set:

| Phase | Requests observed |
| --- | --- |
| authenticated Board load | `/api/config`, `/api/status`, `/api/sessions`, `/api/events`, tree + Git status, assistant history, Board metadata (workflow + 4 taxonomies), Board list, and five shell metrics: about 19 API/SSE requests including the long-lived stream |
| visibility return | SSE reconnect, tree, Git status, Board list; 5 additional requests |
| Backlog landing | shell set plus ranked Backlog and five facet lists (Projects, Labels, Initiatives, Actors, Workflows) |
| Work Item landing | shell set plus work, sessions, why, project-scoped backlog/observations, workflow/actor/project lists, history and audit reads; commonly 10+ Vogt reads |

The exact count varies with capabilities, persisted tabs, and response timing;
the important result is that the wake work is distributed across independent
owners rather than being one large lazy-load operation.

### Retained terminal cost

- [`web/src/tabLifecycle.ts`](../web/src/tabLifecycle.ts) intentionally returns
  `always` for every terminal tab.
- [`web/src/TerminalWorkspace.tsx`](../web/src/TerminalWorkspace.tsx) can render
  multiple panes for one tab; each pane is a `<Terminal>`.
- [`web/src/Terminal.tsx`](../web/src/Terminal.tsx) opens one WebSocket per pane,
  restores IndexedDB data, writes the cache back, and replays a server snapshot
  into xterm. The terminal is configured with 5,000 lines of scrollback.
- [`engine/server/src/ws.rs`](../engine/server/src/ws.rs) sends the retained
  scrollback (up to the configured default 4 MiB per session) in 64 KiB frames
  whenever the resume cursor is no longer retained. Several panes or a full
  reset can create substantial main-thread parsing and layout work after the
  network is back.

The terminal visibility handler reconnects only when the WebSocket is already
`CLOSED`/`CLOSING`; a silently dead socket remains `OPEN` until the platform
detects it. That is an important resume edge case.

### Server/asset path

- [`engine/server/src/api.rs`](../engine/server/src/api.rs) computes history
  storage statistics and agent-task artifact statistics inside `/api/status`.
  It is an operational dashboard response, not a cheap auth probe, yet it is
  on the critical boot path.
- [`engine/server/src/app.rs`](../engine/server/src/app.rs) enables the
  `tower-http` compression feature in Cargo but does not install a
  `CompressionLayer`. The embedded asset handler sends hashed JS/CSS with
  immutable cache headers, but direct engine responses are uncompressed. A
  reverse proxy may compensate, but the binary itself does not.
- Large lazy assets are real but not on the first screen: Monaco's emitted
  `ts.worker` is about 6.6 MiB raw, `editor.api2` 1.9 MiB, and
  `standaloneServices` 1.6 MiB. `TerminalWorkspace` is about 406 kB raw. They
  should remain lazy; they are a separate editor/terminal-open cost.

## Prioritized recommendations

### P0 — fix the resume stall

1. **Create one app lifecycle coordinator.** On foreground, perform one
   debounced reconciliation after network availability is usable. It should
   own SSE reconnect, session refresh (if needed), FileTree refresh, shell badge
   refresh, and the active surface refresh. Remove the independent
   `visibilitychange`/`focus` fetchers or make them consume the coordinator's
   single wake token.
2. **Put deadlines on every request.** Use an `AbortController` timeout for
   ordinary HTTP reads (shorter for badges/tree, longer for explicit file or
   history reads). Abort stale requests on a second wake and show the existing
   stale/offline state instead of leaving a loading state blocked behind TCP.
3. **Suspend inactive terminal panes.** Keep terminal state in the client cache,
   but close or park inactive pane sockets and reattach only the active pane(s)
   on foreground. At minimum, ping/health-check an `OPEN` WebSocket after wake
   and reattach if it is not responsive. Add an explicit per-session snapshot
   byte/time budget and instrument replay duration.
4. **Separate authentication from operational status.** Add a cheap authenticated
   `/api/auth/check` (or equivalent) and load history/artifact/storage statistics
   after the shell is usable. Cache the operational response briefly.

### P1 — reduce avoidable read fan-out

5. Replace the five shell badge reads with one bounded aggregate endpoint, or
   serve cached/stale counts and refresh them during idle time. Do not make
   badge freshness gate the usable shell.
6. Only mount FileTree when the Files/editor workspace is active, or at least
   revalidate it once per lifecycle wake and only when its data is older than a
   TTL. Avoid doing both `focus` and `visibilitychange` work.
7. Make Sessions' assistant pending-action read conditional on assistant
   capability, and share/cache assistant history with the Assistant surface.
8. Share immutable taxonomy caches (projects, actors, labels, initiatives,
   workflows) between Board, Backlog, Audit, and Work Item. Prefer an aggregate
   detail response for Work Item rather than its current 10+ independent reads;
   load below-the-fold evidence/history on disclosure.
9. Install `CompressionLayer` in the engine (or make the deployment's required
   proxy compression explicit and tested). This matters most when Monaco or
   terminal chunks are opened on a cold cache.

### P2 — secondary costs

10. Add client TTL/stale-while-revalidate caches for public config and stable
    list/facet reads; use ETags/conditional requests where the engine/core can
    support them.
11. Keep Monaco language workers lazy by file type and investigate reducing the
    language contribution set. Do not pull them into the initial route graph.
12. Measure xterm cache copies and `term.write` replay time. A 4 MiB cache is
    copied before IndexedDB writes and can cause visible main-thread pauses.
13. Continue to use the existing Board/Backlog virtualization thresholds, and
    profile FileTree recursion and session-rail rendering with large workspaces.

## Recommended instrumentation/reproduction

Add temporary production-safe marks (or a debug flag) for:

- `pageshow`/foreground start and end of each lifecycle reconciliation;
- request URL, duration, abort reason, and whether it was a wake-triggered read;
- number of mounted terminal panes, WebSocket state, snapshot bytes, and xterm
  replay duration;
- `/api/status`, tree, Git status, and core operation server timings from the
  existing request-id access logs.

Reproduce with Chrome DevTools by opening several terminal tabs/split panes,
loading Board or Work Item, toggling Offline for 5–10 seconds, toggling Online,
then dispatching a foreground/page return. Capture a Network export and a
Performance trace. The success criterion should be a bounded wake budget (one
SSE reconnect, one session refresh if stale, one FileTree read, one active-view
read, and no more than one bounded badge refresh) and visible stale/offline UI
within the request deadline.

## Verification performed

- `pnpm typecheck` — passed.
- `pnpm test` — 84 files, 771 tests passed.
- `pnpm build` — passed.
- `uv run python scripts/check_bundle.py` — passed: 47.9 kB initial JS,
  147.7 kB initial CSS; editor/terminal chunks deferred.
- Startup graph accounting (the awaited `index.tsx` imports) — about 282 kB
  raw / 99 kB gzip before the first render, including App and shell dependencies.
- Playwright request interception against the built app — used to count the
  authenticated Board load and visibility-return fan-out described above.

No application code was changed in this audit branch; the branch contains this
report only.
