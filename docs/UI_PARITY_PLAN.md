# Vogt GUI parity plan — surface header and places rail

Status: review and implementation plan (no product code changed by this
handoff review)

Reviewed: 2026-08-20

Design source: [`design/design_handoff_surface_header_and_rail/`](../design/design_handoff_surface_header_and_rail/)

Archive provenance: Node B `/mnt/4tnvme/Temp/UI issues to solve(1).zip`,
copied to [`design/UI issues to solve(1).zip`](../design/UI%20issues%20to%20solve%281%29.zip)
at 2,301,046 bytes, SHA-256
`b91147f48dbb397bbcfda570d2873ccd47731aa6fb93407091daf7028bddea87`.

## What was reviewed

The handoff is explicitly a delta against `dev` at `095ddf9d4bf9`. Its build
target is turn 5 of `Vogt Surface Header.dc.html`: `5a` (header) and `5b`
(rail). Turns 1–4 are historical explorations and must not be reintroduced.
The written source of truth is [`rail-spec.md`](../design/design_handoff_surface_header_and_rail/rail-spec.md);
the PNGs were inspected alongside the current implementation:

| Design evidence | Intended parity target |
| --- | --- |
| `screenshots/5a-header-delta-on-dev.png` | A single-toned surface header: one freshness statement with a state-coloured left rule, one accent action, and grouped view tabs. |
| `screenshots/5b-rail-delta-on-dev.png` | A 248px places rail with one attention pointer, always-visible session menu, state words/ages, collapsible sections, and Files collapsed by default. |
| `screenshots/1a-header-as-is.png`–`1d-header-44px-bar.png` | Historical header options; useful for explaining rejected approaches, not implementation targets. |
| `screenshots/2a-header-control-scale.png` | Historical control scale; the current 32px/44px baseline remains authoritative. |
| `screenshots/3a-rail-as-is.png`–`3d-rail-attention-first.png` | Historical rail exploration; do not restore the withdrawn fixed bands or count derivation. |
| `screenshots/4a-rail-build-target.png` | Historical rail proposal; only the surviving concepts named by turn 5 apply. |
| `screenshots/5a-header-delta-on-dev.png`, `5b-rail-delta-on-dev.png` | Build targets for the six additive changes below. |

The existing browser suite was run against the current app with an isolated
Vite port:

```text
PLAYWRIGHT_PORT=4179 pnpm exec playwright test gui.spec.ts \
  --project=desktop --grep 'working header puts|crowded places rail'
2 passed
```

Those tests prove the current baseline (one-row geometry and a single rail
scroller); they do not prove the new handoff behaviours.

## Verified implementation delta

### Header — current versus target

Current [`SurfaceHeader.tsx`](../web/src/SurfaceHeader.tsx) already provides the
semantic slot order `title → honesty → spacer → controls → action → detail`,
the narrow disclosure, and the existing 32px/44px control floor. Current CSS
also keeps the header on one aligned row on desktop. The missing work is the
handoff's three CSS/markup contracts:

1. `.surface-header-honesty` has no state-coloured left rule or 8px inset, and
   surfaces still need to reduce the slot to one lead freshness statement while
   keeping collector/provenance detail in the existing disclosure.
2. The action slot is not enforced as the only accent-filled control. A
   surface can still style controls independently; this needs an assertion and
   a per-surface audit.
3. Existing view-tab groups are not consistently wrapped in
   `div.surface-header-tabs` with the segmented 26px/44px interaction grammar.

The target is additive. Do not replace `SurfaceHeader.tsx`, revive dead
per-surface header classes, or reintroduce the withdrawn two-band header.

### Rail — current versus target

Current [`App.tsx`](../web/src/App.tsx) and [`styles.css`](../web/src/styles.css)
already have the rail as the only desktop scroller, a resizable/collapsible
rail, canonical `PlaceCount` metrics, and the phone bottom navigation. The
remaining verified differences are:

1. There is no `.rail-attention` pointer between “Go to…” and the places nav.
   Waiting sessions and engine outage are not surfaced above the nav.
2. Each session row still renders two hover-hidden `.row-btn` controls plus a
   `.close` control. The coarse-pointer override is 28px, below the app's 44px
   touch-target rule. There is no single overflow menu with the specified
   Attach / Bookmark / Duplicate / Rename / Kill & remove order.
3. “Running”, “Recent places”, and Files are plain labels, not
   `aria-expanded` section-toggle buttons. Files is not collapsed by default,
   and `mydevenv2.rail.sections.v1` is not persisted.
4. Session rows expose a dot and name/cwd but not the target state word and
   relative age beside the activity indicator.

Do not add a second rail scroller, a fixed identity subtitle, emoji/glyph-only
navigation, or a second waiting-session interaction. The attention card only
navigates to Sessions; `WaitingSession.tsx` remains the place that displays the
prompt tail and sends keystrokes.

## Delivery sequence

### 0. Freeze the acceptance boundary

- Re-read `rail-spec.md` §0–§1 before editing; record the withdrawn proposals
  in the PR description.
- Treat the existing `SurfaceHeader`, `placeMetrics`, `WaitingSession`,
  `narrow`, and `identity` modules as authoritative.
- Add tests before implementation, without modifying the existing assertions
  in `surfaceHeader.test.tsx`, `shell.test.tsx`, `placeMetrics.test.ts`, or
  `waitingSession.test.tsx`.

### 1. Header parity (A1–A3)

Files: `web/src/styles.css`, surface components that supply header slots, and
focused tests under `web/src/__tests__/`.

- Add honesty tone modifiers: `--fresh` → `--activity-done`,
  `--partial|--stale` → `--activity-running`, `--outage|--never` →
  `--activity-waiting`, with neutral fallback. Keep colour accompanied by
  text; do not add hues.
- Ensure each header has at most one `.surface-header-action button`, styled
  with the accent fill and `#08111c` text; disabled actions retain their box
  but lose the fill. Controls must not contain an accent action.
- Wrap existing view tabs in `.surface-header-tabs`; use the specified 2px
  inset, 8px group radius, 4px gap, 26px members, and 44px coarse-pointer
  members. Preserve current `aria-pressed` and query-string behaviour.
- Keep extra coverage/provenance paragraphs in the existing `detail` slot,
  not duplicated in the honesty lead line.

Acceptance: the six header assertions in `rail-spec.md` §5 pass, the existing
header geometry test remains green, and a desktop screenshot of Board and
Audit is visually comparable to `5a-header-delta-on-dev.png` at the same
viewport.

### 2. Rail parity (B1–B3)

Files: `web/src/App.tsx`, `web/src/styles.css`, a small client-state helper
for the persisted section state, and rail/shell tests.

- Derive one attention condition: outage wins; otherwise show one waiting
  session (singular route for one, `/sessions` for multiple). Render no card
  for running/idle sessions. The card must not call `api.sessionInput`.
- Replace the three row buttons with one visible `button.row-menu` (24px
  desktop, 32px coarse-pointer) that opens one menu in the specified order.
  Right-click must open the same menu; destructive Kill & remove stays
  separated. Add the state word and relative age to the row.
- Convert Running, Recent places, and Files labels to accessible toggle
  buttons. Persist `{running, recent, files}` under
  `mydevenv2.rail.sections.v1`; default `files` to `false`. Keep the existing
  `PlaceCount` node inside the Running toggle so its aria-label remains
  discoverable.
- Preserve the rail's single-scroller invariant and current resize/collapse
  behaviour. The rail remains hidden on narrow layouts; the phone bottom nav
  remains the mobile navigation.

Acceptance: the six rail assertions in `rail-spec.md` §5 pass, the crowded
rail geometry test remains green, and a desktop screenshot is comparable to
`5b-rail-delta-on-dev.png` at 395px rail width in the supplied 2× reference.

### 3. Integrated visual and device validation

- Capture Board, Audit, Sessions with one waiting session, Sessions with an
  outage, and the Files-collapsed state at desktop widths 1280×900 and
  1440×900.
- Capture phone Board, Inbox, and Sessions at the existing Playwright iPhone
  device. Verify the rail is absent, controls are at least 44px, and the
  attention pointer is represented only by the Sessions surface/card.
- Compare screenshots by region: header baseline and left honesty rule;
  segmented tabs and accent action; rail attention card; session menu hit
  area; section disclosure and Files state. Do not chase fixture copy or
  mock session names.
- Run `pnpm typecheck`, the focused and full Vitest suites, and the complete
  Playwright browser suite. Run `pnpm build` before any Rust release build so
  the embedded frontend is fresh.

## Acceptance checklist

- [ ] The target archive is retained with its checksum and the extracted
      handoff stays under `design/`, not `web/src/`.
- [ ] No withdrawn turn 1–4 proposal has been reintroduced.
- [ ] Header A1–A3 tests and screenshot comparisons pass.
- [ ] Rail B1–B3 tests and screenshot comparisons pass.
- [ ] Existing shell, place-metric, waiting-session, and header tests pass
      unchanged.
- [ ] Desktop rail has exactly one scrolling element; no nested session or
      file-tree scroll trap is introduced.
- [ ] Every write-affording control still follows the reason/audit contract;
      the new rail menu is navigation/action chrome, not a bypass around the
      operation registry.
- [ ] `docs/RESTRUCTURE.md` is updated only if these handoff changes alter its
      staged plan; `docs/DESIGN.md` is not updated until the behaviour ships.

## Suggested commit split

1. `test(web): pin surface header and places rail parity assertions`
2. `style(web): tone honesty slot and group surface view tabs`
3. `style(web): make the surface action the sole accent control`
4. `feat(web): point waiting sessions and outages from the places rail`
5. `refactor(web): replace session hover actions with one overflow menu`
6. `feat(web): persist collapsible rail sections`
7. `test(web): capture desktop and phone parity screenshots`

