# Vogt — Agent Guidance

Design-phase project: a standalone, self-hosted, open-source product
development environment (Jira-like scope) built AI-native. Python is the
implementation language.

## Where things live

- `docs/DESIGN.md` — the design outline; source of truth for architecture,
  domain model, and roadmap. Update it when decisions change; don't fork
  competing design docs.
- `design/` — diagrams, mockups, exploratory notes (may be messy).
- `src/vogt/` — the implementation (M0 and M1 landed). Layer order is
  strict: `core` (entities, ids, time, digests, the workflow engine, the
  ranking function, the contract) → `storage` (interface + the SQLite
  backend, the only place SQL lives) → `application` (use-cases in
  `services/`, the transactional write path) → `registry` (one definition
  per operation) → `adapters/{cli,http,mcp}` (thin, generated from the
  registry). An adapter that decides anything is the bug.
  - `core/workflow.py`, `core/ranking.py` and `core/observed.py` are pure:
    no storage, no context, no clock of their own. That is what makes them
    testable against a table of cases and `why` an explanation rather than
    a summary.
  - `adapters/git/` is the only place Vogt runs `git` to *change* local
    state (cloning, for `project.import`). `collectors/git_local.py` also
    shells out to git and stays where it is, because it reads a checkout
    that already exists — the split is "writes a working tree" vs "does
    not", not "uses git".
  - `collectors/` return findings and write nothing (FR-O2). The sweeper
    appends them and writes the coverage record; the worst a broken
    collector can do is produce stale evidence, visible as such.
- `docs/CONFIG.md` and `config.example.toml` are **generated** from
  `src/vogt/config.py` by `scripts/gen_config_docs.py`. Edit the schema, run
  the script; CI fails on drift (NFR-Q4).

## Working on the code

```bash
uv sync                       # dev environment
uv run pytest                 # tests + coverage gate (>=80%)
uv run mypy                   # strict, over src/, tests/ and scripts/
uv run ruff check . && uv run ruff format --check .
uv run python scripts/check_docs.py
```

Adding an operation means writing the handler in `application/services/`,
its parameter and result models in `application/models.py`, and one entry in
`registry/operations.py` — that alone gives it a CLI command, a REST route
with request and response schemas, and an MCP tool. Then add a step to
`SCRIPT` in `tests/test_parity.py`; the harness fails if you don't, and fails
again if you exclude it from a surface without saying why.

Three rules the code enforces so review does not have to:

- A **mutating** operation whose parameters lack a required `reason` fails at
  registry construction, not at runtime (FR-S1).
- Every declared write goes through `audited_write`, which lands the entity
  change, the audit row and the event row in one transaction (NFR-I1).
- Callers name things the way people do — `WI-7`, a project slug, an actor's
  `identity_ref`. Resolution lives in `services/_resolve.py`, so a typo
  fails as "no work item WI-70" rather than as a foreign-key error.

## Ground rules

- **Cadastre is prior art, not a dependency.** It lives at
  `~/Working/Active/cadastre` and is the same author's work — read it freely
  for patterns (declared/observed split, trust ledger, transport parity,
  audit design), but do not import from it or couple to its API in v1.
- Key inversions vs cadastre — do not regress these (`docs/DESIGN.md` §2):
  observed-first visibility, explicit collector coverage, a real write
  plane, actors/people as core entities.
- Scope decisions that are easy to reintroduce by accident — don't
  (`docs/REQUIREMENTS.md` §3 lists all of them with reasons):
  - **Nothing enforces.** Compliance, trust, and drift are values to be
    read; no operation may take them as a precondition (FR-G13).
  - **Nothing discovers.** Collection scope is the registered project
    list. No crawling roots, no candidate listings, no re-checking on a
    timer (FR-G15).
  - **No lockfiles, no resolved versions.** Dependency edges are path/git
    references between projects, nothing more (FR-D1).
  - **Observed-first is gated by promotion + suppression + exclusions**
    (`docs/DESIGN.md` §3.6) — never wire raw markers into ranked views.
  - AI-assisted drift detection is a **non-committed stretch goal**; no
    requirement or interface may assume it exists.
- Every feature must be reachable via CLI, REST, and MCP with tested parity;
  the GUI consumes the same HTTP adapter.
- Every write requires a principal and a reason (audit table).
- mypy strict + ruff from the first commit; no unmigrated schema changes.

## Deployment target (r4)

`docs/DEPLOYMENT.md` §2.2 is the target and is concrete: a Compose stack on
**Node B**, deployed by **Komodo** from `indexarr/ops` at `personal/vogt/`,
from a digest-pinned **GHCR** image published by tag-triggered GitHub
Actions. Four things are easy to get wrong here:

- **Never write an `ssh … docker compose up -d` deploy step**, and never
  hand-edit a deployed container. Komodo applies the ops compose; that is
  the only path (NFR-D7).
- **Don't gate allocation values.** `${PORT:?}`-style required values on a
  tailnet-bound port or an operator-owned cert path broke every cadastre
  deploy after `cadastre#42`. Defaults are *required* for these and
  forbidden for anything encoding exposure or identity (NFR-D2, revised —
  read §4.1 before "fixing" a default you find in a compose file).
- **No fronting proxy.** TLS terminates in-process from the host's
  Tailscale certificate; there is no Caddyfile entry and no public DNS
  record, by decision (NFR-D6, revised).
- **Publishing ≠ deploying.** A tag publishes a signed image and moves
  nothing. Production moves on a digest bump in ops plus `DeployStack`
  (NFR-D10).

Workspace-level Komodo/Infisical/runner rules live in `~/Working/AGENTS.md`
and are not restated here; `docs/DEPLOYMENT.md` §6 records only the Node B
failure modes that specifically bite this stack.

### If this repository ever goes public

`runner-policy.yml` calls a reusable workflow in the **private**
`github-policy` repo, and GitHub does not let a public repository resolve
that. Going public (NFR-O1 says it will, at a milestone of the owner's
choosing) therefore breaks the gate and must be handled in the same change.

Cadastre also moved its PR CI to GitHub-hosted runners when it went public,
because `pull_request` builds untrusted fork code and the self-hosted pool
sits inside the tailnet — see the header of
`~/Working/Active/cadastre/.github/workflows/ci.yaml`. Vogt is private, so
its self-hosted PR CI is fine today; the moment it is not, that reasoning
applies here too.

### CI prerequisites — done, do not redo (2026-08-12)

The new-repo checklist in `~/Working/docs/CI-RUNNER-GATES.md` has been run
for this repository. Recorded because its step 1 is the one that gets
skipped, and skipping it is how every hosted-runner violation in this org
has started: CI goes red for want of a runner, `ubuntu-latest` makes it
green, and nothing blocks the commit.

- `vogt` is in the **`public-node-b` runner group** (id 4).
- Actions are **enabled** for `vogt` (the org policy is
  `enabled_repositories=selected`, so this is not automatic).
- `main` is protected: required checks `ci` and
  `runner-policy / runner-policy`, strict, linear history, no force pushes.
  `enforce_admins` is false, matching cadastre — the owner can still push
  directly when something needs it.
- First run verified on `node-b-gha-public-rust` /
  `node-b-gha-public-publish-2`, both in `public-node-b`.

If CI is ever red with a job stuck queued, it is a runner-group question,
never a reason to change `runs-on`.
