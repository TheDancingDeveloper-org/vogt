---
title: "ops: move vogt-dev engine /tmp and /cache off the container writable layer"
labels: [pods, phase-0, ops, bug, "model:sonnet"]
depends_on: []
vogt: WI-4426
lands: Forgejo indexarr/ops by PR (apps/vogt-dev/), operator approves the rollout
---

## Why

On 2026-11-19 at 22:47Z the vogt-dev engine pod was evicted: `Container engine exceeded its local ephemeral storage limit "8Gi"`. Twelve sessions died with it. Nineteen minutes after the restart (23:06Z) the new pod already held about 5.7G in its writable layer, measured from inside the container:

| Path | Size | What |
|---|---|---|
| `/tmp` | 3.8G | test temp dirs (`/tmp/.tmp*`, `/tmp/chatwidget-tests-*`, `codex-home`) from a host Codex nextest run |
| `/opt/rust/rustup` | 1.4G | toolchain `1.95.0`, auto-installed by rustup next to the image's `1.94.0` |
| `/cache` | 0.5G | `XDG_CACHE_HOME`, npm, pnpm, pip, uv and `cargo/bin` |

**Cause.** The engine image is the session pod image (vogt `rust2-images.yml:198-199`). Its ENV points every tool cache at `/cache` and sets `RUSTUP_HOME=/opt/rust/rustup` (`engine/Dockerfile.pod:75-79`, `269-270`). The engine Deployment mounts nothing at `/tmp`, `/cache` or `/opt/rust`, so host PTY sessions, which inherit the engine env (`engine/server/src/pty.rs:1429-1477`), write all of it into the 8Gi container layer. Also:
- The engine's shared scratch cargo target defaults to `/tmp/claude-1000/cargo-target` (`engine/server/src/config.rs:1642-1659`, `session_scratch.rs:30-42`).
- The engine never reads `CLAUDE_CODE_TMPDIR`.

## What to change (ops `apps/vogt-dev/deployment.yaml`)

1. Mount the `vogt-dev-builds` PVC into the `engine` container twice:
   - subPath `engine-tmp` at `/tmp`;
   - subPath `engine-cache` at `/cache`.
2. The existing `workspace-roots` init container (or a new tiny one) creates `engine-tmp` with mode `1777` (sticky; see WI-4299) and `engine-cache` with `0755`, owned by 1000.
3. Engine env:
   - `RUSTUP_AUTO_INSTALL=0`;
   - `ENGINE_SHARED_CARGO_TARGET_DIR=/var/lib/vogt/builds/shared-target`.
4. Engine `resources.limits.ephemeral-storage` follows decision Q1 (default assumed: `16Gi` until host sessions are capped by P1.8).
5. Leave `/opt/rust` alone. With `RUSTUP_AUTO_INSTALL=0` a toolchain mismatch fails loudly instead of writing 1.4G.

## Done when

1. The rendered Deployment (`kubectl kustomize apps/vogt-dev`) shows the two subPath mounts, the env lines and the limit. A server dry-run passes.
2. After the operator-approved rollout, `findmnt /tmp /cache` inside the engine container shows the builds PVC.
3. One hour after the rollout, with normal host load, the writable layer is under 1G (`du -sxh / --exclude=…` of the overlay, or the kubelet's `ephemeral-storage` usage from `kubectl get --raw /api/v1/nodes/winrarhost4/proxy/stats/summary`).
4. A host session's `rustup show` with a `rust-toolchain.toml` naming another channel fails with an auto-install error and writes nothing under `/opt/rust`.

## Out of scope

Engine code changes: those are P1.8 and P1.9.
