# Vogt — Deliverable Stages (v0.3, revision r4)

Status: **M0–M6 delivered — v1 is built** (2026-08-12). Requirement IDs
refer to `REQUIREMENTS.md`; per its §4, scope changes here must update that
document in the same change.

Each stage below carries an "as built" note recording where the delivery
differed from the sketch. What those notes cannot say is whether the set of
them adds up to the requirements baseline — that is `REQUIREMENTS.md` §5,
written after v1 by checking the build against every ID. It found four
requirements short of their text (FR-L1, NFR-I3, FR-S6, NFR-S4) and one CI
gate that does not fire on the paths most likely to trip it (NFR-Q4).
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

Deferred and withdrawn requirement IDs (FR-G2, FR-G5–G10, FR-D7) appear in
no stage by design — see `REQUIREMENTS.md` §3.

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
  docs-skip path filtering and gate-job pattern.
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
- `release.yml` completes: buildx image, syft SBOM, keyless cosign sign +
  attest, push to `ghcr.io/thedancingdeveloper-org/vogt`, tag-triggered
  only, on `[self-hosted, node-b, linux, x64, docker, publish]`.
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
