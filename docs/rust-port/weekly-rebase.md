# Rust port: weekly rebase procedure (P0.5 / WI-1047)

Main keeps moving while the port lives on `rust-core`. Once a week:

1. `git fetch origin && git rebase origin/main` on `rust-core` (or merge, per
   the branch's current sync rule).
2. Re-record the goldens into a directory named for the new base:
   `uv run python scripts/parity.py record --impl python --out tests/parity/golden/<merge-base-sha7>`.
   CI selects `tests/parity/golden/${merge-base:0:7}` (see the parity step in
   `.github/workflows/rust-core.yml`), so the name must match the merge base.
3. Delete the previous golden directory. Only the one CI selects is kept;
   `mcp-tools.json`, `openapi.json` and `registry.json` beside it are separate,
   generated fixtures and stay.
4. Check both implementations against the new golden:
   `parity.py check --impl python --golden <dir>` and
   `VOGT_PARITY_RUST_BIN=engine/target/debug/vogt-core parity.py check --impl rust --golden <dir> --only cli`
   (plus the http/mcp transports CI runs).
5. Regenerate the ledger: `python3 scripts/drift_ledger.py --write` (defaults:
   `--base origin/main --head HEAD`; writes the git-ignored
   `docs/local/RUST_PORT_DRIFT.md`). A new migration is a hard item.
6. Triage the rows and file each unported behaviour change on the owning
   chunk's work item (or on I6 once that chunk is closed). The chunk comes from
   `tests/parity/ownership.toml`.

## Recorded runs

### 2026-10-19 (first end-to-end record)

| | |
| --- | --- |
| port base (merge-base) | `21c95f674e1b03ee177394f605abb1abfac41cdd` |
| origin/main | `f8402e92b08049fcb2f499dca898776a5f83211a` (68 commits ahead of the base) |
| rust-core | not rebased in this run (operator holds rust-core syncs) |
| golden | `tests/parity/golden/21c95f6` kept (CI's selection); stale `39b89c8`, `b108fa4`, `bd7bbb8`, `fe9d4db`, `21c95f6-s4`, `21c95f6-s9` deleted (no test or workflow referenced them) |
| parity | `check --impl python` 149 steps agree; `check --impl rust` (cli transport) 149 steps agree |
| ledger | 36 rows touching the Python core, no new migrations. By chunk: application+core+tests 8, tests-only 6, application+tests 4, adapters+application+tests 3, adapters+tests 3, tests+unassigned 3, adapters+application+config+tests 2, adapters+application+core+tests 2, single others 5 |
| items raised | none filed yet: the 36 rows (WI-1870/1871/1903-era session/sweep/web fixes) are owed a triage pass like `drift-triage-2026-10-16.md` when rust-core is next synced; the 3 `unassigned` rows need ownership.toml entries |
