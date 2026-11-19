# Phase 2: sequenced issues (less detail)

Start these after P1.12 passes. Each becomes its own GitHub issue. They are listed in order.

## P2.1 Session-pod loss survival: keep unlanded work (WI-3370 chain)
- **Existing items:**
  - WI-4267 A: the pod checkpoint saves unlanded work as a graft draft (session agent and contract). Opus medium.
  - WI-4268 B: the engine records saved work, hibernate ordering, failure policy, and lost-pod hibernation (`pod_lost`). Opus medium.
  - WI-4269 C: restore on wake, and ENGINE.md. Sonnet.
  - WI-4270 D: live acceptance S19 on vogt-dev.
- **Design:** `~/Working/notes/wi3370-keep-unlanded-work-design-2026-11-15.md`. Operator decisions D1–D7 were answered on 2026-11-15 (see the WI-3370 comment).
- **Why after phase 1:** today a lost, evicted or OOM-killed session pod ends its session and removes the record (`pod_reconcile.rs:357-365`, `sessions.rs:2955-2956`). Salvage is archive-only (`pod_salvage.rs:12-13`).
- **Done when:** WI-4270's acceptance passes, plus "delete one session pod by hand; the session resumes with its edits".

## P2.2 Build caches: registry seed and kache write keys (WI-3164, WI-3354)
- **Registry seed.** A per-node read-only cargo registry seed: a CronJob fills a PVC that pods mount read-only at `/cache-ro` (`ENGINE_POD_CACHE_PVC`, `pod_backend.rs:1598-1613`). The entrypoint copies or links it into `CARGO_HOME`; today nothing reads `VOGT_CARGO_SEED_DIR`. The node is chosen at schedule time so caching and spill-over coexist (WI-3354).
- **kache write keys.** Per-session keys through the cache provisioner (`session_cache.rs:1-45`; set `ENGINE_POD_CACHE_PROVISIONER_URL`).
- **Old PVCs.** Delete or repurpose the unused `session-cache-winrarhost1`/`-winrarhosttrig` PVCs (winrarhost1 is an excluded node).
- **Done when:** cold vs warm `cargo clippy` timings per node are on WI-3164, and the second pod on a node is within 1.5× of a warm host build.

## P2.3 Project to image map (WI-3162 remainder)
- Today the engine picks the image from the template's `VOGT_SESSION_IMAGE`, else `ENGINE_POD_IMAGE` (`pod_backend.rs:1481-1485`).
- Add a project-level image choice:
  - `base` for Python-only projects;
  - `rust` for vogt and graft;
  - `mobile` for FarmEggs.
- **Done when:** three projects boot on three different images.

## P2.4 Graft token re-mint for long-lived pods (new)
- Minted tokens are capped at 12 h (`graft_mint.rs:45-49`). Nothing re-mints for a live pod, so a session past 12 h loses `g land`.
- Re-mint before expiry and deliver it over the session stream. The entrypoint's token-file path is rewritten atomically.
- **Done when:** a test with a short TTL shows a land after the first token's expiry.

## P2.5 Oversight and interactive sessions as pods (WI-3170)
- **Needs:** P2.1; the workspace config volume (WI-3373, WI-3374); oversight plan storage (WI-3366); the oversight graft mint (`mirror:bind`, WI-4386, landed).
- **Done when:** an oversight pod drives a worker fleet and survives a simulated node loss by wake.

## P2.6 Shells and agent tasks as pods; delete the local PTY (WI-3171)
- Agent tasks and the assistant get a pod template (today `template: None` → host, at `agent_tasks.rs:1469-1470` and `assistant.rs:3292,4622`).
- Then remove the `local` backend and `pty.rs` spawn, and set the host cap to 0.
- WI-895 (per-session cgroups for host sessions) is closed as moot then.
- **Done when:** `grep` finds no local PTY spawn in `engine/server`, and e2e is green against pods.

## P2.7 Bundle snapshot cost (new, small)
- `bundle_snapshot` scans the account's whole `projects/` tree on every pod start (`secret_broker.rs:1331-1370`). It also silently drops a transcript over 16 MiB (`:144`), so the woken session starts fresh.
- Index transcripts by conversation id, and surface the drop as a session warning.

## P2.8 Accounts (WI-3159 after WI-3152)
- More than one Claude account, with per-account caps (today one cap applies to every account, `claude_config.rs:320-324`).
- Blocked on the WI-3152 refresh-rotation spike, by the operator's 2026-11-15 deferral.

## P2.9 vogt-prod (WI-2698)
- Apply the same shape to vogt-prod once dev passes P1.12. WI-2698's nice/CARGO_BUILD_JOBS workaround is then superseded.
