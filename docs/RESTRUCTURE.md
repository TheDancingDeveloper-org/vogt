# The places restructure — implementation plan

**What this document is.** The implementation plan for turning the design
export in [`design/restructure-2026-08/`](../design/restructure-2026-08/) into
the shipped PWA and phone app. It resolves the design review completed on
2026-08-17 and orders the work so a surface is never drawn ahead of the
contract that makes it true.

**What it is not.** A requirement. `design/README.md` says a wireframe is not
a requirement, and the same is true of a plan derived from one. Stage 0 amends
the requirement set before product code starts. Until then this file describes
a proposed implementation, not an owed capability.

**Authority.** `docs/DESIGN.md` describes what exists, `docs/REQUIREMENTS.md`
states what is owed and withdrawn, and the operation registry defines every
Python capability. This plan must be corrected when any of those disagree with
it. Unbuilt decisions stay here and in the requirements gap register; they do
not enter `DESIGN.md` as though they already exist.

---

## Review disposition

The review found seven defects in the first plan. All seven are design inputs
to this revision, not follow-up notes.

| Finding | Resolution in this plan |
|---|---|
| 1. The proposed approval subsystem competed with the engine's delivered pending-action gate and pointed at the unrelated public-identity module. | Stage 2 extends and reuses `AssistantRuntime`. There is no core approval ledger, no `approval.*` registry family, and no change to `application/identity.py`. |
| 2. A client-side merge of three incompatible reads could not provide a correct ordered, paginated Inbox and omitted CI and engine attention. | Stage 1 adds one registry-backed `inbox.list` operation with one normalized row, server-owned ordering, keyset pagination, coverage and deduplication. The browser consumes only that operation. |
| 3. Removing the top-level tab strip stranded work-item detail, Git, GUI streaming, history, tasks, assistant, editors and terminals, especially on a phone with no drawer. | Stage 3 contains a complete route and reachability matrix. Existing deep links remain valid; secondary machine tools live inside Sessions; every phone screen exposes a text-labelled “Go to…” control. |
| 4. Instant archive/snooze and expire-on-read conflicted with the typed-reason and read-only-query rules. | Stage 1 separates local “seen” state from shared declared triage. Archive, snooze and restore are audited writes with typed reasons. Reads merely interpret elapsed snoozes and never mutate. |
| 5. The Backlog drawing showed only bulk move and the plan silently dropped delivered bulk label. | Stage 7 retains bulk transition **and** bulk add/remove label, with separate reason fields and the existing partial-batch behaviour. |
| 6. The first plan said NFR-S5 mandated fixed heights. It does not. | NFR-S5 remains unchanged. Stage 5 adds a separate content-sizing requirement and proves measured virtualization plus bounded server reads without treating an estimate as a design height. |
| 7. The plan treated a CI collector as an open scope decision even though `GitHubActionsCollector` already emits `ci.check` through `gh-actions`. | No CI collector is added. Stage 1 consumes the existing observations, reports `gh-actions` coverage, and deduplicates checks to the newest observed revision. |

---

## Decisions closed by this revision

These were the former open decisions. Stage 0 records them in the requirement
history; implementation does not reopen them.

1. **Archive is occurrence-scoped, shared and audited.** An Inbox entry key
   contains a stable source subject and a material source version. Archiving
   hides that occurrence for the whole instance. A byte-for-byte
   re-observation does not resurface it; a materially changed notification,
   CI result or agent state gets a new version and can surface again.
2. **Approval uses the existing assistant gate.** Assistant-proposed terminal
   input and Vogt writes keep the delivered one-at-a-time, exact-payload,
   120-second gate. A Vogt write executes with the approving caller's paired
   core credential. A session agent using its own actor-scoped MCP token is a
   different path: its authorized writes remain immediate and audited to that
   session actor. This uplift does not turn every PTY agent call into an
   asynchronous proposal.
3. **Old tab state is migrated, not discarded.** Machine tabs become Sessions
   panes or tools. Surface and work-item tabs become recent-place entries, and
   the formerly active one becomes the current route. Migration is idempotent
   and keeps the old value until the new state has been written successfully.
4. **CI collection already exists.** Inbox consumes `gh-actions`. Absence is
   expressed through coverage; it is never inferred from an empty result.
5. **Projects keeps drift context, not a second action surface.** A project
   page retains its drift count, age and summary and links to
   `/inbox?source=drift&project=<slug>`. Evidence, accept/reject controls and
   triage live only in the canonical Inbox.
6. **Every existing capability moves into the new shell in this pass.** The
   visual uplift covers Board, Backlog, Inbox, Sessions and phone layouts.
   Projects, Audit, work-item detail, Settings, History, Tasks, Assistant, Git,
   GUI streaming and the editor keep their current internal presentation while
   being rehoused and made reachable. Their later visual redesign is a
   separate pass.

---

## Contracts that every stage preserves

### One approval boundary

The sole model-output approval boundary is
`POST /api/assistant/actions/:id` in the Rust engine. It already owns the exact
payload, expiry, one-pending-action invariant, denial, voice prohibition and
approver credential. The Sessions and phone designs are additional views of
that state, not new stores of it.

The UI must distinguish three acts rather than using “agent approval” for all
of them:

| Act | Actor and gate |
|---|---|
| Assistant proposes terminal input | Pending action; on-screen approval sends the exact bytes. |
| Assistant proposes a Vogt write | Pending action; on-screen approval sends the exact registry call using the approver's paired core token. |
| A session agent calls Vogt through its session MCP token | Direct authorized call; audited to `agent:session:<id>` with the operation's required reason. No human approval is implied. |

A pending action is ephemeral: it is held in memory, one exists at a time, a
new message supersedes it, and it expires after 120 seconds. Push is a hint
that opens the authoritative current action. This plan does not invent a
durable approval queue or show expired cards as actionable history.

### One Inbox contract

“Inbox” means one attention-oriented read, not a renaming of every event in
the system. `events.list` remains the instance's ordered history and Audit
remains the place to inspect it. Collected GitHub notifications remain
instance-scoped and are never marked read upstream.

The unified Inbox contains only normalized rows from these sources:

| Source | Backing fact | Inclusion and deduplication |
|---|---|---|
| `github` | Latest `forge.notification` observations | One row per material thread version within registered projects. |
| `drift` | Drift proposals | Open proposals, with both evidence sides available before resolution. |
| `ci` | Existing `ci.check` observations from `gh-actions` | Failed checks on each project's newest observed revision; old-revision failures do not keep the project red. |
| `agent` | Live session attention plus collected bound-task findings | Current `waiting-for-input` / `errored` activity and material `agent_task.run` findings; routine running/idle state is not an interruption. |

Entry keys are not ad hoc display ids. Their material versions are defined per
source:

| Row | Entry-key material version |
|---|---|
| GitHub notification | thread plus a digest of source update, title, reason, subject type and URL; upstream `unread` and collection time are excluded |
| Drift | proposal id plus a digest of its evidence snapshot and proposed change |
| CI | project, revision and check name plus source update, status and conclusion |
| Live session attention | engine session id, activity state and `activity_changed_at` |
| Bound-task finding | task-run id plus a digest of its material findings and outcome |

This is what “materially changed” means throughout the plan. A collector
looking again, a local seen dot changing or GitHub's unread flag changing does
not mint a new occurrence by itself.

The server, not Solid, owns normalization, ordering, source watermarks,
coverage and pagination. The global order is descending
`(occurred_at, source, entry_key)`. A first request fixes observed and declared
high-water marks plus the last sort key; an opaque cursor carries them. Each
source reader is bounded to `limit + 1` and keyset-filtered below that key. No
implementation may fetch all three stores into the browser and sort them
there.

The high-water marks prevent later arrivals or material versions from jumping
into page two. They are not a promise that attention remains actionable:
resolved drift, archived rows and recovered sessions may disappear between
pages. The response identifies current-condition sources and recomputes their
counts under the same upper watermark rather than claiming a repeatable
historical snapshot.

Live engine activity is allowed to disappear from a later page when the
session no longer needs attention. That is a current condition, not an
immutable historical entry. The cursor still prevents newly arrived or
reordered rows from appearing ahead of the page boundary; the response marks
the engine source as live so a caller does not mistake it for a frozen event
log.

### Two kinds of Inbox state

“Seen” is presentation state. It is stored per client by entry key, has no
cross-device promise, changes no shared fact and needs no reason.

Archive, snooze and restore are shared instance decisions in the declared
store. They name the entry key, actor, reason and time and therefore use the
audited write path. Snooze also carries an absolute `until`. The triage row
keeps a bounded normalized snapshot of the occurrence shown when the decision
was made, so archived/snoozed filters do not depend on a live engine condition
or an observed projection still existing. A read treats a past snooze as
active again without updating a row. Maintenance may compact a projection
later, but reading the Inbox never writes.

“Archive all read” means the read entries on the currently loaded filtered
page, not every row in the estate. The UI collects one batch reason, then calls
the singular archive operation once per entry with that same reason. Each
entry gets its own audit and event row; successes clear from selection,
refusals remain and are reported individually.

### Places and panes

A place is a stable product surface and cannot be closed. A pane is a terminal,
editor or machine tool inside Sessions and may be opened, split or closed.
Removing the product-level tab strip does not remove multi-pane work.

The desktop rail is text-first. Symbols may remain in terminal output, status
badges and content where they carry domain meaning; pictographic or emoji-only
navigation does not.

---

## Delivery sequence

| Stage | Delivers | Depends on |
|---|---|---|
| 0 | Requirements and acceptance contracts | — |
| 1 | Normalized Inbox read and audited triage | 0 |
| 2 | Existing approval gate exposed in Sessions and push | 0 |
| 3 | Places shell, routes and state migration | 0 |
| 4 | Inbox surface | 1, 3 |
| 5 | Measured-windowing and bounded-read prototype | 0 |
| 6 | Board uplift | 3, 5 |
| 7 | Backlog uplift | 3, 5 |
| 8 | Sessions workspace | 2, 3 |
| 9 | Phone layout and deep-linked approval | 4, 6, 7, 8; live dev stack for final validation |
| 10 | Conformance, live demonstrations and documentation close-out | all prior stages |

Stages 1, 2, 3 and 5 are independently reviewable after Stage 0. A release
does not ship the shell without the route matrix, or a new Inbox without its
server operation.

---

## Stage 0 — Define and pin the acceptance boundary

**No product code.** This stage converts the chosen uplift into traceable
requirements and removes stale statements from the plan's source material.

- **Drawn in** — `Vogt Design Guardrails.dc.html` and all four surface files
- **Repo** — `docs/REQUIREMENTS.md` and `docs/ROADMAP.md`
- **Existing requirements preserved** — FR-N3, FR-U3, FR-U6, FR-U7,
  FR-U10–U22, FR-T2, FR-T3, FR-M1–M4 and NFR-S5

### Requirement changes

Use the next free IDs when Stage 0 is implemented; the intended clauses are
named here so numbering is mechanical rather than improvised:

- **FR-N4 — normalized attention Inbox.** One registry operation provides the
  four sources, server ordering, opaque keyset pagination, source coverage and
  stable entry identities. It does not merge the `events.list` history or
  mutate GitHub read state.
- **FR-N5 — Inbox triage.** Shared archive, snooze and restore are audited
  writes with typed reasons; material source changes may resurface a subject;
  local seen state is explicitly outside the shared contract.
- **FR-U23 — places and panes.** Stable places replace product tabs without
  losing any route or capability. Machine tabs become Sessions panes/tools and
  old client state is migrated.
- **FR-U24 — Inbox interaction.** The surface consumes FR-N4 only, shows
  coverage and provenance, keeps drift evidence ahead of action, and collects
  reasons for every write and batch.
- **FR-U25 — content-sized virtual surfaces.** Board cards and Backlog rows
  size to content and expand in place while retaining NFR-S5's virtualization
  and bounded-server-read guarantees.
- **FR-T8 — approval presentation.** Sessions, Assistant and phone render the
  one existing pending action consistently; an approver may replace only a
  Vogt write's reason before approving, the engine returns the resulting exact
  payload for review, and only a subsequent approval may send it to the core.
- **FR-M5 — sessions-first phone navigation.** The four primary places use the
  bottom bar; every secondary route remains reachable through a labelled
  “Go to…” control or a contextual link; push opens the current pending action.

Revise, with history rather than renumbering:

- **FR-U7** keeps project-scoped drift status and links its action to the
  canonical Inbox instead of requiring a duplicate resolver.
- **FR-M2** adds a pending assistant action to the explicitly allowed default
  push kinds. The notification carries no raw payload or reason on the lock
  screen.
- **FR-U11** names `/inbox`, `/sessions` and `/settings` and preserves every
  existing deep link listed in Stage 3.
- **NFR-S5 is not revised.** Add FR-U25 beside it. NFR-S5 requires
  virtualization, bounded reads and interaction at scale; it never required a
  fixed CSS height.

Record the work as owed in the gap register and add the delivery stages to
M11/M13 in `ROADMAP.md`. Do **not** add the places shell to `DESIGN.md` yet:
that document changes only when a stage is delivered.

### Acceptance inventory

Stage 0 adds a traceability table mapping each new or revised clause to:

- its server, engine and Solid tests;
- the manual browser demonstration, where CSS, drag/drop or focus is involved;
- the device demonstration, where push, WebView or native clipboard is
  involved;
- the dev-stack prerequisite for cross-process behaviour.

**Done when** `uv run python scripts/check_docs.py` and the full
`uv run pytest` suite (including `tests/test_requirements_audit.py`) pass,
every later stage names an existing requirement, and no requirement claims
fixed heights or a missing CI collector.

---

## Stage 1 — Core and engine: one Inbox operation

The UI cannot begin until one page can be requested, ordered and continued
without a client merge.

- **Requirements** — FR-N4, FR-N5, FR-U2, FR-U10, NFR-S5
- **Repo** — `src/vogt/application/models.py`,
  `src/vogt/application/services/inbox.py`, `src/vogt/storage/`,
  `src/vogt/registry/operations.py`, `engine/contract/` and the engine client
- **Drawn in** — `InboxSurface.dc.html`

### The normalized row

`InboxEntry` carries at least:

- `entry_key` — source subject plus material version;
- `source` and `kind`;
- `occurred_at` and `observed_at` where both exist;
- title and a bounded summary;
- project slug, work-item ref and session id when known;
- source subject key and source URL when known;
- trust and freshness/provisional state;
- current shared triage state and snooze expiry;
- typed action targets such as drift id or observed subject key, rather than
  client-parsed strings.

The source-specific payload remains available only where an action needs it.
The normalized row does not flatten away drift's two evidence sides or turn an
engine's live activity into an observation.

### Source adapters

- GitHub uses the same filtering and instance-scope statement as
  `notifications`. Its material version uses exactly the fields in the table
  above; upstream unread state and collector timestamps cannot make an
  unchanged sweep a new occurrence.
- Drift reads proposals in keyset order. Resolved proposals fall out of the
  active stream; Audit and the project summary retain their history.
- CI uses `roll_up` from `core/checks.py` per project and emits only failing
  checks from the newest observed revision. It reports `gh-actions` as
  unconfigured, unswept, partial, failed or current using the existing
  coverage record.
- Agent task findings use collected `agent_task.run` observations. Live
  session rows come from the configured engine and join through Vogt's
  declared session link so the row can name the project, work item and session
  actor honestly.

The engine contract gains an `activity_changed_at` wall-clock timestamp on a
session summary. It is set when the activity state changes and is the
`occurred_at` for live attention. `EngineSession` mirrors only that additional
field. No activity value is copied into the declared session row.

### Pagination and coverage

Add `InboxListParams` with:

- `sources`, shared `triage_states`, `project` and optional `work_item`
  filters;
- `limit`, capped at 100;
- one opaque `cursor`.

Add `InboxListResult` with:

- `entries`;
- `next_cursor` or null;
- `snapshot_at`;
- per-source `coverage` and counts limited to the same snapshot;
- the GitHub instance-scope statement;
- engine availability as a distinct live-source status.

The first request fixes `snapshot_at`. Subsequent requests decode and validate
the source high-water marks, retain the original filters and use the last
total sort key. `snapshot_at` is the response label for those watermarks, not
a promise that live session attention cannot recover between pages.
Changing filters starts a new snapshot. Invalid or mismatched cursors receive
a named request error; the server never silently restarts at page one.

Existing `notifications`, `drift.list` and `events.list` remain compatible for
CLI users and their existing surfaces. `inbox.list` is a new projection over
their facts, not a replacement for those domain reads.

### Declared triage

Add `0008_inbox_triage.sql` and an `InboxTriage` entity. The current projection
is keyed by `entry_key` and records state, snooze expiry, actor, latest
decision time and the bounded normalized occurrence snapshot; audit and events
retain the history.

Registry operations:

- `inbox.list` — read;
- `inbox.archive` — one entry key plus reason;
- `inbox.snooze` — one entry key, absolute until plus reason;
- `inbox.restore` — one entry key plus reason.

All three writes use `audited_write` and have CLI, REST and MCP parity from the
registry. They reject an unknown entry key, blank reason, past snooze time and
an inapplicable state with the core's own named error. Snooze expiry is a
query predicate, not a write performed by `inbox.list`.

### Tests

- source normalization and deterministic tie ordering;
- page boundaries with equal timestamps, arrivals after page one and changed
  filters;
- no duplicate CI failure from an older revision;
- GitHub and CI “not collected” versus collected-empty;
- engine absent versus configured-but-unreachable;
- unchanged re-observation stays archived, changed material resurfaces;
- elapsed snooze becomes visible with no revision, audit or event written by
  the read;
- each write refuses without a reason and lands entity, audit and event in one
  transaction;
- registry parity, migration upgrade fixtures and transactional migration-
  failure coverage.

**Done when** two pages can be read while new source facts arrive without a
duplicate or a client-side merge; CI and engine attention are real sources;
and all triage writes are explainable through Audit.

---

## Stage 2 — Engine: reuse the pending-action gate

This stage changes presentation and delivery around the existing guarantee. It
does not create a second approval authority.

- **Requirements** — FR-T2, FR-T3, FR-T8, revised FR-M2
- **Repo** — `engine/server/src/assistant.rs`,
  `assistant_api.rs`, `push.rs`, `push_api.rs`, `engine/contract/`,
  `web/src/api.ts` and `web/src/Assistant.tsx`
- **Drawn in** — the approval bar in `SessionsSurface.dc.html` and the phone
  approval sheet

### Wire extension

Keep `PendingActionView` and keep
`POST /api/assistant/actions/:id {"approve": bool}` as the only decision and
delivery route. Add
`PATCH /api/assistant/actions/:id {"reason": string}` as a preview/update step:

- it is rejected for `send_input`;
- it is accepted only while the named `vogt_write` is current and unexpired;
- it replaces only the `reason` member in the held registry arguments;
- the server regenerates the pretty payload from those held arguments;
- a blank or contentless reason is refused by the same validation that
  created the original card;
- it returns the updated `PendingActionView`, performs no effector call and
  does not extend the 120-second expiry.

The UI first submits the edited reason, then renders the exact card the engine
returned. A separate Approve click sends that already-reviewed held payload
through the unchanged POST route. “Edit reason” can therefore neither deliver
a write nor mutate a hidden field during delivery. An unchanged approval keeps
the assistant-proposed reason. Deny remains a non-write and does not invent an
audit reason.

### Push

When a pending action is created, the engine may send the new
`assistant_approval` push kind through the existing push service. The lock
screen body says only that terminal input or a Vogt change is waiting; it does
not expose terminal bytes, target payload or audit reason. Its data URL is
`/sessions?approval=<id>`.

Web Push, foreground service-worker navigation and Capacitor's native
`pushNotificationActionPerformed` already understand a URL. Opening it mounts
Sessions, reads `assistant/history` and selects the action only if its id still
matches. Expired or superseded ids show “This request is no longer pending”
and no approve button.

The push preference participates in quiet hours and digesting like existing
kinds. Voice may announce that approval is needed but still has no resolve
path.

### Explicit non-goals

- no `approvals` SQL table;
- no `approval.request/list/decide` registry operations;
- no change to `application/identity.py`;
- no interception of arbitrary session MCP calls;
- no queue, restart recovery or approval history;
- no payload in a push notification.

### Tests

Retain all current gate tests and add:

- reason replacement changes only the held reason, returns the regenerated
  exact card, and makes no core call until the later approval;
- reason replacement cannot target terminal input or omit a reason;
- a different authenticated caller may approve and that caller's paired token
  reaches the core;
- a stale push id cannot approve the current card;
- a pending action produces the allowed push kind with a deep link and no
  sensitive fields;
- quiet hours, digest and disabled preference apply;
- voice and a new assistant message still cannot approve.

**Done when** Assistant, Sessions and phone all resolve the same in-memory
pending action, and deleting the existing gate would fail the test rather than
leave a second path working.

---

## Stage 3 — Web: places shell and complete reachability

This is the highest front-end blast radius. It lands only with the migration
and every route below.

- **Requirements** — FR-U11, FR-U16, FR-U20, FR-U21, FR-U23, FR-M5
- **Repo** — `web/src/App.tsx`, `routes.ts`, `tabs.ts`, `layout.ts`,
  `workspaceLayouts.ts`, `CommandPalette.tsx` and `styles.css`
- **Drawn in** — `Vogt.dc.html` and direction 1a in
  `Vogt Restructure Wireframes.dc.html`

### Route and home matrix

Existing paths remain valid. “Home” describes composition, not a redirect that
breaks a saved URL.

| Route | Place/pane after uplift | Desktop reach | Phone reach |
|---|---|---|---|
| `/` | Board on desktop; Sessions on narrow/coarse clients, replaced into the explicit route | launch | launch |
| `/board` | Board place | Work rail | bottom bar |
| `/backlog` | Backlog place | Work rail | bottom bar |
| `/inbox` | Inbox place | Work rail | bottom bar |
| `/projects` | Projects place, current overview/deps/import content retained | Estate rail | “Go to…”, project links and deep link |
| `/audit` | Audit place | Estate rail | “Go to…”, work-item audit links and deep link |
| `/settings` | Settings place | rail footer | “Go to…” and Sessions header |
| `/w/:ref` | Work-item detail place | Board/Backlog/Inbox/Projects links and command palette | contextual links, “Go to…” and deep link |
| `/sessions` | Sessions list/workspace | Machine rail | bottom bar |
| `/t/:id` | Sessions with terminal pane active | running list, command palette, deep link | Sessions list, contextual links and deep link |
| `/e/*path` | Sessions with editor pane active | file tree, command palette, deep link | “Go to…”/file search and deep link |
| `/g`, `/g/*path` | Sessions Git tool | Sessions tool switcher and command palette | “Go to…” and deep link |
| `/gui` | Sessions GUI-stream tool | Sessions tool switcher and command palette | “Go to…” and deep link |
| `/history` | Sessions History tool | Sessions tool switcher and command palette | “Go to…” and deep link |
| `/tasks` | Sessions Tasks tool | Sessions tool switcher and command palette | “Go to…” and deep link |
| `/assistant`, `/assistant/*path` | Sessions Assistant tool | Sessions tool switcher and command palette; hidden when unconfigured | “Go to…”, Sessions header and deep link; hidden when unconfigured |

Every primary phone header includes a visible text-labelled “Go to…” button
that opens the existing command palette in navigation mode. It is not a
hamburger and contains no direct mutating command. FR-U16 continues to require
that a write entry opens the surface that collects its reason.

### Shell composition

- Add `Rail.tsx`: 248px, text labels, Work / Estate / Machine groups, running
  sessions, file search/tree, connection state and Settings.
- Add stable place rendering independent of the pane store. Board, Backlog,
  Inbox, Projects, Audit, Settings and WorkItemDetail cannot be closed.
- Add a Sessions pane store for terminal/editor splits and its tool selection.
  `tabbed`/`ide` becomes an internal Sessions pane-layout preference rather
  than a product shell mode.
- Remove the drawer, scrim, product tab strip and mobile tab-count sheet only
  after all matrix rows work.
- Delete `TAB_GLYPHS` and emoji navigation labels. Status glyphs with an
  accessible text name remain permitted.
- Keep the command palette global and route-driven.
- Make place filters and selected panes URL-addressable without dropping
  existing Board, Backlog, Projects or Audit queries.

### State migration

Read `mydevenv2.tabs.v1` once and write a versioned places/workspace state:

- terminal and editor tabs become Sessions panes;
- Git, GUI, History, Tasks and Assistant become the selected or recent
  Sessions tool;
- Board, Backlog, Projects and Audit become recent place paths;
- work-item tabs become recent `/w/:ref` paths;
- the old active tab chooses the initial route/pane;
- saved workspace layouts retain only their machine panes/tools and reinterpret
  their layout mode inside Sessions.

Write the new state first, set a migration marker second, and leave the old key
untouched for one release. Re-running migration is a no-op. Malformed entries
are skipped with a one-time, named toast that says exactly what could not be
restored; valid neighbours still migrate.

### Visual scope

Projects, Audit, WorkItemDetail, Settings, History, Tasks, Assistant, Git, GUI
and Editor keep their current inner markup in this stage. They receive only
the containment, responsive and navigation adjustments necessary to function
inside their new home. This prevents a shell change from becoming an
unbounded redesign while still refusing to strand them.

### Tests

Rewrite `shell.test.tsx` around places and add a table-driven route test with
one case for every row above. Cover pasted URLs, reload, back/forward,
unconfigured Assistant and both outage halves. Add migration tests for every
old tab kind, multiple work items, saved layouts, malformed state, idempotence
and active-route choice. Command-palette tests assert reachability and absence
of writes.

**Done when** every old URL opens equivalent functionality, every current tab
kind has a migration destination, all primary and secondary phone routes are
discoverable without a drawer, and no product place can be closed.

---

## Stage 4 — Web: Inbox surface

The surface consumes one operation. It does not know how GitHub, drift, CI and
agent rows are fetched or globally ordered.

- **Requirements** — FR-N3–N5, FR-U2, FR-U10, FR-U18, FR-U24
- **Repo** — new `web/src/Inbox.tsx`, `vogtApi.ts`,
  `Projects.tsx`, `AuditBrowser.tsx` and shared coverage/state helpers
- **Drawn in** — `InboxSurface.dc.html` and the phone Inbox state

### Read path

- Add `/inbox` with URL filters for source, shared triage state, project and
  work item.
- Call `inbox.list` with the server cursor; “load more” appends that response
  without resorting the client array.
- Each row shows source, title, provenance, trust/provisional status,
  `occurred_at` age and observation/coverage age where distinct.
- Counts and empty copy come from the response's source coverage. An
  unconfigured or unswept source says “Not collected”; a covered empty source
  says “Nothing needs attention.”
- SSE `vogt-changed` and engine activity trigger a first-page reconciliation.
  A half-typed reason suspends replacement of that row. Reconnect performs a
  new first-page read and the surface reports its view age.
- `AuditBrowser` loses its `?view=inbox` branch but keeps Audit.
- Projects retains project drift count/age/summary and routes its call to
  `/inbox?source=drift&project=<slug>`. It contains no second resolver.

“Seen” is written to a versioned local-storage set when a row is opened or
explicitly marked seen. It affects only the local unread dot and the
“Archive all read” selection. It never calls the server or changes GitHub's
unread flag; upstream unread remains visible as provenance.

### Actions

Actions are derived from typed fields on `InboxEntry`:

- archive, snooze and restore call Stage 1 operations and always open inline
  reason capture;
- convert an observed GitHub, CI or agent finding uses `work.adopt` so the
  source link and provenance survive; it does not fabricate an unrelated
  `work.create`;
- suppress uses the existing singular `suppress` operation with the source
  subject and a typed reason;
- drift accept/reject uses `drift.resolve` only after both evidence sides are
  visible and a reason is typed; bulk drift resolution remains absent;
- open item and open session are navigation;
- live agent rows that have no adoptable observation offer navigation, not a
  fake conversion.

Keyboard `j`/`k` moves focus. `e`, `s` and any other mutating key open the same
inline reason control as a pointer; they never execute immediately. `c` is
offered only for an adoptable observation and `o` follows the row's typed
target. Hints are hidden on coarse pointers without removing the controls.

“Archive all read” shows the exact number of loaded, visible, locally seen
entries, collects one reason, invokes the singular operation per key, and
renders partial success in the same shape as Backlog bulk operations.

### Tests

Add `inbox.test.tsx` for:

- no calls to `notifications`, `drift.list` or `events.list` from this surface;
- cursor continuation without browser sorting;
- covered-empty versus every not-collected state;
- local seen state making no HTTP request;
- typed reasons on pointer, keyboard and batch paths;
- partial batch failures retaining only failed entries;
- changed occurrence resurfacing;
- drift evidence ordering and no bulk accept;
- adoption preserving the source subject;
- project links opening the canonical filtered Inbox;
- SSE reconciliation and draft protection.

**Done when** every rendered row came from `inbox.list`, every absence is
qualified by coverage, and no keyboard or bulk path can write without a typed
reason.

---

## Stage 5 — Shared measured windowing and bounded reads

This is an explicit prototype gate before either high-volume surface is
rewritten.

- **Requirements** — NFR-S5 and FR-U25
- **Repo** — new `web/src/measuredWindow.ts` and tests; Board/Backlog data
  access; list models/storage where server filters are missing
- **Drawn in** — the content-sizing clauses in the guardrails

### Measured-window primitive

Implement one keyed primitive shared by Board and Backlog:

- caller supplies stable item keys and a conservative estimated height;
- only rendered items receive a `ResizeObserver`;
- a Fenwick tree or equivalent indexed prefix structure stores measured or
  estimated sizes and finds the first visible offset in logarithmic time;
- overscan is expressed in pixels, not a fixed number of rows;
- measurements above the viewport adjust scroll anchoring so content does not
  jump;
- width, font and expansion changes invalidate affected measurements;
- removed/reordered keys do not donate measurements to another item;
- a layout-free test environment uses the estimate but production never
  treats it as a CSS height contract.

The estimate is an implementation seed, not a design size. No
`--board-card-h`, `ROW_HEIGHT` or equality test between TypeScript and CSS
remains.

### Bounded data access

Measured DOM windowing does not satisfy NFR-S5 if the client still downloads
the whole estate.

- Add `board.list` as one registry-backed batched projection over `work.list`'s
  filters. Its request carries the complete multi-valued Board filters, lane
  mode, the visible workflow states, a bounded per-cell page size and opaque
  per-cell cursors. Its response carries each requested cell's rows,
  `next_cursor` and exact total plus column totals. The storage query groups
  counts and pages the requested cells in one operation, so twenty lanes do
  not become twenty HTTP calls or twenty full scans. As a registry operation,
  it has generated CLI, REST and MCP surfaces and a parity-script case.
- `board.list` uses the same stable `(created_at, ref)` order as `work.list` and
  keeps a snapshot/cursor contract while the user scrolls. Preserve
  `work.list` for its current callers rather than widening its singular
  parameters until they mean two things.
- Board makes one bounded batch for the visible cells and asks for the next
  cursor only when a cell's window approaches the end. It does not loop to
  `MAX_ITEMS` at mount and does not issue one request per cell.
- Lanes are partitions. The server supplies column and cell totals, so WIP
  counts never depend on which pages the client happened to fetch.
- Backlog retains its ranked server pagination and fetches the next page only
  on an explicit continuation or near-end window. It never recomputes ranking
  in the browser.
- Filters reflected in the URL are sent to the server; an unsupported filter
  blocks the read with a named explanation instead of filtering one page and
  presenting its total as estate-wide.

### Prototype and acceptance

Build a fixture with thousands of varied-height rows/cards, long titles,
expanded content and width changes. Instrument rendered-node count, request
count, measurement updates and scroll position.

The gate passes when:

- DOM nodes stay bounded by the viewport and overscan;
- initial network rows are bounded independently of estate size;
- initial request count is bounded independently of the number of lanes;
- a measured height change above the viewport preserves the anchored item;
- jumping near the end loads the correct page with no skipped or duplicated
  key;
- filtering, optimistic moves and reason typing do not rebuild every cell;
- browser find/select trade-offs remain documented for virtualized content.

Unit tests cover the data structure; a real-browser test covers actual
`ResizeObserver` and scrolling. If this prototype fails, Stages 6 and 7 stop;
they do not fall back to clipping content or fetching the estate.

The server tests also cover `board.list` filter combinations, per-cell cursor
continuation, snapshot stability, exact totals, query bounds and transport
parity.

---

## Stage 6 — Web: Board

- **Requirements** — FR-U4, FR-U10–U15, FR-U17, FR-U22, FR-U25, NFR-S5
- **Repo** — `web/src/Board.tsx` and `styles.css`
- **Drawn in** — `BoardSurface.dc.html`

### Work

- Replace fixed-slot windowing with Stage 5's keyed measured window.
- Remove `CARD_HEIGHT`, `CARD_GAP`, `CARD_SLOT` and their CSS height pair.
- Use the batched `board.list` cell pagination from Stage 5 instead of the
  current estate-loading loop and cap. Every item remains reachable by
  scrolling its cell.
- Clamp collapsed title/body content at three lines and expand in place.
  Expansion remeasures the card; it is not a modal.
- Preserve server-owned workflow columns, optimistic drag rollback, keyboard
  move, swimlanes, WIP totals, collapse preferences and URL filters.
- Header order becomes title, evidence freshness/honesty, flexible space, view
  controls and primary action.
- Filters become chips, “+ filter” and named saved lenses without losing any
  existing project, kind, state, label, initiative or actor dimension.
- Keep quick-create and move reason capture inline. A refusal prints the
  server sentence verbatim in monospace at the attempted move.
- Keep trust on every card and distinguish declared, observed and unverified.

### Tests

Rewrite `boardScale.test.tsx` around bounded pages, measurements and rendered
nodes rather than a fixed-height equality. Keep all drag, rollback, keyboard,
filter, saved-lens, WIP and outage assertions. Add long-title, expanded-card,
measurement invalidation and page-boundary cases.

The manual browser demonstration covers real drag/drop, resize, filter URL
reload and rollback with CSS loaded.

**Done when** long content changes card height without clipping, interaction
and reads remain bounded at estate scale, and every existing Board contract
still passes.

---

## Stage 7 — Web: Backlog

- **Requirements** — FR-U6, FR-U10, FR-U14, FR-U15, FR-U17, FR-U25, NFR-S5
- **Repo** — `web/src/Backlog.tsx`, `vogtApi.ts` and a read-only ranking-rules
  registry operation if the current response cannot supply the drawn panel
- **Drawn in** — `BacklogSurface.dc.html`

### Work

- Remove `ROW_HEIGHT` and use Stage 5's keyed measured window over server
  pages.
- Let title, provenance and why factors wrap; expand actions in place.
- Keep rank, ref, score, trust and age visible in the collapsed row.
- Preserve quick-create, state filters, named filters, absent states and
  server-ranked order.
- Preserve **both** delivered bulk families:
  - transition to a selected state with its own typed reason;
  - add or remove a label with a different typed reason.
- Each bulk family keeps one registry write per selected item, clears only its
  own reason after completion, leaves refused items selected and reports each
  server refusal. A transition reason can never justify a label write.
- Declared rows offer Open, Open session and Select. Observed rows offer Adopt
  and Suppress with reason. Controls that do not apply are absent rather than
  disabled without explanation.
- The coverage footer names collectors that did not run and the age of those
  that did.
- The weights panel is read-only and says so. If `why` cannot expose the
  global rule set honestly, add `ranking.rules` as a registry-backed read over
  `core/ranking.py`; do not duplicate weights in TypeScript and do not add a
  setter.

### Tests

Retain every existing bulk-label and bulk-transition test, including separate
reason state and partial batches. Add varied row heights, expansion,
continuation, scroll anchoring, read-only rules and coverage footer cases.
Mutation tests should fail if either bulk family disappears.

**Done when** a two-line title and why factors do not clip, ranking remains
server-owned and paginated, and both bulk operations retain their independent
typed-reason guarantees.

---

## Stage 8 — Web: Sessions workspace

- **Requirements** — FR-E1–E5, FR-U10, FR-U20, FR-U21, FR-U23, FR-T2,
  FR-T3, FR-T8
- **Repo** — new `web/src/Sessions.tsx` plus the existing terminal, editor,
  Git, GUI, History, Tasks, Assistant, split-pane and clipboard components
- **Drawn in** — `SessionsSurface.dc.html`

### Work

- Build `/sessions` around a session list, an internal pane bar and the
  existing terminal/editor/tool components.
- Sort the list by attention: pending action target, waiting for input,
  errored, running, then idle/recent. The sort uses live engine state and says
  when that source is unavailable.
- Keep terminal attach, scrollback, split, rename, kill and create behaviour.
- Keep editors, Git, GUI streaming, History, Tasks and Assistant reachable
  through the internal tool switcher and their preserved deep links.
- Show a work-item pill on a linked terminal and keep both FR-U20 navigation
  legs.
- Render Stage 2's pending action once. Terminal input names the target
  session; a Vogt write names operation, target, exact payload, proposed
  reason and that approval uses the current user's paired credential.
  Never label the session actor as the proposer unless the response actually
  carries that fact.
- Provide Approve, Edit reason (Vogt writes only) and Deny against the same
  action endpoint. Show expiry/supersession in the engine's words.
- Keep Copy and Paste on the composer row through the shared clipboard bridge
  used by browser, installed PWA and Capacitor.
- Replace the wireframe footer with accurate copy: direct session writes are
  audited to the session actor; assistant writes require on-screen approval
  and are audited to the approver.

### Tests

Add Sessions composition tests for every internal tool and old deep link.
Mount real Terminal/Editor split fixtures, pending terminal input, pending Vogt
write, edited reason, expired action, absent Assistant, engine outage and core
outage. Verify direct session actor attribution separately from assistant
approver attribution.

**Done when** all machine capabilities work without leaving Sessions, no
approval implementation exists outside `AssistantRuntime`, and the UI never
misstates which actor made a write.

---

## Stage 9 — Phone and installed PWA

The phone is the same Solid bundle in Capacitor, not a second implementation.

- **Requirements** — FR-M1–M5, FR-U21, FR-U23, FR-T2, FR-T8
- **Repo** — responsive Solid components, `ModKeyRow.tsx`, `push.ts`,
  `web/public/sw.js`, `styles.css` and the Capacitor shell
- **Drawn in** — `Vogt Phone.dc.html` and turn 4 of `Vogt Mobile.dc.html`

### Primary navigation and secondary reach

- Use the 60px bottom bar for Sessions, Inbox, Board and Backlog with live,
  text-labelled counts.
- Remove the tab-count sheet. There is no hamburger and no long-press-only
  action.
- Put “Go to…” in every primary header. Its navigation-mode palette reaches
  Projects, Audit, Settings, work items and all Sessions tools from Stage 3.
- Contextual links continue to be the fastest path to project, item, audit,
  file and terminal details.
- The default narrow/coarse landing route is `/sessions`.

### Surface behaviour

- Sessions shows waiting sessions as cards with the actual visible PTY prompt
  and explicit input controls; other sessions are rows.
- One-tap `y + Enter` and `Ctrl-C` are labelled terminal-input acts, not Vogt
  approvals.
- A pending assistant action opens the Stage 2 sheet and still requires the
  on-screen Approve control. Voice offers no approval command.
- Inbox uses source pills and the exact same server cursor and reason rules as
  desktop. Row actions are a bottom sheet with targets at least 52px tall.
- Board renders one workflow state at a time through a pill row while retaining
  its server filter URL.
- Backlog uses its measured rows and keeps both bulk modes reachable; phone
  actions use sheets, not hover.
- `ModKeyRow` remains 44px high with at least 52px key width, sticky Ctrl and
  next-key chording.
- Composer text stays at least 16px and no touch target is below 44px.

### Push-to-approval flow

1. Stage 2 creates one pending action and sends a non-sensitive push.
2. Web Push or Capacitor opens `/sessions?approval=<id>`.
3. Sessions reads current assistant history.
4. A matching, unexpired id opens the exact card.
5. Approval uses the foreground authenticated request and paired core token.
6. Success returns to the target session/item; stale, denied or expired state
   cannot send anything.

### Automated and manual acceptance

Automated tests cover breakpoint rendering, every bottom-bar route, “Go to…”
reachability, coarse-pointer hint removal, target sizes, push URL handling,
stale ids and the absence of voice approval.

Final acceptance additionally requires:

- the merged `dev` image deployed through Komodo using
  `docs/DEPLOYMENT.md` §9.4; no direct `docker compose up`;
- a physical device with the dev app registered for FCM;
- push arrival, open, exact-card display, on-screen approval and the session
  becoming unblocked;
- native clipboard copy/paste and voice vocabulary checks;
- visual inspection at phone and tablet widths.

**Done when** automated checks pass **and** the device flow has been witnessed
against the live dev stack. An APK build or jsdom assertion alone cannot close
this stage.

---

## Stage 10 — Conformance, demonstrations and close-out

- **Requirements** — every ID introduced or revised in Stage 0
- **Repo** — implementation tests, `docs/REQUIREMENTS.md`,
  `docs/DESIGN.md`, `docs/ROADMAP.md`, `design/README.md` and the export sync
  map
- **Drawn in** — guardrails §§1–7

### Automated gate

From the repository root and each owned workspace:

```bash
uv run pytest
uv run mypy
uv run ruff check .
uv run ruff format --check .
uv run python scripts/check_docs.py

cd web
pnpm typecheck
pnpm test
pnpm test:browser
pnpm build

cd ../engine
cargo fmt --check
cargo clippy -- -D warnings
cargo test --all
```

A fresh `pnpm build` precedes any release Cargo build so the embedded
`web/dist/` is current.

### Browser demonstration

With real CSS and browser events:

- every route in Stage 3 reloads and survives back/forward;
- rail, pane switcher and “Go to…” preserve reachability at desktop, tablet and
  phone widths;
- a real Board drag round-trips, and refusal rolls back at the drop site;
- measured Board/Backlog content scrolls at scale with no jump, clipping,
  repeats or gaps;
- a filtered Inbox continues across pages while a new row arrives;
- every absence distinguishes unavailable, not collected and collected-empty.

Record browser/version, fixture size and result in the requirement evidence.
This closes the outstanding M11 browser demo rather than merely adding more
jsdom coverage.

### Live stack and device demonstration

Deploy the already published/pinned merged dev image through Komodo, run the
merged-stack smoke script, then perform the Stage 9 flow on hardware. Record
the deployed digest and device/app build. Production does not move as part of
this validation.

### Visual and semantic sweep

- only design tokens supply colour; amber/red retain semantic meanings;
- no gradients or decorative alert colours;
- type, borders, radii, shadows and touch targets follow the guardrails;
- monospace is limited to machine text and verbatim server sentences;
- every aggregate has age and coverage;
- every subject has trust/provisional state;
- every write path takes a typed reason and displays the server refusal
  unedited;
- no emoji-only navigation, fixed content height, duplicate drift resolver,
  client-side Inbox merge or missing bulk-label path remains.

### Documentation close-out

Only after evidence exists:

- mark delivered Stage 0 clauses complete and leave any short conjunct in the
  gap register with its actual blocker;
- update `DESIGN.md` to describe the places shell, Inbox projection and
  Sessions composition that now exist;
- update `ROADMAP.md` status;
- mark implemented screens in `design/README.md`;
- update `design/restructure-2026-08/github.md`, including its stale
  `Vogt Inbox.dc.html` name;
- keep the original design export unchanged as provenance.

**Done when** all automated suites pass, the browser and device evidence is
recorded, the live dev stack has run the merged image, and documentation
describes delivered behaviour rather than intention.

---

## Explicitly out of scope

This uplift does not:

- approval-gate arbitrary session MCP calls or create a durable approval
  service;
- mark GitHub notifications read upstream or create per-actor forge inboxes;
- merge `events.list` into the attention Inbox;
- add another CI collector;
- visually redesign Projects, Audit, Settings, work-item detail, History,
  Tasks, Assistant, Git, GUI streaming or Editor beyond their new containment;
- remove existing CLI/REST/MCP reads that the new projection composes;
- deploy production;
- claim mobile, voice, push, drag/drop or CSS behaviour from source inspection
  alone.

---

## Artefact index

Everything is under
[`design/restructure-2026-08/`](../design/restructure-2026-08/), imported
verbatim by `e92bc9d`.

| File | Used by |
|---|---|
| `Vogt Design Guardrails.dc.html` | requirements, content sizing, conformance |
| `Vogt.dc.html` | desktop places shell |
| `BoardSurface.dc.html` | Board |
| `BacklogSurface.dc.html` | Backlog |
| `InboxSurface.dc.html` | normalized Inbox and triage |
| `SessionsSurface.dc.html` | Sessions and pending-action presentation |
| `Vogt Phone.dc.html` | phone behaviour and approval sheet |
| `Vogt Mobile.dc.html` | sessions-first navigation choice |
| `Vogt Restructure Wireframes.dc.html` | direction 1a |
| `github.md` | export sync map, corrected at close-out |
| `android-frame.jsx` | phone prototype frame |
| `support.js`, `doc-page.js` | export runtime |

The export's reference to `Vogt Inbox.dc.html` is stale;
`InboxSurface.dc.html` is the file present. The export also carried a copy of
`web/src/styles.css` which was deliberately not imported. The live stylesheet
and token block remain authoritative.
