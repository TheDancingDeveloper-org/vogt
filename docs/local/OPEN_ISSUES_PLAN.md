# Open GitHub Issues — Cohesive Delivery Plan

**Snapshot date:** 2026-08-18  
**Repository:** [TheDancingDeveloper-org/vogt](https://github.com/TheDancingDeveloper-org/vogt)  
**Scope:** All 53 issues open when this review was performed

This document is a planning aid derived from the live GitHub tracker. It is not
a requirements or architecture source of truth. Requirements remain in
`docs/REQUIREMENTS.md`, architecture remains in `docs/DESIGN.md`, and milestone
definitions remain in `docs/ROADMAP.md`. Reconcile this snapshot with GitHub
before using it after the date above.

## Summary

The tracker contains 49 issues carrying only the generic `bug` label and four
unlabelled issues. None has a milestone or assignee, and 51 of the 53 issues
were opened on 2026-08-18. Despite that apparent volume, the issues form a
smaller set of coherent work packages with shared implementation foundations.

The plan below assigns every open issue to exactly one primary package. Some
issues also act as acceptance criteria for earlier packages; those dependencies
are called out explicitly rather than duplicating the issue.

## Recommended delivery plan

### 0. Operational safeguards

Issues: [#56](https://github.com/TheDancingDeveloper-org/vogt/issues/56),
[#35](https://github.com/TheDancingDeveloper-org/vogt/issues/35)

- Pin shipped migration IDs before making further schema changes (#56).
- Extract one JSON-RPC-aware MCP health probe and use it for both Vogt and
  Cadastre (#35).
- Keep these in one milestone but deliver them as separate PRs: they share
  urgency, not implementation.

### 0. Contract adoption (parallel domain workstream)

Issue: [#39](https://github.com/TheDancingDeveloper-org/vogt/issues/39)

Treat this as a standalone Python/domain epic:

1. Deliver FR-G16 contract adoption state and `not_applicable` reporting.
2. Add the FR-G17 existing-project scaffold operation.
3. Add FR-G18 advisory recommendations.
4. Add FR-G19 audited criterion inapplicability.

Land #56 before any schema migration required by this work. Every new operation
must retain CLI, REST, and MCP parity through the operation registry.

### 1. Dialog and feedback foundations

Issues: [#96](https://github.com/TheDancingDeveloper-org/vogt/issues/96),
[#97](https://github.com/TheDancingDeveloper-org/vogt/issues/97),
[#98](https://github.com/TheDancingDeveloper-org/vogt/issues/98),
[#99](https://github.com/TheDancingDeveloper-org/vogt/issues/99)

Build the reusable interaction primitives before fixing each consuming surface:

1. Create the accessible dialog primitive with naming, focus containment,
   dismissal policy, and focus restoration (#96).
2. Replace native confirm/alert usage across Files, Git, History, Tasks, and GUI
   with that primitive (#98).
3. Replace the single toast with a severity-aware, keyed queue that preserves
   critical failures and offers actions (#99).
4. Give visible feedback matching live-region semantics and ordered accessible
   announcements (#97).

This package is a prerequisite for the command palette, Inbox phone action
sheet, and dirty-draft protection.

### 2. Route truth and navigation state

Issues: [#77](https://github.com/TheDancingDeveloper-org/vogt/issues/77),
[#80](https://github.com/TheDancingDeveloper-org/vogt/issues/80),
[#81](https://github.com/TheDancingDeveloper-org/vogt/issues/81),
[#95](https://github.com/TheDancingDeveloper-org/vogt/issues/95),
[#100](https://github.com/TheDancingDeveloper-org/vogt/issues/100)

Introduce one route descriptor/model that owns:

- the current primary place and selected Sessions tool;
- configured, unavailable, and not-found outcomes;
- desktop and phone active-navigation state;
- `aria-current` semantics;
- the invoking/return route for modal-style routes such as Settings.

Use it to hide the unverified GUI stream while retaining a truthful direct-link
outcome (#77), render explicit missing-terminal and disabled-Assistant states
(#80/#81), identify the current place consistently (#95), and return from
Settings without losing context (#100).

### 3A. Sessions and editor lifecycle

Issues: [#70](https://github.com/TheDancingDeveloper-org/vogt/issues/70),
[#76](https://github.com/TheDancingDeveloper-org/vogt/issues/76),
[#79](https://github.com/TheDancingDeveloper-org/vogt/issues/79),
[#82](https://github.com/TheDancingDeveloper-org/vogt/issues/82),
[#103](https://github.com/TheDancingDeveloper-org/vogt/issues/103)

Recommended order:

1. Protect dirty editor buffers across browser/PWA lifecycle exits (#82).
2. Repair atomic terminal splitting, including cleanup on failure (#79).
3. Define retention by tab kind: terminals remain live where required while
   safe non-terminal tools suspend or unmount (#103).
4. Build Sessions as the owner of pane state and machine tools while retaining
   every existing deep link (#70).
5. Stop terminal font scaling from stealing the browser's normal zoom gesture
   and verify the whole shell across zoom levels (#76).

The retention policy and Sessions composition should be designed together;
otherwise #70 risks entrenching #103.

### 3B. Truthful machine-tool state

Issues: [#53](https://github.com/TheDancingDeveloper-org/vogt/issues/53),
[#78](https://github.com/TheDancingDeveloper-org/vogt/issues/78),
[#86](https://github.com/TheDancingDeveloper-org/vogt/issues/86),
[#84](https://github.com/TheDancingDeveloper-org/vogt/issues/84),
[#101](https://github.com/TheDancingDeveloper-org/vogt/issues/101),
[#85](https://github.com/TheDancingDeveloper-org/vogt/issues/85),
[#83](https://github.com/TheDancingDeveloper-org/vogt/issues/83)

Deliver this package as three vertical PRs sharing a common
loading/empty/error/stale/retry presentation model:

- **Git:** classify an empty repository selection without a 500 (#53), provide
  a registered-project repository chooser without filesystem discovery (#78),
  and retain explicit local errors for status, branches, log, and diff (#86).
- **History:** distinguish read failure from an empty archive (#84), then add
  bounded continuation beyond the first 200 sessions (#101).
- **Agent Tasks:** make failures persistent and recoverable (#85), then protect
  dirty drafts across selection, refresh, and route changes (#83).

Errors remain local to the panel whose read or action failed; global feedback
supplements them but must not be the only evidence.

### 3C. Command palette

Issues: [#87](https://github.com/TheDancingDeveloper-org/vogt/issues/87),
[#88](https://github.com/TheDancingDeveloper-org/vogt/issues/88),
[#89](https://github.com/TheDancingDeveloper-org/vogt/issues/89),
[#90](https://github.com/TheDancingDeveloper-org/vogt/issues/90),
[#91](https://github.com/TheDancingDeveloper-org/vogt/issues/91),
[#102](https://github.com/TheDancingDeveloper-org/vogt/issues/102)

1. Implement the palette as an accessible modal combobox/listbox using the
   shared dialog foundation (#87/#89).
2. Centralize shortcut registration so every advertised binding works in its
   declared contexts, including `?` help (#88).
3. Split providers into immediate static commands and cached, cancellable
   dynamic providers with explicit invalidation (#102).
4. Make New File and Open File enter real, distinct workflows or remove them
   until supported (#90).
5. Make History results navigate to a qualified session, query, and match
   rather than generic History (#91).

### 3D. Places rail and live steering

Issues: [#57](https://github.com/TheDancingDeveloper-org/vogt/issues/57),
[#58](https://github.com/TheDancingDeveloper-org/vogt/issues/58),
[#59](https://github.com/TheDancingDeveloper-org/vogt/issues/59),
[#60](https://github.com/TheDancingDeveloper-org/vogt/issues/60),
[#72](https://github.com/TheDancingDeveloper-org/vogt/issues/72),
[#94](https://github.com/TheDancingDeveloper-org/vogt/issues/94)

1. Establish one coherent rail scrolling/sizing policy and retain a usable
   Files minimum under crowded session fixtures (#59).
2. Repair Files hierarchy and progressive controls (#60).
3. Restore labelled attention, connection, and file status indicators (#58).
4. Make session rows semantic, keyboard-operable, and focus-visible (#94).
5. Derive desktop rail and phone navigation counts from shared live selectors,
   with zero/loading/unavailable distinctions (#57/#72).

### 4A. Shared surface grammar and Inbox

Issues: [#74](https://github.com/TheDancingDeveloper-org/vogt/issues/74),
[#69](https://github.com/TheDancingDeveloper-org/vogt/issues/69),
[#68](https://github.com/TheDancingDeveloper-org/vogt/issues/68)

1. Build a shared semantic surface header with explicit title,
   honesty/freshness, controls, action, and detail slots (#74).
2. Apply it to Inbox's loading, empty, partial, stale, unavailable, selection,
   and batch states without flattening their meaning (#69).
3. On phone, replace eager inline operations with source pills and an
   accessible action sheet while preserving evidence-before-action and typed
   reasons (#68).

### 4B. Board and Backlog

Issues: [#61](https://github.com/TheDancingDeveloper-org/vogt/issues/61),
[#62](https://github.com/TheDancingDeveloper-org/vogt/issues/62),
[#63](https://github.com/TheDancingDeveloper-org/vogt/issues/63),
[#64](https://github.com/TheDancingDeveloper-org/vogt/issues/64),
[#65](https://github.com/TheDancingDeveloper-org/vogt/issues/65),
[#66](https://github.com/TheDancingDeveloper-org/vogt/issues/66)

Recommended order:

1. Replace Board's capped estate load with bounded, server-owned, snapshot-
   stable per-cell pages (#63). Any new read operation must enter the Python
   registry and retain CLI/REST/MCP parity.
2. Build shared progressive filter-chip and named-lens machinery for Board and
   Backlog (#61/#65), retaining exact URL state.
3. Share measured expansion/window invalidation machinery between Board cards
   and Backlog rows (#64/#66).
4. Add the phone Board's one-workflow-state-at-a-time presentation over the
   bounded data model (#62).

Do not treat DOM virtualization as completing #63: NFR-S5 also requires bounded
data reads.

### 5. Mobile conformance

Issues: [#67](https://github.com/TheDancingDeveloper-org/vogt/issues/67),
[#71](https://github.com/TheDancingDeveloper-org/vogt/issues/71),
[#75](https://github.com/TheDancingDeveloper-org/vogt/issues/75),
[#92](https://github.com/TheDancingDeveloper-org/vogt/issues/92),
[#93](https://github.com/TheDancingDeveloper-org/vogt/issues/93)

1. Establish global phone control tokens: at least 44-by-44-pixel hit areas and
   at least 16-pixel form text (#92/#93).
2. Promote waiting sessions and provide target-scoped `y + Enter` and `Ctrl-C`
   terminal input actions (#71).
3. Contain every secondary route within the phone shell without horizontal
   overflow or unrelated placeholder content (#75).
4. Treat first-useful-content visibility across Sessions, Inbox, Board, and
   Backlog as the final cross-surface acceptance check (#67).

Issue #67 should not be fixed as an isolated CSS compression exercise. It
depends on the surface-header, filter, Board, Inbox, and Sessions work above.

### 6. PWA release quality

Issues: [#104](https://github.com/TheDancingDeveloper-org/vogt/issues/104),
[#105](https://github.com/TheDancingDeveloper-org/vogt/issues/105)

- Measure bundle and route costs early, but set the final budgets and lazy-load
  boundaries after the Sessions and route architecture settles (#104). Ensure
  non-editor routes do not fetch editor workers and preserve offline behavior.
- Adopt one user-facing Vogt identity across the document title, manifest,
  login/error copy, push metadata, icons, and installed-app upgrade behavior
  (#105). Derive route-aware titles from the shared route model.

Use separate PRs under one release-readiness milestone.

### Final acceptance: restructure browser coverage

Issue: [#73](https://github.com/TheDancingDeveloper-org/vogt/issues/73)

Treat #73 as the parent acceptance epic, not as a late PR that recreates all
browser tests at once. Every package above must add the behavioral and geometry
regressions it owns. Close #73 only after the combined suite covers:

- the complete Stage 3 route matrix;
- crowded desktop rail and Files reachability;
- Board and Backlog filtering, continuation, measured expansion, and phone
  state selection;
- Inbox coverage, progressive actions, and evidence-before-action;
- Sessions composition, waiting prompts, and terminal input;
- all four phone navigation counts;
- representative mobile target, font-size, containment, and first-viewport
  geometry.

## Dependency spine

```text
#56 ----------------------> #39
#96-#99 ------------------> #83, #89, #68
#77/#80/#81/#95/#100 ----> #70 ----> #75
#63 + #61-#66 + #74/#69 -> #67
all GUI packages ---------> #73
#70/#102/#103 -----------> final #104 budgets
```

## Suggested milestones

1. **Safeguards and foundations** — #35, #56, #96-#100.
2. **Sessions and machine tools** — #53, #70, #76-#91, #101-#103.
3. **M11 surface conformance** — #57-#66, #69, #72-#74, #94-#95.
4. **M13 mobile conformance** — #67-#68, #71, #75, #92-#93.
5. **PWA release readiness** — #104-#105, with #73 as the final acceptance
   gate spanning the GUI milestones.
6. **Contract adoption** — #39, sequenced independently after #56.

Milestones should express delivery outcomes, while individual issues retain
their existing acceptance boundaries. Multi-issue PRs are appropriate only
where the implementation is genuinely shared.

## Items to pull forward

The following carry production, data-loss, misleading-state, or boot-safety
risk and should not wait for the broader restructure sequence:

- #35 — false-green Cadastre credential probe;
- #53 — ordinary empty Git selection produces HTTP 500;
- #56 — applied migration identities are not protected in CI;
- #77 — non-functional GUI streaming is advertised as available;
- #79 — terminal split is broken;
- #82 — dirty editor buffers can be lost on browser/PWA exit;
- #84-#86 — outages are presented as valid empty data.

## Coverage of the snapshot

Every issue open at review time appears once as a primary item in this plan:

```text
35, 39, 53, 56, 57, 58, 59, 60, 61, 62, 63, 64, 65, 66, 67, 68, 69,
70, 71, 72, 73, 74, 75, 76, 77, 78, 79, 80, 81, 82, 83, 84, 85, 86,
87, 88, 89, 90, 91, 92, 93, 94, 95, 96, 97, 98, 99, 100, 101, 102,
103, 104, 105
```
