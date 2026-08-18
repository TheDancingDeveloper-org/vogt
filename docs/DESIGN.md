# Vogt — Design Outline

Status: **v0.3 (revision r5), built**; **§1.2 reversed at r9** (2026-08-14 —
Vogt runs the work it governs) · design 2026-08-12, reconciled against the
delivered v1 on 2026-08-12, and against the merged product on 2026-08-15.
Scope: standalone product. Cadastre is prior art and a lessons source, not a
dependency — see §11.

**This document describes what Vogt *is*.** That rule was tightened on
2026-08-15 and it changes how to read the sections below. Where the build
decided something differently from an earlier draft, the decision is described
in place and the withdrawn alternative is named so the absence reads as a
choice. Where something was designed and **never delivered**, it is *not*
described here as though it existed: it is a numbered gap in
[`REQUIREMENTS.md`](REQUIREMENTS.md) §7, with what is missing and what its
absence costs. A design document that describes unbuilt things is the most
expensive kind of wrong, because it reads exactly like one that does not.

The requirement-by-requirement delivery verification is `REQUIREMENTS.md` §5
(v1) and §6 (v2).

Companion documents: [`REQUIREMENTS.md`](REQUIREMENTS.md) (numbered FR/NFR
baseline, the revision history, and §7's gap register),
[`SCHEMA.md`](SCHEMA.md), [`DEPLOYMENT.md`](DEPLOYMENT.md),
[`ROADMAP.md`](ROADMAP.md), [`ENGINE.md`](ENGINE.md) (the session engine's
own reference), [`USER_GUIDE.md`](USER_GUIDE.md).

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

### 1.2 Non-goals (v1) *(one reversed at r9)*

- Multi-forge support (GitHub + GitHub Actions are the only *optional*
  forge integration in v1).
- Multi-node / hosted SaaS (single-node self-hosted only).
- Time tracking, sprint ceremonies, burndown charts.
- ~~Being an agent runner. Vogt tells agents *what* and *why*; it does
  not execute them.~~ **Reversed at r9** — see below. The line stays because
  it was true for v1, and because a reader of the delivered v1 will find
  nothing in it that runs anything.
- Enforcing anything. Vogt reports; the human or agent acts (§5).

**The agent-runner reversal (r9).** Vogt now runs the work it governs.
MyDevEnv2 is merged in as Vogt's session engine — from the `dev` branch of
MyDevEnv2, head `2214a7d` — and with it come PTY sessions, agent tasks and
an assistant, so a work item can *open a coding session* in its project's
tree instead of only describing one. The design change is recorded here
rather than in a deleted bullet because the original line was not a mistake:
Vogt had no execution surface, and a tracker that claims to run agents
without one is describing a wish.

What made it a non-goal was never a distaste for execution; it was the
worry, in the same family as §2.1's "never going looking", that a system
which can act starts acting on its own. That worry is answered by a
narrower rule, which survives and is now the boundary: **Vogt never decides
to run anything on its own.** Every session traces to a person, or to a
schedule a person created. Autonomous work pickup — an agent taking the top
backlog item because it was there — is deferred by name in
[`REQUIREMENTS.md`](REQUIREMENTS.md) §3, and §5.1's rule that collection
scope is the registered project list is untouched. §2.1 also stands
unchanged: an execution surface is not an enforcement surface, and nothing
in the merged product consumes compliance, trust or drift status as a
precondition for running anything.

The numbered form of all this is `REQUIREMENTS.md` revision **r9** (families
FR-E, FR-T, FR-M, and the appended FR-U/FR-S/NFR rows); the merge's own
reasoning is [`MERGE_MYDEVENV2.md`](MERGE_MYDEVENV2.md).

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
- *r15*: `forge_state_mismatch` auto-accepts in **one direction only**.
  `gh-issues` reads open issues, and an unchanged subject is not
  re-appended, so the newest observation of an issue closed last month
  still says `open` — "closed upstream" is a fact somebody produced,
  "open upstream" is also what a closed-and-not-re-read issue looks like.
  Closing an item on the first is state-sync; reopening finished work on
  the second is a write made from an absence nobody observed, and is
  human-gated.
- *r15*: a work item's own text may name a forge issue without anybody
  adopting a link, and the two registers then disagree with nothing
  watching — `referenced_issue_state_mismatch` (FR-R7). Read-only,
  always human-resolved, and matched only on a **qualified** reference
  (`owner/name#12`, or an issue URL): a bare `#12` is as likely to be a
  pull request as an issue, which the work item that motivated the kind
  demonstrates in its own title.
- *r15*: a proposal whose raising condition a **later sweep no longer
  reproduces** is marked `superseded_at` rather than closed (FR-R6). It
  stays open, keeps its snapshot, and still needs a person — what changes
  is that the inbox can tell "still true" from "raised under evidence that
  has since moved on", which somebody was otherwise reconstructing from
  timestamps by hand.
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
- **Every edge is observed.** Reference kind is `path | git | declared`, and
  the delivered producers are the two manifest kinds: a dependency that lives
  only in a deploy script or in someone's head is not in the graph. `declared`
  is a `RefKind` member nothing writes — the gap and what it costs are
  `REQUIREMENTS.md` §7 (**FR-D9**); `SCHEMA.md` §2.2 records the same absence
  in the schema.
- **Resolution is by path or repo URL** to a registered project. Internal
  edges are exactly the ones expressed as paths and git URLs, which is why
  package identity is not needed. Unresolved internal-looking references
  are retained with their raw target and reported as
  `unresolved_dependency`.
- **`mirrored_source`** is reported where the same source appears both as a
  path member of one project and as a separate registered project. Reported
  as an observation; contents are never compared. *Built at r15*: its own
  offline collector (`mirrored-source`), matching on the package name both
  manifests declare — the identity the two copies already agree on, and the
  only signal available that is not a content comparison. A name two
  registered projects both claim is dropped rather than guessed at. `deps`
  lists the relation from both ends; nothing asserts divergence, and the
  two declared versions are recorded as the facts they are.
- **`dep-refs` writes one scan record per project** (*r15*), naming the
  manifests it read, the ones that would not parse, and the ones present in
  a format it does not read at all. Without it a `deps` answer of "no
  references" is three different answers wearing one number: a project that
  references nothing, a Go or Maven project whose graph this cannot see, and
  a project no sweep has walked (FR-O4).
- Update-automation posture (version-updates config / vulnerability alerts
  / automated security fixes — three independent toggles, never one
  boolean) is **forge posture**, not dependency data. It moves to the forge
  module at M5.
- Roadmap fit: reference extraction and the cross-project graph land with
  M2; `unresolved_dependency` reporting with M3; `mirrored_source` and the
  scan record at r15, after an estate onboarding produced eighteen mirrored
  crates by hand.

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
               (network): gh-issues, gh-prs, gh-actions, gh-releases,
               session-outcomes (registered only when an engine is
               configured; FR-E6, FR-E7)
gui/           static ES modules consuming the HTTP adapter only (M6 chose
               buildless over React — see `ROADMAP.md` M6; a wheel that
               needs npm to build is the cost that decided it)
storage/       SQLite x2 (declared.sqlite3, observed.sqlite3), migrations
```

**The merged tree, from r9.** The repository also carries the session engine
and the front ends it brought, and the layer rules above still describe the
Python core rather than the whole product:

```
engine/        the Rust session engine (server + contract crates): PTYs,
               scrollback, activity, agent tasks, push, the assistant —
               and, from M9, the front door: the only listening process,
               proxying /api/vogt and /mcp to vogt-core on loopback
web/           the Solid PWA, the product's front end from M11; consumes
               the engine's API and, through /api/vogt, the same public
               operations the CLI and MCP see. One route model supplies the
               desktop rail, phone bar and Sessions tools with their current
               place/tool and owns loading, unavailable and not-found states.
               Sessions owns the machine workspace: terminal/editor panes and
               Git, History, Tasks, GUI and Assistant deep links compose there.
               Terminal panes alone stay mounted while inactive; other tools
               unmount and retain only their promised selection/draft state.
mobile/        the Capacitor shell that loads that PWA
src/vogt/      unchanged, and still the only definition of an operation
```

The engine's own reference — what it owns, how to run it, its full wire
contract, the assistant and the agent-task scheduler — is
[`ENGINE.md`](ENGINE.md). It is one document because it used to be eight, each
describing MyDevEnv2 as a separate product.

Two properties hold the shape together, and both are asserted rather than
described. **The registry is still the single definition**: the PWA's route
table resolves against it, and so do the assistant's Vogt tools, which are
fetched from the core's own MCP `tools/list` rather than written out again.
**The core is still complete alone**: it serves its own port, its own GUI
and its own MCP when no engine is present, and CI runs the suite with
`engine/`, `web/` and `mobile/` deleted to keep that true (NFR-Q6).

The direction of dependency is the thing to preserve. The engine calls the
core, and the core calls the engine only for sessions — four operations
across a loopback boundary, which is what makes two processes worth having
instead of one language argument (`MERGE_MYDEVENV2.md` §4).

- **Transport parity tests**: for each use-case, a matrix test drives it via
  CLI, REST, and MCP and asserts identical results and identical audit rows.
  Two explicit exclusion lists exist and are checked for staleness in both
  directions. `LOCAL_ONLY` holds six — `init`, `serve`, `backup`, `restore`,
  `import`, `mcp.stdio` — each acting on the local process or data directory
  with no meaningful remote semantics, and each carrying its reason as the
  dictionary's value rather than in a comment. `HTTP_ONLY` is **empty**, and
  has been since M4: no operation has yet earned an exception in that
  direction. It stays declared because a list that only exists once something
  needs it is a list nobody adds to correctly.
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
- **Both allow and deny decisions are audited** — into `auth_decisions`
  (`SCHEMA.md` §2.1), separately from the declared-write audit, because a
  denial changes nothing and so has no entity or revision to hang from.
- Five scopes exist, parse, imply correctly, and **each gates at least one
  operation** *(r13 — until then `writeback` gated none, so a token issued with
  it in good faith could only read)*. `writeback` gates `forge.writeback`,
  which arms or disarms a project's upstream pushing, and is deliberately not
  implied by `project.write`: managing projects and deciding this instance may
  speak to a forge on your behalf are different powers, and only the second has
  effects outside the instance. Causing an individual upstream write still
  needs `work.write`, because it is a consequence of commenting or
  transitioning; the scope decides whether that consequence is switched on.
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

*As built, one shape difference runs through all of it: **there are no path
parameters**.* `HttpRoute` is a method and a literal path, and each
operation's arguments are one pydantic model shared by all three transports
— so an identifier travels as a query or body field, never in the URL. The
sketch's `GET /projects/{id}/brief` is `GET /api/projects/brief?project=…`,
`POST /work/{id}/transition` is `POST /api/work/transition`, and
`POST /drift/{id}/accept|reject|contest` is one `POST /api/drift/resolve`
carrying the resolution. Nothing uses `DELETE`: `DELETE /suppressions/{id}`
is `POST /api/suppressions/revoke`, because a revocation is an audited write
that needs a reason, and a reason does not fit a `DELETE`. The registry holds
**61 operations**; the six in `LOCAL_ONLY` are not mounted, so **55 routes** sit
under `/api`. The paths below name the operation, not the URL.

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
- *(r6)* `POST /projects/import` — clone a named repository, register it,
  consolidate it (§5.2). And `GET /notifications` — what GitHub is trying to
  say about the registered projects, collected per repository as observations
  (FR-O8). Deliberately a different surface from `/events` above: that feed
  is what *this instance* did, ordered by a cursor it owns, and merging a
  forge's inbox into it would make the cursor meaningless and the provenance
  unreadable. The inbox belongs to the configured token's account, which
  makes it instance-scoped rather than per-actor — a limit the view states
  rather than hides.
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

A **Contract** states what a compliant project looks like. It carries a
version identifier so a status can name which contract it was evaluated
against, and it is **sourced from configuration** — four settings
(`contract_required_files`, `contract_required_dirs`, `contract_required_meta`,
`contract_version`), read through one helper so no call site can pick up three
of them and miss the fourth *(r13; before that it was a constant, and a
self-hoster could not state a different contract without editing Python)*.

Making the rules editable puts pressure on the version: an operator who edits
them and leaves the version at `v1` records statuses claiming to be the stock
contract. So a contract whose rules differ from the built-in default while
still carrying the default version gets a short digest of its own rules
appended — `v1+3f9a2c` — and a contract the operator *named* keeps that name,
because naming it is the deliberate act the digest infers in its absence.

The defaults, which are the contract this repository holds itself to:

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

**A criterion is asked of the repository, not of the disk.** Where the target
is a git checkout, a required file or directory must be *tracked*:
present in the working tree and absent from every clone is a failure, and it
says so — `AGENTS.md is present in the working tree but not tracked, so no
clone of this repository has it`. `CriterionResult.tracked` carries the
distinction (`None` where the question could not be asked, so
`contract check --path` on a plain folder still works as FR-G4 requires).

This was not a hypothetical. Two projects onboarded on 2026-08-16 were
recorded as satisfying `AGENTS.md` on an untracked 288-byte stub — one of
sixty-three byte-identical copies a tool had dropped across the workspace —
and one satisfied `docs/` on an *empty* untracked directory, which git cannot
represent even in principle, while its real documentation sat in
`documentation/`. A compliance number that improves as such stubs spread, and
not at all as repositories are fixed, is measuring the wrong thing. It also
makes a recorded status reproducible: the same commit checked out elsewhere
now gives the same answer.

The consequence worth stating plainly is that a required directory needs a
tracked file in it — which is why `design/` in this repository holds a
`.gitkeep`, and always did.

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

### 5.2 Import is registration for a repository that lives elsewhere (r6)

Registration assumes the working tree is authoritative for its own
provenance. That holds for a folder and fails for a checkout of a repository
that lives on GitHub: register the local tree and the first sweep is
comparing two sources whose relationship nobody has established. Import
(FR-P6) establishes it — clone the named repository into `import_root`,
register the result with `repo_url` set, consolidate the existing forge state
(FR-B3) — so the local tree begins as a known derivative of the remote and
every later divergence is news rather than ambiguity.

Import is **not** discovery, and §5.1 is unchanged. The user names one
repository; nothing is listed, searched or suggested. The distinction is not
academic — the difference between "clone what I named" and "show me what I
have" is the difference between one command and a permanent classification
problem, and the second one arrives disguised as a dropdown on the import
form (`REQUIREMENTS.md` §3, deferred by r6).

Three properties make the operation safe to repeat. The clone lands before
the declared write, so a failed registration leaves a checkout rather than a
project pointing at nothing — the same ordering `project create` uses for its
scaffold. An occupied destination is never overwritten: a clone of the same
remote is registered as-is, anything else fails untouched (FR-P7). And the
credential reaches `git` through an askpass helper rather than the remote
URL, so it appears in no process listing, no `.git/config`, and no stored
`repo_url` (FR-S8) — FR-S7's rule, applied to the one place a token would
otherwise be trivially embeddable.

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
- `vogt init | migrate | status | backup | restore | export | import`. `init`
  creates and is idempotent; `migrate` moves an existing instance forward and
  refuses a data directory that holds none, because collapsing the two would
  make the destructive-sounding verb the safe one. **`serve` migrates before it
  assembles anything**, and `/health/ready` compares each store's applied
  version against the highest migration the build ships, answering `503` that
  names the store, both numbers and the verb to run *(r13 — until then it
  reported the applied number alone, so a container carrying an unrun migration
  came up green and failed afterwards on a missing table)*. A store ahead of
  the build stays green: `migrate` refuses that case with more diagnosis than a
  probe can carry.
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
  - `release.yml` — tag-triggered only: semver image tags, `latest`, the
    wheel, SBOM and provenance attestations, keyless cosign signing,
    publish to GHCR. It publishes; it does not deploy.
  - `build.yml` *(r5)* — a push to main, docs paths ignored: the same
    image, tagged `sha-<commit>` only, signed, with no semver and `latest`
    unmoved. A build is not a release.
  - A mixed commit (code + docs) runs the full pipeline; the docs job is a
    subset, never a bypass — branch protection requires `ci.yml` only when
    code paths changed, handled with a single gate job that succeeds
    trivially on docs-only diffs rather than by marking checks optional.
- **Releases stay tag-driven** so a merge can never cut one, and no push
  can deploy one (NFR-D10). What r5 changed is that obtaining a *deployable
  artefact* no longer requires inventing a version number for it —
  `REQUIREMENTS.md` r5 records why three versions in one afternoon was the
  symptom.
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

Post-v1: M7 (import and the notification inbox) and M8 (`connect` — reaching
an instance from an agent environment). The merge stages **M9–M14** carry the
§1.2 reversal into delivery — foundations, coding sessions, GUI uplift, the
AI layer, mobile, consolidation. **v2 = M9–M14**, M14 being the consolidation
stage rather than a feature one.

Two of those stages end in a demo that has not been run, because neither can be
run without a browser and a phone: M11's and M13's. `ROADMAP.md` says so at
each stage and `REQUIREMENTS.md` §7 carries them as gaps, so that "built"
nowhere quietly means "watched working".

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
