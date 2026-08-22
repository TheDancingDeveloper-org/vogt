# Vogt — Agent Guidance

A standalone, self-hosted, open-source product development environment
(Jira-like scope) built AI-native. Two implementation languages, deliberately:
**Python** for the domain — entities, workflow, ranking, storage, the operation
registry and every adapter generated from it — and **Rust** for the optional
session engine, which runs PTYs, streams them over WebSocket and serves the
PWA. Porting 16.5k lines of audited domain logic to Rust, or a PTY fan-out
engine to Python, both cost more than a narrow HTTP interface between two
processes does.

The Python core is the authority for the operation registry, and an operation
that does not exist there does not exist on any surface. The engine is a
client of the core, never the reverse.

## Where things live

- `docs/DESIGN.md` — the design outline; source of truth for architecture,
  domain model, and roadmap. Update it when decisions change; don't fork
  competing design docs. **It describes what exists** — a capability that was
  designed and never built belongs in `docs/ROADMAP.md`, not here.
- `design/` — kept as an empty, tracked directory because the project
  contract requires it. Wireframes and mockups are maintainer working
  material and live outside the repository (git-ignored `docs/local/`).
- `src/vogt/` — the implementation. Layer order is strict: `core` (entities,
  ids, time, digests, the workflow engine, the ranking function, the contract)
  → `storage` (interface + the SQLite backend, the only place SQL lives) →
  `application` (use-cases in `services/`, the transactional write path) →
  `registry` (one definition per operation) → `adapters/{cli,http,mcp}`
  (thin, generated from the registry). An adapter that decides anything is
  the bug.
  - `core/workflow.py`, `core/ranking.py` and `core/observed.py` are pure:
    no storage, no context, no clock of their own. That is what makes them
    testable against a table of cases and `why` an explanation rather than
    a summary.
  - `adapters/git/` is the only place Vogt runs `git` to *change* local
    state (cloning, for `project.import`). `collectors/git_local.py` also
    shells out to git and stays where it is, because it reads a checkout
    that already exists — the split is "writes a working tree" vs "does
    not", not "uses git".
  - `collectors/` return findings and write nothing. The sweeper appends
    them and writes the coverage record; the worst a broken collector can do
    is produce stale evidence, visible as such.
- `docs/CONFIG.md` and `config.example.toml` are **generated** from
  `src/vogt/config.py` by `scripts/gen_config_docs.py`. Edit the schema, run
  the script; CI fails on drift.
- `deploy/` — `vogt.compose.yml` is the public base (published image),
  `vogt.build.yml` the build-from-checkout overlay, `.env.example` the values
  a host must state. Everything else in there is an overlay that states only
  its difference from the base; `tests/test_public_delivery.py` pins that the
  base stays self-contained. Operator-local deployment notes belong in the
  git-ignored `docs/local/`, never in the tree.
- `engine/` — the Rust session engine. It is **its own Cargo workspace**
  (`engine/Cargo.toml`, members `server` and `contract`); the repository root
  is not one, so `cargo` run from the root finds no manifest. `engine/server`
  owns PTY lifecycle, WebSocket attach, SSE, the workspace-scoped file and git
  APIs and the assistant loop; `engine/contract` is the shared wire DTOs the
  PWA mirrors in TypeScript. It carries its own `Dockerfile` and `deploy/`
  because it publishes its own image, separate from the core's. Read
  `engine/AGENTS.md` before changing anything in there. The dev-pod toolchain
  lives in `engine/Dockerfile.pod` (built by `.github/workflows/pod-base.yml`
  as a base image consumed by digest), so **adding a tool to the pod means
  editing `engine/Dockerfile.pod`**, not `engine/Dockerfile`, which installs
  only the per-commit halves on top of it.
- `web/` — the Solid/Vite PWA, and the product's GUI going forward. It is baked
  into the Rust binary at compile time (`engine/server/src/assets.rs` embeds
  `../../web/dist/`), so a `cargo build` without a fresh `pnpm build` ships a
  stale frontend — the single most common way to "fix" a UI bug and see
  nothing change. `src/vogt/gui/static/` is the older buildless GUI and stays
  until the Solid surfaces reach parity.
- `mobile/` — the Capacitor 8 Android shell. Its WebView loads a deployed PWA
  directly, so UI changes ship without an APK rebuild; only native plumbing
  (plugins, manifest, FCM) needs one.
- `docs/ENGINE.md` — the engine's single reference: what it owns, how to run it,
  its full wire contract (§5), the assistant (§6), agent tasks (§7). **There is
  no `docs/engine/` directory**; a path naming one is stale.
- `docs/USER_GUIDE.md` — how a person drives the product, both halves of it.
- `docs/ROADMAP.md` — everything designed anywhere in this documentation set
  and not built. **The engine has no separate backlog.**
- `docs/DEPLOYMENT.md` — the generic production deployment guide;
  `docs/CUSTOMISATION.md` — the supported extension points.
- There is no desktop client in this tree. A reference to `client/` anywhere
  is a reference to something that is not here.

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

Logging has one convention and it is short: every logger comes from
`vogt.observability.logger("<area>")`, never from `logging.getLogger`, so the
whole core lives under `vogt.*` and verbosity is a decision about Vogt rather
than about urllib3. Structured fields travel as `extra={"vogt": {...}}` and
render as `key=value` in text and as top-level keys in JSON. Every line
written while a request is being served carries that request's id
automatically; the access line itself is `adapters/http/access_log.py` and
nothing else should write one.

Three rules the code enforces so review does not have to:

- A **mutating** operation whose parameters lack a required `reason` fails at
  registry construction, not at runtime.
- Every declared write goes through `audited_write`, which lands the entity
  change, the audit row and the event row in one transaction.
- Callers name things the way people do — `WI-7`, a project slug, an actor's
  `identity_ref`. Resolution lives in `services/_resolve.py`, so a typo
  fails as "no work item WI-70" rather than as a foreign-key error.

## Ground rules

- Key design commitments — do not regress these (`docs/DESIGN.md` §2):
  observed-first visibility, explicit collector coverage, a real write
  plane, actors/people as core entities.
- Scope decisions that are easy to reintroduce by accident — don't
  (`docs/DESIGN.md` lists them with reasons):
  - **Nothing enforces.** Compliance, trust, and drift are values to be
    read; no operation may take them as a precondition.
  - **Nothing discovers.** Collection scope is the registered project
    list. No crawling roots, no candidate listings, no re-checking on a
    timer.
  - **No lockfiles, no resolved versions.** Dependency edges are path/git
    references between projects, nothing more.
  - **Observed-first is gated by promotion + suppression + exclusions**
    (`docs/DESIGN.md` §3.6) — never wire raw markers into ranked views.
  - AI-assisted drift detection is a **non-committed stretch goal**; no
    requirement or interface may assume it exists.
- **Transport parity.** Every feature must be reachable via CLI, REST, and
  MCP with tested parity; the GUI consumes the same HTTP adapter.
- Every write requires a principal and a reason (audit table).
- **Optional stays optional.** A missing GitHub token, MCP client, external
  MCP server, assistant provider, or session engine must not break a core
  workflow; it must surface as "not collected" or "not configured".
- mypy strict + ruff from the first commit; no unmigrated schema changes.

## Deployment rules

The public path is the core image from the root `Dockerfile`, run from
`deploy/vogt.compose.yml`. Things that are easy to get wrong:

- **Never hand-edit a deployed container.** A deployment is the compose file
  plus its environment; change those and redeploy.
- **Don't gate allocation values.** `${PORT:?}`-style required values on a
  port or an operator-owned path break every deploy that relied on the
  default. Defaults are *required* for allocation values and forbidden for
  anything encoding exposure or identity (a public URL, a non-loopback bind
  address) — read `docs/DEPLOYMENT.md` before "fixing" a default you find
  in a compose file.
- **Publishing ≠ deploying.** A tag publishes an image and moves nothing.
  An instance moves when its configuration pins a new digest.
- **The core and engine images are separate** and are published by separate
  jobs in `.github/workflows/build.yml`. Do not assume a change under
  `engine/` ships with a core release, or the reverse.

CI runs on self-hosted runners by policy (`.github/workflows/ci.yml` explains
the constraint at the top of the job). If CI is red with a job stuck queued,
it is a runner question, never a reason to change `runs-on`.
