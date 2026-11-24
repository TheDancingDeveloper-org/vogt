---
title: "Host placement is an explicit, capped exception (workers and reviews always pods)"
labels: [pods, phase-1, feature, "model:opus"]
depends_on: [P1.5]
vogt: new (successor to WI-895 for the host side)
lands: graft vogt/rust-2
---

## Why

Twelve host sessions shared the engine container, filled its ephemeral quota and got it evicted. Nothing stops this:

- **The placement rule.** `runs_in_pod` (`engine/server/src/sessions.rs:4065-4077`) and the spawn check (`sessions.rs:2371-2387`):
  - `placement=host` always gives the host;
  - otherwise a pod only when the resolved template's backend is not `local`;
  - **a missing or unresolvable template silently gives the host** (`.ok()`).
- **The role is ignored.** Oversight and workers are treated alike.
- **Host-only starts.** Agent tasks start with `template: None, placement: None` (`engine/server/src/agent_tasks.rs:1469-1470`), and so does the assistant (`engine/server/src/assistant.rs:3292`, `4622`). Both always run on the host.
- **No cap.** Nothing limits concurrent host sessions.
- **Shared env.** Host PTYs inherit the engine env (`engine/server/src/pty.rs:1429-1477`), so every host build writes to the engine's `/tmp`, `/cache` and rustup.

## What to change

Files: `engine/server/src/sessions.rs` (placement and spawn only), `engine/server/src/pty.rs` (env block), `engine/server/src/config.rs`, `engine/server/src/agent_tasks.rs` and `engine/server/src/assistant.rs` (pass role and template through only), `docs/ENGINE.md` (placement section), and tests. Do not run this alongside P1.5.

1. **Role policy.** Sessions with role worker or review are refused `placement=host` (409, with a reason naming the policy) unless `ENGINE_HOST_PLACEMENT_ALLOW_WORKERS=1`. Put the check in `validate_template_placement` (`sessions.rs:101-150`).
2. **No silent host.** A non-empty template that does not resolve is a 400. An empty template on a deployment whose default backend is pod uses the default pod template.
3. **Cap.** `ENGINE_HOST_SESSION_CAP` (default `0` = unlimited) caps concurrent live host sessions, counting agent tasks and assistant starts. Over the cap is a 429 with `{cap, counts}`.
4. **Per-session dirs.** Each host PTY gets `TMPDIR`, `CARGO_TARGET_DIR` and `XDG_CACHE_HOME` under `<ENGINE_HOST_SESSION_SCRATCH_ROOT>/<session-id>/`. The default root is under the engine state or builds volume and is configurable. The dir is removed at session end.
5. **Docs.** ENGINE.md documents the three settings and the refusal codes.

## Done when

1. Tests: a worker with `placement=host` is refused, and accepted when the override is set. An unresolvable template is a 400. All fail on the current code.
2. Test: with `ENGINE_HOST_SESSION_CAP=2`, a third host session (shell or agent task) gets 429 with counts. Stopping one frees a slot.
3. Test: a host PTY's environment has the per-session `TMPDIR`, `CARGO_TARGET_DIR` and `XDG_CACHE_HOME`, and the dir is gone after the session exits.
4. Gates are green for `vogt-engine-server`. ENGINE.md is updated.
