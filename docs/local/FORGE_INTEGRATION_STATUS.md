# Forge integration (#171) — delivery status

_Working note. Branch: `feat/forge-provider-integration`. Not for commit unless
you want it as a tracked handoff._

## Landing update (2026-08-21)

**Phases 1–4 committed and in PR #177 against `dev`.** Phase 4 committed as
`11626c5` (`forge(4/5): … (#175)`). Suite green: **954 passed, 0 failed** — the
pre-existing `test_cli` Python-3.14 argparse-colour failure is now fixed
separately (`150d793`, forces `NO_COLOR`; no production change). `ruff` and
`mypy` clean; web `tsc`+`vitest` clean (420 passed).

- **PR #177 — MERGED to `dev`** (merge commit `604dd19`, 2026-08-21). Base `dev`
  (not `main`, see divergence note below). Carried the four forge phases **plus**
  work stacked on the same branch: #164 (surface header + rail) and #166/#165
  (dev APK front door) rode as ancestor commits and GitHub auto-marked #164/#166
  MERGED when their heads landed; #167/#168 (Sessions rail cleanup) and a
  deploy-doc drift fix were committed here (`898be31`, `c7911f4`). CI fully green
  before merge (ruff check + format, mypy over src+tests, pytest 954/0, engine
  fmt/clippy/test, web tsc+vitest, Android). Issues closed manually after merge
  (base `dev`, so no keyword auto-close): #172–#175, #169, #170, #167, #168, and
  #165 (via #166).

  Extra fixes to get CI green that the handover had not flagged — the branch had
  never been pushed, so only local `ruff check` / `mypy src/vogt` had run:
  `ruff format` on ten phase files (`b710695`); two mypy loop-var errors in
  seed-registry tests that bare `mypy` (src+tests) catches but `mypy src/vogt`
  missed (`c4bf8ce`); the `test_cli` NO_COLOR fix (`150d793`); and a pre-existing
  engine clippy failure on `dev` itself — clippy 1.98's `result_large_err` on
  `require_bearer` (`f2bd023`, an `#[allow]`; `dev`'s own CI was already red on
  it, unrelated to this work).

- **Phase 5 (#176, Forgejo) — DEFERRED past MVP1.** MVP1 of the forge work is
  GitHub-only per the pivot (below). #176 stays open with a deferral comment;
  the #171 provider interface it would validate is already delivered by
  Phases 1–4. Do **not** implement it now.

- **The pivot — "vogt as the forge frontend".** The maintainer agreed
  (2026-08-21) to make the forge the system of record: on a linked project the
  GitHub issue *is* the work item, with a thin local overlay. Recorded as
  **design issue #178**, split into implementation issues **#179–#183**
  (a: per-actor PAT linking; b: repo picker + parity-gated import; c:
  upstream-truth work items + overlay refactor; d: publish verb; e: native-item
  migration + forge-less guarantee withdrawal). #171 got a pointer comment;
  #178 supersedes #171's native-vs-mirror work model but not its provider
  interface.

- **origin/main divergence — finding + resolution.** The earlier worry that
  "months of work never reached origin/main" was a misread of the topology.
  Work on this repo flows through **`dev`**, not `main`: every recent PR
  (#160/#163/#164/#166) targets `dev`, and `HEAD` was exactly `origin/dev` + N
  commits with `origin/dev` fully contained. `origin/main` (`1026d2f`, the #129
  squash) is simply far behind `dev` — a pre-existing, separate promotion
  concern, not something this work created or must fix. The #129 content itself
  exists locally as `f6b9c33` on `ci/lift-runner-ceiling` (its build.yml is
  byte-identical to `origin/main`). **Resolution: base the forge PR on `dev`**
  (matching convention and the `vogt-dev` deploy target); leave `main`
  promotion as its own task for the maintainer. No force-push, no main rewrite.

## Goal (as set)

1. Deliver issues #170–#176 (the forge-provider integration, Phases 1–5).
2. Review-fix all remaining open GitHub issues.
3. Commit, push, merge the PRs, and deploy to **vogt-dev on node b**
   (the deploy will kill this terminal session, so it must be strictly last).

## Committed (all green at time of commit; see caveat)

| Commit | Phase | Issue |
|---|---|---|
| `dd5c5f8` | 1 — ForgeProvider seam, zero behaviour change | #172 |
| `c7c22ba` | 2 — incremental all-state sync, watermarks, last-confirmed, backlog fix | #173 |
| `5b0e930` | 3 — symmetric drift, trust from last-confirmed, PR view filter | #174 (closes #169, #170) |

Each commit: full suite green **except one pre-existing failure** —
`tests/test_cli.py::test_no_command_prints_help`. It fails on this machine's
**Python 3.14** venv because argparse now emits ANSI colour codes in help, so
`'usage: vogt' in stdout` misses. Confirmed failing identically at `dd5c5f8`
(the Phase-1 commit), i.e. it is **not** a regression from this work — the
project targets 3.11 (mypy `python_version = "3.11"`). Candidate fix for
task 2: set `NO_COLOR=1`/strip ANSI in the test, or disable argparse colour.

## Phase 4 (#175) — essentially DONE, UNCOMMITTED, suite GREEN

**Update (2026-08-21):** Phase 4 is now functionally complete. Full suite:
**953 passed, 1 failed** — the failure is the same pre-existing `test_cli`
Python-3.14 argparse-colour issue, nothing else. The grep exit criterion is
**met**: the only remaining references outside `adapters/forge/` &
`adapters/github/` are a comment in `adapters/git/clone.py:95` and
`imports.py` naming `GitHubProvider` (a **forge** export — the v1
github-shaped import path).

### Remaining before committing Phase 4 (small)
1. **4 ruff E501 line-length fixes** (nothing else fails ruff):
   - `src/vogt/adapters/forge/github.py:276` (`_version_update_config` get line)
   - `src/vogt/application/services/writeback.py:308`
   - `tests/test_notifications.py:186` and `:191`
   Then re-run `.venv/bin/ruff check src/vogt tests` and `.venv/bin/mypy src/vogt`.
2. Commit as `forge(4/5): the full surface behind the provider … (#175)`.

### Test migration done since last checkpoint
- `test_notifications.py`: `gh-notifications`→`forge-notifications`; imports
  `KIND_NOTIFICATION` from `adapters.forge`; the direct-collector test now uses
  `ForgeNotificationsCollector()`.
- `test_checks.py`, `test_drift.py`: seed helper `begin_sweep(collector=…)`
  now `forge-checks`.
- `test_github.py`, `test_forge_module.py`, `test_import.py`, `test_m2_demo.py`,
  `test_collectors.py`: already green (they exercise `github_collectors` /
  onboard / posture through the patched `from_token_file`, which now resolves
  the forge provider + read collectors).

### Decision taken (legacy github collectors)
Kept the legacy `GitHubActionsCollector`/`GitHubReleaseCollector` +
`github_collectors()` + `posture.py`/`notifications.py`/`consolidate.py` in
`adapters/github/` as tested lower-level modules. They are **unregistered in
production** (collect.py registers only the forge collectors) and only imported
by their own unit tests, so they don't violate the "no load-bearing GitHub
reference outside adapters/forge" criterion. A later cleanup could delete them
and fold their unit tests into `test_forge_provider.py`; not required for #175.

## (superseded) earlier note — Phase 4 in progress, suite RED

Goal of Phase 4: move the whole read+write surface behind the provider so no
load-bearing GitHub reference remains outside `adapters/forge/` (grep-verified),
and rename the remaining `gh-*` collectors.

### Done in the working tree
- `adapters/forge/kinds.py` (new): canonical KIND_* + collector-name constants +
  `COLLECTOR_ALIASES` (all six gh-* → forge-*, incl. gh-consolidate→forge-issues)
  + `current_collector()`. `sync.py` now sources its constants from here.
- `adapters/forge/models.py`: added `ForgeLabel`, `ForgePosture`, `ForgeNotification`.
- `adapters/forge/github.py`: `GitHubProvider` gained `labels()`, `posture()`,
  `notifications()`, `describe()` (repo-exists for import), `clone_token()`.
  Write-surface return type now `forge.writeback.WriteBackResult`.
- `adapters/forge/provider.py`: protocol extended with labels/posture/notifications.
- `adapters/forge/collectors.py` (new): provider-backed, capability-gated,
  renamed read collectors — `forge-checks`, `forge-releases`, `forge-labels`,
  `forge-posture`, `forge-notifications` — with `not_supported` receipts.
  Notification/label subject keys deliberately kept as the old
  `gh:{slug}!{thread}` / `ghlabel:{slug}/{name}` schemes so observations dedup.
- `adapters/forge/writeback.py` (new): `WriteBackPolicy/Action/Result`,
  `PERMITTED`, `permits` moved here from `adapters/github/writeback.py`
  (which now re-imports them). Provider-agnostic.
- `adapters/forge/registry.py`: added `github_identity()` (token-less provider
  for pure parse/URL work) and `has_configured_forge()` now routes through
  `spec.build` (honours the `from_token_file` monkeypatch tests use).
- Service layer rewired to import forge-only:
  - `services/collect.py`: registers `forge_sync_collectors` +
    `forge_read_collectors` (dropped `github_collectors`).
  - `services/writeback.py`: `attempt()` now calls the **provider** write
    surface (`comment/create_issue/add_labels/set_state`); `onboard()`
    reworked to "reset watermark + run the forge collectors" (no
    `GitHubConsolidator`); helpers `_count_latest`, `_ONBOARD_READS`.
  - `services/imports.py`: uses `github_identity()` + `github_provider()`;
    `_describe` takes a provider; no `GitHubClient` construction.
  - `services/drift_service.py`, `inbox.py`, `notifications.py`: KIND_* and
    collector names imported from `adapters/forge`.

### NOT yet done in Phase 4
- **Tests still reference the old collector names/classes** → suite is RED.
  Last run: `tests/test_notifications.py` (16 failures) selects
  `SweepParams(collectors=["gh-notifications"])`; also references to update:
  - `tests/test_notifications.py`: all `gh-notifications` → `forge-notifications`;
    `GitHubNotificationCollector` import/use (line 23, 226-228).
  - `tests/test_github.py`: line 26 `GitHubPostureCollector` import + line 116
    use; line 109 docstring; the 4-name set at 173-176 is the
    `github_collectors()` legacy set — decide whether `github_collectors` and
    the legacy `GitHubActions/Release/Posture/Notification` collectors +
    `GitHubConsolidator` should be **deleted** (they're now unregistered in
    prod) or kept as tested implementation detail. Cleanest: delete them and
    their tests, since Phase 4's point is the rename.
  - `tests/test_checks.py` line 30 `collector="gh-actions"` → `forge-checks`.
  - `tests/test_drift.py` line 153 `begin_sweep(collector="gh-actions")` →
    `forge-checks` (or rely on alias map — but coverage keys on the live name).
  - `tests/test_forge_module.py`: the `forge` fixture + onboard/posture/
    notification tests will now run the **forge-*** collectors via the patched
    `from_token_file`; verify onboard counts still hold (issues==4 should pass
    because forge-issues reads state=all; labels==1 now comes from the
    **forge-labels** read collector — verify it runs in onboard via
    `_ONBOARD_READS`).
- **Retire the legacy github modules** once tests are migrated:
  `adapters/github/collectors.py` (GitHubActions/Release + `github_collectors`),
  `posture.py`, `notifications.py`, `consolidate.py`. Keep `client.py` and
  `writeback.py` (the provider's implementation). Re-grep the exit criterion:
  `grep -rn "adapters.github\|GitHubClient" src/vogt --include=*.py | grep -v adapters/forge/ | grep -v adapters/github/` should show only comments.
- **Docs**: CONFIG unchanged; consider a REQUIREMENTS note that the r15
  residual for forge subjects is closed (SCHEMA.md already updated in Phase 2).

## Remaining phases / tasks

- **Phase 5 (#176)** — `ForgejoProvider` against `/api/v1`, host-qualified keys
  `forge:{host}/{owner}/{repo}#{n}`, capability declaration (posture/notifications
  = false), invert the two WI-9 not-supported tests to supported-path, new
  `tests/test_forgejo.py`. Registered by host from `forge_token_files`.
- **Task 2** — review-fix all other open issues. Open issues seen so far:
  #169 (closed by Phase 2/3), #170 (closed by Phase 3). Run
  `gh issue list --state open` to get the full set. Note the pre-existing
  `test_cli` Python-3.14 argparse-colour failure is a real fix candidate.
- **Task 3 (LAST — kills the session)** — commit Phase 4/5, push the branch,
  open/merge the PR(s), trigger deploy to **vogt-dev on node b**. Check
  `deploy/vogt-stack.komodo.md` and the Komodo/Woodpecker credentials
  (per AGENTS.md) for the deploy trigger. Confirm before firing.

## How to verify quickly
```
.venv/bin/python -m pytest -q            # full suite (~70s)
.venv/bin/ruff check src/vogt tests
.venv/bin/mypy src/vogt
```
Unrelated dirty files in the tree (leave them): `web/*`, `deploy/vogt-stack.komodo.md`,
`docs/OPEN_ISSUES_PLAN.md`, `docs/OPEN_SOURCE_TRANSITION.md`.
