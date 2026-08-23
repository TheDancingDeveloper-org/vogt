# Vogt restructure conformance review — live dev instance

**Reviewed:** 2026-08-17 (UTC)  
**Instance:** `https://vogt-dev.sprooty.com/#/`  
**Scope:** visual and interaction conformance of the deployed PWA to the August 2026 restructure materials; no product code was changed.  
**Verdict:** **Not yet truthful to the design as a delivered visual/interaction experience.** The deployment has implemented much of the restructure's *information architecture and domain honesty*, but several of the most conspicuous interaction and composition rules are absent or contradicted—especially on phone. It looks more like existing forms/tables rehoused under a new rail than the designed product uplift.

## 1. Authority and method

This review deliberately separates three kinds of claim:

1. `design/restructure-2026-08/*` is design intent, **not** a specification (`design/README.md`).
2. `docs/RESTRUCTURE.md` is an implementation plan, **not** itself a requirement.
3. Stage 0 did mint requirements. The currently owed clauses include FR-U23 (places/Sessions), FR-U24 (Inbox), FR-U25 (content-sized Board/Backlog), FR-T8 (pending action presentation) and FR-M5 (phone navigation/reachability). `docs/REQUIREMENTS.md` §7 already describes all of these as incomplete.

Accordingly, “not truthful” here means either:

- the live UI materially contradicts an explicit guardrail or a minted requirement; or
- the screen makes the restructure appear delivered while preserving the old interaction model under the new route/shell.

### Validation performed

The deployed instance was opened with its authenticated front-door token in Playwright Chromium 151 and captured at:

- desktop: 1440×1000;
- phone: Playwright iPhone 13 profile (390 CSS px wide, coarse/touch input).

Routes inspected: `/sessions`, `/inbox`, `/board`, `/backlog`, `/projects`, `/audit`, `/settings`; `/` was also checked and correctly resolved to Board on desktop and Sessions on phone. Screenshots in this folder are direct live captures, not mockups.

Limitations:

- no writes were executed, so transition refusal, archive/snooze, suppression, adoption, approval, push and deep-link completion were not destructively exercised;
- the live instance had one running session and no pending approval/waiting-for-input prompt, so those states could be judged only by their absence and by static reachability, not by an end-to-end act;
- this is a visual/interaction review, not a replacement for the bounded-read, migration, cursor-continuation or device/FCM evidence already listed as missing in `docs/REQUIREMENTS.md` §7.

## 2. Executive assessment

| Area | Result | Summary |
|---|---|---|
| Desktop places shell | **Partial** | Stable text places, route reachability and 248px rail are recognisable. The rail is cluttered with pictographic file actions, and the surfaces do not share the intended header/filter grammar. |
| Inbox | **Partial / structurally credible** | A real standalone, server-ordered Inbox with coverage and evidence exists. Presentation is a large operations form, not the concise stream designed; phone actions remain inline rather than in a bottom sheet. |
| Board | **Materially non-conformant** | Desktop still uses a dense grid of selects and visually clipped cards. Phone stacks the desktop controls and all workflow columns instead of showing one state at a time. |
| Backlog | **Materially non-conformant** | Desktop is still a dense table, not the designed content-sized ranked rows. Phone is a scaled/stacked filter form; the first viewport contains no backlog content. |
| Sessions | **Materially incomplete** | The designed steering/workspace composition is absent in the observed state. Phone shows a tiny concatenated session control and tool buttons, not attention-sorted session cards/rows. |
| Phone shell | **Navigation only** | The four labelled places and “Go to…” exist, but counts are missing and primary surface behaviour remains desktop-first. |
| Domain honesty | **Strongest area** | Coverage, freshness, trust, “not collected” and explicit reason fields are visible. This is important real delivery, but it does not compensate for the missed composition and interaction design. |

**Bottom line:** the implementation has delivered the skeleton and several truth-preserving contracts, but not the designed experience. Calling the overall restructure “delivered” would be misleading. “Partially implemented, with shell/navigation and Inbox contracts landed” is accurate.

## 3. Cross-cutting findings

### F1 — High: phone surfaces remain desktop-first and push useful content below the fold

**Evidence:** `screenshots/phone-sessions.png`, `phone-inbox.png`, `phone-board.png`, `phone-backlog.png`.

At the actual 390 CSS-pixel viewport, the base type scale is broadly readable, but the surface composition is not phone-first. Board, Backlog and Inbox spend the first viewport on explanatory prose, refresh/selection controls and stacked filters; the useful cards/rows begin below the fold. Sessions exposes a single compact concatenated session control rather than a readable attention-sorted list.

This is a composition/density failure rather than a claim that every literal font token is wrong. The intended phone scale is 17px screen title, 14–14.5px row title, 12.5px controls and 11.5px meta, with the phone acting as a steering device. The live surfaces technically fit the viewport but do not use it to surface the information the design prioritises.

**Impact:** the phone cannot provide the designed glanceable steering experience. Filters and explanatory copy displace sessions, attention rows and ranked work.

### F2 — High: phone Board directly contradicts the “one state at a time” rule

**Evidence:** `screenshots/phone-board.png`; the captured DOM included cards from Open, In progress, Blocked, Review and Done on the same route.

The guardrail and Stage 9 explicitly require a pill row selecting one workflow state and say never to stack every column vertically. The live phone shows:

- a long explanatory paragraph;
- refresh controls;
- a large five-select/filter form;
- then the complete multi-state board lower in the same scroll view.

There is no state-pill selector in the first viewport, and the live DOM contained all columns' items. This recreates exactly the “mile of page” failure the design called out.

### F3 — High: Board and Backlog retain the old form/table interaction model

**Evidence:** `screenshots/desktop-board.png`, `desktop-backlog.png`, `phone-board.png`, `phone-backlog.png`.

The desktop guardrail says filters are chips plus “+ filter” and named lenses, **never a four-column grid of selects**. The live Board uses Project, Initiative, Label, Assignee and Swimlanes selects in a large bordered grid. The live Backlog uses Project, Label, Initiative, Actor and Page size selects, with type/state chips below.

The Backlog drawing calls for ranked content-sized rows with rank/ref/title, why factors, provenance, score/trust/age and in-place actions. The live desktop is recognisably the old dense table: checkbox, REF, TITLE, TYPE, STATE, PRI, TRUST, PROJECT, UPDATED, SCORE. Long titles are visually truncated by column width. On phone, the first viewport is almost entirely filtering controls and loading/freshness chrome; no ranked work is visible.

This is the clearest example of “rehousing rather than restructuring.” The route and shell changed, while the surface's mental model did not.

### F4 — High: the observed Board cards do not visibly meet content-sized/expand-in-place intent

**Evidence:** `screenshots/desktop-board.png`.

Several card titles end in ellipses (`no…`, `as…`) and cards appear to share a compact slot-like visual height. No visible affordance communicates “expand in place,” and the live card presentation does not resemble the drawing's content-sized rows/cards with three-line clamp and explicit expanded actions.

`docs/REQUIREMENTS.md` says measured windows now exist but explicitly leaves large-estate browser proof and clipping risk open under FR-U25. The screenshot validates that the remaining risk is not theoretical: meaningful titles are clipped in the delivered view.

### F5 — High: Sessions is a route, not yet the designed workspace/steering surface

**Evidence:** `screenshots/desktop-sessions.png`, `phone-sessions.png`.

Desktop shows a mostly empty page with one compact concatenated control (`vogtdev/home/sprootyrunning`) and four tool buttons. It does not present the designed three-part workspace: attention-sorted session list, pane bar/active terminal workspace, and pending-action composition.

Phone is more severe. The design says:

- sessions sorted by what needs attention;
- waiting sessions as cards with actual visible PTY prompt and labelled `y + Enter` / `Ctrl-C` acts;
- other sessions as readable list rows;
- working sessions show actor/binding/continuity context.

The observed phone route shows only “Connected”, the concatenated session control, and Git/History/Tasks/GUI stream buttons. Even allowing for the live fixture having no waiting session, the lone running session is not rendered as the designed row and its name/cwd/state are visually run together.

### F6 — High: Inbox semantics are substantially delivered, but its designed stream presentation is not

**Evidence:** `screenshots/desktop-inbox.png`, `phone-inbox.png`.

What is truthful and good:

- Inbox is its own place;
- it states server ordering;
- coverage is explicit per source;
- “Not collected” and “Not available” are distinct from empty;
- source, trust, occurrence/observation age and evidence-before-action are present;
- reason fields are visibly adjacent to mutating controls.

What misses the design:

- desktop opens with a large coverage dashboard plus bulk-action form, not a compact source/state header and readable stream;
- rows expose every action and field inline, creating a wide operations console rather than progressive disclosure;
- raw JSON evidence dominates drift rows;
- phone retains the same inline forms/actions. The design explicitly requires row actions in a bottom sheet, one per line at ≥52 px;
- the first phone viewport is consumed by coverage and selection controls, so no attention row is visible.

This surface is the strongest implementation contractually and the weakest example of why contractual delivery alone is not visual conformance.

### F7 — Medium-high: phone bottom navigation lacks the designed live counts

**Evidence:** all phone screenshots.

The four labelled primary places exist and the active place is clear. That is real FR-M5 progress. However, the 60 px bar was designed with live counts; Sessions, Inbox, Board and Backlog show no counts/badges at all despite Inbox reporting 83 attention items in its coverage panel and Board reporting 17 loaded items.

Counts are not decoration here; they are the phone's glanceable steering signal.

### F8 — Critical: the Files tree in the left rail is visibly broken; it also violates the text-first/no-pictographic-navigation intent

**Evidence:** `screenshots/desktop-sessions.png`, `desktop-board.png`, and captured DOM text. See the addendum below for the explicit broken-sidebar assessment.

The primary place labels are text-first and good. But the always-visible file tree is full of emoji/pictographic glyphs (`📁`, `📝`, `📄`, `📋`, `✦`, `⇪`, `>_`, `✎`, `⧉`, `×`, `⬇`). The guardrails specifically say the repo's file-icon emoji set is deliberately not used in these designs, and Stage 3 says to delete emoji navigation labels.

The result is not merely visually unlike the selected rail: in the live screenshot the rows are not legible enough to function as a file tree. Several actions are also glyph-only at 22–28 px in CSS, below the phone/touch target rule if they become reachable on coarse inputs.

### F9 — Medium: surface header grammar is inconsistent and verbose

**Evidence:** all desktop screenshots.

The design standard is: title → freshness → honesty → spacer → view controls → primary action. Live screens diverge:

- Board starts with counts and a long paragraph, then refresh controls;
- Backlog begins with a view toggle, evidence banner, then filters;
- Inbox begins with an eyebrow, title, explanatory paragraph, source select, bulk form, coverage dashboard;
- Sessions begins with eyebrow, title, explanatory copy, connection state and tools.

Much of the copy is accurate, but it is documentation copy occupying operational UI. Honesty should be attached to answers, not require every surface to lead with a paragraph explaining its implementation.

### F10 — Medium: secondary routes are reachable, but phone containment is not consistently credible

**Evidence:** `screenshots/phone-projects.png` and captured phone `/settings` text.

“Go to…” is visible and secondary routes did open, which is an important success. But the Projects route is still a desktop-oriented multi-tab/filter/list surface, and Settings opens with generic “Open a file from the rail or create a session” shell text before the settings form. The phone route exists; it is not yet convincingly rehoused as a phone-usable secondary destination.

## 4. Surface-by-surface conformance matrix

| Designed/required behaviour | Live observation | Assessment |
|---|---|---|
| Stable places, no closable product tabs | Board/Backlog/Inbox/Projects/Audit/Sessions are stable routes in a left rail; phone has four fixed primary routes | **Conforms** |
| Desktop default `/` → Board; phone default `/` → Sessions | Observed | **Conforms** |
| 248 px text-first rail with Work/Estate/Machine, running sessions and file tree | Present at desktop; width and grouping match closely | **Mostly conforms** |
| No emoji/pictographic nav | File tree/actions are heavily pictographic/emoji | **Does not conform** |
| Shared header grammar | Each surface composes title/status/actions differently; explanatory prose dominates | **Does not conform** |
| Chip/+filter interaction, not grids of selects | Board and Backlog retain large select grids | **Does not conform** |
| Board content-sized cards, three-line clamp, expand in place | Titles visibly ellipsized; expand affordance not evident | **Unproven / visibly weak** |
| Phone Board: one state at a time | All workflow states/cards remain in one vertical page | **Does not conform** |
| Backlog content-sized ranked rows with why/provenance | Desktop uses dense legacy table; phone first screen is filters | **Does not conform** |
| Both Backlog bulk transition and label modes | Not safely exercised; desktop selection checkboxes present | **Not verified here** |
| Unified Inbox over one server operation | UI states server ordering and exposes one coherent coverage set | **Conforms in presentation evidence** |
| Coverage distinguishes not-collected/empty/unavailable | Explicit `Not collected` and `Not available` seen | **Conforms** |
| Inbox evidence before action and typed reasons | Evidence precedes drift controls; reason inputs visible | **Conforms visually** |
| Phone Inbox actions in bottom sheet | Actions remain inline in rows/forms | **Does not conform** |
| Sessions workspace with list, panes and tools | Only minimal list/tool shell visible; no active pane composition | **Incomplete** |
| Waiting session prompt/one-tap response | No waiting fixture on live instance | **Not verified** |
| Four-place 60 px bottom bar with live counts | Four labelled routes present; counts absent | **Partial** |
| “Go to…” on every primary header | Visible on inspected phone primary routes | **Conforms** |
| Phone type/density and ≥44 px touch targets | Controls are touch-sized, but composition is desktop-first and useful content is pushed below the fold | **Does not conform** |
| Trust/freshness/coverage attached to answers | Strong on Inbox, Board and Backlog | **Mostly conforms** |
| No gradients | Main authenticated surfaces use flat token colours; login was not included in conformance captures | **Conforms for reviewed surfaces** |

## 5. What is genuinely delivered

The report should not overcorrect into saying nothing landed. The following are materially visible on the live stack:

1. **Places architecture:** stable desktop rail, stable product routes, recent places, running session section, Settings in the rail, phone primary routes.
2. **Narrow default and navigation:** root chooses Sessions on phone; the four labelled bottom places and visible “Go to…” work.
3. **Inbox contract presentation:** standalone Inbox, normalized source coverage, server-order claim, per-source honest absence, occurrence/trust/provenance, audited-reason affordances and evidence-before-action.
4. **Truth annotations:** Board and Backlog expose update/freshness/coverage and trust; observed and declared work are visibly distinct.
5. **No fake enforcement:** the UI describes server-owned workflow operations and reasons rather than implying the client decides compliance/workflow truth.

Those are meaningful outcomes. They justify “restructure in progress,” not “design delivered.”

## 6. Recommended acceptance disposition (no implementation prescription)

1. **Do not close Stage 9 or Stage 10.** The live screenshots directly fail Stage 9's one-state phone Board, action-sheet behaviour, live counts and final visual inspection.
2. **Keep FR-U23/U24/U25/T8/M5 open exactly as `docs/REQUIREMENTS.md` §7 currently does.** This review supports that gap register rather than contradicting it.
3. **Treat visual conformance as an acceptance boundary, not polish.** Specifically gate:
   - phone rendered scale at a real 390 px viewport/device;
   - one-state phone Board;
   - phone action sheets;
   - content-visible first viewport on Board, Backlog and Inbox;
   - designed Sessions row/card/pane composition;
   - removal of the legacy select-grid/table presentation from Board/Backlog;
   - live counts on phone places.
4. **Use the captured live screenshots as the “before” baseline** and compare future captures to the exported surfaces, not merely to component tests.
5. **Separate contractual and visual sign-off.** Inbox can be contractually strong while visually off-design; Sessions can be route-reachable while compositionally incomplete. Both statements need to remain true in status reporting.

## 7. Screenshot index

All paths are relative to this report.

### Desktop

- [Board](screenshots/desktop-board.png)
- [Backlog](screenshots/desktop-backlog.png)
- [Inbox](screenshots/desktop-inbox.png)
- [Sessions](screenshots/desktop-sessions.png)

### Phone

- [Sessions](screenshots/phone-sessions.png)
- [Inbox](screenshots/phone-inbox.png)
- [Board](screenshots/phone-board.png)
- [Backlog](screenshots/phone-backlog.png)
- [Projects](screenshots/phone-projects.png)

## 8. Source references used

- `design/README.md` — status of the export and authority caveat
- `design/restructure-2026-08/Vogt Design Guardrails.dc.html` — principles, type/density, desktop shell, mobile and Inbox rules
- `design/restructure-2026-08/Vogt.dc.html`
- `design/restructure-2026-08/BoardSurface.dc.html`
- `design/restructure-2026-08/BacklogSurface.dc.html`
- `design/restructure-2026-08/InboxSurface.dc.html`
- `design/restructure-2026-08/SessionsSurface.dc.html`
- `design/restructure-2026-08/Vogt Phone.dc.html`
- `docs/RESTRUCTURE.md` — Stages 3–10 and their done criteria
- `docs/REQUIREMENTS.md` — FR-U23, FR-U24, FR-U25, FR-T8, FR-M5 and the current §7 gap register

---

**Conclusion:** The concern is substantiated. The deployed instance is truthful about much of its data, but not yet truthful to the restructure as a delivered design. The largest gaps are not pixel-level differences: they are the retained legacy filter/table model, the missing Sessions composition, the phone Board's all-states page, inline phone actions, absent nav counts, and phone surfaces that remain desktop-first in composition.

## Addendum — left-hand Files sidebar is visibly broken

**Confirmed.** The original capture did record this, but the report understated it as “pictographic clutter.” The captured `desktop-sessions.png` shows the Files tree rendering as repeated rows of tiny broken/empty glyph boxes and ellipsized `…` content rather than readable filenames or folder names. In the 1440×1000 capture, the sidebar's Files section is visibly non-functional at a glance: the user cannot identify the workspace tree, and the rows appear as malformed icon/text output.

This is distinct from the design issue about emoji navigation:

- **Observed symptom:** file rows display as broken/blank glyphs and `…`, with no legible names in the captured viewport.
- **Expected design:** a readable, always-available file tree with named folders/files, search, and clearly bounded actions; the export includes concrete examples such as `docs`, `engine`, `src`, `web`, `DESIGN.md`, `REQUIREMENTS.md`, `Board.tsx` and `styles.css`.
- **Likely contributing implementation facts:** `FileTree.tsx` renders emoji icons from `fileIcons.ts`, uses a very compressed 12px tree style, and places multiple icon-only action buttons in each row. The live symptom should still be treated as a deployed rendering defect rather than inferred solely from source.
- **Severity:** **Critical for the Sessions/desktop shell**. The Files tree is a primary machine capability in the rail, not decoration. A broken tree undermines editor/file navigation and makes the claimed Sessions workspace materially misleading.

The screenshot evidence is now explicitly indexed as a broken Files sidebar finding; no code was changed.
