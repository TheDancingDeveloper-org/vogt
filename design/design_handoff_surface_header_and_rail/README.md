# Handoff: Vogt surface header + places rail (delta against `dev`@095ddf9)

## Overview

Two related UI fixes for the Vogt PWA (`web/`, Solid + Vite), both about chrome
that repeats on every route: the **shared surface header** (`SurfaceHeader.tsx`)
and the **left-hand places rail** (`aside.places-rail`).

**This bundle has been re-cut against `dev`.** The first version was built from
`main`, which is 78 commits behind under `web/src/` (and the two histories have
diverged). Most of what that version proposed has since shipped on `dev`, in
some cases better. What remains is **six additive changes** — three per surface
area — and everything else is explicitly withdrawn with a reason.

Build **turn 5** in the design doc: option `5a` (header) and `5b` (rail).
Turns 1–4 are history: they were recreated from `main`, and their per-surface
header classes (`.board-header`, `.vogt-backlog-header`, `.vab-header`) are dead
CSS that survives only as the `class` prop passed to `SurfaceHeader`.

## About the design files

The files here are **design references created in HTML** — one Design Component
showing intended look and behaviour, plus a delta stylesheet and a written spec.
The task is to recreate them in the existing codebase: Solid + TypeScript under
`web/src/`, styling in `web/src/styles.css`, using the project's own tokens,
class names and test harness. No CSS framework, no component library, no new
colours.

`rail.css` is the exception: it is written against `dev`'s stylesheet and is
**append-only**. Do not replace the shipped rail or header blocks with it.

## Fidelity

**High-fidelity.** Every value is taken from `web/src/styles.css` at
`dev`@095ddf9. Two caveats:

- The design's mock content (session names, counts, project names) is fixture
  data, not copy to ship.
- Turns 1–4 contain values that no longer exist on `dev` — notably a
  `node-b · actor:tim` identity line that was invented and is **not** part of
  the delta (`identity.ts` has no such value).

## Read this first

`rail-spec.md` is the source of truth. Its §0 is a full drift audit of what
changed under `web/src/` between the recreation and `dev`; §1 lists everything
withdrawn and why; §2–§3 are the six changes; §5 is the acceptance criteria;
§6 is a suggested commit split.

## The six changes

**Header (`5a`) — CSS only, no change to `SurfaceHeader.tsx`**

- **A1.** `.surface-header-honesty` gains `padding-left: 8px; border-left: 3px`
  in a state colour (`--fresh` / `--partial|--stale` / `--outage|--never`). One
  statement above the fold; every extra freshness paragraph moves into the
  `detail` slot's existing disclosure.
- **A2.** Exactly one accent control per header, always the `action` slot
  (`--accent` fill, `#08111c` text, weight 600; disabled loses the fill and
  keeps the box). Nothing in `controls` may take it.
- **A3.** View tabs become `div.surface-header-tabs`: 2px inset, `radius 8px`,
  `gap 4px`, members `min-height 26px` — the one control allowed under the
  header's 32px floor, because the group's own box clears it. 44px at
  `pointer: coarse`.

**Rail (`5b`) — additive; the rail is already the scroller**

- **B1.** An attention card above nav for a waiting session or an engine outage
  (at most one; outage wins). It is a **pointer** — it routes to Sessions, where
  `WaitingSession.tsx` shows the prompt tail and offers the keystrokes. It never
  sends input and never says "approval".
- **B2.** One always-visible `button.row-menu` (`24×24`, `32×32` touch) per
  session row, replacing the three hover-revealed `.row-btn`/`.close` targets;
  delete the reveal block *and* its `pointer: coarse` override (28px is under
  the 44px floor).
- **B3.** `.places-section-label` becomes `button.places-section-toggle` for
  Running / Recent places / Files, state in `mydevenv2.rail.sections.v1`, Files
  collapsed by default; keep the existing `.place-count` markup inside the
  button so `shell.test.tsx`'s aria-label lookups still resolve.

## Withdrawn (already shipped on `dev`)

The two-band header layout · a 30px control floor · inline control labels · the
phone header stack · rail rules R1–R3 (one scroller, non-shrinking rows, pinned
nav) · count badges and the `—` convention · the count-source table ·
`.rail-tabbar` · the fixed 248px rail · the identity subtitle. Reasons are in
`rail-spec.md` §1 — read them before re-proposing any of it.

## Interactions & behaviour

- Header: the `detail` disclosure holds what the honesty line no longer repeats
  (coverage grid, per-collector ages, provenance). View tabs keep their existing
  `aria-pressed` + query-string behaviour. The single accent action is the
  surface's one write affordance and keeps its existing disabled explanation.
- Rail: the attention card appears only for `waiting-for-input` or an engine
  outage; sections are `aria-expanded` buttons; the `···` menu holds Attach ·
  Bookmark · Duplicate · Rename · Kill & remove (separated), and right-click on
  the row opens the same menu.
- Honesty rules that are product requirements, not decoration: absence is not
  zero (`placeMetrics` throws rather than reporting `0`); colour is never the
  only signal; a typed *reason* is never truncated, and so never appears in the
  rail.

## State management

Nothing new server-side, and no new reads — counts already come from
`placeMetrics.ts` (four canonical reads, one in flight at a time, 750ms
coalescing). New client state is exactly one key:
`localStorage mydevenv2.rail.sections.v1` = `{running, recent, files}` booleans,
Files `false` by default. Header disclosure state is page-local and not
persisted.

## Design tokens

All already in `:root` — do not add colours:
`--bg #0d1117` · `--bg-elev #161b22` · `--bg-tab #1c232c` · `--bd #30363d` ·
`--bd-strong #484f58` · `--fg #c9d1d9` · `--fg-muted #8b949e` ·
`--accent #58a6ff` · `--activity-idle #6e7681` · `--activity-running #d29922` ·
`--activity-waiting #f85149` · `--activity-errored #ff7b72` ·
`--activity-done #3fb950`. Accent-on-accent text is `#08111c` (as
`.login-submit` uses). Translucent fills in use: `rgba(88,166,255,.12)`,
`rgba(88,166,255,.08)`, `rgba(248,81,73,.08)`, `rgba(255,123,114,.08)`.

Control sizes: header controls `min-height 32px` (44px coarse), segmented tabs
26px, row menu 24px (32px coarse), badges 18px. Radii `5 · 6 · 8 · 9 · 10px`.
Spacing `2 · 4 · 6 · 8 · 10 · 12 · 14 · 16px`.

**Line-height discipline** (a real defect caught three times while building
this): never below `1.5` on a clipped `10–11px` line, never below `1.4` on a
clipped heading — a line box shorter than the glyph box cuts descenders.
Tested: `10px/1.6`, `11px/1.6`, `13px/1.4`, `20px/1.4`.

## Assets

None. No images, icon fonts or emoji. The three-letter place glyphs that appear
in turn 3–4 mocks are placeholders and are not part of the delta.

## Screenshots

`screenshots/` holds a 2× PNG per option, each including its badge, label and
annotation notes so it reads on its own.

| File | What it shows |
| --- | --- |
| `5a-header-delta-on-dev.png` | **Build this.** Header delta on Board and Audit in dev's shipped grammar. |
| `5b-rail-delta-on-dev.png` | **Build this.** Rail delta on dev's scrolling rail with `.place-count` pills. |
| `4a-rail-build-target.png` | History — the pre-audit rail proposal and its spec card. |
| `2a-header-control-scale.png` | History — the pre-audit header proposal and control scale. |
| `3a-rail-as-is.png` · `3b`, `3c`, `3d` | History — rail exploration recreated from `main`. |
| `1a-header-as-is.png` · `1b`, `1c`, `1d` | History — header exploration recreated from `main`. |

## Files

| File | What it is |
| --- | --- |
| `rail-spec.md` | **Source of truth.** Drift audit, withdrawn list, the six changes, acceptance criteria, commit split. |
| `rail.css` | Append-only delta stylesheet against `dev`'s `styles.css`. |
| `Vogt Surface Header.dc.html` | The design doc. Turn 5 is the build target; turns 1–4 are history. Opens in a browser (`support.js` included). |
| `github.md` | Repo association, last sync (`dev`@095ddf9), screen map, sync history. |

Repo files to read before implementing: `web/src/SurfaceHeader.tsx` ·
`web/src/__tests__/surfaceHeader.test.tsx` · `web/src/placeMetrics.ts` ·
`web/src/WaitingSession.tsx` · `web/src/narrow.ts` · `web/src/identity.ts` ·
`web/src/App.tsx` (rail ~1060–1200, `.phone-bottom-nav` ~1494) ·
`web/src/styles.css` (`.surface-header*` ~549–760 and ~8320–8400;
`.places-rail` ~269–500; `.place-count` ~405; `.session-row` ~1233–1277;
`.phone-bottom-nav` ~1153–1335) · `web/src/Board.tsx` (~1859) ·
`web/src/AuditBrowser.tsx` (~982) · `web/src/Sessions.tsx` ·
`web/src/__tests__/shell.test.tsx`.

## Where to start

1. Read `rail-spec.md` §0 (drift audit) and §1 (withdrawn) before writing code.
2. Re-run the audit if `dev` has moved again — it moved 78 commits between this
   design's first draft and its second.
3. Append `rail.css`. Delete only the two blocks §3 names (the `.row-btn`
   hover-reveal and its coarse override).
4. Add the §5 acceptance assertions **before** wiring behaviour. Existing tests
   must pass unmodified; if one needs editing, the change is wrong.
5. Header A1→A3 first (independent of the rail), then rail B1→B3.
