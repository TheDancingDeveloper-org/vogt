# Vogt — Requirements (v0.3)

Status: **baseline, revision r7** (2026-08-13), distilled from `DESIGN.md`,
`SCHEMA.md`, `DEPLOYMENT.md` and the originating product discussion.
**v1 (M0–M6) is built**; §5 is the requirement-by-requirement verification
of the delivered system against this document, including the four
requirements that are not fully met.

Priority key (MoSCoW): **M** = must have (v1 is not shippable without it) ·
**S** = should have (v1 target, degradable) · **C** = could have (explicitly
designed-for, may slip past v1). Deferred items are in §3.

**v1 = M0–M6** (all stages in `ROADMAP.md`). **MVP = M0–M2** — the first
build that is daily-usable. "M" priority means required for v1, not
required for MVP; the roadmap says which stage delivers each ID.

Requirements use *shall*; each is intended to be testable. Source column
points at the governing design section.

### Revision r2 — what changed and why

Five scope decisions, taken after a whole-proposal review:

1. **Gates are a status, not an enforcement mechanism.** The contract is
   checked and reported as a per-project value; nothing in the system
   blocks, refuses, or relocates on the basis of it. This withdraws
   FR-G2, FR-G9, FR-G10.
2. **Dependency tracking records references, not resolved versions.**
   Edges between projects by path or repo URL; no manifest/lockfile
   version resolution, no ecosystem package identity. This rewrites
   FR-D1–D5 and withdraws FR-D7.
3. **MVP line moves to M2**, and read-only GitHub collectors move
   *into* M2 so the MVP runs against the real backlog (FR-O5 split).
4. **Observed-first gains a suppression model** (FR-W10, FR-W11,
   FR-G12) — without it the ranked views drown in unfiltered markers.
5. **Two modelling holes closed**: the `/events` cursor (FR-N1) and
   drift-evidence survival against retention pruning (FR-R5).

### Revision r3 — no discovery, no continuous checking

One further decision: **the system never goes looking.** Collection scope
is the list of projects the user explicitly registered (FR-G15); there is
no crawling of roots, no candidate listing, and no scheduled re-checking of
contract compliance. The contract is evaluated when someone asks
(FR-G4/FR-G14), and the answer is reported with its age like every other
value in the system.

This defers FR-G5–FR-G8 and rescopes FR-G12 from sweep-root exclusions to
per-project ones. The non-enforcement rule that FR-G7 carried is retained
verbatim as FR-G13, because it is a principle, not a mechanism.

The intended future direction — explicitly a **non-committed stretch
goal**, listed in §3 and designed for by nobody — is that drift detection
and recommendation eventually become an AI integration reading the
observation store, rather than a larger scheduler. Nothing in v1 assumes
it, and no v1 requirement may be justified by it.

Per §4, IDs are never renumbered or reused: withdrawn and deferred
requirements stay in place marked with the revision that moved them, and
replacements append.

### Revision r4 — the target deployment state is named

The deployment target was previously abstract ("a tailnet server"). It is
now concrete, and the requirements say so: **Vogt runs as a Docker Compose
stack on Node B (`winrarhost`), deployed by Komodo from the `indexarr/ops`
GitOps repository, from a signed image published to GHCR by tag-triggered
GitHub Actions on self-hosted runners** (NFR-D7–D10, NFR-C4).

Three existing requirements change as a consequence, revised in place:

1. **NFR-D2 splits.** "No default host/port anywhere" was one rule doing
   two jobs. Values that encode *exposure or identity* still have no
   defaults; values that are pure *host allocation* (a tailnet-bound port,
   an operator-owned certificate path) now require concrete defaults. The
   literal rule cost cadastre every deploy from `cadastre#42` on —
   `DEPLOYMENT.md` §4.1 records the incident.
2. **NFR-D6 changes mechanism.** TLS is terminated **in-process** from the
   host's Tailscale-issued certificate. No fronting Caddy, no
   `tailscale serve`, no entry in Node B's Caddyfile — that Caddy is host
   infrastructure rather than a Komodo stack, and a tailnet-only service
   has no reason to couple to it.
3. **NFR-PO4 gains the real distribution path.** The OCI image is GHCR,
   signed keylessly, and consumed by a Komodo stack — not a generic
   "signed OCI image with compose file".

This changes no functional requirement, no stage boundary, and nothing
about the forge-optional core: NFR-PO1–PO3 are untouched, and the product
still self-hosts anywhere Docker runs. Node B is *this estate's*
deployment, specified so that M4 has a target instead of a shape.

### Revision r5 — a build is not a release

One requirement changes: **NFR-C3**, revised in place. It had said a push to
main shall never publish an image, so the only way to obtain a deployable
artefact was to tag a version — which produced v0.1.0, v0.1.1 and v0.1.2 in
one afternoon, none of them marking a release. Two were repairs to an image
that had never been executed; the third was a uid change concerning one
deployment and no user of this software. The version number had become a
build counter, which is the failure where a number that should mean "this is
what changed for you" comes to mean "the pipeline ran again".

The requirement was conflating two acts that only look alike. A **release**
is tag-triggered and says *use this*: semver image tags, `latest`, the
wheel, the SBOM attestation. A **build** is a push to main and says nothing:
`sha-<commit>` only, no semver, `latest` unmoved, no wheel. Both are signed
— commit images are the artefacts that actually reach production between
releases, so signing only releases would put the strongest guarantee on the
thing that ships least often.

What NFR-C3 protected is intact and asserted by tests in `test_deploy.py`:
merging cannot cut a release, and merging cannot deploy (NFR-D10 —
production moves when a digest is pinned in `indexarr/ops` and `DeployStack`
runs). What it loses is the accidental coupling that made a version bump the
price of a hotfix. `.github/workflows/build.yml` implements the build half;
`release.yml` keeps the release half unchanged.

*(This summary is required by §4 and was missing when NFR-C3 was marked
`revised r5` — added during the delivery verification in §5.)*

### Revision r6 — onboarding from the forge, and the forge's inbox

Two additions, both post-v1 (M7 in `ROADMAP.md`), neither changing an
existing requirement.

**1. A repository on GitHub is onboarded by importing it, not by finding it
locally.** Until now the only ways to get a project were `project.register`
(point at a folder you already have) and `project.create` (scaffold a new
skeleton). Both assume the working tree already exists and is authoritative
for its own provenance. For a repository that lives on GitHub that is the
wrong starting point: it makes Vogt reconcile two sources — a local tree of
unknown ancestry and a remote — from the first sweep, when one of them is
plainly derived from the other. FR-P6/FR-P7 add import: clone the named
repository into the configured import root, register it, and consolidate its
existing forge state (FR-B3) in one operation.

This is not discovery, and must not become it. FR-G15 is untouched: the user
names the repository. Listing an account's repositories to choose from is a
registration-candidate listing — deferred by r3 and deferred again here (§3),
because the GUI form is exactly where it would reappear as a convenience.

**2. GitHub notifications are observations, not events.** The `/events` feed
(FR-N1) is Vogt's own audit-backed feed of what happened *here*; FR-N2 keeps
push out of scope, and neither changes. What GitHub calls a notification is
upstream evidence about a repository, so it is collected like any other
evidence (FR-O8) and read through its own view (FR-N3). Scoping the view to
onboarded repositories is not a filter but a consequence: collection scope is
the registered project list, so the per-repository endpoint can only ever
return notifications for projects that exist here.

**One earlier decision is narrowed.** M6 shipped the GUI read-only, on the
grounds that a write needs a reason its author typed and a button cannot type
one (FR-W1). FR-U3's import form can, so the rule is restated rather than
abandoned: a mutating operation may appear in the GUI only through a view
that collects a reason the user typed. Import qualifies; resolving drift from
a list does not, and remains out. No requirement changes — FR-U1 and FR-W1
are untouched — but the reasoning M6 recorded was broader than the rule it
was protecting, and `ROADMAP.md` M7 names the difference.

Two limits are stated up front rather than discovered later. Notifications
belong to the *token's* account, so the inbox is instance-scoped, not
per-actor — per-actor inboxes need per-actor tokens and are deferred.
And nothing is marked read upstream: that is a mutation of somebody else's
inbox, and it belongs behind the write-back policy (FR-B1) if it is ever
built. It is not built.

### Revision r7 — a client that can connect

**A requirement is added, and one is corrected.** The trigger was an ordinary
question — how does a client install Vogt's MCP server? — which had no
answer, and no requirement whose failure would have said so.

**1. FR-A8 is new.** FR-A5 requires the stdio bridge to *exist* and to
discover the remote tool list at startup. It does. Nothing required that a
client could *obtain* it, or that an instance could say where it is. The
verification in §5 could not have caught this: it walks requirement IDs
looking for ones nothing implements, and there was no ID here to walk.

Three items each recorded a fragment of the same hole and each was filed as
minor. §5.1 lists NFR-D3 as *vacuously satisfied* — "Vogt ships no
client-setup script". §5.1 lists `DEPLOYMENT.md` §4.3's `CONNECTING.md` as
half-built, and explicitly notes it is "not a numbered requirement". NFR-PO4
promised `uv tool install` against no index and went unlisted entirely.
Individually: a rule with nothing to bind, a document nobody generated, a
conjunct nobody checked. Together: a product that could not be connected to,
with no failing test and no failing requirement anywhere.

The missing fact turned out to be a single one. `/connection-info` reported
every path a client needs and not the URL — because the server genuinely
cannot know it: it binds `0.0.0.0:8000` in a container and is published at a
tailnet address on another port. That makes the URL an **exposure** value,
which under NFR-D2 must be configured and must never carry a default. So
`public_url` has none, and an instance without one reports that nobody has
said rather than inventing an answer. A URL the server guessed would be
wrong in exactly the deployment the field exists for, and from a client a
wrong URL and an unreachable one look the same.

**2. NFR-PO4 is corrected, not extended.** It named two install paths joined
by "and": `uv tool install`, and the OCI image. The image half shipped at M4,
signed and digest-pinned. The wheel half never had an index behind it — no
PyPI project, no release asset, the wheel reachable only as an expiring CI
artifact — and M4 claimed the requirement as delivered.

The correction is deliberately *not* to build a private index. Vogt is
private today and public later (NFR-O1); an index built for the interim
would have one user and a migration to undo. The image is the supported
install path, PyPI is the path at the public milestone, and neither is
needed to reach a running instance — which is FR-A8's point.

**3. §5.4 gains the method note.** The delivery verification finds
requirements that nothing implements. It cannot find a requirement whose
implementation satisfies half its text, because one citation makes the ID
look answered. Requirements joined by "and" shall be verified per conjunct.

---

## 1. Functional requirements

### FR-P — Projects & per-repo view

| ID | Requirement | Pri | Source |
|---|---|---|---|
| FR-P1 | The system shall register projects rooted in a folder or git repository, holding: name, root path, lifecycle state, current version, contract version, compliance status. | M | DESIGN §3.1 |
| FR-P2 | The system shall provide a per-project "brief": state, backlog, open bugs, current version, CI status, contract compliance, dependency summary — in one call. | M | DESIGN §4.2 |
| FR-P3 | The system shall derive a project's current version from tags/releases/manifest observations, and report declared-vs-observed version mismatch as drift. | M | SCHEMA §2.4 |
| FR-P4 | The system shall support project lifecycle states (`incubating / active / maintenance / archived`) with validated transitions. | S | DESIGN §3.1 |
| FR-P5 | *(r2, r3)* A project shall be exactly one explicitly registered repository or folder. Members of a multi-package workspace shall not be separate projects; a standalone repository mirroring a workspace member is its own project, and the relationship is reported (FR-D8), never reconciled. | M | DESIGN §3.1 |
| FR-P6 | *(r6)* The system shall import a GitHub repository named explicitly by the caller (`owner/name` or a repository URL): clone it into the configured import root, register the result as a project with its `repo_url` set, and — unless the caller declines — consolidate its existing forge state (FR-B3) in the same operation. The clone shall be written before the declared write, so a failed registration leaves a checkout and not a project pointing at nothing. Import shall never list, search, or suggest repositories (FR-G15). | S | DESIGN §4 |
| FR-P7 | *(r6)* Import shall be non-destructive at its destination. A destination that does not exist is created; a destination that is already a clone of the same remote is registered as-is rather than re-cloned; any other occupied destination shall fail without modifying it. | S | DESIGN §4 |

### FR-G — Project contract & compliance status

*r2: this section previously specified gate enforcement. The contract is
now evaluated and reported; it never blocks an operation.*
*r3: evaluation is on demand only — no scheduled re-checking, no
filesystem discovery.*

| ID | Requirement | Pri | Source |
|---|---|---|---|
| FR-G1 | The system shall hold a declarative project contract (required files, required directories, required metadata), carrying a version identifier, sourced from configuration. | M | DESIGN §5 |
| FR-G2 | ~~Project creation in a controlled location shall be blocked unless the contract is satisfied.~~ **Withdrawn (r2)** — enforcement removed; superseded by FR-G11. | — | — |
| FR-G3 | Every contract evaluation shall return the specific rules evaluated and the specific criteria that failed — never a bare pass/fail. | M | DESIGN §5 |
| FR-G4 | A contract check shall be invocable on demand (`contract check`) against any candidate folder/repo, without registering it. | M | DESIGN §4.2 |
| FR-G5 | ~~Registered projects shall be re-checked every sweep, yielding a per-project compliance status.~~ **Deferred (r3)** — continuous re-checking is a scheduled-discovery mechanism the product does not need to earn its keep. Superseded for v1 by FR-G14 (on-demand check, recorded with its age). | — | §3 |
| FR-G6 | ~~Folders under a sweep root that are not registered projects shall be listed as registration candidates.~~ **Deferred (r3)** — requires filesystem discovery; see FR-G15. | — | §3 |
| FR-G7 | ~~Compliance status shall be computed for every registered project regardless of location.~~ **Deferred (r3)** — the automatic-computation half defers with FR-G5; the non-enforcement half is retained and restated as FR-G13. | — | §3 |
| FR-G8 | ~~Sweep roots shall be an explicitly configured list of paths, defining where collection looks for projects.~~ **Deferred (r3)** — the concept exists only to support discovery. Collection scope is the registered project list (FR-G15). | — | §3 |
| FR-G9 | ~~`project migrate`: gate check → scaffold → relocate into a controlled root → register.~~ **Withdrawn (r2)** — with no entry gate there is no reason for the system to relocate projects; moving a project remains a manual operation the user performs, after which they update its registered path. | — | — |
| FR-G10 | ~~The global view shall distinguish stack (gated) from legacy (observed-only) projects.~~ **Withdrawn (r2)** — the two-tier estate model is replaced by a single compliance status on every project (FR-G5). | — | — |
| FR-G11 | *(r2)* `project create` shall scaffold a contract-compliant skeleton. Registering an existing non-compliant folder shall succeed, reporting its status; registration shall never be refused on contract grounds. | M | DESIGN §5 |
| FR-G12 | *(revised r3)* Each registered project shall carry exclusion patterns applied to collection within it (default: `.venv/`, `node_modules/`, `target/`, `dist/`, `build/`, `.git/`); excluded paths shall yield no observations. | M | DESIGN §3.6 |
| FR-G13 | *(r3)* No component of the system shall consume contract compliance, trust state, or drift status as a precondition for any operation. These are values to be read. | M | DESIGN §2.1 |
| FR-G14 | *(r3)* The result of an on-demand contract check against a registered project shall be recorded as that project's compliance status (`compliant / non_compliant / not_checked`) with failing criteria and a checked-at timestamp. The value shall always be reported with its age and shall never be refreshed implicitly. | M | DESIGN §5 |
| FR-G15 | *(r3)* Collection scope shall be the explicitly registered project list. The system shall not discover projects by crawling the filesystem, and shall not maintain a candidate or unregistered-folder listing. | M | DESIGN §5.1 |

### FR-W — Work management (the write plane)

| ID | Requirement | Pri | Source |
|---|---|---|---|
| FR-W1 | The system shall support creating, updating, and closing work items (kinds: feature / bug / chore / question) with title, body, state, priority p0–p4, effort, project, blockers, assignee. | M | DESIGN §3.1 |
| FR-W2 | Work-item state shall be governed by a configurable per-kind state machine; invalid transitions shall be rejected with the violated rule named. | M | DESIGN §3.3 |
| FR-W3 | The system shall support initiatives (cross-project epics) with weight, and blocking relationships between work items. | M | DESIGN §3.1 |
| FR-W4 | Observed work (forge issues/PRs, promoted markers) shall be visible in backlog/bug views immediately, without prior declaration (**observed-first**), subject to FR-W10 and FR-W11. | M | DESIGN §2 |
| FR-W5 | The system shall support `adopt`: promoting an observed item into a declared work item with a maintained link to its origin. | M | SCHEMA §4 |
| FR-W6 | The system shall support comments on work items, attributed to an actor. | S | DESIGN §3.1 |
| FR-W7 | Both humans and agents shall be first-class actors: assignable, attributable, and auditable identically. | M | DESIGN §3.1 |
| FR-W8 | Work items shall support typed, cross-project relations: `depends_on` (blocks completion, feeds ranking), `relates_to`, `duplicate_of`, `parent_of` — semantically aligned with GitHub issue relations. | M | DESIGN §3.1 |
| FR-W9 | Work items shall support labels/tags (name + optional color), filterable in every view, aligned with GitHub labels: observed upstream labels are captured, preserved on `adopt`, and synced by name when write-back is enabled. | M | DESIGN §3.1 |
| FR-W10 | *(r2)* An authorised actor shall be able to **suppress** an observed subject, by `subject_key` or by pattern, as an audited write carrying a reason. Suppressed subjects shall remain observable and queryable but shall be excluded from ranked and aggregated views, and shall stay suppressed across re-observation. | M | DESIGN §3.6 |
| FR-W11 | *(r2)* Only source markers matching a configured promotion pattern (default: `TODO(vogt)` / `FIXME(vogt)`) shall enter backlog/bug views. All other markers shall be observed and queryable but not promoted. The set of file types scanned shall be configuration, not hard-coded. | M | DESIGN §3.6 |

### FR-V — Views & ranking

| ID | Requirement | Pri | Source |
|---|---|---|---|
| FR-V1 | The system shall provide a global backlog and a global open-bugs view across all projects, filterable by project, kind, state, priority, assignee, label, trust. | M | DESIGN §4.2 |
| FR-V2 | Global backlog ordering shall be deterministic, from documented constant-weight inputs. | M | DESIGN §3.4 |
| FR-V3 | `why <item>` shall return the per-input score contributions for any ranked item. | M | DESIGN §3.4 |
| FR-V4 | Every aggregating answer (brief, backlog, bugs, deps) shall carry the freshness of the underlying sweeps (oldest relevant sweep timestamp). | M | DESIGN §6 |

### FR-O — Observation & collection

| ID | Requirement | Pri | Source |
|---|---|---|---|
| FR-O1 | Collectors shall be plugins; core collectors (`git-local`, `source-markers`, `dep-refs`, `contract-checker`) shall require no network and shall operate only over registered projects (FR-G15). `contract-checker` runs on demand only. | M | DESIGN §1.1 |
| FR-O2 | The observation store shall be append-only; collectors shall never mutate declared data. | M | SCHEMA §1 |
| FR-O3 | Every sweep shall record its collector, scope, time window, and outcome (coverage records). | M | SCHEMA §3.1 |
| FR-O4 | Absence shall only be asserted within provably swept scope; otherwise the answer is "not collected", and partial coverage shall be disclosed in responses, never silently returned as complete. | M | DESIGN §6 |
| FR-O5 | *(split r2)* **(a)** Read-only GitHub collectors — issues, PRs, Actions runs, releases/tags — shall be available from M2, so observed-first views cover forge-hosted work. **(b)** Historical backfill/consolidation and per-toggle update-automation posture land with the forge module at M5. | M* | DESIGN §3.5, §4 |
| FR-O6 | CI status shall be modeled as generic per-revision check observations; GitHub Actions is one producer, not the model. | M | DESIGN §1.1 |
| FR-O7 | Unchanged observations (same subject, same content digest) shall not grow the store; sweeps shall count them as unchanged. | S | SCHEMA §3.1 |
| FR-O8 | *(r6)* GitHub notifications shall be collected as observations through the **per-repository** endpoint, so that collection scope remains the registered project list (FR-G15) by construction rather than by filtering. They shall not be promoted into ranked views (DESIGN §3.6): a notification is a signal that something happened, not a claim that there is work. A token lacking the notifications scope shall degrade to `partial` coverage like any other unavailable source (FR-O4), never to a failed sweep. | S* | DESIGN §3.5 |

\* M *when the GitHub adapter is enabled*; the adapter itself is optional (NFR-PO1).

### FR-D — Dependency references

*r2: this section previously specified version-resolved, multi-ecosystem
dependency tracking. It now records **which projects reference which**,
by path or repository URL, and stops there.*

| ID | Requirement | Pri | Source |
|---|---|---|---|
| FR-D1 | *(revised r2)* The system shall record dependency edges between projects, each identified by filesystem path or repository URL. It shall not parse lockfiles and shall not resolve package versions. | M | DESIGN §3.5 |
| FR-D2 | *(revised r2)* Each edge shall record its reference kind (`path / git / declared`) and, when observed, the manifest file it was read from. Vendored path references therefore stay distinguishable from repository references. | M | DESIGN §3.5 |
| FR-D3 | *(revised r2)* Path and repository references shall resolve to the owning registered project where one exists; unresolved references shall be retained with their raw target. | M | DESIGN §3.5 |
| FR-D4 | *(revised r2)* The system shall answer reverse lookups: all registered projects that reference a given project. | M | DESIGN §4.2 |
| FR-D5 | *(revised r2)* Dependency reporting shall include one drift kind: `unresolved_dependency` — an internal-looking reference whose target is not a registered project. | S | SCHEMA §2.4 |
| FR-D6 | *(moved r2)* Update-automation posture shall be tracked as independent facts (version-updates config / vulnerability alerts / security fixes), never one boolean. This is forge posture, delivered with the forge module (FR-O5b), not with dependency references. | M* | DESIGN §3.5 |
| FR-D7 | ~~Advisory/vulnerability enrichment (RustSec, OSV, GitHub advisories).~~ **Withdrawn (r2)** — advisory matching requires resolved versions, which r2 removes. Deferred to §3. | — | — |
| FR-D8 | *(r2)* Where the same source exists both as a path member of one project and as a separate registered project, the system shall report the `mirrored_source` relationship as an observation. It shall not compare contents and shall not assert divergence. | C | DESIGN §3.5 |

### FR-R — Drift & reconciliation

| ID | Requirement | Pri | Source |
|---|---|---|---|
| FR-R1 | The drift engine shall compare declared state against observations and raise typed drift proposals with linked evidence. | M | SCHEMA §2.4 |
| FR-R2 | Drift shall never silently mutate declared data; proposals require explicit accept / reject / contest by an authorized actor. | M | DESIGN §3.2 |
| FR-R3 | Auto-accept rules shall be configurable per project and per drift kind; the shipped default is low-risk auto-accept (state-sync kinds agent-acceptable; destructive/structural kinds always human-gated). | M | DESIGN §3.2 |
| FR-R4 | Every declared entity shall carry a computed trust state: `verified / stale / unverified / disputed` — derived from observation freshness and agreement, never hand-set. `disputed` is distinct from the drift *resolution* status `contested`, which is chosen by an actor. | M | SCHEMA §4 |
| FR-R5 | *(r2)* A drift proposal shall embed a self-contained evidence snapshot at raise time, and observations referenced by a proposal shall be exempt from retention pruning while that reference exists. Evidence shall never become unreachable through retention. | M | SCHEMA §2.4, §5 |

### FR-A — API surfaces & parity

| ID | Requirement | Pri | Source |
|---|---|---|---|
| FR-A1 | Every capability shall be available via REST, CLI, and MCP; the GUI shall consume the same REST surface. Nothing shall be GUI-only. | M | DESIGN §2 |
| FR-A2 | All surfaces shall be generated from one transport-neutral operation registry (name, scope, mutating flag, argument schema, route) — including the MCP stdio bridge. | M | DESIGN §4.1 |
| FR-A3 | Transport parity shall be enforced by tests whose exclusions are explicit named lists (`HTTP_ONLY`, `LOCAL_ONLY`) that fail when stale in either direction. | M | DESIGN §4.1 |
| FR-A4 | The REST API shall publish a generated OpenAPI document. | M | DESIGN §4 |
| FR-A5 | MCP shall be served over stdio (local, no server required) and streamable HTTP at `/mcp`; a stdio bridge shall serve clients that can only spawn local processes, discovering the remote tool list at startup rather than hardcoding it. | M | DESIGN §4.1 |
| FR-A6 | Unsupported MCP protocol versions shall be refused with the supported list named; bridge↔server version skew shall warn on stderr and never block startup. | M | DESIGN §4.1 |
| FR-A7 | The server shall expose plain-HTTP `/health/live`, `/health/ready`, `/version`, and `/connection-info` on every port that serves MCP. | M | DEPLOY §1 |
| FR-A8 | *(r7)* **Connecting a client shall be a capability of the product, not an exercise for the reader.** The instance shall state the URL clients reach it at — configured, never inferred, and reported as absent when unset rather than guessed — and shall render client configuration from it (`connect`). A client that speaks streamable HTTP shall require no Vogt code installed; installation shall be a property of the stdio bridge alone, and shall be stated as its cost rather than assumed. | M | DEPLOY §4.3 |

### FR-S — Security, identity & audit

| ID | Requirement | Pri | Source |
|---|---|---|---|
| FR-S1 | Every declared write shall record, in the same transaction: actor, operation, entity, caller-supplied reason, transaction id, monotonic revision, timestamp. | M | SCHEMA §2.1 |
| FR-S2 | The acting principal shall be derived from authentication only (token / mTLS / trusted proxy / local OS user) and shall never be a caller-suppliable field. | M | DESIGN §4.1 |
| FR-S3 | Authorization shall use scoped tokens (`read`, `work.write`, `project.write`, `admin`, `writeback`) bound to actors. | M | DEPLOY §3 |
| FR-S4 | Write operations shall be double-gated: server started with writes enabled AND principal holds the scope — checked at both tool listing and invocation. Ungranted tools shall be invisible, not erroring. | M | DESIGN §4.1 |
| FR-S5 | Both allow and deny authorization decisions shall be audited. | M | DESIGN §4.1 |
| FR-S6 | The audit log shall be queryable through the API (filter by actor, entity, operation, time). | M | DESIGN §4.2 |
| FR-S7 | Secrets (tokens) shall be supplied via file reference, never argv or URL. | M | DEPLOY §2.2 |
| FR-S8 | *(r6)* Credentials used to clone a repository (FR-P6) shall be supplied to `git` out of band — never in the remote URL, never in argv, and never written into the clone's own configuration. A project's stored `repo_url` shall never contain credentials. | M* | DEPLOY §2.2 |

### FR-B — Forge write-back (optional module)

| ID | Requirement | Pri | Source |
|---|---|---|---|
| FR-B1 | Write-back to GitHub shall be governed per project: `none / comment_only / full`, default `none`. | M* | DESIGN §4 |
| FR-B2 | Every write-back action shall be audited locally and re-observed on the next sweep (the system observes its own writes like anyone else's). | M* | DESIGN §4 |
| FR-B3 | Onboarding a repo with existing GitHub state shall be a read-only consolidation: full backfill of issues, PRs, labels, releases, and CI history as observations. No GitHub mutation shall occur during onboarding; existing GitHub objects are authoritative for themselves, and disagreement raises drift proposals rather than upstream corrections. | M* | DESIGN §4 |
| FR-B4 | Write-back shall be additive/forward-only under every policy level: create, comment, label, close/reopen. Deletion, history rewriting, and force operations against GitHub shall not exist as capabilities. | M* | DESIGN §4 |
| FR-B5 | *(decided 2026-08-12)* Comment write-back shall be **outbound only**: comments authored in Vogt post to the linked forge object under `comment_only` and `full`. Inbound forge comments shall remain observations, visible against the linked item, and shall never be copied into the `comments` table. Bidirectional conversation mirroring is deferred (§3). | M* | DESIGN §4 |

### FR-L — Lifecycle & administration

| ID | Requirement | Pri | Source |
|---|---|---|---|
| FR-L1 | The CLI shall provide `init`, `status`, `serve`, `migrate` (schema), `backup`, `restore`, `export`, `import`. | M | DESIGN §7 |
| FR-L2 | Backup shall snapshot both stores consistently with a schema-version manifest; restore shall verify the manifest before touching data. | M | DEPLOY §5 |
| FR-L3 | Collectors shall run on an in-process schedule and be triggerable on demand. | M | DEPLOY §1 |

### FR-N — Events (notification surface)

| ID | Requirement | Pri | Source |
|---|---|---|---|
| FR-N1 | *(revised r2)* The system shall expose a cursor-based `/events` feed backed by a single append-only `events` table in the declared store with a monotonic `seq`. Declared writes shall insert their event row in the same transaction as the entity change and audit row. Sweep completions and CI state transitions shall be published into the same table by the application layer at sweep completion. `seq` is the cursor; no client shall be required to merge orderings across the two stores. | M | DESIGN §4.2, SCHEMA §2.5 |
| FR-N2 | Email/desktop/webhook push is out of scope for v1; the events feed is the sole notification surface. | M | DESIGN §9 |
| FR-N3 | *(r6)* The system shall expose collected GitHub notifications (FR-O8) as their own read-only view, filterable by project, reason and unread state, and separate from the `/events` feed — that feed is this instance's own history and shall not be merged with a forge's. The view shall state that its contents belong to the configured token's account and are therefore instance-scoped, not per-actor. Nothing shall be marked read upstream. | S* | DESIGN §4.2 |

### FR-U — GUI

| ID | Requirement | Pri | Source |
|---|---|---|---|
| FR-U1 | The GUI shall provide: per-project view, global backlog, global bugs, drift inbox, dependency graph view, audit browser. | M | DESIGN §10 (M6) |
| FR-U2 | The GUI shall display trust state and data freshness on every aggregated view. | M | DESIGN §6 |
| FR-U3 | *(r6)* The GUI shall provide an import form taking a repository reference and a reason (FR-P6), and a notification inbox over FR-N3. Both shall consume only the public REST API, and the import form shall offer no repository listing, search, or suggestion. | S | DESIGN §10 |

---

## 2. Non-functional requirements

### NFR-PO — Portability & offline

| ID | Requirement | Pri |
|---|---|---|
| NFR-PO1 | The product shall be fully functional with no GitHub and no forge — no network at all: projects, work, backlog, ranking, contracts, compliance status, dependency references, drift, audit. Forge and advisory integrations are optional plugins that only ever *add*. | M |
| NFR-PO2 | The full test suite shall run forge-less; forge-dependent tests are a separately marked layer. | M |
| NFR-PO3 | Self-hosting shall require zero external services: SQLite storage, single process, single volume. | M |
| NFR-PO4 | *(revised r4, r7)* The supported install path is an OCI image published to `ghcr.io/thedancingdeveloper-org/vogt` with SBOM and keyless cosign signature, consumed by a Docker Compose stack; image references in deployed compose files shall be **digest-pinned**, not alias-tracking. The wheel shall be published to **PyPI** when the repository goes public (NFR-O1), and until then to no index at all: a private index built to carry it would be a distribution channel with one user and a migration to undo. Reaching an instance does not require it (FR-A8). | M |

### NFR-D — Deployment & network

| ID | Requirement | Pri |
|---|---|---|
| NFR-D1 | One server process, one port: GUI, `/api`, `/mcp`, and health endpoints path-routed together. Split-mode is not a v1 topology. | M |
| NFR-D2 | *(revised r4)* No default shall encode **exposure or identity** — a public hostname, a `0.0.0.0` bind, a published LAN port, or a client-trusted URL — in code, images, docs, or examples. Values that are pure **host allocation** (a tailnet-bound port, an operator-owned certificate or token path) shall instead carry concrete defaults in the deployment's compose file, overridable from the stack environment, commented with the host they describe. Required-value gates (`${X:?}`) shall not be used for allocation values. | M |
| NFR-D3 | Client bootstrap tooling shall reconcile configured endpoint *values*, not key existence. | M |
| NFR-D4 | Health probes shall be plain HTTP and MCP-protocol-version-agnostic. | M |
| NFR-D5 | Default exposure is loopback (local) or tailnet/LAN; no public ingress in v1. Server functions behind NAT with outbound-only network use. | M |
| NFR-D6 | *(revised r4)* TLS shall be terminated **in-process** by `serve` (`--tls-cert` / `--tls-key`) from an operator-owned certificate mounted read-only; loopback may be plaintext. No fronting proxy, no public DNS record, and no entry in the host's shared reverse proxy shall be required to reach the service. | S |
| NFR-D7 | *(r4)* The target deployment shall be a single Docker Compose stack on Node B (`winrarhost`), whose desired state lives in the `indexarr/ops` repository at `personal/vogt/docker-compose.yml` and is applied only by Komodo (`DeployStack`, stack `personal-vogt`, server `Local`). Deployed containers shall never be hand-edited, and no deploy step shall SSH to the host to run compose directly. | M |
| NFR-D8 | *(r4)* In the NFR-D7 deployment, the published listener shall bind the host's Tailscale address, never `0.0.0.0`; stateful data shall use a named volume, and operator-owned material shall use absolute bind mounts (never `./relative` paths, which Komodo's per-deploy stack clone silently redirects). NFR-D5 continues to govern what the *product* permits an operator to bind. | M |
| NFR-D9 | *(r4)* The shipped image and the NFR-D7 compose stack shall run non-root with a read-only root filesystem, `no-new-privileges`, all capabilities dropped, and writable scratch supplied as `tmpfs`. Token files shall be mode-restricted to the container's uid. | M |
| NFR-D10 | *(r4)* Publishing an image and deploying it shall be distinct acts: a tag publishes, and production moves only when the pinned digest in `indexarr/ops` is changed and a deploy is executed. Rollback shall be a revert of that digest plus a redeploy, subject to the forward-only migration constraint (NFR-I3). | M |

### NFR-I — Integrity & reliability

| ID | Requirement | Pri |
|---|---|---|
| NFR-I1 | Declared writes shall be transactional: entity change + audit row + event row + revision bump commit atomically or not at all. | M |
| NFR-I2 | Collector or upstream failure shall never corrupt or mutate declared data (worst case: stale observations, visible as such). | M |
| NFR-I3 | Migrations shall be forward-only, run under a lock, and gate readiness until complete. | M |
| NFR-I4 | Derived tables shall be reconstructible from retained observations at any time; reconstruction is bounded by the retention horizon, and the latest observation per subject is retained regardless of age. | M |
| NFR-I5 | Retention: latest observation per subject kept indefinitely; history pruned by configurable policy (default 180 days), except observations referenced by drift proposals (FR-R5). | S |

### NFR-S — Scale & performance

| ID | Requirement | Pri |
|---|---|---|
| NFR-S1 | v1 envelope: single node, ≤ ~500 projects, ≤ ~100k work items, with interactive (< 1 s) brief/backlog/bug queries inside that envelope. | S |
| NFR-S2 | Observed-store growth shall be proportional to change, not polling frequency (digest dedup). | M |
| NFR-S3 | The storage layer shall avoid SQLite-only semantics that would block a later Postgres backend behind the same interface. | S |
| NFR-S4 | *(r2)* A seeded benchmark fixture at the NFR-S1 envelope shall exist from M2, and the interactive-query target shall be asserted against it in CI. | S |

### NFR-Q — Quality & maintainability

| ID | Requirement | Pri |
|---|---|---|
| NFR-Q1 | Python 3.11+; mypy `--strict` on `src/` and `tests/`; ruff — from the first commit. | M |
| NFR-Q2 | CI coverage gate ≥ 80% from M0. | M |
| NFR-Q3 | A feature merges only with CLI + REST + MCP + audit coverage (enforced by the parity test matrix, subject to the FR-A3 exclusion lists). | M |
| NFR-Q4 | The pydantic config schema is the single source of truth; example configs, compose files, and docs are generated from it and CI fails on drift. | M |
| NFR-Q5 | Own dependencies: committed `uv.lock`; Renovate/Dependabot weekly with version updates, vulnerability alerts, and security fixes each explicitly enabled; CI fails on manifest/lockfile mismatch. | M |

### NFR-C — CI/CD (GitHub Actions)

| ID | Requirement | Pri |
|---|---|---|
| NFR-C1 | Docs-only changes (`docs/**`, `design/**`, `**/*.md`) shall not trigger the full pipeline — docs lint/link/config-drift checks only. | M |
| NFR-C2 | Mixed code+docs changes shall run the full pipeline; the docs path is never a bypass (trivially-succeeding gate job pattern for required checks). | M |
| NFR-C3 | *(revised r5)* **Releases** — a semver-tagged image, `latest`, the wheel, the SBOM attestation — shall be tag-triggered only; a push to main shall never cut a release. A push to main **may** publish a **commit-identified** image (`sha-<commit>`, signed, carrying no semver and never moving `latest`), because deploying a fix must not require inventing a version number for it. Deploying remains a separate act (NFR-D10). | M |
| NFR-C4 | *(r4)* Every workflow job shall select a self-hosted runner explicitly (`runs-on: [self-hosted, node-b, linux, x64, …]`); GitHub-hosted runners and dynamic `runs-on` expressions shall not appear. The repository shall be added to the `public-node-b` runner group before its first workflow exists. Jobs needing a Docker daemon shall additionally request the `docker, publish` labels. | M |
| NFR-C5 | *(r4)* Image signing shall be keyless (workflow OIDC identity via Fulcio/Rekor), so that no signing key exists to store or rotate and the signature binds to this repository and workflow. | M |

### NFR-O — Open source & product

| ID | Requirement | Pri |
|---|---|---|
| NFR-O1 | The platform shall be **MIT licensed** (decided 2026-08-12; `LICENSE` in place, matching cadastre) and developed on GitHub — initially in a private repository under `TheDancingDeveloper-org`, going public at a milestone of the owner's choosing. | M |
| NFR-O2 | Images published with SBOM, signature, and attestations. | S |
| NFR-O3 | The project repository shall itself satisfy the default project contract (AGENTS.md, README, LICENSE, docs/, design/, src/). | M |

---

## 3. Explicitly deferred (non-goals for v1)

Recorded so absence is a decision, not an omission.

Deferred from the outset: multi-forge support (GitHub is the only,
optional, forge target); hosted/multi-tenant SaaS; multi-node/HA; public
internet exposure; split MCP/API processes; boards, sprints, burndown,
time tracking; inbound webhooks (polling baseline); agent execution (the
platform informs agents, it does not run them); Postgres backend (kept
possible, not built); attachments (paths/URLs in body text meanwhile).

Deferred by revision r2:

- **Gate enforcement** — blocking creation or registration on contract
  grounds (was FR-G2), and any system-performed relocation of projects
  (was FR-G9). Compliance is reported; the human decides.
- **The `~/WorkingStack` migration model** — a second, governed estate
  root only existed to give the gate an entry point. Without enforcement
  there is nothing to migrate *into*, and moving a project would break
  path references (path dependencies between workspace members, IDE and
  agent configs, CI checkouts) for no gain.
- **Version-resolved dependency tracking** — lockfile parsing, requested
  spec vs resolved version, direct vs transitive, ecosystem package
  identity, and the drift kinds that depend on them
  (`dep_version_skew`, `lock_manifest_mismatch`,
  `vendored_divergence`).
- **Advisory / vulnerability enrichment** (was FR-D7) — requires
  resolved versions.
- **Bidirectional comment mirroring** (FR-B5) — copying forge comments
  into Vogt's own `comments` table. It needs identity mapping for forge
  authors and loop suppression for Vogt's own outbound comments, and the
  observation view already answers "what was said upstream" without
  either.
- **Per-project authorization scopes** — v1 scopes are instance-wide; an
  agent holding `work.write` can write to every project. Noted as a
  known limitation, not an oversight.

Deferred by revision r3:

- **Filesystem discovery of projects** — crawling configured roots to find
  repos, and the registration-candidate listing that followed from it (was
  FR-G6, FR-G8). Projects are registered explicitly; the system never goes
  looking.
- **Continuous contract re-checking** (was FR-G5, FR-G7) — the contract is
  evaluated when asked, and the answer carries its age.

Deferred by revision r4:

- **Automated deploy on release.** The estate has a `komodo-deploy.sh`
  pattern that rewrites the image tag in ops, commits, and POSTs
  `DeployStack` from CI. Vogt does not use it in v1: a tag publishes a
  signed image, and moving production stays a separate act (NFR-D10).
  Reintroducing it is a small change and an explicit decision, not an
  oversight.
- **Public ingress for the Node B stack** — no DNS record, no Cloudflare
  entry, no reverse-proxy block. Consistent with NFR-D5, restated here
  because a tailnet-only stack is the one shape where someone is most
  likely to "just add a Caddy block".

Deferred by revision r6:

- **Repository listing, search or suggestion during import** — an account's
  repositories enumerated for the user to pick from. This is the r3
  registration-candidate listing (was FR-G8) arriving through the import
  form, which is the one place it looks like a convenience rather than a
  policy change. Import takes a repository the caller names (FR-P6).
- **Marking GitHub notifications read from Vogt.** A `PATCH` against
  somebody's inbox is a forge mutation; if it is ever built it belongs under
  the write-back policy (FR-B1) and in the write-back ledger (FR-B2), not as
  a convenience on a read view.
- **Per-actor notification inboxes.** Notifications belong to the account
  whose token is configured, so FR-N3 is instance-scoped. Per-actor inboxes
  require per-actor forge credentials, which is a larger identity change than
  the inbox is worth today — and the same limitation as the per-project
  authorization scopes deferred by r2.

Named stretch goal, **not committed and not designed for**:

- **AI-assisted drift detection and recommendation.** The intended
  eventual answer to "what changed and what should I do about it" is an
  integration that reads the observation store and proposes, rather than a
  larger scheduler and more drift rules. It is recorded here so that the
  *absence* of a heavier sweep mechanism reads as a decision. No v1
  requirement may be justified by this, and no v1 interface may assume it.
  The one thing v1 does that would enable it later is keep evidence
  self-contained and provenance-stamped (FR-R5, FR-O3) — which it does for
  its own reasons.

---

## 4. Traceability & change control

- Requirement IDs are stable; changes append, never renumber. Withdrawn
  requirements remain in place, struck through, with the withdrawing
  revision named.
- Each milestone (`ROADMAP.md`) shall name the requirement IDs it
  delivers; M-scope changes require updating this document in the same
  change.
- Revisions are numbered (r1, r2, …) and summarised at the head of this
  document.

---

## 5. Delivery verification (v1, 2026-08-12)

M0–M6 are built; 482 tests pass at 92% coverage. This section is the audit
of the *delivered* system against every requirement above — read against
the source and the tests, not against the roadmap's claims. It exists
because a requirements document that is never checked back against the
build is a wish list, and because §4 makes each milestone name the IDs it
delivers, which is a promise nobody had yet verified.

**Delivered in full: 65 of 72 live functional requirements, and 30 of 34
non-functional ones.** The exceptions are below, each with what is actually
missing rather than a score. Withdrawn and deferred IDs (FR-G2, FR-G5–G10,
FR-D7) are not counted; they are absences by decision (§3).

Three of the gaps (FR-G1, FR-D2, FR-S3) are the same failure mode: a
capability whose *type* exists and whose *producer* does not. A `RefKind`
member no operation emits, a `Scope` no operation requires, a `Contract` no
configuration can replace. Each reads as delivered from inside the code and
is unreachable from outside it — the class of gap a citation-grep cannot
find and a parity test does not cover, because all three are consistent
across CLI, REST and MCP by being consistently absent from every one.

### 5.1 Not fully delivered

| ID | Pri | What is missing | Severity |
|---|---|---|---|
| FR-L1 | M | The CLI provides `init`, `status`, `serve`, `backup`, `restore`, `export`, `import` — **not `migrate`**. Migrations are applied by `init`, which is idempotent and brings an existing instance forward, so the capability exists under another name and no data is at risk locally. What is missing is the verb an operator would reach for, and the one the deployed stack needs (below). | Low alone; compounds NFR-I3 |
| NFR-I3 | M | Migrations are forward-only ✅ and run under `migration_lock` ✅. They do **not gate readiness**: `/health/ready` reports the *applied* schema version without comparing it to the version the running image expects, and `serve` does not migrate. In the Node B topology (`command: serve`) an image carrying a new migration comes up, passes its healthcheck, and fails later on a missing table — as a SQL error at whatever touched it first, not as a red probe. See `DEPLOYMENT.md` §5. | **Highest of these** — it is the deploy path |
| FR-S6 | M | The audit log is queryable by actor, operation and entity, but **not by time**: `ListAuditParams` carries `limit`, `actor_id`, `operation`, `entity_id` and no time bound. "What happened between Tuesday and Thursday" is the query an audit log exists to answer, and it is answerable today only by paging. | Medium |
| FR-G1 | M | The contract is declarative ✅, carries a version identifier ✅, and names every failing criterion ✅ — but it is **not sourced from configuration**. It is the constant `DEFAULT_CONTRACT` in `core/contract.py`; `VogtConfig` has no contract keys, `CONFIG.md` lists none, and `evaluate()` is only ever called with the default. Nobody self-hosting Vogt can state a contract other than this repository's own without editing Python. | Medium — it is the requirement's entire "sourced from configuration" clause, in a product meant for others to run |
| FR-D2 | M | Edges record `path` and `git` ✅ with the manifest they came from ✅. The third reference kind, `declared`, **cannot be produced**: `DESIGN.md` §3.5's `project link A depends_on B` was never given an operation, no `project_dependencies` table exists, and `RefKind`'s third member is unreachable. A dependency no manifest expresses — a service calling another, a deploy script's assumption — cannot be recorded, so it is absent from `deps` and from the reverse lookup FR-D4 promises. | Medium |
| FR-S3 | M | All five scopes exist, parse, imply correctly and are issuable ✅. **`writeback` gates no operation.** `forge.writeback` sets a project's policy and requires `project.write`; the upstream write is a consequence of `work.write` operations. A token issued with `writeback` alone can read and nothing else. | Medium — a control that grants nothing is worse than one that does not exist, because it is issued in good faith |
| NFR-S4 | S | The benchmark fixture asserts the NFR-S1 interactive target at **500 projects and 5,000 work items**, not the ~100k items NFR-S1 names. The reduction is deliberate and argued in `tests/test_benchmark.py` — seeding 100k rows per run would cost minutes and prove nothing about the *query* — but the requirement says "at the NFR-S1 envelope" and the fixture is an order of magnitude below it on one axis. It catches an accidental N+1; it does not evidence the envelope. | Low, and honestly documented in the test |

An eighth was missed by this verification entirely and is recorded here by
r7:

- **NFR-PO4** promised two install paths joined by "and" — `uv tool install`
  *and* an OCI image. The image shipped; the wheel had no index behind it,
  and M4 claimed the requirement delivered. The ID is cited in `src/` and
  `tests/`, so the §5.4 method passed it: every citation was about the image.
  Corrected in r7 rather than implemented — see the revision note.

Two further items were **vacuously satisfied** — nothing violated them
because the thing they constrain was never built. Both are resolved by
FR-A8 (r7), and the pattern they formed is the finding:

- **NFR-D3** (client bootstrap reconciles endpoint *values*, not key
  existence). Vogt shipped no client-setup script. `connect` is now the
  first thing the rule binds: it emits values read from the instance, and
  there is no key-existence check anywhere in it.
- **`DEPLOYMENT.md` §4.3's generated `CONNECTING.md`.** The server half
  existed and was tested; the generator did not. It is now the `connect`
  operation rather than a committed file — a file in the repository is one
  more copy of a URL, which is the failure §4.3 was written to prevent.

Each of these was individually small and correctly described. What none of
them said, and what no requirement owned until FR-A8, is that between them
**a client could not connect to this product at all**.

### 5.2 Delivered differently from the specification

Each of these satisfies its requirement by a different mechanism than the
design named. They are listed so that a reader comparing document to code
does not mistake a decision for a defect; `ROADMAP.md`'s per-stage "as
built" notes carry the reasoning.

| Where | Specified | Built |
|---|---|---|
| FR-U1/U2, `DESIGN.md` §4 | React SPA | Buildless ES modules. A wheel that needs npm to build, or committed bundler output nothing verifies, was the cost; `test_the_gui_needs_no_build_step` fails if a build step appears. |
| FR-D6, `SCHEMA.md` §3.2 | `latest_autoupdate_posture` and five other typed `latest_*` tables | `latest_observations` (generic) + `latest_dep_refs`. Posture is observation kind `posture` carrying three independent facts. Seven rebuild paths would have been seven places to drift. |
| FR-A5, `DESIGN.md` §4.2 | MCP tools `next`, `annotate` | Neither exists. `backlog` already answers "what next" in ranked order; `annotate` was `work.comment` under another name. |
| NFR-O2 | "SBOM (syft) → cosign sign + attest" | Buildkit attestations (`sbom: true`, `provenance: true`) plus `cosign sign` over the digest. One attestation producer; a second would need a reason to disagree with the first. |
| FR-U1 | Six views | Six views, five in the nav — the dependency graph is reached from a project page (`#/deps/<slug>`) rather than being a global entry, because a reference graph with no project selected has nothing to draw. |
| FR-A1/A4, `DESIGN.md` §4.2 | REST with path parameters (`/projects/{id}/brief`, `POST /drift/{id}/accept`) | **No path parameters anywhere.** One pydantic model per operation serves all three transports, so an identifier travels as a query or body field. `DELETE` is unused: revoking a suppression is `POST /suppressions/revoke`, because it is an audited write needing a reason, and a reason does not fit a `DELETE`. Three verbs collapse into one `drift.resolve` carrying the resolution. |
| FR-L3 | Delivered at M2 | Built at M6. The on-demand half existed from M2; the in-process schedule did not, and `DEPLOYMENT.md` §1 had described it since M4. Found by walking the must-have IDs for ones cited nowhere in `src/` — which is the check this section now institutionalises. |

### 5.3 Where the schema document had drifted

`SCHEMA.md` was written before M0 and described the store as intended.
Three corrections, all made in place:

- **`project_dependencies` was listed and never built** — the table behind
  FR-D2's `declared` reference kind (§5.1).
- **`auth_decisions` (M4) and `writeback_actions` (M5) were built and never
  documented.** Two tables holding, respectively, every authorization
  decision and every attempted forge write — the stored evidence for FR-S5
  and FR-B2 — were absent from the only document that claims to describe
  the schema.
- **§3.2 listed seven derived tables where two exist**, and §2.4/§4 read
  answers off `latest_contract_checks` and `latest_forge_items`, which are
  `kind` filters over `latest_observations` rather than tables.

The direction is consistent and worth naming: the document tracked what
each revision *removed* with care, and never recorded what implementation
*added*. Nothing in CI reads this file.

### 5.4 One process gap worth more than any single requirement

**NFR-Q4's drift gate can be bypassed by a code-only change.** The
regenerate-and-`git diff --exit-code` check that keeps `config.example.toml`
and `docs/CONFIG.md` honest lives only in `docs.yml`, which triggers on
`docs/**`, `design/**` and `**/*.md`. A commit that adds a field to
`src/vogt/config.py` and nothing else runs `ci.yml` alone — where the check
does not exist — and lands with the generated artefacts stale. This was not
found by reading the workflow: it was found by running the generator during
this audit and watching it rewrite two committed files.

The fix is one step in `ci.yml`, in the job that already runs when
`src/**` changes. Recorded here rather than in `ROADMAP.md` because it is a
gate failure, not a stage deliverable: NFR-Q4 says CI fails on drift, and
for the paths most likely to cause drift, it does not.

***Fixed in r7.*** `ci.yml`'s test job now regenerates and diffs, so a
code-only change to the config schema fails there. Applied while adding
`public_url` and `import_root` — two commits of exactly the shape that would
have slipped through.

### 5.4a A second process gap: conjuncts are not verified *(r7)*

**A requirement joined by "and" is verified as though it were one claim.**
§5.5's method searches each ID across `src/` and `tests/` and hand-checks the
ones cited nowhere. That finds a requirement nothing implements — it found
FR-L3 — and it cannot find a requirement whose implementation covers half its
text, because a single citation makes the ID look answered.

NFR-PO4 is the instance. It named two install paths, `uv tool install` **and**
an OCI image. The image half is cited in `test_deploy.py` and in the compose
header; the wheel half had no index behind it at all. Every citation the
search found was about the image, so the ID passed, and M4 claimed it.

The rule this adds: **an ID containing "and", a comma-joined list, or a
parenthesised second clause is verified once per conjunct, and §5.1 records
the conjunct rather than the ID.** No tooling is proposed — the failure was
one of method, and a checklist that says "split the sentence first" is the
whole fix.

### 5.5 What was verified, and how

- Every FR/NFR ID was searched for across `src/` and `tests/`; IDs cited
  nowhere were then checked by hand against the delivered behaviour, since
  an uncited requirement is either unbuilt (FR-L3's failure mode at M6) or
  built without a thread back to why.
- That pass alone was not enough, and a second one is what found FR-G1,
  FR-D2 and FR-S3. Each of those is *heavily* cited, because the type it
  names is real and well-commented; what is missing is the producer. So the
  design documents were then read in the other direction — every table in
  `SCHEMA.md` looked up in the DDL, every route in `DESIGN.md` §4.2 looked
  up in the registry, every enum member traced to something that can emit
  it. **A requirement that names a value is met only when something can
  produce that value**, which is not a property citation-counting can see.
- Claims that could be checked by running something were: the suite,
  the coverage gate, the CLI's verb list, the operation registry's 55
  operations, the generated OpenAPI document, the config generator.
- Requirements whose subject is a *deployment* (NFR-D7–D10) are verified
  against `deploy/personal-vogt.compose.yml` and `release.yml`, which is
  where they are expressible. The stack is written, hardened and
  port-allocated; its image reference is still a placeholder digest, so
  nothing has been deployed from it — which is NFR-D10 working as intended,
  not an omission.
