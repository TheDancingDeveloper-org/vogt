---
title: "WI-3229 leftovers: ENGINE.md pod boot/placement/restart, entrypoint fixture, audit ignore"
labels: [pods, phase-1, docs, "model:haiku"]
depends_on: [P1.2]
vogt: WI-3229
lands: graft vogt/rust-2
---

## What to change

Files: `docs/ENGINE.md`, `engine/deploy/tests/session-entrypoint.sh`, the CI audit step in `.github/workflows/rust-core.yml`, and `deny.toml` if needed.

1. **ENGINE.md.** Bring three sections up to date with the code at the current tip:
   - **Pod boot:** the entrypoint steps `validate` → `handoff`, from `engine/deploy/session-entrypoint.sh:13-378`.
   - **Graft token:** a per-session mint gated on the project's `landing.source`; revoked at teardown; TTL capped at 12 h.
   - **Engine restart:** pods outlive the engine and are re-adopted; a session pod's own loss ends the session until WI-3370.
2. **Fixture.** Refresh the stale `session-entrypoint.sh` fixture so it passes against the current entrypoint.
3. **Audit.** Make the RUSTSEC-2024-0449 (unsafe-libyaml) ignore consistent between `deny.toml` and the `rust-core.yml` audit step.

## Done when

1. `bash engine/deploy/tests/session-entrypoint.sh` passes.
2. The audit step and `cargo deny check` agree on the ignore list.
3. Each changed ENGINE.md paragraph cites the code it describes.
