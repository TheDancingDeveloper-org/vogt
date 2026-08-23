# Vogt — surface header & rail: delta spec against `dev`@095ddf9

**Read this instead of any earlier version of this file.** The first spec in
this bundle was written against `main`, which is 78 commits and 82 changed
files behind `dev` under `web/src/` — and the two histories have diverged
(`main` carries one commit `dev` does not). Your agent's conflict report is
correct: `.board-header`, `.vogt-backlog-header` and `.vab-header` are dead as
CSS and survive only as the `class` prop passed to `SurfaceHeader`.

Everything below is **additive to `dev`**. Nothing reverts a shipped rule.

Design: `Vogt Surface Header.dc.html`, turn 5 — `5a` (header), `5b` (rail).
Styles: `rail.css`, now an append-only delta stylesheet.

---

## 0. Drift audit — what changed under `web/src/` since the recreation

Compared `main`@1026d2f (the basis for turns 1–4) against `dev`@095ddf9.
82 files changed. What touches these screens:

**New files that supersede parts of this design**

| File | What it is | Effect here |
| --- | --- | --- |
| `SurfaceHeader.tsx` + `__tests__/surfaceHeader.test.tsx` | the shared header, slots `title · honesty · spacer · controls · action · detail`, `collapseControls` disclosure for narrow clients | The header is now a component with a **test-asserted slot order**. 2a's two-band layout is withdrawn. |
| `placeMetrics.ts` + `__tests__/placeMetrics.test.ts` | shell-owned counts: `inbox.counts.active`, `project.list.total`, `work.list.total`, `backlog.total_considered`; `loading/ready/stale/unavailable`; one read in flight; 750ms coalescing; a missing total throws rather than becoming `0` | 4a's count rules **and my count-source table** are withdrawn. Never re-derive Inbox from "active minus the local seen set" — `dev` reads the server's own `counts.active`. |
| `WaitingSession.tsx` + `__tests__/waitingSession.test.tsx` | phone card for a waiting session: reads scrollback, shows the prompt tail, then offers `y + Enter` / `Ctrl-C` / Open — explicitly *not* Vogt approvals | The rail's attention card must **point at this**, never duplicate the acts. |
| `narrow.ts` | the shell's narrow query (`max-width: 768px`) | Use it; do not add a second breakpoint source. |
| `identity.ts` | `APP_NAME = "Vogt"`, `productDocumentTitle()` | There is **no instance/actor identity line**. The `node-b · actor:tim` subtitle in turns 3–4 was invented; it is not in the delta. |
| `ProgressiveFilters.tsx`, `RouteOutcome.tsx`, `Dialog.tsx`, `FeedbackCenter.tsx`, `FileWorkflowDialog.tsx`, `resizablePane.ts`, `keyboardShortcuts.ts`, `routeModel.ts`, `retainedRead.ts`, `pendingAction.ts` | filter/dialog/route/pane infrastructure added since | Not touched by this design, but they own chrome next to it — read before moving anything in a header or the rail. |

**Changed behaviour in the files this design edits**

- `.places-rail` is now the scroller (`overflow-y:auto`, `overscroll-behavior:contain`,
  `scrollbar-gutter:stable`), children `flex:0 0 auto`, percentage caps deleted,
  plus `.rail-resize-handle` (drag) and `.rail-collapse`/`.rail-reopen` +
  `.app--rail-collapsed`.
- `.place-count` exists, and `.places-section-label--counted` puts one on the
  Running label (`shell.test.tsx` asserts `[aria-label="2 running sessions"]`
  and a `data-state="stale"` count in both `.places-nav` and `.places-section-label`).
- `.phone-bottom-nav` is the ≤768px navigation, with counts, also asserted in
  `shell.test.tsx`.
- `.places-rail > .file-tree` has its own sizing block; Files is still expanded.
- `.session-row .row-btn`/`.close` still `opacity: 0` until
  `:hover`/`:focus-within`; `pointer: coarse` gets `opacity .7` at **28px** —
  still under the 44px floor the rest of the app keeps.
- `.surface-header-controls button/select` and `.surface-header-action button`
  carry `min-height: 32px`, 44px at `pointer: coarse`; labels are forced to one
  row; the ≤768px block re-orders slots visually only.

**Status of turns 1–4 in this bundle**: history. They were recreated from
`main`; their per-surface header classes are dead and their rail structure has
been replaced. Keep them for the reasoning; build from turn 5.

---

## 1. Withdrawn, with reasons

Do not re-litigate these.

| Proposed | Shipped on `dev` | Verdict |
| --- | --- | --- |
| 2a two-band title bar + honesty strip | `SurfaceHeader` one row, slots in DOM order | Withdrawn — the strip needs `order` at desktop width; the component exists so DOM and visual order cannot disagree, and its test asserts the order. |
| 2a 30px control floor | `min-height: 32px`, 44px coarse | Withdrawn — 32px is shipped and already fixed the 26/30/36px mix. |
| 2a inline control labels | `.surface-header-controls label { flex-direction: row }` | Withdrawn — same fix, already in. |
| 2a phone header stack | ≤768px `order` block + `collapseControls` | Withdrawn — theirs distinguishes chrome (Board's cadence) from the surface itself (Inbox source pills). |
| 4a R1–R3 (one scroller, non-shrinking rows, pinned nav) | the rail is itself the scroller | Withdrawn — simpler, same defect solved. Fixed identity/footer bands go with it. |
| 4a R4/R5 counts and `—` | `placeMetrics.ts` + `.place-count[data-state]` | Withdrawn — theirs separates *stale* from *not read* and bounds the query fan-out. |
| 4a R9 count-source table | `placeMetrics.ts` | Withdrawn — use its four reads verbatim. |
| 4a R10 `.rail-tabbar` | `.phone-bottom-nav` | Withdrawn. |
| 4a fixed 248px rail | resize handle + collapse/reopen | Withdrawn — width is user state. |
| 3–4 identity subtitle (`node-b · actor:tim`) | `identity.ts` has no such value | Withdrawn — it was invented. |

---

## 2. Delta A — surface header (`5a`), CSS only

No change to `SurfaceHeader.tsx`; `surfaceHeader.test.tsx` must pass unmodified.

### A1. Honesty carries tone

`.surface-header-honesty` gains `padding-left: 8px; border-left: 3px solid`,
coloured by a state class the surface sets:

| Class | Colour | Means |
| --- | --- | --- |
| `--fresh` | `--activity-done` | live / answered just now |
| `--partial`, `--stale` | `--activity-running` | a collector failed; poll paused; provisional |
| `--outage`, `--never` | `--activity-waiting` | Vogt unreachable; never swept |
| (none) | `--bd-strong` | honesty with no tone of its own |

The slot holds **one** statement: a bold lead clause (the answer's age) then
`·`-separated qualifiers. Every additional freshness paragraph moves into the
`detail` slot, inside the `details.surface-header-disclosure` those surfaces
already use. Nothing is removed; it moves one press away, which is what
`detail` is for. Per surface: Board (`.board-summary` gains the lead clause),
Audit (`ViewAgeBadge` + provenance), Inbox (response age + coverage summary),
Backlog, Projects, Work item detail, Sessions (connection state).

### A2. One accent control per header, always the `action` slot

`.surface-header-action button`: `--accent` fill, `#08111c` text, weight 600.
Disabled loses the fill, keeps the box. Nothing in `controls` may take it.
Board → Quick create · Backlog → Quick create · Projects → Check contract ·
Work item → Edit · Sessions → New session · Inbox, Audit → none.

### A3. View tabs are a segmented group

Wrap the existing view-tab buttons (`.vab-views`, `.vogt-backlog-views`,
`.vogt-projects-views`) in `div.surface-header-tabs`: `2px` inset, `radius 8px`,
`gap 4px`; members `min-height 26px`, `padding 0 12px`, `radius 6px`;
`aria-pressed="true"` takes the accent fill. 26px is the one control allowed
under the 32px floor because the group's box clears it; at `pointer: coarse`
members go to 44px like everything else.

---

## 3. Delta B — places rail (`5b`), additive

The rail is the scroller; every block added is `flex: 0 0 auto`.

### B1. Attention card — a pointer, not a second `WaitingSession`

- Placed after `.rail-go-to`, before `.places-nav`.
- Rendered **only** when a session's `activity === "waiting-for-input"`
  (`--activity-waiting`) or the engine is unreachable (`.rail-attention--outage`,
  `--activity-errored`). Outage wins ties. At most one. `running` never earns one.
- Copy: `"{n} session{s} waiting"` + `"{name} · {cwd-tail} · {age}"`; or
  `"Engine unavailable"` + the server's own sentence.
- Click routes to `/t/{id}` for a single waiting session, else `/sessions`,
  where `WaitingSessionCard` shows the prompt tail and offers the keystrokes.
  **The rail card never sends `y`, never sends `Ctrl-C`, and never labels
  anything an approval** — it shows before it asks, by pointing at the thing
  that shows.
- `.places-rail` is `display:none` at ≤768px, so this and the phone card cannot
  both appear.

### B2. Session rows: one visible overflow control

Replace `.row-btn`/`.close` (★ ⧉ ×) with one `button.row-menu` — `24×24`,
`32×32` at `pointer: coarse`, `opacity: 1` always. Delete the `opacity: 0` +
`:hover`/`:focus-within` reveal block **and** its coarse override (28px is under
the floor). Menu, in order: Attach · Bookmark / Remove bookmark · Duplicate
(same cwd) · Rename · **Kill & remove** (destructive, separated). Right-click on
the row opens the same menu (today it opens rename; this widens it).

Second line is the state **word** beside the dot — `waiting for input · 40s`,
`running · 6m`, `idle 12m` (`.state`, `.state--waiting`). Colour is never the
only signal.

### B3. Sections collapse

`.places-section-label` becomes `button.places-section-toggle`
(`aria-expanded`, caret `▾`/`▸`, existing `.place-count` kept on Running) for
Running, Recent places and Files. State in `localStorage
mydevenv2.rail.sections.v1` — `{"running":true,"recent":true,"files":false}`,
**Files collapsed by default**. The rail is one scroller now; collapsing is what
stops it becoming a long one. Do not persist scroll position. Keep
`.places-section-label--counted`'s markup inside the button so
`shell.test.tsx`'s `[aria-label="2 running sessions"]` lookup still resolves.

---

## 4. Line-height discipline (a real defect, caught three times)

Never a `line-height` below `1.5` on a clipped `10–11px` line, or below `1.4` on
a clipped heading. A line box shorter than the glyph box cuts descenders as soon
as the row is height-constrained. Tested: `10px/1.6`, `11px/1.6`, `13px/1.4`,
`20px/1.4`.

---

## 5. Acceptance criteria

Add to `web/src/__tests__/`. **Existing tests must pass unchanged** — if
`surfaceHeader.test.tsx`, `shell.test.tsx`, `placeMetrics.test.ts` or
`waitingSession.test.tsx` needs editing, the change is wrong.

**Header**
1. Slot order for a fully-populated header is still
   `["title","honesty","spacer","controls","action","detail"]`.
2. Each surface renders at most one `.surface-header-action button`, and no
   accent-filled control inside `.surface-header-controls`.
3. The honesty slot renders exactly one statement; per-collector / per-source
   detail is inside `[data-surface-header-slot="detail"]`.
4. With Vogt answering 503 the honesty slot carries
   `.surface-header-honesty--outage` and the action button is disabled.
5. Under `pointer: coarse` every control in `controls`, `action` and
   `.surface-header-tabs` measures ≥44px.

**Rail**
6. One waiting session → exactly one `.rail-attention`; engine also unreachable →
   still one, with `--outage`; only running sessions → none.
7. The rail card contains no control that calls `api.sessionInput`; activating it
   navigates to the session or `/sessions`.
8. `.session-row .row-menu` computes `opacity: 1` with no hover simulation, and
   no `.row-btn` remains in the rail.
9. Collapsing Files writes `mydevenv2.rail.sections.v1` and survives a remount;
   Files is collapsed on first run; the Running count is still reachable by its
   existing `aria-label`.
10. `.places-rail` remains the only scrolling element in the rail (regression
    guard on 1bcd0ef), and no rail text node has `clientHeight < scrollHeight`
    at viewport heights 400, 600, 900, 1200.

---

## 6. Suggested commit split on `feat/surface-header-and-rail-redesign`

1. `style(web): give the surface header's honesty slot its own tone` — A1.
2. `style(web): make the header's action slot the one accent control` — A2.
3. `refactor(web): group the view tabs into one segmented control` — A3.
4. `feat(web): raise a waiting session in the rail` — B1.
5. `fix(web): give the session row one visible overflow control` — B2 (deletes
   the hover-reveal block and its coarse override).
6. `feat(web): let the rail's sections collapse` — B3.

1–3 and 4–6 are independent. Re-run the drift audit (§0) before merging if `dev`
has moved again — it moved 78 commits between this design's first draft and its
second.
