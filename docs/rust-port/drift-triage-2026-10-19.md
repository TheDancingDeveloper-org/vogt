# Rust port drift triage — 2026-10-19

Triage of `scripts/drift_ledger.py` (P0.5 / WI-1047) against `origin/main`
as of `f8402e92`. This is triage only: nothing was ported, no golden was
re-recorded, and rust-core was not rebased. The rebase is deferred to
oversight under I6 (WI-1081).

## Range

| | |
| --- | --- |
| port base | `21c95f674e1b03ee177394f605abb1abfac41cdd` (merge-base of `origin/rust-core` and `origin/main`) |
| main tip | `f8402e92` — *fix(web): stop the event-stream reconnect loop flapping "Engine unavailable" (WI-1903)* |
| rust-core tip | `1ac3934a` |
| ledger rows | **36** commits that touch `src/vogt/`, `tests/`, `scripts/`, `docs/CONFIG.md` or `config.example.toml` |
| all commits on main since the base | 68 |

The 3 rows that had been `unassigned` only touched `src/vogt/__init__.py`, the
version string. It now belongs to a `version` chunk in
`tests/parity/ownership.toml`.

## Classification

| class | rows | what it means |
| --- | --- | --- |
| new operation | 0 | the registry gained no operation; `e1fe371f` only rewrites the `session.answer` description |
| behaviour change | 22 | an existing operation behaves differently; the Rust port must absorb it |
| config / schema / migration | 6 | new config keys (GitHub App, rate budget) and new result fields (`ClaimCheck`, `ClaimPull`, digest question fields). No migration. |
| docs / tests-only no-op | 8 | version bumps, CI policy and engine-only tests |

Every actionable row has an owning port slice. **No new work item was created.**

## Actionable drift, by owner

### WI-1073 — S8a sessions I (start/stop/list/screen/input/wait/wake/hibernate/sweep)

- `d6353bcc` `db92e26a` — queued input left in the box is submitted; engine-sized waits follow queued input; stuck verdict; sweep flags input pending; multi-line bodies are pasted; Claude suggestions are ignored (WI-1870). This touches `core/delivery.py` and `core/oversight.py`, and adds Claude screen fixtures.
- `f8edcf48` — sweep: a session at a question menu needs you, not idle; the digest carries `question`, `question_kind` and `options` (WI-1871)
- `6da0252d` — every agent start gets a Task section; sweep flags awaiting-task (WI-1854)
- `720e794d` `87c15a0f` — a Claude model runs Claude Code; a refused turn is blocked; a `Task:` reason is the task; a hand-resumed oversight keeps ownership of what it starts (identity/context change in HTTP and MCP adapters) (WI-1852)
- `00f61680` `e0b29721` — stale blocked reports are cleared or aged out (WI-1834)
- `7f4a4d11` — `AUTOPILOT: DONE` publishes a done change; adds the `done_at` field (WI-1518)

### WI-1074 — S8b sessions II (grants, answers, blocked, agent activity)

- `e1fe371f` — scoped oversight answers: an oversight session may answer "Yes" on an allow-listed one-line Bash command in the worker's worktree, decided by the engine; the worker-loop allow list (WI-1831)
- `78a36365` `aa28d580` `76c70871` — completion claims are checked against git and the forge: new `core/claims.py` and `services/claims.py`, `ClaimCheck`/`ClaimPull` on work results, DONE claims noted on the item, `merged_by` recorded, and only the session's own commits counted (WI-1835)

### WI-1058 — P1.9 application (brief and policy rendering)

- `a774099e` — the landing policy overrides a contradicting task in the brief (`_policy_conflicts.py`) (WI-1832)
- `f7609a52` — the landing base is resolved per initiative and per named long-lived branch; touches `core/entities.py` (WI-1841)
- `3b86fc64` — brief text for session scratchpad cleanup (WI-1810)

### WI-1064 — P2.6 external adapters (engine client, budgets)

- `a4cc0cdd` `49862911` — turn budgets are sized per model, with a budget restart cap (`core/dispatch.py`) (WI-1833)
- `5c9c0068` `f760de03` — budget-stopped sessions are handed off and restarted on their own model, effort and budget (`services/budget_handoff.py`, HTTP scheduler) (WI-1833)

### WI-1065 — P2.7 forge adapters

- `c8d62119` `4b4dee65` — authenticate to GitHub as a GitHub App installation (`adapters/github/app_auth.py`), with new config keys `github_app_id`, `github_app_installation_id` and `github_app_private_key_file` (WI-1838)
- `1c59243e` `954b8bfd` — spend less of the shared GitHub budget: `forge_posture_interval_seconds` and `github_rate_limit_floor`, and a rate-limited posture read raises instead of recording "no config" (WI-1838)
- `ce2225ba` — `merged_by` is read from GitHub's single-PR endpoint and cached per PR (WI-1835)
- `92047ce1` `e6a0d56e` — a page of pull requests no longer wedges the issue sync
- `61ee4b37` — a not-planned upstream close finishes as `wont_do` (touches `core/drift.py` and `core/observed.py`; WI-1070 should check the drift side)

### WI-1050 — P1.2 configuration (cross-reference)

- `c8d62119` `4b4dee65` `1c59243e` — five new config keys (listed under WI-1065), plus `docs/CONFIG.md` and `config.example.toml`. The CONFIG.md golden changes at the next re-record.

## Migrations (hard items, not optional drift)

None. No `.sql` file under `migrations/` changed in the range.

## New operations on main (0)

None added and none removed.

## Docs / tests-only no-ops (8)

Version bumps: `f45128ea` (v0.7.20), `d867e845` (v0.7.19) and `8c709c72`
(v0.7.18). These touch `src/vogt/__init__.py` and `tests/test_product_version.py`.

CI policy: `7a5a2133` (`scripts/check_workflow_policy.py` and its test, WI-1886).

Engine agent-auth tests only: `7b181744`, `e23dfedb`, `156be815` and `9dcb9322` (WI-1812).
