# Mobile connect/reconnect review — 2026-10-17

Scope: why connecting and reconnecting on the Vogt mobile app "feels very clanky". This is a review only; no code was changed. The mobile app is the PWA (`web/`) wrapped by Capacitor (`mobile/`). Every stream is served by the Rust engine; the Python core serves no streams. Base: `origin/main` 21c95f67 (v0.7.17).

Paths are relative to the repo root. `web/src/...` paths are client code, and `engine/server/src/...` paths are server code. Line numbers are as of the base commit. Findings were read from the code, not reproduced on a device. Each one was spot-checked against the source.

## How it is wired

| Channel | Client | Server | Resume |
|---|---|---|---|
| Global events, SSE | `fetch` stream reader, `web/src/api.ts:661-740` | `/api/events`, `engine/server/src/api.rs:1076-1134`; 15 s `:ka` keepalive | Server honours `Last-Event-ID` (4096-event ring, `events.rs:10`). **The client never sends it.** |
| Terminal, WebSocket | one socket per pane, `api.ts:748` | `/api/sessions/{id}/attach`, `ws.rs`; auth in the first frame within 5 s | `resume_from` byte cursor gives a delta reattach (works) |
| Chat, SSE | `Chat.tsx` | `chat_api.rs:~190-214` | none: no ids, no replay |
| Board, Backlog, … | interval polling | REST | n/a |
| Lifecycle | `wakeCoordinator.ts` merges `visibilitychange`, `focus` and Capacitor `resume` into a single wake (250 ms debounce) | — | — |

Nothing listens for `online`/`offline`, and the `@capacitor/network` plugin is not installed.

## Findings, ranked by user impact

### 1. A failed event-stream retry wakes the whole app, which causes a refetch storm during outages (High)
- The SSE error path calls `setConnected(false)` and then `noteForeground("sse-reconnect")` on **every** failed attempt (`web/src/store.ts:453-463`).
- `store.ts:525-528` skips its own forced reconnect for that wake reason. Every other wake handler still runs:
  - the session-list reconcile (`store.ts:529-532`);
  - every `viewAge` surface (`viewAge.tsx:290`: Board, Inbox, Backlog, Projects, …);
  - `FileTree.tsx:455` and `History.tsx:294`;
  - every terminal (`Terminal.tsx:986-996`). This handler has no reason filter: it cancels the pane's backoff and calls `connect()` at once.
- The stale timer (`store.ts:262-266`, 45 s) sends the same wake.

**Scenario:** the server redeploys or the tunnel is away for 60 s.
- SSE retries about 8 times.
- Each retry fires list, board, inbox and file-tree reads at a dead server.
- Each retry also drives N terminals into an immediate reconnect, so they flap between "Loading terminal..." and "Reconnecting (try N)". That defeats their 8 s backoff cap.
- Every failed engine auth also adds to `AUTH_FAILURES` (`engine/server/src/auth.rs:532-543`), which slows auth for everyone.

**Fix:** never wake on stream loss. Emit `sse-reconnect` only on the *open* edge after a gap of more than about 5 s, and have `Terminal.tsx` ignore it while its own socket is healthy.

### 2. A healthy event stream is torn down on every focus, resume and boot (High)
- `store.ts:525-528` forces `forceReconnectEventStream()` (`:500-515`) for any wake other than `sse-reconnect`.
- That includes `focus`. On Android the WebView fires `focus` when the keyboard closes, a dialog closes, or the notification shade is pulled down.
- It also includes `boot`: `App.tsx:852-853` starts the stream and then calls `noteForeground("boot")`, which kills and reopens the brand-new stream 250 ms later. That puts a second SSE handshake on the startup critical path.
- Because the client never sends `Last-Event-ID`, each forced reconnect is followed by a full refetch rather than a replay.

**Fix:**
- Force a reconnect only when the stream is not open or has been silent for more than one keepalive (about 20 s).
- Never force a reconnect on `boot` or `focus`.
- Send `Last-Event-ID` on reconnect (see finding 6).

### 3. "Disconnected" shows with no grace period and disables actions (High)
- `setConnected(false)` takes effect immediately on SSE error (`store.ts:455`) and when the stale timer fires (`store.ts:262-264`). The UI then changes at once:
  - the header shows "Disconnected / may be stale" (`Sessions.tsx:200-217`);
  - every row turns `session-row--stale` (`SessionList.tsx:49,84`);
  - Approve/Deny and similar buttons are disabled (`Sessions.tsx:232,251,286,305-314`; `Assistant.tsx:376,1968-2018`).
- Finding 2 means a focus blip briefly drops `connected` as well.

**Scenario:** a 300 ms handoff from Wi-Fi to 4G flashes the whole screen to stale and back, and a tap on Approve during the blip is lost.

**Fix:**
- Wait 2–3 s before showing disconnected.
- Keep actions enabled and let the request itself fail, with a retry.

### 4. A session-list refresh rebuilds the store, so rows jump (Medium-high)
- `loadSessions` empties `s.sessions = {}` and `s.order = []` and refills them (`store.ts:138-148`). It runs on every wake (findings 1 and 2), on `lagged`, and on stream open.
- New object identities make Solid re-render or re-mount rows, and the attention-ordered rail can re-sort under the user's finger. This is the same family as the earlier rail-rename bug.

**Fix:** apply the list with `reconcile(list, { key: "id" })` so rows update in place, and keep order stable while the user is touching the list.

### 5. A connect has no deadline, so a stalled handshake hangs forever (Medium-high)
- **SSE:** the connect calls `runtimeTransport().request` directly, with no deadline (`api.ts:681`). The stale timer is armed only once the stream is alive (`store.ts:276-282`). A TCP or TLS handshake that stalls on resume, for example while the tailnet or WireGuard path comes back up, is never retried. The header stays "Disconnected" until the user leaves the app and comes back.
- **Terminal:** a socket stuck in `CONNECTING` is treated as live by the wake handler (`Terminal.tsx:990-996`). `checkWatchdog(true)` then returns early because the socket is not OPEN (`Terminal.tsx:520`), so the pane stays on "Loading terminal..." until the OS times out the connect.

**Fix:** add an 8–10 s connect deadline to the SSE fetch and to a socket still in `CONNECTING`. When it fires, abort and go to the normal backoff.

### 6. Resume is a refetch, not a replay (Medium)
- **Global SSE:** the server has `Last-Event-ID` and a 4096-event ring (`api.rs:1080-1128`), but the client discards ids. Even when the client is too far behind, the server sends `{"type":"lagged","skipped":0}` with no `id` (`api.rs:1098-1103`, `1121-1126`; `skipped` is hard-coded to 0 when the cursor has aged out). The client then reloads everything.
- The live broadcast holds 256 events (`events.rs:168-171`), against a ring of 4096, so bursts lag easily.
- **Chat SSE:** no ids and no replay (`chat_api.rs:199-213`, broadcast of 256 at `chats.rs:757`). Each reconnect can only refetch the transcript or lose messages.

**Fix:**
- Track the last `id` and send it as `Last-Event-ID` on reconnect.
- Give `lagged` an id and a real `skipped` count.
- Raise the bus capacity toward 4096.
- Add id plus ring replay to the chat SSE.

### 7. No network-change trigger (Medium)
- SSE backoff is `min(30 s, 1 s·2^n)` plus 30% jitter (`store.ts:292-300`), up to about 39 s.
- The breaker (`connectionHealth.ts:511-516`: 4 failures, 2–30 s cooldown) pauses pollers for the same window.
- Nothing reacts to the network returning, so after a lift or tunnel the app can stay dead for up to about 39 s while it is in the foreground.

**Fix:** add `@capacitor/network` `networkStatusChange` and `window` `online` listeners. They should reset the backoff and the breaker and trigger one coalesced reconnect.

### 8. The terminal socket's close code is ignored, so auth failures retry forever (Medium)
- The engine closes with distinct codes:
  - 4401 when the socket is unauthorized;
  - 4408 when the auth frame times out (`ws.rs:56,314-350`);
  - no close frame at all after 5 lag resyncs, which sends only a `{"type":"lag"}` text frame first (`ws.rs:52,216-223`).
- The client's `close` handler takes no event (`Terminal.tsx:1400-1418`). Every close is therefore a network drop, and it retries at the 8 s cap for ever (`terminalReconnect.ts:580-581`).
- HTTP and SSE route a 401 to sign-out correctly (`api.ts:309,691`). Only the terminal socket gets this wrong.
- The 5 s `AUTH_DEADLINE` is tight for a phone that is still waking up.

**Fix:**
- **Client:** on 4401, go to the auth path, not a retry. On the `lag` frame, reattach at once with `resume_from`.
- **Server:** send a real close code (for example 4429) on lag give-up, and consider a 10 s auth deadline.

### 9. Terminal liveness depends only on the client, and parking makes resume visibly cold (Medium)
- The server never pings the terminal socket (`ws.rs:577,665-683`). Dead sockets are found by the client watchdog, which probes every 30 s with a 5 s timeout (`terminalWatchdog.ts:3-4`).
- Panes park after being hidden for 30 s (`Terminal.tsx:999-1012`). Background WebView timers are throttled, so when that happens is unpredictable.
- On return, `resumeDormant()` can `term.reset()` and replay (`Terminal.tsx:1476,1549-1560`).
- Each drop writes a permanent yellow `[disconnected]` line into the scrollback (`Terminal.tsx:1408-1416`).

**Fix:**
- Server-side WebSocket Ping every 15–20 s.
- Show `[disconnected]` as an overlay, not buffer text.
- Prefer a `resume_from` delta over reset-and-replay. Park the active pane later than 30 s.

### 10. Startup is sequential and the native shell is bare (Medium)
- **Startup chain** (`App.tsx:836-858`):
  1. `validateCredentials`, with a retry and a 4 s deadline, plus a fallback probe to `/api/status` on a 404 (`api.ts:540-553`).
  2. `await refreshSessions()`, with an 8 s deadline.
  3. The SSE stream, which finding 2 then restarts.
- On a slow network the user stares at "Checking your session…" (`App.tsx:1622`). **Fix:** render the shell optimistically when a token is stored, and run the three steps in parallel.
- **No cached app shell:** `web/public/sw.js:56-85` deletes all caches on activate and fetches navigations from the network. A cold open on a slow tunnel is a white screen.
- **No renderer-crash handling:** `MainActivity.java:42-63` loads the remote origin and has no `onRenderProcessGone` handler. A renderer killed in the background leaves a dead WebView.
- **Server-chooser flash:** `ServerActivity` builds the "Connect to Vogt" form even when it immediately forwards to Main, so the form flashes on every cold launch. Back from Main lands on that form, which reads as "logged out".
- **Full reload on auth change:** any auth-state change does `window.location.reload()` (`App.tsx:820-823`), which blanks the screen.

### 11. Proxy configuration is not documented for SSE (Low, but verify in ops)
- The repo holds no Caddy config. `docs/DEPLOYMENT.md:264-289` shows `reverse_proxy … { flush_interval -1 }` but doesn't warn that a site-level `encode` can buffer `text/event-stream`, and it says nothing about idle timeouts.
- The prod and dev edge config lives in the ops repo (`personal/caddyv2` on Node B). Check it for `encode`, `stream_close_delay`, and the Cloudflare idle limit (about 100 s) on the demo route.
- On a redeploy (`app.rs:~700-712`), every stream drops at the same moment. Finding 1 then multiplies the result.
- The engine sets no server-side idle timeouts (`axum::serve`, `app.rs:706`).

## What already works
- Wake triggers are coalesced, and session-list reads are deduplicated (`store.ts:96-116`).
- `lagged` handling is debounced by 1 s.
- Terminal sockets are guarded by identity, so there are no duplicate sockets.
- Initial attaches are staggered.
- Terminal reattach with `resume_from` is a byte-exact delta. The subscription is opened before the snapshot is taken, so there is no loss or duplication.
- HTTP and SSE 401s correctly sign the user out.

## Suggested work items (in order)

| # | Item | Size | Fixes |
|---|---|---|---|
| A | Stop waking the app on SSE loss: emit the wake only on reopen after a gap, and filter it in `Terminal.tsx` | S | 1 |
| B | Force SSE reconnect only when the stream is down or silent for more than 20 s; never on `boot`/`focus` | S | 2 |
| C | 2–3 s grace before showing "Disconnected"; keep actions enabled | S | 3 |
| D | `reconcile` the session list by id instead of rebuilding it | S | 4 |
| E | Connect deadlines for the SSE fetch and for terminal sockets in `CONNECTING` | S | 5 |
| F | Network listeners (`@capacitor/network` + `online`) that reset backoff and the breaker | S | 7 |
| G | Terminal close codes: 4401 goes to sign-out, `lag` leads to an immediate delta reattach; server sends a real close code and a 10 s auth deadline | M | 8 |
| H | Client sends `Last-Event-ID`; `lagged` gets an id and a real count; bus capacity raised to 4096 | M | 6 |
| I | Chat SSE ids and replay | M | 6 |
| J | Server WebSocket ping; `[disconnected]` as an overlay; prefer a delta over reset on unpark | M | 9 |
| K | Parallel boot, service-worker shell cache, `onRenderProcessGone`, `ServerActivity` flash | M | 10 |
| L | Document SSE-safe proxy settings and audit the ops Caddy config | S | 11 |

A–C are small client-only changes and should remove most of the visible flicker. Validate them on a device by:
- toggling airplane mode;
- switching apps for 2 s and for 60 s;
- redeploying vogt-dev while the app is open;

and counting requests and overlay changes in the engine log.
