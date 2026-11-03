# Rust core dev-use gap register

**Scope:** `vogt-core` on graft line `rust-2`, cloned at changeset
`cbea6a4b97bdb`, compared with the Python core and the engine/PWA contract.
This is a dev-use register, not a claim that every parity difference blocks a
Rust preview. A gap is **blocking** when it prevents a safe, useful Rust-core
dev stack for the PWA, MCP, work items, sessions, inbox, or agent tasks;
**degrades** when the stack remains usable but behavior is materially reduced;
**cosmetic** when it does not affect dev operation.

The Python core remains the product authority. The Rust core is selected by the
stack's `VOGT_CORE_IMPL` path; therefore a Rust-only defect is a cutover blocker
without implying that the current Python-default dev stack is broken.

## Priority-ordered register

| Priority | Area / gap | Severity | Evidence | Owning item |
| --- | --- | --- | --- | --- |
| 1 | Safe implementation switch and rollback are not yet the completed dev contract. Merging/building the Rust path can otherwise become the cutover event. | **blocks dev** | `engine/Dockerfile` and `engine/deploy/entrypoint.sh` contain the Rust/Python selection path; the holistic review identifies the merge-as-cutover risk. | [WI-2660](https://vogt.sprooty.com/work/WI-2660) |
| 2 | No in-process scheduler: collector sweeps, watch evaluation, and budget-stop settlement do not run on the Rust core. | **blocks dev** | `engine/core/src/main.rs` documents that there is no scheduler; `sweep_interval_seconds`, `watch_interval_seconds`, and `forge_posture_interval_seconds` are parsed but not consumed by a running loop. Python counterpart: `src/vogt/adapters/http/scheduler.py`. | [WI-2021](https://vogt.sprooty.com/work/WI-2021) |
| 3 | Blocking application dispatch runs on Tokio workers, so concurrent session waits can starve health and ordinary PWA/MCP requests. The MCP body is also unbounded and the store lock can poison. | **blocks dev** | Rust dispatch calls `operation.run` synchronously in `engine/core/src/adapters/http/app.rs` and `mcp.rs`; Python offloads blocking work with `asyncio.to_thread`. | [WI-2663](https://vogt.sprooty.com/work/WI-2663) |
| 4 | Registered Rust operations still return 501 or conditionally fail on normal dev data: charters, watches, `session.report`, `usage.board`, `instance.diagnostics`, and linked-project `initiative.publish`. | **blocks dev** | Rust registry/service validation and `NotImplemented` paths; holistic review records eight stubs plus the conditional publish stub. These are called by oversight tooling and/or the PWA. | [WI-2664](https://vogt.sprooty.com/work/WI-2664), [WI-2665](https://vogt.sprooty.com/work/WI-2665) |
| 5 | Rust parity does not prove a successful engine-backed session/chat lifecycle, and the committed CLI golden is red at the reviewed tip (`work.get.claim_check` drift). | **blocks dev** | `scripts/parity.py` requires a golden and has no successful path for the 15 session/chat operations; `tests/test_parity.py` and `tests/parity/` are the contract. | [WI-2662](https://vogt.sprooty.com/work/WI-2662), [WI-2667](https://vogt.sprooty.com/work/WI-2667) |
| 6 | Session features consumed by the PWA/agent workflow are incomplete or divergent: digest/live fields, account and budget summary, model policy, cold wake/budget hand-off, and related session lifecycle behavior. | **blocks dev** | Rust/Python comparison in the holistic review: `session.list`/`session.digest` live fields, `session.history_list`, and `session.start` policy features diverge; the relevant feature items are already open. | [WI-2022](https://vogt.sprooty.com/work/WI-2022), [WI-2024](https://vogt.sprooty.com/work/WI-2024), [WI-2667](https://vogt.sprooty.com/work/WI-2667) |
| 7 | The Rust core accepts configuration keys for sweeps, watches, forge posture, usage, cold wake, scratch projects, autopilot, and image digest but does not apply all of them. | **degrades** | `engine/core/src/config.rs` parses and explicitly reports ignored keys; the dev stack sets `VOGT_CORE_IMPL` and core token variables in `deploy/stack.compose.yml`. | [WI-2671](https://vogt.sprooty.com/work/WI-2671) |
| 8 | Read-API residue differs from the Python/core contract: disk status, writable reason, clone, initiative summary, place metrics, project brief, inbox ordering, deploy-lanes coverage, and dispatch preferences. | **degrades** | Holistic-review comparison against `tests/test_parity.py`, `tests/test_pwa.py`, and the Rust read services; no accepted-differences file was present at the reviewed Rust tip. | [WI-2672](https://vogt.sprooty.com/work/WI-2672), [WI-1081](https://vogt.sprooty.com/work/WI-1081) |
| 9 | Engine-backed parity is absent for HTTP and MCP success paths, including chat/watch/session rename/remove and auth decisions. | **degrades** | Parity harness transport options exist, but current Rust coverage is primarily CLI and the session success paths are unavailable/untested. | [WI-2667](https://vogt.sprooty.com/work/WI-2667) |
| 10 | Collector coverage and read performance diverge on dev-sized data; five of fifteen collectors are not represented and several reads take 1.2–7.4 seconds. | **degrades** | Holistic review records the missing `deploy-lanes` collector and slow `coverage`, `projects/brief`, `drift`, `deps`, and `place/metrics` reads. | [WI-2672](https://vogt.sprooty.com/work/WI-2672), [WI-2673](https://vogt.sprooty.com/work/WI-2673) |
| 11 | Process lifecycle and observability are incomplete: SIGTERM drain, panic boundary, and authenticated access-log actor attribution are missing. | **degrades** | Holistic review cites `engine/core/src/main.rs` and the access-log path; the engine can still serve ordinary requests, but shutdown and diagnostics are unsafe. | [WI-2670](https://vogt.sprooty.com/work/WI-2670) |
| 12 | Storage failures can leak rusqlite text and entropy failure can produce zero-randomness IDs. | **degrades** | Rust error mapping and `fresh_id` behavior are called out in the holistic review; the dedicated error/ID items already exist and the migration schema itself is shared. | [WI-2668](https://vogt.sprooty.com/work/WI-2668), [WI-2669](https://vogt.sprooty.com/work/WI-2669) |
| 13 | Rust migration/schema is not a cutover gap: it embeds the same migration SQL/checksums and has backup, lock, and quick-check paths. | **cosmetic** | `engine/core/src/storage/sqlite/embedded.rs` embeds the migration files from `src/vogt/storage/sqlite/migrations`; the holistic review found no Rust-only DDL. | Already covered by the shared migration path; no new item |
| 14 | Auth and broker wiring is not a cutover gap: the Rust core uses the stack core token and the engine resolves other bearers through the core; token hashing and constant-time comparison match Python. | **cosmetic** | `engine/AGENTS.md` documents `VOGT_CORE_TOKEN`/`VOGT_CORE_TOKEN_FILE`, and `engine/server/src/core_auth.rs` owns core-token resolution. | Existing engine/core auth contract; no new item |
| 15 | Rust-core dependency/process hygiene (audit, duplicate crates, CI cost, and delivery gating) does not prevent a local dev preview but must be resolved before replacement/cutover. | **degrades** | Holistic review records the red merge process, missing Rust-core audit gate, duplicate crates, and expensive/flaky CI. | [WI-2661](https://vogt.sprooty.com/work/WI-2661), [WI-2677](https://vogt.sprooty.com/work/WI-2677), [WI-2674](https://vogt.sprooty.com/work/WI-2674) |
| 16 | Pod-backed sessions do not survive an engine restart: the session token and stream cursor are in memory, so the reconciler marks every surviving pod lost (deletes it) and the session returns as hibernated. | **degrades** | `engine/server/src/pod_reconcile.rs` module comment; `docs/ENGINE.md` "Pod-backed sessions". | [WI-3205](https://vogt.sprooty.com/work/WI-3205) |

## Blocking-gap ownership

All blocking rows already have an existing worker-sized item under the Rust-port
path; no duplicate child was created. In priority order, the blocking work is:

1. WI-2660 — safe implementation switch and rollback.
2. WI-2021 — scheduler and settlement loop.
3. WI-2663 — non-blocking dispatch and bounded MCP/store handling.
4. WI-2664 and WI-2665 — remove the registered operation stubs.
5. WI-2662 and WI-2667 — restore parity and prove engine-backed session flows.
6. WI-2022 and WI-2024 — complete session digest/summary and model-policy behavior.

## Sources and limits

The register was checked against the Python core, Rust core, engine guidance,
`deploy/stack.compose.yml`, `scripts/parity.py`, `tests/test_parity.py`, and the
2026-10-28 holistic review. The review's referenced
`docs/rust-port/accepted-differences.md` was not present on the `rust-2` clone,
so no undocumented difference is treated as accepted merely because it appears
in the Rust implementation. No live deployment or credential/broker probing was
performed.
