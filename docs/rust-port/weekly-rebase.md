# Rust port: weekly rebase procedure (P0.5 / WI-1047)

Main keeps moving while the port lives on `rust-core`. Once a week:

1. `git fetch origin && git rebase origin/main` on `rust-core` (or merge, per
   the branch's current sync rule).
2. Record the golden through CI, never from a worker checkout. Dispatch
   `rust-core.yml` on the ref to record with `record_golden=true`
   (`gh workflow run rust-core.yml --ref <ref> -f record_golden=true`).
   The job writes `tests/parity/golden/<sha7>/{cli,http,mcp,schema}.json` and
   their audits under the pinned `UV_PYTHON`, then opens a `golden/<sha7>`
   pull request. Land that pull request before relying on the golden
   (WI-2661: workers never commit a golden re-record).
   The job names the directory for the dispatched commit. When that is not the
   merge base, the selector falls back to the newest ancestor recording and
   announces it (see `.github/workflows/rust-core.yml`).
3. Do not delete another recording by hand. The selector keeps the one named
   for the merge base, or the newest ancestor when none matches;
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

### 2026-10-20 (I6 drift closure, WI-1081)

| | |
| --- | --- |
| sync | `origin/main` @`09d3c552` (the operator's freeze SHA) **merged** into rust-core through a PR, no rebase. New merge base `09d3c55`. |
| golden | `tests/parity/golden/09d3c55` recorded from the Python core on cli, http, mcp and the schema slice; a second independent recording of each was byte-identical. `21c95f6` deleted. Contract (cli-help, CONFIG.md, config.example.toml), `errors.json`, `registry.json`, `mcp-tools.json` and `openapi.json` regenerated from Python. |
| parity | Python and Rust both green: cli 170, http 89, mcp 170 steps; `--only dump`, `--only actor`, `--only schema`; crossopen; contract; selftest; `errors --check`; `gen_registry.py --check`; `gen_validator_corpus.py --check`. |
| ledger | 36 rows since `21c95f67`. 10 are no-ops (version bumps, CI policy, engine-shell tests, typing); 3 were already ported (078e979c); the other 23 are ported in WI-1081. The halves that need infrastructure the Rust core lacks are follow-ups WI-2021 (scheduler), WI-2022 (digest rows), WI-2023 (Codex resubmit loop) and WI-2024 (WI-1590 model policy, `project.update agent_policy`). No new migrations. |
| accepted differences | recorded in the row above; the named file was never added |

### 2026-10-27 (WI-2623, merge-base moved by #1193)

| | |
| --- | --- |
| sync | `origin/main` @`84ec1058` fast-forwarded into rust-core through #1193. New merge base `84ec105`. |
| golden | After `g sync` to `ce791d17882ee`, `tests/parity/golden/84ec105` was re-recorded from the Python core on cli, http, mcp and the schema slice, including migration 0028 (declared schema version 28). `09d3c55` deleted. Contract fixtures (cli-help, CONFIG.md, config.example.toml) and `errors.json` regenerated from Python. |
| parity | Python green against `84ec105`: cli, http 89, mcp 170; `--only actor`, `--only schema`; selftest; `errors --check`; `contract --impl python`. Rust green: http 89, mcp 170, `--only dump`, `--only schema`, crossopen. `contract --impl rust` still differs: the Rust CLI help lists `charter` and `watch` and wraps long usage lines differently, and Rust renders no CONFIG.md. |
