# Vogt — Agent Guidance

A standalone, self-hosted, open-source product development environment
(Jira-like scope) built AI-native. Two implementation languages, deliberately:
**Python** for the domain — entities, workflow, ranking, storage, the operation
registry and every adapter generated from it — and **Rust** for the session
engine merged in from MyDevEnv2, which runs PTYs, streams them over WebSocket
and serves the PWA. The split is not accidental and is argued in
`docs/MERGE_MYDEVENV2.md` §4: porting 16.5k lines of audited domain logic to
Rust, or a PTY fan-out engine to Python, both cost more than a narrow interface
between two processes does.

Nothing about that changed the Python core. It is still the authority for the
operation registry, and an operation that does not exist there does not exist
on any surface.

## Where things live

- `docs/DESIGN.md` — the design outline; source of truth for architecture,
  domain model, and roadmap. Update it when decisions change; don't fork
  competing design docs. **It describes what exists** — a capability that was
  designed and never built belongs in `docs/REQUIREMENTS.md` §7, not here, and
  r11 says why at length.
- `design/` — diagrams, mockups, exploratory notes (may be messy). Holds
  `restructure-2026-08/`, the desktop and mobile restructure export. Its two
  rules are in `design/README.md`: nothing there is a specification, and
  nothing there is a source of truth about the built product.
- `docs/RESTRUCTURE.md` — the staged plan for turning that export into the
  shipped PWA and phone app. A plan, not a requirement: nothing in it is owed
  until its Stage 0 mints an ID in `docs/REQUIREMENTS.md`.
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
- `engine/` — the Rust session engine, merged from MyDevEnv2 with its history.
  It is **its own Cargo workspace** (`engine/Cargo.toml`, members `server` and
  `contract`); the repository root is not one, so `cargo` run from the root
  finds no manifest. `engine/server` owns PTY lifecycle, WebSocket attach, SSE,
  the workspace-scoped file and git APIs and the assistant loop;
  `engine/contract` is the shared wire DTOs the PWA mirrors in TypeScript. It
  carries its own `Dockerfile` and `deploy/` because it publishes its own
  image to its own Komodo stack, which is why those live under `engine/`
  rather than joining the root ones. It **no longer carries a `.woodpecker/`**:
  the fork brought one across and it was inert here — Woodpecker builds the
  Forgejo repositories, and this one is on GitHub — while still reading as an
  authoritative second way to publish and deploy the same Dockerfile. The
  engine's image is built by `.github/workflows/build.yml`, and the pre-merge
  Forgejo repository keeps its own pipeline. Read `engine/AGENTS.md` before
  changing anything in there.
- `engine/Dockerfile.pod` — the dev-pod toolchain (Node, the Rust dev
  toolchain, sway, selkies, Java/Gradle, the Android SDK, Flutter), split out
  of the merged image's runtime stage because it was 22 of that build's 34
  minutes and depends on none of this repository's source. Published as
  `vogt-pod-base:{lean,full}-<hash>-<week>` by
  `.github/workflows/pod-base.yml` and consumed by digest as `POD_BASE_IMAGE`.
  **Adding a tool to the pod means editing this file**, not `engine/Dockerfile`;
  `docs/DEPLOYMENT.md` §10 documents what it carries.
- `web/` — the Solid/Vite PWA, and the product's GUI going forward. It is baked
  into the Rust binary at compile time (`engine/server/src/assets.rs` embeds
  `../../web/dist/`), so a `cargo build` without a fresh `pnpm build` ships a
  stale frontend — the single most common way to "fix" a UI bug and see
  nothing change. `src/vogt/gui/static/` is the older buildless GUI and stays
  until the Solid surfaces reach parity.
- `mobile/` — the Capacitor 8 Android shell. Its WebView loads the deployed PWA
  directly, so UI changes ship without an APK rebuild; only native plumbing
  (plugins, manifest, FCM) needs one.
- `docs/ENGINE.md` — the engine's single reference: what it owns, how to run it,
  its full wire contract (§5), the assistant (§6), agent tasks (§7). It replaced
  `docs/engine/`, which held eight documents describing MyDevEnv2 as a separate
  product. **There is no `docs/engine/` directory**; a path naming one is stale.
- `docs/USER_GUIDE.md` — how a person drives the product, both halves of it.
- `docs/REQUIREMENTS.md` §7 — the gap register: everything designed anywhere in
  this documentation set and not built, split into what is owed (with an ID)
  and what was withdrawn (deliberately without one). **The engine has no
  separate backlog** — that is what §7 and `docs/ROADMAP.md` are for.
- The archived GPUI desktop client was **not** carried across; the MyDevEnv2
  repo remains its archive. A reference to `client/` anywhere in this tree is a
  reference to something that is not here.

## Working on the code

The Python core, from the repository root:

```bash
uv sync                       # dev environment
uv run pytest                 # tests + coverage gate (>=80%)
uv run mypy                   # strict, over src/, tests/ and scripts/
uv run ruff check . && uv run ruff format --check .
uv run python scripts/check_docs.py
```

The engine and the PWA, each from its own directory — there is no root-level
runner that drives all three, and inventing one would hide which toolchain
failed:

```bash
cd engine                     # Cargo workspace root
cargo fmt --check
cargo clippy -- -D warnings
cargo test --all              # server unit + integration (HTTP + WS)

cd ../web
pnpm install
pnpm typecheck
pnpm build                    # refresh web/dist/ before a release cargo build
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

**The merge did not merge the deployments.** Two pipelines still feed separate
stacks, but only one of them lives here: this repository (GitHub Actions →
GHCR → `personal/vogt`), and the pre-merge Forgejo repository
`indexarr/MyDevEnv2` (Woodpecker → Forgejo registry → `prod-mydevenv2` and
`dev-mydevenv2`). The second one is genuinely elsewhere. The fork vendored a
copy of its `.woodpecker/server.yml` into `engine/`, which never ran — Vogt is
not one of Woodpecker's repositories — and it has been deleted, because a
pipeline that publishes a competing image and redeploys production is a bad
thing to have lying around looking real. Both stacks are still
Komodo-from-`indexarr/ops` on Node B, so the rules above apply to both.
Collapsing them into one stack is planned, not done —
`docs/MERGE_MYDEVENV2.md` §10 — so do not assume a change under `engine/`
reaches the standalone stacks by merging here, or the reverse.

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
