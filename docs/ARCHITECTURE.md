# Vogt — Architecture

This document describes Vogt as it is built and shipped: what the pieces are,
how they fit together, and the rules that hold the shape in place.

Companion documents: [`SCHEMA.md`](SCHEMA.md) (tables and data topology),
[`DEPLOYMENT.md`](DEPLOYMENT.md) (running it), [`ENGINE.md`](ENGINE.md) (the
session engine's own reference), [`USER_GUIDE.md`](USER_GUIDE.md) (driving
it) and [`CONFIG.md`](CONFIG.md) (the generated settings reference).

---

## 1. Mission

Vogt is a self-hosted product development environment for small teams, solo
developers and the coding agents that work with them. It answers one question
for a single repository or across every project you register:

> What is the state of this work, how fresh is that answer, what should be
> done next, and is the project itself healthy?

The answer is the same whether a person asks in the PWA, a script asks over
REST, or an agent asks over MCP — and every answer says where it came from and
how old it is. Alongside the answer, Vogt gives those agents a place to do the
work: terminal sessions in a project's own tree, a scheduled task queue, and
an assistant that can be driven by voice.

Vogt is fully functional with no forge at all: projects, work items, backlog,
ranking, contracts, compliance, dependency references, drift and audit all
work over plain folders and local git. Forge integration (GitHub, and
Forgejo/Gitea hosts) is optional; an absent forge yields "not collected",
never a failure.

Vogt ships as one stack: the `vogt-stack` image (Python core, Rust engine and
Solid PWA behind one port) plus the `vogt-voice` sidecar, released as a pair
and run by one Compose file with voice on by default. The core image is a
build input to the stack image, not a product; the engine is not optional.
Development happens on `main`; releases are `v*` tags.

---

## 2. Principles

- **Observed-first.** Work that collectors find in your repositories and on
  your forge is visible immediately, before anyone types it in. Adoption
  upgrades its trust; suppression removes it from ranked views.
- **Vogt reports; it never enforces.** No operation takes contract
  compliance, trust state or drift status as a precondition.
- **Vogt never goes looking.** Collection scope is the list of registered
  projects. Nothing crawls the filesystem, nothing keeps a list of
  unregistered candidates, and nothing re-checks compliance on a timer.
- **Declared and observed stay separated.** What you assert lives in one
  store; what collectors found lives in another. A drift engine joins them
  and raises proposals. Collector failure can corrupt nothing.
- **Every answer carries provenance and freshness.** Entities carry a trust
  state; aggregate views carry the age of the sweeps behind them; a query
  whose scope exceeds swept scope says so.
- **Transport parity.** Every operation is defined once and reaches the CLI,
  REST, MCP and the PWA from that one definition. Parity is tested.
- **Every write is audited and explained.** Each declared write records its
  actor, its operation and a required free-text `reason`, and lands its event
  in the same transaction. Identity comes from authentication, never from a
  request field.
- **Nothing runs on its own.** Every session traces to a person or to a
  schedule a person created. An execution surface is not an enforcement
  surface.

---

## 3. System map

One published port, one container, one sidecar:

```
vogt-stack container
  vogt-engine  (Rust; the front door, :8910)
    ├── /                 the Solid PWA, embedded in the binary
    ├── /api/...          sessions, terminals, files, git, agent tasks, push,
    │                     assistant, history
    ├── /api/vogt/...     proxied to the core's /api, core token injected
    ├── /mcp              proxied to the core, caller's token forwarded
    ├── /api/install/...  proxied untouched; the core self-gates
    └── /healthz, /readyz
  vogt serve   (Python; the core, loopback only, :8000 inside the container)
    ├── /api/...          REST (FastAPI; OpenAPI at /openapi.json)
    ├── /mcp              MCP streamable HTTP
    ├── /health/live, /health/ready, /version
    └── collector scheduler (background sweeps)

vogt-voice container   (Rust; STT + TTS on the Compose network, reached by
                        the engine at http://voice:8000/v1, never published)
```

- **The core** owns the *work*: projects, work items, backlog, bugs, ranking,
  drift, contract, audit, the two SQLite stores, and the operation registry
  every surface is generated from. It listens on loopback inside the
  container and is never published; the entrypoint refuses to start if
  `VOGT_CORE_URL` names anything else.
- **The engine** owns the *doing*: PTY sessions, agent tasks, the assistant,
  workspace-scoped file and git APIs, push, session history — and the front
  door, the only listening process. The image also carries the `claude` and
  `codex` agent CLIs the sessions run.
- **The PWA** (`web/`) is compiled into the engine binary. It calls the
  engine's APIs directly and reaches the core through `/api/vogt`, holding
  one credential — the engine's bearer token — and never a core token.
- **The voice sidecar** (`voice/`) speaks the OpenAI-compatible audio
  contract the engine uses, with a baked-in Whisper model and Piper voice, so
  a fresh stack transcribes the microphone and speaks replies with no
  account. It is the `voice` service in `deploy/stack.compose.yml`, on by
  default through `COMPOSE_PROFILES=voice`; clearing the profile leaves it
  out.
- **The mobile shell** (`mobile/`) is a Capacitor Android wrapper that loads
  a deployed front door by URL, so server and PWA releases reach installed
  phones without an APK rebuild; only native plumbing (push, microphone)
  needs the shell rebuilt.

The engine calls the core over loopback for `/api/vogt` and `/mcp`, and
fetches the assistant's Vogt tools from the core's own MCP `tools/list`. The
core calls the engine only for sessions — the `session.*` operations
(`vogt.adapters.engine.client`) and the `session-outcomes` collector. The
engine calls the voice sidecar; the PWA and the mobile shell call only the
engine. The core token is a file both halves read (`VOGT_CORE_TOKEN_FILE`):
the engine presents it on `/api/vogt`, and the core adopts it at `init` as the
actor and scopes named by `VOGT_BOOTSTRAP_CORE_TOKEN_*`. That is the whole
first-boot bootstrap.

---

## 4. Layers

### 4.1 The core (`src/vogt/`)

```
registry/      the operation registry: every capability defined once
core/          entities, workflow, drift, ranking, trust, contract, auth
application/   use-cases and services; the ONLY layer adapters may call
adapters/
  cli/         argparse over the registry
  http/        FastAPI: REST + OpenAPI, health, install, the sweep scheduler
  mcp/         MCP over stdio and streamable HTTP, and the remote bridge
  forge/       the ForgeProvider seam: GitHub and Forgejo/Gitea, sync, write-back
  github/      GitHub client, consolidation, notifications, posture
  engine/      the client the session operations use
  git/         clone through an askpass helper (tokens never reach a URL)
collectors/    core (offline): git-local, source-markers, dep-refs,
               mirrored-source, contract-checker; session-outcomes when an
               engine is configured; forge-* when a forge is configured
storage/       SQLite x2 (declared.sqlite3, observed.sqlite3), migrations
```

Adapters call the application layer, the application layer calls core and
storage, and nothing below an adapter knows which transport asked. Collectors
write only the observed store; the application layer is the only writer of
the declared store, and `application/writes.py` is the one path an audited
write takes.

### 4.2 The engine (`engine/`)

`engine/` is its own Cargo workspace: `server/` builds the `vogt-engine`
binary and `contract/` holds the shared wire DTOs. One module per concern:

- **PTY sessions** (`pty.rs`, `sessions.rs`, `scrollback.rs`) — a
  server-owned PTY per session that survives every client disconnecting,
  with a ring-buffer scrollback.
- **WebSocket attach** (`ws.rs`) — the first frame authenticates, a snapshot
  is replayed, then live output streams.
- **SSE** (`events.rs`, `activity.rs`) — a server-wide event stream carrying
  each session's `idle` / `running` / `waiting-for-input` / `errored` state.
- **Files and git** (`files.rs`, `git.rs`) — APIs bounded to the workspace
  root.
- **Agent tasks** (`agent_tasks.rs`, `workflow_engine.rs`) — a durable
  scheduled-agent registry (`manual`, `interval`, UTC `daily`, event triggers
  from the core's own state, an explicit `api` fire) whose runs are real PTY
  sessions, optionally bound to a project or work item.
- **The assistant** (`assistant*.rs`, `vogt_tools.rs`) — a server-side
  tool-use loop over the sessions and a curated slice of Vogt, every effector
  behind an on-screen approval, with a durable interaction log.
- **The front door** (`vogt_core.rs`, `app.rs`, `auth.rs`, `assets.rs`) — the
  embedded PWA, the bearer gate, and the two proxied route families.
- **Push and history** (`push*.rs`, `history.rs`) — web push (VAPID), native
  push (FCM), and archived session history.

The engine is bootable alone: with no core configured it serves sessions,
stays ready, and refuses the Vogt routes with a named reason. The wire
contract, the assistant's threat model and the task scheduler are in
[`ENGINE.md`](ENGINE.md).

---

## 5. Operation registry and transport parity

Every capability of the core is one entry in
`src/vogt/registry/operations.py`: a name, a summary, a scope, a `mutating`
flag, a pydantic parameter model, a result model, a handler in the
application layer, an HTTP route and a CLI binding. The registry holds 89
operations. From that one definition the **CLI** (`vogt <verb>`) is
generated; the **REST** routes under `/api` are generated with their schemas
in the OpenAPI document at `/openapi.json`; and the **MCP** tool list is
generated, one tool per operation, named for it (`work.get` is `work_get`).
The **PWA** is a client: `web/src/vogtApi.ts` names each path it uses after
the operation that serves it, and `tests/test_pwa.py` resolves every entry
against the registry, so a view cannot grow an endpoint of its own.

There are no path parameters: `HttpRoute` is a method and a literal path, and
an operation's arguments are one model shared by all three transports, so an
identifier travels as a query or body field (`GET /api/projects/brief?project=…`).
Nothing uses `DELETE`: a revocation is `POST /api/suppressions/revoke`,
because it is an audited write that needs a reason.

**Parity is tested.** `tests/test_parity.py` drives each operation through
CLI, REST and MCP and asserts identical results and identical audit rows. Two
named exclusion lists live in `src/vogt/registry/registry.py`, each entry
carrying its reason, and the test fails if either names an operation that
does not exist: `LOCAL_ONLY` (`init`, `migrate`, `serve`, `backup`,
`restore`, `import`, `mcp.stdio` — each acts on the local process or data
directory, and none is mounted under `/api` or offered over MCP) and
`HTTP_ONLY`, which is empty.

Two routes sit beside the registry rather than in it: the unauthenticated
health probes, and the first-run install surface (`GET /api/install/status`,
`POST /api/install/bootstrap`) — the one unauthenticated write, which mints
the first `admin` token only while the token store holds no rows at all and
refuses with `install_closed` forever after.

**Identity is never an argument.** The principal is derived from
authentication — a token bound to an actor, or `local:<os-user>` on the
unauthenticated loopback path, which is granted `admin` because the caller
already has the data directory. `reason` is the only caller-supplied audit
field, and it is required.

**Scopes** are `read`, `work.write`, `project.write`, `writeback` and
`admin`. `admin` implies everything; `work.write` and `project.write` each
imply only `read`; `writeback` gates exactly one operation, `forge.writeback`,
which arms a project's upstream pushing. Scopes are instance-wide. Allow and
deny decisions are both recorded in `auth_decisions`, separately from the
write audit.

---

## 6. Declared vs observed

Two SQLite databases with different write disciplines:

- **`declared.sqlite3`** — what you assert: projects, work items, relations,
  labels, initiatives, actors, tokens, suppressions, contract adoptions and
  exemptions, coding sessions, drift proposals, workflow definitions, the
  `events` feed and the `audit` log. Every write goes through the
  application layer with an actor and a reason.
- **`observed.sqlite3`** — what collectors found: `sweeps` (coverage records)
  and `observations` (immutable evidence rows), the derived
  `latest_observations` and `latest_dep_refs` tables rebuilt at sweep
  completion, and the forge sync's bookkeeping (`sync_state`,
  `subject_seen`). Nothing writes it except collectors.

Cross-store joins happen in the application layer, never by SQL `ATTACH`, so
each store stays independently portable and restorable.

### 6.1 Collectors and coverage

A collector runs over registered projects and appends observations under a
sweep record naming its collector, scope, start, finish and outcome. The
sweep is what makes "absent" different from "not collected": "vanished
upstream" is asserted only when a completed sweep provably covered the
subject.

| Collector | Needs | Produces |
|---|---|---|
| `git-local` | nothing | `git.checkout`, `git.tag`, `git.branch` |
| `source-markers` | nothing | `marker` (TODO/FIXME occurrences, with a `promoted` flag) |
| `dep-refs` | nothing | `dep_ref` (internal-looking `path`/`git` references read from `Cargo.toml`, `package.json`, `pyproject.toml`) and one `dep_scan` receipt per project |
| `mirrored-source` | nothing | `mirrored_source` where a path member of one project is also a registered project, matched on declared package name |
| `contract-checker` | nothing | `contract.check` — on demand only; a plain `sweep` never runs it |
| `session-outcomes` | a configured engine | `session.outcome`, `agent_task.run` |
| `forge-issues`, `forge-prs` | a forge token | `forge.issue`, `forge.pull_request`, synced all-state and incrementally by watermark, plus a `forge.sync` receipt |
| `forge-checks`, `forge-releases`, `forge-labels`, `forge-posture`, `forge-notifications` | a forge token | `ci.check`, `release`, `forge.label`, `forge.posture`, `forge.notification` |

Every observation carries a deterministic `subject_key`
(`gh:{owner}/{repo}#{n}`, `contract:{slug}`, `session:{id}`, …) and a content
digest; an unchanged subject in a new sweep writes no new row, so growth is
proportional to change, not to polling. While `serve` runs,
`CollectorSchedule` (`adapters/http/scheduler.py`) runs the collectors in the
background every `sweep_interval_seconds` (default 900), one sweep at a time,
as a service principal; `sweep` is also callable on demand from every
transport. Each registered project carries exclusion patterns applied before
collection, so vendored and generated trees never become observations.

### 6.2 Observed-first without drowning

1. **Promotion by convention.** Only markers matching
   `marker_promotion_patterns` (default `TODO(vogt)`, `FIXME(vogt)`) enter
   backlog and bug views; every other marker is still observed and queryable.
2. **Suppression.** `suppress` is an audited write keyed on a `subject_key`
   or a pattern, with a required reason, that removes a subject from ranked
   and aggregated views permanently, surviving re-observation;
   `suppression.revoke` undoes it.
3. **Adoption.** `work.adopt` promotes an observed subject into a declared
   work item with `origin=adopted` and a `work_links` row; drift then keeps
   the pair honest.

### 6.3 Linked projects

A project carries a persisted `link_state`. On a **linked** project — one
attached to a forge repository by `forge.link`, `forge.publish` or
`project.import` — the work items *are* the mirrored forge issues. The
observed mirror is the truth for title, body, labels and open/closed; the
`work_overlay` table carries the local half (workflow state, priority,
effort, assignee, initiative, bound branches), keyed by the forge subject.
`services/upstream.py` is the one place the join happens, so every surface
returns each upstream issue exactly once. Writes go through synchronously:
the provider call runs before the declared transaction, so a failed upstream
write commits nothing locally. At link time a project's open native work
items migrate upstream and are retired by marker (`superseded_by`), never
deleted.

The forge write surface is append-only by construction: `comment`,
`create_issue`, `add_labels`, `set_state`, `create_repo`, and one bounded
`update_issue_body` that `initiative.publish` uses to re-render a managed
region of a tracking issue. The `ForgeProvider` interface has no name for
delete, history rewrite or force-push. Comments flow outbound only: a comment
authored on the forge stays an observation.

### 6.4 Drift

The drift engine (`core/drift.py`, `services/drift_service.py`) reads both
stores and writes only `drift_proposals`. A proposal never silently mutates
declared data: a person or an authorised agent accepts, rejects or leaves it
`contested` through `drift.resolve`. The kinds are `version_mismatch`,
`unresolved_dependency`, `broken_path_dependency`, `forge_state_mismatch`,
`vanished_upstream`, `ci_red_vs_healthy`, `update_automation_gap`,
`referenced_issue_state_mismatch`, `initiative_checkbox_drift` and
`initiative_tracking_close`.

The default policy is low-risk auto-accept: `version_mismatch` and
`forge_state_mismatch` in the closing direction may be accepted by an agent;
reopening finished work, and everything destructive or structural, is
human-gated with the reason named. Every proposal carries a self-contained
evidence snapshot taken at raise time and pins the observations it references
against retention pruning, so a proposal never outlives its evidence. A
proposal whose raising condition a later completed sweep no longer reproduces
is marked `superseded_at` rather than closed: it stays open and still needs a
person. Contract violations are not drift; they are a computed status on the
project (§8) with no declared counterpart to disagree with.

---

## 7. Trust and freshness

Trust is computed, never hand-set (`services/views.py: trust_for`):

| State | Meaning |
|---|---|
| `verified` | a sweep within the last `verify_horizon_hours` (default 24) confirmed the linked subject |
| `stale` | the last confirmation is older than the horizon |
| `unverified` | nothing has ever confirmed it — the honest answer for declared work linked to nothing |
| `disputed` | an open drift proposal names it |

Confirmation is "last seen", not "last changed": `subject_seen.last_confirmed_at`
is touched on every forge sync batch, including those where nothing moved, so
a stable open issue does not age to `stale` merely because its payload has
not changed. A trust state is `disputed`; a drift resolution a person chose
is `contested`.

Every aggregate answer — a project brief, the backlog, the bugs view, the
inbox — carries the freshness of the sweeps behind it and its coverage: which
collectors have run over this scope and when, and which have never run. An
uncollected source is a different answer from an empty one.

Provenance is a row, not a column. Every declared write lands in `audit`
(actor, operation, entity, reason, payload digest, transaction id, monotonic
revision) and in `events` (a monotonic `seq` that is the `/events` cursor) in
the same transaction. Sweep completions and CI transitions are published into
the same feed by the application layer on the collectors' behalf, so a client
never merges orderings across the two stores.

Ranking is deterministic and explainable: priority, staleness, blocking
fan-out, initiative weight, an open pull request or a recently committed
branch, and a trust penalty, each with a constant weight in
`core/ranking.py`. `why <item>` returns the per-input contributions.

---

## 8. Contract and compliance

A **contract** states what a compliant project looks like. It is sourced from
four settings read through one helper (`core/contract.py`) —
`contract_required_files`, `contract_required_dirs`, `contract_required_meta`
and `contract_version` — with these defaults, which are the contract this
repository holds itself to:

```
required_files:  [AGENTS.md, README.md, LICENSE]
required_dirs:   [docs/, design/, src/]
required_meta:   [name, lifecycle_state, owner]
```

A contract whose rules differ from the built-in default while still carrying
the default version gets a digest of its rules appended (`v1+3f9a2c`), so a
recorded status always names the contract it was evaluated against.

**Adopted, not imposed.** `contract.adopt` is an audited declaration per
project and `contract.decline` withdraws it; a project that has adopted
nothing reports `not_applicable`. `project.create` scaffolds a compliant
skeleton and adopts in the same act. `contract.inapplicable` records, with a
reason and an author, that a criterion cannot apply to this project; it is
still reported, not counted as failing.

**Evaluated on demand, and only on demand.** `contract.check` runs against
any path, registered or not, and returns every criterion evaluated with the
failures named. Against a registered project it records `compliance_status`
(`compliant`, `non_compliant`, `not_checked`) with `compliance_checked_at`,
and lands a `contract:{slug}` observation so the evidence outlives the
column. `compliance` reads the last result together with its age;
`not_checked` is a first-class answer. A criterion is asked of the
repository, not of the disk: in a git checkout a required file must be
*tracked*, and a required directory needs a tracked file in it. Each failing
criterion carries its remedy — the mechanical fix (`project.scaffold` writes
the missing skeleton files and never overwrites) or, where the content is a
decision such as a licence, an instruction addressed to a person.

**It reports and never gates.** No code path treats `compliance_status`,
trust or drift as a precondition. Registering a non-compliant folder succeeds
and reports its status; a declined contract refuses nothing.

---

## 9. Storage and migrations

Schema detail is in [`SCHEMA.md`](SCHEMA.md); operator procedure is in
[`DEPLOYMENT.md`](DEPLOYMENT.md).

```
$VOGT_DATA_DIR/                  (in the stack: /var/lib/vogt)
  declared.sqlite3               authoritative, mutable, audited, revisioned
  observed.sqlite3               append-only evidence + derived tables
  backups/<timestamp>/           both stores, engine state, manifest.json
```

Every connection opens with `journal_mode = WAL`, `foreign_keys = ON`, a busy
timeout and the configured `sqlite_synchronous`. There is no external
database, cache or queue; the core needs SQLite and `git`.

**Migrations** are forward-only SQL files under
`src/vogt/storage/sqlite/migrations/{declared,observed}/`, applied by
`storage/sqlite/migrator.py` under a `migration_lock` (a lock older than
fifteen minutes is stolen so a crashed process cannot wedge the instance),
with every applied migration's checksum verified on every run. `vogt init`
creates an instance and is idempotent; `vogt migrate` brings an existing one
forward and refuses a data directory that holds none. `serve` migrates both
stores before it accepts traffic, and `/health/ready` compares each store's
applied version with the highest migration the build ships, answering `503`
with the store, both numbers and the verb to run until they match.

**Retention** (`observations.prune`, `retention_days`, default 180) prunes
observation history under three rules in precedence order: the latest
observation per `subject_key` is kept indefinitely; any observation a drift
proposal references is kept; everything else older than the window goes.

**Backup and restore** (`services/lifecycle.py`) copy both stores and, when
`engine_state_dir` is configured, the engine's state directory as one act,
with a `manifest.json` recording what was taken from where. That directory
holds what the core's stores do not: archived session history (`history.db`),
push subscriptions and the VAPID keypair (`push.json`), the agent-task
registry and the assistant's interaction log (`assistant-log.db`). In the
shipped stack the core's data and the engine's home are two named volumes
(`vogt-data`, `engine-home`), so the data outlives a pod you reset.

---

## 10. MCP

MCP is the primary agent surface, generated from the same registry as
everything else. Two transports and one bridge:

- **stdio** (`vogt-mcp`, or `vogt mcp stdio`) — an agent spawns the process
  and speaks newline-delimited JSON-RPC over stdin/stdout against the local
  data directory. No server is required; the principal is `local:<os-user>`.
  stdout is the framing channel and every diagnostic goes to stderr.
- **streamable HTTP** at `/mcp`, on the same port as REST and the PWA. In the
  stack the engine proxies it to the core with the caller's `Authorization`
  forwarded untouched, because the credential on an MCP request is already a
  core token bound to an actor. Responses stream over SSE.
- **`vogt-mcp-remote`** — a stdio bridge for agent products that can only
  spawn a local process. Configured by `VOGT_URL` and `VOGT_TOKEN_FILE` (or
  `VOGT_HTTP_TOKEN`), it forwards everything, hardcodes no tools, learns the
  remote's tool list as it passes, and reports version skew as one stderr
  warning that never blocks.

Both server transports share one dispatcher (`adapters/mcp/surface.py`), and
unsupported protocol versions are refused with the supported list named.

**Tokens.** A core token is minted by `token.issue` (`vogt token issue`),
bound to an actor and a scope set, and revocable by `token.revoke`. Writes
are double-gated: the server must run with writes enabled and the principal
must hold the scope, checked at both `tools/list` and `tools/call`. Ungranted
tools are absent from the list rather than present and refusing, so an
agent's tool list is exactly what it can do.

A session started from Vogt for a project or work item registers Vogt's own
MCP server into the agent automatically, carrying a per-session token bound
to that session's actor, so the agent's writes are attributed to it. The
assistant fetches its Vogt tools from the same `tools/list` at the start of
every turn; a curated capability matrix decides which are readable without
confirmation, which need an on-screen approval, and which are never offered.
No default endpoint ships anywhere: a client is told where its instance is.

---

## 11. The demo artefact

Two public sites run the current build against seeded, read-only data, with
no sign-in and nothing persisted:

- **vogt-demo.thedancingdeveloper.com** — the desktop app.
- **vogt-mobile-demo.thedancingdeveloper.com** — the same app in a phone
  frame, as the Android shell wraps it.

Both are the `ghcr.io/thedancingdeveloper-org/vogt-demo` image, the
`demo-runtime` target of `engine/Dockerfile`. The demo is the shipped Solid
PWA selected at runtime, not a fork or a screenshot site. Normal and demo
packaging share one PWA compilation: `demo-build.json` records the source ref
and SHA and the SHA-256 of every built asset; demo augmentation verifies those
hashes and only then adds `demo-manifest.json`, a static GUI-stream
illustration and `mobile-demo.html`. A stale hash fails the build.

Before `App` evaluates, the boot module (`web/src/runtimeTransport.ts`)
validates the manifest against `demo-build.json` and installs a browser-only
transport (`web/src/demo/`). Its deterministic state lives in
`sessionStorage`; HTTP, event-stream and terminal-socket requests are answered
in the browser. Terminal sessions replay the real attach ordering but accept
only a canned input list and execute nothing. Writes last only in that tab;
**Reset demo** restores the canonical tour.

The only server in the image is a small read-only static-file origin
(`engine/deploy/demo-server.mjs`) with no Python core, session engine, PTY,
proxy, workspace mount or credential, and it refuses `/api` and `/mcp` paths.
The mobile site is a second Compose project from the same signed digest with
`deploy/mobile-demo.overlay.yml`, which changes only the root document
(`DEMO_ROOT_DOCUMENT`); `/index.html` remains the real PWA loaded inside the
phone frame, so the two entry points cannot drift apart. Native push and
microphone behaviour are the only parts of the mobile shell the browser frame
cannot demonstrate.
