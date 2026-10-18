# CI: pull requests, the merge queue, and `main`

Vogt's CI does two different jobs. A pull request gets fast feedback on what
it changed. The merge queue proves that the exact tree `main` will become still
passes everything. Before WI-1886 every PR ran the whole suite (20–60 minutes),
and each merge forced sibling PRs to rebase and run it all again.

## What runs where

| Event | `ci.yml` | `codeql.yml` | `runner-policy` | `dependency review` |
|---|---|---|---|---|
| `pull_request` | only the halves the diff touches (below) | only the languages the diff touches | full | reviews the PR's dependency changes |
| `merge_group` (the queue) | **everything** | every language | full | posts the context; the review already ran on the PR |
| `push` to `main` | everything | every language | full | — |

### Path scoping on a pull request

`ci.yml`'s `classify changes` job reads `git diff` against the merge base:

| Changed paths | Runs |
|---|---|
| `.github/workflows/`, `Dockerfile`, `.dockerignore`, `deploy/`, `.gitignore` | everything, as in the queue |
| `src/`, `tests/`, `scripts/`, `pyproject.toml`, `uv.lock`, `config.example.toml` | Python lint/mypy/pytest on 3.13, and the core-alone job |
| `engine/`, `docs/ENGINE.md` | the engine job's **rust** half: fmt, rust-version, cargo audit, clippy, nextest, doctests |
| `web/` | the engine job's **web** half: pnpm audit, typecheck, vitest, demo Playwright, PWA build, bundle budget |
| `voice/` | the voice job |
| `mobile/` | the android job |
| anything else (docs, markdown) | nothing: the `ci` gate passes on skipped halves |

Only the queue and `main` run these:

- Python **3.11**, the declared floor. A PR runs 3.13 only.
- the core-alone job when no Python changed. It deletes `engine/`, `web/` and
  `mobile/` before it tests, so on a PR only a Python change can affect it.
- a web-only change rebuilt into the engine. The crate embeds `web/dist`, but
  a web-only PR does not run clippy or nextest against the new bundle.

CodeQL classifies `*.py`, JS/TS sources plus `package.json`/`pnpm-lock.yaml`,
and `*.rs` plus `Cargo.*`. A language with no change still reports its
`analyze (<language>)` context, with its steps skipped. A change to
`codeql.yml` or `.github/codeql/` analyses everything.

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

Queue runs use the same labels as PR runs: `self-hosted`, plus `docker`,
`publish` or `bigmem` where a job asks for one. No vogt workflow targets the
GCP rust-heavy or spot labels. The queue adds full runs, but PRs now skip
everything they do not touch, and a merge no longer re-runs every open PR.
CodeQL still has a single `bigmem` slot, so its three languages serialise in
the queue just as they did on PRs.

## Landing

`land-pr` (workspace `.agent-tools/bin/land-pr`) detects a merge queue from
the base branch's rulesets. It enqueues with `gh pr merge --auto` and follows
the entry's `merge_group` runs. On a failed run it prints the failing job's
log and exits 1. The PR is never rebased by hand while it is queued.

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
