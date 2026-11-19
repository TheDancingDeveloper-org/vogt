---
title: "Pod heartbeat survives sustained traffic (busy pod sessions killed: 'agent heartbeat silent for more than 60s')"
labels: [pods, phase-1, bug, "model:opus"]
depends_on: []
vogt: WI-4418
lands: graft vogt/rust-2
---

## Why

Busy pod sessions are ended by the engine's liveness check while they are producing output:

```
WARN ending a session whose pod died session=ad039e34-… code=1 note=agent heartbeat silent for more than 60s
```

- In WI-4418's run that came about 60 s after start. On 2026-11-19 at 23:02Z it came 14.5 min after start. In both cases the agent was streaming.
- The live agent log of another pod shows `input from the engine` frames arriving about every 16 ms.

## Root cause

**Sender** (`engine/session-agent/src/lib.rs:2345-2431`):

```rust
loop {
    tokio::select! {
        Some(data) = output_rx.recv() => { … send Output … }
        Some(message) = stream.next() => { … Input / Resize / Ack / Flush / Salvage … }
        _ = tokio::time::sleep(BUNDLE_DEBOUNCE) => {   // :2419, BUNDLE_DEBOUNCE = 1 s (:39)
            flush_bundle(…); check CLI exit; send_frame(Heartbeat { cursor })   // :2430
        }
    }
}
```

- The `sleep` future is rebuilt on every pass, so any frame on another branch restarts the 1 s countdown.
- Every `Output` gets an `Ack` back (`engine/server/src/agent_stream.rs:1473-1476`), and the engine also sends `Input` frames. While a session is busy the timer never fires.
- The same branch is the bundle flush (credential and transcript write-back, WI-3160) and the CLI-exit check, so those starve too.

**Checker:**
- Only `Frame::Heartbeat` calls `link.note_heartbeat()` (`agent_stream.rs:1524-1527`); `Output` does not (`:1456-1480`).
- `pod_reconcile.rs:94-104` ends the session when `heartbeat_age > heartbeat_timeout`.
- The default is 60 s (`pod_backend.rs:516-517`), overridable with `ENGINE_POD_HEARTBEAT_TIMEOUT` (`:771`).

**Consequence:** `end_session` deletes the pod with **no salvage** (`pod_reconcile.rs:157-161`, `sessions.rs:878-901`), and the record is removed (`sessions.rs:2955-2956`). The session's work is gone.

## What to change

Files: `engine/session-agent/src/lib.rs`, `engine/server/src/agent_stream.rs`, and tests in both crates (`engine/server/tests/integration.rs` if needed).

1. **Agent:** create `let mut tick = tokio::time::interval(BUNDLE_DEBOUNCE); tick.set_missed_tick_behavior(MissedTickBehavior::Delay);` before the loop, and select on `tick.tick()`. Keep the existing flush, exit-check and heartbeat body.
2. **Engine:** call `link.note_heartbeat()` for every accepted agent frame: `Output`, `Gap`, `Flushed`, `Salvaged` and `Heartbeat`. This protects pods still running an older session image.

## Done when

1. A session-agent test with paused tokio time sends PTY output every 300 ms and receives an engine `Input` every 16 ms for 120 s. It asserts at least 100 `Heartbeat` frames and at least 100 bundle-flush attempts, and fails on the current code.
2. An engine test (fake agent) sends only `Output` frames for 3× `heartbeat_timeout`. The session is not ended. It fails on the current code.
3. A silent agent (connected, no frames) is still ended after `heartbeat_timeout` (existing or new test).
4. Gates are green for `vogt-engine-server` and `vogt-session-agent`.
5. The hand-off states the session-image rebuild needed for half 1 and that half 2 works with the old image.
