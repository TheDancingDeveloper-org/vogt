# Vogt — Design Outline

Status: **v0.3 (revision r4), built** · design 2026-08-12, reconciled
against the delivered v1 on 2026-08-12. Sections that describe something
the build decided differently are marked *as built* in place; the
requirement-by-requirement verification is `REQUIREMENTS.md` §5.
Scope: standalone product. Cadastre is prior art and a lessons source, not a
dependency — see §11.

Companion documents: [`REQUIREMENTS.md`](REQUIREMENTS.md) (numbered FR/NFR
baseline, with the r2 change summary), [`SCHEMA.md`](SCHEMA.md),
[`DEPLOYMENT.md`](DEPLOYMENT.md), [`ROADMAP.md`](ROADMAP.md).

---

## 1. Mission

One tool that answers, for a single repo or across every project:

> What is the state of this work, how fresh is that answer, what should be
> done next, and is the project itself healthy?

— answerable identically by a human in a GUI, a script over REST, or an AI
agent over MCP.

### 1.1 Forge-optional core (hard requirement)

The product must be **fully functional with no GitHub — no forge at all**.
Plain folders and local git repositories are first-class: projects,
work items, backlog, ranking, contracts, compliance, dependency references,
drift, and audit all work against the filesystem and local git alone.

Forge-optional is a statement about *dependency*, not about *sequencing*.
Read-only GitHub collectors ship at M2 alongside the local ones (r2
decision, FR-O5a), because the estate this tool governs keeps most of its
real work — issues, PRs, CI results — on GitHub, and an MVP populated only
by source markers would be a demo rather than a daily driver. What stays
optional is the *dependency*: nothing in the core requires the adapter, the
whole test suite runs forge-less, and an absent forge yields "not
collected", never failure.

Consequences:

- Core collectors (`git-local`, `source-markers`, `dep-refs`,
  `contract-checker`) have zero network dependencies and run only over
  explicitly registered projects (§5.1).
- `POST /projects` scaffolds and registers a compliant local folder/repo;
  creating the GitHub repo is an optional step that only runs when the
  forge adapter is configured.
- CI status is modeled as generic `RevisionCheck`-style observations; the
  GitHub Actions collector is one producer.
- Write-back, historical backfill, and forge-derived drift remain at M5.

### 1.2 Non-goals (v1)

- Multi-forge support (GitHub + GitHub Actions are the only *optional*
  forge integration in v1).
- Multi-node / hosted SaaS (single-node self-hosted only).
- Time tracking, sprint ceremonies, burndown charts.
- Being an agent runner. Vogt tells agents *what* and *why*; it does
  not execute them.
- Enforcing anything. Vogt reports; the human or agent acts (§5).

### 1.3 Alternatives considered

Recorded because "why not use an existing tracker" is the first question
this proposal must answer.

| Alternative | Why not |
|---|---|
| **GitHub Issues/Projects + an MCP shim** | Covers the write plane well and would delete a third of this design. It cannot express the parts that motivate the product: coverage-modelled observation ("has anything even looked at this repo lately?"), declared-vs-observed separation with typed drift, cross-project dependency references, contract compliance, or a ranked global view over repos that are *not* on GitHub. It also makes every answer network-bound and rate-limited, and puts the estate's index inside a service the owner does not run. |
| **Jira / Linear / self-hosted alternatives (Plane, Taiga)** | Same write plane, none of the observation layer, and all of them assume work is *entered*. The estate's problem is the opposite: work already exists in the filesystem and on the forge and nobody has an index of it. Bending one of these into an observation platform is more work than the observation platform. |
| **A pile of scripts + a Markdown index** | The honest baseline, and what exists today. It fails on freshness (no answer to "when was this last true"), on provenance, and on being usable by agents without bespoke parsing per script. |
| **Extend cadastre** | Rejected for domain reasons, see §11. |

What survives the comparison is the observation, coverage, trust, drift and
cross-project layer — the write plane is table stakes that has to exist for
those to attach to, and exists here mainly because NFR-PO1 forbids
outsourcing it to a forge.

---

## 2. Design principles (learnings applied)

Carried from cadastre (proven there):

| Principle | What it means here |
|---|---|
| Declared/observed split | Authoritative store (what we assert) is separate from the observation store (what collectors found). A drift engine joins them. Collector failure can corrupt nothing. |
| Trust + provenance | Every entity/answer carries `verified / stale / unverified / disputed` plus source and timestamp. |
| Transport parity | `core → application → adapters(cli, http, mcp, gui-api)`. Parity is *tested*, not intended. |
| Audit as a table | Every write: principal, operation, reason, transaction id, monotonic revision. `reason` is required — essential for agent-driven writes. |
| Explainable ranking | The global backlog ranking can always answer `why <item>` with per-contribution scores. |
| Checks return evidence | A contract check returns its result **with the failing rules named**, never a bare boolean. |
| Zero-dep self-hosting | SQLite + forward-only migrations + migration lock. Backup/export/import are v1 commands, not v2. |

Inverted from cadastre (its anti-learnings for this domain):

| Cadastre posture | Vogt posture | Why |
|---|---|---|
| Declaration-first: undeclared work is invisible | **Observed-first**: collected work is visible immediately; adoption upgrades trust | Cadastre's import blocked with 97% of items lacking declared repos; a tracker must never show an empty view of a busy estate |
| Absence in observations can read as drift | **Coverage is modeled explicitly**: "collector X last swept scope Y at T". Absence is only meaningful inside swept scope | Most cadastre "missing" drift was collector-coverage artifact |
| Read-only map, no write-back | **Owns the write plane**, incl. opt-in GitHub write-back | It's a tracker; creating/closing/moving work is the product |
| No people model | Actors (humans *and* agents) are core entities from day one | Retrofitting assignment/attribution is miserable |
| Hand-rolled ASGI | FastAPI | Free OpenAPI, validation, auth middleware; big API surface planned |
| Docs state defaults | Config schema is single source of truth; docs/examples generated from it | The :18081/:18092 stale-default incident |

### 2.1 Reporting, not enforcing; and never going looking (r2/r3)

Vogt computes and publishes facts; it does not stand in anyone's way. No
operation in the system takes contract compliance, trust state, or drift
status as a precondition. This was a deliberate reversal: an earlier draft
made the project contract a *gate* that blocked creation in controlled
locations and admitted projects to a governed root only by passing it.

That design bought little and cost a lot. With a single user and agents
that can call `mkdir`, the gate was advisory in practice — the draft
already conceded that out-of-band creation would be "flagged, not
physically blocked". Meanwhile it forced a second estate root, a
system-performed relocation of projects, and the breakage that comes with
moving a project that other projects reference by path.

The information the gate produced is worth keeping; the barrier is not. So
the contract is evaluated on request and reported as a per-project status
with named failing criteria (§5), and the response to a violation is a
human or agent reading the status and deciding.

**And the system never goes looking (r3).** Collection operates over the
projects the user registered — nothing crawls the filesystem for
repositories, nothing maintains a list of unregistered candidates, and
nothing re-checks compliance on a timer. Discovery and continuous
re-validation are where this class of tool accumulates fiddly machinery:
deciding what counts as a project, tuning exclusions until the noise
stops, and re-answering a question nobody asked. The cheaper posture is
that Vogt answers when asked and stamps the answer with its age.

The intended eventual replacement for the heavier version of that
machinery is an AI integration reading the observation store and
recommending — recorded as a **non-committed stretch goal**
(`REQUIREMENTS.md` §3), designed for by nothing in v1.

---

## 3. Domain model (draft)

### 3.1 Core entities

- **Project** — the unit of the per-repo view, and **exactly one
  explicitly registered repository or folder**. Fields: name, root path,
  repo URL, lifecycle state (`incubating / active / maintenance /
  archived`), current version (from tags/releases), contract version,
  compliance status, health rollup.
  - *Granularity rule (r2, FR-P5)*: members of a multi-package workspace
    are **not** separate projects — a Cargo workspace with seven crates is
    one project. A standalone repository that mirrors a workspace member
    *is* its own project, and the relationship between the two is reported
    as `mirrored_source` (§3.5), never reconciled. This keeps "project"
    meaning one thing everywhere: one contract check, one compliance
    status, one CI story, one place work attaches.
- **WorkItem** — the unit of work. Kind (`feature / bug / chore /
  question`), title, body, state (workflow-managed, §3.3), priority,
  effort, project, origin (`created / adopted / observed`), trust state,
  assignee (Actor), labels, typed relations, links.
  - **Relations** are typed edges between work items, cross-project:
    `depends_on` (this cannot complete until that does — feeds ranking's
    blocking fan-out and blocks completion), `relates_to`, `duplicate_of`,
    `parent_of` (sub-items). Aligned with GitHub issue semantics so
    observed forge relations map losslessly.
  - **Labels** are free-form tags (name + optional color), shared per
    instance, filterable in every view, GitHub-label aligned.
- **Initiative** — cross-project grouping (epic), with weight for ranking.
- **Actor** — human or agent. Identity, kind, credentials binding. Every
  audit row and assignment references an Actor.
- **Observation** — an immutable record from a collector: forge issue/PR
  state, CI run result, source marker, repo checkout state, release/tag,
  dependency reference, contract check result. Carries source, scope,
  collected-at.
- **CollectorSweep** — coverage record: collector, scope, started/finished,
  outcome. The thing that makes "absent" ≠ "not collected".
- **Contract** — the declarative statement of what a compliant project
  looks like (§5). Configuration, not a per-project negotiation.
- **Suppression** — an audited decision that a given observed subject (or
  pattern of subjects) does not belong in ranked views (§3.6).
- **Comment / Event** — collaboration and notification primitives (§4.2).
- **AuditRecord** — principal (Actor), operation, reason, transaction id,
  revision, timestamp.

### 3.2 Declared vs observed in this domain

- Declared store: Projects, WorkItems (created/adopted), Initiatives,
  Actors, Suppressions, workflow config, events, audit.
- Observed store: Observations + CollectorSweeps, append-only.
- **Drift** examples: declared WorkItem links a GitHub issue that is closed
  upstream but open here; project version declared 1.4 but latest tag is
  1.5; CI red on default branch while project state says healthy; a
  dependency reference points at a path no registered project owns.
- Drift produces **proposals**, not silent writes: "issue #42 closed
  upstream → close linked item WI-118?" A human or authorized agent accepts,
  rejects, or leaves contested. Default policy: **low-risk auto-accept** —
  state-sync kinds (`forge_state_mismatch`, `version_mismatch`) may be
  auto-accepted by agents; destructive or structural kinds are always
  human-gated. Per-project/per-kind overrides.
- Every proposal carries a **self-contained evidence snapshot** taken at
  raise time, and pins the observations it references against retention
  pruning (FR-R5). A proposal must never outlive its evidence.
- *r2*: contract violations are **not** drift. They are a computed status
  on the project (§5) — there is no declared counterpart to disagree with
  and no declared data to change, so the proposal lifecycle would be
  ceremony around a fact.

### 3.3 Workflow

- v1: one configurable state machine per work-item kind (default:
  `open → in_progress → review → done`, plus `blocked`, `wont_do`).
  Transitions are validated; invalid transitions are rejected with the rule
  named. No boards/sprints in v1 — the ranked backlog *is* the board.

### 3.4 Ranking

- Deterministic global scoring: priority, staleness, blocking fan-out,
  initiative weight, CI-red boost, trust penalty for unverified items.
- Constant, documented weights in v1; `why <id>` returns the per-input
  contributions. No ML, no hidden state.
- Ranked views exclude suppressed and unpromoted subjects (§3.6). The
  ranking is only as trustworthy as its input set.

### 3.5 Dependency references (r2)

Vogt records **which projects reference which**, and stops there. Edges are
identified by filesystem path or repository URL; no lockfile is parsed and
no package version is resolved.

Motivating case, and what the cheap model does with it: **rustnzb** is a
Cargo workspace whose seven `nzb-*` crates exist in three forms at once —
vendored path members (via `[workspace.dependencies]` + `[patch.crates-io]`),
standalone GitHub repos, and published crates.io releases (see
`~/Working/docs/RUSTNZB-DEPENDENCY-MIGRATION-PLAN.md`). Reference-level
tracking shows that rustnzb path-references `nzb-core` *and* that a
standalone `nzb-core` project exists — the three-forms situation is
**surfaced** as `mirrored_source`. It does not say whether the copies have
diverged or at which version; answering that needs resolved versions, and
resolved versions need the lockfile subsystem this revision deletes.

That is the trade r2 makes deliberately: the graph tells you where the risk
lives, a `git diff` tells you whether it has bitten.

Model:

- **`dep-refs` is a core (offline) collector**. It reads manifests only —
  `Cargo.toml`, `package.json`, `pyproject.toml` — and extracts only
  **internal-looking references**: `path = …`, `git = …`, `file:`,
  `link:`, `workspace:`, and direct git URLs. Registry dependencies
  (`serde`, `react`) are ignored entirely; Vogt has no reason to hold an
  opinion about them. This avoids lockfile format churn, transitive
  resolution, feature/platform conditionals, and per-ecosystem resolver
  semantics — the whole expensive half of dependency tooling.
- **Edges also may be declared.** `project link A depends_on B` records an
  edge no manifest expresses (a service calling another, a doc pipeline
  consuming a schema). Reference kind is `path | git | declared`.
- **Resolution is by path or repo URL** to a registered project. Internal
  edges are exactly the ones expressed as paths and git URLs, which is why
  package identity is not needed. Unresolved internal-looking references
  are retained with their raw target and reported as
  `unresolved_dependency`.
- **`mirrored_source`** is reported where the same source appears both as a
  path member of one project and as a separate registered project. Reported
  as an observation; contents are never compared.
- Update-automation posture (version-updates config / vulnerability alerts
  / automated security fixes — three independent toggles, never one
  boolean) is **forge posture**, not dependency data. It moves to the forge
  module at M5.
- Roadmap fit: reference extraction and the cross-project graph land with
  M2; `unresolved_dependency` reporting with M3.

### 3.6 Observed-first without drowning (r2/r3)

Observed-first is the product's best idea and its biggest hazard. The real
estate (`~/Working/Active`) currently contains **151 git repositories,
5,226 TODO/FIXME occurrences in source, 432 in Markdown**, and about **28
leaked `pytest` temp directories**. Registering even a dozen real projects
therefore puts hundreds to thousands of markers in scope. An unfiltered
observed-first backlog is several thousand items deep on day one, mostly
worthless, and the ranked global view — the headline feature — is
destroyed by its own input.

Three mechanisms, all required before observed-first is switched on:

1. **Promotion by convention** (FR-W11). Only markers matching a configured
   pattern — default `TODO(vogt):` / `FIXME(vogt):` — enter backlog and bug
   views. Every other marker is still observed, still queryable, still
   counted; it just does not claim to be work. This inverts the default
   from "everything is work until dismissed" to "work is what someone
   marked as work", which is the only version that scales.
2. **Suppression** (FR-W10). An audited write, keyed on `subject_key` or a
   pattern, with a required reason, that removes a subject from ranked and
   aggregated views permanently — surviving re-observation, which a
   dismissal that lived only in the observation store could not.
   Suppression is a first-class operation, not `adopt` + `wont_do`: the
   latter fabricates a declared work item for every piece of noise.
3. **Per-project exclusions** (FR-G12, rescoped r3). Glob patterns on each
   registered project, applied before collection, so `.venv/`,
   `node_modules/`, `target/`, `dist/` never become observations at all.
   Cheapest of the three, and with r3 the *only* one that has to handle
   volume: since nothing crawls the filesystem, the 28 stray `pytest` temp
   directories are simply never registered and therefore never seen. The
   exclusions exist for noise *inside* a project the user does want.

Explicit registration does most of this work. It is worth noticing that
the discovery mechanism r3 deferred was also the thing that would have
manufactured most of the noise the other two mechanisms exist to clean up.

---

## 4. Architecture

```
core/          entities, workflow engine, drift, ranking, trust, contracts
application/   use-cases; the ONLY layer adapters may call
adapters/
  cli/         thin argparse/typer over application
  http/        FastAPI: REST + generated OpenAPI; serves GUI API
  mcp/         MCP server (stdio + streamable HTTP); tool set mirrors REST
  github/      OPTIONAL forge adapter plugin: collectors (read, from M2) +
               write-back (opt-in write, M5); absent = fully functional
collectors/    plugin registry — core (no network): git-local,
               source-markers, contract-checker, dep-refs; optional
               (network): gh-issues, gh-prs, gh-actions, gh-releases
gui/           static ES modules consuming the HTTP adapter only (M6 chose
               buildless over React — see `ROADMAP.md` M6; a wheel that
               needs npm to build is the cost that decided it)
storage/       SQLite x2 (declared.sqlite3, observed.sqlite3), migrations
```

- **Transport parity tests**: for each use-case, a matrix test drives it via
  CLI, REST, and MCP and asserts identical results and identical audit rows.
  Two explicit exclusion lists exist and are checked for staleness in both
  directions: `HTTP_ONLY` and `LOCAL_ONLY` (`init`, `serve`, `backup`,
  `restore` — operations that act on the local process or data directory
  and have no meaningful remote semantics).
- **Write-back** lives only in `adapters/github/` behind a per-project
  `write_back: none | comment_only | full` policy. Every write-back action
  is itself audited and produces an Observation on the next sweep (closing
  the loop: we observe our own writes like anyone else's).
- **Comments flow outbound only** (FR-B5). A comment authored in Vogt
  posts upstream; a comment authored on GitHub stays an observation shown
  against the linked item and is never copied into `comments`. This keeps
  `comments` unambiguously ours — every row has a Vogt Actor and an audit
  trail — and avoids needing forge-author identity mapping and
  loop suppression to tell our own echo from someone else's remark.
- **GitHub onboarding is non-destructive consolidation.** Enabling the
  adapter for a repo performs a read-only backfill (issues, PRs, labels,
  releases, CI history) into observations; **no GitHub mutation ever occurs
  during onboarding**. Existing GitHub objects are authoritative for
  themselves — declared items attach via `adopt`, and disagreement surfaces
  as drift proposals, not corrections pushed upstream. Even with write-back
  at `full`, operations are **additive/forward only** (create, comment,
  label, close/reopen per policy) — never deletion, history rewrite, or
  force operations, ever.

### 4.1 MCP by default — server/client architecture

MCP is the **primary surface**: agents are the expected first users, and
REST/CLI/GUI are peers over the same operations.

- **One transport-neutral operation registry.** Every operation is defined
  once — name, scope, `mutating` flag, argument schema, HTTP route — and
  the MCP tool list, FastAPI routes, CLI commands, *and the stdio bridge*
  are all generated from that registry. Cadastre's biggest MCP duplication
  was the same 20 tool signatures hand-mirrored across its server, its
  remote bridge, and its registry; here the bridge is generated, never
  hand-written. Parity exclusions are named lists that fail when stale,
  never glob matches.
- **Identity is never a tool argument.** The principal is derived from
  authentication (token, mTLS, trusted proxy, or `local:<os-user>`) —
  a caller-supplied principal would let any token forge provenance.
  `reason` *is* a tool argument (free text), is required, must be
  non-empty, and is the only caller-supplied audit field.
- **Writes are double-gated**: the server must be started with writes
  enabled AND the principal must hold the scope; checked at both
  `tools/list` and `tools/call` time. Ungranted tools are invisible, not
  erroring — an agent's tool list is exactly what it can do. Note the
  honest limitation: scopes are instance-wide in v1, so an agent with
  `work.write` can write to every project (deferred, `REQUIREMENTS.md` §3),
  and in the loopback topology there is no authentication at all.
- **Both allow and deny decisions are audited.**
- **Transports**: stdio (local, same data-dir, no server required) and
  streamable HTTP at `/mcp` on the same port as REST/GUI/health (see
  `DEPLOYMENT.md` §1). A `vogt-mcp-remote` stdio bridge serves agent
  products that can only spawn local processes; it discovers the remote's
  actual tool list at startup and reports version skew as a single stderr
  warning — never to stdout (MCP framing channel), never fatal.
- **Protocol versions**: unsupported MCP protocol versions are refused with
  the supported list named. Nothing outside a real MCP client (health
  checks especially) ever pins a protocol version.
- **No default endpoint** ships anywhere — see `DEPLOYMENT.md` §4.

### 4.2 API surface sketch (v1)

- `projects/` CRUD + `GET /projects/{id}/brief` (per-repo view)
- `work/` CRUD, `POST /work/{id}/transition`, `POST /work/{id}/adopt`
- `GET /backlog?scope=global|project` + `GET /work/{id}/why`
- `GET /bugs?scope=global` (the cross-project open-bugs view)
- `POST /contract/check` (evaluate any folder/repo, registered or not),
  `GET /projects/{id}/compliance` (last recorded result + its age)
- `POST /suppressions`, `GET /suppressions`, `DELETE /suppressions/{id}`
- `GET /events?after=<cursor>` — the notification surface. Backed by a
  single append-only `events` table in the declared store with a monotonic
  `seq`; declared writes insert their event in the same transaction, and
  sweep completions and CI transitions are published into the same table by
  the application layer at sweep completion. `seq` *is* the cursor —
  clients never merge orderings across the two stores. No email/push in v1.
- `GET /projects/{id}/deps` (references out), `GET /deps?project=<id>`
  (reverse lookup: which projects reference this one)
- `GET /drift`, `POST /drift/{id}/accept|reject|contest`
- `GET /observations`, `GET /coverage`
- `GET /audit`
- `actors/`, `auth` (token issue/rotate; scopes: `read`, `work.write`,
  `project.write`, `admin`, `writeback`)
- MCP tools: one per registry operation, named for the operation
  (`project.brief`, `backlog`, `why`, `work.create`, `work.transition`,
  `work.adopt`, `suppress`, `contract.check`, `drift.list`, `deps`, …) —
  the same names and semantics as the CLI verbs, because both are generated
  from the same registry. *As built*: the sketch above named `next` and
  `annotate`; neither survived. `backlog` already answers "what next" in
  ranked order, and a second operation returning its first row would have
  been a view with no view behind it; `annotate` was `work.comment` under
  another name.

---

## 5. The project contract and compliance status

A **Contract** states what a compliant project looks like. It is
configuration, carrying a version identifier so a status can name which
contract it was evaluated against. Default v1 contract:

```
required_files:  [AGENTS.md, README.md, LICENSE]
required_dirs:   [docs/, design/, src/]
required_meta:   [name, lifecycle_state, owner]
```

**It is evaluated on demand, and only on demand** (r3), and it blocks
nothing (§2.1):

- `contract check <path>` runs against any folder or repo, registered or
  not, returning every rule evaluated and every criterion that failed.
- When the target is a registered project, the result is recorded as that
  project's compliance status (`compliant / non_compliant / not_checked`)
  with failing criteria and a checked-at timestamp.
- That status is surfaced on the project brief and in the global view
  **always paired with its age**, exactly like every other value the
  system reports. `not_checked` is a first-class, unembarrassing answer;
  a three-week-old `compliant` is honest in a way a silently-refreshed one
  is not.

Nothing re-checks on a timer. If you want a current answer, ask for one —
which is a one-line CLI call, an MCP tool an agent can invoke as part of
its own workflow, and a thing that costs nothing when nobody cares.

`project create` scaffolds a compliant skeleton, because starting compliant
is easier than becoming compliant. Registering an existing non-compliant
folder succeeds and reports its status — registration is never refused on
contract grounds.

### 5.1 Registration is the scope (r3)

**Collection scope is the list of registered projects.** `project
register <path>` (or `project create`, which scaffolds first) is how
something enters Vogt's world. There are no sweep roots, no crawling, and
no candidate listing.

Each registered project carries exclusion patterns (FR-G12) applied to
collection within it — `.venv/`, `node_modules/`, `target/`, `dist/` — so
that vendored and generated content never becomes observations.

Moving a project is a human operation: move it, then update its
registration path. Nothing is relocated by the system and nothing breaks
behind your back.

*Withdrawn history, so the absences read as decisions:*

- *r2*: an earlier draft made controlled locations a governed estate root
  (working name `~/WorkingStack`) that projects entered one at a time by
  passing a gate, with `project migrate` performing the relocation. Both
  withdrawn — without enforcement there is nothing to migrate *into*, and
  a system-performed move breaks every absolute reference to a project
  (path dependencies between workspace members, IDE workspaces, agent and
  MCP configs, CI checkout paths) for no gain.
- *r3*: sweep roots and the registration-candidate listing that replaced
  them are also gone. The cost of explicit registration is one command per
  project, once. The cost of discovery is a permanent classification
  problem — what is a project, what is vendored, what is scratch — paid
  every sweep, plus the noise it generates downstream (§3.6). For an
  estate whose working root currently holds 28 stray `pytest` temp
  directories, that trade is not close.

---

## 6. Trust, freshness, coverage

- Trust states on declared entities: `verified` (recently confirmed against
  observation), `stale` (confirmation aged out), `unverified` (never
  confirmed), `disputed` (declaration and observation disagree,
  unresolved). Note the deliberate split of vocabulary: a *trust state* is
  computed and is `disputed`; a *drift resolution* is chosen by a human or
  agent and may be `contested`. One word never means both things.
- Every API answer that aggregates (brief, backlog, bugs) carries the oldest
  relevant sweep timestamp: "global bug view; GitHub swept 6 min ago,
  contract check 2 h ago."
- Coverage rule: a query whose scope exceeds swept scope must say so, never
  silently return partial results as if complete.

---

## 7. Storage & operations

Schema and data topology: [`SCHEMA.md`](SCHEMA.md).
Deployment and network topologies: [`DEPLOYMENT.md`](DEPLOYMENT.md).

- Two SQLite files; declared store has monotonic revision + audit + events +
  migrations tables; observed store is append-only with a retention policy
  that never prunes the latest observation per subject, nor any observation
  a drift proposal references.
- `vogt init | status | backup | restore | export | import`. *As built*:
  there is no separate `migrate` verb — `init` is idempotent and brings an
  existing instance forward, and `serve` migrates before it reports ready.
  FR-L1 names `migrate` explicitly, so this is a gap recorded in
  `REQUIREMENTS.md` §5, not a decision.
- Config: pydantic settings schema is the single source of truth; example
  config and docs are generated from it in CI (drift between docs and
  defaults fails the build).
- Packaging: `uv tool install`, or an OCI image on GHCR with SBOM +
  keyless signature. GUI ships in the image.
- **Deployment target (r4)**: a Docker Compose stack on Node B, deployed by
  Komodo from the `indexarr/ops` GitOps repository, tailnet-bound, with TLS
  terminated in-process from the host's Tailscale certificate. The image is
  digest-pinned in ops; publishing (a tag) and deploying (a digest bump)
  are separate acts. Full topology, hardening, and the Node B failure modes
  worth knowing in advance: [`DEPLOYMENT.md`](DEPLOYMENT.md) §2.2–§2.3, §6.

---

## 8. Engineering standards (day one, non-negotiable)

- mypy `--strict` on `src/` and `tests/`; ruff.
- Transport-parity test matrix with named exclusion lists (§4).
- Coverage gate in CI (start ≥80%).
- Forward-only migrations, tested against fixture databases.
- A seeded benchmark fixture at the NFR-S1 envelope from M2, with the
  interactive-query target asserted in CI — the two-store split means every
  aggregate query is an application-layer join, and that cost needs a
  tripwire rather than a discovery at M6.
- Every feature lands with CLI + REST + MCP + audit coverage or it doesn't
  land.
- **Own dependencies practice what we preach**: `uv.lock` committed,
  Renovate (or Dependabot) with weekly cadence configured from the first
  commit — with version updates, vulnerability alerts, and security fixes
  each explicitly enabled (they are independent toggles) — and a CI check
  that fails on manifest/lockfile mismatch.

### 8.1 CI/CD shape (GitHub Actions)

- **Docs-only changes do not trigger the full pipeline.** Path filtering
  splits the workflows:
  - `docs.yml` — runs on changes limited to `docs/**`, `design/**`,
    `**/*.md`: markdown lint, link check, config-docs-drift check only.
  - `ci.yml` — runs on `src/**`, `tests/**`, `pyproject.toml`, lockfile,
    migration files: lint, mypy strict, tests, coverage gate.
  - `release.yml` — tag-triggered only: image build, SBOM, keyless cosign
    signing, publish to GHCR. It publishes; it does not deploy.
  - A mixed commit (code + docs) runs the full pipeline; the docs job is a
    subset, never a bypass — branch protection requires `ci.yml` only when
    code paths changed, handled with a single gate job that succeeds
    trivially on docs-only diffs rather than by marking checks optional.
- Release workflows stay tag-driven so doc pushes to main can never publish
  an image.
- **Every job names a self-hosted runner** (`runs-on: [self-hosted, node-b,
  linux, x64, …]`); GitHub-hosted runners are prohibited estate-wide and
  the repository joins the `public-node-b` runner group before its first
  workflow exists. The self-hosted image preinstalls no language runtime,
  so `setup-uv` is explicit, and only the two Docker-in-Docker workers
  advertise `docker`/`publish`.

---

## 9. Open questions

Resolved 2026-08-12 (r1):

- **Licence: MIT** — matching cadastre. Developed in a private repository
  under `TheDancingDeveloper-org` first; public at a milestone of the
  owner's choosing (NFR-O1).
- **Name: Vogt** (final) — the German reeve/bailiff who oversaw an estate
  (Vogtei), enforced its rules, and answered for its work; sits alongside
  cadastre (the land register). Package/CLI `vogt`, env prefix `VOGT_*`.
- Drift autonomy → low-risk auto-accept defaults (§3.2).
- Notifications → audit-backed `/events` feed, no push in v1 (§4.2).
- M4 auth → **token-only**: scoped bearer tokens bound to actors,
  issued/rotated via CLI, one mechanism for GUI/REST/MCP; OIDC may layer
  on later.
- Attachments → **deferred past MVP**; schema leaves room, bug evidence
  meanwhile lives as paths/URLs in body text.
- **M5 (GitHub write-back) before M6 (GUI)** — build the GUI once against
  complete data. This ordering is fixed, not swappable.

Resolved 2026-08-12 (r2, after whole-proposal review):

- Gates → **status, not enforcement** (§2.1, §5); `~/WorkingStack` and
  `project migrate` withdrawn (§5.1).
- Dependency tracking → **references, not resolved versions** (§3.5).
- MVP line → **M0–M2**, with read-only GitHub collectors pulled into M2
  (§1.1).
- Observed-first → **promotion by convention + suppression + root
  exclusions** (§3.6).
- `/events` cursor → **single events table in the declared store** (§4.2).
- Drift evidence → **snapshot at raise time + pinned against retention**
  (§3.2).
- Project granularity → **one repo/folder; workspace members are not
  projects** (§3.1).

Resolved 2026-08-12 (r3):

- **No discovery, no continuous checking** (§2.1, §5, §5.1). Collection
  scope is the registered project list; the contract is evaluated on
  demand and reported with its age. FR-G5–G8 deferred.
- AI-assisted drift detection and recommendation is recorded as a
  **non-committed stretch goal** (`REQUIREMENTS.md` §3) — the reason the
  scheduler stays small, and something no v1 requirement may lean on.

Resolved 2026-08-12 (the last of the open questions):

- **Comment write-back is outbound only** (FR-B5). Comments authored in
  Vogt post upstream under `comment_only`/`full`; inbound forge comments
  stay observations against the linked item. Mirroring both ways would
  need forge-author identity mapping and loop suppression for our own
  writes, and buys little the observation view doesn't already give.
- **No manual ranking override.** `rank_order` is dropped from
  `work_items`; ordering is computed from documented weights and stays
  fully explainable, with `priority` and initiative weight as the
  hand-set inputs that already feed the score (§3.4).
- **Trust is `disputed`, drift resolution is `contested`** (§6). The
  computed vocabulary and the chosen vocabulary no longer collide.
- **Cadastre: accept the duplication for v1** (§11).

Resolved 2026-08-12 (r4):

- **Deployment target: a Node B Compose stack deployed by Komodo**
  (`DEPLOYMENT.md` §2.2). Desired state in `indexarr/ops` at
  `personal/vogt/`, image from GHCR digest-pinned, exposure bound to the
  Tailscale address with no public DNS and no Caddyfile entry. `personal/`
  rather than `prod/` because this is homelab infrastructure, matching
  `personal/cadastre`.
- **TLS terminates in-process, not behind a proxy** (NFR-D6 revised). Node
  B's Caddy is host infrastructure rather than a Komodo stack, and a
  tailnet-only listener that already holds a Tailscale-issued certificate
  gains nothing from fronting it.
- **"No default port anywhere" was too broad** (NFR-D2 revised). Defaults
  that encode exposure or identity stay forbidden; defaults that are pure
  host allocation are now *required*, because gating them is what broke
  every cadastre deploy after `cadastre#42`. The distinction is what the
  value decides, not whether it is a number.
- **Publish and deploy are separate acts** (NFR-D10). A tag publishes a
  signed image; production moves only when a human or agent bumps the
  pinned digest in ops and runs `DeployStack`. Automating that bump via
  `ops/scripts/komodo-deploy.sh` stays available and is not v1 scope.

Nothing is open. New questions get appended here as they arise.

---

## 10. Roadmap

Defined in [`ROADMAP.md`](ROADMAP.md): seven stages (M0 Foundation → M6
GUI) with per-stage requirement IDs and demo acceptance criteria.
**MVP = M0–M2** (r2): a daily-usable tracker with observed-first views over
the local estate *and* read-only GitHub. M3 adds compliance reporting and
the drift lifecycle, M4 service mode, M5 forge consolidation and
write-back, M6 the GUI. **v1 = M0–M6.**

---

## 11. Relationship to cadastre

Cadastre (the land register — infrastructure, hosts, services) and Vogt
(the reeve — product work, backlog, project health) are separate products
with separate domains and separate stores. **Vogt does not import from
cadastre and does not couple to its API in v1.**

The cost of that decision is real and should be stated rather than
discovered: the declared/observed split, coverage modelling, trust
computation, the audit spine, the operation registry, the transport-parity
harness and the MCP stdio bridge are all being written a second time, and
will drift apart under two maintenance loads.

**Decision (2026-08-12):** accept the duplication for v1 and re-converge
afterwards. Building Vogt against a shared kernel now would
couple two designs while one of them is still being learned, and the r2
review of this proposal is itself evidence that Vogt's requirements are
still moving. Instead: keep the shared concepts *named* identically across
both codebases (`declared`/`observed`, `sweep`, `coverage`, `trust_state`,
`drift_proposal`, operation registry), so that extracting a common package
after v1 is a mechanical refactor rather than a redesign. Revisit at v1
with two working implementations to compare.

The related open question — whether cadastre eventually feeds *in* as a
collector, adding infrastructure context to a project's brief — stays open,
and is a read-only integration over its REST API if it happens. It is not
a v1 dependency in either direction.
