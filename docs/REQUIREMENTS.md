# Vogt — Requirements (v0.3)

Status: **baseline, revision r9** (2026-08-14), distilled from `DESIGN.md`,
`SCHEMA.md`, `DEPLOYMENT.md`, [`MERGE_MYDEVENV2.md`](MERGE_MYDEVENV2.md) and
the originating product discussion.
**v1 (M0–M6) is built**; §5 is the requirement-by-requirement verification
of the delivered system against this document, including the four
requirements that are not fully met.

Priority key (MoSCoW): **M** = must have (not shippable without it) ·
**S** = should have (target for its release, degradable) · **C** = could have
(explicitly designed-for, may slip). Deferred items are in §3.

**v1 = M0–M6** (all stages in `ROADMAP.md`). **MVP = M0–M2** — the first
build that is daily-usable. "M" priority means required for v1, not
required for MVP; the roadmap says which stage delivers each ID.

**v2 = M9–M13**, the merge stages added by revision r9; M14 is consolidation
and delivers no new ID. **Merge-MVP = M9–M10** — the first build where a work
item can open a coding session. Requirements marked *(r9)* are v2 scope and
their priority reads against v2, not v1.

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

### Revision r8 — the handshake is a negotiation, not a gate

**FR-A6 is revised in place, and it was wrong rather than merely incomplete.**
It said unsupported MCP protocol versions "shall be refused with the supported
list named". Vogt implemented that faithfully on both transports, and a test
asserted it.

The consequence showed up the first time a real client connected. Claude Code
moved to protocol `2025-11-25`; Vogt supports `2025-06-18` and older, so it
answered `-32602 unsupported MCP protocol version` and the client dropped the
connection. Cadastre, reached from the same container by the same mechanism,
carried on working. Vogt was registered, reachable, authenticated — and
unusable, on the day the whole M8 exercise was about making it reachable.

Refusing makes a server unusable by every client **newer** than itself, which
is the direction clients always drift. The MCP specification says as much: a
server that does not recognise the requested version must answer with one it
does support, and the client decides whether to continue. FR-A6's own second
clause had the right instinct already — bridge↔server skew "shall warn and
never block" — and the first clause contradicted it for no reason anyone
recorded.

What is kept is the useful half: the client is still told what the server
speaks. It is told by being *answered* in a version the server speaks, rather
than by being handed a list alongside a refusal.

Two tests changed with the requirement, which is the point of writing them
against IDs: `test_an_unsupported_version_is_refused_with_the_supported_list`
became `test_an_unknown_version_negotiates_down_rather_than_refusing`, and its
HTTP counterpart likewise. A third was added, because negotiating down must
not become ignoring what was asked for.

### Revision r9 — Vogt runs the work it governs

**One non-goal is reversed, deliberately.** `DESIGN.md` §1.2 and §3 of this
document have listed *"being an agent runner"* as out of scope since the
first draft: Vogt tells agents *what* and *why*; it does not schedule or
execute them. That was the right boundary for a product with no execution
surface. The merge adopted in [`MERGE_MYDEVENV2.md`](MERGE_MYDEVENV2.md) —
MyDevEnv2 brought in as Vogt's session engine, from the **`dev` branch of
MyDevEnv2 at head `2214a7d`** — gives Vogt one, and the boundary moves with
the reason for it: **Vogt still never decides to run anything on its own.**
Every session starts because a person asked, or because a person created the
schedule that asked. What r3 refused — the system going looking, continuous
checking, autonomous action — stays refused, and FR-G15 is untouched. What
changes is only that "start work on this" is now an *operation* instead of
advice.

The half of the non-goal that survives is restated in §3 as **autonomous work
pickup**, so that the reversal is bounded rather than open: there is no loop
in which an agent takes the top backlog item because it was there.

Four subordinate decisions, each recorded because a future reader will ask:

1. **The engine is adopted as-built, not respecified.** Sessions, PTY,
   attach, scrollback, activity states, agent tasks, push and the assistant
   arrive with delivered behaviour and their own documents
   (`API_CONTRACT.md`, `ASSISTANT.md`, `AGENT_TASKS.md`, under `docs/engine/`
   after the merge). FR-E1/E2 and FR-T1/T2 state the *load-bearing*
   properties Vogt now depends on — the ones whose regression would break a
   Vogt requirement — not a re-derivation of the engine's full surface.
2. **The write plane is not weakened by the front door.** FR-W1's rule — a
   write needs a reason its author typed — and r6's restatement — a mutating
   operation appears in the GUI only through a view that collects one — bind
   the new surfaces identically (FR-U6, FR-U15, FR-T3). The proxy forwards;
   it never pre-approves (FR-S9).
3. **The assistant's approval gate is a structural guarantee, not
   configuration.** The engine's threat model — no model output reaches an
   effector without an on-screen approval, and voice never approves — is
   promoted from module documentation to a numbered requirement (FR-T2),
   because the merged product leans on it far harder than MyDevEnv2 did.
4. **The GUI is specified to interaction depth, not surface list.** M6's
   requirements named views (FR-U1) and the delivery was judged by their
   existence. A Jira/Trello-grade GUI fails in its *interactions* — a drag
   that lies about what the server accepted, a filter that resets on reload,
   a list that only updates on refresh — so FR-U10–U22 pin the interaction
   contract itself: liveness, addressability, optimistic honesty, keyboard
   reach, and what every surface does when its data source is absent. Each is
   testable without a pixel being asserted.

**Numbering, checked rather than assumed.** The draft named each family's
maximum "as of r8" — FR-U3, FR-S8, FR-A8, NFR-D10, NFR-C5, NFR-I5, NFR-Q5 —
and every one of those is correct against the file; each ID appended below
continues its family with no collision and no gap, and the three new family
letters (E, T, M) are unused by any existing family. Two things the draft did
not have right are corrected here.

- **NFR-S was absent from the draft's list of maxima although the draft
  appends to it.** Its maximum was NFR-S4, so NFR-S5 is right — but it was
  right unchecked, which is how the next one lands wrong. Recorded here as
  checked: every family's IDs now run 1..n with no gap and no repeat.
- **The stage numbers were wrong.** The draft's stages opened at M8, on the
  stated grounds that M7 was the last one. M8 is taken: `ROADMAP.md` already
  carries *M8 — Reachable by an agent* (FR-A8), the stage r8's own revision
  note describes itself as happening during. The merge stages are therefore
  **M9–M14**, and every reference to them here and in `ROADMAP.md` uses the
  corrected numbers. Renumbering a stage is not renumbering a requirement:
  §4 forbids the latter, and no appended ID is affected.

This revision folds `MERGE_MYDEVENV2.md` §12 into this document and §13 into
§3; §14 of that document becomes `ROADMAP.md` M9–M14. Those sections stay in
place as the merge's working record, and no longer govern. Sections §§1–11 of
that document remain the design authority for the merge itself, and the
Source column below cites them as **MERGE §n**.

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
| FR-A6 | *(revised r8)* An MCP `initialize` naming a protocol version the server does not recognise shall be answered with the newest version the server **does** support, leaving the client to continue or disconnect — the handshake is a negotiation, not a gate. A recognised version shall be answered with itself. Bridge↔server version skew shall warn on stderr and never block startup. | M | DESIGN §4.1 |
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
| FR-S9 | *(r9)* The front door shall hold the single public token namespace, extended with Vogt capabilities; each front-door token shall map to a named Vogt actor whose paired core token the proxy injects — so audit records real actors, the proxy never pre-approves, and the double-gated writes of FR-S4 are not weakened. | M | MERGE §9 |
| FR-S10 | *(r9)* A session started for a project or work item shall receive a per-session actor-scoped token, minted at start and revoked at session end; its writes shall be distinguishable in the audit log from every other actor's. | M | MERGE §6.1, §9 |

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
| FR-U4 | *(r9)* The GUI shall present a board of work items whose columns are the workflow's states read from `workflow.list` — never hard-coded — where a drag is a `work.transition` and an illegal transition bounces with the server's stated reason. | M | MERGE §7.1 |
| FR-U5 | *(r9)* The GUI shall present a work item in full: description, state history, comments, relations, labels, per-item audit trail, collected evidence with freshness and trust, and the start-session control (FR-E4). | M | MERGE §7.2 |
| FR-U6 | *(r9)* The GUI shall present the ranked backlog and bugs views with the explainable `why`, quick-create, and bulk transition/label — under r6's rule: a mutating operation appears only through a view that collects a reason the user typed. | M | MERGE §7.3 |
| FR-U7 | *(r9)* The GUI shall present per-project pages: brief, CI status, contract/compliance, drift inbox, dependency graph, and the import form. | S | MERGE §7.4 |
| FR-U8 | *(r9)* The PWA shall consume only public APIs, and every URL in the shipped bundle shall resolve against the operation registry *and* the engine's API contract — extending the existing M6 assertion to the Solid bundle and to both halves. | M | MERGE §7 |
| FR-U9 | *(r9)* The legacy GUI shall keep serving at `/ui-legacy` until every operation it exposed is reachable in the PWA, and shall then be removed — parity is asserted, not assumed. | S | MERGE §5.1 |
| FR-U10 | *(r9)* Views showing server-announced state — session activity (FR-E2), work item state, drift arrivals, notification counts — shall update live from the SSE stream without a manual refresh. A lost stream shall be indicated and shall reconcile on reconnect; a stale view shall never present itself as current. | M | MERGE §7; engine README |
| FR-U11 | *(r9)* Every project, work item, board (including its active filter set), session, and audit query shall be addressable by URL: deep links shall survive reload, be shareable, and restore the exact view. Terminal deep links (`/#/t/<id>`) are adopted as-built; Vogt surfaces shall follow the same scheme. | M | MERGE §7; engine README |
| FR-U12 | *(r9)* A drag or inline edit shall render optimistically and reconcile against the server's answer, which is authoritative: a refused `work.transition` shall roll the item back visibly and surface the server's stated reason where the drop happened. The client shall never persist, cache, or re-derive a state the server refused. | M | MERGE §7.1 |
| FR-U13 | *(r9)* The board shall support swimlanes by project or initiative, per-column WIP counts, and collapse/expand of lanes and columns; lane and column layout preferences shall persist per client. | S | MERGE §7.1 |
| FR-U14 | *(r9)* Board, backlog, and bugs views shall filter by project, workflow state, type, label, initiative, and actor, with filters combinable and reflected in the URL (FR-U11). A combined filter shall be nameable and recalled as a saved filter; saved filters are per-client state in v2 (server-side shared filters are deferred, §3). | S | MERGE §7.1, §7.3 |
| FR-U15 | *(r9)* Quick-create shall exist on the board and backlog: title, type, project, and the typed reason FR-W1 requires, inline, without leaving the view; every other field is deferrable to the detail view. A quick-create that omits the reason shall not submit. | M | MERGE §7.3, FR-W1 |
| FR-U16 | *(r9)* The command palette shall reach every read surface (projects, work items, sessions, views) by fuzzy name, and every GUI-exposed mutating verb by opening the view that collects its reason — the palette itself shall never execute a write directly (r6's rule restated for the keyboard path). | S | MERGE §7 |
| FR-U17 | *(r9)* Trust state and freshness shall be displayed on every aggregated view (FR-U2 extended to the new surfaces), and session-derived evidence shall show the session's activity state as its liveness indicator: a claim backed by a still-running session is marked provisional, not fresh. | M | FR-U2, MERGE §6.2 |
| FR-U18 | *(r9)* The drift inbox shall show each proposal's evidence (both sides of the disagreement, with provenance and age) *before* any act is possible, and accept/reject shall collect a typed reason. Bulk accept shall not exist. | M | MERGE §7.4, FR-R2 |
| FR-U19 | *(r9)* The audit browser shall filter by actor, project, operation, and time range, and every rendered write shall show who, what, and why. A work item's detail view shall link into the audit browser pre-filtered to that item. | S | MERGE §7.5, FR-S6 |
| FR-U20 | *(r9)* A work item linked to a session (FR-E4) shall show the live activity badge and an open-terminal control that navigates to the terminal surface attached to that session; the terminal surface shall link back to the owning work item. | M | MERGE §6.1, §7.2 |
| FR-U21 | *(r9)* Every surface shall have a designed absent state: engine unavailable → Vogt views work and session controls disable with the named reason; vogt-core unavailable → terminal, files, git, and assistant-over-sessions work and Vogt surfaces report the outage rather than rendering empty data as truth (FR-E9's degrade rule, made visible). | M | MERGE §11.2, FR-E9 |
| FR-U22 | *(r9)* The board shall be operable entirely from the keyboard: item focus, moving an item between columns (the drag's equivalent, still subject to FR-U12's reconcile), opening detail, and quick-create shall each have a binding discoverable in the GUI. | S | MERGE §7.1 |

### FR-E — Coding sessions & session engine *(r9)*

The families below are introduced by r9 and are v2 scope throughout; their
priorities read against v2 (M9–M13), per the r9 revision note.

| ID | Requirement | Pri | Source |
|---|---|---|---|
| FR-E1 | *(r9)* The system shall run interactive terminal sessions: per-session PTY with ring-buffer scrollback, WebSocket attach with snapshot replay, multiple concurrent clients per session, and full lifecycle (create / list / get / rename / kill / delete). *(Adopted as-built from the engine.)* | M | MERGE §5.2; engine README |
| FR-E2 | *(r9)* Each session shall carry a live activity state (`idle` / `running` / `waiting-for-input` / `errored`) derived from output heuristics and published on the server-wide SSE event stream. | M | engine README |
| FR-E3 | *(r9)* The system shall start a session *for a registered project*: the working directory shall be the path the project registry records for it, and template selection shall consult the registry — never path heuristics — so a session opened "for" a project always opens in the registry's tree. The import root and the engine's workspace root shall be the same tree. | M | MERGE §6.3 |
| FR-E4 | *(r9)* The system shall start a session *for a work item*: the item's brief (description, `why`, relations) shall be written to a prompt file via the agent-task prompt mechanism; the session id shall be recorded on the work item as an audited write; and the work item's views shall reflect the session's live activity state. | M | MERGE §6.1 |
| FR-E5 | *(r9)* Sessions started for a project or work item shall register the Vogt MCP server for agents running inside them, carrying a per-session actor-scoped token (FR-S10), so that agent writes to Vogt are attributed to that session's actor. | M | MERGE §6.1 |
| FR-E6 | *(r9)* Session outcomes — exit code, duration, resulting working-tree delta — shall be collected as observations with freshness and trust, like all other evidence. | S | MERGE §6.2 |
| FR-E7 | *(r9)* A scheduled agent task may be bound to a project or work item; a bound run's findings shall be recordable as Vogt observations, not only as push notifications. The schedule remains one a person created (§3). | S | MERGE §6.1 |
| FR-E8 | *(r9)* `session.start`, `session.list`, and `session.stop` shall be operations in the registry, and therefore present with parity on CLI, REST, and MCP (FR-A1). | M | MERGE §6.2 |
| FR-E9 | *(r9)* The engine shall remain bootable with vogt-core absent, degrading to plain sessions — absence of the core costs Vogt features, never session availability (the ContextKeeper degrade rule, applied inward). | S | MERGE §11.2 |

### FR-T — Conversational assistant (the AI layer) *(r9)*

| ID | Requirement | Pri | Source |
|---|---|---|---|
| FR-T1 | *(r9)* The assistant shall be a server-side tool-use loop with read access to sessions (`list_sessions`, `read_session_tail`) and to a curated read-only slice of the operation registry (at minimum: `backlog`, `bugs`, `why`, `project.brief`, `project.list`, `work.get`, `work.list`, `compliance`); registry-backed tool schemas shall be generated from the registry, not hand-written. | M | MERGE §8.1 |
| FR-T2 | *(r9)* Every mutating assistant tool — `send_input`, any `work.*` write, `session.start` — shall pass the pending-action gate: one pending action at a time, carrying the exact payload and target, expiring unapproved, approved only by an on-screen act. No model output shall be able to bypass the gate, and the voice path shall never approve. *(Promoted from the engine's `ASSISTANT.md` threat model.)* | M | MERGE §8.2 |
| FR-T3 | *(r9)* An assistant-initiated Vogt write shall be audited to the approving user's actor with a `why` derived from the conversational context — never to a shared "assistant" actor. | M | MERGE §8.2, FR-W1 |
| FR-T4 | *(r9)* Assistant tool results carrying external content — terminal output, forge-derived text, imported issue bodies — shall be delimited as untrusted data; the threat-model rule that external content never becomes instructions extends to every Vogt read. | M | MERGE §8.5 |
| FR-T5 | *(r9)* The assistant shall be drivable by voice: push-to-talk STT in the mobile shell, spoken replies in any client, with a validation pass against domain vocabulary (project names, "backlog") before v2 ships — voice is adopted unproven and shall not be presumed working. | S | MERGE §8.4; engine ASSISTANT.md |
| FR-T6 | *(r9)* The assistant shall not exist unless configured: absent its API key the routes answer 404 and every GUI hides the surface. *(As-built rule, retained.)* | M | engine ASSISTANT.md |
| FR-T7 | *(r9)* The tool loop shall be provider-portable: an OpenAI-compatible backend and a native Anthropic backend shall both be supported, and the currently-documented hang against `claude-*` proxy routes shall be resolved or the route refused with a named reason. | C | MERGE §8.4 |

### FR-M — Mobile surface *(r9)*

| ID | Requirement | Pri | Source |
|---|---|---|---|
| FR-M1 | *(r9)* The mobile app shall be the Capacitor shell loading the merged PWA. Its MVP1 feature set shall be: terminal sessions, assistant with voice, push, backlog/board read, and session start/approve. | M | MERGE §3; ROADMAP M13 |
| FR-M2 | *(r9)* Push notifications shall be routed for events worth a phone interruption: a session entering `waiting-for-input` or `errored`, new drift, and the agent-task notify hook — and for nothing else by default. | S | MERGE §10 |
| FR-M3 | *(r9)* Vogt surfaces shall be usable at phone widths; the board shall render as a list, not columns, below the narrow breakpoint. | S | MERGE §7 |

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
| NFR-D11 | *(r9)* The merged product shall ship as one stack with one published port: the Rust engine is the front door, serving the PWA, its native APIs, the WebSocket attach path, `/api/vogt` and `/mcp` proxied to the core, and aggregate health. Vogt-core shall bind loopback only and shall never be published. Any port that serves MCP shall serve plain HTTP health, so FR-A7 and NFR-D1 hold at the front door rather than being lost behind it. (MERGE §5.2) | M |
| NFR-D12 | *(r9)* The dev/prod split shall be branch-shaped before any merge phase lands: `dev` builds `:dev` images deployed to a dev stack for live validation, and only `main` deploys to prod. Several merge stages — mobile, voice, push — are verifiable only against a live dev stack. *(Adopted from the engine's `uplift.md`, where it is already named a prerequisite; MERGE §10.)* | M |

### NFR-I — Integrity & reliability

| ID | Requirement | Pri |
|---|---|---|
| NFR-I1 | Declared writes shall be transactional: entity change + audit row + event row + revision bump commit atomically or not at all. | M |
| NFR-I2 | Collector or upstream failure shall never corrupt or mutate declared data (worst case: stale observations, visible as such). | M |
| NFR-I3 | Migrations shall be forward-only, run under a lock, and gate readiness until complete. | M |
| NFR-I4 | Derived tables shall be reconstructible from retained observations at any time; reconstruction is bounded by the retention horizon, and the latest observation per subject is retained regardless of age. | M |
| NFR-I5 | Retention: latest observation per subject kept indefinitely; history pruned by configurable policy (default 180 days), except observations referenced by drift proposals (FR-R5). | S |
| NFR-I6 | *(r9)* Backup and restore shall cover the whole product state as one act: the core's SQLite, the engine's `state_dir`, and enough registry/workspace metadata to re-establish FR-E3's path agreement after restore. (MERGE §10) | M |

### NFR-S — Scale & performance

| ID | Requirement | Pri |
|---|---|---|
| NFR-S1 | v1 envelope: single node, ≤ ~500 projects, ≤ ~100k work items, with interactive (< 1 s) brief/backlog/bug queries inside that envelope. | S |
| NFR-S2 | Observed-store growth shall be proportional to change, not polling frequency (digest dedup). | M |
| NFR-S3 | The storage layer shall avoid SQLite-only semantics that would block a later Postgres backend behind the same interface. | S |
| NFR-S4 | *(r2)* A seeded benchmark fixture at the NFR-S1 envelope shall exist from M2, and the interactive-query target shall be asserted against it in CI. | S |
| NFR-S5 | *(r9)* GUI views shall stay interactive at estate scale — on the order of a hundred projects and a few thousand open work items: long lists virtualize, the board's filter and drag paths do not degrade with backlog size, and no view fetches the whole estate to render a page of it. Server-side pagination and filtered queries already exist; the GUI shall use them. (MERGE §7) | S |

### NFR-Q — Quality & maintainability

| ID | Requirement | Pri |
|---|---|---|
| NFR-Q1 | Python 3.11+; mypy `--strict` on `src/` and `tests/`; ruff — from the first commit. | M |
| NFR-Q2 | CI coverage gate ≥ 80% from M0. | M |
| NFR-Q3 | A feature merges only with CLI + REST + MCP + audit coverage (enforced by the parity test matrix, subject to the FR-A3 exclusion lists). | M |
| NFR-Q4 | The pydantic config schema is the single source of truth; example configs, compose files, and docs are generated from it and CI fails on drift. | M |
| NFR-Q5 | Own dependencies: committed `uv.lock`; Renovate/Dependabot weekly with version updates, vulnerability alerts, and security fixes each explicitly enabled; CI fails on manifest/lockfile mismatch. | M |
| NFR-Q6 | *(r9)* Both test suites shall pass in the merged repository, and two absence-modes shall stay green: the forge-less run (NFR-PO1–PO3, untouched) and a core run with no engine present — vogt-core remains fully testable alone. (MERGE §5.1, FR-E9) | M |

### NFR-C — CI/CD (GitHub Actions)

| ID | Requirement | Pri |
|---|---|---|
| NFR-C1 | Docs-only changes (`docs/**`, `design/**`, `**/*.md`) shall not trigger the full pipeline — docs lint/link/config-drift checks only. | M |
| NFR-C2 | Mixed code+docs changes shall run the full pipeline; the docs path is never a bypass (trivially-succeeding gate job pattern for required checks). | M |
| NFR-C3 | *(revised r5)* **Releases** — a semver-tagged image, `latest`, the wheel, the SBOM attestation — shall be tag-triggered only; a push to main shall never cut a release. A push to main **may** publish a **commit-identified** image (`sha-<commit>`, signed, carrying no semver and never moving `latest`), because deploying a fix must not require inventing a version number for it. Deploying remains a separate act (NFR-D10). | M |
| NFR-C4 | *(r4)* Every workflow job shall select a self-hosted runner explicitly (`runs-on: [self-hosted, node-b, linux, x64, …]`); GitHub-hosted runners and dynamic `runs-on` expressions shall not appear. The repository shall be added to the `public-node-b` runner group before its first workflow exists. Jobs needing a Docker daemon shall additionally request the `docker, publish` labels. | M |
| NFR-C5 | *(r4)* Image signing shall be keyless (workflow OIDC identity via Fulcio/Rekor), so that no signing key exists to store or rotate and the signature binds to this repository and workflow. | M |
| NFR-C6 | *(r9)* The merged CI shall run both halves on every push — Rust fmt/clippy/test, web typecheck, APK build, and the existing Python suite — and the build-vs-release discipline of NFR-C3 (a push builds `sha-` images, only a tag releases) shall govern the merged image. (MERGE §5.1) | M |

### NFR-O — Open source & product

| ID | Requirement | Pri |
|---|---|---|
| NFR-O1 | The platform shall be **MIT licensed** (decided 2026-08-12; `LICENSE` in place, matching cadastre) and developed on GitHub — initially in a private repository under `TheDancingDeveloper-org`, going public at a milestone of the owner's choosing. | M |
| NFR-O2 | Images published with SBOM, signature, and attestations. | S |
| NFR-O3 | The project repository shall itself satisfy the default project contract (AGENTS.md, README, LICENSE, docs/, design/, src/). | M |

---

## 3. Explicitly deferred (non-goals for v1, and from r9 for v2)

Recorded so absence is a decision, not an omission.

Deferred from the outset: multi-forge support (GitHub is the only,
optional, forge target); hosted/multi-tenant SaaS; multi-node/HA; public
internet exposure; split MCP/API processes; ~~boards~~ *(reversed at r9 —
FR-U4 adds a board whose columns are the workflow states that already
exist)*, sprints, burndown, time tracking; inbound webhooks (polling
baseline); ~~agent execution (the platform informs agents, it does not run
them)~~ *(reversed at r9 — see the revision note, and "autonomous work
pickup" below for the half that survives)*; Postgres backend (kept possible,
not built); attachments (paths/URLs in body text meanwhile).

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

Deferred by revision r9. Everything r2–r8 deferred — discovery, candidate
listing, per-actor notification inboxes, multi-forge, multi-node — **remains
deferred and is not re-litigated by the merge**.

- **Autonomous work pickup.** The system shall not start sessions on its own
  initiative: no loop in which an agent takes the top backlog item because it
  was there. Every session traces to a human act or a human-created schedule
  (FR-E4, FR-E7). **This is the surviving core of the non-goal r9 reversed**,
  and it is the reason the reversal is a change of surface rather than of
  posture — the r3 rule that the system never goes looking is untouched.
- **Voice approval.** Approving a pending action by voice, in any form. FR-T2
  forbids it for v2; lifting that is a threat-model revision, not a feature.
- **Backend convergence on Rust.** The two-process shape is the v2
  requirement (NFR-D11); porting the Python core is a possible future with no
  requirement justified by it — the same discipline r3 applied to the
  AI-assisted-drift stretch goal below. Note that NFR-D11's two processes are
  *not* the split MCP/API topology deferred at the outset: there is still one
  published port, and the core is not reachable from outside it.
- **Sprint ceremonies, burndown, time tracking.** v1's non-goals stand;
  Jira-*shaped* does not mean Jira-complete.
- **Server-side saved filters.** FR-U14's saved filters are per-client in v2.
  Sharing them requires per-user server state, which Vogt does not otherwise
  have and should not grow for this alone.
- **Bulk drift resolution.** FR-U18 forbids bulk accept deliberately: a drift
  acceptance is a declared-state write and carries its own reason. Making
  that convenient in bulk is exactly how r6's rule would erode.
- **GUI-side offline mode.** The PWA installs, but no Vogt surface caches
  data for offline mutation; a queued offline write cannot carry an honest
  freshness answer.
- **The archived GPUI desktop client.** It stays in the MyDevEnv2 repository
  and is not carried into the merged tree.
- **Cadastre consolidation.** Both "estate registers" keep running as they
  are; their overlap is its own future investigation, not part of this merge.
- **iOS shell.** MVP1 is Android — the existing Capacitor shell. Nothing
  designed here precludes iOS and nobody builds for it in v2.

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

*This section verifies v1 and is not restated by r9. The counts below are
v1's and do not move because a later revision added requirements. The IDs r9
appended are verified separately in §6. That section was first written as the
stages landed, on the reasoning that §5 was written after v1 in one pass and
that doing it that way is how seven requirements went a year without anyone
checking. M14 replaced it with a single per-conjunct pass anyway, because
writing a row the week you wrote the code turned out to swap one bias for
another — §6's opening says what that cost.*

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

## 6. Delivery verification (v2 to date, 2026-08-14)

*This section verifies the IDs revision r9 appended — **46 of them** — and
counts them apart from §5's for the reason §5 gives: v1's numbers do not move
because a later revision added requirements. M9–M13 are built; M14 is this
document catching up with them, and then with the work this section's previous
pass caused.*

The first pass at this section was written stage by stage as each landed, by
the person who had just written the code. The second was the §5.4a pass:
every requirement was **split into its conjuncts before anything was looked
up**, and each conjunct was then read against the source and the tests rather
than against `ROADMAP.md`'s claims or against the rows this section used to
carry. Where a row here contradicts one of those, the contradiction is the
finding.

**This is the third pass**, taken after the PWA acquired a test runner and
after FR-E6, FR-E7 and NFR-I6 were built. Every row it moves was re-read
against the source and the tests; no row was adjusted from a summary of what
had landed. Where a commit's own subject line claims more than its code does,
this section says so — three of them do.

**201 conjuncts across 46 IDs. 108 are delivered, 64 are implemented and
asserted by nothing, 8 cannot be verified in this environment at all, and 21
are short or absent.** Each conjunct is in exactly one of those four, by this
precedence: short before unverifiable, unverifiable before untested, untested
before delivered. So a conjunct counted "unverifiable here" is one whose
implementation was read and believed; a conjunct counted "short" was not
believed, and §6.2 says why.

**A fourth pass moved six conjuncts, and three of them were moved by nothing
being written.** The pipeline ran. Every claim this section made about the merged image, the two
image streams and the Android shell was a claim about a workflow file that had
never once executed, and three of them turned out to be true; the two that
were false failed on their first run, in the two places a file cannot show
you. `docs` failed on a link that resolved on this machine and nowhere else.
The engine job failed on `sudo apt-get` with *root is not in the sudoers
file* — the runner is root, so there was nothing to elevate from. Neither was
a code defect and both were invisible to review, which is §6.3 finding 16.

**Delivered means a file and a test.** Not a citation — §5.4a's whole point is
that a grep for an ID finds the docstring that names it. Where code plainly
does the thing and nothing asserts it, the answer is *implemented, untested*,
which §5 distinguishes and this section does too.

**The conjuncts that moved were re-counted individually; the rows that did not
move keep the previous pass's split.** §6.2 is one conjunct per row, as its
own opening line says, so its arithmetic is countable by eye; §6.1, §6.2a and
§6.2b group conjuncts by ID and their rows carry several each. The totals are
therefore checkable without re-deriving all 201:

- **106 delivered** = 61 + 18 out of §6.2 + 3 out of §6.2a + 20 out of §6.2b,
  then +6 in the fourth pass (four out of §6.2, one each out of §6.2a and
  §6.2b)
- **64 untested** = 28 − 3 to §6.1 + 1 out of §6.2 + 39 out of §6.2b, − 1
- **21 short** = 43 − 19 + 1 arriving from §6.2b, − 4
- **8 unverifiable** = 69 − 60 − 1

One bookkeeping correction, since it is the kind of thing this section exists
to catch in others: §6.2's FR-T2 row was marked **Closed** when
`assistant_auto_type` was removed, and counted as zero rather than moving,
and §6.1 never grew a row to name the conjunct it had gained. The totals were
right and the conjunct was nowhere. It has a row in §6.1 now, and §6.2's
closed row is gone, and neither is a count change.

**The environment changed once, and the change is the story of this pass.**
`web/vitest.config.ts` and 75 tests in `web/src/__tests__/` mount the five
Vogt surfaces in jsdom against a fake front door installed *under*
`vogtApi.ts`'s single `fetch` — so a test asserting "the board asked
`workflow.list`" is asserting about the URL a real Vogt would receive, and the
route table, the query encoding and the 503→`VogtUnavailable` mapping are the
product's own. **That collapses "unverifiable in this environment" from 69
conjuncts to 9.** What it does not do is turn them all into deliveries: 39 of
the 60 that left §6.2b went to §6.2a, because a thing that *could* now be
asserted and is not is untested, not unverifiable. The four buckets moved by
very different amounts for that reason.

**Of the 93 conjuncts in FR-U4–U22, forty-two are delivered** — thirteen
before this pass and twenty-nine moved by it. The GUI was specified to
interaction depth precisely so it could be judged by its interactions, and
the board's interactions are now judged: a drop moves the card before
anything is written, a typed reason is what submits it, a refusal rolls the
card back and renders Vogt's sentence *in the column the drop landed in*, and
nothing a refusal touched reaches storage. So are the ranked views' two bulk
writes, the item page's inline edit, and every surface's outage state. What is
still unproven is what a headless DOM cannot reach, and it is worth naming
rather than implying:

- **The gesture, as opposed to what the gesture does.** `fireEvent.dragStart`
  and `fireEvent.drop` invoke the same handlers a browser would, and the
  board's drag state is a signal rather than `dataTransfer`, so the semantics
  are genuinely exercised. But no test fires `dragover`, and `dragover`'s
  `preventDefault` is what makes a drop event happen at all in a browser. That
  a pointer can start and complete a drag is still the M11 demo's job.
- **The shell.** `App.tsx`'s URL→tabs effect — 1,489 lines, and the thing M11
  found broken for *every* surface — is mounted by nothing. The tests mount
  one surface at one URL, so what is proven is that a surface handed its URL
  restores its view, not that pasting that URL into the app opens it.
- **Anything with a layout.** No CSS is loaded, so FR-M3's phone width,
  NFR-S5's virtualization at scale and every visual claim remain the demo's.
- The engine's SSE stream, a device, and a speaker. **Not the merged image and
  not the APK** — both were built by CI on 2026-08-14, on runners that have
  the Docker daemon and the Android SDK this container does not. The merged
  image ran both its entrypoints before it was pushed, and is pinned by
  digest in `deploy/vogt-stack.compose.yml`. What no runner can supply is a
  hand on a phone.

Three r9 IDs were **missing from this section entirely** before the previous
pass: FR-E6, FR-E7 and NFR-I6. All three are now built, and all three are
below — FR-E6 and FR-E7 as the `session-outcomes` collector and the engine's
task bindings, NFR-I6 as a backup that copies the engine's state directory and
a manifest that records where the estate was.

### 6.1 Delivered

The conjuncts with a file and a test behind them. Where an ID's conjuncts are
split across this table and §6.2, only the ones named here are delivered.

| ID | Conjuncts delivered | The test that says so |
|---|---|---|
| FR-E1 | PTY with ring-buffer scrollback; WebSocket attach with snapshot replay; all six lifecycle verbs | `engine/server/src/scrollback.rs` (`drops_oldest_on_overflow`, `snapshots_only_bytes_after_a_retained_cursor`); `engine/server/tests/integration.rs` (`ws_attach_echoes_input_and_replays_on_reattach`, `create_list_and_kill_session`, `rename_session`, `get_session_returns_typed_detail_shape`) |
| FR-E2 | The four activity states; derived from output heuristics | `engine/server/src/activity.rs` (`detects_yn_prompt`, `nonzero_exit_becomes_errored`, `quiet_window_collapses_to_idle`, `recent_output_is_running`) |
| FR-E3 | `cwd` is the path the project registry records, and a work item with no project is refused rather than guessed | `tests/test_sessions.py::test_a_session_opens_in_the_path_the_registry_records`, `::test_a_work_item_with_no_project_has_no_tree_to_open_in`; engine-side `integration.rs::session_cwd_must_stay_under_workspace_root` |
| FR-E3 | The import root and the engine's workspace root are the same tree — co-located by the compose stack, and now *reported* by the front door rather than assumed | `api.rs::check_workspace_agreement` publishes a `workspace_agreement` readiness check; `vogt_core.rs` asserts the disagreeing branch (`ok: false`, a detail that says what it costs, `fatal: false`, and the container still ready) and the nothing-to-compare branch. Deliberately non-fatal: some projects being invisible is a bad answer, not a dead server (FR-E9). The comparison is by path component, not by string prefix — §6.3 finding 12, resolved, and `a_sibling_directory_is_not_inside_the_workspace` is the test that would have caught it |
| FR-E4 | The description reaches the brief; the brief is written through the agent-task prompt mechanism; the session id is recorded as an audited write; the item's views carry live activity | `tests/test_sessions.py::test_the_work_items_brief_travels_with_the_session`, `::test_the_session_is_linked_to_its_work_item`, `::test_the_work_item_view_shows_what_is_running_for_it`; `integration.rs::session_prompt_is_written_to_a_file_the_child_is_pointed_at` |
| FR-E4 | The brief carries the item's `why` — the ranking's per-input contributions, not a single score | `tests/test_sessions.py::test_the_brief_says_why_the_item_is_ranked_where_it_is` (which asserts the *contributions*, so a total on its own would fail it), `::test_a_brief_survives_a_ranking_that_cannot_be_computed` (the session starts either way, so a score never becomes a precondition for starting work) |
| **FR-E6** | **All four**: exit code, duration, the working-tree delta, and all three collected as observations with the freshness and coverage every other kind carries | `src/vogt/collectors/session_outcomes.py`; `tests/test_session_outcomes.py` (twenty-one, including `test_a_finished_session_reports_its_exit_code_and_duration`, `test_the_delta_counts_what_the_tree_recorded_in_the_window`, `test_an_outcome_is_collected_and_never_declared`, `test_the_outcome_carries_the_sweeps_coverage`, `test_an_unchanged_outcome_does_not_grow_the_store`). The three shapes the previous pass called unbuilt are each asserted twice: once for the value, once for its absence — a session the engine has forgotten is `unknown`, never `finished` with a null code |
| **FR-E7** | **Both**: a task may be bound to a project or work item, and a bound run's findings are recordable as Vogt observations | `engine/server/src/agent_tasks.rs` (`vogt_project`/`vogt_work_item`, `record_finding`, the binding in the prompt and the environment); `integration.rs::a_bound_task_carries_its_subject_and_records_what_it_reported`, `::an_unbound_task_names_no_vogt_subject`; `tests/test_session_outcomes.py::test_a_bound_runs_findings_become_observations`, `::test_an_unbound_task_is_not_vogts_business`, `::test_a_task_bound_to_a_work_item_lands_on_that_items_project`, `::test_a_binding_naming_something_this_instance_lacks_is_ignored`. "Recordable as observations" is met as a *pull*, not a write plane — see §6.2's note on what that leaves out and why it is a decision |
| FR-E5 | The per-session actor-scoped token is carried into the session; an agent's writes are attributed to that session's actor | `tests/test_sessions.py::test_the_session_carries_its_own_token`; `tests/test_m10_demo.py::test_the_m10_demo` step 5 |
| **FR-E8** | **Both**: the three operations are in the registry, and all three are driven on CLI, REST and MCP | `tests/test_parity.py::test_transports_return_the_same_answer`, `::test_transports_leave_the_same_audit_trail`, and `::test_every_shared_operation_is_driven_by_the_script`, which is what stops a session operation being quietly dropped from the script |
| FR-E9 | The engine boots and stays ready with no core, and the Vogt routes refuse with a named reason | `engine/server/tests/vogt_core.rs::with_no_core_configured_the_engine_is_still_ready`, `::with_no_core_configured_the_vogt_routes_refuse_with_a_reason`, `::an_unreachable_core_is_reported_without_declaring_this_pod_unready` |
| FR-T1 | Server-side tool loop; `list_sessions`; `read_session_tail`; the schemas are the core's, forwarded verbatim | `engine/server/src/assistant.rs` (`plain_reply_round_trip`, `list_then_tail_then_reply`, `tools_come_from_the_core_not_from_a_literal_in_this_file`); `vogt_tools.rs::conversion_forwards_the_served_schema_verbatim` |
| FR-T2 | Every Vogt write passes the gate unconditionally; the card carries the exact payload and target, and the approved payload is the one the core receives; approval is an authenticated on-screen act and nothing else | `assistant.rs::a_vogt_write_waits_for_approval_and_then_uses_the_approver_pairing`, `::a_write_without_a_reason_is_refused_before_it_becomes_a_card`; `auth.rs::maps_mutating_routes_to_capabilities` |
| FR-T2 | `send_input` passes the same gate, with no setting that turns it off | `assistant.rs::send_input_pauses_and_approve_delivers`, which no longer takes an `auto_type` argument because there is none: `assistant_auto_type` is removed rather than defaulted, and the dispatcher's `send_input` arm refuses instead of delivering, so an edit that loses the interception fails closed |
| FR-T3 | The credential is the *approver's*, taken from the request that pressed approve; a write has no shared fallback and an unpaired approver is refused by name | `assistant.rs::a_vogt_write_waits_for_approval_and_then_uses_the_approver_pairing` (proposed by an unpaired token, approved by a paired one, and the core saw the approver's), `::an_unpaired_approver_gets_a_refusal_rather_than_a_shared_actor`; `vogt_tools.rs::a_write_has_no_fallback_credential` |
| FR-T4 | Both delimiters exist; terminal output is delimited; every Vogt read and every approved write's answer is delimited | `assistant.rs::list_then_tail_then_reply`, `::a_vogt_read_arrives_delimited_as_untrusted_data` (instruction-shaped text from the core arrives inside the tags) |
| FR-T4 | Forge-derived text and imported issue bodies arrive as data — structurally, because every string from the core reaches the model through a core answer and every core answer is wrapped where it arrives | `assistant.rs::every_place_the_core_answers_this_loop_delimits_what_it_said`, which reads the source and asserts the `Ok` arm of every `vogt.call` site delimits; `::an_imported_issue_body_arrives_as_data_like_any_other_stored_text`. There is no forge-aware path in the loop and there does not need to be — but "every" was a fact about two call sites rather than a rule, and a third added tomorrow would have been undelimited with nothing failing. Deleting the wrapping at one site now turns three tests red |
| FR-T4 | The system prompt names the delimiters as untrusted, and names all four the loop now uses | `assistant.rs::every_delimiter_the_loop_emits_is_one_the_prompt_names` and `::the_prompt_names_no_delimiter_that_nothing_emits`, over `emitted_delimiters`, which reads the tags out of the source of both emitting files rather than holding a list of them. Both directions, so a tag added to the loop and not the prompt fails, and so does a rule about a boundary that never arrives. §6.3 finding 13 is what the previous version of this test missed |
| FR-S9 | The front door holds one token namespace carrying Vogt capabilities; the proxy strips the caller's credential and injects the paired core token; the proxy never pre-approves — a refusal forwards nothing | `engine/server/src/auth.rs::scoped_tokens_limit_capabilities`; `vogt_core.rs::the_core_is_handed_the_core_token_not_the_callers`, `::two_front_door_tokens_reach_the_core_as_two_actors`, `::a_write_needs_the_vogt_write_capability` and `::an_unauthenticated_caller_never_reaches_the_core`, both of which assert nothing was proxied |
| **FR-S10** | **All four**: the token is per-session and actor-scoped, minted at start, revoked at stop, and its writes are distinguishable in the audit log | `tests/test_sessions.py::test_the_session_carries_its_own_token`, `::test_stopping_a_session_revokes_what_it_held`; `tests/test_m10_demo.py` step 5, which asserts the comment's actor is `agent:session:<id>` *and* differs from the `session.start` actor |
| **FR-U8** | **All three**: the PWA reaches Vogt only through the front door and no other origin; every path resolves against the operation registry; every engine path resolves against `app.rs`'s router *and* `API_CONTRACT.md` | `tests/test_pwa.py::test_every_vogt_path_in_the_pwa_is_a_registered_operation`, `::test_the_pwa_reaches_vogt_only_through_the_front_door`, `::test_every_engine_path_in_the_pwa_is_a_route_the_engine_serves`, `::test_every_engine_path_in_the_pwa_is_in_the_engine_s_api_contract`, `::test_no_vogt_surface_opens_its_own_door`. Read against source rather than a built bundle, because no bundle is built here — the sources are what a bundle is built from, and a second call site would fail the check |
| FR-U9 | The legacy GUI keeps serving, and parity is asserted rather than assumed — in both directions | `tests/test_pwa.py::test_the_pwa_renders_everything_the_legacy_gui_did` (an exact set, so a view added to the wrong front end fails too) |
| FR-U6, FR-U15, FR-U18 | r6's rule, on the surface most likely to erode it: no exported write can be called without a reason, and quick-create will not submit without one | `tests/test_pwa.py::test_every_vogt_write_the_pwa_offers_collects_a_reason` |
| **FR-T5** | Push-to-talk: the microphone is open exactly as long as the button is held, from a pointer or from the keyboard, and the release sends what was said | `web/src/__tests__/assistant.test.tsx` (six), the first thing in this repository to mount `Assistant.tsx`. Held rather than toggled because the take auto-sends — a toggle left on in a room with other people does not merely listen, it eventually speaks — and a pointer that ends off the button still ends the take. Two of the six were rewritten after the mutations they were meant to catch survived: one now asserts the take is *closed* once rather than that one message was sent, because sending clears the draft and the message count stays right for the wrong reason, and the other says in its own body that the behaviour it pins has a spare mechanism holding it |
| FR-U16 | Every read *view* is reachable from the palette; the palette can never execute a write, checked by import | `tests/test_pwa.py::test_the_palette_reaches_every_vogt_surface`, `::test_the_command_palette_never_writes_to_vogt` |
| FR-U16 | Projects and work items are reachable *by name*, fuzzily, and a project's entry opens the deep link a shared URL uses | `web/src/__tests__/commandPalette.test.tsx` (`offers each registered project by name`, `finds a project by a fuzzy fragment of its name` — `rstnz` finds `rustnzb` and does not drag in `Vogt` — `opens the project's own deep link`, `still reaches work items and every view by name`, and `contributes nothing when Vogt cannot be asked`, which is the palette declining to fail open) |
| FR-U4 | The columns are the union of `workflow.list`'s states, walked from each machine's initial state, and are never written down | `web/src/__tests__/board.test.tsx` (`draws one column per state the server published`, `changes shape when the workflow does` — a different machine and not one previous name survives — and `gives a state no machine mentions a column, and says no machine mentions it`) |
| FR-U4, FR-U12 | A drag is a `work.transition`; it renders optimistically before anything is written; a refusal rolls the card back; the refusal is Vogt's own sentence, rendered in the column the drop landed in | `board.test.tsx` (`moves the card on the drop, before anything is written`, `sends the transition with the reason the user typed`, `rolls a refused move back and renders Vogt's own sentence where the drop happened`, `will not submit a move with no reason`, `puts the card back when the reason is abandoned`). Residual: the tests fire `dragStart` and `drop` and never `dragover`, whose `preventDefault` is what lets a browser deliver a drop at all — the semantics are asserted, the gesture is the M11 demo's |
| FR-U12 | A refused state is never persisted, cached or re-derived, on the board and on the item page | `board.test.tsx::never persists a state the server refused` (the only thing in storage is which columns collapsed); `workItemDetail.test.tsx::does not re-derive a refused value when the editor is reopened` |
| FR-U12 | An inline edit renders optimistically and reconciles against the server's answer, which wins | `workItemDetail.test.tsx` (`renders the change while Vogt is still deciding, and says it is unsaved`, using a held response so the optimistic frame is observable at all; `keeps the server's version of the change, not the one that was typed`; `rolls a refused edit back and shows Vogt's own reason beside the field`) |
| FR-U6 | Bulk transition and bulk label, each one audited write per item carrying the batch's reason, each refusing without one, each reporting a partial batch as partial in Vogt's words | `backlog.test.tsx` (six tests for label, three for transition, including `does not let a reason typed for a transition justify a labelling` and `leaves the refused items selected and the rest not`) |
| FR-U6, FR-U15 | The ranked views render, quick-create raises an item, and bulk transition moves a batch — as things a person operates rather than bindings that exist | `backlog.test.tsx`, `board.test.tsx::raises an item without leaving the board` (the new card appears in its column and no navigation happens) |
| FR-U15 | Quick-create exists on the board as well as the backlog, will not submit without a title *or* a typed reason, never prefills the reason, and is unreachable while Vogt cannot be asked | `board.test.tsx`'s FR-U15 block (seven), including `never prefills the reason, however convenient the last one was` — the board deliberately remembers the last *move's* reason, and the create form deliberately does not inherit it |
| FR-U11, FR-U14 | The board's six filters and its swimlane mode are the URL: restored from a pasted link, written back when chosen on the surface, round-tripped, and put back when the shell navigates to the bare path | `board.test.tsx`'s FR-U11 block (four) and `restores every one of the six filters from a pasted link`; `backlog.test.tsx::round-trips a filter chosen on the surface through the URL` |
| FR-U14 | A combined filter is nameable, recalled in full, survives a reload as per-client state, and a refresh interval is not part of what a name means | `board.test.tsx`'s FR-U14 block (five), including `keeps the multi-valued filters intact through the round trip` and `does not let a named view change how often the board refreshes`; `backlog.test.tsx::saves a named filter set and recalls it` |
| **FR-U17** | A claim backed by a still-running session is marked provisional, not fresh — read from the observed store itself, not from a ranking over it | `web/src/WorkItemDetail.tsx`'s "Observed evidence" panel, through the new `observations.list` binding in `vogtApi.ts`; ten tests in `workItemDetail.test.tsx`. Three states, not two: an observation whose payload does not carry the flag at all is `unverified` rather than settled, in the same words the trust badge uses, because a blank says "no opinion" when the honest answer is "nobody checked". `settlement()` reads the collector's own `provisional` flag rather than re-deriving liveness — a surface deciding for itself what "still running" means is a second copy of a rule the collector already keeps, and the two would eventually disagree. There is no work-item parameter on the operation, so the panel matches on the payload's `work_item`, which `session_outcomes.py` writes for both kinds it produces |
| FR-U17 | Trust state is on every card and every ranked row, and an absent one reads as `unverified` rather than as blank; the aggregate says how old it is | `board.test.tsx`'s FR-U17 block (four — including `puts one on every card, so the aggregate cannot drop the awkward column`); `backlog.test.tsx::reads an absent trust state as unverified`. A blank badge says "no opinion"; the honest answer is "nobody checked" |
| FR-U22 | Focus moves across and within columns; `Shift`+arrow proposes a move and still collects the reason; `Enter` opens the item at its own URL; `n` opens quick-create and is announced on the board's own keyboard line | `board.test.tsx`'s two FR-U22 blocks (five), including the one asserting that an `n` typed into the move composer is not stolen by the shortcut |
| FR-U18 | Bulk accept does not exist, and cannot arrive by accident | `tests/test_pwa.py::test_drift_is_resolved_one_proposal_at_a_time` (one call site, and no multi-select) |
| FR-U21 | Every Vogt surface can tell an outage from an empty answer and renders the server's own reason; the core-absent half — terminals, files, git keep working | `tests/test_pwa.py::test_every_vogt_surface_distinguishes_an_outage_from_emptiness` asserts the structural half and says in its own docstring that it cannot judge the copy; `web/src/__tests__/absentStates.test.tsx` and the outage blocks in the other three files now judge it, on all five surfaces — the server's sentence verbatim, the sentence that says what the absence *means*, nothing rendered as data, writes disabled, and a 500 called a failure rather than an outage. `contextkeeper.rs::a_contextkeeper_outage_leaves_terminals_working_and_unprotected` is the core-absent half. **One panel is thinner than the rest**: the item page's Sessions panel renders Vogt's sentence and nothing asserts it does — found while mutation-testing FR-U17, when a mutation that should have gone red survived because the same literal appears first in `sessionsFailure`. The conjunct is delivered on all five surfaces; that one panel is where a regression would be quiet |
| FR-M2 | New drift is summarised, coalesced and cursored; the default set is exactly the four FR-M2 names, and no more | `engine/server/src/vogt_drift.rs` (`one_drift_is_named`, `a_burst_is_one_notification_that_counts`, `a_cursor_survives_a_round_trip`); `push.rs::only_the_kinds_fr_m2_names_are_on_by_default` |
| FR-M3 | The board's list layout at phone width, in the three rules that make it one: `display: block` on the row, the head row hidden, and the state name grown out of `attr(data-state)` | `tests/test_pwa.py::test_the_board_is_a_list_below_the_narrow_breakpoint`, `::test_the_board_cells_carry_what_the_hidden_head_row_said`, `::test_the_vogt_surfaces_share_the_engine_s_narrow_breakpoint` |
| NFR-D11 | The engine's native APIs; the WebSocket attach path; `/api/vogt` proxied under its own prefix with the query string intact; `/mcp` proxied with the caller's credential unchanged; aggregate health with a non-fatal core check | `engine/server/tests/vogt_core.rs` (`a_vogt_read_reaches_the_core_under_its_own_prefix`, `a_query_string_survives_the_hop`, `mcp_forwards_the_callers_credential_unchanged`, `a_reachable_core_is_reported_with_its_schema_state`); `integration.rs::readyz_is_public_and_returns_checks` |
| **NFR-I6** | **All four**: the core's SQLite; the engine's `state_dir`; enough metadata to re-establish FR-E3's path agreement; and one act that covers them | `tests/test_lifecycle.py` — nine for the stores, and six more: `test_a_backup_carries_the_engines_state_and_says_so`, `test_a_backup_without_the_engine_says_which`, `test_a_backup_survives_an_unreadable_engine_directory`, `test_a_restore_puts_the_engine_state_back`, `test_a_restore_reports_an_estate_that_moved`, `test_an_older_manifest_still_restores`. The manifest is version 2 and carries `engine_state` — a sentence on *every* branch, so a backup that covered two thirds of the product is distinguishable from one that covered all of it before somebody restores it — and `import_root`, which with the restored `projects.root_path` values and the front door's `workspace_agreement` check is what re-establishing the path agreement needs. The restore **reports** a moved estate (`import_root_then` / `import_root_now`) and does not rewrite the stored paths, which is the right division — the requirement asks for the metadata, not for a silent rewrite of every project's root — but it is worth knowing that nothing enumerates which projects are now unresolvable. §6.3 finding 14, resolved: the merged compose sets `VOGT_ENGINE_STATE_DIR`, and because two configurations now name one directory, `api.rs::check_backup_agreement` publishes a `backup_agreement` readiness check that says either which directory the backup covers or what the archive would silently omit (`vogt_core.rs`, three tests) |
| NFR-Q6 | The forge-less run; the core run with no engine present, produced by deleting `engine/`, `web/` and `mobile/` rather than by inspection | `.github/workflows/ci.yml` job `core`; `tests/test_pwa.py`'s skip guard is what lets it pass. **Run for real on 2026-08-14** — the job passed on a self-hosted runner, so "the core alone still works" is now an observation rather than a workflow file |
| **NFR-C6** | The pipeline governs the merged image: it is built from `engine/Dockerfile` with the repository as context, the PWA is built first because `rust-embed` reads `web/dist/` at compile time, both entrypoints run in the candidate before the push, the digest is signed, and a tag can release it | `tests/test_deploy.py::test_the_merged_image_is_built_from_the_engine_dockerfile`, `::test_the_pwa_is_built_before_the_merged_image`, `::test_both_halves_run_before_the_merged_image_is_pushed` (all three over both workflows), `::test_a_tag_can_release_the_merged_image`. The build half is more than asserted — it ran, on 2026-08-14, and published a signed `dev-ee18adc`. The release half is a path that exists and no tag has yet taken, which is why its test reads `release.yml` rather than a registry |
| **NFR-D12** | `dev` builds `:dev` images and `main` builds `sha-<commit>`, for the core-only image **and** the merged stack, with no alias either can move | `tests/test_deploy.py::test_the_two_streams_are_kept_apart`, parametrized over both jobs — a rule kept by the core image and dropped by the merged one would leave the artefact the merge exists for as the unlabelled stream |
| NFR-D11 | One published port, and it is the engine's: the merged stack publishes exactly one mapping, it maps to 8910, vogt-core's 8911 is published nowhere, and the front door reaches it over loopback | `tests/test_deploy.py::test_the_merged_stack_publishes_the_engine_and_only_the_engine`, `::test_the_merged_stack_pins_a_published_digest`, `::test_the_merged_stack_takes_its_core_token_from_a_file`. Asserted from the compose file, which until this pass no test in the repository read — which is how a placeholder digest of sixty-four zeros survived four stages under prose describing a pinned image. The *stack* half — two processes actually coming up together under `entrypoint.sh` — is still §6.2b's |

**Six IDs are delivered in every conjunct: FR-E6, FR-E7, FR-E8, FR-S10,
FR-U8, NFR-I6.** Three of them were the shape this product was already good
at proving — transport parity, an actor in an audit row, a URL in a route
table, each a machine-checkable relationship. The other three are the three
that were entirely unbuilt at the previous pass, which is worth noting for
what it says about the method: a requirement written down and audited as
*absent* was easier to deliver whole than a requirement audited as
half-present, because nothing about it had to be argued with first.

### 6.2 Delivered differently, or short — per conjunct

Twenty-one conjuncts. §5.4a: the conjunct is the row, not the ID. Each row
is one claim that the requirement makes and the build does not, which is why
this table's rows can be counted and the other three tables' cannot.

Twenty rows left this table in the third pass — nineteen of them counted, and
the twentieth was FR-T2's, already closed and already counted elsewhere. One
row arrived. It is FR-U17's, and it is the only kind of arrival that matters:
a conjunct that was believed while it was unverifiable and did not survive
becoming checkable.

**The fourth pass removed one more**, NFR-C6's merged-image row, and it is
worth saying why rather than only that it moved: the row asserted that
`release.yml` was untouched and that no path existed by which a tag could
produce a merged image. `release.yml` has had a `stack-image` job — semver
tags, both entrypoints smoke-tested, `cosign sign` on the digest — since the
commit that added `build.yml`'s. The row was describing the repository as it
had been when its first half was written. That is the same failure this
section exists to catch in `ROADMAP.md`, committed here, and the fix is the
one this section always prescribes: it is now asserted by a test that reads
the file.

| Conjunct | What is actually true | Severity |
|---|---|---|
| FR-E3 — "template selection shall consult the registry" | Vogt performs no template selection at all. `start_session` passes the caller's `template` through as the command (`src/vogt/application/services/sessions.py`); there is no per-project template, no registry lookup, and nothing to consult. The clause is satisfied only in the sense that a heuristic it forbids is also absent. The engine's own template matching is untouched and still applies to sessions the engine starts. | Low — the failure it forbids cannot happen; the capability it implies does not exist |
| FR-E5 — "shall register the Vogt MCP server for agents running inside them" | **Delivered differently, and the previous description of it was wrong in a way that hid a defect.** Registration is not "run out of band by the container bootstrap": `agent-auth.sh` runs `mcp-bootstrap` *in the session's own startup*, per session, which is why sessions have had a Vogt MCP server all along. Believing otherwise is what stopped anyone asking the next question — **which Vogt it registered.** The answer was: not this one. The bootstrap read only its own `VOGT_MCP_URL`, defaulting to `winrarhost:18094`, so every session's agent was pointed at the *core-only stack this merge replaces* — the one `DEPLOYMENT.md` §9.5 turns off — while `start_session` exported the session's real endpoint and had it discarded. The opencode registration was worse: it pinned `VOGT_URL` at registration time, overriding the session's, and is written once and reused by every session after — the exact lesson the cadastre `:18081 → :18092` move taught, recorded in a comment two functions above it and not applied. Now: explicit override, then the session's own `VOGT_URL`, then the front door on loopback, which needs no DNS or certificate and cannot name a retired deployment (NFR-D11). Three tests in `tests/test_deploy.py`. **What is still short is only portability**: the wrappers and the script live in the image, so a session started against an engine built another way has no Vogt MCP server. | Low, down from Medium — and the row is a reminder that a wrong description of a working mechanism is more dangerous than a missing one, because it retires the question |
| FR-T3 — "a `why` derived from the conversational context" | Still short, and less so. The `why` is whatever the model puts in the tool argument, and **nothing can verify that a sentence was derived from anything** — that half is not a gap to be closed but a claim no code can make. What was a gap is that the two failures the prompt names by name went unenforced, so the phrasing the instructions single out was the one that always got through. `assistant.rs::contentless_reason` now refuses them: a reason that only says who asked, and a reason that restates the act. It refuses by *removal*, so "the user asked for this after the sprint scope changed" passes and "as requested" does not — the refusal must not teach the model to hide the provenance, which would trade a useless audit row for a misleading one. The refusal returns to the model as a tool error and the loop continues, so the ordinary outcome is a second attempt before any card reaches a person. Three tests, one of which asserts the refusal reaches the model rather than only that no card appeared. | Medium — an unverifiable reason in an audit row is the failure mode FR-W1 exists against, and this narrows the class it can be *contentless* in without pretending to settle whether it is true |
| FR-T5 — "a validation pass against domain vocabulary … before v2 ships" | Not run, and nothing was built that would let it be: the recognizer's best match goes straight into the composer and is auto-sent. No project list, no slug normalisation, no `WI-\d+` repair. What was done instead is that the prompt now states items are `WI-7` and projects are slugs. The requirement says voice "shall not be presumed working", and it is still presumed. | **The requirement's own words** |
| FR-T7 — a native Anthropic backend | `ChatBackend` has two variants, `Http` and a test mock. Nothing in the repository speaks the Anthropic API. | Known, priority C |
| FR-T7 — "the hang shall be resolved or the route refused with a named reason" | Neither. No code inspects the model name; the nearest mitigation is a 60-second client timeout, which turns a hang into a timeout without naming it. | Known, priority C |
| FR-M1 — "session start/approve" | There is no Vogt session-approval flow, on any surface. `vogtApi.ts` has no approval binding and the registry has no approval operation; the two approvals in the product are ContextKeeper's recovery bundle and the assistant's pending action, neither of which is this. Session *start* exists and is three taps deep through a work item. | Medium — it is a named MVP1 item that was never built |
| FR-U5 — "state history" | The panel renders the workflow's states with the current one marked, and says in as many words that the transitions belong in the audit trail. That is the machine, not the item's history: a reader cannot see when this item entered this state or what it came from without leaving the surface. | Low, and honestly labelled on the surface itself |
| FR-U5 — "per-item audit trail" | A link to the audit browser pre-filtered to the item, not an embedded trail — which is a reasonable division and is FR-U19's second clause working. What the link cannot show is comments: they are audited against the comment rather than the work item, so the per-item filter returns creates, transitions and updates only. The surface says so; the gap is server-side and open. | Medium — an audit trail that silently omits a kind of write is the failure this product exists to prevent |
| FR-U10 — "drift arrivals … shall update live" | The drift inbox has no subscription and no poll. It re-reads when its filter key changes or when a person presses Refresh. | Medium |
| FR-U10 — "notification counts … shall update live" | Same: the inbox reads `unread` once per query and never again. | Medium |
| FR-U10 — "a stale view shall never present itself as current" | True of the board, which tracks when it loaded and says "Stale — updated N ago, retrying". The other four surfaces have no age indicator at all: a backlog tab left open all morning looks exactly like one loaded a second ago. | **Medium** — it is the clause the requirement is actually about, kept on one surface of five |
| FR-U16 — "every GUI-exposed mutating verb by opening the view that collects its reason" | One of them. `New Work Item…` opens the backlog's quick-create; transition, comment, drift resolve, import and session start/stop have no palette entry. The board's own quick-create arrived without a palette entry too, so the ratio is unchanged by a surface gaining one. The rule the clause exists to keep — never execute, only open — is kept perfectly by the one entry that exists. | Low |
| FR-U19 — "filter by … project" | Applied to the loaded window, not to the query: `ListAuditParams` takes actor, operation and entity and nothing else. The surface says so rather than implying otherwise, and resolves at most 500 of a project's items to scope the filter. | Compounds FR-S6 |
| FR-U19 — "filter by … time range" | Same, and this is §5.1's FR-S6 reappearing in the surface that needed it most. With no offset and no cursor on `audit.list`, the browser also cannot see past the newest 500 records at all. | Compounds FR-S6 |
| NFR-D12 — "deployed to a dev stack for live validation" | Nothing deploys, from any branch. `build.yml` says so in its own step summary, and `ci.yml` records the decision — Vogt has never deployed from CI (NFR-D10). **The artefact now exists**: on 2026-08-14 the `stack-image` job ran for the first time, built `engine/Dockerfile` with the repository as its context, ran `vogt --version` and `mydevenv2-server --help` inside the candidate, signed the digest and published `dev` and `dev-ee18adc`; `deploy/vogt-stack.compose.yml` pins that digest rather than the placeholder it carried. So the image a dev stack would run and the image `dev` builds are the same artefact, and it is a real one. **What is left is one human act** — `docs/DEPLOYMENT.md` §9.4 — and until it happens no stack has run this image and mobile, voice and push remain unvalidated. | **High**, and now blocked on a deploy rather than on a build. Everything this section can do for it has been done |
| NFR-D12 — "only `main` deploys to prod" | Vacuously true, per the row above. `main` builds a `sha-` image and publishes it; a human pins a digest and deploys. | — |
| NFR-C6 — "shall run both halves on every push" | On pushes to `main` and `dev`, and on pull requests — a push to any other branch with no PR open runs nothing. Within that, each half runs only when its own paths changed: a `mobile/`-only push runs no Rust and no Python; a `web/`-only push runs no APK build. This is NFR-C1 working as intended and NFR-C6's literal sentence being false; the reduction is deliberate and argued in the workflow. | Low, and honestly documented in the file |
| NFR-C6 — a signed APK | The APK builds unsigned with Gradle's debug key, pointed at `127.0.0.1:8910`, and the workflow calls it "a build artifact [that] points at nothing". **It does now build** — `the Android shell assembles` ran on a self-hosted runner on 2026-08-14, after the engine job it is gated behind stopped failing — so what is short is the signing, not the build. The keystore lives in the retired forge; where a signed APK is published is an untaken decision, not an oversight. | Low until there is somewhere to publish one |
| NFR-S5 — "long lists virtualize" | One of three does. The backlog truly windows — fixed row height, `ResizeObserver`, a sliced window with overscan. The audit browser pages, deliberately, because an audit row is variable-height and a reason is never truncated. The board **caps** at 60 cards per cell with an explicit "+N more"; all loaded items stay in memory and in the reactive graph. | Board virtualization outstanding, and named as such since M11 |
| NFR-S5 — "the board's filter and drag paths do not degrade with backlog size" | Unevidenced, and there is reason to doubt it: the cell and column projections are linear scans over the whole loaded set, run per cell and per column, over as many as 2,000 items. `tests/test_benchmark.py` is a server-side query tripwire, and the PWA's tests run in a jsdom with no layout and estates of one to three items, so neither says anything about this. | Medium — it is the clause with no test and no argument |

**FR-E7's second clause, and the thing its author chose not to build.** The
requirement says a bound run's findings "shall be recordable as Vogt
observations, not only as push notifications", and the previous pass judged
that it "needs a new write plane, not a field". What was built is not a write
plane: the run's finding is recorded *on the run*, in the engine, and Vogt
**pulls** it on the next sweep as an `agent_task.run` observation against the
subject the task was bound to. The push still fires; this is "not only", not
"instead of".

What is deliberately absent is a run filing an observation of its own
choosing — arbitrary kind, arbitrary subject, arbitrary payload. **That is a
decision, and this section counts it as one**, for a reason the requirement
did not name and `SCHEMA.md` §1 does: nothing writes `observed.sqlite3`
except a collector, and that rule is the only thing that makes freshness and
coverage mean anything. A second writer with no sweep behind it produces rows
whose absence is unreadable — an observation that never arrived and an
observation nobody looked for become the same answer. The clause asks that
findings be *recordable as observations*, and they are; it does not ask for a
particular mechanism, and the mechanism chosen keeps the store's one
invariant. What it costs is worth stating plainly: a run's finding is a line
of free text through the notify phrase it was already using, with no
structure and no kind of its own, and an agent that wants to say something
structured about a work item has the write plane it always had — the declared
side, through an audited operation, with an actor and a reason.

### 6.2a Implemented, and asserted by nothing

Sixty-four conjuncts whose code was read and believed, which nothing in any
suite would notice the loss of. (Sixty-five, less NFR-D12's, which the fourth
pass asserted: `test_the_two_streams_are_kept_apart` is parametrized over the
core-only job *and* the merged one, so the branch split is now checked for
both images rather than for neither.) They are not defects; they are the places a
regression would be silent. Grouped by ID because the remedy is the same in
each — a test, in the language the code is written in.

**This bucket more than doubled, and that is the test runner working rather
than the product regressing.** Thirty-nine conjuncts arrived from §6.2b: with
`web/src/__tests__/` in place, a GUI claim that could be asserted and is not
is untested, not unverifiable. Every row below marked *(from §6.2b)* is one
that stopped being the demo's job and became a test somebody has not written.

| ID | The conjuncts | What would assert it |
|---|---|---|
| FR-E1 | Multiple concurrent clients per session | A second `ws_attach` that does not close the first — the one multi-attach test closes the first socket before opening the second, so concurrency is exercised nowhere |
| FR-E2 | The activity state is published on the server-wide SSE stream | A test that reads `/api/events`; today the only activity assertion polls `GET /api/sessions/{id}` |
| FR-E4 | The brief carries the item's relations | An assertion in `test_the_work_items_brief_travels_with_the_session`, which today checks the ref, title, body, session id and token variable |
| FR-E9 | The engine degrades to plain sessions; session availability is never lost | A test that names the property. Every session test in `integration.rs` runs with `vogt_core_url: None`, so it is exercised on every run and would fail loudly if it broke — but a reader looking for the requirement finds nothing, and the day someone gives that fixture a core the coverage vanishes silently |
| FR-T1 | The curated set is the eight operations FR-T1 names | A test that names them. The existing one compares `CURATED_READS.len()` to itself and would pass if `compliance` were deleted |
| FR-T2 | One pending action at a time; a new user message supersedes; the 120-second expiry; voice cannot approve | Three Rust tests and one front-end test. There is a front-end test runner now — `web/package.json` has `test` — so the reason this row gave for itself has expired and the row has not. `Assistant.tsx` **is** mounted now, by `assistant.test.tsx`, but only around the microphone: the pending-action rules — one at a time, superseded by a new message, expired at 120 seconds, and never approvable by voice — are still asserted by nothing on this side |
| FR-T6 | Absent the API key the assistant routes answer 404 | One request to `/api/assistant/*` in a test that already boots with `assistant_api_key: None` and never asks |
| FR-T6 | Every GUI hides the surface, the route included *(from §6.2)* | A mount of `App.tsx` at `#/assistant` with `assistant_enabled` false. The route is gated now — the same condition the drawer button carries, read reactively so a deep link still opens once the config resolves — and the gate that was missing was found by an audit rather than by a test, which is the second time in this product for this exact shape (§6.3 findings 3 and 7) |
| FR-M2 | `waiting-for-input`, `errored`, and the agent-task notify hook are routed | A test that drives `spawn_activity_watcher`; the drift watcher has unit tests and these two, which are the headline notifications, have none |
| FR-S9 | The audit records real actors across the hop; FR-S4's double gating is unweakened behind the proxy | A test that drives a real vogt-core through the front door. Every engine test uses a stand-in core that approves everything, so the second gate is asserted on its own and never behind the first |
| FR-U5 | Description, comments, relations, labels, collected evidence, and the start-session control, on one page *(from §6.2b)* | A mount of `WorkItemDetail` asserting each panel is there. `workItemDetail.test.tsx` mounts it and asserts the inline edit and the outage states, and nothing about what the page contains |
| FR-U6 | The explainable `why` on the ranked views *(from §6.2b)* | An assertion that the `why` panel renders the contributions `GET /why` returned. The harness answers that route and no test looks at what was drawn with it |
| FR-U7 | Brief, CI status, contract and compliance, drift inbox, dependency graph, import form on a project page *(from §6.2b)* | A mount of `Projects`. `absentStates.test.tsx` mounts it for the outage cases and incidentally proves the page survives a per-panel failure; nothing asserts the six panels exist when Vogt answers |
| FR-U10 | Session activity updates from the SSE stream without a refresh; the board reloads on `vogt-changed` | A test that drives `onVogtChanged`. The board subscribes to it (`Board.tsx`'s `onMount`) and the front door republishes the core's cursor onto the stream (`vogt_core.rs`), so both ends are asserted and the join between them is not |
| FR-U10 | A lost stream is indicated and reconciles on reconnect *(from §6.2b)* | A stubbed `EventSource` that closes and reopens. jsdom has none, and the harness stubs `fetch` only |
| FR-U11 | Project, work item, session and audit query addressable by URL *(from §6.2b)* | A mount of `App.tsx` — not of a surface. The surfaces restore from their own URLs and that is now asserted, but `App.tsx`'s URL→tabs effect is what turns a pasted link into an open surface, and it is the half M11 found broken for every surface and the half nothing mounts |
| FR-U13 | Swimlanes by project or initiative; per-column WIP counts; collapse/expand of lanes and columns; layout persisted per client *(from §6.2b)* | Four assertions on the board, which is already mounted twenty-eight times in `board.test.tsx`. The swimlane *mode* is asserted as a filter value and the grouping it produces is not; `data-wip` is rendered and read by nobody |
| FR-U16 | Sessions by fuzzy name *(from §6.2b)* | A palette test with a populated session store. `commandPalette.test.tsx` asserts projects, work items and views, and the session entries come from the engine's store, which no test seeds |
| FR-U18 | Both sides of a disagreement, with provenance and age, rendered open before any act is possible *(from §6.2b)* | A mount of the drift inbox against a proposal carrying an evidence snapshot. That bulk accept cannot arrive is asserted from source; that the evidence is *shown first* is not asserted anywhere |
| FR-U19 | Actor and operation filtered server-side; every row shows who, what and why; the item page links in pre-filtered *(from §6.2b)* | A mount of `AuditBrowser`, which `absentStates.test.tsx` already does for its outage states and never with records in it |
| FR-U20 | The live activity badge, the open-terminal control, and the terminal's link back to the item *(from §6.2b)* | A work item with a session on it. The harness deliberately 404s engine paths, so a test of this one has to stub the engine as well as Vogt — which is a fixture nobody has written, not an environment nobody has |
| FR-U21 | Engine unavailable → Vogt views keep answering and session controls disable with the named reason *(from §6.2b)* | The mirror of the outage tests that exist. Every one of them takes Vogt away; none takes the engine away, which in a test is no harder and in the product is the only half a person can reach |
| NFR-D11 | The engine serves the PWA; vogt-core binds loopback only; a port that serves MCP also serves plain HTTP health | The loopback binding is enforced in `engine/deploy/entrypoint.sh`, which no test reads and which does not fail on a non-loopback URL — it silently declines to start a core. The Rust side does no validation of `vogt_core_url` at all |
| NFR-C6 | fmt, clippy, `cargo test`, `pnpm typecheck`, `pnpm test`, the APK build and pytest are all in the pipeline | The only test that reads `ci.yml` checks `runs-on:` lines. `pnpm test` runs in the `engine` job, before the bundle is built and gated by the same path filter, on the argument that `assets.rs` already makes a `web/`-only change an engine change |
| NFR-Q6 | Both suites pass in the merged repository | They are separately path-gated, so "both" is jointly checked only on a change that touches both trees or a shared file |
| NFR-S5 | No view fetches the whole estate to render a page of it | True on inspection of all four surfaces, and the board's 2,000-item bulk read across four sequential requests is the weakest case rather than a clean one |

### 6.2b Unverifiable in this environment

**Eight conjuncts, down from sixty-nine.** Not doubted — read, and believed,
and unprovable here. Each row names what would settle it, because "run the
demo" is not a plan and a list of what the demo has to show is.

The sixty that left are the measure of what the PWA's test runner bought.
Twenty went to §6.1, because a surface mounted in jsdom against a fake front
door settles what the surface does with the server's answer. **Thirty-nine
went to §6.2a**, because that is where a claim goes once it *could* be
asserted and is not — nearly twice as many as were settled, which is the
honest ratio and the one worth remembering when the next runner arrives. One
went to §6.2 (FR-U17's provisional clause), because becoming checkable is how
a believed thing gets found out.

What is left needs a device, a layout engine, or a deploy — nothing here is
waiting on somebody deciding to write a test.

**"Needs hardware" was the wrong phrase, and the fourth pass retires it.** It
described this container and quietly implied the estate. The Docker daemon
and the Android SDK this environment lacks are both present on the
self-hosted runners the product already builds on, and on 2026-08-14 they
produced a merged image and an APK. What remains genuinely needs something no
runner has: a phone in a hand, a speaker, a browser with a layout engine, and
a person choosing to deploy.

| ID | The conjuncts | What would verify them |
|---|---|---|
| FR-M1 | The Capacitor shell loading the merged PWA, and MVP1's terminals, assistant with voice, push, and backlog/board read | An APK, built against a real `VOGT_ANDROID_SERVER_URL`, installed on a device. **The APK is no longer the missing part**: `cap sync` and Gradle have both run — locally as validation, then on a self-hosted runner, where `the Android shell assembles` passed on 2026-08-14. What it points at is `127.0.0.1:8910`, so what is missing is a build against the deployed URL and a phone to install it on |
| FR-M3 | The Vogt surfaces at phone width | The same APK, or a browser at 375px. jsdom loads no stylesheet, so `test_pwa.py`'s three assertions about the board's narrow-breakpoint rules remain assertions about `styles.css` as text |
| FR-T5 | Spoken replies | A device with a speaker |
| NFR-D11 | One stack — the two processes coming up together, under one supervisor, in one container | **The image half is settled.** `build.yml`'s `stack-image` job ran, so `docker build -f engine/Dockerfile .` is no longer hypothetical: it builds, and the step that was called the riskiest unproven one — copying uv's standalone CPython between build stages — holds, because `vogt --version` runs in the built image and could not without it. `mydevenv2-server --help` runs too, so both halves are present. The published-port claim moved to §6.1 when a test began reading the compose file. What is left is the thing neither a build nor a file can show: `entrypoint.sh` starting vogt-core beside the engine, the engine finding it on loopback, and `/readyz` reporting a core it actually reached — which needs the compose stack up (`DEPLOYMENT.md` §9.4) and `scripts/smoke_merged_stack.sh` pointed at it |

### 6.3 What this pass found that no requirement names

The first three were found by running the two halves together and looking at
what came out; the rest by this audit and the one after it. All of them share
a shape — **working behaviour and a false record** — which is the failure
class this product exists to make visible.

Findings are struck through when the thing they describe is fixed *and* the
fix was checked here — **eight are** (4, 5, 7, 11, 12, 13, 14, 15), one of
them (5) only in part, and two of the remaining eight (6, 8) have closed a
half each. Sixteen entries.

**Three of those strikes were added by the fourth pass to findings that were
already fixed** — 12's string comparison had become a component comparison,
13's delimiter test had been rewritten to read both source files, and 15's two
comments had been corrected — and the list still described the code as broken.
The §6.1 row citing 13 named a test that no longer exists under that name.
That is this list committing the failure it is a list of, twice over: a record
that stopped being true while the code improved, and a citation that would
survive a grep because the *ID* is still there. It is the argument for the
rule §5.4a already makes — check the conjunct against the source, never
against the row that describes it, including when the row is in this file.

The rate at which this list is worked down and the rate at which it grows are
similar, and the new entries are all of the same kind as the old ones.

1. **A session's credential was silently replaced** by the pod's, in three
   places (`ROADMAP.md` M10 as built). Writes succeeded; the audit named the
   wrong actor.
2. **Six columns in the legacy GUI were em dashes on every row** — field
   names that were never right, and two columns removed from the product by
   r2. An em dash is also how that GUI says "not collected".
3. **ContextKeeper's two effectful posts were ungated**, so a read-only
   token could start a terminal, while every other write in the engine was
   capability-gated.
4. ~~**`SCHEMA.md` §2.6 documents FR-E6 as implemented.** It states that
   session outcomes "are collected as observations with freshness and trust",
   and gives that as the reason there is no outcome column — reasoning from a
   thing that does not exist to a schema decision that does.~~ **Resolved,
   and checked on both sides**, which is what resolving this one required.
   The document is corrected: `SCHEMA.md` is at r5, the paragraph now says in
   as many words that it "described a mechanism that did not exist until r5",
   and it names this finding as what caught it. And the mechanism exists:
   `src/vogt/collectors/session_outcomes.py`, registered when an engine is
   configured, with `session:` and `task-run:` subject keys added to §3.1 and
   twenty-one tests. A correction to the document alone would have left the
   finding standing, because the finding was never about the wording.
   *Nothing in CI reads that file*, which is the same finding §5.3 made about
   it, and that half is still true.
5. ~~**The `:dev` image stream and the merged stack are different
   artefacts.**~~ **Resolved as an artefact question, and open as a
   deployment one.** `build.yml`'s `stack-image` job now builds
   `engine/Dockerfile` and publishes a `vogt-stack` repository under the same
   GHCR org, which `deploy/vogt-stack.compose.yml` pins; the core-only
   `vogt` repository keeps its own stream, two repositories rather than two
   kinds of tag in one, argued in both files. So there is an artefact to pin
   and it is in the registry the compose file names. What is unchanged is
   that nothing has been built or deployed: the pinned digest is still
   sixty-four zeros, and the job has never run (NFR-D12 above).
6. **`entrypoint.sh` does not fail on a non-loopback core URL.** It derives
   the core's listen address from `VOGT_CORE_URL` so that "loopback only,
   never published" is enforced rather than commented — but the non-loopback
   branch declines to start a core and keeps proxying. The stack then serves
   a front door with nothing behind it, reports the core as absent, and stays
   ready, because the core's readiness check is deliberately not fatal
   (M9 as built). Two correct decisions composing into a silent misconfigure.
   *Half of this closed*: a `VOGT_CORE_URL` with **no port** now exits 78 at
   boot rather than proxying into nothing, on exactly this reasoning. The
   non-loopback branch is unchanged and is now argued in the file as a
   legitimate two-service topology, which is a fair reading — a host that is
   not loopback is a choice somebody made, and a URL with no port is a typo.
   The silent case that remains is a host somebody meant to be loopback and
   misspelled.
7. ~~**The assistant tab is reachable with no API key.** `assistant_enabled`
   gates the drawer entry and not the route, so `#/assistant` renders the
   surface and 404s into a toast. The same shape as finding 3: a gate on the
   way in, and a second door.~~ **Resolved.** `App.tsx`'s URL→tabs effect
   carries the same condition as the drawer button, read reactively so a deep
   link opened before the config resolves still lands. Nothing asserts it
   (§6.2a).
8. **One model-visible path still carries remote text outside the
   delimiters** — down from three, and the one left is the one that matters
   most. `list_sessions` is now wrapped in `<session-list>` and tool errors in
   `<tool-error>`, both with the reasoning written down: the rule is about
   provenance, not about which tool produced the string. But
   `deliver_vogt_write`'s failure path is still
   `format!("not delivered: {reason}")` with no wrapper, and that `reason` is
   the core's own sentence coming back from `vogt.call` — the same text that,
   on the *success* path two lines above, is delimited. The commit that closed
   the other two is titled "Everything the model did not author now arrives
   delimited", which is the claim this finding exists to check and is not
   true. `write_token`'s refusal and `deny_pending_locked`'s are also bare and
   matter less: those sentences are the engine's own.
9. **NFR-PO2's "separately marked layer" does not exist.** `pyproject.toml`
   declares `--strict-markers` and no `markers` list, and no test carries a
   forge marker. The forge-less run is forge-less by construction — nothing
   provisions a token — which is a stronger property than the one the
   requirement asks for and is not the one it asks for. §5 counted NFR-PO2
   delivered.
10. **The board tells its reader that Vogt cannot push it changes, and a test
    now holds that sentence in place.** The comment this finding was raised
    against — `Board.tsx`'s header, claiming `events.list` "is not in
    `vogtApi.ts`'s route table, and nothing proxies it into the SSE stream" —
    has been corrected, and the header now describes the follower accurately.
    The *user-visible* sentence twelve hundred lines below it has not:
    `.board-note` still reads "Freshness is polling — Vogt does not yet
    publish a change stream to this client", while the same file's `onMount`
    subscribes to `vogt-changed` and `vogt_core.rs` asserts the front door
    republishes it. Worse than before: `board.test.tsx`'s freshness test
    asserts that string, so the false sentence is now load-bearing and a
    correction fails a test. The comment above the code was fixed and the
    copy on the screen was not, which is the same document-versus-build shape
    §5.3 found in `SCHEMA.md`, moved one layer out.
11. ~~**NFR-I6 belongs to no milestone.** §4 requires each milestone to name
    the requirement IDs it delivers, and every other r9 ID appears in some
    stage's `Delivers` line. NFR-I6 appears in none — which is why nothing
    built it, nothing claimed it, and this section had no row for it until
    now.~~ **Resolved**: M14's `Delivers` line reads "NFR-I6 (unclaimed by any
    other stage)", and the ID was built within the week — which is the
    argument for §4's rule rather than a coincidence. *§4's rule is still
    enforced by nobody*: no test reads `ROADMAP.md`, and the only check on the
    pairing is somebody sitting down to do this.
12. ~~**`workspace_agreement` compares two paths as strings.**~~ **Resolved,
    and the fix was the test.** The check compared canonicalised `String`s
    with `starts_with`, so a workspace root of `/srv/work` and an import root
    of `/srv/workspace` agreed — reporting agreement in exactly the case it
    was written to catch. It now compares `Path`s, which compare by component,
    and `a_sibling_directory_is_not_inside_the_workspace` builds those two
    real directories under one parent and asserts the disagreement. That test
    is the point: both original tests used unrelated temporary directories, so
    neither had a name that was a prefix of the other and neither could have
    noticed. It remains silent when `VOGT_IMPORT_ROOT` is unset — correctly,
    since there is nothing to compare, though that is the shape of a
    misconfiguration too, which is why `backup_agreement` (finding 14) says
    what an unset variable would cost rather than only that it is unset.
13. ~~**The delimiter test does not read the loop.**~~ **Resolved, in both
    directions.** The old test asserted that `SYSTEM_PROMPT` contained four
    literal tag strings and that `untrusted` formatted one of them, so a fifth
    delimiter named in no prompt would have passed it — the same shape as the
    `CURATED_READS.len()` comparison §6.2a still names: a test whose name
    states a relationship and whose body compares a literal to itself. It is
    now two tests over an extractor, `emitted_delimiters`, which reads the tags
    out of the *source* of both files that emit them — `assistant.rs` for what
    the loop wraps itself and `vogt_tools.rs` for what came from the core,
    each split at its test module so the tests' own failure messages cannot be
    scanned as evidence. `every_delimiter_the_loop_emits_is_one_the_prompt_names`
    and `the_prompt_names_no_delimiter_that_nothing_emits` then compare the
    two sets each way, so a tag added to either side without the other fails.
    Scanning one file and asserting about both is how the first version passed
    while missing a tag, and the extractor says so in its own comment.
14. ~~**The merged stack's backup is a core-only backup.**~~ **Resolved, and
    then given a check, because setting the variable was the smaller half.**
    `deploy/vogt-stack.compose.yml` sets `VOGT_ENGINE_STATE_DIR` to
    `/home/sprooty/.local/share/mydevenv2`, the path the engine's own default
    resolves to under this container's home — so a backup taken in the merged
    stack covers both halves. What that leaves is two configurations naming
    one directory, which is the shape of every finding in this list: the day
    one moves, `vogt backup` keeps succeeding and starts copying a directory
    the engine does not use, and nobody finds out until a restore. So the
    front door now reports it. `api.rs::check_backup_agreement` publishes a
    `backup_agreement` readiness check — non-fatal, for FR-E9's reason —
    saying either which directory the backup covers or, when they disagree,
    what the archive would silently omit. Three tests in `vogt_core.rs` assert
    the agreeing branch, the disagreeing one, and the single-half deployment
    that must not be called misconfigured; `scripts/smoke_merged_stack.sh`
    checks it before a deploy and `DEPLOYMENT.md` §9.2 lists it. The original
    reasoning stands and is worth keeping: `engine_state_dir` has no default,
    deliberately and rightly, because its absence is what makes a backup
    honest about covering one half. Anywhere it is unset, `vogt backup` still
    writes `engine_state: "not configured"` and skips the VAPID keypair, the
    push subscriptions, the agent-task prompts and the session history — the
    designed behaviour, and the manifest says so. What remains open is the
    third thing the compose file's header names: the estate tree itself is not
    covered by `vogt backup` at all, and never claimed to be.
15. ~~**Two comments in `assistant.rs` describe code that is no longer
    there.**~~ **Both resolved.** The gate's comment now says the branch is
    unconditional and why `assistant_auto_type` was removed rather than
    defaulted; `parse_vogt_write`'s doc comment is back above
    `parse_vogt_write`, and `untrusted` keeps only the FR-T4 paragraph that
    was always about it. Neither changed behaviour, which is the whole reason
    they survived: nothing in a test suite reads a comment. What follows is
    what they said.
    The tool dispatcher's gate still reads "`send_input` with auto-type on is
    the single exception, and it is a configured one about a PTY" — over a
    branch that is now unconditional, in the file whose header explains at
    length that `assistant_auto_type` was removed because a switch made
    FR-T2's justification false. And `fn untrusted` carries
    `parse_vogt_write`'s doc comment ("Turn a model's proposed Vogt write into
    a card a person can approve…"), with FR-T4's paragraph appended to it,
    because the new function was inserted between the old comment and the
    function it documented; `parse_vogt_write` now has no doc comment at all.
    Neither changes behaviour. Both are the failure this section is a list of.
16. **Both first-run CI failures were checks written against this machine.**
    The pipeline for the merged product ran for the first time on 2026-08-14
    and failed twice, in ways review could not see and neither of which was a
    code defect:

    * `docs` failed on `engine/AGENTS.md`'s link to
      `/home/sprooty/Working/AGENTS.md`. `scripts/check_docs.py` resolved it
      happily *here*, because that file exists on this machine — the checker
      answered a question about the filesystem it was running on rather than
      about the repository. A link that resolves nowhere else is worse than a
      name, so the link is now the name, and the checker rejects absolute
      paths outright.
    * The engine job failed on `sudo apt-get install ripgrep` with *root is
      not in the sudoers file*. The step assumed a normal user; the runner is
      root, so there was nothing to elevate from and no sudoers entry to
      elevate with. It now elevates only when there is something to elevate
      from.

    The shared shape is the one this section keeps finding: **a check that
    cannot fail where it is written.** Every assertion about CI in this
    document — the merged image, both image streams, the APK, `pnpm test`
    running at all — was, until that morning, an assertion about a file that
    had never executed, and the value of running it was not the three that
    turned out true but the two that could not have been read. It also
    retires a phrase this section used: what was called "needs hardware" was
    a description of *this container*, and the runners have the daemon and
    the SDK it lacks.
