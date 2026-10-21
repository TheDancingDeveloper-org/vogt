# CI: pull requests, the merge queue, and `main`

Vogt's CI does two different jobs. A pull request gets fast feedback on what
it changed. The merge queue proves that the exact tree `main` will become still
passes everything. Before WI-1886 every PR ran the whole suite (20–60 minutes),
and each merge forced sibling PRs to rebase and run it all again.

## What runs where

| Event | `ci.yml` | `codeql.yml` | `runner-policy` | `dependency review` |
|---|---|---|---|---|
| `pull_request` | only the halves the diff touches (below) | **does not run** | full | reviews the PR's dependency changes |
| `merge_group` (the queue) | **everything** | **does not run** | full | posts the context; the review already ran on the PR |
| `push` to `main` | everything | **does not run** | full | — |
| `push` of a `v*` tag (beta, prod) | — | every language | — | — |
| `workflow_dispatch` | — | every language, at `ref` if given | — | — |

### Path scoping on a pull request

`ci.yml`'s `classify changes` job reads `git diff` against the merge base:

| Changed paths | Runs |
|---|---|
| `.github/workflows/`, `Dockerfile`, `.dockerignore`, `deploy/`, `.gitignore` | everything, as in the queue |
| `src/`, `tests/`, `scripts/`, `pyproject.toml`, `uv.lock`, `config.example.toml` | Python lint/mypy/pytest on 3.13, and the core-alone job |
| `engine/`, `docs/ENGINE.md` | the engine job's **rust** half: fmt, rust-version, cargo audit, clippy, nextest, doctests |
| `web/` | the engine job's **web** half: pnpm audit, typecheck, vitest, demo Playwright, PWA build, bundle budget |
| `voice/` | the voice job |
| `engine/server/**` or `engine/contract/**` | only the Rust member the diff touches: clippy, nextest and doctests run with `-p` |
| `voice/{server,stt,tts,models}/**` | only that voice member, likewise scoped with `-p` |
| `mobile/` | the android job |
| anything else (docs, markdown) | nothing: the `ci` gate passes on skipped halves |

Scoping narrows *which crates* are checked, never *what* is checked: a PR
touching one member runs the same clippy, nextest and doctest steps as the
queue, just with `-p <member>` instead of `--workspace`. Anything that cannot
be attributed to one member (`engine/Cargo.toml`, a new crate, another
`engine/` or `voice/` path) falls back to the full workspace, as does a scope
that resolves to nothing. `cargo fmt`, the rust-version check and
`cargo audit` read the whole manifest tree and are never scoped.

Only the queue and `main` run these:

- Python **3.11**, the declared floor. A PR runs 3.13 only.
- the core-alone job when no Python changed. It deletes `engine/`, `web/` and
  `mobile/` before it tests, so on a PR only a Python change can affect it.
- a web-only change rebuilt into the engine. The crate embeds `web/dist`, but
  a web-only PR does not run clippy or nextest against the new bundle.

### Speed: caches and test runners

Each job reuses work rather than repeating it:

- `Swatinem/rust-cache` for the engine and voice workspaces, with a
  per-workspace `shared-key` (`engine-ci`, `voice-ci`) so a scope change does
  not throw the dependency cache away. Only `main` saves a new cache; PRs
  restore it.
- `astral-sh/setup-uv` with its cache enabled and keyed on `uv.lock`, so a
  Python job restores the resolved environment instead of resolving again.
- `cargo-nextest` for the Rust and voice tests. It runs each test in its own
  process, so a hang is terminated by `engine/.config/nextest.toml` and
  reported as a failure instead of holding the job to its timeout
  (WI-1312), and `--no-fail-fast` lists every failure in one round.
  Doctests are not nextest's job and keep their own step.

Python's `pytest` stays serial on purpose. The suite is bound by SQLite file
I/O rather than CPU (about 275 s of 291 s in the 2026-10 profile), and its
tests bind sockets and spawn subprocesses, so xdist would add failure modes
without buying wall clock. `pytest -n auto` is not used and does not need to
be: a PR already runs only the Python job the diff touches.

### Release dry run

`release.yml` runs only on a `v*` tag, so without a rehearsal the first
execution of the release path is the release itself. `release-dry-run.yml`
removes that: on a PR touching the files a release reads (the release, build,
pod-base and mirror workflows, the Dockerfiles and their ignore files, the
version manifests and the version/policy scripts) and nightly at 03:00 UTC on
`main`, it runs

- the release-shape tests, the workflow-policy gate, and
  `check_product_version.py` with no tag (the manifests agree with each
  other), and
- a local build of the core, then of the merged stack on top of that local
  core, starting both halves.

There is nothing to leak: no login, no `packages: write`, no `secrets.`, no
`id-token`, no `cosign`, no `push`, no tag and no GitHub Release. It is
deliberately not a required check (it depends on a Docker runner and is slow),
so it can only ever be advisory on a PR; its nightly run is what notices drift
that no PR touches.

### Quarantined jobs

`runner-watchdog.yml` runs on a schedule only and is quarantined with
`continue-on-error: true` while `gh` is absent from the self-hosted runner
image (it exits 127). The red run stays visible in the Actions tab; the
`TODO(WI-2073)` on the job is the removal condition. Because it has no PR or
`merge_group` trigger, there is no required context to restrict.

### CodeQL

CodeQL runs only when a release is cut, as an operator decision. It does not
gate pull requests, the merge queue, or `main`, so its `analyze (<language>)`
jobs are not required checks. A `v*` tag push (beta and prod) runs it. So does
a `workflow_dispatch`: the dev deploy lane passes the commit it deploys as the
`ref` input, and checkout uses that SHA. Every such run analyses all three
languages. The job names are unchanged.

### Concurrency

Every workflow that runs on PRs cancels in progress per ref (`github.ref`).
A force-push to a PR therefore cancels the superseded run. Each queue entry has
its own `gh-readonly-queue/main/pr-N-<sha>` ref, so queue entries never cancel
each other. The queue removes an entry it no longer needs.

### Required checks must run in the queue

A required context that nothing posts on `merge_group` leaves every queued
entry waiting until the queue times it out. `scripts/check_workflow_policy.py`
(run by `runner-policy`) fails when a check listed under SECURITY.md's
"required checks" has no `merge_group` producer.

### Runners

Queue runs use the same labels as PR runs: `self-hosted`, plus `docker`, `rust-heavy`,
`publish` or `bigmem` where a job asks for one. No vogt workflow targets the
GCP spot labels; the engine job uses `rust-heavy` (OOM on the small pool). The queue adds full runs, but PRs now skip
everything they do not touch, and a merge no longer re-runs every open PR.
CodeQL no longer runs in the queue. Its single `bigmem` slot serialises the
three languages within a release run.

## How changes land

1. Branch from the base — `origin/integrate/vogt-next` for a worker (see
   [`CONTRIBUTING.md`](CONTRIBUTING.md#2-workflow)), `origin/main` for a fork
   pull request. Run the targeted gates for what you touched.
2. Push and open the pull request. It runs only the path-scoped checks: fast
   feedback on the changed halves, not the whole suite.
3. The review happens on the pull request. The proposed tree is not retested
   by a second review run.
4. `land-pr` enqueues it. The merge queue runs the full suite on the exact
   tree `main` will become: Python 3.11 and 3.13, the core alone, Rust and
   the PWA, voice, the Android shell, and the `ci` gate.
5. Merge only when `ci`, `dependency review` and `runner-policy` are green —
   those are the required status checks on `main`. A red queue run dequeues
   the pull request; fix it and enqueue again.
6. Versions move only at release. Feature pull requests do not bump them, and
   a `v*` tag is cut from `main` after the integration pull request lands.

The `ci` gate takes seconds; the queue run is the expensive part and is
deliberately not duplicated on every push to a pull request.

## Landing

`land-pr` (workspace `.agent-tools/bin/land-pr`) detects a merge queue from
the base branch's rulesets. It enqueues with `gh pr merge --auto` and follows
the entry's `merge_group` runs. On a failed run it prints the failing job's
log and exits 1. The PR is never rebased by hand while it is queued.

The required status checks on `main` are `ci`, `dependency review` and
`runner-policy`. Those are the three contexts a change must produce; every
other job (`build`, `e2e`, `release`, `codeql`) is a separate workflow and is
not required. Feature pull requests do not bump the product version: the
version moves only in a release change, and `check_product_version.py` in CI
asserts the manifests currently agree rather than that they changed.

Workers run targeted checks before pushing — the test file they touched, the
one crate they touched, the one surface they touched — and let the queue run
the full suite. `scripts/check.sh <surface>` is the convenience form. See
[`CONTRIBUTING.md`](CONTRIBUTING.md#2-workflow) for the workflow itself.

## Turning the queue on (operator)

The workflows support a queue but do not require one. Until a ruleset with a
`merge_queue` rule exists on `main`, PRs merge exactly as before, with the
narrower PR checks. The exact ruleset is in the WI-1886 hand-off. In
outline: one branch ruleset on `main` with a `merge_queue` rule
(method rebase, build concurrency 3, group size 1–3, wait 5 minutes,
`ALLGREEN`, check timeout 60 minutes) and the same required checks as today
(`ci`, `runner-policy`, `dependency review`).

Once the queue is on, `push` to `main` re-runs the full suite on a commit the
queue has already tested. It is kept deliberately for now: `build.yml` and
`e2e.yml` key off it, and it is the full check for anything that reaches `main`
without the queue. Dropping `ci.yml`'s `push` trigger is a follow-up once the
queue has proved itself.
