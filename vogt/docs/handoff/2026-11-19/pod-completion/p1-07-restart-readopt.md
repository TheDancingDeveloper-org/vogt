---
title: "Engine restart with live pods: forced-kill path and readoption test hardening"
labels: [pods, phase-1, feature, "model:opus"]
depends_on: [P1.3]
vogt: new (WI-3205 follow-up)
lands: graft vogt/rust-2
---

## Why

The goal is that a session survives an engine restart or eviction. The code is designed for that:

- **Graceful shutdown.** `hibernate_for_shutdown` leaves pod sessions alone and persists their records (`engine/server/src/sessions.rs:3868-3875`, `3911-3929`). The pod watcher sees `shutting_down` and keeps the pod (`sessions.rs:878-888`).
- **Records exist from create time** (`sessions.rs:~2520-2578`). Pod stream tokens sit at `<state_dir>/pod-tokens` (`sessions.rs:4833-4836`); on dev the state_dir is the `vogt-dev-state` PVC.
- **Boot.** `recover_records` marks live records `Recovered` (`sessions.rs:4731-4757`). `readopt_pods` runs before the server serves (`app.rs:804`) and re-attaches each pod at the history-log cursor (`sessions.rs:1054-1144`, `agent_stream.rs:748-763`).
- **Agents** reconnect forever with backoff (`engine/session-agent/src/lib.rs:2489-2512`). On a 401/403 they kill the CLI (`lib.rs:2477-2484`).

What is not proven:
- **Forced kill.** An ephemeral-storage eviction may be immediate, with no shutdown hook. Only the graceful path has integration tests (`engine/server/tests/integration.rs:23735`, `23822`).
- **The load-only race** in `a_restarted_engine_adopts_the_running_pod_and_replays_exactly` (reported on WI-3229, `integration.rs:~18621`).
- **Reconnect grace.** An adopted link is judged by the same heartbeat timeout as soon as it reconnects (`agent_stream.rs:227-234`).

## What to change

Files: `engine/server/tests/integration.rs`. Change `engine/server/src/sessions.rs` (readopt only) and `engine/server/src/pod_reconcile.rs` only if a test exposes a defect. Do not run this alongside P1.3.

## Done when

1. An integration test kills the engine with no shutdown hook (drop the runtime or abort the task) while a fake pod agent is mid-output. A new engine on the same state_dir adopts the pod, and the client sees continuous output at the cursor with no duplicates and no gap.
2. The graceful-shutdown readoption tests stay green.
3. The load-only race is fixed, and 20 consecutive full-suite runs of `vogt-engine-server` pass it. Or the hand-off shows it cannot reproduce on the current tip, with the run log.
4. An adopted link gets a full `heartbeat_timeout` from its reconnect before liveness is judged (test).
5. The hand-off lists any defect found and fixed, with the failing-then-passing test.
