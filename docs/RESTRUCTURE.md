# The places restructure — a staged plan

**What this document is.** A plan for turning the design export in
[`design/restructure-2026-08/`](../design/restructure-2026-08/) into the
shipped PWA and phone app. Nine stages, ordered so nothing is drawn before the
operation behind it exists.

**What it is not.** A requirement. `design/README.md`'s first rule is that a
wireframe is not a requirement, and that applies to a plan drawn from one:
**nothing here is owed until Stage 0 mints an ID for it** in
`REQUIREMENTS.md`. It is also not a design document — `DESIGN.md` describes
what exists and is held to it, and every screen below is unbuilt. Where this
document and the code disagree, the code is right and this document is old.

**Why every stage cites its source.** The product's own first principle is that
every answer carries provenance. A plan derived from a set of drawings should
be checkable against them rather than trusted, so each stage names the
`.dc.html` it came from, the guardrail clause it obeys, and the repository file
it changes.

---

## The gap

Read against the build at `design/restructure-wireframes`.

| Drawn | Built today | Where it lives now |
|---|---|---|
| 248px text-first places rail; surfaces are not closable | Drawer plus a closable tab strip, in `tabbed` or `ide` mode | `App.tsx:905-1210`, `layout.ts` |
| Inbox is its own place, one stream over GitHub · drift · CI · agents | `?view=inbox` on the audit browser, *plus* a second drift inbox on Projects | `AuditBrowser.tsx:114`, `Projects.tsx:71` |
| Cards and rows size to their content; collapsed cards clamp at 3 lines | Fixed `CARD_HEIGHT = 116` and `ROW_HEIGHT = 40`, pinned to CSS variables | `Board.tsx:136-142`, `Backlog.tsx:73` |
| No glyph or emoji navigation anywhere | `TAB_GLYPHS` is entirely emoji, and the drawer buttons match it | `App.tsx:96-110` |
| Sessions is a surface holding terminal and editor panes | Terminals and editors are top-level tabs beside the Vogt surfaces | `tabs.ts:4-18` |
| Phone lands on sessions, four-place bottom bar, one board state at a time | Same PWA responsive at 768px, with a tab-count sheet | `styles.css` @768, `App.tsx:1376` |

### Two things the export cannot settle by itself

**The inbox as drawn has no backing operation.** `registry/operations.py`
publishes `notifications` as *read-only*. There is no archive, snooze or
mark-read operation, and no approval concept behind the Sessions approval bar.
Of the six buttons an Inbox row draws, only *convert* (`work.create`) and
*suppress* (`suppress`) exist today. Stages 1 and 2 build the rest.

**Guardrail §3 contradicts NFR-S5.** "No fixed `--board-card-h` tile, no 40px
backlog row" removes exactly what NFR-S5 requires and what `boardScale.test.tsx`
asserts. The guardrails' own §8 concedes this is an implementation note rather
than design. Stage 0 amends the requirement to *measured* windowing rather than
deleting it.

---

## Settled before planning

| Question | Answer |
|---|---|
| Backend | Core operations get added — not faked in the client |
| Shell | Full places model; the tab strip goes |
| Windowing | Content-sized, windowed by measurement |
| Gate | Stage 0 defines before anything is built |

---

## Stage 0 — Define

*No product code.* The review-and-define gate: every stage below has a numbered
requirement to point at before a line is written, because this repository does
not ship otherwise.

- **Drawn in** — `Vogt Design Guardrails.dc.html`, the whole document
- **Guardrail** — §1 principles · §7 don't · §8 "implementation notes that are not design"
- **Repo** — `docs/REQUIREMENTS.md` §5–§7, `docs/DESIGN.md`, `docs/ROADMAP.md`

**What changes**

- New requirement IDs for notification state, agent-write approval, the places
  shell, the unified inbox, and the sessions-first phone. §7.3 warns that
  minting IDs turns closed decisions back into open work, so each is minted
  deliberately, with the drawing it comes from named.
- **NFR-S5 amended** from fixed-height windowing to measured-offset windowing.
  The assertion that `CARD_HEIGHT` must equal `--board-card-h` is replaced, not
  dropped.
- Two IDs are *superseded* rather than added: whatever pins the audit browser's
  `?view=inbox`, and whatever pins the tab model. Recorded as superseded, with
  the successor named.
- `DESIGN.md` gains the places shell; `ROADMAP.md` gains the stage.
- The six [open decisions](#open-decisions) are answered and written down.

**Done when** `scripts/check_docs.py` and `tests/test_requirements_audit.py`
pass, and every stage below names an ID that exists.

---

## Stage 1 — Core: notification state

First, because it is independent of the UI and because the Inbox cannot
honestly draw *archive* and *snooze* until something can record them.

- **Drawn in** — `InboxSurface.dc.html`: row action bar, snooze panel, "Archive all read"
- **Guardrail** — §6 "actions, all present: archive, convert, open, snooze, suppress with a reason"
- **Also** — `Vogt Phone.dc.html`, the bottom sheet's reason capture
- **Repo** — `application/services/notifications.py`, `registry/operations.py:847`

**The architectural constraint.** Notifications are derived from *observations*
— collector-appended, append-only evidence. State a person sets cannot live
there. It goes in the **declared** store keyed on `subject_key`, which is
exactly the reasoning `migrations/declared/0003_observed_first.sql` already
gives for `suppressions` and `work_links`, and it is what makes an archive
survive re-observation of the same subject.

**What changes**

- `storage/sqlite/migrations/declared/0008_notification_state.sql` — new
  `notification_states` table: `subject_key`, `state` ∈ (read, archived,
  snoozed), `snooze_until`, `actor_id`, `reason`, `created_at`.
- `core/entities.py` — `NotificationState`.
- `storage/interface.py` and `storage/sqlite/declared.py` — read and write.
- `application/models.py` — params and result models; `NotificationsParams`
  gains a `state` filter.
- `application/services/notifications.py` — join state onto
  `NotificationView`, expire lapsed snoozes on read.
- `registry/operations.py` — three mutating operations with `HttpRoute` and
  `CliBinding`, each refusing without a reason. MCP tools and the CLI pick them
  up from the registry.
- `web/src/vogtApi.ts` — new entries in `ROUTES`. A Python test already
  resolves every path here against the registry, so a typo fails the build.

**Tests** — `tests/test_notifications.py`, `test_migrations.py`,
`test_registry.py`, `test_requirements_audit.py`

**Done when** a notification can be archived and snoozed from the CLI, the
state survives the next sweep re-observing the same subject, and every write
left an audit row carrying its reason.

---

## Stage 2 — Core: agent-write approval

The largest new subsystem, and the only one that changes *who a write is
attributed to*. Worth its own review before any code.

- **Drawn in** — `SessionsSurface.dc.html`, the "Approval required" bar
- **Quoting** — actor `session:clip-fix-3` · "on your credential, not the agent's" · Approve / Edit the reason / Deny
- **Guardrail** — §1.6 "agents are visible actors… an approval happens on the approver's credential"
- **Also** — `Vogt Phone.dc.html`, "1 write waiting on your approval". The export calls the push that carries this "the highest-value remaining piece of the mobile app".
- **Repo** — `application/identity.py`, `application/writes.py`

**What changes**

- `migrations/declared/0009_approvals.sql` — the proposed write, the requesting
  actor and session, the reason, state, `decided_by_actor_id`, `decided_at`.
- `registry/operations.py` — `approval.request`, `approval.list`,
  `approval.decide`.
- `application/identity.py` and `writes.py` — an approved write executes as the
  *approver*, not the proposer. This is the semantic change; the audit row must
  make both actors legible or the trail lies.
- Push: an approval request is a notifiable event. `web/src/push.ts` and
  `api.ts:328` already carry a subscription shape to extend.

**Tests** — new `tests/test_approvals.py`; `test_auth.py` and `test_writes.py`
revised for the attribution rule.

**Done when** a session's proposed transition is refused until approved, the
approved write lands under the approver's actor, and the audit row names both.

---

## Stage 3 — Web: the places shell

Highest blast radius on the front end. Doing it before the surfaces means each
surface is built once, into its final home.

- **Drawn in** — `Vogt.dc.html`, the entry point, and the rail every surface file repeats
- **Quoting** — Work: Board · Backlog · Inbox — Estate: Projects · Audit — Machine: Sessions. Running list with activity dot and continuity glyph, then Files with search, then connected · Settings.
- **Guardrail** — §4 desktop shell, all six clauses · §7 "no emoji in navigation", "don't move a surface behind a closable tab"
- **Chosen from** — `Vogt Restructure Wireframes.dc.html`: five directions, **1a** was chosen
- **Repo** — `web/src/App.tsx`, `tabs.ts`, `routes.ts`, `layout.ts`

**What changes**

- New `web/src/Rail.tsx` — 248px, three groups, active state is a 2px accent
  bar plus `--bg-elev` and never a boxed tile. Carries the running-session list
  and the file tree with search; neither is a modal.
- `routes.ts` — add `/inbox` and `/sessions`. Its own header notes a path
  reaches the app only if `App.tsx`'s URL→tabs effect has an arm for it, so
  both move together.
- `tabs.ts` — the `Tab` union loses `board`, `backlog`, `projects`, `audit`.
  What remains are panes *inside* Sessions. `STORAGE_KEY` goes to `.v2` with a
  migration off `mydevenv2.tabs.v1`, because people have tabs open.
- `App.tsx` — drawer, scrim, resizer, tab strip and mobile tab sheet removed.
  **`TAB_GLYPHS` deleted** (`App.tsx:96-110`): guardrail §7 rules out
  pictographic navigation, and the comment there explaining why the glyphs
  match the drawer's goes with it.
- `layout.ts` — `tabbed`/`ide` is obsolete as a shell mode; it either goes or
  is rescoped to how panes arrange inside Sessions.
- `CommandPalette.tsx`, `KeyboardShortcuts.tsx`, `workspaceLayouts.ts` follow
  the union.
- `styles.css` — new rail block; the drawer and tab-strip blocks go.

**Tests** — `shell.test.tsx` (14) rewritten, the only web test naming the
drawer and tab strip. `commandPalette.test.tsx` (12) and `live.test.tsx` (24)
touched. `tests/test_pwa.py` checks the built bundle.

**Done when** every route in `APP_ROUTES` opens its place, a pasted link
restores it, no surface can be closed, and nothing in navigation is an emoji.

---

## Stage 4 — Web: Inbox

One place, three existing reads plus Stage 1's writes. The honesty rules are
the hard part, not the layout.

- **Drawn in** — `InboxSurface.dc.html`, desktop, with the 196px source/state aside
- **And** — `Vogt Phone.dc.html` `onInbox`, the phone's scrolling source pills
- **Guardrail** — §6 all five clauses · §1.4 "empty is two different answers" · §8 "notifications need their own route rather than `?view=inbox`, and the drift read currently on Projects has to feed the same stream"
- **Repo** — `AuditBrowser.tsx:331-388` (`describeCoverage`), `Projects.tsx:956`

**What changes**

- New `web/src/Inbox.tsx` at `/inbox`, merging `notifications`, `drift.list`
  and `events.list` into one ordered stream. Each row: unread dot · source tag
  in that source's colour · title · provenance and freshness line · relative
  age.
- `AuditBrowser.tsx` — `ViewName` collapses to `"audit"`; the inbox half goes.
  `describeCoverage` moves to a shared module, because it is what makes "not
  collected" and "nothing to say" different answers and Inbox needs it.
- `Projects.tsx` — the drift view feeds the same stream (decision 5 settles
  whether it also keeps a local view).
- Actions wired to what exists: archive · snooze · convert → `work.create` ·
  suppress → `suppress` · open item · open session. Reason capture is inline in
  the surface, and a refusal renders Vogt's own sentence *unedited, in
  monospace* (§1.5, §7).
- Header states sweep coverage. Filtering to a source with no collector shows
  **"Not collected"**, never an empty list.
- Keyboard: `j`/`k` move, `e` archive, `s` snooze, `c` convert, `o` open
  session — hidden on coarse pointers per §5.

**Tests** — new `inbox.test.tsx`. `auditQuery.test.tsx` (35) loses its inbox
arm; `driftInbox.test.tsx` (8) and `absentStates.test.tsx` (10) revised — the
last is where the two empties are already asserted apart.

**Done when** filtering to CI with no collector configured says "Not collected"
and explains that nothing below is evidence either way; every row carries an
age and a trust; no write goes through without a reason.

---

## Stage 5 — Web: Board

The requirement amendment from Stage 0 lands here as code.

- **Drawn in** — `BoardSurface.dc.html`
- **Quoting** — header grammar · filter chips with *+ filter* and saved lenses · inline create · expand-in-place · move-with-reason · refusal in mono
- **Guardrail** — §3 "rows and cards size to their content — this is the point of the restructure" · §4 filters and inline reasons · §8 the windowing note
- **Repo** — `Board.tsx:136-142, 2066-2092`, `styles.css:3967-3974, 4513-4520`

**What changes**

- Delete `CARD_HEIGHT`, `CARD_GAP`, `CARD_SLOT` and the `--board-card-h` /
  `--board-card-gap` pair they must currently match.
- Replace the fixed-slot window with a measured one: a `ResizeObserver` per
  rendered card, prefix-sum offsets, binary search for the first visible index.
  NFR-S5's guarantee is kept; its mechanism changes.
- Collapsed cards clamp at 3 lines and expand in place — the expansion is not a
  modal (§4: "a modal over the board counts as leaving the view").
- Header regrammar: title · freshness chip · honesty chip · spacer · view
  controls · primary action. Filters become chips plus *+ filter* plus named
  saved lenses, replacing the select grid.
- Inline quick-create and inline move-with-reason; the refusal panel prints the
  server's sentence verbatim.
- Trust badge on every card: declared neutral, observed amber (§1.2).

**Tests** — `board.test.tsx` (49) and `boardScale.test.tsx` (15). The second is
the NFR-S5 assertion — **rewritten against measured offsets, not deleted**. The
unrun browser demo in `REQUIREMENTS.md` §7.2 still needs a person and a
browser; jsdom cannot fire `dragover`.

**Done when** a long title makes its card taller instead of clipping, the
column still windows at scale, and the drag round-trip and its rollback still
pass.

---

## Stage 6 — Web: Backlog

- **Drawn in** — `BacklogSurface.dc.html`
- **Quoting** — rank · ref · title · score bar · trust · age; why-factors under the title; weights panel that reads and does not set; coverage footer naming `ci ✗ not in this sweep`
- **Guardrail** — §3 "no 40px backlog row" · §1.1 provenance and freshness · §7 "don't show an aggregate without its age"
- **Repo** — `Backlog.tsx:73, 984-1018, 1595-1599`

**What changes**

- `ROW_HEIGHT` goes; `virtualized()` and `window_()` take the measured strategy
  factored out in Stage 5; the spacer div at 1595 is computed from measured
  offsets.
- Rows expand in place to Open · Open session · Adopt · Suppress · Select, with
  the provenance line beneath.
- Bulk select with *one* reason for the batch, as drawn.
- Weights panel is read-only and says so — it reads the core's ranking config,
  it does not set it.

**Tests** — `backlog.test.tsx` (32).

**Done when** a two-line title no longer clips, the coverage footer names what
did not run, and every score shows its age.

---

## Stage 7 — Web: Sessions

- **Drawn in** — `SessionsSurface.dc.html`
- **Quoting** — 262px session list · pane bar with `panes 2` · per-pane work-item pill · approval bar · composer with Copy and Paste · footer "every write an agent makes carries a reason and lands an audit row"
- **Guardrail** — §4 "terminals and editors live inside Sessions" · §1.6 agents are visible actors · §8 the clipboard note
- **Repo** — `TerminalWorkspace.tsx`, `EditorWorkspace.tsx`, `SplitEditor.tsx`, `clipboard.ts`

**What changes**

- New `web/src/Sessions.tsx` at `/sessions`, hosting the existing workspaces as
  panes rather than tabs.
- The approval bar reads Stage 2 and states the actor, what is written, and
  that it goes on your credential — in those words, because §1.6 is what they
  encode.
- Copy and Paste on the composer row. Guardrail §8 is explicit that this is a
  PTY and WebView-clipboard problem, not a layout one: the design only reduces
  how often you need it, and **one bridge for web, PWA and Capacitor is the
  actual fix**. Scoped here, but it is its own piece of work.

**Done when** attaching, splitting and approving all work without leaving the
place, and an agent's write names the session in the audit row.

---

## Stage 8 — Mobile

Same Solid bundle: `mobile/capacitor.config.ts` sets `webDir: "web"` with a
server URL, so this is one codebase at a second form factor — not a second app.

- **Drawn in** — `Vogt Phone.dc.html`, the working prototype, in `android-frame.jsx`
- **And** — `Vogt Mobile.dc.html`: turn 4 sessions-first (chosen), turn 3 the surfaces
- **Guardrail** — §5, all seven clauses · §3 "terminal composer is 16px — smaller triggers iOS zoom-on-focus"
- **Repo** — `ModKeyRow.tsx`, `styles.css` @768 and `pointer:coarse`, `App.tsx:1376`

**What changes**

- Four-place bottom bar — Sessions · Inbox · Board · Backlog — 60px, live
  counts. The tab-count sheet at `App.tsx:1376` goes; there is no hamburger.
- Landing is the session list sorted by what needs you. A waiting session is a
  *card* carrying the actual PTY prompt with one-tap `y ⏎` and `^C`; everything
  else is a row.
- Row actions move into a bottom sheet, one per line at ≥52px. Nothing
  important behind long-press; nothing at all behind hover.
- Board shows **one state at a time** via a pill row — the existing vertical
  stack is what made the phone board a mile of page.
- `ModKeyRow.tsx` keeps its contract: 44px tall, 52px minimum key width, Ctrl
  sticky and chording with the next key.
- Keyboard hints hidden on coarse pointers; no touch target below 44px.

**What this cannot close.** `REQUIREMENTS.md` §7.2's phone demo (a push
arrives, is opened, the session is unblocked) and real-device FCM delivery both
need a device and a hand. The APK builds in CI; delivery is unconfirmed. Stage
8 makes them *possible*, not done.

**Done when** the phone opens on what needs you, an approval can be answered
from the lock screen forward, and every layout claim at 768px has been seen
rather than asserted in jsdom.

---

## Stage 9 — Conformance and close-out

- **Drawn in** — `Vogt Design Guardrails.dc.html` §2, §3, §7
- **Repo** — `web/src/styles.css`, the token block the guardrails quote

**The sweep**

- No colour outside the token set; no gradients; amber and red never decorative
  — one meaning per colour on every surface.
- Type scale held: 20px surface title, 13–14px body, 12px controls, 11.5px
  meta, 10.5px badges, nothing below 10px. Phone: 17 / 14–14.5 / 12.5 / 11.5,
  composer 16.
- Monospace only for machine text — refs, paths, commands, terminal output,
  Vogt's own error sentences.
- Borders 1px; radii 4 / 6 / 8–14; shadows only on sheets, toasts and the phone
  FAB.
- Every aggregate carries its age; every subject carries its trust; every write
  carries a reason; no refusal paraphrased.

**Close-out**

- Close the Stage 0 IDs in `REQUIREMENTS.md` §5/§6; move anything unbuilt into
  §7 with the reason.
- Update `design/restructure-2026-08/github.md`'s screen map — it is the tool's
  own sync record and will be rewritten on the next sync, so the repository's
  copy should not be the stale one.
- Note in `design/README.md` which screens are now built, since its whole
  premise is that the drawings drift from the code.

---

## Open decisions

Six answers Stage 0 has to write down. Each changes code, so none can be
deferred past it.

1. **Does an archive survive re-observation?** Suppression does, deliberately.
   If archive is keyed on `subject_key` it behaves the same way; if it is keyed
   on the observation, the next sweep resurfaces it. The design says
   notifications are "instance-scoped, not per-actor", which settles the actor
   question but not this one.
2. **Does an approved write execute as the approver?** The drawing says "on
   your credential, not the agent's" in amber. That is the strongest claim in
   the export and it rewrites attribution in `identity.py`. Confirm it is meant
   literally before Stage 2 starts.
3. **What happens to tabs people already have open?** `mydevenv2.tabs.v1` holds
   live state. Migrate board/backlog/projects/audit entries into rail places
   and the rest into Sessions panes, or drop them with a toast. Silent loss is
   the one wrong answer.
4. **Is a CI collector in scope?** Guardrail §8 says no — with GitHub
   unconfigured, GitHub and CI simply report "not collected". But the Inbox
   draws CI rows with real content and colours the tag red. Confirm the source
   is declared and empty rather than built.
5. **Does Projects keep a drift view?** §8 says the drift read on Projects "has
   to feed the same stream". Feeding Inbox and *also* keeping the local view
   means two places to resolve the same proposal; removing it means leaving a
   project page to act on that project's drift.
6. **How much of the export is in this pass?** Its own closing paragraph lists
   Projects (with contract age and the dependency graph), Audit, work-item
   detail, the command palette, the file viewer/editor, and tablet width as
   **still unbuilt**. This plan covers Board, Backlog, Inbox, Sessions, the
   shell and the phone. The rest keeps today's design inside the new rail —
   which is coherent, but it should be a decision rather than an omission.

---

## Artefact index

Everything lives in [`design/restructure-2026-08/`](../design/restructure-2026-08/),
imported verbatim by `e92bc9d`.

| File | What it is | Cited by |
|---|---|---|
| `Vogt Design Guardrails.dc.html` | The only part that is not a drawing — principles, tokens, type, density, don'ts. Read first. | 0, 5, 6, 9, and every stage's guardrail line |
| `Vogt.dc.html` | Desktop shell: rail, three groups, running list, file tree, drawer-free | 3 |
| `BoardSurface.dc.html` | Board with content-sized cards, chips, lenses, inline create and move | 5 |
| `BacklogSurface.dc.html` | Ranked backlog, why-factors, read-only weights, coverage footer | 6 |
| `InboxSurface.dc.html` | One inbox over GitHub, drift, CI and agent events | 1, 4 |
| `SessionsSurface.dc.html` | Session list, panes, approval bar, composer | 2, 7 |
| `Vogt Phone.dc.html` | Working phone prototype in an Android shell | 1, 2, 4, 8 |
| `Vogt Mobile.dc.html` | Phone options — turn 4 sessions-first was chosen, turn 3 the surfaces | 8 |
| `Vogt Restructure Wireframes.dc.html` | The five shell directions the rest came out of; **1a** was chosen | 3 |
| `github.md` | The tool's sync record and screen → repo-file map | 9 |
| `android-frame.jsx` | The device frame the phone mockups render inside | 8 |
| `support.js`, `doc-page.js` | The export's own runtime; every `.dc.html` loads `./support.js` from beside it | — |

### Two notes for cross-checking

`github.md`'s screen map names **`Vogt Inbox.dc.html`**, which is not in the
directory — superseded by `InboxSurface.dc.html`. Not a missing file; a stale
row in the map, and Stage 9 fixes it.

The export also carried a copy of `web/src/styles.css`, deliberately not
imported. The colour tokens the guardrails quote are the live ones — read them
in `web/src/styles.css`, not in a second copy.
