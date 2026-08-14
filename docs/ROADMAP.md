# Vogt — Deliverable Stages (v0.3, revision r9)

Status: **M0–M6 delivered — v1 is built** (2026-08-12); **M7 and M8 are
post-v1**; **M9–M14 are v2** — the MyDevEnv2 merge, added by
`REQUIREMENTS.md` revision r9 (2026-08-14). **M9–M13 are built**
(2026-08-14); M11's demo is outstanding for want of a browser, and M13's for
want of a device, and its "as built" note says what that costs. **M14 is in
progress** — the sentence here previously read "M12–M14 are not started",
which contradicted the line above it in the same paragraph. Requirement IDs
refer to `REQUIREMENTS.md`; per its §4, scope changes here must update that
document in the same change.

Each stage below carries an "as built" note recording where the delivery
differed from the sketch. What those notes cannot say is whether the set of
them adds up to the requirements baseline — that is `REQUIREMENTS.md` §5,
written after v1 by checking the build against every ID. It found seven
requirements short of their text (FR-G1, FR-D2, FR-L1, FR-S3, FR-S6,
NFR-I3, NFR-S4) and one CI gate that does not fire on the paths most likely
to trip it (NFR-Q4).
**Nothing in this document should be read as delivered until §5 agrees.**

## The cut lines

- **MVP = M0–M2** (r2): a tracker that is daily-usable by you and your
  agents — the write plane, observed-first views over your registered
  projects, and read-only GitHub so the views cover work that actually
  exists. Everything after is enrichment.
- **M3** adds contract checking and the drift lifecycle; **M4** makes it a
  self-hosted service; **M5** adds forge consolidation and write-back;
  **M6** adds the GUI. **v1 = M0–M6.**
- M5 precedes M6 and the order is fixed: build the GUI once, against
  complete data.
- **v2 = M9–M13** (r9): the merge with MyDevEnv2, after which Vogt runs the
  work it governs. **Merge-MVP = M9–M10** — the first build where a work item
  can open a coding session. M14 is consolidation and delivers no new ID.
- M9 precedes M10 for the same reason M5 precedes M6, from the other
  direction: the session capability is built once, against a repository and a
  stack that are already one.

Each stage ends with a **demo** — the stage is done when the demo runs,
its requirements' tests pass, and transport parity is green for every
operation the stage added.

| Stage | Name | One-liner | Requirements delivered |
|---|---|---|---|
| M0 | Foundation | Storage, audit + events spine, operation registry, CI skeleton | FR-L1(part), FR-S1, FR-S2(local), FR-A2(part), FR-A3(harness), FR-N1(store), NFR-Q1–Q5, NFR-C1–C4, NFR-I1, NFR-I3, NFR-S3, NFR-O1, NFR-O3, NFR-PO3 |
| M1 | Core tracker | Local work tracking over CLI + REST + MCP stdio | FR-P1, FR-P2, FR-P4, FR-P5, FR-G11, FR-W1–W3, FR-W6–W9, FR-V1–V3, FR-A1–A4, FR-A5(stdio), FR-S6, FR-N1, FR-N2, NFR-Q3 |
| M2 | Eyes *(MVP)* | Collectors incl. read-only GitHub, observed-first, suppression, trust & freshness | FR-O1–O4, FR-O5a, FR-O6, FR-O7, FR-W4, FR-W5, FR-W10, FR-W11, FR-G12, FR-G15, FR-V4, FR-D1–D4, FR-P3, FR-R4, FR-L3, NFR-I2, NFR-I4, NFR-I5, NFR-S1, NFR-S2, NFR-S4, NFR-PO1, NFR-PO2 |
| M3 | Contract & drift | On-demand contract checks; the drift proposal lifecycle | FR-G1, FR-G3, FR-G4, FR-G13, FR-G14, FR-R1–R3, FR-R5, FR-D5, FR-D8 |
| M4 | Service | Node B stack via Komodo, auth, remote MCP, ops | FR-A5(full), FR-A6, FR-A7, FR-S3–S5, FR-S7, FR-L1(full), FR-L2, NFR-D1–D10, NFR-C5, NFR-PO4, NFR-O2 |
| M5 | GitHub module | Consolidation, forge drift, write-back | FR-O5b, FR-B1–B5, FR-D6 |
| M6 | GUI | The visual surface over the same API | FR-U1, FR-U2 |
| M7 | Onboarding & inbox *(post-v1)* | Import a repository from GitHub; collect its notifications | FR-P6, FR-P7, FR-S8, FR-O8, FR-N3, FR-U3 |
| M8 | Reachable by an agent *(post-v1)* | `connect`, and the five estate prerequisites behind it | FR-A8; `DEPLOYMENT.md` §7 (1–5) |
| M9 | Foundations *(v2)* | One repo, one stack, one published port; both halves' CI green | NFR-D11, NFR-D12, NFR-C6, NFR-Q6, FR-U9 |
| M10 | Coding sessions *(v2, merge-MVP)* | A work item can open a session in its project's tree | FR-E1–E5, FR-E8, FR-E9, FR-S9, FR-S10 |
| M11 | GUI uplift *(v2)* | The Solid PWA becomes the single front end, specified to interaction depth | FR-U4–U8, FR-U10–U22, NFR-S5 |
| M12 | AI layer & voice *(v2)* | The assistant learns the Vogt domain; voice is validated rather than assumed | FR-T1–T4, FR-T6; FR-T5 validated, FR-T7 attempted |
| M13 | Mobile MVP1 *(v2)* | The phone is a first-class surface | FR-M1–M3, FR-E6, FR-E7 |
| M14 | Consolidation *(ongoing)* | Old stacks retired, names settled, r9 verified against the build | NFR-I6 (unclaimed by any other stage) |

Deferred and withdrawn requirement IDs (FR-G2, FR-G5–G10, FR-D7) appear in
no stage by design — see `REQUIREMENTS.md` §3.

**On the numbering of M9–M14.** `MERGE_MYDEVENV2.md` §14 drafted these
stages as M8–M13, on the stated grounds that M7 was the last one. M8 was
already taken by *Reachable by an agent* above — the stage during which r8's
protocol-negotiation failure was found — so the merge stages start at M9.
The requirement IDs they deliver are unaffected: §4's append-only rule
governs IDs, and no ID moved.

---

## M0 — Foundation

**Objective**: the skeleton everything else bolts onto, with the quality
gates and CI shape locked in before any feature exists.

Deliverables:
- Repo scaffold satisfying its own default contract — including the
  `LICENSE`, which makes licence selection an **M0 decision**, not a
  pre-publication one (NFR-O1/O3).
- `pyproject.toml`, committed `uv.lock`, Renovate config (all three
  toggles), mypy strict + ruff + coverage gate wired.
- GitHub Actions: `docs.yml` / `ci.yml` / tag-only `release.yml` with the
  docs-skip path filtering and gate-job pattern. (`build.yml` joins them at
  r5, publishing commit images from main — it is not an M0 deliverable.)
- `declared.sqlite3` + `observed.sqlite3` with migration framework, lock,
  meta/revision; `audit` and `events` tables and the transactional write
  path — entity + audit row + event row + revision bump, atomically
  (NFR-I1, FR-N1).
- Actor model with `local:<os-user>` principal derivation; `reason`
  required and non-empty on every write.
- Storage behind an interface that avoids SQLite-only semantics, so a
  Postgres backend stays possible without a redesign (NFR-S3).
- The transport-neutral **operation registry** and the parity-test harness
  with both exclusion lists (`HTTP_ONLY`, `LOCAL_ONLY`) live.
- CLI: `init`, `status`.

**Demo**: `vogt init`, register a project record from the CLI with a
reason, `status` shows revision 1, the audit row carries actor + reason,
and `/events` returns exactly one row at `seq=1`. `mypy --strict` and the
parity harness pass in CI; a docs-only commit runs only `docs.yml`.

### M0 as built — three notes

Recorded because each one differs slightly from the sketch above, and the
absences and additions should read as decisions.

1. **Six operations, not two.** The demo requires registering a project and
   reading the event and audit rows it produced, so M0 ships
   `project.register`, `project.list`, `events.list` and `audit.list`
   alongside `init` and `status` — each on all three surfaces. This is the
   demo's own surface area, not an M1 pull-forward: FR-P1's lifecycle
   transitions, FR-P2's brief, FR-S6's full audit query and FR-N1's complete
   feed semantics all remain M1 work.
2. **`LOCAL_ONLY` is not empty; it names `init`.** DESIGN §4 already lists
   `init` among the operations with no meaningful remote semantics, so
   starting the list empty would have meant either an exclusion that lies or
   an `init` route that a running server cannot honour. The list is live and
   checked for staleness in both directions, which is what FR-A3 asks for;
   `backup`, `restore` and `serve` join it at M4.
3. **`init` is a bootstrap, not a declared write.** It creates the instance
   rather than changing anything inside one, so it lands an audit row at
   revision 0 and emits **no event** — which is what makes the demo's
   "exactly one row at `seq=1`" true, and means a client attaching to
   `/events` sees changes it can act on rather than the instance's own
   birth. Every other write, including the auto-registration of a
   previously unseen principal, lands both rows (`SCHEMA.md` §2.5).

## M1 — Core tracker (first daily-usable build)

**Objective**: a working local tracker — the write plane — reachable from
all three surfaces.

Deliverables:
- `project register` / `project create` (scaffolds a compliant skeleton;
  registration is never refused, FR-G11); lifecycle states; the
  one-repo-one-project granularity rule (FR-P5).
- Work items (4 kinds, p0–p4, effort, assignee); typed cross-project
  relations (`depends_on`, `relates_to`, `duplicate_of`, `parent_of`);
  labels (GitHub-aligned); initiatives; comments.
- Cursor-based `/events` feed over the M0 events table.
- Workflow engine with per-kind state machines; rejected transitions name
  the violated rule.
- Deterministic ranking with `why` explanations; `brief`, `backlog`,
  `bugs` views with filters.
- REST (FastAPI + OpenAPI) and MCP stdio, both generated from the
  registry; CLI verbs for everything; audit browsing.
- No manual ranking override: ordering is computed, `why` explains all of
  it, and `priority` / initiative weight are the hand-set inputs.

**Demo**: from Claude Code via stdio MCP: create a bug, block it on
another item, transition it, ask `backlog` and `why` — then show the same
state from the CLI and `curl`, with identical answers, and the audit trail
and event feed of everything the agent just did.

### M1 as built — four notes

1. **27 operations, one definition each.** The registry now carries the
   whole write plane plus the views, and the parity harness drives every one
   of them through CLI, REST and MCP as a single ordered script against three
   isolated instances — ordered, because you cannot relate two work items
   before creating them, and a `why` that never ran against a real ranked
   item proves nothing.
2. **`mcp.stdio` joins `init` in `LOCAL_ONLY`.** A transport that takes over
   the process's stdout has no remote semantics; offering it as a REST route
   would mean a server hijacking its own framing channel. It is also a bare
   `vogt-mcp` console script, because an MCP client config wants a command
   rather than an argument list.
3. **Workflows are stored, not hard-coded.** `workflow_defs` holds one
   machine per kind, seeded from the shipped defaults by `migrate` — not by
   bootstrap, because an instance created before the table existed never
   bootstraps again, and not by the migration SQL, because the defaults would
   then be spelled twice and drift. `workflow.list` publishes them, so an
   agent can pick a legal next state instead of guessing and handling a
   rejection.
4. **Ranking rounds before it answers.** An unrounded staleness contribution
   carries float noise like `1.157e-05`, which is not information, reads as
   spurious precision in `why`, and makes two reads of one item look
   different. Four decimal places is far below any ordering the weights can
   express. `ci_red_boost` is listed in `why` as an input that *cannot fire
   yet* rather than omitted, so the explanation is honest about what it is
   not considering.

## M2 — Eyes (MVP complete)

**Objective**: the tool sees work you didn't type in — including the work
that lives on GitHub, which is where most of it actually is.

Deliverables:
- Collector framework: plugin registry, in-process scheduler, on-demand
  `sweep`, sweep/coverage records, digest dedup, append-only store +
  rebuildable `latest_*` tables. **Scope is always the registered project
  list** (FR-G15) — no crawling, no candidates.
- Core collectors: `git-local` (branch, dirty state, tags → version),
  `source-markers`, `dep-refs` (path/git/workspace references from
  `Cargo.toml`, `package.json`, `pyproject.toml` — manifests only, no
  lockfiles, no versions).
- **Read-only GitHub collectors** (FR-O5a): issues, PRs, Actions runs,
  releases/tags. No writes, no backfill, no posture — those are M5.
- Noise control before observed-first is switched on: promotion by
  convention (`TODO(vogt):`), audited `suppress`, per-project exclusions.
- Observed-first backlog/bugs; `adopt` promotion with maintained links.
- Cross-project dependency references: resolution to registered projects,
  reverse lookup, unresolved targets retained.
- Computed trust states; freshness stamps on every aggregated answer;
  "not collected" semantics for unswept scope.
- Retention policy over the observed store (NFR-I5): latest observation
  per subject kept indefinitely, history pruned on a configurable window.
  The drift-proposal exemption arrives with FR-R5 at M3.
- The NFR-S4 benchmark fixture at the ~500-project / ~100k-item envelope,
  asserting the sub-second interactive target of NFR-S1 in CI.

**Demo**: register a dozen real projects; sweep. The global bugs view
shows GitHub issues *and* promoted markers from rustnzb and rustTorrent
with freshness stamps and trust states; suppress a noisy marker and watch
it leave the ranked view but stay in `observations`; `deps --project
nzb-core` lists the projects referencing it; adopt one item into a ranked
work item. Then re-run the whole suite with the network unplugged and the
GitHub adapter disabled — everything except forge observations still works
(NFR-PO1/PO2). **This demo is the MVP acceptance test.**

### M2 as built — four notes

1. **Two derived tables, not five.** `SCHEMA.md` §3.2 named five typed
   `latest_*` projections. Only the dependency one carries anything the
   observation does not already say, so this ships `latest_observations`
   (generic, keyed by subject) plus `latest_dep_refs`. The other four
   differed in payload shape and not in behaviour, and five rebuild paths
   would have been five places for a collector and its projection to drift
   apart. Recorded in the migration.
2. **A marker is a leading annotation, not a mention.** The first sweep of
   this repository promoted 21 "markers" — every one of them documentation
   *about* the promotion pattern: `DESIGN.md` explaining that `TODO(vogt):`
   enters the backlog, the generated `config.example.toml` listing the
   defaults, a table cell naming them. Vogt read its own description of
   markers and filed it as work. The pattern is now anchored to the start of
   a line, after comment leaders and list bullets only, which took that
   repository from 283 observed markers to 57 and from 21 promoted to 0 —
   the right answer, since neither project uses the convention yet. Any
   project that documents its conventions would have hit this.
3. **Suppression and adoption live in the declared store.** Both are audited
   decisions, not observations, which is exactly why a suppression survives
   re-observation of the same subject — a dismissal recorded in the evidence
   store could never have worked. Adopted subjects are folded into their
   work item rather than listed twice.
4. **The benchmark is a tripwire, not a benchmark suite.** It seeds 500
   projects and asserts the ranked views stay interactive. The threshold is
   generous because a wall-clock number on a shared runner is not a metric;
   what it actually catches is an accidental per-item query inside a ranked
   view, which would otherwise be invisible until M6.

## M3 — Contract & drift

**Objective**: the contract is checkable and disagreements between
declared state and observation become resolvable proposals.

Deliverables:
- The default contract (config, versioned identifier); `contract check`
  against any path, returning every rule evaluated and every failing
  criterion (FR-G3/G4).
- Recording the result as a registered project's compliance status with a
  checked-at timestamp, surfaced with its age on the brief and the global
  view (FR-G14). Nothing re-checks on a timer.
- The FR-G13 assertion tested: no code path consumes compliance, trust, or
  drift status as a precondition.
- Drift proposal lifecycle: raise with evidence, accept / reject /
  contest, all audited, with the default low-risk auto-accept policy;
  evidence snapshot at raise time and retention pinning (FR-R5).
- Local drift kinds: `version_mismatch`, `unresolved_dependency`.
- `mirrored_source` reporting (FR-D8) where a path member is also a
  registered project.

**Demo**: `contract check` a project missing `AGENTS.md` → the result names
exactly that criterion, and the project's brief shows `non_compliant`
alongside "checked 4 seconds ago"; check nothing for a week and the same
brief says so rather than quietly refreshing. Tag a release without
updating the declared version, sweep → `version_mismatch` proposal with
evidence; accept it, and the audit trail shows who accepted it and why.
Delete the observation's history window and confirm the proposal still
renders its evidence.

### M3 as built — three notes

1. **FR-G13 is asserted structurally, not just behaviourally.** One test
   registers work against a non-compliant project and transitions it, to
   show nothing refuses. A second greps the source: only the modules that
   *compute or report* compliance may mention `compliant` /
   `non_compliant` at all. A service that starts comparing against those
   values fails the suite, which is the only version of this rule that
   survives people who have not read the requirement.
2. **`contract-checker` is a collector that opts out of sweeps.** It
   carries `on_demand_only`, and `CollectorRegistry.select` skips such
   collectors unless they are named. Without that flag, adding the M4
   scheduler would quietly reintroduce the continuous re-checking r3
   deleted — the requirement would have been enforced by nothing but
   memory.
3. **Evidence is protected twice, deliberately.** A proposal carries a
   self-contained snapshot *and* pins the observation it points at against
   retention. Either alone would do in the happy path; both are needed
   because the two stores are pruned, backed up and restored
   independently. The test deletes `observed.sqlite3` outright and the
   proposal still renders its evidence.

## M4 — Service (the Node B stack)

**Objective**: from local tool to always-on service with real identity,
running where it will actually live — `DEPLOYMENT.md` §2.2 is the target,
not a shape to be decided at this stage.

Deliverables:
- `serve`: one port, path-routed GUI-placeholder / `/api` / `/mcp` /
  health; streamable HTTP MCP with protocol-version negotiation;
  `/connection-info`; in-process TLS from a mounted certificate
  (`--tls-cert` / `--tls-key`, NFR-D6).
- Token auth bound to actors; scopes; double-gated writes; scope-filtered
  `tools/list`; allow+deny audit; token-file config only.
- `vogt-mcp-remote` bridge with startup tool discovery and stderr skew
  warning.
- `backup` / `restore` / `export` / `import`; generated example configs;
  the NFR-D2 default-policy split enforced (exposure values ungated,
  allocation values defaulted).
- `release.yml` completes: buildx image, SBOM and provenance attestations,
  keyless cosign sign over the digest, push to
  `ghcr.io/thedancingdeveloper-org/vogt`, tag-triggered only, on
  `[self-hosted, node-b, linux, x64, docker, publish]`. *(As built: the
  attestations are buildkit's rather than a separate syft run, and cosign
  signs without a separate `attest` step. `build.yml` joins it at r5 —
  see `REQUIREMENTS.md`.)*
- The ops-repo stack: `indexarr/ops` → `personal/vogt/docker-compose.yml`,
  digest-pinned, tailnet-bound, hardened per NFR-D9, healthchecked on
  `/health/ready`. **Allocate the port here** and verify it free on Node B
  before committing (see `DEPLOYMENT.md` §2.2).

**Demo**: tag a release → GitHub Actions publishes a signed image and
nothing deploys; pin the digest in `indexarr/ops`, `DeployStack`
`personal-vogt`, and the stack comes up on Node B. From a dev box on the
tailnet, Claude Code connects via the bridge with a read-only token —
write tools are absent from its tool list; swap to a `work.write` token —
they appear. Probe `/health/ready` with plain curl over the Tailscale
address (and confirm nothing answers on the LAN address). Backup, destroy
the stack, redeploy, restore — state intact. Then revert the digest and
redeploy to prove rollback.

### M4 as built — four notes

1. **Port 18094 is allocated, and was verified free** rather than
   inherited from `DEPLOYMENT.md`. Cadastre holds 18090 and 18092; 18095
   was already in use. The compose file carries it as a *default*, not a
   `${X:?}` gate — gating allocation values is what cost cadastre every
   deploy after `cadastre#42`.
2. **The served context carries the authenticated principal.** The first
   version resolved the token correctly and then ran the operation under a
   context built from the OS user, so every audited write through the
   server would have been stamped with the server operator's name. A test
   that asserted attribution caught it. Authentication that resolves the
   right identity and then does not use it is worse than none: it looks
   correct in the audit trail.
3. **A 401 is answered before an empty body is.** The bridge treated an
   empty response as "notification, no reply", so a rejected token left the
   client waiting forever instead of learning its credential was refused.
4. **Import is read-only, and says so.** Merging two instances needs an
   identity-conflict policy — same slug, different project; same ref,
   different item — and inventing one silently is how an import destroys
   what it was meant to restore. `restore` is the supported way to move an
   instance; `import` reads a file and reports what is in it.

## M5 — GitHub module (the adapter earns its keep)

**Objective**: the forge side stops being read-only observation and starts
keeping declared work honest — in both directions, carefully.

Deliverables:
- **Onboarding = non-destructive consolidation** (FR-B3): a read-only
  backfill of all *existing* issues, PRs, labels, releases, and CI history
  into observations. Zero GitHub mutations during onboarding; existing
  state is incumbent and preserved.
- Forge drift kinds: `forge_state_mismatch`, `vanished_upstream`,
  `ci_red_vs_healthy`; per-project/per-kind auto-accept policy.
- Per-toggle update-automation posture (FR-D6): version-updates config,
  vulnerability alerts, automated security fixes — three facts, three
  answers, never one boolean. `update_automation_gap` drift.
- Write-back module: `none / comment_only / full`, additive/forward-only,
  audited, re-observed on the next sweep. Comments go **outbound only**
  (FR-B5) — inbound forge comments stay observations against the linked
  item.

**Demo**: enable the module for the rustnzb org and verify — via the
GitHub audit log and `updated_at` timestamps — that onboarding changed
nothing upstream, while every existing issue and PR appears in the global
views with labels intact. Then close an issue on GitHub that a declared
item links → drift proposal; accept → the item closes with provenance.

### M5 as built — four notes

1. **"Changed nothing upstream" is asserted, not inspected.** The demo
   proposed checking GitHub's audit log after the fact. The test instead
   records every HTTP request the adapter makes and fails on any method
   that is not a GET — which cannot pass by accident, and fails at the
   moment somebody adds a mutation rather than the next time a human reads
   a log.
2. **Write-back is a consequence of a declared write, never a separate
   act.** There is no "push this upstream" operation: a comment authored
   here posts upstream *as part of commenting*, and finishing an item
   closes the linked issue *as part of transitioning*. A separate push
   command would let the declared change and its upstream half drift
   apart, which is the thing this product exists to notice.
3. **FR-B4 is enforced by absence.** There is no deletion, force or
   history-rewrite capability — not disabled, not gated, not present. The
   client refuses any method other than POST and PATCH, and a test asserts
   the union of every policy's permitted actions is exactly
   `{create, comment, label, close, reopen}`.
4. **Consolidation does not flood the backlog.** A backfill reads closed
   history too — that is the whole point — but only *open* issues are
   promoted. Otherwise onboarding a ten-year-old repository would put a
   decade of finished work into the ranked view on day one, which is the
   observed-first hazard (DESIGN §3.6) arriving through a different door.

## M6 — GUI

**Objective**: the visual surface, consuming only the public REST API.

Deliverables: per-project view; global backlog + bugs; drift inbox;
dependency reference graph; audit browser; trust, freshness and
compliance-age rendered on every aggregate. Served from the same single
port.

**Demo**: every M2/M3/M5 demo step repeated through the browser, and
nothing the GUI does is absent from the API (parity rule holds).

### M6 as built — one deviation and four findings

**Deviation: buildless ES modules, not a React SPA.** `DESIGN.md` §4's
architecture sketch named React (since corrected in place). The reason for not using it is packaging rather than taste: Vogt
installs as a Python wheel, so a framework build means either a Node
toolchain present at wheel-build time — `pip install vogt` requiring npm —
or committed bundler output, which is a generated artefact in version
control that nothing verifies. Neither is worth it for six views over an API
that already exists. What the design actually required is unchanged: the GUI
consumes only the public REST surface, adds no capability of its own, and is
served from the same single port. `test_the_gui_needs_no_build_step` fails if
a build step ever appears, so the deviation has to be re-argued rather than
quietly reversed.

1. **FR-U2 was unsatisfiable as specified.** "Trust and freshness on every
   aggregated view" could not be met for the drift inbox or the dependency
   graph, because `DriftListResult` and `DepsResult` carried no freshness.
   The GUI could have computed it client-side — which would have broken the
   parity rule from the other direction, the GUI doing something the API
   cannot. Fixed in the API. The inbox is where it matters most: an empty
   inbox is reassuring only if something has looked recently, and without
   freshness a collector that stopped running reads as "no drift".
2. **The parity rule is checked against the shipped source, not the
   intent.** A test extracts every `/api/...` literal from `app.js` and
   resolves it against the operation registry, asserts there is exactly one
   `fetch(` in the file, and asserts every operation the GUI names is
   non-mutating. A view that grows its own endpoint fails at the moment it
   is written.
3. **The comment-reading bug appeared for the fourth time.** The check that
   the GUI uses `sessionStorage` rather than `localStorage` failed on the
   comment explaining why. Same shape as the marker collector matching prose
   about markers and the deploy test reading its own comment: a comment
   explaining a rule contains the words the rule forbids. Every
   source-reading assertion in `test_gui.py` now goes through one
   comment-stripping helper.

4. **FR-L3 had never been built.** Found by walking every must-have
   requirement ID and asking which are cited nowhere in `src/` or `tests/`.
   FR-L3 has two halves — collectors run "on an in-process schedule" *and*
   are triggerable on demand — and only the second existed. `DEPLOYMENT.md`
   §1 has listed "collector scheduler (in-process background sweeps)" in the
   `serve` process diagram since M4, so the deployment document described
   something that did not run. Built at M6 and covered by
   `tests/test_scheduler.py`; the requirement belonged to M2.

The GUI is read-only. Resolving drift, transitioning work and setting a
write-back policy all require a reason its author typed, and a button cannot
type one — "accepted via GUI" is not a reason (FR-W1).

The schedule is on by default, at fifteen minutes. That is the one place a
default was chosen rather than required, and the reason is that an instance
which never looks cannot tell stale evidence from none — the failure this
product exists to prevent. `--no-schedule` and `sweep_interval_seconds: 0`
both turn it off.

---

## M7 — Onboarding & inbox (post-v1)

**Objective**: getting a GitHub repository into Vogt should be one act, and
what GitHub is trying to tell you about it should be visible here.

Deliverables:
- **Import** (FR-P6, FR-P7): `project.import` takes a repository the caller
  names, clones it into `import_root`, registers it with `repo_url` set, and
  runs the FR-B3 consolidation — one operation, one reason, one audit trail.
  Clone before declared write, so the failure mode is a stray checkout rather
  than a project pointing at nothing.
- **Clone credentials out of band** (FR-S8): the token reaches `git` through
  an askpass helper. Not in the URL, not in argv, not in the clone's config,
  and not in the stored `repo_url`.
- **`gh-notifications`** (FR-O8): a collector over the per-repository
  notifications endpoint, unpromoted, degrading to `partial` when the token
  lacks the scope.
- **The inbox** (FR-N3, FR-U3): a read-only view and a GUI page, plus the
  import form.

**Demo**: import a repository that exists only on GitHub, from the GUI, with
a reason. It lands on disk at the configured root, appears in the project
list with its issues and PRs already consolidated, and its notifications show
up in the inbox on the next sweep — while `/events` still contains only what
this instance did.

### Why import rather than register-then-reconcile

`project.register` assumes the working tree is authoritative for its own
provenance, which is true for a folder and false for a checkout of somebody
else's repository. Registering a local tree and letting collectors discover
the remote means the first sweep compares two sources of unknown
relationship; importing means the local tree is a known derivative of the
remote from the first observation, and every divergence after that is real
news. It is the same argument as consolidation being read-only (FR-B3): the
incumbent is authoritative, and Vogt's job is to notice, not to reconcile
what it could simply have known.

**The temptation this stage must refuse** is the repository picker. An import
form with a text field is one HTTP call away from an import form with a
dropdown of your repositories, and that dropdown is the registration
candidate listing r3 removed (was FR-G8). `REQUIREMENTS.md` §3 defers it
again by name so that adding it is a decision rather than a Tuesday.

### M7 as built — one deviation and three notes

**Deviation: the GUI is no longer read-only.** M6 shipped a rule — every
operation the GUI names is a read — justified by FR-W1: a write needs a
reason its author typed, and a button cannot type one. The import form can,
and does: `reason` is a required field, sent verbatim, and the audit row
records what the user wrote rather than "via GUI". The rule the GUI actually
keeps is therefore narrower than the one M6 wrote down, and it is now stated
that way: a mutating operation may appear in the GUI only through a view
that collects a typed reason. `tests/test_gui.py` holds the permitted set as
`GUI_WRITES` and asserts the reason field exists for each member, so adding
a second write means arguing for it rather than appending to a list. Drift
resolution stays out — resolving from a list *is* a button.

1. **The cloner is injected through the context, not passed to the
   handler.** `AppContext` already carries the clock and the id factory for
   exactly this reason, and `project.import` is the only operation that
   reaches the internet to change local state. Putting it anywhere else
   would have meant the parity harness — which drives every shared operation
   over all three transports — needing a network to run.
2. **The token reaches `git` through `GIT_ASKPASS`.** The three obvious
   alternatives all leak: a credential in the remote URL is written into
   `.git/config` permanently, `-c http.extraHeader=` sits in argv, and
   `credential.helper store` writes a file nobody deletes. Two tests assert
   the negative — the token appears in no recorded argv and in no resulting
   `.git/config`, and the helper does not survive the clone.
3. **An occupied destination is never overwritten.** A clone of the same
   remote is reused and reported as such; anything else fails untouched. The
   test that matters is the one where the destination holds unrelated files:
   an import that deletes somebody's working tree would be the most
   destructive thing this product could do, and it is the cheapest possible
   mistake to make.

---

## v2 — the merge (M9–M14)

Added by `REQUIREMENTS.md` revision **r9**, which reverses `DESIGN.md`
§1.2's *"being an agent runner"* non-goal: MyDevEnv2 becomes Vogt's session
engine, and Vogt runs the work it governs. The surviving boundary is that
Vogt never decides to run anything on its own — every session traces to a
person or to a schedule a person created (`REQUIREMENTS.md` §3, autonomous
work pickup). The design behind these stages is
[`MERGE_MYDEVENV2.md`](MERGE_MYDEVENV2.md) §§1–11; sizes are relative
(S < M < L), and as everywhere in this document, "as built" notes come after
the stage is built, not before.

## M9 — Foundations (M)

**Objective**: one repository, one stack, one published port — the ground
every later merge stage stands on, and nothing user-visible beyond the fact
that both halves now answer at the same address.

Delivers NFR-D11, NFR-D12, NFR-C6, NFR-Q6, FR-U9.

Deliverables:
- **The repo merge, with history**, from **MyDevEnv2@dev** (head `2214a7d`)
  via `git subtree`: `engine/` (Rust workspace), `web/` (Solid PWA),
  `mobile/` (Capacitor shell), MyDevEnv2's documents under `docs/engine/`.
  The archived GPUI desktop client is not carried over
  (`REQUIREMENTS.md` §3). Provenance is the `dev` branch, not `main`: `dev`
  is where the Vogt MCP registration and the ContextKeeper work already
  live, and merging `main` would mean re-doing them.
- **The front door** (NFR-D11): the Rust engine is the single listening
  process — PWA, native APIs, WebSocket attach, `/api/vogt` and `/mcp`
  reverse-proxied to vogt-core, and aggregate health. Vogt-core binds
  loopback only. Every port that serves MCP still serves plain HTTP health
  (FR-A7), which is the invariant the proxy is most likely to quietly lose.
- **The dev/prod split** (NFR-D12), branch-shaped before anything else
  lands: `dev` → `:dev` images → the `dev-vogt` stack on Node B; only `main`
  reaches prod. Mobile, voice and push are verifiable nowhere else.
- **Merged CI** (NFR-C6): Rust fmt/clippy/test, web typecheck and APK build
  join the Python suite, under NFR-C3's build-vs-release discipline.
- **Both absence-modes green** (NFR-Q6): the forge-less run, unchanged, and
  a core run with no engine present.
- **The legacy GUI keeps serving** at `/ui-legacy` (FR-U9) so nothing
  regresses while M11 is being built.

**Demo**: one URL serves the PWA, a terminal session, and a Vogt backlog
query. Both suites pass in the merged repository, including the engine-less
core run; a push to `dev` produces a `:dev` image that deploys to the dev
stack, and no push to `dev` moves prod.

### M9 as built — one deviation and five notes

**The deviation: the demo ran against two processes, not against the
stack.** vogt-core and the engine were started by hand on loopback and
driven through the engine's port — the backlog answered at `/api/vogt`,
`/ui-legacy` served the vanilla GUI, `POST /api/sessions` opened a PTY, an
MCP `initialize` completed byte-identically to the core's own answer, and a
work item created through the front door landed in the audit log as
`agent:m9demo` with the reason its caller typed. What had *not* run was the
merged image: no Docker daemon was reachable from the environment this was
built in, so `engine/Dockerfile` parsed, every `COPY` resolved, and nothing
more could honestly be claimed.

**The deviation is closed, on 2026-08-14, and by a runner rather than by
this container.** `build.yml`'s `stack-image` job ran for the first time:
28m53s, `engine/Dockerfile` built with the repository as its context, `vogt
--version` and `mydevenv2-server --help` both run *inside* the candidate
before it was pushed, the digest signed, and `dev` plus `dev-ee18adc`
published to `ghcr.io/thedancingdeveloper-org/vogt-stack` — which
`deploy/vogt-stack.compose.yml` now pins in place of the placeholder digest
of zeros it had carried since this note was written. The riskiest unproven
step, copying uv's standalone CPython between build stages, holds: `vogt
--version` cannot run in that image without it.

What is still unproven is the *stack*, as opposed to the image — the two
processes coming up together under `entrypoint.sh`, the engine finding the
core on loopback, and `/readyz` reporting a core it actually reached. That
needs the compose stack up (`DEPLOYMENT.md` §9.4) and
`scripts/smoke_merged_stack.sh` pointed at it, and it is a deploy, which is
a human act (NFR-D10).

1. **`ReadinessCheck` grew a `fatal` flag, and the core's probe is not
   fatal.** Aggregate health was specified; what it should *do* was not. If
   an absent core made the container unready, a healthcheck would restart
   the engine — which cannot revive the core and would kill every live PTY
   doing it, the exact cost FR-E9 says an absent core must never have. So
   the four engine-owned checks keep the verdict and the core's outage is
   reported beside them. Do not "fix" a red `vogt_core` check by making it
   fatal.

2. **Supervision is the entrypoint, not a framework.** s6-overlay and
   supervisord were both weighed and rejected: this container is a
   development pod that already sequences optional daemons in an ordered
   script, runs as `sprooty` rather than root, and gets tini as PID 1 from
   the compose. A second init system would have displaced that tini, put
   PID 1 back to root, and split startup order across two places — for two
   long-lived processes. The core is a backoff respawn loop; the engine
   stays what `exec` replaces the shell with, so the container's lifetime is
   the front door's lifetime. The core's listen address is *derived from*
   `VOGT_CORE_URL`, so "loopback only, never published" is enforced rather
   than commented, and §5.2's two-service fallback still works unchanged.

3. **`vogt init` now runs before every `serve`.** `DEPLOYMENT.md` §5
   records that nothing migrates — `serve` does not, and there is no `vogt
   migrate` — leaving a manual "run `vogt init` after a digest bump" step
   that a deploy would eventually skip. The supervisor runs it in the retry
   loop. The gap in the *product* is still open (FR-L1); what closed is the
   gap in this deployment.

4. **What the merged CI does not carry over from Woodpecker**, each for a
   reason and each written into the workflow: sccache-over-Redis (that
   Redis OOM-crash-looped and blocked the pipeline — `Swatinem/rust-cache`
   replaces it), the Forgejo registry pushes and Komodo deploy steps (Vogt
   does not deploy from CI, NFR-D10), and **APK release signing**, whose
   keystore lives in the retired forge — the APK builds unsigned, and where
   a signed one gets published is an untaken release decision, not an
   oversight.

5. **Three bugs the merge surfaced, two of them older than it.** The CLI
   had been dead on Python 3.13 and later since `serve` grew `--no-auth`:
   `BooleanOptionalAction` refuses an option name starting with `--no-`, at
   parser construction, so *every* command including `--help` raised — and
   the suite never saw it because it ran on 3.11, while `requires-python`
   says 3.11 *or later*. An engine integration test was flaky about one run
   in five. And `/ui-legacy/` — with the trailing slash a browser sends —
   fell through to the PWA's catch-all, because a wildcard route segment
   matches at least one character; it answered 404 with the engine's "web
   bundle not present" placeholder, which reads like a broken front door
   rather than a route that never matched.

**Still open after M9**: the merged image has no TLS (the engine has none;
where the core terminated TLS in-process, the merged port speaks plain HTTP
over WireGuard, and whether Caddy or `tailscale serve` fronts it is the
ingress decision §11.1 leaves open), CI does not yet build
`engine/Dockerfile` with a repository-root context, and the merged image's
registry name is a placeholder awaiting that decision.

## M10 — Coding sessions (M) *(merge-MVP)*

**Objective**: the capability the reversal exists for — a work item can open
a session in its project's tree, and what the agent does in there comes back
as attributed, audited writes.

Delivers FR-E1–E5, FR-E8, FR-E9, FR-S9, FR-S10.

Deliverables:
- **Workspace unification** (FR-E3), the one semantic join: Vogt's import
  root and the engine's workspace root become the same tree, and the project
  registry — not a path heuristic and not a repo-name match — decides where a
  session for a project opens.
- **`session.start` / `session.list` / `session.stop`** in the operation
  registry (FR-E8), and therefore on CLI, REST and MCP with parity, like
  everything else.
- **Work item ↔ session linkage** (FR-E4): the brief written to a prompt file
  through the agent-task mechanism, the session id recorded on the item as an
  audited write, the item's views carrying the session's live activity state
  (FR-E2).
- **First-party MCP registration** (FR-E5) for agents inside a session,
  carrying a **per-session actor-scoped token** minted at start and revoked at
  session end (FR-S10).
- **Auth mapping** (FR-S9): the front door's token namespace maps to named
  Vogt actors whose paired core tokens the proxy injects. The proxy forwards
  and never pre-approves; double-gated writes are unweakened.
- **The engine still boots without the core** (FR-E9) — absence of Vogt costs
  Vogt features, never a terminal.

**Demo**: import a GitHub repository, create a work item on it, start an
agent session from that item, watch the agent update the work item over MCP,
and read the write in the audit log attributed to *that session's* actor —
not to the proxy and not to a shared assistant. Then stop vogt-core and
confirm the terminal is still usable.

### M10 as built — the demo, and what running it found

The demo runs as `tests/test_m10_demo.py` and was also run live, against
vogt-core and the engine as two processes: a work item, a real PTY opened in
the registry's tree, the brief on disk where the engine put it, an agent
inside that terminal posting a comment with the credential its session was
given, and the audit reading
`agent:session:ses_01KZZ1XPW5GMG6NMKG6BV7Q7RE` beside a `session.start` from
`local:sprooty`.

**Running it live is what found the requirement failing.** Three
independent places overwrote a session's token with the pod's shared one,
and every one of them failed silently — the agent authenticated, wrote,
and got a 200, while the audit log recorded the wrong actor:

1. `mydevenv2-agent-auth` fetched the pod's Vogt token unconditionally, and
   with `MYDEVENV2_AUTO_AGENT_AUTH=1` — the deployed setting — it is what
   launches every session's shell.
2. The MCP wrapper Claude Code and OpenCode are registered with re-brokers
   through that same helper before running the bridge.
3. The stdio bridge read only `VOGT_TOKEN_FILE`, which the broker rewrites.

All three now defer to a session's own token when `VOGT_SESSION_ID` is set,
and the bridge states the precedence in one place. Codex was already correct
by accident: it registers the URL natively with `--bearer-token-env-var
VOGT_HTTP_TOKEN` and reads what the session set.

The class of bug is worth naming, because M11–M13 can reintroduce it: **a
credential that is silently replaced produces working writes and a false
audit trail.** Nothing errors, no test that stubs the transport can see it,
and the only signal is an actor name nobody reads until they need to. Two
smaller notes from the same session:

- The engine has **no environment override for `workspace_root`**; it comes
  from the config file, else `$HOME/Working`. An hour went to a session
  refusing to open in a path that was, as far as the operator was
  concerned, configured.
- Vogt sends the registry's **absolute** path as the session's `cwd`, which
  the engine accepts via `resolve_existing_allow_absolute` — the path must
  still resolve inside the engine's workspace root after symlinks. Estates
  mounted somewhere other than that root will be refused, correctly, and
  §6.3's "the import root and the engine's workspace root shall be the same
  tree" is what prevents it.

## M11 — GUI uplift (L)

**Objective**: the Solid PWA becomes the single front end, specified to
interaction depth rather than to a list of views — M6's lesson, applied
before the fact this time.

Delivers FR-U4–U8, FR-U10–U22, NFR-S5, and retires FR-U9's legacy surface
at parity.

Build order, which is part of the deliverable:
1. Board **with its interaction contract** — FR-U10–U12 land *with* FR-U4,
   not after it. A board that renders before it reconciles is a board that
   will be shipped that way.
2. Work item detail (FR-U5, U17, U20).
3. Backlog and bugs (FR-U6, U14, U15).
4. Project pages and the drift inbox (FR-U7, U18).
5. Global surfaces — audit browser (FR-U19), notification inbox, admin.
6. Palette and keyboard pass (FR-U16, U22).
7. Absent-state pass (FR-U21).

The parity rule carries over and gets stronger (FR-U8): every URL in the
shipped bundle resolves against the operation registry *and* the engine's
API contract. r6's rule binds the new surfaces unchanged — a mutating
operation appears only through a view that collects a reason the user typed,
which is why quick-create (FR-U15) will not submit without one and why the
command palette opens that view rather than executing the write.

**Demo**: every operation the legacy GUI exposed is reachable in the PWA; a
board drag round-trips `work.transition`, *including* a rejected transition
rolling back visibly with the server's stated reason; a board URL carrying
filters restores its exact view after reload; killing the engine mid-demo
disables session controls with a named reason while every Vogt view keeps
answering.

### M11 as built — the demo did not run, and what that costs

**The deviation, stated first because it governs everything else: none of
the five surfaces has been rendered in a browser.** This environment has
none. What is proven is structural — they call the operations the registry
serves, they collect a reason before every write, they distinguish an outage
from an empty answer, they build and typecheck — and every one of those is
asserted in `tests/test_pwa.py`. What is *not* proven is anything a person
would notice: no drag has been dragged, no deep link has round-tripped
through the router, no refusal has bounced a card, no layout has been seen.
The M11 demo above is the acceptance test and it is outstanding.

That is also why the legacy GUI is still here. FR-U9's condition — every
operation it exposes is rendered by a PWA surface — was reached, and a test
now asserts it stays reached. Removing a verified front end in favour of an
unverified one is not what the requirement was asking for. `test_pwa.py`
carries the reminder and the order of operations: run the demo, then delete
`src/vogt/gui/`, the `/ui-legacy` routes and both tests together.

1. **Parity was nearly declared by a spelling.** The first parity check
   compared route *tables*: the client listed `notifications` and no surface
   asked for it, and the check said parity was met. Parity now means a
   surface calls the operation. The general form is worth keeping: a
   requirement about what a user can reach, tested against what a client
   could theoretically call, is satisfied by writing a constant.

2. **The client's types were wrong in three ways, and every surface had
   quietly routed around them** — a `swept_at` the server never sends, edge
   pairs where the server sends adjacency, `Record<string, unknown>` where
   the server has always been precise. Each surface had written a defensive
   parser, and each parser hid the mismatch instead of surviving it. The
   types now come from `models.py` and the parsers are gone.

3. **Deep links did not work at all** until the surfaces were integrated:
   routes and tab kinds existed with no branch in the URL→tabs effect, so a
   cold load opened nothing and the tabs only appeared because the shell
   persists them. Activating a tab also dropped its query string, which on
   these surfaces *is* the view. Two surfaces had independently grown a
   workaround for that.

4. **Six columns in the legacy GUI had been em dashes on every row**, found
   while building their replacements: field names that were never right, and
   two columns (`ecosystem`, `constraint`) removed from the product by r2
   along with lockfiles. Nothing failed, because an em dash is also how that
   GUI renders "not collected" — so a typo and an honest absence looked
   identical in the one product whose argument is that you can tell those
   apart. `tests/test_gui.py` now reads the accessors off the models.

5. **Deliberate readings, recorded so they are decisions.** A board drop
   pre-fills the reason with the last one the server accepted; r6 permits it
   — "a form with a required field" — and the alternative considered, one
   armed reason per session, is how fifty audit rows end up saying "triage".
   The board caps rendered cards per column with an explicit "+N more"
   rather than virtualizing, and pages its reads with a truncation banner;
   NFR-S5's "does not fetch the estate to render a page" holds, true
   virtualization does not. Bulk drift accept does not exist, by §3.

**Open after M11**: the browser demo; comments are audited against the
comment rather than the work item, so a per-item audit filter shows creates,
transitions and updates but not comments (a server-side fix, either an
`entity_kind`/parent filter on `audit.list` or a change to what a comment
write audits); and `backlog`/`bugs` have no `offset`, so there is no way to
page past the top 200 of a ranked view.

## M12 — AI layer & voice validation (M–L)

**Objective**: the assistant learns the Vogt domain — and voice, adopted
unproven, is put through its paces rather than assumed.

Delivers FR-T1–T4, FR-T6; FR-T5 validated; FR-T7 attempted.

Deliverables:
- **Registry-derived read tools** (FR-T1): the curated read slice generated
  from the operation registry, not hand-written, so a new operation does not
  mean a new hand-maintained schema.
- **The gated write set** (FR-T2): every mutating tool through the
  pending-action gate — one at a time, exact payload shown, expiring
  unapproved, approved only on screen. Voice never approves
  (`REQUIREMENTS.md` §3).
- **Honest attribution** (FR-T3): an assistant-initiated write is audited to
  the approving user's actor with a `why` from the conversation.
- **Threat-model extension** (FR-T4): Vogt reads are external content by the
  assistant's own rule — issue titles and imported forge text get the same
  untrusted-data delimiting terminal output already gets.
- **Provider cleanup** (FR-T7): a native Anthropic path, or the documented
  `claude-*` hang resolved, or the route refused with a named reason.
- **The voice shakedown** (FR-T5): a deliberate validation pass against
  domain vocabulary — project names, "backlog" — because "it has a mic" is
  not evidence.

**Demo**, by voice on the APK: ask for the top bug, hear the answer, start a
session on it, approve by on-screen tap — and confirm that saying "approve"
does not.

### M12 as built — three notes

1. **The tool schemas are fetched, not written.** The core already generates
   MCP tool schemas from its operation registry, so the assistant asks it —
   `tools/list` over JSON-RPC, `inputSchema` forwarded verbatim — instead of
   keeping a second copy that drifts. Curation is an intersection against a
   named read set, and a curated name the core does not serve is logged and
   skipped rather than invented. What is deliberately *not* taken from the
   core is which tools mutate: that comes from a local write set, so the
   approval gate never depends on a remote answer.

2. **The credential is the approver's, not the proposer's.** FR-T3 says an
   assistant write is audited to the approving user's actor, and the only way
   that is true by construction is to take the front-door pairing from the
   request that pressed approve. A write has no shared fallback at all: an
   unpaired approver is refused by token name, because falling back to a
   deployment-wide actor is exactly the "shared assistant actor" the
   requirement forbids.

3. **Voice is still unproven, and is now written down as such.** FR-T5 asks
   for a validation pass against domain vocabulary before v2 ships; it needs
   a device and a microphone, and neither exists here. What was done instead
   is smaller and honest: the prompt states that items are `WI-7` and
   projects are slugs, which is what a recognizer's output has to survive.
   FR-T7 was not attempted — the `claude-*` proxy hang is unexplained and the
   loop is OpenAI-compatible only.

## M13 — Mobile MVP1 (S–M)

**Objective**: the phone becomes a real surface — the one where an agent
waiting for input actually reaches you.

Delivers FR-M1–M3, FR-E6, FR-E7.

Deliverables:
- The Capacitor shell repointed at the merged PWA (FR-M1), APK CI on the dev
  stream, phone-width pass with the board rendering as a list below the
  narrow breakpoint (FR-M3).
- **Push routing worth an interruption** (FR-M2): `waiting-for-input`,
  `errored`, new drift, and the agent-task notify hook — and nothing else by
  default, because a notification channel that cries wolf is uninstalled.
- **Session outcomes as observations** (FR-E6): exit code, duration, and the
  working-tree delta the session left behind, carrying freshness and trust
  like all other evidence.
- **Bound agent tasks** (FR-E7): a scheduled task may name a project or work
  item and file its findings as observations rather than only as a push.

**Demo**, from the phone: receive a push that a session is waiting for input,
open it, unblock it. Then, **not from the phone**: `vogt observations list
--kind session.outcome` (or `--kind agent_task.run`), which is where the
outcome lands and the only place it is readable. The original sentence said
"see the session's outcome land as an observation against the work item", and
no surface can show that: `vogtApi.ts` has no `observations` binding, so no
Vogt surface in the PWA reads the observed store at all, and the item page's
"Collected evidence" panel is the ranking's contributions rather than
evidence. The evidence is real, tested and CLI-only — `REQUIREMENTS.md` §6.2
carries the row (FR-U17's provisional clause) and this line is not allowed to
imply otherwise.

### M13 as built — three notes and a thing that was already wrong

**The APK assembles; nothing was ever installed.** The first version of this
note said there was no Android SDK here. That was wrong — `/opt/android-sdk`,
Gradle 9.7 and Java 21 are in this image, and `cap sync` plus
`./gradlew assembleDebug` produce a 5.5M debug APK whose manifest reads
`application-label:'Vogt'`, `package: com.sprooty.mydevenv2`, pointing at
`vogt.sprooty.com` with cleartext off. So the build is proven and the
configuration's branches are proven against a real Gradle run rather than
only under node.

The build is now also proven where it counts: `the Android shell assembles`
passed on a self-hosted runner on 2026-08-14, in the job gated behind the
engine job, so the first green engine run was also the first APK this
pipeline has ever produced. Local Gradle was validation; the runner is the
build (that is the standing rule for everything here).

What is still unproven is everything after the build: **no APK has been
installed on a device, and no notification has been delivered by either
transport**. The drift path has never run against a live core. Release
signing is also still absent — the keystore lives in the retired forge — so
what CI produces is an unsigned debug artefact.

1. **The shell was pointing at the wrong stack, and it looked like it
   worked.** `capacitor.config.ts` hardcoded `mydevenv2.sprooty.com` — the
   standalone engine, with no vogt-core behind it. An APK built on that
   default reaches a front door where `/api/vogt` answers 503, `vogt.
   configured` is false, and the four Vogt tabs are hidden or in their outage
   state, while terminals and the assistant work perfectly. The build now
   asks for the URL and fails without one, because the merged stack has no
   settled name (§11.1) and NFR-D2 forbids inventing one. `cleartext` follows
   the scheme rather than being hardcoded `false`, which the tailnet's plain
   HTTP had turned into a runtime network error.

2. **FR-M2's "and for nothing else by default" was already false.** Idle-stall
   and agent-task-started notifications both defaulted on, and neither is in
   the set the requirement names. Both default off now; the watchers still
   run and the toggles still exist, and a device that already made a choice
   keeps it. This is the requirement being read as a *list* rather than as a
   direction, which is what it says.

3. **One reader of the core's event cursor, and drift push hangs off it.**
   The follower (M11, FR-U10) already polls `events.list` and republishes each
   change onto the server's bus; the drift watcher subscribes to that bus the
   way the session watcher subscribes to activity, and adds only the filter
   and the fan-out. It fires on `drift.raised` — a named kind, not a `drift.`
   prefix, because "and for nothing else by default" has to survive the core
   growing kinds — and coalesces a ten-second window, so a sweep raising
   thirty proposals is one notification rather than thirty.

   This was built twice. The first attempt was a second poller with its own
   persisted cursor, which is strictly more machinery for the same
   notification and was replaced. What the surviving design costs is stated
   rather than hidden: the follower's cursor is in memory, so drift raised
   while the engine was down is never notified. A redeploy is a hole in the
   stream. The proposal is not lost — it stays open in the inbox until
   somebody rules on it — so what is missed is the interruption, not the work,
   and the alternative buys that back at the price of a phone that can replay
   an estate's history after a restart. A missed buzz is recoverable by
   opening the app; a channel someone switched off is not.

**The breakpoint the surfaces had all missed**: the shell goes to phone
layout at 768px and each of the five Vogt surfaces had picked 900, leaving a
band where the shell was a phone and the surface inside it was not. Phone
work is at 768 now, and a test fails if a sixth breakpoint narrower than
that appears.

## M14 — Consolidation (S, ongoing)

**Objective**: finish the merge rather than leave it half-standing. Delivers
no new requirement ID, which is the point.

Deliverables:
- The standalone stacks retired (`prod/dev-mydevenv2`, `personal/vogt`) once
  the merged stack has carried the load.
- The name and domain decision (`MERGE_MYDEVENV2.md` §11.1), and the sunset
  of the `MYDEVENV2_*` config aliases after their transition period.
- **The r9 as-built reconciliation**: `REQUIREMENTS.md` and `DESIGN.md`
  brought back against what was actually built, in the usual style —
  including a §5-style delivery verification of every ID r9 added, by §5.5's
  method and §5.4a's per-conjunct rule. *(Done: `REQUIREMENTS.md` §6, and
  `DESIGN.md` §4's merged-tree section.)*
- **NFR-I6** — backup and restore covering the whole product as one act: the
  core's SQLite, the engine's `state_dir`, and enough metadata to
  re-establish FR-E3's path agreement after a restore. *(Built. It had
  appeared in no milestone's `Delivers` line at all, which is why nobody had
  built it — a §4 traceability failure the per-conjunct audit found. It
  lands here because retiring the standalone stacks is exactly when somebody
  needs to restore one.)*

**Demo**: there is none, and that is honest — the acceptance test for this
stage is `REQUIREMENTS.md` §5 agreeing with the build, which is the same bar
v1 was held to.

### M14 as built, so far — what the pipeline settled

**The merge reached `main` on 2026-08-14**, 328 commits fast-forwarded after
a green `dev`, which is NFR-D12's route taken rather than described. Both
image streams exist and are signed: `dev`/`dev-<sha>` on the dev branch and
`sha-<commit>` from main, in `vogt-stack` beside the core-only `vogt`.

Three things worth recording, because none of them was visible before CI ran
for the first time and all three had been asserted in prose:

1. **Two first-run failures, both checks written against this machine.**
   `docs` failed on a link that resolved here because the file exists on this
   filesystem; `check_docs.py` now rejects absolute paths outright. The engine
   job failed on `sudo apt-get` with *root is not in the sudoers file* — the
   runner is root, so there was nothing to elevate from. Neither was a code
   defect and neither was findable by reading. `REQUIREMENTS.md` §6.3 finding
   16.
2. **"Needs hardware" was wrong, and is retired from the documents.** It
   described this container. The runners have the Docker daemon and the
   Android SDK it lacks, and on the same day they produced the merged image
   and an assembled APK. What genuinely remains needs a phone, a speaker, a
   browser with a layout engine, and somebody choosing to deploy.
3. **The APK assembles in CI**, in the job gated behind the engine job that
   was failing — so the first green engine run was also the first APK the
   pipeline has ever built. It still points at `127.0.0.1:8910` and is still
   signed with Gradle's debug key.
4. **A cancelled run was a check that never ran.** Path gating classifies a
   push by `before..sha`, so each commit is examined by exactly one run — and
   `cancel-in-progress` was true for every trigger, so pushing twice within a
   few minutes cancelled the first run and the next run's range began after
   it. A lint error reached `dev` and sat there green; it was found by
   running `ruff` by hand. Cancellation is now pull-requests-only, whose runs
   classify against the merge base. `REQUIREMENTS.md` §6.3 finding 19.

Still open, and each is somebody's decision rather than unfinished work: the
standalone stacks are retired only after the merged stack carries load
(§9.5); the `MYDEVENV2_*` aliases sunset after their transition period; and
the APK keystore lives in the retired forge, so where a signed APK is
published is an untaken decision.

---

## Revision r5 — a build is not a release

NFR-C3 said a push to main shall never publish an image, so the only way to
obtain a deployable artefact was to tag a version. Within one afternoon that
produced v0.1.0, v0.1.1 and v0.1.2 — none of which marked a release. Two were
repairs to an image that had never been executed, and the third was a uid
change that concerns one deployment and no user of this software.

Version numbers had become a build counter, which is the failure mode where a
number that should mean "this is what changed for you" comes to mean "the
pipeline ran again".

The requirement was conflating two acts that only look alike:

| | Release | Build |
|---|---|---|
| Trigger | `v*` tag | push to main |
| Image tags | `0.1.2`, `0.1`, `latest` | `sha-<commit>` |
| Wheel, SBOM attestation | yes | image SBOM only |
| Signed | yes | yes |
| Says "use this" | yes | no |

What NFR-C3 was protecting is intact. Merging still cannot cut a release: no
semver tag, no `latest`, no wheel. Merging still cannot deploy: production
moves only when a digest is pinned in `indexarr/ops` and `DeployStack` runs
(NFR-D10). What it loses is the accidental coupling that made a version bump
the price of a hotfix.

Commit builds are signed like releases. An unsigned artefact that can reach
production would be a wider hole than the one this closes.
