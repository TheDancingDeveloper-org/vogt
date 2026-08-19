# Vogt — Requirements (v0.3)

Status: **baseline, revision r18** (2026-08-18), distilled from `DESIGN.md`,
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

### Revision r10 — a front door inherits what it fronts

**Nothing here is a new idea. Four requirements were already right and had
stopped being about the product.** The trigger was the first real project
import — RustNZB, on 2026-08-15 — which failed, and then kept failing
differently for four separate reasons. None of them had a failing test,
because each was written when the sentence around it was still true.

**1. FR-A7 and FR-A8 said "the server" when there was one.** r7 established
that a client must be able to obtain the instance's address, and that the
address is an *exposure* value the server cannot infer (NFR-D2): it binds a
container port and is published somewhere else entirely. That reasoning was
correct and is now one hop short. r9's merge put a second process in front of
the first, and every client-facing obligation quietly stayed with the process
that no longer owns the address.

Observed on the merged stack: `/version`, `/connection-info`,
`/health/ready` and `/health/live` are not routed at the front door at all.
The PWA's catch-all answers them — with `index.html`, at **200**. So the
documented way to discover a Vogt instance returns a web page, successfully.
`GET /api/connect`, which FR-A8 exists to make work, renders a ready-made MCP
client configuration pointing at `http://vogt-dev.tailc7d3c.ts.net:8910/mcp`
— the core's loopback-adjacent address, which no client can reach. The
product's own answer to "how do I connect" is a wrong address, handed over
with no error anywhere.

The consequence was not cosmetic. `vogt-mcp-remote` reads
`/connection-info` at startup, got HTML, and died on the JSON parse — so
Vogt's MCP was unusable from every bridge client while the MCP endpoint
itself answered `initialize` and `tools/list` perfectly. Cadastre's bridge,
in the same pod against its own server, worked throughout, which is what made
it look like Vogt was down rather than one route being absent.

So FR-A7 and FR-A8 are revised to bind to **whichever process publishes the
address**, and FR-A9 states the composition rule directly, because the next
process someone puts in front will inherit the same obligations and nothing
currently says so.

FR-A7 also gains the clause the failure actually turned on: **a documented
probe path shall never be answered by a fallback handler.** A 404 is a
correct answer to a path that is not served. A 200 carrying something else is
worse than either, because no client can tell it from success.

**2. FR-A6's rule was right and was scoped to one symptom.** r8 established
that bridge↔server version skew warns and never blocks — the handshake is a
negotiation, not a gate. The bridge honoured that, and guarded an unreachable
server and a non-200 response, and then parsed the body with nothing around
it. One unparseable banner ended the process. The general rule was always the
intended one: **a discovery or pre-flight step shall never prevent a client
from using a service that works.** FR-A6 now says so instead of implying it.

**3. FR-O4 was about coverage and the violation was inside a payload.**
"Absence shall only be asserted within provably swept scope" is exactly the
rule that was broken, one level below where it was written. The production
image shipped without `git`; every `git` call in the `git-local` collector
failed; the helper returned `""` for each; and the collector published
`{"is_git_repository": true, "branch": "", "head": "", "dirty": false,
"dirty_files": 0}` for every registered project. `dirty: false` is a positive
claim that a working tree is clean, made by a collector that never read one,
and it is indistinguishable downstream from a real observation — the precise
harm FR-O4 exists to prevent, expressed as a field rather than as coverage.
FR-O8 already had the right shape for the fix ("shall degrade to `partial`
coverage … never to a failed sweep"); FR-O9 generalises it from one token
scope to any unreadable source.

**4. Nothing required the built artefact to be run.** NFR-Q gates types,
lint, coverage and parity; NFR-C governs what is published. Between them, an
image can go green, be signed, be digest-pinned and deployed while missing a
package the product shells out to at runtime. That is what happened: `git` is
a runtime dependency of both `project.import` (FR-P6, FR-P7) and `git-local`
(FR-O1), the image never had it, and the first thing to notice was a failed
import in production. Reading a Dockerfile cannot catch this; running the
binary in the built image can. NFR-Q7 requires that, and requires the probes
of FR-A7 to be checked **at the published address** rather than against the
process behind it — checking the core would have passed on every one of these
days.

**Method note, in the spirit of §5.4.** Three of the four failures were
invisible to the delivery verification for the same reason: the requirement
was cited as satisfied by an implementation that satisfies it *at one
address, in one topology*. §5.4 already requires requirements joined by "and"
to be verified per conjunct. This revision adds: a requirement about a
client-facing surface shall be verified **at each address that publishes
it**, and a requirement about a runtime dependency shall be verified **in the
artefact that ships**.

### Revision r11 — a design document describes what exists

*2026-08-15. No behaviour changes. What changes is where unbuilt things are
allowed to be written down, and seven of them acquire IDs so they can be
tracked instead of remembered.*

The documentation pass that produced this revision consolidated MyDevEnv2's
eight surviving documents into the Vogt set — the vendor was still describing
itself as a separate product with its own name, its own backlog and its own
deployment story, eighteen stages after it stopped being one. `ENGINE.md` is
now the engine's single reference, `DEPLOYMENT.md` §10–§11 carry its image and
its stacks, and `USER_GUIDE.md` documents one product rather than half of one.
The full list of what moved where is §7's preamble.

**The rule this revision adds.** A design document describes what the system
*is*. A capability that was designed and never built does not belong in one,
because a description of an unbuilt thing is indistinguishable from a
description of a built one, and every reader after the author is the person who
cannot tell. Such capabilities live in **§7**, this document's gap register,
which is the single place to ask "what was designed and is not here".

That is a rule about *placement*, not about candour: the design documents were
in fact honest — they said *not built* and *as built* in place, which is how
this pass could find them at all. What they could not do is let anyone count
the gaps, because the gaps were scattered across five files in the middle of
paragraphs about things that do exist.

**Two kinds of gap, kept apart.** §7 distinguishes them, because conflating
them is how a decision turns back into a task:

- **Owed** — a numbered requirement no code meets. It keeps its ID, or gets
  one here if the design specified a capability nobody had ever numbered.
- **Withdrawn** — designed once, deliberately not adopted, and owed to nobody.
  These carry no ID *on purpose*; minting one would make a closed decision
  look like open work, which is exactly the failure r2 and r3 spent their
  revisions avoiding.

**Seven IDs were appended and one was withdrawn within the hour**, each for a
capability some document specified and no requirement had ever named. Per §4,
IDs are append-only and each continues its family from its current maximum
(FR-D8, FR-E9, FR-M3, FR-S10, FR-V4 as of r10):

| New ID | What it names | Pri |
|---|---|---|
| FR-D9 | Declared dependency edges — the `declared` `RefKind` nothing produces | C |
| FR-E10 | GUI streaming operable where a deployment enables it | C |
| FR-E11 | Two live sessions shall not silently share one working tree | C |
| FR-M4 | An FCM client entry for the dev shell's application id | S |
| FR-S11 | The `writeback` scope shall gate the write-back it names | S |
| ~~FR-S12~~ | ~~A work item's audit filter shall reach its comments~~ **Withdrawn (r11)** — already delivered; see below | — |
| FR-V5 | Ranked views shall be pageable past their first page | S |

> **Status of this table, later the same day**: r13 delivered FR-S11 and FR-V5,
> and found FR-M4 already delivered before r11 raised it. FR-D9, FR-E10 and
> FR-E11 — the three `could-have`s — are what is left, and §7.1 governs.

None of the *new* IDs is a must-have. Two must-haves in the register are older
than this revision and were carried in from §5.1 — **NFR-I3** and **FR-L1** —
and NFR-I3 is the one row here with operational consequence: an image carrying
a new migration passes its healthcheck and fails afterwards on a missing table.
It was missing from the register's first draft, which is its own small lesson,
since §5.1 has called it "the highest of these" since 2026-08-12. **Both were
delivered at r13**, which is the revision immediately above this one.

**Three of this revision's own claims were wrong, and the method that caught
them is §5.4a's.** The first draft of §7 was assembled from what the other
documents *said* was missing rather than from the source, which is the exact
mistake §6's opening warns about — and it produced three bad rows:

- **FR-S6 was listed as owed. It is delivered.** §5.1's own row has said so,
  struck through, since r7: `ListAuditParams` carries `since`, `until`,
  `project` and `offset`, the interval is half-open so consecutive windows tile
  the log exactly, and `tests/test_audit_query.py` has twenty-three tests. The
  row was copied from `README.md`'s status paragraph, which was itself stale.
- **FR-S12 was minted for something that exists.** `ROADMAP.md` M11 said a
  per-item audit filter misses comments; `declared.py`'s trail query is a
  semi-join — `a.entity_id = ? OR (a.entity_kind = 'comment' AND a.entity_id
  IN (…))` — and `test_a_comment_appears_in_its_work_items_audit_trail` and
  `test_one_items_comments_stay_out_of_another_items_trail` cover both
  directions. The roadmap note was true when written and nobody revisited it.
  The ID stays, struck through, per §4: it was published, and an ID that
  silently disappears is worse than one that records a mistake.
- **FR-M4 claimed three conjuncts and only one is short.** The distinct
  `applicationId` is built — `build.gradle` reads `MYDEVENV2_ANDROID_APP_ID`
  and says in a comment what it is for — and so is the per-build front door
  (`VOGT_ANDROID_SERVER_URL`, no default). What is missing is the FCM client
  entry for the dev id, because `google-services.json` is keyed to the
  application id. The requirement is narrowed to that.

The lesson is the one this document keeps relearning and is worth stating a
third time: **a gap register assembled from other documents inherits their
staleness.** Every row in §7 has now been read against the source. Where a
document and the code disagreed, the code won, and the document was wrong three
times out of twelve.

**One correction of record.** `ROADMAP.md` M12 said FR-T7 "was not attempted".
It was: `engine/server/src/assistant.rs` refuses a `claude-*` model id on the
OpenAI-compatible transport with a sentence naming the model, the transport and
the setting that overrides it — which is the second of the two ways FR-T7
offers out of the hang. The native Anthropic backend, its first way, is still
absent. The roadmap now says both halves, because "not attempted" understated
the work by exactly the amount that would have caused someone to redo it.

### Revision r13 — the schema gate closes, and `migrate` is a verb

*2026-08-15. Two must-haves delivered: **NFR-I3** and **FR-L1**. Both had been
open since M4.*

NFR-I3 was the only row in §7 with operational consequence, and it was three
holes wearing one ID:

1. **`serve` did not migrate.** The deployed topology runs `command: serve` and
   never runs `init`, so the one entrypoint production uses was the one that
   left the schema alone. `build_server` now migrates both stores before it
   assembles anything — in `build_server` rather than in `run`, because a fix
   only the production entrypoint exercises is one nothing can catch
   regressing.
2. **`/health/ready` compared nothing.** It reported the *applied* schema
   version, which is a fact about the disk that answers no question a deployer
   has: an image whose migration had not run looked exactly like one whose
   migration had. Stores now report `bundled_schema_version()` — read from the
   migration files that shipped, never a constant somebody has to bump — and
   the probe answers `503` when either store is behind, naming the store, both
   numbers, and the verb to run.
3. **There was no verb.** `init` had always done the work, so the thing an
   operator reached for after a digest bump was a word that reads like "start
   over" on a live data directory. `migrate` is now an operation
   (`FR-L1`), local-only for the same reason `init` is, and it refuses an
   empty data directory rather than conjuring an instance — `init` creates,
   `migrate` moves forward, and collapsing them would make the
   destructive-sounding verb the safe one.

**A store *ahead* of the build stays green**, deliberately. `migrate` refuses
that case with the forward-only message, which names the migration and says to
restore a backup or deploy the newer build; a probe that also went red would
send an operator to the healthcheck for a diagnosis only the migrator can give,
and would make a rollback look like a broken container.

`tests/test_schema_gate.py` (eleven) is the evidence, and both halves were
mutation-checked: deleting the two `migrate` calls in `build_server` fails the
server test, and disabling the comparison fails all three probe tests. The gate
is parametrized over both stores, because a check that read only `declared`
would pass a deploy whose observed migration had not run.

**Four should-haves close with them**, leaving §7.1 with five rows, none of
which can be closed by writing code here:

- **FR-G1 — the contract is configuration.** Four settings
  (`contract_required_files`, `_dirs`, `_meta`, `contract_version`) replace the
  constant, read through one helper so a call site cannot pick up three and
  miss the fourth. The interesting part is what a *version* means once the
  rules are editable: an operator who edits them and leaves the version at `v1`
  would record statuses claiming to be the stock contract, and every comparison
  across instances after that is wrong with nothing on screen to say so. So a
  contract whose rules differ from the built-in default while still carrying
  the default version gets a digest of its own rules appended — `v1+3f9a2c` —
  and a contract the operator *named* keeps its name untouched, because that is
  the explicit act the digest is inferring in its absence.
- **FR-S11 — `writeback` gates something.** It now gates `forge.writeback`, the
  operation that arms a project's upstream pushing, and is deliberately not
  implied by `project.write`: managing projects and deciding this instance may
  speak to a forge on your behalf are different powers, and only the second has
  effects outside the instance. `tests/test_auth.py` asserts both directions,
  and a second test asserts that **every** scope gates at least one operation —
  which is the check that would have caught this in M5 and did not exist.
- **FR-V5 — ranked views page.** `backlog` and `bugs` take an offset;
  `total_considered` is unchanged by the slice, so `offset + len(items) <
  total_considered` is the whole of "there is more". **One limit survives and
  is not what the requirement asked about**: `RANKING_CANDIDATE_LIMIT` caps the
  candidate set at 1,000, so paging reaches the end of the *ranked window*
  rather than the end of the estate. That cap predates this and is deliberate —
  scoring happens in Python — but it means "pageable past the first page" is
  now true and "every open item is reachable in ranked order" still is not.
  §7.1 no longer carries FR-V5; the residual is named in §7.4.
- **NFR-S4 — the benchmark runs at the envelope, and the reason it did not was
  wrong.** The fixture sat at 5,000 items against NFR-S1's ~100k, and the
  argument recorded in the test was that seeding 100k "would take minutes".
  Nobody had measured it. It takes about two seconds — the rows go in through
  one transaction on the direct write path, not the audited one. The envelope
  is now the default and `VOGT_BENCHMARK_SCALE=tripwire` is the opt-out, which
  is the right way round: a smaller fixture should be something somebody asks
  for, not something they get without noticing.

Three of those four were closed by writing code. The fourth was closed by
running a command that had been described as too slow to run, which is the
second time this documentation set has recorded that shape and the reason
§6 has a paragraph about it.

**FR-M4 was already delivered, and this register's row for it was the fourth
wrong one.** It claimed the dev shell had no FCM client entry. It has one:
`google-services.json` carries `com.sprooty.mydevenv2.dev`, CI exports
`MYDEVENV2_ANDROID_APP_ID` for a non-prod APK stream, and both `build.gradle`
and `capacitor.config.ts` read it. (When this was written the export was
`engine/.woodpecker/server.yml`'s `mobile-apk-dev` step. That file was the
fork's vendored copy of the engine's Woodpecker pipeline, it never ran against
this repository, and deleting it moved the export to `ci.yml`'s `android` job
— which is a better answer than it looks, because the id is now set by a build
that actually runs.) The row
came from `uplift.md`'s mobile caveat, which was true when written and was
fixed afterwards — the same failure as FR-S6 and FR-S12, from the same cause,
which is that the register's first draft read documents instead of source.

What was genuinely missing is that **nothing asserted the three files agreed**.
`build.gradle` carries the comment "Keep in sync with capacitor.config.ts",
which is the shape of a rule nobody can enforce: two toolchains read the two
files, and a disagreement surfaces as an APK that installs fine and silently
never registers for push — indistinguishable from a broken push service.
`tests/test_mobile_identity.py` (five) is the check that comment was asking
for, including one that fails if CI ever builds every stream under the prod id.

**FR-T5 narrows to the part that needs a room.** The prompt sentence standing
in for the validation pass — that work items are `WI-7` and projects are slugs
— was implemented and asserted by nothing, so deleting it would have broken no
build and degraded voice into uselessness at the one input the domain is made
of. Three tests in `assistant.rs` now pin it and the spoken-reply instruction
beside it. The validation pass itself still needs a person, a microphone and a
device, and that is now the whole of what FR-T5 is short.

Both of those were reached by checking rather than believing — the Rust
toolchain and the Firebase manifest were both present, and both had been
written off in the previous pass as things this environment could not reach.

### Revision r12 — a second model vendor is a choice, not a capability

*2026-08-15. One clause deferred. No behaviour changes.*

FR-T7 was two requirements joined by "and": **refuse rather than hang**, and
**support a native Anthropic backend**. The first is delivered and stays. The
second is deferred, and the reasoning is worth writing down because a deferral
that reads as neglect gets reversed by the next person who notices it.

The requirement was raised because the assistant's only transport was
OpenAI-compatible and `claude-*` routes through the configured proxy hung.
**The failure that mattered was the hang, not the vendor.** A hang is the worst
thing a chat surface can do, because it is indistinguishable from thinking, and
the 60-second client timeout that used to catch it reported "took too long"
about something that was never going to answer. That is closed: the route
refuses, names the model, the transport and the setting that overrides it, and
`assistant_allow_claude_proxy` lets a deployment whose proxy does serve those
routes say so and own the result.

What the second clause would buy, once the first is delivered, is a **choice of
vendor** rather than a capability. Nothing in v2 depends on Anthropic
specifically; the loop works against whatever OpenAI-compatible backend a
deployment configures. What it would cost is a second transport with its own
tool-call semantics, its own streaming shape and its own failure modes, kept
working by the same tests, in a product whose assistant is one surface among
eight. The fault that motivated it is also *a proxy's*, not this product's —
and building a second client to route around somebody else's broken route is
the kind of fix that outlives the problem.

So it moves to §3, where it is a decision with a reason rather than a `C`
nobody schedules. Reversing it needs one thing: a use that wants Anthropic
tool-calling specifically, at which point the ID is still here and the argument
above is what has to be beaten.

### Revision r14 — a tracker first, and a zero that says which zero it is

Two changes, from onboarding eight real projects and reviewing what the
instance then said about them. Both are corrections toward what this document
already says, not new direction.

**The contract reads as a verdict on repositories nobody opted in.** r2 removed
the contract *gate* and was right to; §2.1 states the posture as "reporting, not
enforcing", and FR-G13 forbids any operation consuming compliance as a
precondition. That much holds. What r2 did not do is give the reporting an
inside voice. Eight projects were checked during onboarding and eight failed —
every one on `design/`, every one on `LICENSE` — and the only word the product
has for that is `non_compliant`. A criterion that nothing in an estate satisfies
is not a finding about the estate; and a user who registered a project to track
issues, wanting none of the estate machinery, is told their repository is
non-compliant with a template they were never offered.

The template exists and is good: `default_scaffold` writes the minimum, says in
each file what it is for, and pointedly refuses to choose a licence on the
owner's behalf. It is reachable only from `project create`, into a new
directory. So the product is generous to a project it creates and merely
critical of one it is handed — which is the same asymmetry r2 removed at the
gate, surviving in the reporting.

r14 makes the contract **opt-in per project**, and pairs every reported gap with
the means to close it: the same scaffold, runnable against a registered project,
and a recommendation an agent can act on. Vogt should be usable as an issue
tracker by someone who never adopts a single one of its conventions.

**An empty answer must carry the same provenance as a full one.** FR-O4 already
requires this of coverage — "absence shall only be asserted within provably
swept scope" — and r10's FR-O9 already requires it of observation payloads, in
terms that name the failure precisely: *a default that happens to be falsy is an
assertion*. Neither reaches the results of operations, and that is where it was
observed failing:

- `forge onboard` against a Forgejo remote returned `issues: 0 … detail: null`
  for a repository that has an open issue. Byte-identical to an honest empty.
- `coverage` reported `projects: 1` with eight registered, because the count is
  scoped to the last sweep and does not say so.
- A collector's aggregate `status` read `failed` or `partial` for one underlying
  fault, according only to how the sweep was scoped.

r14 extends FR-O9's principle from payloads to operation results, and adds the
mechanism that makes it enforceable rather than aspirational: an adapter
declares what it can read, and a subject it cannot read yields `not_supported`
rather than zero. This is the pre-1.0 window for it. REST, CLI, GUI and MCP are
generated from one transport-neutral operation registry, so changing a result
shape is one change today and a four-surface breaking change after 1.0.

### Revision r15 — the places plan becomes an acceptance boundary

*2026-08-17. Stage 0 of `RESTRUCTURE.md` turns the reviewed places, Inbox,
Sessions and phone uplift into an owned contract. The drawings and the plan
remain proposals; the IDs below are the work that is now owed.*

Seven requirements are appended using the next free number in each family.
They cover the server projection before its surface, the shared triage writes,
the places/panes shell, the Inbox interactions, measured content-sized views,
the existing assistant gate's presentation, and phone reachability:

| New ID | What it names | Pri |
|---|---|---|
| FR-N4 | A normalized, server-ordered, keyset-pageable Inbox projection over GitHub, drift, CI and agent attention, with coverage and stable material-versioned entry keys. | M |
| FR-N5 | Audited shared Inbox archive, snooze and restore, while local seen state remains presentation-only and material source changes can resurface an occurrence. | M |
| FR-U23 | Stable places, Sessions panes/tools and idempotent migration of the old tab state without losing routes or capabilities. | M |
| FR-U24 | An Inbox surface driven only by FR-N4, with coverage/provenance, evidence-before-action and typed reasons for every write and batch. | M |
| FR-U25 | Content-sized, measured and virtualized Board cards and Backlog rows, alongside NFR-S5's existing estate-scale guarantees. | S |
| FR-T8 | Consistent presentation of the one existing pending action, including reason preview/update for Vogt writes and a subsequent exact-payload approval. | M |
| FR-M5 | Sessions-first phone navigation with four primary places, labelled secondary-route reachability and push deep links to the current pending action. | S |

Three existing requirements are revised in place, with their earlier r9
meaning retained in the rows below and the new conjuncts named here:

- **FR-U7** keeps project-scoped drift summary, age and count, but its action
  link now targets the canonical filtered Inbox; Projects does not grow a
  second resolver or triage surface.
- **FR-M2** adds the `assistant_approval` push kind. Its lock-screen content is
  non-sensitive and the push opens the current action; terminal bytes, Vogt
  payloads and reasons never appear in the notification.
- **FR-U11** names `/inbox`, `/sessions` and `/settings` and preserves every
  existing deep link in the route matrix in `RESTRUCTURE.md` Stage 3.

**NFR-S5 is deliberately not revised.** It continues to require
virtualization, bounded reads and estate-scale interaction; FR-U25 adds the
content-sizing contract without turning an estimate into a fixed CSS height.

The new and revised clauses are owed in §7.1. Their staged delivery is
recorded in `ROADMAP.md`: Stage 0 is the contract gate, Stages 1, 3–8 are the
M11 web work, Stage 2 extends M12's assistant/push work, Stage 9 is M13's
phone pass, and Stage 10 is M14's browser/device/live-stack conformance
close-out. The historical §5/§6 audits are not rewritten to claim these
unbuilt clauses.

### Revision r16 — voice becomes a way of working, and a model is something you ask for

*2026-08-17. Voice-first user journeys, provider profiles and model/effort
selection for spawned sessions are named as requirements. A proof of concept
precedes any of it: `docs/VOICE_POC.md`. Nothing here is delivered.*

Until now voice was an input method bolted onto the assistant (FR-T5): a
push-to-talk button, a spoken reply, adopted unproven. r16 promotes it to a
**way of using Vogt with no screen in hand** — asking what needs attention,
asking about a project's open issues, asking for work to be started, asking
for a fresh session on a named model at a named effort — on the phone and on
the desktop, over the same engine. The reference for the interaction shape is
[voicemode](https://github.com/mbailey/voicemode): STT and TTS behind
OpenAI-compatible audio endpoints, so a cloud provider and a local
Whisper.cpp/Kokoro pair are interchangeable by configuration.

Three things this revision decides, so they are not re-litigated by the POC:

1. **The gate holds.** FR-T2's "voice never approves" is unchanged. A voice-only
   journey that ends in a mutating act — *work on WI-7*, *start a session* —
   ends in a spoken announcement that a card is waiting and one on-screen tap
   (FR-M2 pushes the deep link, so the tap is one screen away on the phone).
   The POC exists partly to find out whether that tap is tolerable in the
   flow. If it is not, the argument for a *narrow* spoken approval (a
   read-back of the exact payload plus a per-action spoken nonce) has to be
   made against FR-T2 in a later revision; it is **not** made here.
2. **"Claude subscription" is the `claude` CLI, not an HTTP backend.** A
   subscription has no API surface this product can call, so the way to spend
   it is the existing `Claude Code (protected)` template — a session, not the
   assistant loop. That is why FR-T9 asks for provider *profiles* rather than a
   second transport, and why the r12 deferral of a native Anthropic backend
   stands. OpenRouter and The Claw Bay are both OpenAI-compatible and are one
   `base_url` apart.
3. **A session still needs a project (FR-E3).** *"Research the best risotto in
   Wollongong"* has none. Rather than a heuristic default cwd — the failure
   FR-E3 exists to prevent — FR-T11 asks for a configured **scratch project**
   that a subject-less spoken request resolves to, named as such in the
   session's name and audit row.

Six requirements are appended and two are revised in place:

| New ID | What it names | Pri |
|---|---|---|
| FR-T9 | Assistant provider profiles: named `{base_url, key, default model, effort}` sets, selectable per deployment and per request; OpenAI-compatible only. | M |
| FR-T10 | A `notifications` read tool in the assistant, over the same projection the Inbox reads (FR-N4), so *"are there any notifications?"* is answered from the server's order, not the model's. | M |
| FR-T11 | Spoken session spawning: `session.start` gains `model` and `effort`, defaults come from the profile, and a subject-less request resolves to the configured scratch project. Gated by FR-T2. | M |
| FR-T12 | Server-side voice pipeline: STT and TTS behind OpenAI-compatible audio endpoints (`/audio/transcriptions`, `/audio/speech`), configurable to a cloud provider or a local Whisper.cpp/Kokoro pair; the existing on-device Web Speech / Capacitor path remains as fallback and is chosen per client. | S |
| FR-T13 | Voice-first journeys as an acceptance list: the five utterances in `VOICE_POC.md` §2 shall each complete by voice alone up to (and only up to) the FR-T2 gate, on the phone and on the desktop, with a domain-vocabulary repair pass (project slugs, `WI-\d+`) between the recognizer and the composer. Absorbs and closes FR-T5's validation clause. | S |
| FR-M6 | The mobile shell shall keep receiving updates while backgrounded — push (FR-M2) as the wake, a foreground service only for the duration of an active voice conversation, and no always-listening microphone. Push arrivals during an active conversation are spoken. | S |

- **FR-T5** is revised: the push-to-talk and spoken-reply clauses stand; the
  "validation pass" clause moves to FR-T13, where it is an acceptance list
  rather than an intention.
- **FR-E8**'s `session.start` grows `model` and `effort` on every surface (CLI,
  REST, MCP) — parity is the requirement, so the assistant is not the only
  caller that can ask.

The POC scope, sequencing and what it must prove are in `docs/VOICE_POC.md`.
Its outcome is expected to revise this section once more: either FR-T13's
utterance list is met and the gate tap is fine, or the tap is not fine and
somebody has to argue with FR-T2.
### Revision r17 — what the onboarding pass filed, and the sweep it asked for

The estate onboarding of 2026-08-17 registered thirty-eight projects and filed
four issues against this repository (#47–#50). Three name gaps in what is
built; the fourth asks for a sweep nobody had run. As with r14, none of them is
new direction — each is a correction toward something this document already
says.

**A relationship the design named and no code ever wrote (FR-D8).** `rustnzb`
vendors seven `nzb-*` crates that are *also* separately published, separately
registered repositories; `rustTorrent` does the same with eleven `librtbit-*`
crates. Eighteen concrete cases, each of which Vogt held as two unrelated
projects that happened to share a name, and each of which somebody had to work
out by hand. FR-D8 has specified the answer since r2 — report the
`mirrored_source` relationship as an observation, compare no contents, assert
no divergence — and `grep -rn mirrored_source src/` returned nothing. It is
built at r15 as its own offline collector, matching on the package name both
manifests declare, which is the identity the two copies already agree on and
the only signal available that is not the content comparison FR-D8 forbids.

Worth recording separately: **§7.1 did not carry FR-D8**, and by its own
admission rule it should have — something was designed, it is not built, and a
reader of `DESIGN.md` §3.5 could reasonably have believed it was. The register
was assembled from gaps that documents *stated*; this one was visible only by
grepping the source for the identifier. A gap register that misses a row is
worse than one nobody reads, so the omission is named here rather than quietly
resolved by the same change that closes it.

**A proposal that outlived the thing that raised it (FR-R6).** `WI-2`'s fix
stopped `dep-refs` misclassifying Cargo dependency inheritance as an unresolved
reference. The thirty-six proposals it had already raised under the old logic
stayed open through the fix, its deploy, a later regression and a re-fix, and
closed only because a person confirmed from timestamps that they predated the
fix and ran `drift resolve --reject` thirty-six times. `drift detect` only ever
*adds*: it raised zero new proposals on that pass, reported five `already_open`,
and never looked at the thirty-six. This is not a one-off — every fix that
changes what a collector reports leaves behind whatever it had already raised.

FR-R6 does not auto-close, deliberately: FR-R2 keeps resolution with a person
or an authorised agent, and FR-U18 refuses even bulk *accept* in the GUI on the
grounds that evidence should be read per proposal. It marks. A proposal whose
condition a later completed sweep no longer reproduces carries `superseded_at`
and a detail naming the sweep, stays open, keeps its snapshot, and clears again
if the condition returns.

`#48` also asked whether the CLI should carry FR-U18's no-bulk-accept
discipline, since a loop of single `drift resolve` calls is what cleared the
thirty-six. **It should not, and the reason is that they are different
mechanisms.** FR-U18 forbids a *button* that resolves many proposals from one
click without their evidence on screen; the CLI has no such verb, and each call
in that loop resolved one proposal, took its own reason, and wrote its own
audit row. What made the loop unsafe was not its shape but that the operator
had to establish staleness by hand — which is the gap FR-R6 closes.

**Two registers and no referee (FR-R7).** `WI-16` mirrored GitHub issue `#44`,
named it in the first line of its body, and was marked done when the fix
deployed; `#44` stayed open for hours. Nothing inside Vogt noticed. What
noticed was a freshly-spawned onboarding agent following the import playbook,
which treats an open `#44` as a hard stop, correctly refusing to proceed. Vogt
already collects `forge.issue` observations and already treats
declared-versus-observed disagreement as the thing drift exists to catch; this
shape had simply never been wired up as one. FR-R7 raises it as its own kind:
read-only, human-resolved, never applying a change.

It matches a **qualified** reference — `owner/name#12`, or an issue URL — and
never a bare `#12`. `WI-16`'s own title reads "Regression from #43", where #43
is a pull request, while the issue it mirrors appears in the body as a URL;
resolving `#n` against the item's project would have produced a proposal about
a PR from a sentence describing history. Precision is load-bearing here,
because a rejected proposal is re-raised by the next `detect` — a false
positive is a recurring cost, not a one-time one.

Building it surfaced a live hazard in the kind next door. `gh-issues` collects
**open** issues, and dedup appends nothing for an unchanged subject, so the
newest observation of an issue closed last month still says `open` — and
`forge_state_mismatch` was auto-accepting in both directions. The reopen
direction therefore reopened finished work from an absence nobody observed. It
is now auto-acceptable only where the evidence is positive (`closed` upstream
→ close the item); the other direction is human-gated and its summary says what
the evidence can and cannot see.

**Three zeros, audited (#50).** WI-9 taught `forge onboard` to say which of its
zeros it meant, and was written against exactly one collector's one failure
mode. #50 asked for the sweep that fix did not do: every surface that returns a
bare count, checked for the same three-way ambiguity — genuinely empty / not
collected / never run.

| Surface | Could a zero here mean "not collected"? | State after r15 |
|---|---|---|
| `forge onboard` | Yes — a Forgejo remote returned four zeros and a null detail | Fixed at WI-9: `supported: false` and a detail naming the host |
| `coverage` | Yes | Already honest: `never_run`, per-collector outcome, `never_swept` count |
| `deps` | **Yes, three ways** — nothing to find, a manifest format `dep-refs` does not parse, or a project no sweep has walked | Fixed: `dep-refs` writes a scan record per project; `deps` carries `status`, `manifests_read`, the unsupported and unreadable manifests, and a `detail` saying which zero this is |
| `brief.dependencies` | Yes — `status: collected` was claimed from an estate-wide check, so a project registered after the last sweep reported `collected, 0` | Fixed: claimed per project, from that project's scan record |
| `contract check` / `contract evaluate` | Yes — a root path that could not be read returned `non_compliant`, a verdict from a read that never happened, and `contract check` recorded it on the project | Fixed: `not_checked`, with the criterion detail saying so |
| `compliance` | No, but its `not_checked` had one meaning for two causes | Fixed: the detail distinguishes "nobody ran it" from "the last run could not read the path" |
| `drift detect` | Partly — it refuses outright with no coverage at all, and said nothing about unswept projects on a partly swept instance | Fixed: `not_collected` names the projects nothing could have been raised for |
| `drift list` | No | Already honest: carries `freshness`, and an empty inbox is only reassuring beside it |
| `observations` | Yes — an instance with no evidence tables returned `[]` with no detail | Fixed: a detail saying no sweep has run |
| `notifications` | No | Already honest: detail plus `freshness` |
| `backlog`, `bugs`, `why`, `project brief` (CI half) | No | Already honest: `freshness`, `CiSummary.status`, `NotCollected` |
| `sweep` | No | Per-collector outcome and named per-project failures |
| `status`, `project list`, `work list`, `audit list`, `events`, `export`, `import`, `session list` | No | Declared-store counts with no collector between the question and the answer; there is no third state to confuse |

Two residuals the sweep found and r15 does **not** fix, named here so they are
not rediscovered as defects:

- **An observation's `observed_at` is when its payload was first seen, not
  when it was last confirmed.** Dedup writes nothing for an unchanged subject
  (§3.1 of `SCHEMA.md`, NFR-S2), which is the property that keeps growth
  proportional to change — and it means no subject can be dated to the sweep
  that last saw it. Two consequences are live: a closed GitHub issue's newest
  observation says `open` indefinitely (the reason FR-R7 is human-gated and
  `forge_state_mismatch` lost half its auto-accept), and `latest_dep_refs`
  keeps a reference whose manifest entry has been deleted until something
  re-observes that subject. Closing it means a per-subject last-seen record —
  a schema change to the evidence store and a new coverage concept — which is
  a requirement someone should write deliberately rather than something to
  smuggle into a bugfix.
- **`ObservationsResult.total` is the size of the page, not of the store.**
  Every other `total` in the result models is a real count. This one is
  `len(rows)` because the query is paged with no count behind it, so `total ==
  limit` means "there may be more" and never "that is all". Documented on the
  field at r15; making it a true count is a storage change with its own cost.

---

---

### Revision r18 — an interaction nobody can replay is an interaction nobody can audit

*2026-08-18. Two durable logs are named: every assistant/voice interaction in
both directions, and every TTY session's full byte stream at a 60-day default
horizon. Neither is delivered. Both are audit requirements, and both close a
gap where this product currently keeps less evidence about itself than it
demands of the estate it governs.*

This register has spent three revisions on the cost of two records disagreeing
with nothing watching — FR-R6's proposals that outlived their cause, FR-R7's
work item done while its issue stayed open, r17's whole sweep for zeros that
could not say which zero they were. r18 names a plainer version of the same
problem: **an interaction that leaves no record at all.** There is nothing to
reconcile, because there is nothing.

Two gaps, both confirmed against `dev` at `8b19ecb` rather than assumed:

1. **The assistant's transcript is in-memory and singular.**
   `engine/server/src/assistant.rs` holds one `Conversation`
   (`assistant.rs:190`) behind a mutex, with no persistence primitive anywhere
   in the file. It does not survive a restart, it is not per-actor, and
   `history()` hands back a clone of a `Vec` that the next process will not
   have. FR-T3 audits the *write* an assistant caused; nothing records the
   conversation that caused it, and a conversation that caused no write —
   which includes every refusal, every expired pending action, and every
   question a person asked and acted on themselves — leaves no trace whatever.
2. **The session log is output-only, optional, and its retention horizon is
   whatever the last caller passed.** `pty.rs:390` appends the PTY's output
   stream to a per-session file; `write_input` (`pty.rs:142`) records nothing,
   so the log cannot answer what a person typed. `history` is
   `Option<Arc<SessionHistory>>` (`app.rs:52`) and construction failure
   degrades to no recording at all. `cleanup_old_sessions(retention_days)` is
   reachable from exactly one place — an on-demand endpoint that takes the
   number from the request body (`history_api.rs:187`) — so there is no
   configured default, no schedule, and nothing runs if nobody calls.

Three things this revision decides, so the build does not re-litigate them:

1. **Audio is still not stored.** FR-T12's rule — *"Audio is not stored unless
   a debug flag says so"* — is unchanged, and FR-T14 is deliberately a log of
   text and structure: recognised utterance, repaired utterance, composed
   request, reply, tool calls, tool results, pending actions and their
   outcomes. A voice interaction is fully reconstructable from that without
   retaining a recording of somebody's voice, which is a different class of
   data with a different cost and a different conversation attached to it.
2. **The TTY log records input, and that means it records secrets.** A log
   that cannot say what was typed cannot answer the question it exists for, so
   FR-E12 captures both directions. The consequence is named in the
   requirement rather than discovered later: FR-S7 and FR-S8 keep credentials
   out of argv, out of URLs and out of a clone's own configuration, and this
   log reintroduces them somewhere new — a pasted token, a password at a
   prompt that does not echo. The requirement therefore makes the log a
   scope-gated read like the audit log and makes its horizon a **maximum**
   rather than a floor. Redaction at non-echoing prompts was considered and
   deliberately not required: the echo bit is a heuristic, and a log that is
   *sometimes* redacted invites being trusted as though it always were.
3. **Raw bytes, not a scraped transcript.** The stated reason for FR-E12 is
   capturing what happened inside full-screen agent CLIs — `codex`,
   `claude`, `opencode` — and those paint with alternate-screen switches,
   cursor addressing and partial redraws. Scraping them to text yields redraw
   noise rather than what was on screen. `scrollback.rs` already reached this
   conclusion for the ring buffer, in its own words: *"Replaying raw bytes into
   xterm.js is the cleanest path to a faithful redraw on reattach."* FR-E12
   makes the durable log the same shape, with timing, so it replays.

Two requirements are appended:

| New ID | What it names | Pri |
|---|---|---|
| FR-T14 | Every assistant interaction durably logged in both directions — utterance, request, reply, tool calls, tool results, and each pending action's outcome including denial and expiry — attributable to an actor and surviving restart. Transcripts and structure, never audio. | M |
| FR-E12 | Every TTY session's full byte stream, input and output, recorded durably with timing for faithful replay of full-screen TUIs; retained for a configurable default of **60 days**, enforced on a schedule; scope-gated to read, with the secret hazard of input capture named. | M |

Both are **M**. The priority is not inflation: this register's own FR-S1 makes
every declared write carry actor, reason and timestamp *in the same
transaction*, and FR-S5 audits authorization denials as well as grants. A
product that holds itself to that for a label change, while keeping nothing at
all about an hour of agent work in a terminal or a conversation that moved a
work item, is not applying its own standard to itself.

**Storage is the honest cost, and it is named here rather than discovered.**
NFR-S2 requires the *observed store* to grow proportionally to change rather
than polling frequency; neither of these logs is an observation store, and
neither can honour that rule — a TUI redrawing at speed writes bytes
proportional to time, not to change. The 60-day horizon is what bounds it, and
that is why FR-E12 makes the horizon configured and scheduled rather than
whatever the last caller of an endpoint happened to pass.

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
*r14: the contract is opt-in per project, and every reported gap comes with
the means to close it. A project that has adopted no contract is not
non-compliant; it is untracked by the contract, which is a valid way to use
the product.*

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
| FR-G16 | *(r14)* Contract adoption shall be **per project and opt-in**. A project with no adopted contract shall report compliance as `not_applicable` and shall never be described as non-compliant; no view, export or brief shall imply a fault in a project that declined the contract. `FR-G14`'s statuses apply only to projects that adopted one. | M | DESIGN §2.1 |
| FR-G17 | *(r14)* The scaffold of FR-G11 shall be invocable against an **already-registered** project, not only at creation. It shall never overwrite an existing file, and shall report created and skipped paths exactly as `project create` does. Reporting a gap the product can close, without offering to close it, is the enforcing posture in a different costume. | S | DESIGN §5 |
| FR-G18 | *(r14)* A contract evaluation shall be able to emit a **recommendation** alongside its result: for each failing criterion, the mechanical remedy where one exists (a file or directory the scaffold would write), and where the remedy needs judgement — which licence, what belongs in `design/` — an instruction addressed to an actor, suitable for a human to read or an agent to execute. The recommendation shall be advisory output, never applied implicitly, and shall carry no authority beyond FR-G13. | S | DESIGN §2.1, §5 |
| FR-G19 | *(r14)* Contract criteria shall distinguish a criterion the project **has not met** from one it **cannot meet by construction** (a Cargo workspace has no root `src/`). Both may be reported; they shall not share a single word. Marking a criterion inapplicable for a project shall be an audited declaration carrying a reason, not a silent exemption. | S | DESIGN §5 |

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
| FR-V6 | *(r14)* Ranked views shall exclude observed subjects whose lifecycle is `closed` (FR-O12). A view named for open work shall contain only work that is open; a closed subject remains observable, queryable and countable, and is reachable through an explicit filter. Trust state describes how well a subject is known, not whether it is still outstanding, and shall not be read as the latter. | M | DESIGN §3.6 |
| FR-V7 | *(r14)* Classification of an observed subject that carries no classifying signal — an unlabelled forge issue — shall be reported as unclassified rather than silently defaulted into a kind. A subject that falls into no ranked view because nothing said what it was shall remain discoverable. | S | DESIGN §3.6 |
| FR-V5 | *(r11)* A ranked view shall be pageable past its first page: `backlog` and `bugs` shall accept an offset alongside their limit, so an estate with more open work than one page can hold is reachable rather than truncated. A view that truncates shall say so — which it already does; what it cannot currently do is continue. | S | §7 |

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

| FR-O10 | *(r14)* The principle of FR-O9 shall extend from observation payloads to **operation results**. Any result reporting a count, a total, or an empty collection shall carry the scope it was computed over and whether every source in that scope was readable. An operation that could not read a source shall say so in the result; it shall not report zero. `forge onboard` returning `issues: 0, detail: null` for a repository it has no adapter for is the same assertion FR-O9 forbids, made one layer up. | M | DESIGN §6 |
| FR-O11 | *(r14)* Every forge and collector adapter shall **declare what it can read** — hosts, ecosystems, subject kinds. A subject outside every declared capability shall yield `not_supported` naming the capability that is missing, never an empty success. A partially-implemented adapter shall therefore be safe to ship: the gaps report themselves. | M | DESIGN §1.1 |
| FR-O12 | *(r14)* An observed forge subject shall carry a **lifecycle** derived from its source — `open`, `closed`, or `unknown` — held separately from the ranked-view state of DESIGN §3.6, which deliberately asserts no workflow state. `unknown` is the required value where the source could not be read (FR-O11) and shall be surfaced, never silently treated as either of the other two. | M | DESIGN §3.6 |
| FR-O9 | *(r10)* An observation payload shall carry only values that were read. Where a source cannot be read at all — a missing or unrunnable tool, a failed command, a timeout — the collector shall report that it could not read it and the sweep shall degrade to `partial` naming the project (FR-O3, FR-O4); it shall not emit a field asserting a state it did not observe. A default that happens to be falsy is an assertion: `dirty: false` from a checkout that was never opened is indistinguishable from a clean one. | M | DESIGN §6 |

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
| FR-D8 | *(r2, delivered r15)* Where the same source exists both as a path member of one project and as a separate registered project, the system shall report the `mirrored_source` relationship as an observation. It shall not compare contents and shall not assert divergence. | C | DESIGN §3.5 |
| FR-D9 | *(r11)* An operation shall record a dependency edge no manifest expresses — a service that calls another, a pipeline that consumes a schema — with reference kind `declared`. `RefKind` has carried the member since M2 and nothing produces it, so an edge that lives only in a deploy script is invisible to `deps` and to the reverse lookup. | C | DESIGN §3.5 |

### FR-R — Drift & reconciliation

| ID | Requirement | Pri | Source |
|---|---|---|---|
| FR-R1 | The drift engine shall compare declared state against observations and raise typed drift proposals with linked evidence. | M | SCHEMA §2.4 |
| FR-R2 | Drift shall never silently mutate declared data; proposals require explicit accept / reject / contest by an authorized actor. | M | DESIGN §3.2 |
| FR-R3 | Auto-accept rules shall be configurable per project and per drift kind; the shipped default is low-risk auto-accept (state-sync kinds agent-acceptable; destructive/structural kinds always human-gated). | M | DESIGN §3.2 |
| FR-R4 | Every declared entity shall carry a computed trust state: `verified / stale / unverified / disputed` — derived from observation freshness and agreement, never hand-set. `disputed` is distinct from the drift *resolution* status `contested`, which is chosen by an actor. | M | SCHEMA §4 |
| FR-R5 | *(r2)* A drift proposal shall embed a self-contained evidence snapshot at raise time, and observations referenced by a proposal shall be exempt from retention pruning while that reference exists. Evidence shall never become unreachable through retention. | M | SCHEMA §2.4, §5 |
| FR-R6 | *(r15)* Where a completed sweep newer than an open proposal no longer reproduces the condition that raised it, the proposal shall be marked as superseded by fresher evidence, and the mark shall clear if the condition reproduces again. Marking shall not resolve the proposal, shall not alter its evidence snapshot, and shall be coverage-gated: absence outside a completed sweep is "not collected" (FR-O4). | S | SCHEMA §2.4 |
| FR-R7 | *(r15)* Where a work item's own text names a forge issue by a qualified reference — `owner/name#number` or an issue URL — and the item's state disagrees with the issue's most recent observed state, the system shall raise it as a drift kind of its own. It shall be human-gated, shall propose no change, and shall not duplicate a proposal already raised for an adopted link on the same subject. | S | SCHEMA §2.4 |

### FR-A — API surfaces & parity

| ID | Requirement | Pri | Source |
|---|---|---|---|
| FR-A1 | Every capability shall be available via REST, CLI, and MCP; the GUI shall consume the same REST surface. Nothing shall be GUI-only. | M | DESIGN §2 |
| FR-A2 | All surfaces shall be generated from one transport-neutral operation registry (name, scope, mutating flag, argument schema, route) — including the MCP stdio bridge. | M | DESIGN §4.1 |
| FR-A3 | Transport parity shall be enforced by tests whose exclusions are explicit named lists (`HTTP_ONLY`, `LOCAL_ONLY`) that fail when stale in either direction. | M | DESIGN §4.1 |
| FR-A4 | The REST API shall publish a generated OpenAPI document. | M | DESIGN §4 |
| FR-A5 | MCP shall be served over stdio (local, no server required) and streamable HTTP at `/mcp`; a stdio bridge shall serve clients that can only spawn local processes, discovering the remote tool list at startup rather than hardcoding it. | M | DESIGN §4.1 |
| FR-A6 | *(revised r8, r10)* An MCP `initialize` naming a protocol version the server does not recognise shall be answered with the newest version the server **does** support, leaving the client to continue or disconnect — the handshake is a negotiation, not a gate. A recognised version shall be answered with itself. **More generally, no discovery or pre-flight step shall prevent a client from using a service that works**: bridge↔server version skew, an unreachable or erroring discovery endpoint, and a discovery response that cannot be parsed or is not of the expected shape shall each warn on stderr and never block startup. | M | DESIGN §4.1 |
| FR-A7 | *(revised r10)* Plain-HTTP `/health/live`, `/health/ready`, `/version`, and `/connection-info` shall be served on every port that serves MCP, by **whichever process publishes that port** — a fronting process inherits them for the address it publishes and shall not leave them to the process it fronts. A path named here shall be served or refused with a named reason; it shall never be answered by a fallback handler, because a `200` carrying something else cannot be told from success. | M | DEPLOY §1, MERGE §5.2 |
| FR-A8 | *(r7, revised r10)* **Connecting a client shall be a capability of the product, not an exercise for the reader.** The instance shall state the URL clients reach it at — configured, never inferred, and reported as absent when unset rather than guessed — and shall render client configuration from it (`connect`). Where a process is fronted, **the URL stated and the configuration rendered shall be the fronting process's own**: the address a client must use is a property of the door it knocks on, and a fronted process can no more infer the door's published address than it could infer its own (NFR-D2). A client that speaks streamable HTTP shall require no Vogt code installed; installation shall be a property of the stdio bridge alone, and shall be stated as its cost rather than assumed. | M | DEPLOY §4.3, MERGE §5.3 |
| FR-A9 | *(r10)* Where one process fronts another's surface, it shall satisfy that surface's client-facing obligations at the address it publishes — the probes of FR-A7, the stated URL and rendered client configuration of FR-A8 — or refuse the path with a named reason identifying which half of the product refused. Silently not routing a fronted obligation shall be treated as not implementing it. | M | MERGE §5.3 |

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
| FR-S11 | *(r11)* Every scope the system issues shall gate at least one operation. `writeback` gates none: `forge.writeback` sets a project's policy under `project.write`, and the upstream write is a consequence of commenting or transitioning under `work.write` — so a token issued with `writeback` alone can only read, which is a trap for whoever issues one in good faith. Either the scope gates the write-back it names, or it is withdrawn and the issuer told why. | S | DESIGN §4.1 |
| FR-S12 | ~~*(r11)* An audit query filtered to one work item shall return every write about that item, comments included.~~ **Withdrawn (r11), on the day it was raised** — the capability already exists. `declared.py`'s trail query is a semi-join over the comment ids belonging to the item, and `tests/test_audit_query.py` asserts both that a comment appears in its item's trail and that it stays out of another item's. It was raised from a stale `ROADMAP.md` note rather than from the source; the ID is kept struck through per §4 because it was published. | — | — |

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
| FR-N4 | *(r15)* One registry-backed `inbox.list` operation shall provide a normalized attention read over GitHub notifications, drift proposals, failing current-revision CI checks, and agent attention. The server shall own source coverage, deterministic global ordering, stable material-versioned entry keys, high-water marks and opaque keyset pagination; the client shall not merge `notifications`, `drift.list`, `events.list` or engine reads. The projection shall not merge the `/events` history or mutate GitHub read state. | M | RESTRUCTURE Stage 0, Stage 1 |
| FR-N5 | *(r15)* Shared Inbox archive, snooze and restore shall be audited writes with typed reasons, keyed to the material occurrence. An unchanged re-observation shall remain triaged, while a materially changed source version may resurface; local seen state shall be per-client presentation state outside the shared contract, and reading an elapsed snooze shall not write. | M | RESTRUCTURE Stage 0, Stage 1 |

### FR-U — GUI

| ID | Requirement | Pri | Source |
|---|---|---|---|
| FR-U1 | The GUI shall provide: per-project view, global backlog, global bugs, drift inbox, dependency graph view, audit browser. | M | DESIGN §10 (M6) |
| FR-U2 | The GUI shall display trust state and data freshness on every aggregated view. | M | DESIGN §6 |
| FR-U3 | *(r6)* The GUI shall provide an import form taking a repository reference and a reason (FR-P6), and a notification inbox over FR-N3. Both shall consume only the public REST API, and the import form shall offer no repository listing, search, or suggestion. | S | DESIGN §10 |
| FR-U4 | *(r9)* The GUI shall present a board of work items whose columns are the workflow's states read from `workflow.list` — never hard-coded — where a drag is a `work.transition` and an illegal transition bounces with the server's stated reason. | M | MERGE §7.1 |
| FR-U5 | *(r9)* The GUI shall present a work item in full: description, state history, comments, relations, labels, per-item audit trail, collected evidence with freshness and trust, and the start-session control (FR-E4). | M | MERGE §7.2 |
| FR-U6 | *(r9)* The GUI shall present the ranked backlog and bugs views with the explainable `why`, quick-create, and bulk transition/label — under r6's rule: a mutating operation appears only through a view that collects a reason the user typed. | M | MERGE §7.3 |
| FR-U7 | *(r9, revised r15)* The GUI shall present per-project pages: brief, CI status, contract/compliance, drift summary with count, age and context, dependency graph, and the import form. Project drift actions shall link to the canonical filtered Inbox (`/inbox?source=drift&project=<slug>`); evidence, triage and resolution shall not be duplicated on the project page. | S | MERGE §7.4; RESTRUCTURE Stage 0, Stage 4 |
| FR-U8 | *(r9)* The PWA shall consume only public APIs, and every URL in the shipped bundle shall resolve against the operation registry *and* the engine's API contract — extending the existing M6 assertion to the Solid bundle and to both halves. | M | MERGE §7 |
| FR-U9 | *(r9)* The legacy GUI shall keep serving at `/ui-legacy` until every operation it exposed is reachable in the PWA, and shall then be removed — parity is asserted, not assumed. | S | MERGE §5.1 |
| FR-U10 | *(r9)* Views showing server-announced state — session activity (FR-E2), work item state, drift arrivals, notification counts — shall update live from the SSE stream without a manual refresh. A lost stream shall be indicated and shall reconcile on reconnect; a stale view shall never present itself as current. | M | MERGE §7; engine README |
| FR-U11 | *(r9, revised r15)* Every project, work item, board (including its active filter set), session, and audit query shall be addressable by URL: deep links shall survive reload, be shareable, and restore the exact view. The places shell shall name `/inbox`, `/sessions` and `/settings`, preserve the existing deep links in `RESTRUCTURE.md` Stage 3's route matrix, and keep terminal deep links (`/#/t/<id>`) valid. | M | MERGE §7; engine README; RESTRUCTURE Stage 0, Stage 3 |
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
| FR-U23 | *(r15)* The GUI shall use stable, non-closable places instead of product-level tabs without losing a route or capability. Terminal/editor tabs shall migrate to Sessions panes, machine tools shall remain reachable inside Sessions, work-item and surface tabs shall become addressable recent places, and migration of `mydevenv2.tabs.v1` shall be idempotent and preserve the old value until the new state is written successfully. | M | RESTRUCTURE Stage 0, Stage 3 |
| FR-U24 | *(r15)* The Inbox surface shall consume only `inbox.list`, render source coverage, provenance, trust and freshness, show drift evidence before its actions, and collect a typed reason for every shared write and batch. Local seen/selection state shall not call the server, and keyboard, pointer and batch paths shall use the same reason boundary. | M | RESTRUCTURE Stage 0, Stage 4 |
| FR-U25 | *(r15)* Board cards and Backlog rows shall size to their measured content, wrap and expand in place, and use one keyed measured window with pixel overscan, scroll anchoring and invalidation for width/font/expansion changes. The estimate shall not become a fixed CSS height; NFR-S5's virtualization, bounded reads and estate-scale interaction guarantees remain in force. | S | RESTRUCTURE Stage 0, Stage 5–7; NFR-S5 |

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
| FR-E8 | *(r9, revised r16)* `session.start`, `session.list`, and `session.stop` shall be operations in the registry, and therefore present with parity on CLI, REST, and MCP (FR-A1). *(r16)* `session.start`'s `model` and `effort` parameters (FR-T11) are part of that parity: every surface may ask, not only the assistant. | M | MERGE §6.2 |
| FR-E9 | *(r9)* The engine shall remain bootable with vogt-core absent, degrading to plain sessions — absence of the core costs Vogt features, never session availability (the ContextKeeper degrade rule, applied inward). | S | MERGE §11.2 |
| FR-E10 | *(r11)* Where a deployment enables GUI streaming, it shall work: a launched process shall render and its stream shall be viewable from the GUI surface. The compositor and the streamer are installed in the image and the launch and process APIs are built; production runs with the stream switched off and unverified, so normal GUI affordances are withdrawn and direct links say why. The server may advertise the surface only after an operator records an end-to-end render verification. | C | DEPLOYMENT §10.6 |
| FR-E11 | *(r11)* Two live sessions shall not silently open in the same working tree. Nothing today coordinates between sessions, so two agents can edit one checkout concurrently and neither is told — a class of loss no audit row records, because both writes are legitimate. The requirement is that the second session is *told*, not that it is refused: Vogt reports, it does not enforce (FR-G13). | C | §7 |
| FR-E12 | *(r18)* Every TTY session shall be recorded to a durable log capturing the **full byte stream in both directions** — input as well as output — as raw bytes with timing, so that a full-screen TUI (`codex`, `claude`, `opencode`) replays faithfully including alternate-screen switches and partial redraws; a scraped text transcript does not satisfy this. It is distinct from FR-E1's ring-buffer scrollback, which is bounded and ephemeral by design. Recording shall not degrade silently: a deployment that asked for it and is not getting it shall be told, because an absent log is indistinguishable from a session that did nothing. Logs shall be retained for a **configurable horizon defaulting to 60 days**, enforced on a schedule — not only by an on-demand call carrying its own number, which is the current state and means the horizon is whatever the last caller passed. Because input capture necessarily records typed secrets — a pasted token, a password at a prompt that does not echo — reading a log shall be scope-gated as the audit log is (FR-S3, FR-S6), the log shall never be served unauthenticated, and the retention horizon is a **maximum, not a floor**. This does not weaken FR-S7 or FR-S8; it names where those rules stop holding. | M | r18; FR-E1, FR-S3, FR-S6, FR-S7 |

### FR-T — Conversational assistant (the AI layer) *(r9)*

| ID | Requirement | Pri | Source |
|---|---|---|---|
| FR-T1 | *(r9)* The assistant shall be a server-side tool-use loop with read access to sessions (`list_sessions`, `read_session_tail`) and to a curated read-only slice of the operation registry (at minimum: `backlog`, `bugs`, `why`, `project.brief`, `project.list`, `work.get`, `work.list`, `compliance`); registry-backed tool schemas shall be generated from the registry, not hand-written. | M | MERGE §8.1 |
| FR-T2 | *(r9)* Every mutating assistant tool — `send_input`, any `work.*` write, `session.start` — shall pass the pending-action gate: one pending action at a time, carrying the exact payload and target, expiring unapproved, approved only by an on-screen act. No model output shall be able to bypass the gate, and the voice path shall never approve. *(Promoted from the engine's `ASSISTANT.md` threat model.)* | M | MERGE §8.2 |
| FR-T3 | *(r9)* An assistant-initiated Vogt write shall be audited to the approving user's actor with a `why` derived from the conversational context — never to a shared "assistant" actor. | M | MERGE §8.2, FR-W1 |
| FR-T4 | *(r9)* Assistant tool results carrying external content — terminal output, forge-derived text, imported issue bodies — shall be delimited as untrusted data; the threat-model rule that external content never becomes instructions extends to every Vogt read. | M | MERGE §8.5 |
| FR-T5 | *(r9, revised r16)* The assistant shall be drivable by voice: push-to-talk STT in the mobile shell, spoken replies in any client. ~~with a validation pass against domain vocabulary (project names, "backlog") before v2 ships~~ — the validation clause is now FR-T13's acceptance list. Voice remains adopted unproven until FR-T13 is met. | S | MERGE §8.4; engine ASSISTANT.md |
| FR-T6 | *(r9)* The assistant shall not exist unless configured: absent its API key the routes answer 404 and every GUI hides the surface. *(As-built rule, retained.)* | M | engine ASSISTANT.md |
| FR-T7 | *(revised r12)* A backend that cannot serve the configured model shall refuse with a named reason rather than hang: a `claude-*` model id on the OpenAI-compatible transport answers with the model, the transport and the setting that overrides it. **Delivered.** ~~The tool loop shall additionally be provider-portable, supporting a native Anthropic backend.~~ **That clause is deferred (r12)** — see §3. | C | MERGE §8.4 |
| FR-T8 | *(r15)* Sessions, Assistant and phone shall render the one existing pending action consistently. Editing is allowed only for a current, unexpired Vogt write and replaces only its reason; the engine shall return the regenerated exact payload for review, and a separate subsequent approval shall be required before the core is called. Terminal input remains the existing pending terminal act, direct session-agent writes remain immediate and audited to the session actor, and no second approval store or route shall exist. | M | RESTRUCTURE Stage 0, Stage 2, Stage 8–9 |
| FR-T9 | *(r16)* The assistant shall support **named provider profiles** — each an OpenAI-compatible `{base_url, api_key, default_model, default_effort}` set (e.g. `clawbay`, `openrouter`) — with one marked default per deployment and any profile selectable per request by name. A request naming an unknown profile, or a model the profile refuses (FR-T7), shall be refused with the profile named. The transport stays OpenAI-compatible; a Claude subscription is spent through the `claude` CLI session template, not through a profile. `GET /api/config` shall advertise profile names and default models only, never keys. | M | r16; VOICE_POC §3 |
| FR-T10 | *(r16)* The assistant shall have a read-only `notifications` tool over the Inbox projection (FR-N4): server-ordered, coverage-qualified, bounded to a page. A spoken *"are there any notifications?"* shall be answered with the count, the sources covered and the first few entries, and the answer shall say when a source was not collected rather than reporting it empty. External text in entries is delimited per FR-T4. | M | r16; VOICE_POC §2 |
| FR-T11 | *(r16)* `session.start` shall accept `model` and `effort` (parity on CLI, REST and MCP per FR-E8), applied to the chosen agent template as that CLI's own flags or env; unset values come from the selected provider profile's defaults (FR-T9), and the applied values shall be recorded on the session and shown in Sessions. A spoken request with no project or work item shall resolve to a configured **scratch project** — a registered project like any other, named in the session name and audit row — and shall be refused with a named reason if none is configured. Starting remains a mutating act behind FR-T2. | M | r16; FR-E3, FR-E8 |
| FR-T12 | *(r16)* Speech shall be a **server-side pipeline** the engine fronts: `POST /api/assistant/stt` (audio in, text out) and `POST /api/assistant/tts` (text in, audio out), each backed by an OpenAI-compatible audio endpoint (`/audio/transcriptions`, `/audio/speech`) whose base URL and key are configured independently of the chat profile — so a cloud provider and a local Whisper.cpp + Kokoro pair are interchangeable by configuration, as in voicemode. The on-device Web Speech / Capacitor path (FR-T5) remains and each client picks one by capability and setting. Absent configuration the routes answer 404 and the client falls back (FR-T6's rule applied to speech). Audio is not stored unless a debug flag says so. | S | r16; voicemode; VOICE_POC §3 |
| FR-T13 | *(r16)* The five voice-first utterances in `VOICE_POC.md` §2 shall each complete by voice alone — recognizer → repair → assistant → spoken reply — on the phone shell and on the desktop, up to and only up to the FR-T2 gate, which is announced and resolved on screen. Between recognizer and composer there shall be a **repair pass** that normalises project slugs against `project.list` and repairs `WI-\d+` forms, showing the repaired text before it is sent. This is the validation pass FR-T5 promised, made into an acceptance list; until it passes, voice is presumed not working. | S | r16; FR-T5; VOICE_POC §5 |
| FR-T14 | *(r18)* Every assistant interaction shall be recorded to a durable log in **both directions**, surviving restart and attributable to the actor who drove it: the recognised utterance and, where FR-T13's repair pass changed it, the repaired form alongside the raw one; the composed request; the model's reply; every tool call with its arguments; every tool result; and every FR-T2 pending action with its outcome — **approved, denied, or expired**. The log shall complement FR-T3 rather than replace it: FR-T3 audits the write an assistant caused, and this records the conversation that did or did not cause one, a distinction that today leaves every refusal and every un-acted question unrecorded. **Audio shall not be stored** — FR-T12's rule stands and this log is text and structure. External content in a logged entry stays delimited as untrusted data per FR-T4, including when the log is later read back to a model. Reading the log shall be scope-gated (FR-S3) and its retention horizon configurable. | M | r18; FR-T3, FR-T4, FR-T12, FR-S6 |

### FR-M — Mobile surface *(r9)*

| ID | Requirement | Pri | Source |
|---|---|---|---|
| FR-M1 | *(r9)* The mobile app shall be the Capacitor shell loading the merged PWA. Its MVP1 feature set shall be: terminal sessions, assistant with voice, push, backlog/board read, and session start/approve. | M | MERGE §3; ROADMAP M13 |
| FR-M2 | *(r9, revised r15)* Push notifications shall be routed for events worth a phone interruption: a session entering `waiting-for-input` or `errored`, new drift, the agent-task notify hook, and an assistant pending-action approval — and for nothing else by default. The approval notification shall expose no terminal bytes, Vogt payload or reason and shall deep-link to `/sessions?approval=<id>`; the link is only a hint to the authoritative current action. | S | MERGE §10; RESTRUCTURE Stage 0, Stage 2 |
| FR-M3 | *(r9)* Vogt surfaces shall be usable at phone widths; the board shall render as a list, not columns, below the narrow breakpoint. | S | MERGE §7 |
| FR-M4 | *(r11)* A dev build of the mobile shell shall install alongside a prod build on one device **and register for push**. The id and the front door are already build inputs (`MYDEVENV2_ANDROID_APP_ID`, `VOGT_ANDROID_SERVER_URL`); what is missing is an FCM client entry for the dev id, since `google-services.json` is keyed to the application id. Until it exists, validating a mobile change means uninstalling the working app — which is why FR-M2's push routing and FR-T5's voice pass are both unverified on hardware. The signing key may be shared. | S | §7 |
| FR-M5 | *(r15)* On narrow/coarse clients, the four primary places shall use a text-labelled bottom bar for Sessions, Inbox, Board and Backlog. Every secondary route shall remain reachable through a labelled “Go to…” control or a contextual link, including Projects, Audit, Settings, work items and all Sessions tools; push shall open the current pending action through its deep link. There shall be no approval-by-voice path. | S | RESTRUCTURE Stage 0, Stage 9 |
| FR-M6 | *(r16)* The mobile shell shall keep receiving updates while backgrounded. The wake is push (FR-M2), not polling; while a voice conversation is active the shell shall hold an Android foreground service (with the required persistent notification) so audio capture, TTS playback and the assistant socket survive screen-off, and shall release it when the conversation ends. There shall be **no always-listening microphone** and no wake word. A push that arrives during an active conversation shall be spoken as well as shown; a push that arrives outside one behaves as FR-M2. Battery cost of a held service shall be measured in the POC before it ships. | S | r16; VOICE_POC §4 |

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
| NFR-Q7 | *(r10)* A requirement about the shipped artefact shall be verified against the shipped artefact. Every tool the product invokes at runtime shall be proven present by **executing it in the built image**, not by reading the file that installs it; and the probes of FR-A7 together with the configuration rendered by FR-A8 shall be checked **at each published address**, not against the process behind it. Asserting on a Dockerfile or on a fronted process is evidence about a source file or an inner hop, and neither is the artefact a client meets. | M |

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

Deferred by revision r12:

- **A native Anthropic backend for the assistant** (half of FR-T7). The loop
  stays OpenAI-compatible. The clause was raised because `claude-*` routes
  through the configured proxy hung; **that failure is closed by refusing
  rather than hanging**, which is the half of FR-T7 that is delivered. A second
  transport would buy a choice of vendor rather than a capability — nothing in
  v2 depends on Anthropic specifically — and would cost a second set of
  tool-call semantics, streaming shape and failure modes to keep working. The
  fault that motivated it is a proxy's rather than this product's, and building
  a second client to route around somebody else's broken route is a fix that
  outlives its problem. Reversed by a use that wants Anthropic tool-calling
  specifically; the ID is unchanged and r12 is the argument to beat.
  *(r16 note: FR-T9 asks for **provider profiles**, not a second transport.
  OpenRouter and The Claw Bay are both OpenAI-compatible, and a Claude
  subscription is reached through the `claude` CLI, not an HTTP API — so r16
  leaves this deferral standing and routes around it rather than reversing
  it. See r16.)*

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
| FR-L1 | M | ~~The CLI provides everything but `migrate`.~~ **Delivered (r13).** `migrate` is an operation in the registry and a CLI verb, local-only like `init`, reporting both stores' applied and expected versions. It refuses a data directory with no instance rather than creating one. `tests/test_schema_gate.py`. | — |
| NFR-I3 | M | ~~Readiness does not gate on migration, and `serve` does not migrate.~~ **Delivered (r13).** `build_server` migrates both stores before assembling the app, and `/health/ready` compares each store's applied version against `bundled_schema_version()` — the highest migration the build ships — answering `503` naming the store, both numbers and `vogt migrate`. A store *ahead* of the build stays green, because `migrate` refuses that case with more diagnosis than a probe can carry. `tests/test_schema_gate.py`, mutation-checked on both halves. | — |
| FR-S6 | M | ~~Not queryable by time.~~ **Delivered.** `ListAuditParams` now carries `since`, `until`, `project` and `offset`, and `AuditListResult` carries `total`. The interval is half-open — `since` inclusive, `until` exclusive — so consecutive windows tile the log exactly and a write made at midnight is counted once rather than in both weeks that touch it. A naive bound is read as UTC rather than the server's zone, since the stored `at` is always UTC and reinterpreting a bound locally would move a query's boundary by an hour on one host and not on another. `tests/test_audit_query.py` (twenty) and an `audit.list` step in the parity script, so the new parameters answer the same on CLI, REST and MCP. | — |
| FR-G1 | M | ~~Not sourced from configuration.~~ **Delivered (r13).** Four settings carry the rules and the version, read through `configured_contract()`. A contract whose rules differ from the built-in default while keeping the default version gets a digest appended, so a status cannot claim to be the stock `v1` and not be. `tests/test_contract_config.py` (thirteen). | — |
| FR-D2 | M | Edges record `path` and `git` ✅ with the manifest they came from ✅. The third reference kind, `declared`, **cannot be produced** — no operation emits one. Carried as **FR-D9** in §7.1, priority C. | Medium |
| FR-S3 | M | ~~`writeback` gates no operation.~~ **Delivered (r13)** as FR-S11. `forge.writeback` now requires `writeback`, and `tests/test_auth.py` asserts both that the scope grants it and that `project.write` alone does not — plus that every issuable scope gates at least one operation, which is the check whose absence let this sit from M5. | — |
| NFR-S4 | S | ~~The fixture asserts the target an order of magnitude below the envelope.~~ **Delivered (r13).** It now seeds 500 projects and 100,000 work items by default. The recorded reason for scaling down — that seeding 100k "would cost minutes" — had never been measured; it takes about two seconds, because the rows go in through one transaction on the direct write path. `VOGT_BENCHMARK_SCALE=tripwire` opts back down for a fast local loop. | — |

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

**201 conjuncts across 46 IDs. 143 are delivered, 41 are implemented and
asserted by nothing, 7 cannot be verified in this environment at all, and 10
are short or absent.** Each conjunct is in exactly one of those four, by this
precedence: short before unverifiable, unverifiable before untested, untested
before delivered. So a conjunct counted "unverifiable here" is one whose
implementation was read and believed; a conjunct counted "short" was not
believed, and §6.2 says why.

**A fourth pass moved forty-one conjuncts, and three of them were moved by
nothing being written.** The pipeline ran. Every claim this section made about
the merged image, the two image streams and the Android shell was a claim
about a workflow file that had never once executed; three turned out to be
true, and the two that were false failed on their first run, in the two
places a file cannot show you. `docs` failed on a link that resolved on this machine and nowhere else.
The engine job failed on `sudo apt-get` with *root is not in the sudoers
file* — the runner is root, so there was nothing to elevate from. Neither was
a code defect and both were invisible to review, which is §6.3 finding 22.

**Delivered means a file and a test.** Not a citation — §5.4a's whole point is
that a grep for an ID finds the docstring that names it. Finding 24 is this
rule failing in this section: a row named a file and three tests, none of
which exist, and it survived four passes because naming them *looked* like
having them. `tests/test_requirements_audit.py` now checks every path and
every test name §6.1 cites. Where code plainly
does the thing and nothing asserts it, the answer is *implemented, untested*,
which §5 distinguishes and this section does too.

**The conjuncts that moved were re-counted individually; the rows that did not
move keep the previous pass's split.** §6.2 is one conjunct per row, as its
own opening line says, so its arithmetic is countable by eye; §6.1, §6.2a and
§6.2b group conjuncts by ID and their rows carry several each. The totals are
therefore checkable without re-deriving all 201:

- **143 delivered** = 102 at the third pass, then +41 in the fourth: fifteen
  out of §6.2, twenty-six out of §6.2a and two out of §6.2b, **less two
  returned to §6.2a by finding 24** — the first conjuncts this document has
  ever moved backwards, and the number in this section I trust most
- **41 untested** = 28 − 3 to §6.1 + 1 out of §6.2 + 39 out of §6.2b, − 26, + 2
  returned from §6.1 by finding 24
- **10 short** = 43 − 19 + 1 arriving from §6.2b, − 15
- **7 unverifiable** = 69 − 60 − 2

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
| **FR-E1** | Multiple concurrent clients per session — both attached at once, each seeing what the other typed | `integration.rs::two_clients_watch_one_session_at_once`. The multi-attach test beside it closes the first socket before opening the second, which exercises re-attachment rather than concurrency: a second attach that silently displaced the first would pass it and lose somebody's terminal. Writing this found nothing wrong with the engine and one thing about the shape of the test — see the comment on its teardown |
| FR-E2 | The four activity states; derived from output heuristics | `engine/server/src/activity.rs` (`detects_yn_prompt`, `nonzero_exit_becomes_errored`, `quiet_window_collapses_to_idle`, `recent_output_is_running`) |
| FR-E3 | `cwd` is the path the project registry records, and a work item with no project is refused rather than guessed | `tests/test_sessions.py::test_a_session_opens_in_the_path_the_registry_records`, `::test_a_work_item_with_no_project_has_no_tree_to_open_in`; engine-side `integration.rs::session_cwd_must_stay_under_workspace_root` |
| FR-E3 | The import root and the engine's workspace root are the same tree — co-located by the compose stack, and now *reported* by the front door rather than assumed | `api.rs::check_workspace_agreement` publishes a `workspace_agreement` readiness check; `vogt_core.rs` asserts the disagreeing branch (`ok: false`, a detail that says what it costs, `fatal: false`, and the container still ready) and the nothing-to-compare branch. Deliberately non-fatal: some projects being invisible is a bad answer, not a dead server (FR-E9). The comparison is by path component, not by string prefix — §6.3 finding 12, resolved, and `a_sibling_directory_is_not_inside_the_workspace` is the test that would have caught it |
| FR-E4 | The description reaches the brief; the brief is written through the agent-task prompt mechanism; the session id is recorded as an audited write; the item's views carry live activity | `tests/test_sessions.py::test_the_work_items_brief_travels_with_the_session`, `::test_the_session_is_linked_to_its_work_item`, `::test_the_work_item_view_shows_what_is_running_for_it`; `integration.rs::session_prompt_is_written_to_a_file_the_child_is_pointed_at` |
| **FR-E4** | The brief carries the item's relations, named by the related item's ref and title | `tests/test_sessions.py::test_the_brief_carries_the_items_relations`. An item that blocks another is not the same job as one that stands alone, and an agent handed only a title and a body cannot discover that — the relation lives in Vogt and nowhere in the item's own text. Rendering it as an id would satisfy the letter and tell the reader nothing, so the test asserts the ref and title, and dropping either turns it red |
| FR-E4 | The brief carries the item's `why` — the ranking's per-input contributions, not a single score | `tests/test_sessions.py::test_the_brief_says_why_the_item_is_ranked_where_it_is` (which asserts the *contributions*, so a total on its own would fail it), `::test_a_brief_survives_a_ranking_that_cannot_be_computed` (the session starts either way, so a score never becomes a precondition for starting work) |
| **FR-E6** | **All four**: exit code, duration, the working-tree delta, and all three collected as observations with the freshness and coverage every other kind carries | `src/vogt/collectors/session_outcomes.py`; `tests/test_session_outcomes.py` (twenty-one, including `test_a_finished_session_reports_its_exit_code_and_duration`, `test_the_delta_counts_what_the_tree_recorded_in_the_window`, `test_an_outcome_is_collected_and_never_declared`, `test_the_outcome_carries_the_sweeps_coverage`, `test_an_unchanged_outcome_does_not_grow_the_store`). The three shapes the previous pass called unbuilt are each asserted twice: once for the value, once for its absence — a session the engine has forgotten is `unknown`, never `finished` with a null code |
| **FR-E7** | **Both**: a task may be bound to a project or work item, and a bound run's findings are recordable as Vogt observations | `engine/server/src/agent_tasks.rs` (`vogt_project`/`vogt_work_item`, `record_finding`, the binding in the prompt and the environment); `integration.rs::a_bound_task_carries_its_subject_and_records_what_it_reported`, `::an_unbound_task_names_no_vogt_subject`; `tests/test_session_outcomes.py::test_a_bound_runs_findings_become_observations`, `::test_an_unbound_task_is_not_vogts_business`, `::test_a_task_bound_to_a_work_item_lands_on_that_items_project`, `::test_a_binding_naming_something_this_instance_lacks_is_ignored`. "Recordable as observations" is met as a *pull*, not a write plane — see §6.2's note on what that leaves out and why it is a decision |
| FR-E5 | The per-session actor-scoped token is carried into the session; an agent's writes are attributed to that session's actor | `tests/test_sessions.py::test_the_session_carries_its_own_token`; `tests/test_m10_demo.py::test_the_m10_demo` step 5 |
| **FR-E8** | **Both**: the three operations are in the registry, and all three are driven on CLI, REST and MCP | `tests/test_parity.py::test_transports_return_the_same_answer`, `::test_transports_leave_the_same_audit_trail`, and `::test_every_shared_operation_is_driven_by_the_script`, which is what stops a session operation being quietly dropped from the script |
| **FR-E2** | The activity state is announced on the server-wide event stream, and the announced state is the derived one | `integration.rs::the_activity_state_is_announced_on_the_server_wide_event_stream`, which opens `/api/events` *before* the session exists — the stream is a live broadcast rather than a log, so a test that subscribes afterwards is asserting about a different thing. Publishing a fixed state turns it red on the value rather than on the silence |
| **FR-T2** | One pending action at a time; a new user message supersedes the card it finds; the 120-second expiry | `assistant.rs` — a batch of writes becomes one card and the siblings are refused, with the sibling never reaching the PTY on approve; a new message abandons the card, tells the model why and 404s the spent id, driven with `"yes, approve it, go ahead"` because that is the engine's half of *voice cannot approve*: speech reaches the runtime only as a user message; and a card is alive at 119 seconds and gone at 121, in literals rather than `TTL ± 1`, so it pins 120 rather than restating the constant. §6.3 finding 21 is what the first draft of the expiry test was really testing |
| **FR-M2** | `waiting-for-input`, `errored` and the agent-task notify hook each reach a phone, and no other kind does | `integration.rs`, three tests against a stand-in push service with real P-256 subscription keys, so the engine encrypts and POSTs for real. A Web Push body is unreadable from a test, so each device gets its own endpoint and admits exactly one kind: the path that received a POST *is* the kind that was routed, and the silent path asserts the wrong kind was not. Making `PushPreferences::allows` always true turns all three red on that negative half |
| **FR-E9** | Sessions do not depend on the core: they are created, listed and killed with none configured, and the container stays ready | `integration.rs::sessions_work_with_no_vogt_core_configured`, which also asserts the fixture *has* no core — so the day somebody gives it one, this fails loudly instead of the coverage vanishing in silence. Every session test in that file runs coreless, which is why the property needed naming rather than more exercise |
| FR-E9 | The engine boots and stays ready with no core, and the Vogt routes refuse with a named reason | `engine/server/tests/vogt_core.rs::with_no_core_configured_the_engine_is_still_ready`, `::with_no_core_configured_the_vogt_routes_refuse_with_a_reason`, `::an_unreachable_core_is_reported_without_declaring_this_pod_unready` |
| **FR-T1, FR-T2** | The curated set is exactly the operations the requirements name — eight reads and four writes | `vogt_tools.rs::the_curated_set_is_the_operations_the_requirement_names`, which restates the decision rather than measuring it. The assertion it replaces compared `CURATED_READS.len()` to itself and would have passed with `compliance` deleted; the point of the requirement is that the assistant's reach is something somebody wrote down |
| FR-T1 | Server-side tool loop; `list_sessions`; `read_session_tail`; the schemas are the core's, forwarded verbatim | `engine/server/src/assistant.rs` (`plain_reply_round_trip`, `list_then_tail_then_reply`, `tools_come_from_the_core_not_from_a_literal_in_this_file`); `vogt_tools.rs::conversion_forwards_the_served_schema_verbatim` |
| FR-T2 | Every Vogt write passes the gate unconditionally; the card carries the exact payload and target, and the approved payload is the one the core receives; approval is an authenticated on-screen act and nothing else | `assistant.rs::a_vogt_write_waits_for_approval_and_then_uses_the_approver_pairing`, `::a_write_without_a_reason_is_refused_before_it_becomes_a_card`; `auth.rs::maps_mutating_routes_to_capabilities` |
| FR-T2 | `send_input` passes the same gate, with no setting that turns it off | `assistant.rs::send_input_pauses_and_approve_delivers`, which no longer takes an `auto_type` argument because there is none: `assistant_auto_type` is removed rather than defaulted, and the dispatcher's `send_input` arm refuses instead of delivering, so an edit that loses the interception fails closed |
| FR-T3 | The credential is the *approver's*, taken from the request that pressed approve; a write has no shared fallback and an unpaired approver is refused by name | `assistant.rs::a_vogt_write_waits_for_approval_and_then_uses_the_approver_pairing` (proposed by an unpaired token, approved by a paired one, and the core saw the approver's), `::an_unpaired_approver_gets_a_refusal_rather_than_a_shared_actor`; `vogt_tools.rs::a_write_has_no_fallback_credential` |
| FR-T4 | Both delimiters exist; terminal output is delimited; every Vogt read and every approved write's answer is delimited | `assistant.rs::list_then_tail_then_reply`, `::a_vogt_read_arrives_delimited_as_untrusted_data` (instruction-shaped text from the core arrives inside the tags) |
| FR-T4 | Forge-derived text and imported issue bodies arrive as data — structurally, because every string from the core reaches the model through a core answer and every core answer is wrapped where it arrives | `assistant.rs::every_place_the_core_answers_this_loop_delimits_what_it_said`, which reads the source and asserts the `Ok` arm of every `vogt.call` site delimits; `::an_imported_issue_body_arrives_as_data_like_any_other_stored_text`. There is no forge-aware path in the loop and there does not need to be — but "every" was a fact about two call sites rather than a rule, and a third added tomorrow would have been undelimited with nothing failing. Deleting the wrapping at one site now turns three tests red |
| FR-T4 | The system prompt names the delimiters as untrusted, and names all four the loop now uses | `assistant.rs::every_delimiter_the_loop_emits_is_one_the_prompt_names` and `::the_prompt_names_no_delimiter_that_nothing_emits`, over `emitted_delimiters`, which reads the tags out of the source of both emitting files rather than holding a list of them. Both directions, so a tag added to the loop and not the prompt fails, and so does a rule about a boundary that never arrives. §6.3 finding 13 is what the previous version of this test missed |
| **FR-T6** | Absent the API key, every assistant route answers 404 — the feature is invisible rather than broken | `integration.rs::without_an_api_key_every_assistant_route_is_absent`, over all three routes. A 500 or an empty transcript would make a deployment that never configured an assistant look like one whose assistant is broken |
| FR-S9 | The front door holds one token namespace carrying Vogt capabilities; the proxy strips the caller's credential and injects the paired core token; the proxy never pre-approves — a refusal forwards nothing | `engine/server/src/auth.rs::scoped_tokens_limit_capabilities`; `vogt_core.rs::the_core_is_handed_the_core_token_not_the_callers`, `::two_front_door_tokens_reach_the_core_as_two_actors`, `::a_write_needs_the_vogt_write_capability` and `::an_unauthenticated_caller_never_reaches_the_core`, both of which assert nothing was proxied |
| **FR-S9** | The audit records the real actor across the hop, and FR-S4's double gating is unweakened behind the proxy | `tests/test_front_door.py` — **the only test in this repository that runs both processes**. It boots `vogt serve` on a real socket and the engine binary in front of it, with two front-door tokens paired to two different core tokens; two, because one actor proves nothing (a proxy that hard-coded an identity would attribute a single actor correctly). Both gates are asserted in the arrangement that ships: a token with no capability is refused by the front door *before* the core is asked, though the core token behind it could write; a token that clears the front door is refused by the core when its own scopes do not allow the write; and neither refusal writes anything. Verified by mutation on both sides — pairing both tokens to one actor fails the first, and opening the front door's gate in `auth.rs` (rebuilt) fails the second. Skipped where the engine binary is absent, which is NFR-Q6's core-only run |
| **FR-S10** | **All four**: the token is per-session and actor-scoped, minted at start, revoked at stop, and its writes are distinguishable in the audit log | `tests/test_sessions.py::test_the_session_carries_its_own_token`, `::test_stopping_a_session_revokes_what_it_held`; `tests/test_m10_demo.py` step 5, which asserts the comment's actor is `agent:session:<id>` *and* differs from the `session.start` actor |
| **FR-U8** | **All three**: the PWA reaches Vogt only through the front door and no other origin; every path resolves against the operation registry; every engine path resolves against `app.rs`'s router *and* `API_CONTRACT.md` | `tests/test_pwa.py::test_every_vogt_path_in_the_pwa_is_a_registered_operation`, `::test_the_pwa_reaches_vogt_only_through_the_front_door`, `::test_every_engine_path_in_the_pwa_is_a_route_the_engine_serves`, `::test_every_engine_path_in_the_pwa_is_in_the_engine_s_api_contract`, `::test_no_vogt_surface_opens_its_own_door`. Read against source rather than a built bundle, because no bundle is built here — the sources are what a bundle is built from, and a second call site would fail the check |
| FR-U9 | The legacy GUI keeps serving, and parity is asserted rather than assumed — in both directions | `tests/test_pwa.py::test_the_pwa_renders_everything_the_legacy_gui_did` (an exact set, so a view added to the wrong front end fails too) |
| FR-U6, FR-U15, FR-U18 | r6's rule, on the surface most likely to erode it: no exported write can be called without a reason, and quick-create will not submit without one | `tests/test_pwa.py::test_every_vogt_write_the_pwa_offers_collects_a_reason` |
| **FR-T5** | Push-to-talk: the microphone is open exactly as long as the button is held, from a pointer or from the keyboard, and the release sends what was said | `web/src/__tests__/assistant.test.tsx` (six), the first thing in this repository to mount `Assistant.tsx`. Held rather than toggled because the take auto-sends — a toggle left on in a room with other people does not merely listen, it eventually speaks — and a pointer that ends off the button still ends the take. Two of the six were rewritten after the mutations they were meant to catch survived: one now asserts the take is *closed* once rather than that one message was sent, because sending clears the draft and the message count stays right for the wrong reason, and the other says in its own body that the behaviour it pins has a spare mechanism holding it |
| FR-U16 | Every read *view* is reachable from the palette; the palette can never execute a write, checked by import | `tests/test_pwa.py::test_the_palette_reaches_every_vogt_surface`, `::test_the_command_palette_never_writes_to_vogt` |
| **FR-U16** | The mutating verbs whose collector is a *place* — quick-create, the drift inbox, the import form — each open that view and perform nothing | `commandPalette.test.tsx`'s FR-U16 block (three), asserting the URL each entry opens and that choosing one sends Vogt no write at runtime; `test_pwa.py::test_the_command_palette_never_writes_to_vogt` asserts the same rule by import. **What is deliberately absent is per-item verbs**: "Comment on…" with no item cannot open a form that collects anything, and an entry per verb per item would multiply the list by five, so transition, comment and session start are reached through the item's own fuzzy-named entry, which opens the page carrying all three forms |
| FR-U16 | Projects and work items are reachable *by name*, fuzzily, and a project's entry opens the deep link a shared URL uses | `web/src/__tests__/commandPalette.test.tsx` (`offers each registered project by name`, `finds a project by a fuzzy fragment of its name` — `rstnz` finds `rustnzb` and does not drag in `Vogt` — `opens the project's own deep link`, `still reaches work items and every view by name`, and `contributes nothing when Vogt cannot be asked`, which is the palette declining to fail open) |
| FR-U4 | The columns are the union of `workflow.list`'s states, walked from each machine's initial state, and are never written down | `web/src/__tests__/board.test.tsx` (`draws one column per state the server published`, `changes shape when the workflow does` — a different machine and not one previous name survives — and `gives a state no machine mentions a column, and says no machine mentions it`) |
| FR-U4, FR-U12 | A drag is a `work.transition`; it renders optimistically before anything is written; a refusal rolls the card back; the refusal is Vogt's own sentence, rendered in the column the drop landed in | `board.test.tsx` (`moves the card on the drop, before anything is written`, `sends the transition with the reason the user typed`, `rolls a refused move back and renders Vogt's own sentence where the drop happened`, `will not submit a move with no reason`, `puts the card back when the reason is abandoned`). Residual: the tests fire `dragStart` and `drop` and never `dragover`, whose `preventDefault` is what lets a browser deliver a drop at all — the semantics are asserted, the gesture is the M11 demo's |
| FR-U12 | A refused state is never persisted, cached or re-derived, on the board and on the item page | `board.test.tsx::never persists a state the server refused` (the only thing in storage is which columns collapsed); `workItemDetail.test.tsx::does not re-derive a refused value when the editor is reopened` |
| FR-U12 | An inline edit renders optimistically and reconciles against the server's answer, which wins | `workItemDetail.test.tsx` (`renders the change while Vogt is still deciding, and says it is unsaved`, using a held response so the optimistic frame is observable at all; `keeps the server's version of the change, not the one that was typed`; `rolls a refused edit back and shows Vogt's own reason beside the field`) |
| FR-U6 | Bulk transition and bulk label, each one audited write per item carrying the batch's reason, each refusing without one, each reporting a partial batch as partial in Vogt's words | `backlog.test.tsx` (six tests for label, three for transition, including `does not let a reason typed for a transition justify a labelling` and `leaves the refused items selected and the rest not`) |
| FR-U6, FR-U15 | The ranked views render, quick-create raises an item, and bulk transition moves a batch — as things a person operates rather than bindings that exist | `backlog.test.tsx`, `board.test.tsx::raises an item without leaving the board` (the new card appears in its column and no navigation happens) |
| FR-U15 | Quick-create exists on the board as well as the backlog, will not submit without a title *or* a typed reason, never prefills the reason, and is unreachable while Vogt cannot be asked | `board.test.tsx`'s FR-U15 block (seven), including `never prefills the reason, however convenient the last one was` — the board deliberately remembers the last *move's* reason, and the create form deliberately does not inherit it |
| **FR-U10** | Drift arrivals and notification counts re-read on `vogt-changed` rather than waiting to be asked; all five surfaces report their own age; a lost stream is indicated and reconciles | `web/src/__tests__/live.test.tsx` (twenty-four) over `web/src/viewAge.tsx`, which is the board's inline machinery lifted out so there is one implementation and not five. Two surfaces deliberately do *not* subscribe, and each is pinned by a test so it stays a decision rather than an omission: the backlog, because re-ranking the estate under a reader's cursor on every announced change is a different act from telling them how old the view is, and the project page's sweep aggregates, because a four-panel pull per transition is a poll wearing an event's clothes. The drift re-read is refused while a card holds a half-typed reason, since refetching replaces every proposal object and would eat it (FR-W1) |
| **FR-U10** | A lost stream is indicated and reconciles on reconnect | `live.test.tsx`, over the fix in `api.ts`. This conjunct was in §6.2a — implemented, asserted by nothing — and was not implemented: an ended stream broke the read loop and returned, so a restarted engine left the client believing it was connected with no reconnect scheduled. §6.3 finding 17 |
| **FR-U11** | A pasted link opens the surface it names — project, work item, session and audit query — and a second link opens a second surface without closing the first | `web/src/__tests__/shell.test.tsx` (nine), which mounts the **whole shell** rather than a surface. That was the half M11 found broken for every surface and the half nothing mounted, and the stated blocker was wrong: `App.tsx` mounts in jsdom given four engine stubs and nothing else. `/audit?ref=WI-1` is asserted to carry the item's `entity_id` rather than opening an unfiltered log, and a stale session id opens no phantom tab |
| **FR-T6** | Every GUI hides the assistant when it is not provisioned, the route included | `shell.test.tsx` (two): `#/assistant` with `assistant_enabled` false opens no tab and no drawer button, and the same link *with* a key opens it. The mirror is what makes the first assertion worth anything — without it, a dead route would pass |
| **FR-U7** | The project page carries all six: brief, CI status, contract and compliance, drift inbox, dependency graph, import form | `web/src/__tests__/projectPage.test.tsx` (nine), asserting contents rather than presence — the failing CI check by name, the criteria under the compliance status, the declared-versus-observed version in the brief, the unresolved count on the graph |
| **FR-U5** | The item's own state history: every entry into a state, oldest first, with what it came from, when, who moved it and why | Fifteen tests in `workItemDetail.test.tsx`. The reasons are *fetched* rather than linked, and the deciding argument is not about reasons: an event names its actor by ULID, so "who moved it" needs the audit row whatever the panel decides about why — and once that row is held, linking away for a sentence already in memory charges a click for nothing. Where the join does not close, the row shows the id the event carries and links into the audit pre-filtered; never a blank, because Vogt refuses a write without a reason and a blank would read as "nobody gave one". The walk is bounded and says so when it hits the bound, naming which end is missing |
| **FR-U5** | Description, comments, relations, labels, collected evidence and the start-session control, on one page | Seven tests in `workItemDetail.test.tsx`, six for the panels and one holding all six together. Relations render as links to the related item; evidence shows the per-input contributions and the inputs that did not fire |
| FR-U11, FR-U14 | The board's six filters and its swimlane mode are the URL: restored from a pasted link, written back when chosen on the surface, round-tripped, and put back when the shell navigates to the bare path | `board.test.tsx`'s FR-U11 block (four) and `restores every one of the six filters from a pasted link`; `backlog.test.tsx::round-trips a filter chosen on the surface through the URL` |
| FR-U14 | A combined filter is nameable, recalled in full, survives a reload as per-client state, and a refresh interval is not part of what a name means | `board.test.tsx`'s FR-U14 block (five), including `keeps the multi-valued filters intact through the round trip` and `does not let a named view change how often the board refreshes`; `backlog.test.tsx::saves a named filter set and recalls it` |
| **FR-U17** | A claim backed by a still-running session is marked provisional, not fresh — read from the observed store itself, not from a ranking over it | `web/src/WorkItemDetail.tsx`'s "Observed evidence" panel, through the new `observations.list` binding in `vogtApi.ts`; ten tests in `workItemDetail.test.tsx`. Three states, not two: an observation whose payload does not carry the flag at all is `unverified` rather than settled, in the same words the trust badge uses, because a blank says "no opinion" when the honest answer is "nobody checked". `settlement()` reads the collector's own `provisional` flag rather than re-deriving liveness — a surface deciding for itself what "still running" means is a second copy of a rule the collector already keeps, and the two would eventually disagree. There is no work-item parameter on the operation, so the panel matches on the payload's `work_item`, which `session_outcomes.py` writes for both kinds it produces |
| FR-U17 | Trust state is on every card and every ranked row, and an absent one reads as `unverified` rather than as blank; the aggregate says how old it is | `board.test.tsx`'s FR-U17 block (four — including `puts one on every card, so the aggregate cannot drop the awkward column`); `backlog.test.tsx::reads an absent trust state as unverified`. A blank badge says "no opinion"; the honest answer is "nobody checked" |
| **FR-T5** | Replies are spoken in sentence chunks when the toggle is on and not when it is off; a pending action is announced in words that offer no spoken way to approve it; a new message stops the previous answer mid-sentence | `web/src/__tests__/assistant.test.tsx`'s FR-T5 block (four), driving real replies through a stubbed engine and a stubbed synth. The announcement test asserts an *absence* — no "say yes", no "yes or no" — which is FR-T2's voice clause and the one thing a device demo cannot check, since it can tell you what was said and not what was carefully left unsaid. Writing it found that the chunker split `work.transition` at its own full stop, so the one word saying what was about to happen was read as two |
| FR-U22 | Focus moves across and within columns; `Shift`+arrow proposes a move and still collects the reason; `Enter` opens the item at its own URL; `n` opens quick-create and is announced on the board's own keyboard line | `board.test.tsx`'s two FR-U22 blocks (five), including the one asserting that an `n` typed into the move composer is not stolen by the shortcut |
| **FR-U6** | The `why` panel draws the contributions `GET /why` returned, and says which inputs did not fire | `backlog.test.tsx` (four). The default estate answered that route with an empty `contributions` array, so no existing mount could have seen the panel draw nothing — which is why this was worth writing rather than assuming |
| **FR-U13** | Swimlanes group by project or initiative; per-column WIP counts count what is drawn; lanes and columns collapse and expand; the layout is per-client and survives a reload | `board.test.tsx` (seven). The swimlane *mode* was asserted as a filter value and the grouping it produces was not; `data-wip` was rendered and read by nobody. Counting `item.state` instead of the drawn state turns it red, which is the difference between a WIP count and a filter |
| **FR-U16** | Sessions are reachable by fuzzy name, labelled by name rather than id, and each opens its own terminal | `commandPalette.test.tsx` (four), with the engine's session store seeded — no test had done that, because the entries come from the engine and the harness answers Vogt |
| **FR-U18** | Both sides of a disagreement, with provenance and age, rendered *open* before any act is possible | `driftInbox.test.tsx` (seven). Asserted as an ordering and a state, not a presence: moving the evidence after the controls fails, and so does wrapping it in a `<details>` — a person must not be able to resolve a proposal they have not been shown |
| FR-U18 | Bulk accept does not exist, and cannot arrive by accident | `tests/test_pwa.py::test_drift_is_resolved_one_proposal_at_a_time` (one call site, and no multi-select) |
| **FR-U19** | Filter by actor, project, operation and a half-open time range, server-side; and the log is readable past its newest page | `web/src/__tests__/auditQuery.test.tsx` (seventeen), which asserts **the query the server receives** rather than the rows drawn — a filter applied client-side and one applied server-side look identical on screen and differ entirely in what they can see, so a rows test would have passed against the old surface. The presets are day-aligned so `Yesterday.until === Today.since` and the half-open interval tiles; the timezone is stubbed off-UTC so a build that appended `Z` to typed wall-clock text fails. A filter that cannot be pushed to the server now stops the read rather than filtering one page of a wider query under a total describing the wider one |
| **FR-U20** | Both legs: the live activity badge and the open-terminal control on the item, and the terminal's link back to the work item it was opened for | `workItemDetail.test.tsx`'s FR-U20 block (four) and `terminalLink.test.tsx` (four). The forward leg asserts the engine's activity is what is shown, that an unasked session reads `unknown` rather than `idle`, that the control addresses the *engine's* session id, and that a stopped session gets no control to a PTY that is gone. The return leg asserts the link, that the read includes stopped sessions because a finished run still had a subject, that a PTY Vogt did not start says nothing, and that an unreachable Vogt costs the badge and never the terminal (FR-E9). The blocker §6.2a recorded was smaller than it looked: xterm asks for `matchMedia` at mount and jsdom has none, so `Terminal.tsx` could not be mounted at all — `setup.ts` stubs it the way it already stubs `ResizeObserver` |
| **FR-U21** | The **engine** away, rather than Vogt: the Vogt views keep answering and the session controls disable with the named reason | `engineAbsent.test.tsx` (eight), the mirror of the outage tests that already existed — every one of those takes Vogt away, and none took the engine. The row needed a qualification, now in that file's header: every Vogt read goes through the engine's front door, so "the engine is unavailable" cannot mean the front-door process is down — that takes the Vogt views with it and leaves no absent state to design. The case the requirement names is the front door answering while the *session* engine behind it cannot be reached, which `sessions.list` reports in its `engine` note |
| FR-U21 | Every Vogt surface can tell an outage from an empty answer and renders the server's own reason; the core-absent half — terminals, files, git keep working | `tests/test_pwa.py::test_every_vogt_surface_distinguishes_an_outage_from_emptiness` asserts the structural half and says in its own docstring that it cannot judge the copy; `web/src/__tests__/absentStates.test.tsx` and the outage blocks in the other three files now judge it, on all five surfaces — the server's sentence verbatim, the sentence that says what the absence *means*, nothing rendered as data, writes disabled, and a 500 called a failure rather than an outage. `contextkeeper.rs::a_contextkeeper_outage_leaves_terminals_working_and_unprotected` is the core-absent half. **One panel is thinner than the rest**: the item page's Sessions panel renders Vogt's sentence and nothing asserts it does — found while mutation-testing FR-U17, when a mutation that should have gone red survived because the same literal appears first in `sessionsFailure`. The conjunct is delivered on all five surfaces; that one panel is where a regression would be quiet |
| **FR-M1** | A session waiting for input is answered from the item page, without a terminal — MVP1's "session start/approve", as MERGE §14's M12 demo defines it | `web/src/__tests__/workItemDetail.test.tsx`'s FR-M1 block (six). The control shows the session's own scrollback tail before it offers any way to answer, and offers none at all when the engine cannot be asked. The other reading of "approve" — a Vogt approval *operation* — is deliberately not built, and §6.2 no longer carries a row implying it is owed |
| **FR-T7** | The recorded hang is refused with a named reason — the second of the two ways the requirement offers out of it | `assistant.rs::openai_route_refusal` and three unit tests, plus `integration.rs::a_claude_route_on_the_openai_backend_refuses_with_a_named_reason`, which drives the route and asserts the answer is *not* a 404 — reporting the assistant absent would send an operator looking for a missing API key when the key is fine. The sentence names the model, the transport and the setting that overrides it, because a hang is indistinguishable from thinking and the 60-second client timeout that used to catch it reported "took too long" about something that was never going to answer. `assistant_allow_claude_proxy` turns it off: the fault is a proxy's, not the model's, and a deployment whose proxy serves those routes is entitled to own the result. The check is on the transport, so FR-T7's other clause — a native Anthropic backend, still unbuilt — is untouched by it |
| FR-M2 | The default set is exactly the four FR-M2 names and no more; only newly-raised drift notifies; the core's changes are republished on this server's stream from a cursor that does not replay history at boot | `push.rs::only_the_kinds_fr_m2_names_are_on_by_default`; `push_api.rs::only_newly_raised_drift_notifies`, `::a_new_core_event_kind_does_not_opt_itself_in`, `::session_events_are_not_drift`; `vogt_core.rs::the_core_s_changes_are_republished_on_this_server_s_stream`, `::the_follower_does_not_replay_history_at_boot`. **This row previously cited a `vogt_drift.rs` under the engine's `src`, and three tests in it. No such file exists and none of those three tests exist anywhere** — see §6.3 finding 24. The summarising and coalescing conjuncts moved to §6.2a in the same correction |
| FR-M3 | The board's list layout at phone width, in the three rules that make it one: `display: block` on the row, the head row hidden, and the state name grown out of `attr(data-state)` | `tests/test_pwa.py::test_the_board_is_a_list_below_the_narrow_breakpoint`, `::test_the_board_cells_carry_what_the_hidden_head_row_said`, `::test_the_vogt_surfaces_share_the_engine_s_narrow_breakpoint` |
| NFR-D11 | The engine's native APIs; the WebSocket attach path; `/api/vogt` proxied under its own prefix with the query string intact; `/mcp` proxied with the caller's credential unchanged; aggregate health with a non-fatal core check | `engine/server/tests/vogt_core.rs` (`a_vogt_read_reaches_the_core_under_its_own_prefix`, `a_query_string_survives_the_hop`, `mcp_forwards_the_callers_credential_unchanged`, `a_reachable_core_is_reported_with_its_schema_state`); `integration.rs::readyz_is_public_and_returns_checks` |
| **NFR-D11** | The engine serves the front end from the same port that serves MCP, and that port answers plain HTTP health | `vogt_core.rs::the_port_that_serves_mcp_also_answers_plain_http_health` (one origin, an MCP call and an unauthenticated `/healthz`, no JSON-RPC) and `::the_engine_serves_the_front_end_from_that_same_port`. The second asserts the serving rather than the contents — the suite compiles against a placeholder `web/dist/`, and a real bundle differs from it in every byte except the shape. Removing either route turns one red. The loopback and single-published-port halves are asserted in `tests/test_deploy.py` |
| **NFR-I6** | **All four**: the core's SQLite; the engine's `state_dir`; enough metadata to re-establish FR-E3's path agreement; and one act that covers them | `tests/test_lifecycle.py` — nine for the stores, and six more: `test_a_backup_carries_the_engines_state_and_says_so`, `test_a_backup_without_the_engine_says_which`, `test_a_backup_survives_an_unreadable_engine_directory`, `test_a_restore_puts_the_engine_state_back`, `test_a_restore_reports_an_estate_that_moved`, `test_an_older_manifest_still_restores`. The manifest is version 2 and carries `engine_state` — a sentence on *every* branch, so a backup that covered two thirds of the product is distinguishable from one that covered all of it before somebody restores it — and `import_root`, which with the restored `projects.root_path` values and the front door's `workspace_agreement` check is what re-establishing the path agreement needs. The restore **reports** a moved estate (`import_root_then` / `import_root_now`) and does not rewrite the stored paths, which is the right division — the requirement asks for the metadata, not for a silent rewrite of every project's root — but it is worth knowing that nothing enumerates which projects are now unresolvable. §6.3 finding 14, resolved: the merged compose sets `VOGT_ENGINE_STATE_DIR`, and because two configurations now name one directory, `api.rs::check_backup_agreement` publishes a `backup_agreement` readiness check that says either which directory the backup covers or what the archive would silently omit (`vogt_core.rs`, three tests) |
| **NFR-S5** | Long lists virtualize on all three surfaces that should, and the board's filter and drag paths do not degrade with backlog size | `web/src/__tests__/boardScale.test.tsx` (fifteen). The board's 60-card cap is gone: 60 is now where windowing *starts*, following `Backlog.tsx`'s mechanism rather than a second idea of what virtualization means, and the audit browser pages deliberately because an audit row is variable-height and a reason is never truncated. The second clause is evidenced by **counting, not timing** — jsdom has no layout, so the tests instrument `JSON.parse` to count reads of the one field the projection walks, which keeps the counter out of the product. Tripling the columns adds no pass; four times the estate in four times the lanes costs no more per item; eight keystrokes in the reason composer cost exactly zero, because `placement` compares on ref and target alone. `tests/test_pwa.py` holds the cross-file guard neither suite can: the CSS card height and the windowing constant must agree, or what is drawn and what is under the scrollbar drift apart silently |
| **NFR-Q6** | Both suites pass in the merged repository — the halves are path-gated, so "both" is true because one job weighs every result and refuses anything that is neither a pass nor a deliberate skip | `tests/test_deploy.py::test_the_gate_fails_on_anything_that_is_not_success_or_skipped`. The case worth pinning is `cancelled`: a gate written as *fail only on failure* passes a cancelled job, and a cancelled job checked nothing — which this repository was bitten by today from the other direction (§6.3 finding 19). Rewriting the catch-all arm as `failure)` turns it red |
| NFR-Q6 | The forge-less run; the core run with no engine present, produced by deleting `engine/`, `web/` and `mobile/` rather than by inspection | `.github/workflows/ci.yml` job `core`; `tests/test_pwa.py`'s skip guard is what lets it pass. **Run for real on 2026-08-14** — the job passed on a self-hosted runner, so "the core alone still works" is now an observation rather than a workflow file |
| **NFR-C6** | The pipeline governs the merged image: it is built from `engine/Dockerfile` with the repository as context, the PWA is built first because `rust-embed` reads `web/dist/` at compile time, both entrypoints run in the candidate before the push, the digest is signed, and a tag can release it | `tests/test_deploy.py::test_the_merged_image_is_built_from_the_engine_dockerfile`, `::test_the_pwa_is_built_before_the_merged_image`, `::test_both_halves_run_before_the_merged_image_is_pushed` (all three over both workflows), `::test_a_tag_can_release_the_merged_image`. The build half is more than asserted — it ran, on 2026-08-14, and published a signed `dev-ee18adc`. The release half is a path that exists and no tag has yet taken, which is why its test reads `release.yml` rather than a registry |
| **NFR-C6** | fmt, clippy, `cargo test`, `pnpm typecheck`, `pnpm test`, the APK build and pytest are all in the pipeline | `tests/test_deploy.py::test_every_gate_nfr_c6_names_is_in_the_pipeline`, which restates the requirement's seven and asserts each against `ci.yml`; `::test_the_local_check_runs_what_ci_runs` holds `scripts/check.sh` to the same list from the other side. Presence, not gating — §6.2's row records that each half runs only when its own paths changed, with the argument in the workflow. A step present and skipped is a decision; a step absent is an accident, and until now nothing would have noticed one |
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

Ten conjuncts. §5.4a: the conjunct is the row, not the ID. Each row
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
| FR-T7 — a native Anthropic backend | `ChatBackend` has two variants, `Http` and a test mock. Nothing in the repository speaks the Anthropic API. | **Deferred (r12)**, §3 — no longer short, decided |
| FR-U5 — "per-item audit trail" | **The silent omission is closed.** An item's trail now includes the writes audited against its comments, by semi-join through `comments.work_item_id` — chosen over denormalising a work item id onto `audit`, because back-filling `audit` means editing the record of what happened. What remains is the same division as before: a link to the audit browser rather than an embedded trail, which is FR-U19's second clause working. | Low, down from Medium — the trail no longer omits a kind of write, which was the part that mattered |
| NFR-D12 — "deployed to a dev stack for live validation" | Nothing deploys, from any branch. `build.yml` says so in its own step summary, and `ci.yml` records the decision — Vogt has never deployed from CI (NFR-D10). **The artefact now exists**: on 2026-08-14 the `stack-image` job ran for the first time, built `engine/Dockerfile` with the repository as its context, ran `vogt --version` and `mydevenv2-server --help` inside the candidate, signed the digest and published `dev` and `dev-ee18adc`; `deploy/vogt-stack.compose.yml` pins that digest rather than the placeholder it carried. So the image a dev stack would run and the image `dev` builds are the same artefact, and it is a real one. **What is left is one human act** — `docs/DEPLOYMENT.md` §9.4 — and until it happens no stack has run this image and mobile, voice and push remain unvalidated. | **High**, and now blocked on a deploy rather than on a build. Everything this section can do for it has been done |
| NFR-D12 — "only `main` deploys to prod" | Vacuously true, per the row above. `main` builds a `sha-` image and publishes it; a human pins a digest and deploys. | — |
| NFR-C6 — a signed APK | **The pipeline exists and the key does not.** `release.yml`'s `android` job assembles a release build, verifies the signature with `apksigner` rather than trusting Gradle — an absent signing config yields a silently *unsigned* release APK, which is the artefact worth stopping — and refuses to start without the four keystore secrets, naming which are missing. `ci.yml` still builds only a debug APK on every push, because signing is what turns a build into something somebody installs and that belongs to a release (NFR-C3's rule, applied to the APK). What is short is the keystore itself: it is in the retired forge, and recovering it is a deadline rather than a task — a new key is a different app identity, so losing the old one does not stop signing, it permanently stops *upgrading* every device already carrying the app. `DEPLOYMENT.md` §9.6. | Low, and now waiting on a key rather than on a decision |
| NFR-C6 — "shall run both halves on every push" | On pushes to `main` and `dev`, and on pull requests — a push to any other branch with no PR open runs nothing. Within that, each half runs only when its own paths changed: a `mobile/`-only push runs no Rust and no Python; a `web/`-only push runs no APK build. This is NFR-C1 working as intended and NFR-C6's literal sentence being false; the reduction is deliberate and argued in the workflow. | Low, and honestly documented in the file |

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

Forty-one conjuncts whose code was read and believed, which nothing in any
suite would notice the loss of. (Sixty-five, less the seven the fourth pass
asserted —  NFR-D12, FR-U10, FR-U20, FR-E1, FR-E9, FR-T1/T2 and FR-T6 — which the fourth
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
| FR-M2 | New drift is *summarised* and *coalesced* — a burst inside the window becomes one notification titled "N new drift proposals" | A test that drives `spawn_vogt_drift_watcher`'s bus. The window, the count and the title are in `push_api.rs` and nothing reads them; the three tests beside them are all about which kinds notify. Recorded here by §6.3 finding 24, which found them cited in §6.1 as tests that do not exist |
| FR-U10 | Session activity updates from the SSE stream without a refresh | A test that drives the engine's own activity events through `store.ts`. The board's half is asserted now — `live.test.tsx` drives `vogt-changed` end to end — and the session-activity half still rests on inspection |
| FR-U19 | Actor and operation filtered server-side; every row shows who, what and why; the item page links in pre-filtered *(from §6.2b)* | A mount of `AuditBrowser`, which `absentStates.test.tsx` already does for its outage states and never with records in it |
| NFR-S5 | No view fetches the whole estate to render a page of it | True on inspection of all four surfaces, and the board's 2,000-item bulk read across four sequential requests is the weakest case rather than a clean one |

**One thing the board work leaves behind, and it belongs here rather than in a
row of its own.** Restoring a cell's *DOM* scroll position when the board
rebuilds is untested and untestable here: jsdom has no layout, so the window
slice is correct either way and only a real browser's scrollbar depends on it.
The same is true of the card geometry — 116px is arithmetic from the existing
font sizes and padding, not a measurement. `tests/test_pwa.py` guards the one
number that matters arithmetically, that the CSS height and the windowing
constant agree; the rest is visual and is M11's standing caveat.

### 6.2b Unverifiable in this environment

**Seven conjuncts, down from sixty-nine.** Not doubted — read, and believed,
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
| FR-T5 | That a **speaker makes sound** | A device. Everything else about spoken replies moved to §6.1: whether the app asks for speech, with what text, and whether it ever offers a voice route to approving something are decided in `Assistant.tsx` and are now asserted there. A speaker was always the worst instrument for the last of those, because what is being checked is the absence of a sentence |
| FR-M3 | The Vogt surfaces at phone width | The same APK, or a browser at 375px. jsdom loads no stylesheet, so `test_pwa.py`'s three assertions about the board's narrow-breakpoint rules remain assertions about `styles.css` as text |
| NFR-D11 | One stack — the two processes coming up together, under one supervisor, in one container | **The image half is settled.** `build.yml`'s `stack-image` job ran, so `docker build -f engine/Dockerfile .` is no longer hypothetical: it builds, and the step that was called the riskiest unproven one — copying uv's standalone CPython between build stages — holds, because `vogt --version` runs in the built image and could not without it. `mydevenv2-server --help` runs too, so both halves are present. The published-port claim moved to §6.1 when a test began reading the compose file. What is left is the thing neither a build nor a file can show: `entrypoint.sh` starting vogt-core beside the engine, the engine finding it on loopback, and `/readyz` reporting a core it actually reached — which needs the compose stack up (`DEPLOYMENT.md` §9.4) and `scripts/smoke_merged_stack.sh` pointed at it |

### 6.3 What this pass found that no requirement names

The first three were found by running the two halves together and looking at
what came out; the rest by this audit and the one after it. All of them share
a shape — **working behaviour and a false record** — which is the failure
class this product exists to make visible.

Findings are struck through when the thing they describe is fixed *and* the
fix was checked here — **thirteen are** (4, 5, 7, 11, 12, 13, 14, 15, 17,
19, 21, 22, 23), one of them (5) only in part, and two of the rest (6, 8)
have closed a half each. Twenty-four entries, eleven of them open.

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
17. ~~**A conjunct in §6.2a was not implemented.**~~ **Resolved, and the
    bucket's own definition is what let it sit there.** §6.2a is "implemented, and asserted
    by nothing" — code read and believed. FR-U10's "a lost stream shall be
    indicated and shall reconcile on reconnect" was read, believed, and
    false: `subscribeEvents` treated an ended stream as a normal exit, so a
    restarted engine or a proxy's idle timeout ended the body cleanly, the
    read loop broke, and the client went on believing it was connected with
    no reconnect ever scheduled. Every surface that re-reads on an
    announcement went quiet **while looking live** — which is the clause the
    age badge was being added to satisfy, failing in the one way the badge
    could not cover.

    Two things about how it was found are worth more than the fix. First, it
    was found by writing the test, not by reading the code again — the code
    had already been read twice and believed twice. Second, the row in §6.2b
    that had excused the test said "jsdom has no `EventSource`", and that was
    a **false premise**: this client uses `fetch` and a `ReadableStream`
    precisely so it can send a bearer token, so the existing harness was
    always sufficient. An excuse nobody rechecked kept a defect alive through
    two audits.

    The suite had a matching hole, found the same way.
    `@solidjs/testing-library` registers its own cleanup only when
    `afterEach` is a global, and this suite deliberately runs without Vitest
    globals — so **nothing any test mounted was ever unmounted**. A second
    board answered the first board's events; an old inbox refetched into the
    next test's call log. Harmless while assertions were about one container,
    and not harmless at all for a test that counts the calls a surface makes.
    Every green run before this one was greener than it should have been.

19. ~~**A cancelled run means a commit is checked by nothing, ever.**~~
    **Resolved on the second attempt; the first was struck through here while
    still broken.** NFR-C1
    classifies a push by `before..sha`, so each commit is examined by exactly
    one run and no later run looks at it again. `ci.yml` set
    `cancel-in-progress: true` for every trigger, so pushing twice in a few
    minutes cancelled the first run — and the second run's range began after
    the first commit. Those files were not checked later; they were checked
    never.

    Not hypothetical, and not caught by CI: `tests/test_deploy.py` changed in
    one push, that run was cancelled by the next, and a line-length error
    reached `dev` and sat there green. It was found by running `ruff` by hand
    while looking at something else. The fix is one expression —
    cancellation now applies to pull requests only, whose runs classify
    against the merge base, so a later run covers everything an earlier one
    would have — and `test_deploy.py` asserts it.

    **The first fix did not work, and the record said it had.** Setting
    `cancel-in-progress` to false for pushes governs runs that are *in
    progress*; a run still **pending** is cancelled whenever a newer run
    joins its group, unconditionally and whatever that setting says. On a
    single self-hosted runner nearly every run is pending for a while, so the
    hole stayed open — and a `ruff format` failure went through it in exactly
    the way the lint error had, hours after the workflow gained a comment
    explaining that this could no longer happen. It was found by noticing a
    cancelled run with **no jobs at all**, which is what a pending
    cancellation looks like.

    Pushes are now keyed by commit, so no two pushed commits share a group
    and none can supersede another. The test asserts the group, not the
    setting: `cancel-in-progress` was the thing that read as a fix and was
    not one, and a test asserting it would have gone green on both versions.

    **Confirmed by watching, which is the only reason it is struck through
    this time.** Three pushes in quick succession left three `ci` runs queued
    together — `654ccc3`, `4f669f5`, `14d4456` — where each would previously
    have cancelled its predecessor while pending. The cost is visible in the
    same picture: three runs behind one runner, which is the trade this
    chooses, runner time for the guarantee that no commit goes unchecked.

    The shape is worth naming because this list keeps finding it in new
    dress: **a check that did not run and a check that passed are the same
    colour on a dashboard.** §6.2's NFR-C6 row already records the deliberate
    path gating; what it did not anticipate was gating plus cancellation
    turning a reduction in *when* checks run into a hole in *whether* they
    do. What I did not anticipate is that fixing it wrongly would look
    identical to fixing it.

24. **§6.1 cited a file that does not exist.** FR-M2's delivered row named
    `engine/server/src/vogt_drift.rs` and three tests in it —
    `one_drift_is_named`, `a_burst_is_one_notification_that_counts`,
    `a_cursor_survives_a_round_trip`. There is no such file. None of those
    three test names appears anywhere in the engine. The drift watcher is
    `spawn_vogt_drift_watcher` in `push_api.rs`, and the three tests beside
    it are about which event kinds notify.

    This is the worst entry in this list, and it is in the *delivered* table.
    §6.1's own rule is "a file and a test" — not a citation, because §5.4a's
    whole point is that a grep for an ID finds the docstring naming it. The
    row named a file and tests specifically enough to look trustworthy and
    nobody, through four passes, ran the grep. **The summarising and
    coalescing conjuncts were counted as delivered on the strength of tests
    that were never written**; they are in §6.2a now, and the row cites what
    exists.

    What makes it findable at all is mechanical: every path and test name in
    §6.1 can be checked against the filesystem, and that check now runs in
    `tests/test_requirements_audit.py`. It should have existed the first time
    this section claimed a test by name.

23. ~~**An assertion against a class that exists nowhere.**~~ **Found by a
    survivor, fixed before it landed.** A new FR-U21 test asserted the board
    renders no outage banner by querying `.board-outage` — a class in no file
    in this repository; the surface renders `board-banner--outage`. The
    assertion could not fail, and it read exactly like one that could.

    What found it was a mutation surviving for an unrelated reason: pinning
    the board's `outage` signal left the test green, because a successful
    read clears that signal before the assertion runs. Chasing why led to the
    selector. Neither the mutation nor the test was wrong about the product —
    the mutation was neutralised, and the assertion was vacuous — and one led
    to the other only because a survivor was treated as information rather
    than as noise.

    A negative assertion is where this hides: "nothing is rendered" is true
    of a selector that matches nothing, of a component that renders nothing,
    and of a component that does not exist. §6.3 finding 21 is its sibling
    from the other direction — an assertion that could only ever pass.

21. ~~**A test that proved the wrong path, and looked exactly like proof of
    the right one.**~~ **Caught by a surviving mutation, and rewritten.** The
    first draft of FR-T2's expiry test aged a pending card past 120 seconds,
    read it back through `pending_action()`, and then tried to approve it. The
    approval was refused, the test went green, and it was asserting nothing
    about approval: *reading* expires the card too, so the refusal came from
    the read. Deleting the expiry check from `resolve_action` — the path the
    test was named for — left it green.

    Rewritten to approve a card nothing has looked at, which is also the real
    client: one that never polls history, comes back to a stale screen, and
    presses the button on a 121-second-old card.

    The class is worth naming, because it is not the same as the others in
    this list. Those were records that stopped being true. This is a test that
    was *always* false and could only ever pass — the assertion was real, the
    behaviour was real, and the arrangement quietly routed the one through
    somewhere other than the other. Only a mutation finds it: reading the test
    shows a card, a clock and a refusal, in the right order.

22. ~~**Both first-run CI failures were checks written against this
    machine.**~~ **Both resolved, in the commits that found them.**
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

---

## 7. Designed and not delivered — the gap register *(r15, 2026-08-17)*

**What this section is for.** §5 and §6 verify requirements against the build,
one ID at a time, and they are the authority on *whether a requirement is met*.
This section answers a different question that neither could: **what was
designed, anywhere in this documentation set, and is not here?** Until this
pass that question had no answer, because the gaps were true but scattered —
one in `DESIGN.md` §3.5, one in §4.1, one in §5, three in a roadmap stage's
closing paragraph, and eleven in documents belonging to a product that had been
merged into this one eighteen stages earlier.

Every row below was previously written down somewhere. **Nothing here is a new
discovery**, and that is worth stating plainly: the design documents were
honest. What they were not was countable.

### 7.0 What the consolidation moved

The pass that produced this register removed nine documents and the
`docs/engine/` directory, and reduced a tenth to a pointer. Their content did
not disappear:

| Was | Is now |
|---|---|
| `docs/engine/API_CONTRACT.md` | `ENGINE.md` §5 |
| `docs/engine/ASSISTANT.md` | `ENGINE.md` §6 |
| `docs/engine/AGENT_TASKS.md` | `ENGINE.md` §7 |
| `docs/engine/INTENT.md`, `PLAN.md` | `ENGINE.md` §1–§2 for what was built; §7.3 below for what was not |
| `docs/engine/TOOLING.md` | `DEPLOYMENT.md` §10 |
| `docs/engine/USER_GUIDE.md` | `USER_GUIDE.md`, rewritten to cover both halves |
| `docs/engine/uplift.md` | §7.2 below, and `ROADMAP.md` for what shipped |
| `engine/deploy/KOMODO.md` | `DEPLOYMENT.md` §11 |
| `engine/README.md` (384 lines) | `ENGINE.md`; the file is now a pointer |

`MERGE_MYDEVENV2.md` stays as it is. It is the record of a decision taken on a
date, its §12–§14 already say they were folded and are no longer authoritative,
and editing a dated investigation to match a later world would destroy the one
thing it is good for.

### 7.1 Owed — a numbered requirement that no code meets

Each carries an ID, and §5 or §6 is where its verification lives. Priority is
the requirement's own.

| ID | Pri | What is missing | What the absence costs |
|---|---|---|---|
| **FR-T5** | S | **A person, a microphone and a device.** The prompt-level mitigation is delivered *and now asserted* (r13): three tests pin the vocabulary it teaches and the fact that replies are spoken, so the sentence standing in for the validation pass cannot be trimmed away silently. What remains is the pass itself — speaking to it and finding out. | Voice was adopted unproven and the unproven part is now exactly one thing: whether a recognizer's output survives contact with `WI-7` and a project slug. Everything reachable without hardware has been done. |
| **FR-D9** | C | Any producer of a `declared` dependency edge. | A dependency expressed only in a deploy script or a person's head is invisible to `deps` and to the reverse lookup. |
| **FR-E10** | C | An operable GUI stream. The compositor and streamer are in the image and the launch/process APIs are built. The unverified surface is now withdrawn from normal navigation behind the server-owned `gui_stream_available` gate, with a truthful direct-link outcome; that gate additionally requires the operator-set `GUI_STREAM_VERIFIED=1`, and production leaves it false with `START_SWAY=0` and `GUI_STREAM_URL=""`. | No enabled deployment has yet proved launched-process rendering end to end. |
| **FR-E11** | C | Any signal that two live sessions share a working tree. Nothing in `sessions.py` or the engine's registry compares a new session's `cwd` against the live ones. | Two agents can edit one checkout concurrently and neither is told. No audit row records the loss, because both writes are legitimate. |
| **FR-N4** | M | `inbox.list` now returns all four source coverages, a stable snapshot time, per-source high-water marks and an opaque keyset cursor. Collector/storage reads still use bounded `MAX_SCAN` windows rather than a proven end-to-end bounded-read path. | A client-side merge would misorder, duplicate or silently omit attention and could confuse collected-empty with not-collected; the remaining risk is scale beyond the current scan cap. |
| **FR-N5** | M | The registry, occurrence snapshots, material entry identities and audited archive/snooze/restore path now exist. Full changed-material re-observation and non-drift resurface coverage still need a complete integration proof. | Attention decisions could disappear on re-observation or become unaudited client fiction if material identity regresses. |
| **FR-U23** | M | Stable places, the composed Sessions roster/pane/tool workspace, resource-bounded tab retention, recent-place links and the `mydevenv2.tabs.v1` migration now exist. Unit and Chromium tests cover every primary machine-tool route plus terminal/editor lifecycle. Storage-write failure/retry coverage and the complete secondary route matrix remain. | A secondary route could still be stranded by an incomplete migration or storage failure even though the main tool matrix is now asserted. |
| **FR-U24** | M | The Inbox now consumes only `inbox.list`, shows four-source coverage and evidence before action, and supports pointer, keyboard, batch, adopt, suppress and singular drift-resolution reason paths. SSE draft-protection and partial-failure integration coverage remain. | Duplicate drift resolution and unqualified empty states would make the attention surface misleading. |
| **FR-U25** | S | Board cards and Backlog rows now size to their measured content, wrap, and expand in place under one keyed measured window; Board cells read bounded snapshot-stable server pages. A large-estate browser proof over both surfaces remains. | Without the scale proof, a DOM window over a page-sized read is asserted at fixture size only. |
| **FR-T8** | M | The engine PATCH and Assistant reason-update path now render in Sessions, with exact-payload approval tests. Phone/push deep-link and live-stack evidence remain. | A second client path could approve a stale or altered payload, or imply that voice/direct agent writes share the human gate. |
| **FR-M5** | S | The responsive places shell now has labelled primary navigation, Go to reachability, recent places and browser route checks. Current-action push deep links and device evidence remain. | A phone user can land in a surface with no way to reach settings, audit, projects or machine tools. |
| **FR-T9–T13, FR-M6** | M/S | *(r16)* Everything. Named on 2026-08-17; the POC in `VOICE_POC.md` is the first delivery step and nothing in the tree yet meets any of them. | Voice stays a button on a chat tab; a session's model is whatever the template hard-codes; "any notifications?" has no tool to answer it. |

### 7.2 Owed, and blocked on something that is not code

Three acceptance tests are written, are the stage's stated demo, and have never
run. They are not verification debt of the ordinary kind — no amount of reading
the source closes them, which is why §6 counts their conjuncts apart.

| Gap | Stage | What it needs | Status |
|---|---|---|---|
| **The browser demo** — a drag round-trips a `work.transition`, a refused one rolls back with the server's reason, a filtered board URL restores its view after reload | M11 | A person, a browser | Automated Chromium coverage now exercises real desktop drag/drop, refusal composer entry, filter reload, Inbox evidence/actions and phone navigation. Refusal round-trip against a live server, large-estate layout inspection and the full manual route sweep remain. |
| **The restructure conformance demo** — Inbox cursor continuation while new facts arrive, stable places and migration, measured Board/Backlog content, and the full route matrix | M11 | A browser with CSS and a fixture estate | Partial. The browser suite now covers fixture-backed Board, Inbox and phone paths; server high-water continuation while facts arrive, large measured estates and the complete route matrix still need evidence. |
| **The phone demo** — a push arrives, is opened, and the session is unblocked | M13 | A device, the APK, a hand | Outstanding. The APK builds in CI. Blocked in practice by FR-M4. |
| **Real-device FCM delivery** (FR-M2) | M13 | The same device | Outstanding. `google-services.json` carries the client; first-launch registration and end-to-end delivery are unconfirmed. |

Also outstanding and in the same family: live validation on the dev stack of
the split-pane node-identity fix, the SSE visibility/resume reconnect, the
push-on-hang/fail/end watchers, and the auto-retry on rate limiting. All four
are implemented and pass their suites; none has been watched working against a
running instance. The engine's `uplift.md` said so, and said correctly that an
outstanding item there "needs a requirement before it is anybody's plan" —
these are covered by FR-M2, FR-U10 and FR-E2 respectively, so they are
verification gaps rather than new IDs.

### 7.3 Withdrawn — designed once, deliberately not adopted

**These carry no ID on purpose**, with one signposted exception below. They
were designed for MyDevEnv2 as a standalone product, or drafted as next steps
in a document that is now history. No Vogt requirement depends on any of them,
and minting IDs would turn closed decisions back into open work — the exact
failure r2 and r3 spent revisions avoiding. They are listed because an absence
nobody wrote down gets rediscovered as an oversight.

- **A native Anthropic backend for the assistant** — *the exception, because it
  is half of an existing ID.* FR-T7's other clause, refusing rather than
  hanging, is delivered and stays a requirement; the second transport was
  deferred at **r12** and now lives in §3 with its reasoning. It left this
  register's owed list on the same day it entered it, which is what a
  deprioritisation looks like when the register is honest about the difference
  between owed and decided.

- **The Android emulator VM.** MyDevEnv2's Phase 7: a libvirt/KVM domain with
  `/dev/kvm` passthrough, a start/stop API, Selkies inside the VM, an Android
  tab type, and ADB-over-network back to the pod. Nothing was built and nothing
  in the merged product needs it — the Android SDK in the image builds the APK,
  and running an emulator is not something a work tracker does. Reviving it is a
  new requirement, not a resumed one.
- **The GPUI desktop client.** Deprecated 2026-07-07, not carried across by the
  merge, archived in the MyDevEnv2 repository. A reference to `client/` in this
  tree names something that is not here.
- **Natural-language task drafting, a context-update helper, and webhook
  triggers for agent tasks.** Drafted as next steps against a review of another
  project. Interval and daily schedules cover the cases that motivated them.
- **A wake lock while a terminal is focused, and a dedicated diff tab type.**
  Both from the PWA's original plan; the git surface renders diffs through
  Monaco and no wake lock exists.
- **Server-side saved filters, GUI-side offline mode, bulk drift acceptance,
  and voice approval.** Deferred by §3 with their reasons; repeated here only
  so that a reader working through this register does not conclude they were
  missed rather than refused.
- **Backend convergence on Rust.** §3 again: the two-process shape is the
  requirement (NFR-D11), and nothing forecloses a future port.

### 7.4 Residuals — true, smaller than the requirement, and worth naming

Closing a requirement does not always close the thing a reader assumed it was
about. These are the differences, recorded so nobody rediscovers them as
defects:

- **Ranked views page to the end of the *window*, not the estate** (FR-V5).
  `RANKING_CANDIDATE_LIMIT` is 1,000: the view reads a wide slice and scores it
  in the application layer, so an estate with more than a thousand open items
  has some that no offset reaches. The cap is deliberate and predates the
  requirement — scoring is a documented function of several inputs rather than
  something SQL should express — and `total_considered` reports what was
  *considered*, so a caller cannot currently tell the window from the estate.
  Raising it, or pushing scoring into the query, is a storage-layer change with
  its own benchmark; neither is owed by any requirement today.
- **The contract digest changes a recorded version, not a recorded status**
  (FR-G1). An instance that edits its contract rules starts stamping
  `v1+<digest>` on new checks; statuses recorded before the edit keep saying
  `v1`, which is correct — they were evaluated against `v1` — but means a
  project's stored `contract_version` may name a contract the configuration no
  longer describes until it is re-checked. That is the same freshness rule
  every other value here follows, and the compliance view already pairs the
  status with its age.

### 7.5 What this register is not

It is not a backlog, and nothing in it is scheduled. `ROADMAP.md` holds the
stages; this document holds what is owed. The distinction matters because a gap
register that starts carrying dates becomes a plan nobody agreed to, and then
stops being maintained — which is how the engine ended up with a document
called `uplift.md` that was simultaneously its history, its backlog and its
deployment guide.

The rule for adding a row: **something was designed, it is not built, and a
reader of the design documents could reasonably have believed it was.** A thing
nobody ever wrote down is not a gap here; it is an idea, and ideas belong in
Vogt, which is the product this repository builds.
