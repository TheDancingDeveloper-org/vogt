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
| status | ledger and triage done; rebase deferred to oversight (I6, WI-1081) |
| golden | `tests/parity/golden/21c95f6` kept (CI's selection); stale `39b89c8`, `b108fa4`, `bd7bbb8`, `fe9d4db`, `21c95f6-s4`, `21c95f6-s9` deleted (no test or workflow referenced them) |
| parity | `check --impl python` 149 steps agree; `check --impl rust` (cli transport) 149 steps agree |
| ledger | 36 rows touching the Python core, no new migrations, no new operations. The 3 `unassigned` rows (`src/vogt/__init__.py` version bumps) are now the `version` chunk in ownership.toml |
| items raised | triage in `drift-triage-2026-10-19.md`: 28 actionable rows, filed as one comment each on WI-1073 (9), WI-1074 (4), WI-1058 (3), WI-1064 (4), WI-1065 (8) and WI-1050 (config cross-reference); 8 no-ops |
