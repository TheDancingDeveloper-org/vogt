---
title: "Engine free-space check sees its own ephemeral-storage quota; scratch prune covers CLAUDE_CODE_TMPDIR"
labels: [pods, phase-1, feature, "model:sonnet"]
depends_on: [P1.8]
vogt: new
lands: graft vogt/rust-2
---

## Why

- The engine's low-space guard reads free percent on the volumes of `workspace_root` and `temp_dir()` (`engine/server/src/sessions.rs:2087-2090`, `engine/server/src/session_scratch.rs:355-366`). That is node disk (82% used, 164G free on winrarhost4), not the container's 8Gi ephemeral limit. So it never fired before the eviction.
- Scratch cleanup roots are `std::env::temp_dir()` and `/tmp` (`session_scratch.rs:30-42`). The engine never reads `CLAUDE_CODE_TMPDIR`, which dev sets to `/home/sprooty/Working/Active/.agent-tmp`, so Claude scratchpads there are never pruned.

## What to change

Files: `engine/server/src/session_scratch.rs`, and the read site in `engine/server/src/sessions.rs` only. Tests.

1. Read the engine container's own ephemeral limit from a configured value (`ENGINE_EPHEMERAL_LIMIT`, set from the downward API `limits.ephemeral-storage` in the Deployment) and its usage (`du` of the writable roots, sampled and cached). Refuse new host sessions and raise the existing low-space alert at 75% of the limit.
2. Add `CLAUDE_CODE_TMPDIR` (when set) to the scratch roots that prune and cap.

## Done when

1. Test with a fake limit and fake usage: at 76% a host session start is refused with the low-space error, and at 74% it is accepted. Fails on the current code.
2. Test: a stale scratch dir under a fake `CLAUDE_CODE_TMPDIR` is pruned by the same rules as `/tmp`.
3. The ops line for `ENGINE_EPHEMERAL_LIMIT` (downward API) is written in the hand-off for P1.10 to apply.
4. Gates are green.
