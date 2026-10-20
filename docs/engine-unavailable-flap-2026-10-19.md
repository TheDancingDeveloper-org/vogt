# "Engine unavailable" flapping on prod 0.7.20 (WI-1903)

2026-10-19. Prod at vogt.sprooty.com, v0.7.20, with 24+ agent sessions.

## Symptom

The web GUI showed the "Engine unavailable" rail card and banner on and off every
few seconds. The DOM thrashed, and the operator's HAR (13:39:48–13:40:07 local)
showed 34 of 51 requests at status 0.

## Where the GUI decides "unavailable"

`App.tsx` `railAttention()` shows "Engine unavailable" when `sessionMetric().state`
is `unavailable` or `stale`. The state is `stale` when the session list has
loaded but `isConnected()` is false (`store.ts`). `isConnected()` follows the
`/api/events` SSE stream: it goes false 2.5 s (`DISCONNECT_GRACE_MS`) after the
stream is lost, and true again on the next open.

## What the evidence shows

| Source | Finding |
| --- | --- |
| Caddy (Loki `{container="caddy"}`) | `/api/events` "aborting with incomplete response … reading: context canceled": ~150 per 10 minutes from the operator's Windows Firefox (100.109.218.11), continuous since ~00:00Z, at 3–130 ms after the upstream answered. The phone shows 1–4 per 10 minutes. |
| Engine (Komodo `container_logs`) | A new `/api/events` every ~2 s, plus repeated `request abandoned by the client before a response was ready` for `/api/vogt/sessions` and `/api/vogt/place/metrics`. |
| HAR | Every failure is status 0 with no response: the browser cancelled it. There are no 4xx or 5xx responses. Responses are HTTP/2 through one Caddy hop. 14 requests were cancelled at 0 ms, which is a supersede abort. Streams carry real events (~10/s, frames up to 12 KB) before they are cut. |
| Engine latency (loopback, during the incident) | `/api/sessions` 0.14–0.54 s and `/api/status` 0.27–0.60 s. Some requests log 1–3 s (hibernate, `/mcp`). Load average ~149 on 24 CPUs, IO pressure `some` 9–13 %. Some SQLite `database is locked` errors in history archive and economics writes. Slow, but nothing near the 8 s client deadlines. |
| Deploy | The last prod deploy was at 01:59Z, after the onset. No engine restart lines up with it. |
| Repro | A fresh Chromium or Firefox tab (Playwright, loopback and via Caddy) held one stream for 25–40 s without a single abort. |

The proxy and the engine are not resetting connections. Every cancellation
comes from the client. The first loss happens in the operator's long-lived tab.
A fresh tab does not reproduce it, so the exact first trigger is not proven.
Whatever starts it, the client turns one loss into a permanent loop.

## Root cause: the client's reconnect loop amplifies any early loss

1. **The backoff never grows.** `noteStreamAlive()` set `reconnectAttempts = 0`
   on every open and every frame. A stream that dies soon after opening
   therefore always waited the first backoff step (1–1.3 s) and reconnected
   every ~2 s indefinitely. This matches the engine's 2 s `/api/events`
   cadence.
2. **Every reopen woke the whole app.** `onOpen` raised an `sse-reconnect` wake
   on each reopen after a loss. That wake runs `reconcile("sessions")`, which
   aborts the previous list read, and every `onWake` surface refetches. This
   produces the 0 ms aborted `/api/sessions` and `place/metrics` requests and
   the engine's "abandoned by the client" lines: about 3 requests/s of reads
   that never finish.
3. **The banner had no hysteresis.** Each loss turned `isConnected()` false
   after 2.5 s, and each open turned it true again, so the banner followed the
   loop.
4. **Every poll re-rendered the rail.** `loadSessions()` cleared
   `sessions = {}` and `order = []` and rebuilt them, so every row got a new
   object on every read. With ~150 sessions the rail and the list re-laid-out
   on each read.
5. **Rail starters asked for one page too many.** `readAllStarters` followed
   `truncated` even on a short page, giving offset 0 → 500 → 506: a fourth
   `mode=full` read for nothing on every refresh.

Poller dedup was already in place. The store coalesces unsignalled list
reads, place metrics keeps one read in flight plus one follow-up, and
`reconcile` dedupes per wake. The duplicate-looking traffic is the wake
cascade from (2), not overlapping intervals.

## Fix (branch `fix/engine-unavailable-flap`)

- `STREAM_SETTLE_MS` (1.5 s). A reopened stream must stay up that long before
  it resets the backoff, raises the `sse-reconnect` wake, or clears
  "Disconnected". A stream that keeps dying backs off 1 → 2 → 4 … 30 s, wakes
  nothing, and leaves the banner steady. One settled reopen raises exactly one
  wake. A first open, with nothing lost, behaves as before, so boot does not
  flash.
- `loadSessions()` merges with keyed `reconcile`, so unchanged rows and the
  order keep their identity and nothing re-renders.
- `readAllStarters` stops at a page shorter than the page size.
- `.session-row` gets `content-visibility: auto`, so off-screen rows skip
  layout and paint. This is a cheap alternative to a virtualizer: the list is
  ~150 rows, and it keeps keyboard focus and find-in-page working.

Tests: `eventStreamReconnect.test.ts` section D (no wake for unsettled reopens,
the backoff grows, Disconnected holds until a settle);
`sessionListIdentity.test.ts`; `railStarters.test.ts` (short page).

## Still open

- **What makes the first loss in the operator's tab.** The next time it
  happens, run `wakeLog` in that tab's console, or reload the tab. A reload
  that clears it points to tab state.
- **Prod load.** IO pressure and SQLite `database is locked` errors on
  history archive and economics writes are real, but they are not the cause of
  the flap. They deserve their own item.
