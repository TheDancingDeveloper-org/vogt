# UI parity gaps — surface header & rail vs `UI issues to solve(1).zip`

> **Status (2026-08-20): G1 and G2 implemented.** Decisions taken while
> implementing (see each gap for detail):
> - **G1a** Inbox unread → **solid accent badge** (`4a` rule 4), not `5b`'s red
>   outline.
> - **G1b** Projects drift → **amber-outlined badge**; the number stays the
>   project total, the amber is the attention signal, sourced from a **bounded
>   `drift.list({limit:1})` existence read** added as a fifth shell metric.
> - **G2** honesty → **inline bold freshness lead** (not a pill), scoped to the
>   two surfaces `5a` actually depicts (Board, Audit).
>
> Landed in `web/src/placeMetrics.ts`, `web/src/App.tsx` (`PlaceCount`),
> `web/src/Board.tsx`, `web/src/styles.css`. Full suite green (411 tests), tsc
> clean, production build clean.

**Date:** 2026-08-20
**Scope of this audit:** *only* the design bundle in
`design/UI issues to solve(1).zip`
(SHA-256 `b91147f48dbb397bbcfda570d2873ccd47731aa6fb93407091daf7028bddea87`,
2,301,046 bytes), extracted to
`design/design_handoff_surface_header_and_rail/`. All other material under
`design/` (notably `design/restructure-2026-08/`) is **out of scope** for this
pass, by direction.

**Reference under test:** the deployed `vogt-dev` build at
<https://vogt-dev.sprooty.com/> running `vogt-stack@sha256:648230ae…` /
`vogt@sha256:eb6a2518…` (commit `472a66f`), verified live via Playwright against
the actual deployed JS/CSS bundle.

---

## 0. Findings that are *not* gaps (verified faithful)

These were suspected but confirmed **implemented, deployed, and correct** — they
were only invisible because `vogt-dev` had calm data at review time:

| Item | Where | Evidence |
| --- | --- | --- |
| B1 attention card (waiting / engine outage) | `web/src/App.tsx:1087–1098`; CSS `web/src/styles.css:8433` | Injecting a synthetic `waiting-for-input` session into `GET /api/sessions` made the deployed bundle render **"1 session waiting · claude cadastre · cadastre · 40s ago"** with the red pulsing dot. Guards (`!isConnected()` for outage, `waitingSessions() > 0` for waiting) match spec §B1. |
| B2 always-visible `.row-menu` (`24×24`, `32×32` coarse) | `web/src/App.tsx` session rows | Visible on every session row in the forced-session capture; no `.row-btn` hover-reveal remains. |
| B3 collapsible sections, **Files collapsed by default** | rail `.places-section-toggle`; state `mydevenv2.rail.sections.v1` | Running/Recent/Files render as `▾/▸` toggles; Files loads collapsed. |
| A1 honesty **tone border** | `.surface-header-honesty` | DOM computed `border-left: 3px rgb(63,185,80)` (`--activity-done`), `padding-left: 8px`; amber/red variants present in bundle. |
| A2 one accent action in the `action` slot | header | "Quick create" is the only `--accent`-filled control; nothing in `controls` is accent-filled. |
| A3 segmented view tabs | `.surface-header-tabs` | Audit header renders `.surface-header-tabs` with `aria-pressed` "Audit trail / Notifications", active tab accent-filled. |
| The zip **file import** itself | `design/design_handoff_surface_header_and_rail/` | `unzip` + `diff -rq` against the repo folder is **byte-identical**. The import was faithful; the divergence (below) is in code, not import. |

---

## 1. The controlling contradiction (read before "fixing" anything)

The zip is **internally inconsistent** about rail count colour:

- **Every build-target screenshot** — `3b`, `3d`, `4a`, `5b` — colours the
  counts: Inbox unread as a badge, Projects drift amber, Sessions waiting red.
  `4a`'s "TEN RULES THAT KEEP FAILING", rule 4, states it explicitly:
  > *Three count weights only: muted number = informational · **solid accent =
  > unread (Inbox)** · **outlined `--activity-running` = drift (Projects)**.
  > Nothing else gets a badge.*
- **The written source of truth**, `rail-spec.md` §1, **withdraws** exactly this:
  > *4a R4/R5 counts and `—` → `placeMetrics.ts` + `.place-count[data-state]` →
  > **Withdrawn** — theirs separates stale from not read…*
  and §3 (the changes to build) lists only B1/B2/B3 — **no count colouring**.

The shipped implementation followed the **prose** (counts are grey `--fg-muted`
in the `ready` state; only `loading/stale/unavailable` get non-grey treatment).
That is why the rail reads as "zero colours" against the **pictures**.

**Direction for this pass:** parity to the **mock** (the pictures). The gaps
below are therefore treated as real work, overriding the withdrawn-prose.

Even within the mocks there is a **sub-conflict on Inbox**: `4a` rule 4 and the
`3b`/`3d`/`4a` screenshots show Inbox as a **solid blue (accent)** badge, while
the `5b` `.dc.html` markup colours it **red-outlined** (`#f85149`). This needs a
human decision (see Gap G1).

---

## 2. Gaps

### G1 — Rail count colours are missing (the "zero colours" report) — **HIGH**

**Observed:** all nav counts render grey (`color: rgb(139,148,158)` =
`--fg-muted`, `data-state="ready"`). Confirmed on Board load: Board 16 /
Backlog 164 / Inbox 84 / Projects 38 all grey. Sessions-waiting **does** turn
red when a session waits (so the mechanism is partly present).

**Design (rule 4 + screenshots):**

| Place | Design treatment | Source of the number |
| --- | --- | --- |
| Board, Backlog | muted grey number (informational) | already correct |
| **Inbox** | unread → **solid accent badge** (`4a`/`3b`/`3d`), *or* red-outline (`5b`) — **conflict, needs decision** | `placeMetrics` `inbox.counts.active` (already fetched client-side) |
| **Projects** | **outlined `--activity-running` (amber) "drift" badge** | ⚠️ **drift count is not fetched today** — needs a new read, or fall back to `project.list.total` styled amber only when drift > 0 |
| Audit | red `—` when unavailable | `.place-count[data-state="unavailable"]` exists; wire it |
| Sessions | red "N waiting" | already works |

**Files:** `web/src/App.tsx` (nav rendering ~1099+, `PlaceCount`), `web/src/placeMetrics.ts` (state model `loading|ready|stale|unavailable`), `web/src/styles.css` (`.place-count[data-state]` ~431).

**Blocking decisions:**
- **G1a.** Inbox colour: **solid blue accent** (rule 4) or **red outline** (5b)?
- **G1b.** Projects "drift": add a real drift read, or approximate from the
  existing project total? (No drift count is currently on the client.)

**Note:** implementing G1 means adding an *attention* dimension to counts that
`placeMetrics` deliberately does **not** model today (it models freshness only).
This is a data-model change, not just CSS.

### G2 — Header honesty does not lead with the bold freshness age — **MEDIUM**

**Observed (Board):** honesty slot reads
`17 loaded · of 17 matching · 6 columns` followed by a green **pill**
"Polling — updated 1s ago". The bold lead clause is absent; freshness is a
trailing pill (the withdrawn `1d` grammar).

**Design (5a / A1):** honesty leads with a **bold age clause**
(`<strong>Live · answered 3s ago</strong>`), then `·`-separated qualifiers
(`48 of 213 loaded · 5 columns`) — inline, tone carried by the left border, **no
pill**. Extra freshness paragraphs move into the `detail` disclosure.

**Files:** `web/src/Board.tsx` (`.board-summary`, ~1859 per handoff),
`web/src/SurfaceHeader.tsx` (honesty slot; **no change to slot order** — its test
must stay green), `web/src/styles.css` (`.surface-header-honesty*`).

**Decision:** keep the polling pill as well, or replace it with the inline
bold-age lead to match 5a exactly?

---

## 3. Explicitly *not* gaps (design items the spec withdrew)

Do not re-open these unless directed — the zip marks them withdrawn (`rail-spec.md`
§1, `5a`/`5b` notes):

- Two-band title bar + separate honesty strip (2a/1c) — `SurfaceHeader` slot
  order is test-asserted.
- 30px control floor — dev ships 32px (44px coarse).
- Fixed 248px rail — replaced by resize handle + collapse/reopen (width is user
  state).
- `.rail-tabbar` — replaced by `.phone-bottom-nav`.
- Identity subtitle `node-b · actor:tim` — invented; `identity.ts` has no such
  value.
- Files tree living in the rail as a primary region — Files is collapsed; the
  editor workspace owns the real tree.

The `MACHINE` nav group (Sessions/Git/History/Tasks) present live is from the
exploration (3b/3d/4a) and is additive, not a regression.

---

## 4. Suggested work order (if approved)

1. Resolve G1a (Inbox colour) and G1b (Projects drift source) — **needs human**.
2. G1: extend `placeMetrics` / count rendering with the attention dimension;
   add `.place-count` accent/amber/red variants; keep existing
   `data-state`/`aria-label` markup so `shell.test.tsx` stays green.
3. G2: give the honesty slot its bold-age lead (per-surface `.board-summary`
   etc.); decide pill vs inline.
4. Add acceptance assertions (rail-spec §5 style) **before** wiring behaviour;
   existing `surfaceHeader.test.tsx` / `shell.test.tsx` / `placeMetrics.test.ts`
   must pass unchanged.
5. Rebuild image, redeploy `vogt-dev` (bump `VOGT_STACK_IMAGE` digest in the
   Komodo stack env), re-verify with live screenshots.
