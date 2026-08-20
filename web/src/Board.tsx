// The board (FR-U4), with the interaction contract it is only honest with:
// live-ish (FR-U10), addressable (FR-U11), optimistic and server-authoritative
// (FR-U12), and as much of FR-U13 as columns and lanes need to be usable.
//
// Five decisions a future reader will want the reasoning for:
//
//  1. **Columns are derived, never written down.** `workflow.list` publishes
//     one machine per kind; the columns are the union of those machines'
//     states, ordered by walking each machine from its initial state and
//     pushing states that only lead back to it (the finished ones) to the
//     end. Edit a workflow and the board changes shape on the next load.
//     A state that appears on a loaded item but in no machine still gets a
//     column, marked as such — hiding work because the workflow moved on is
//     worse than admitting the machine and the data disagree.
//
//  2. **The drop collects the reason, at the drop site.** FR-W1 (and r6's
//     restatement) says a mutating operation appears only through a view
//     that collects a reason the user typed; a drag cannot type one. So the
//     drop does not write. It moves the card optimistically into the target
//     column, marks it unsaved, and opens a one-line composer *in that
//     column* — pre-filled with the last reason this session accepted, so a
//     triage run is drag, Enter, drag, Enter, but every move still passes
//     through the place where the reason is visible and editable. Empty
//     reason, disabled button: a drop with no reason cannot submit. Escape
//     rolls the card back. The composer's slot is also where a refusal
//     lands, so the answer arrives where the question was asked.
//
//  3. **The server decides, and says why.** The workflow's edges are used
//     for a hint while dragging ("not a listed edge") and for nothing else:
//     every drop is attempted, and an illegal one bounces with Vogt's own
//     sentence (`transition.not_allowed: bug has no open -> done edge
//     (allowed from open: ...)`), rendered where the drop happened. On a
//     refusal the optimistic position is discarded outright — it is never
//     written to storage, never merged into the item list, and never
//     re-derived (FR-U12's last sentence). The only thing this file
//     persists is which columns and lanes the user collapsed.
//
//  4. **Live, and honest about the difference.** The front door follows
//     vogt-core's `events.list` cursor and republishes each change onto the
//     engine's SSE stream as `vogt-changed` (FR-U10), so a transition
//     somebody else made arrives here rather than waiting for a poll. The
//     poll stays as the floor: a stream can drop, and a board that stopped
//     refreshing because a socket died would be stale while looking
//     current, which is the failure the requirement is actually about. The
//     view still reports its own age and never calls itself current when it
//     is not.
//
//  5. **Bounded reads.** `board.list` fixes one server snapshot, returns exact
//     cell/column totals, and pages each cell independently. The first batch is
//     bounded by visible cells and their overscan; a cell asks for its opaque
//     continuation only as its measured window approaches the loaded end.
//     A live change starts a fresh snapshot instead of mixing revisions.
//
//  6. **The columns window; they do not truncate, and they are projected
//     once.** Two halves of NFR-S5, and they are the same decision.
//
//     A column used to draw its first 60 cards and count the rest into a
//     "+N more" line. That is a truncation that admits it is truncating,
//     which is better than a silent one and still worse than not truncating:
//     the reader asked for a filter, the filter matched, and the 61st match
//     was withheld with an instruction to narrow the filter until the tool
//     agreed to show it. So the cell now *windows* — the same mechanism
//     `Backlog.tsx` uses and for the same reason, because a second idea of
//     what virtualization means on one product is two things to keep true:
//     a measured card height, a `ResizeObserver` on the cell's own scroller,
//     and a slice of the list with overscan either
//     side. Everything the filter matched is reachable, in the order it
//     matched; only what is on screen is in the DOM.
//
//     Windowing is what made the projection's shape matter. The cards a cell
//     draws used to be a `filter` over the lane, run once per cell, and the
//     WIP count used to be a `filter` over the *whole loaded set*, run once
//     per column head **and once per cell** — so a board with 2,000 items,
//     eight columns and twenty lanes did about 340,000 comparisons per pass,
//     and did them all again on every keystroke in the move composer,
//     because the composer's reason lives in the same signal as the card's
//     optimistic position. `projectBoard` below walks the loaded set once
//     and returns every cell, every count and every card's index; the cells
//     read a `Map`. Adding a column or a lane no longer adds a pass, and the
//     composer no longer re-projects at all — `placement` is a memo over the
//     part of the pending move a projection can see, so typing a reason is
//     not a change to where the card is.

import {
  Component,
  For,
  Show,
  createEffect,
  createMemo,
  createSignal,
  onCleanup,
  onMount,
} from "solid-js";
import { useLocation, useNavigate, useSearchParams } from "@solidjs/router";
import { ApiError } from "./api";
import { openWorkItemTab } from "./tabs";
import { ViewAgeBadge, createViewAge, onVogtLive } from "./viewAge";
import { MeasuredWindow } from "./measuredWindow";
import SurfaceHeader from "./SurfaceHeader";
import { ProgressiveFilters, SavedLenses } from "./ProgressiveFilters";
import {
  VogtUnavailable,
  createWork,
  listActors,
  listBoard,
  listInitiatives,
  listLabels,
  listProjects,
  listWorkflows,
  transitionWork,
  type BoardCellPage,
  type WorkItem,
  type Workflow,
  type WorkflowState,
} from "./vogtApi";

interface Props {
  onError?: (message: string) => void;
}

/** Grouping for swimlanes (FR-U13). */
type LaneMode = "none" | "project" | "initiative";

/** Rows fetched per visible cell. The server caps this independently too. */
const CELL_PAGE_SIZE = 30;
const INITIAL_LANE_PAGES = 2;

/** Layout-free seed only; measured cards replace it as soon as they render. */
export const CARD_ESTIMATE = 112;

/**
 * Below this many cards a cell draws whole.
 *
 * `Backlog.tsx`'s reasoning, unchanged: windowing costs the browser's own
 * find-in-page and costs a reader the ability to select across the list, and
 * that is worth paying at length and not worth paying for a screenful. This
 * is the number that used to be the *cap* — the same threshold, now the
 * point at which the column starts windowing instead of the point at which
 * it stopped showing you things.
 */
const VIRTUALIZE_ABOVE = 60;

/** Pixel overscan, not a number of rows. */
const OVERSCAN_PX = 320;
const DEFAULT_VIEWPORT_PX = 480;
const CARD_SPACING_PX = 8;

const POLL_CHOICES = [10, 20, 60, 0] as const;
const DEFAULT_POLL_SECONDS = 20;

const LAYOUT_KEY = "mydevenv2.boardLayout.v1";

// -- filters, which are the URL (FR-U11) ------------------------------------

interface Filters {
  project: string;
  kinds: string[];
  states: string[];
  label: string;
  initiative: string;
  assignee: string;
  lanes: LaneMode;
  /** Phone presentation state. This selects a visible column, not a filter. */
  column: string;
  /** Seconds between polls; 0 means the user paused it. */
  poll: number;
}

interface BoardParams {
  project?: string;
  kind?: string | string[];
  state?: string | string[];
  label?: string;
  initiative?: string;
  assignee?: string;
  lanes?: string;
  column?: string;
  poll?: string;
  [key: string]: string | string[] | undefined;
}

function asList(value: string | string[] | undefined): string[] {
  if (value === undefined) return [];
  const all = Array.isArray(value) ? value : [value];
  return all.map((one) => one.trim()).filter(Boolean);
}

export function asLaneMode(value: string | undefined): LaneMode {
  return value === "project" || value === "initiative" ? value : "none";
}

export function asPoll(value: string | undefined): number {
  if (value === undefined) return DEFAULT_POLL_SECONDS;
  if (value === "off") return 0;
  const parsed = Number.parseInt(value, 10);
  if (!Number.isFinite(parsed) || parsed < 0) return DEFAULT_POLL_SECONDS;
  return parsed;
}

/** The URL keys this surface owns. Anything else in the query is left alone. */
const URL_KEYS = [
  "project",
  "kind",
  "state",
  "label",
  "initiative",
  "assignee",
  "lanes",
  "column",
  "poll",
] as const;

export function filtersFromQuery(query: BoardParams): Filters {
  return {
    project: (query.project ?? "").trim(),
    kinds: asList(query.kind),
    states: asList(query.state),
    label: (query.label ?? "").trim(),
    initiative: (query.initiative ?? "").trim(),
    assignee: (query.assignee ?? "").trim(),
    lanes: asLaneMode(query.lanes),
    column: (query.column ?? "").trim(),
    poll: asPoll(query.poll),
  };
}

/** `null` clears a key; every key this surface does not own is left alone. */
function queryFor(
  active: Filters,
): Record<(typeof URL_KEYS)[number], string | string[] | null> {
  return {
    project: active.project || null,
    kind: active.kinds.length ? active.kinds : null,
    state: active.states.length ? active.states : null,
    label: active.label || null,
    initiative: active.initiative || null,
    assignee: active.assignee || null,
    lanes: active.lanes === "none" ? null : active.lanes,
    column: active.column || null,
    poll:
      active.poll === DEFAULT_POLL_SECONDS
        ? null
        : active.poll === 0
          ? "off"
          : String(active.poll),
  };
}

/** The canonical text of this surface's slice of the query.
 *
 *  Both encoders walk `URL_KEYS` in the same order, so two equal states always
 *  produce the same string — which is what lets one effect tell "the user
 *  changed a filter" from "somebody handed us a different URL". `Backlog.tsx`
 *  reasons this out at length; the board needs it for the same reason and did
 *  not have it, which is why a link pasted into a mounted board used to be
 *  ignored and a tab switch used to drop the query for good. */
export function encodeFilters(active: Filters): string {
  const params = new URLSearchParams();
  const desired = queryFor(active);
  for (const key of URL_KEYS) {
    const value = desired[key];
    if (value === null) continue;
    for (const one of Array.isArray(value) ? value : [value]) params.append(key, one);
  }
  return params.toString();
}

export function encodeQuery(query: BoardParams): string {
  const params = new URLSearchParams();
  for (const key of URL_KEYS) {
    const value = query[key];
    if (value === undefined) continue;
    for (const one of Array.isArray(value) ? value : [value]) params.append(key, one);
  }
  return params.toString();
}

/** A query string back into the shape `filtersFromQuery` reads. */
export function queryFromSearch(search: string): BoardParams {
  const params = new URLSearchParams(search);
  // Built as a plain record and handed back as `BoardParams`: writing through
  // the named keys asks TypeScript for the intersection of their types, and
  // `kind` is a list where `project` is not.
  const out: Record<string, string | string[]> = {};
  for (const key of URL_KEYS) {
    const all = params.getAll(key);
    if (!all.length) continue;
    out[key] = key === "kind" || key === "state" ? all : (all[0] as string);
  }
  return out as BoardParams;
}

// -- saved filters (FR-U14) -------------------------------------------------
//
// Per-client state in v2: server-side shared filters are deferred by name in
// `REQUIREMENTS.md` §3, so this is `localStorage` and the row on screen says
// so. The backlog and bugs views have had this since M11; the board — which
// FR-U14 names first, and whose filter set is the more elaborate of the two —
// had none.
//
// **A saved filter is stored as its query string.** Not as an object: the
// encoder is already canonical, already the thing a link carries, and already
// the thing `filtersFromQuery` can read back. So "save this view" and "copy
// this link" preserve exactly the same set, a stored filter written by an
// older build decodes under the same tolerances a pasted URL gets, and a key
// this build stopped understanding is dropped rather than resurrected as
// `undefined`.

const SAVED_FILTERS_KEY = "mydevenv2.boardFilters.v1";

/** Enough that a runaway list cannot be saved into a full storage quota. */
const MAX_SAVED_FILTERS = 40;

export interface SavedFilter {
  name: string;
  /** The filter set, encoded exactly as the URL encodes it. */
  query: string;
}

export function readSavedFilters(): SavedFilter[] {
  try {
    const raw = window.localStorage.getItem(SAVED_FILTERS_KEY);
    if (!raw) return [];
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed
      .map((entry) => {
        const record = (entry ?? {}) as Record<string, unknown>;
        return {
          name: typeof record.name === "string" ? record.name : "",
          query: typeof record.query === "string" ? record.query : "",
        };
      })
      .filter((entry) => entry.name);
  } catch {
    return [];
  }
}

function writeSavedFilters(entries: SavedFilter[]): SavedFilter[] {
  const next = entries.slice(0, MAX_SAVED_FILTERS);
  try {
    window.localStorage.setItem(SAVED_FILTERS_KEY, JSON.stringify(next));
  } catch {
    // Private mode, quota, or no storage at all: the in-memory list still
    // recalls for this session.
  }
  return next;
}

/** What a saved filter set says it is, for the recall button's tooltip. */
export function describeFilters(active: Filters): string {
  const parts: string[] = [];
  if (active.project) parts.push(`project ${active.project}`);
  if (active.kinds.length) parts.push(`type ${active.kinds.join("/")}`);
  if (active.states.length) parts.push(`state ${active.states.join("/")}`);
  if (active.label) parts.push(`label ${active.label}`);
  if (active.initiative) parts.push(`initiative ${active.initiative}`);
  if (active.assignee) parts.push(`assignee ${active.assignee}`);
  if (active.lanes !== "none") parts.push(`lanes by ${active.lanes}`);
  return parts.length ? parts.join(" · ") : "no filters — the whole board";
}

/**
 * The value of a `<select>` whose options arrive asynchronously.
 *
 * Reading the option list is load-bearing rather than decorative, and this is
 * `Backlog.tsx`'s helper moved to the surface that needed it just as much.
 * Solid compiles `value={...}` into an effect that re-applies when *its own*
 * dependencies change; a value applied before its `<option>` exists is
 * silently dropped by the browser, and the control then reads "All projects"
 * while a project filter is in force — which is a deep link restoring the
 * query and lying about it (FR-U11).
 */
function optionValue(current: string, options: readonly string[]): string {
  return options.find((option) => option === current) ?? current;
}

// -- workflow shapes --------------------------------------------------------
//
// Everything from here to `columnsFor` is pure, and exported for that reason:
// the shape of the board is the part most worth testing and the part least
// needing a browser. `web` has no test runner today (adding one means touching
// `package.json`, which this branch may not), so these are a seam rather than
// a suite — see the report.

function stateName(state: WorkflowState | string): string {
  return typeof state === "string" ? state : state.name;
}

/** The machine's edges, whichever shape the server sent.
 *
 * `workflow.list` publishes `transitions` as `{from: [to, ...]}`; the client's
 * declared type says `{from, to}[]`. Both are read here rather than trusting
 * either, because a wrong guess would silently reorder every column. The
 * mismatch is reported against `vogtApi.ts`, which this file may not edit.
 */
export function edgesOf(workflow: Workflow): Record<string, string[]> {
  const raw: unknown = workflow.transitions;
  const out: Record<string, string[]> = {};
  if (!raw || typeof raw !== "object") return out;
  if (Array.isArray(raw)) {
    for (const edge of raw) {
      if (!edge || typeof edge !== "object") continue;
      const { from, to } = edge as { from?: unknown; to?: unknown };
      if (typeof from !== "string" || typeof to !== "string") continue;
      (out[from] ??= []).push(to);
    }
    return out;
  }
  for (const [from, targets] of Object.entries(raw as Record<string, unknown>)) {
    if (!Array.isArray(targets)) continue;
    out[from] = targets.filter((one): one is string => typeof one === "string");
  }
  return out;
}

export interface StateRank {
  /** Only leads back to where the machine starts, or leads nowhere. */
  finished: boolean;
  /** Steps from the initial state; unreachable states sort last. */
  depth: number;
  /** Discovery order, so siblings keep the machine's own order. */
  seq: number;
}

export function compareRank(a: StateRank, b: StateRank): number {
  if (a.finished !== b.finished) return a.finished ? 1 : -1;
  if (a.finished) return 0; // finished states fall through to name order
  return a.depth - b.depth || a.seq - b.seq;
}

export function rankStates(workflow: Workflow): Map<string, StateRank> {
  const edges = edgesOf(workflow);
  const initial = workflow.initial_state;
  const names = new Set<string>();
  for (const state of workflow.states) names.add(stateName(state));
  for (const [from, targets] of Object.entries(edges)) {
    names.add(from);
    for (const target of targets) names.add(target);
  }
  if (initial) names.add(initial);

  const depth = new Map<string, number>();
  const seq = new Map<string, number>();
  const queue: string[] = [];
  if (initial && names.has(initial)) {
    depth.set(initial, 0);
    seq.set(initial, 0);
    queue.push(initial);
  }
  for (let i = 0; i < queue.length; i += 1) {
    const current = queue[i]!;
    const currentDepth = depth.get(current) ?? 0;
    for (const next of edges[current] ?? []) {
      if (depth.has(next)) continue;
      depth.set(next, currentDepth + 1);
      seq.set(next, queue.length);
      queue.push(next);
    }
  }

  const ranks = new Map<string, StateRank>();
  let unreachable = queue.length;
  for (const name of names) {
    const outgoing = edges[name] ?? [];
    const finished =
      outgoing.length === 0 || outgoing.every((target) => target === initial);
    ranks.set(name, {
      finished,
      depth: depth.get(name) ?? Number.MAX_SAFE_INTEGER,
      seq: seq.get(name) ?? unreachable++,
    });
  }
  return ranks;
}

export interface Column {
  state: string;
  /** Which kinds' machines have this state; empty means none of them do. */
  kinds: string[];
  known: boolean;
}

/** The board's shape, derived from the machines the server published.
 *
 * States are ordered by walking each machine from its initial state; states
 * that only lead back to the start (or nowhere) are "finished" and sort to
 * the end by name. `statesInData` is the set of states the loaded items are
 * actually in: any of those that no machine mentions is appended as an
 * unknown column, because dropping work on the floor when a workflow is
 * edited underneath it would be worse than showing a column nothing claims.
 */
export function columnsFor(
  workflows: Workflow[],
  statesInData: Iterable<string> = [],
): Column[] {
  const best = new Map<string, StateRank>();
  const kinds = new Map<string, string[]>();
  for (const workflow of workflows) {
    for (const [name, rank] of rankStates(workflow)) {
      const previous = best.get(name);
      if (!previous || compareRank(rank, previous) < 0) best.set(name, rank);
      const owners = kinds.get(name) ?? [];
      if (!owners.includes(workflow.kind)) owners.push(workflow.kind);
      kinds.set(name, owners);
    }
  }
  const ordered: Column[] = [...best.entries()]
    .sort(
      ([leftName, left], [rightName, right]) =>
        compareRank(left, right) || leftName.localeCompare(rightName),
    )
    .map(([state]) => ({ state, kinds: kinds.get(state) ?? [], known: true }));

  const orphans = new Set<string>();
  for (const state of statesInData) {
    if (!best.has(state)) orphans.add(state);
  }
  for (const state of [...orphans].sort()) {
    ordered.push({ state, kinds: [], known: false });
  }
  return ordered;
}

/** States every machine that has them treats as finished. Used only to offer
 *  "hide finished columns", which is a column-visibility action recorded in
 *  the URL — never a claim about what the server considers finished. */
export function finishedStatesFor(workflows: Workflow[]): Set<string> {
  const finished = new Set<string>();
  const live = new Set<string>();
  for (const workflow of workflows) {
    for (const [name, rank] of rankStates(workflow)) {
      if (rank.finished) finished.add(name);
      else live.add(name);
    }
  }
  for (const name of live) finished.delete(name);
  return finished;
}

// -- odds and ends ----------------------------------------------------------

/** The server's own words, never a sentence this client made up. */
function serverReason(error: unknown): string {
  if (error instanceof VogtUnavailable) {
    return error.message.trim() || `Vogt answered ${error.status}`;
  }
  if (error instanceof ApiError) {
    return error.body.trim() || `Vogt answered ${error.status}`;
  }
  return error instanceof Error ? error.message : String(error);
}

function humanState(state: string): string {
  return state.replace(/_/g, " ");
}

/**
 * Trust, on every card (FR-U2, FR-U17).
 *
 * `unverified` rather than a blank, always — the legacy GUI's rule, which
 * every other Vogt surface kept and this one did not: a blank cell says "no
 * opinion", and the honest answer is "nobody has verified this". The board is
 * an aggregated view of exactly the kind FR-U2 names, so a card that showed
 * priority and kind and said nothing about whether anyone had checked the
 * item was the aggregate quietly dropping the least convenient column.
 */
export function trustOf(item: Pick<WorkItem, "trust_state">): string {
  const state = item.trust_state;
  return typeof state === "string" && state ? state : "unverified";
}

/** Kinds are a closed set in Vogt (`WorkKind`), and quick-create needs one
 *  before any workflow has loaded, so this is the fallback rather than the
 *  source: the picker prefers the kinds `workflow.list` published. */
const WORK_KINDS = ["feature", "bug", "chore", "question"] as const;

interface Layout {
  columns: string[];
  lanes: string[];
}

/** Per-client layout only. No item ever reaches storage from here — least of
 *  all one whose move the server refused (FR-U12). */
function readLayout(): Layout {
  try {
    const raw = window.localStorage.getItem(LAYOUT_KEY);
    if (!raw) return { columns: [], lanes: [] };
    const parsed = JSON.parse(raw) as Partial<Layout>;
    return {
      columns: Array.isArray(parsed.columns) ? parsed.columns.map(String) : [],
      lanes: Array.isArray(parsed.lanes) ? parsed.lanes.map(String) : [],
    };
  } catch {
    return { columns: [], lanes: [] };
  }
}

function writeLayout(layout: Layout): void {
  try {
    window.localStorage.setItem(LAYOUT_KEY, JSON.stringify(layout));
  } catch {
    // Private mode, quota, or no storage at all: the board still works,
    // it just forgets which columns were collapsed.
  }
}

export interface Lane {
  key: string;
  label: string;
  items: WorkItem[];
}

// -- the projection, which is the board's whole cost ------------------------
//
// Pure and exported for the same reason `columnsFor` is: this is the part of
// the board most worth testing and the part least needing a browser. It is
// also the part NFR-S5's second clause is about — "the board's filter and
// drag paths do not degrade with backlog size" — so it is worth saying what
// the shape is rather than only that it is fast.
//
// **One pass, whatever the board looks like.** Every card the board draws,
// every WIP count in every column head, and every card's position within its
// cell come out of a single walk of the loaded set. The cost is O(items) and
// nothing multiplies it: a board with eight columns and a board with
// twenty-four do the same work, and so do a board with one swimlane and a
// board with fifty. That is the claim, and `__tests__/boardScale.test.tsx`
// is written to fail if it stops being true — by counting, because jsdom has
// no layout and a wall-clock number there would measure the test runner.
//
// It replaced two `filter`s that looked innocent and were not. The cards in
// a cell were `lane.items.filter(...)`, run once per cell, which is one pass
// over the whole set per *column*. The WIP count was `items().filter(...)`,
// run once per column head and again for every cell's `data-wip` — which is
// one pass over the whole set per *cell*, so the total was quadratic in the
// board's own shape. At the sizes this surface loads (`MAX_ITEMS` is 2,000)
// that is hundreds of thousands of comparisons for one render, and it ran
// again on every keystroke in the move composer.

/** The key a cell is filed under: one lane, one column.
 *
 *  `\u0000` because a lane key is a project slug or an initiative id and a
 *  column is a workflow state, and neither is allowed to contain a NUL —
 *  which a `-` or a `:` cannot promise, and a collision here would draw one
 *  cell's cards in another. */
export function cellKey(laneKey: string, state: string): string {
  return `${laneKey}\u0000${state}`;
}

export interface BoardProjection {
  /** `cellKey(lane, state)` → the cards drawn there, in the lane's order. */
  cells: Map<string, WorkItem[]>;
  /** A workflow state → how many loaded items are in it, across every lane. */
  counts: Map<string, number>;
  /** A ref → the cell its card is drawn in, and its index within that cell. */
  where: Map<string, { cell: string; index: number }>;
}

/** No cell owns this; it is what an empty cell reads back.
 *
 *  One shared instance rather than a fresh `[]`, because `For` compares the
 *  list it was handed and a new empty array every read is a new list. */
export const NO_CARDS: readonly WorkItem[] = Object.freeze([]);

/**
 * Where every card goes, in one walk of the lanes.
 *
 * `placement` is the unsaved half of a drop: the card the user is composing a
 * reason for is drawn in the column they dropped it on, not the one the
 * server still says it is in (FR-U12's optimistic half). Only the ref and the
 * target state are passed, deliberately — the reason the user is typing is
 * part of the same signal on the surface and is *not* part of this, so a
 * keystroke cannot invalidate the projection.
 */
export function projectBoard(
  lanes: readonly Lane[],
  placement: { ref: string; to: string } | null,
): BoardProjection {
  const cells = new Map<string, WorkItem[]>();
  const counts = new Map<string, number>();
  const where = new Map<string, { cell: string; index: number }>();
  for (const lane of lanes) {
    for (const item of lane.items) {
      const state =
        placement !== null && placement.ref === item.ref ? placement.to : item.state;
      const key = cellKey(lane.key, state);
      let cell = cells.get(key);
      if (cell === undefined) {
        cell = [];
        cells.set(key, cell);
      }
      where.set(item.ref, { cell: key, index: cell.length });
      cell.push(item);
      counts.set(state, (counts.get(state) ?? 0) + 1);
    }
  }
  return { cells, counts, where };
}

interface PendingMove {
  ref: string;
  from: string;
  to: string;
  laneKey: string;
  reason: string;
  phase: "reason" | "saving";
}

interface CellPageState {
  items: WorkItem[];
  total: number;
  nextCursor: string | null;
  loading: boolean;
  error: string | null;
}

interface Refusal {
  ref: string;
  from: string;
  to: string;
  laneKey: string;
  reason: string;
  message: string;
}

const PRIORITY_ORDER = (priority: string): number => {
  const match = /^p(\d)$/.exec(priority);
  return match ? Number.parseInt(match[1]!, 10) : 9;
};

const Board: Component<Props> = (props) => {
  const [searchParams, setSearchParams] = useSearchParams<BoardParams>();
  const location = useLocation();
  const navigate = useNavigate();

  const [filters, setFilters] = createSignal<Filters>(filtersFromQuery(searchParams));
  const [phone, setPhone] = createSignal(false);
  const [expandedCards, setExpandedCards] = createSignal<Set<string>>(new Set());
  const [expandableCards, setExpandableCards] = createSignal<Set<string>>(new Set());

  const cardExpanded = (ref: string) => expandedCards().has(ref);
  const toggleCardExpanded = (ref: string) => {
    setExpandedCards((current) => {
      const next = new Set(current);
      if (next.has(ref)) next.delete(ref);
      else next.add(ref);
      return next;
    });
  };
  const cardExpandable = (item: WorkItem) =>
    Boolean(item.body?.trim()) || expandableCards().has(item.ref);
  const noteTitleClipping = (ref: string, clipped: boolean) => {
    setExpandableCards((current) => {
      if (current.has(ref) === clipped) return current;
      const next = new Set(current);
      if (clipped) next.add(ref);
      else next.delete(ref);
      return next;
    });
  };

  /**
   * How far down each windowed column the reader has scrolled, held here
   * rather than in the cell that owns it.
   *
   * `For` maps its rows by reference, and both `lanes()` and `items()` are
   * rebuilt from scratch whenever `work.list` answers — so every cell on this
   * board is destroyed and recreated on every load, which with the poll
   * running is every twenty seconds. That never mattered while a cell was a
   * plain stack of cards inside the board's own scroller. It matters now: a
   * cell that kept its own offset would hand the reader back to the top of
   * the column mid-read, on a timer, and it would look as though Vogt had
   * changed something.
   *
   * Deliberately not a signal. Nothing renders from it — each cell has its
   * own signal, seeded from this — and making it reactive would wake every
   * cell on the board on every scroll of any one of them.
   */
  const cellScroll = new Map<string, number>();

  /**
   * Change the filter set, and forget where the old columns were scrolled to.
   *
   * The forgetting happens *here*, synchronously with the change, and not in
   * an effect watching it. `filters()` is one signal, so changing any part of
   * it invalidates `lanes()`, and `For` then tears down and rebuilds every
   * cell before any effect of ours runs — a rebuilt cell would already have
   * read the offset an effect was about to clear. A new filter set is a new
   * column and the reader has not seen the top of it.
   */
  const applyFilters = (next: Filters) => {
    if (encodeFilters(next) !== encodeFilters(filters())) cellScroll.clear();
    setFilters(next);
  };

  const patch = (next: Partial<Filters>) => applyFilters({ ...filters(), ...next });

  const [workflows, setWorkflows] = createSignal<Workflow[] | null>(null);
  const [metadataReady, setMetadataReady] = createSignal(false);
  const [items, setItems] = createSignal<WorkItem[]>([]);
  const [total, setTotal] = createSignal(0);
  const [cellPages, setCellPages] = createSignal<Map<string, CellPageState>>(new Map());
  const [columnTotals, setColumnTotals] = createSignal<Record<string, number>>({});
  const [laneTotals, setLaneTotals] = createSignal<Record<string, number>>({});
  const [boardSnapshot, setBoardSnapshot] = createSignal<string | null>(null);
  const [loadedAt, setLoadedAt] = createSignal<number | null>(null);
  const [loading, setLoading] = createSignal(false);
  const [outage, setOutage] = createSignal<string | null>(null);
  const [loadError, setLoadError] = createSignal<string | null>(null);

  const [projects, setProjects] = createSignal<{ slug: string; name: string }[]>([]);
  const [initiatives, setInitiatives] = createSignal<
    { id?: string; slug: string; title: string }[]
  >([]);
  const [labels, setLabels] = createSignal<string[]>([]);
  const [actors, setActors] = createSignal<
    { identity_ref: string; display_name: string }[]
  >([]);

  const [dragRef, setDragRef] = createSignal<string | null>(null);
  const [dragOver, setDragOver] = createSignal<string | null>(null);
  const [pending, setPending] = createSignal<PendingMove | null>(null);
  const [refusal, setRefusal] = createSignal<Refusal | null>(null);
  const [ack, setAck] = createSignal<string | null>(null);
  /** The card that just came back from a refusal, so the rollback is seen and
   *  not merely true (FR-U12: "roll the item back visibly"). */
  const [bounced, setBounced] = createSignal<string | null>(null);
  const [stickyReason, setStickyReason] = createSignal("");
  const [focusedRef, setFocusedRef] = createSignal<string | null>(null);

  const [layout, setLayout] = createSignal<Layout>(readLayout());
  const collapsedColumn = (state: string) => layout().columns.includes(state);
  const collapsedLane = (key: string) =>
    layout().lanes.includes(`${filters().lanes}:${key}`);
  const toggleColumn = (state: string) => {
    const current = layout();
    const next = current.columns.includes(state)
      ? current.columns.filter((one) => one !== state)
      : [...current.columns, state];
    const updated = { ...current, columns: next };
    setLayout(updated);
    writeLayout(updated);
  };
  const toggleLane = (key: string) => {
    const scoped = `${filters().lanes}:${key}`;
    const current = layout();
    const next = current.lanes.includes(scoped)
      ? current.lanes.filter((one) => one !== scoped)
      : [...current.lanes, scoped];
    const updated = { ...current, lanes: next };
    setLayout(updated);
    writeLayout(updated);
  };

  // -- the URL is the filter set (FR-U11) -----------------------------------
  //
  // One effect, both directions, with the ambiguity resolved by remembering
  // what this surface last asserted — the shape `Backlog.tsx` arrived at and
  // explains, and which the board needed and did not have. Two things were
  // broken without it, both of them FR-U11's actual subject:
  //
  //   * The shell navigates to a bare `/board` when its tab is re-selected,
  //     which drops the query. The old effect read neither the pathname's
  //     *value* nor the query, so nothing re-ran and the filters were gone
  //     from the URL for good — the view survived, its address did not.
  //   * A link pasted into an already-mounted board was ignored: the query
  //     was read once, at construction, and never again.
  //
  // Guarded on the pathname because the router and test harness can keep this
  // component alive while a navigation settles, and `project`, `label` and
  // `assignee` are keys more than one Vogt surface owns.
  let lastWritten = encodeQuery(searchParams);

  createEffect(() => {
    if (location.pathname !== "/board") return;
    const desired = encodeFilters(filters());
    const current = encodeQuery(searchParams);
    if (desired === current) {
      lastWritten = current;
      return;
    }
    if (current !== lastWritten && current !== "") {
      // The query changed to something this surface did not write, and is not
      // empty: a pasted link, or the back button. That is an instruction.
      applyFilters(filtersFromQuery(searchParams));
      lastWritten = current;
      return;
    }
    // Otherwise the signals are authoritative, which is also what restores
    // the query after a tab switch emptied it.
    setSearchParams(queryFor(filters()), { replace: true, scroll: false });
    lastWritten = desired;
  });

  // -- loading --------------------------------------------------------------

  let loadSeq = 0;
  let notified: string | null = null;

  const reportOnce = (message: string) => {
    if (notified === message) return;
    notified = message;
    props.onError?.(message);
  };

  const noteFailure = (error: unknown, what: string) => {
    if (error instanceof VogtUnavailable) {
      setOutage(serverReason(error));
      reportOnce(`Vogt is unreachable: ${serverReason(error)}`);
      return;
    }
    setLoadError(serverReason(error));
    reportOnce(`${what}: ${serverReason(error)}`);
  };

  const loadWorkflows = async () => {
    try {
      const answer = await listWorkflows();
      setWorkflows(answer.workflows ?? []);
      setOutage(null);
    } catch (e) {
      noteFailure(e, "Failed to read the workflows the columns come from");
    }
  };

  /** Best-effort: a filter whose options failed to load still works from the
   *  URL, it just cannot be picked from a list. */
  const loadTaxonomy = async () => {
    const settle = await Promise.allSettled([
      listProjects(),
      listInitiatives(),
      listLabels(),
      listActors(),
    ]);
    const [byProject, byInitiative, byLabel, byActor] = settle;
    if (byProject.status === "fulfilled") {
      setProjects(byProject.value.projects ?? []);
    }
    if (byInitiative.status === "fulfilled") {
      // `initiative.list` returns whole initiatives, `id` included; the
      // client's declared type omits it, so it is read defensively here and
      // the omission reported against `vogtApi.ts`.
      setInitiatives(
        (byInitiative.value.initiatives ?? []) as {
          id?: string;
          slug: string;
          title: string;
        }[],
      );
    }
    if (byLabel.status === "fulfilled") {
      setLabels((byLabel.value.labels ?? []).map((one) => one.name));
    }
    if (byActor.status === "fulfilled") {
      setActors(byActor.value.actors ?? []);
    }
  };

  /** Both sides are the server's; the later `updated_at` wins, so a poll that
   *  started before a transition landed cannot undo it on screen. */
  const mergeItems = (fresh: WorkItem[]): WorkItem[] => {
    const previous = new Map(items().map((item) => [item.ref, item]));
    return fresh.map((item) => {
      const old = previous.get(item.ref);
      if (!old) return item;
      return Date.parse(old.updated_at) > Date.parse(item.updated_at)
        ? old
        : item;
    });
  };

  const laneKeys = (active: Filters): string[] => {
    if (active.lanes === "project") {
      const keys = [
        ...projects().map((project) => project.slug),
        ...Object.keys(laneTotals()),
      ].filter((slug) => !active.project || slug === active.project);
      return [...keys, ""].filter((key, index, all) => all.indexOf(key) === index);
    }
    if (active.lanes === "initiative") {
      const keys = [
        ...initiatives().flatMap((initiative) =>
          initiative.id ? [initiative.id] : [],
        ),
        ...Object.keys(laneTotals()),
      ];
      return [...keys, ""].filter((key, index, all) => all.indexOf(key) === index);
    }
    return [""];
  };

  const requestedStates = (active: Filters): string[] => {
    const available = columnsFor(workflows() ?? [], Object.keys(columnTotals()))
      .map((column) => column.state)
      .filter((state) => !active.states.length || active.states.includes(state));
    if (phone()) {
      const selected = available.includes(active.column) ? active.column : available[0];
      return selected ? [selected] : [];
    }
    if (active.states.length) return active.states;
    return available;
  };

  const boardCells = (active: Filters) =>
    laneKeys(active)
      .slice(
        0,
        typeof IntersectionObserver === "undefined"
          ? undefined
          : INITIAL_LANE_PAGES,
      )
      .flatMap((lane_key) =>
      requestedStates(active).map((state) => ({ lane_key, state })),
    );

  const boardParams = (active: Filters) => ({
    project: active.project || undefined,
    kinds: active.kinds.length ? active.kinds : undefined,
    states: active.states.length ? active.states : undefined,
    label: active.label || undefined,
    initiative: active.initiative || undefined,
    assignee: active.assignee || undefined,
    lane_mode: active.lanes,
    page_size: CELL_PAGE_SIZE,
  });

  const mergeCellPage = (
    previous: Map<string, CellPageState>,
    page: BoardCellPage,
    append: boolean,
  ): Map<string, CellPageState> => {
    const next = new Map(previous);
    const key = cellKey(page.lane_key, page.state);
    const old = previous.get(key);
    const combined = append ? [...(old?.items ?? []), ...(page.items ?? [])] : page.items ?? [];
    const unique = [...new Map(combined.map((item) => [item.ref, item])).values()];
    next.set(key, {
      items: mergeItems(unique),
      total: page.total,
      nextCursor: page.next_cursor ?? null,
      loading: false,
      error: null,
    });
    return next;
  };

  const syncItemsFromCells = (pages: Map<string, CellPageState>) => {
    setItems(
      mergeItems(
        [...pages.values()].flatMap((page) => page.items),
      ),
    );
  };

  const loadItems = async (quiet = false) => {
    if (workflows() === null) return;
    const seq = ++loadSeq;
    if (!quiet) setLoading(true);
    const active = filters();
    try {
      const answer = await listBoard({
        ...boardParams(active),
        cells: boardCells(active),
      });
      if (seq !== loadSeq) return;
      let next = new Map<string, CellPageState>();
      for (const page of answer.cells ?? []) next = mergeCellPage(next, page, false);
      setCellPages(next);
      syncItemsFromCells(next);
      setTotal(answer.total);
      setColumnTotals(answer.column_totals ?? {});
      setLaneTotals(answer.lane_totals ?? {});
      setBoardSnapshot(answer.snapshot);
      setLoadedAt(Date.now());
      setOutage(null);
      setLoadError(null);
      notified = null;
    } catch (e) {
      if (seq !== loadSeq) return;
      noteFailure(e, "Failed to load work items");
    } finally {
      if (seq === loadSeq && !quiet) setLoading(false);
    }
  };

  const loadNextCell = async (laneKey: string, state: string) => {
    const key = cellKey(laneKey, state);
    const current = cellPages().get(key);
    const snapshot = boardSnapshot();
    if (!current?.nextCursor || current.loading || !snapshot) return;
    setCellPages((previous) => {
      const next = new Map(previous);
      next.set(key, { ...current, loading: true, error: null });
      return next;
    });
    try {
      const answer = await listBoard({
        ...boardParams(filters()),
        cells: [{ lane_key: laneKey, state, cursor: current.nextCursor }],
        snapshot,
      });
      const page = answer.cells[0];
      if (!page || answer.snapshot !== snapshot) return;
      let merged: Map<string, CellPageState> | undefined;
      setCellPages((previous) => {
        const next = mergeCellPage(previous, page, true);
        merged = next;
        return next;
      });
      if (merged) syncItemsFromCells(merged);
    } catch (error) {
      const message = serverReason(error);
      setCellPages((previous) => {
        const next = new Map(previous);
        const latest = previous.get(key);
        if (latest) next.set(key, { ...latest, loading: false, error: message });
        return next;
      });
    }
  };

  const loadingLanes = new Set<string>();
  const loadLane = async (laneKey: string) => {
    const snapshot = boardSnapshot();
    const missing = requestedStates(filters()).filter(
      (state) => !cellPages().has(cellKey(laneKey, state)),
    );
    if (!snapshot || !missing.length || loadingLanes.has(laneKey)) return;
    loadingLanes.add(laneKey);
    try {
      const answer = await listBoard({
        ...boardParams(filters()),
        cells: missing.map((state) => ({ lane_key: laneKey, state })),
        snapshot,
      });
      if (answer.snapshot !== snapshot) return;
      let merged: Map<string, CellPageState> | undefined;
      setCellPages((previous) => {
        let next = previous;
        for (const page of answer.cells ?? []) next = mergeCellPage(next, page, false);
        merged = next;
        return next;
      });
      if (merged) syncItemsFromCells(merged);
    } catch (error) {
      const message = serverReason(error);
      setCellPages((previous) => {
        const next = new Map(previous);
        for (const state of missing) {
          next.set(cellKey(laneKey, state), {
            items: [],
            total: 0,
            nextCursor: null,
            loading: false,
            error: message,
          });
        }
        return next;
      });
    } finally {
      loadingLanes.delete(laneKey);
    }
  };

  const reload = async () => {
    await Promise.all([loadWorkflows(), loadTaxonomy()]);
    setMetadataReady(true);
    await loadItems();
  };

  onMount(() => {
    const query = window.matchMedia("(max-width: 768px)");
    const updatePhone = () => setPhone(query.matches);
    updatePhone();
    query.addEventListener("change", updatePhone);
    onCleanup(() => query.removeEventListener("change", updatePhone));
    void reload();
  });

  // Refetch whenever the server-side half of the filter changes. The lane
  // mode is server-owned grouping; only the poll interval stays client-side.
  createEffect<string>((previous) => {
    const active = filters();
    const key = JSON.stringify([
      active.project,
      active.kinds,
      active.states,
      active.label,
      active.initiative,
      active.assignee,
      active.lanes,
      phone() ? active.column : "",
      workflows()?.map((workflow) => [workflow.kind, workflow.states]),
      projects().map((project) => project.slug),
      initiatives().map((initiative) => initiative.id),
    ]);
    if (metadataReady() && previous !== undefined && previous !== key) void loadItems();
    return key;
  });

  // The poll the freshness line below describes. The clock it is measured
  // against belongs to `createViewAge` now, with the rest of the badge.
  createEffect(() => {
    const seconds = filters().poll;
    if (seconds <= 0) return;
    const timer = window.setInterval(() => {
      // A hidden tab is not a view anybody is being misled by, and the
      // visibility handler below reconciles the moment it comes back.
      if (typeof document !== "undefined" && document.hidden) return;
      if (pending()) return; // never race a write the user is composing
      void loadItems(true);
    }, seconds * 1000);
    onCleanup(() => window.clearInterval(timer));
  });

  // Vogt's own changes, pushed, plus the reconcile a tab gets when it comes
  // back to the front. The front door follows the core's event cursor and
  // republishes onto the stream this client already has open (FR-U10), so a
  // transition somebody else made arrives here rather than waiting for the
  // next poll. The poll stays as the floor: this stream can drop, and a board
  // that stopped refreshing because a socket died would be stale while
  // looking current — which is the thing the requirement is actually about.
  //
  // Shared with the other four Vogt surfaces since they gained the same
  // clause; the board's visibility handler used to sit alongside this one and
  // is `onVogtLive`'s second half now, which also gives it the composer guard
  // it never had.
  onVogtLive(() => void loadItems(true), { when: () => !pending() });

  // -- the model the board draws -------------------------------------------

  const activeWorkflows = createMemo(() => {
    const all = workflows() ?? [];
    const kinds = filters().kinds;
    if (!kinds.length) return all;
    const wanted = all.filter((one) => kinds.includes(one.kind));
    return wanted.length ? wanted : all;
  });

  const allColumns = createMemo<Column[]>(() =>
    columnsFor(
      activeWorkflows(),
      Object.keys(columnTotals()),
    ),
  );

  const selectableColumns = createMemo<Column[]>(() => {
    const wanted = filters().states;
    if (!wanted.length) return allColumns();
    const shown = allColumns().filter((column) => wanted.includes(column.state));
    return shown.length ? shown : allColumns();
  });

  const columns = createMemo<Column[]>(() => {
    const available = selectableColumns();
    if (!phone()) return available;
    const selected = available.find((column) => column.state === filters().column);
    return selected ? [selected] : available.slice(0, 1);
  });

  // The default state is the first one the server ordered, and a default is
  // not worth an entry in the address: `column` appears only once the reader
  // has chosen a state, and leaves again when a filter removes that state
  // from the board.
  createEffect(() => {
    if (!phone()) return;
    const selected = filters().column;
    if (!selected) return;
    const available = selectableColumns();
    if (available.length && !available.some((column) => column.state === selected)) {
      patch({ column: "" });
    }
  });

  const finishedStates = createMemo(() => finishedStatesFor(activeWorkflows()));

  const projectName = (slug: string) =>
    projects().find((one) => one.slug === slug)?.name ?? slug;

  const initiativeLabel = (id: string) => {
    const found = initiatives().find((one) => one.id === id);
    return found ? found.title : id;
  };

  const laneOf = (item: WorkItem): { key: string; label: string } => {
    switch (filters().lanes) {
      case "project":
        return item.project_slug
          ? { key: item.project_slug, label: projectName(item.project_slug) }
          : { key: "", label: "No project" };
      case "initiative":
        return item.initiative_id
          ? {
              key: item.initiative_id,
              label: initiativeLabel(item.initiative_id),
            }
          : { key: "", label: "No initiative" };
      default:
        return { key: "", label: "All work" };
    }
  };

  /** Whether anything is narrowing the board, so an empty one can say why. */
  const hasFilters = () => {
    const active = filters();
    return Boolean(
      active.project ||
        active.label ||
        active.initiative ||
        active.assignee ||
        active.kinds.length ||
        active.states.length,
    );
  };

  const lanes = createMemo<Lane[]>(() => {
    const grouped = new Map<string, Lane>();
    // Seed a lane for every project or initiative that exists, before any
    // work is placed. Lanes used to be derived from the loaded items alone,
    // so a project with no work had no lane and was simply absent from the
    // board — which is the state every freshly imported project is in. The
    // first thing an import's owner does is open the board, and the board's
    // answer for a correct import was to show nothing and label it "No
    // matching work", pointing at the filter rather than at the absence.
    if (filters().lanes === "project" && boardSnapshot()) {
      for (const project of projects()) {
        if (filters().project && project.slug !== filters().project) continue;
        grouped.set(project.slug, {
          key: project.slug,
          label: project.name ?? project.slug,
          items: [],
        });
      }
      grouped.set("", {
        key: "",
        label: projects().length === 0 && total() === 0 ? "No work yet" : "No project",
        items: [],
      });
    } else if (filters().lanes === "initiative" && boardSnapshot()) {
      for (const initiative of initiatives()) {
        // `laneOf` keys initiatives by id, so one without an id cannot be
        // matched to any item and would seed a lane nothing can ever join.
        if (!initiative.id) continue;
        grouped.set(initiative.id, {
          key: initiative.id,
          label: initiative.title,
          items: [],
        });
      }
      grouped.set("", { key: "", label: "No initiative", items: [] });
    }
    for (const item of items()) {
      const { key, label } = laneOf(item);
      const lane = grouped.get(key) ?? { key, label, items: [] };
      lane.items.push(item);
      grouped.set(key, lane);
    }
    if (grouped.size === 0) {
      grouped.set("", {
        key: "",
        // "No matching work" is a claim about the filter, and it was being
        // made when nothing had been loaded at all. Say which it is.
        label:
          filters().lanes === "none"
            ? "All work"
            : hasFilters()
              ? "No matching work"
              : "No work yet",
        items: [],
      });
    }
    const ordered = [...grouped.values()].sort((left, right) => {
      if (left.key === "") return 1;
      if (right.key === "") return -1;
      return left.label.localeCompare(right.label);
    });
    for (const lane of ordered) {
      lane.items.sort(
        (left, right) =>
          PRIORITY_ORDER(left.priority) - PRIORITY_ORDER(right.priority) ||
          Date.parse(right.updated_at) - Date.parse(left.updated_at),
      );
    }
    return ordered;
  });

  /** Where a card is drawn: the server's state, unless a drop is unsaved. */
  const displayState = (item: WorkItem): string => {
    const move = pending();
    return move && move.ref === item.ref ? move.to : item.state;
  };

  /**
   * The part of an unsaved drop a projection can see.
   *
   * A memo with its own `equals` rather than a read of `pending()`, and this
   * is the whole reason the composer does not re-project the board. The
   * pending move carries the reason the user is typing in the same object as
   * the card's optimistic position, so a projection that read `pending()`
   * would be invalidated on every keystroke — over two thousand items, per
   * cell, while somebody was mid-sentence. Where the card is drawn changes
   * when the ref or the target changes and at no other time, and this says
   * exactly that.
   */
  const placement = createMemo<{ ref: string; to: string } | null>(
    () => {
      const move = pending();
      return move ? { ref: move.ref, to: move.to } : null;
    },
    null,
    {
      equals: (previous, next) =>
        (previous?.ref ?? null) === (next?.ref ?? null) &&
        (previous?.to ?? null) === (next?.to ?? null),
    },
  );

  /** One walk of the loaded set, and every cell, count and index comes out of
   *  it. See `projectBoard` for why this is not a per-cell `filter`. */
  const projection = createMemo(() => projectBoard(lanes(), placement()));

  const cellItems = (lane: Lane, state: string): readonly WorkItem[] =>
    projection().cells.get(cellKey(lane.key, state)) ?? NO_CARDS;

  const columnCount = (state: string) => {
    const base = columnTotals()[state] ?? 0;
    const move = pending();
    if (!move || move.from === move.to) return base;
    return base + (move.to === state ? 1 : 0) - (move.from === state ? 1 : 0);
  };

  const laneCount = (laneKey: string) => laneTotals()[laneKey] ?? 0;

  const itemByRef = createMemo(() => {
    const index = new Map<string, WorkItem>();
    for (const item of items()) index.set(item.ref, item);
    return index;
  });

  const legalTargets = createMemo(() => {
    const moving = dragRef();
    if (!moving) return null;
    const item = itemByRef().get(moving);
    if (!item) return null;
    const workflow = (workflows() ?? []).find((one) => one.kind === item.kind);
    if (!workflow) return null;
    return new Set(edgesOf(workflow)[item.state] ?? []);
  });

  const writesDisabled = createMemo(() => outage() !== null);

  // -- the move ------------------------------------------------------------

  // A lane is a grouping, not a target: dropping into another lane's cell
  // still moves the item's *state*, and the composer opens in the item's own
  // lane so the card and the question it raised stay together. Changing an
  // item's project or initiative is `work.update`, a different write with its
  // own reason, and the board does not pretend a drag can do it.
  const beginMove = (item: WorkItem, to: string) => {
    if (writesDisabled()) return;
    if (item.state === to) return;
    if (pending()) return;
    setRefusal(null);
    setAck(null);
    setPending({
      ref: item.ref,
      from: item.state,
      to,
      laneKey: laneOf(item).key,
      reason: stickyReason(),
      phase: "reason",
    });
  };

  const cancelMove = () => {
    // The card goes back the instant the reason is abandoned. Nothing about
    // the attempted position survives this.
    setPending(null);
  };

  const applyItem = (updated: WorkItem) => {
    const present = items().some((one) => one.ref === updated.ref);
    setItems(
      present
        ? items().map((one) => (one.ref === updated.ref ? updated : one))
        : [...items(), updated],
    );
  };

  const commitMove = async () => {
    const move = pending();
    if (!move) return;
    const reason = move.reason.trim();
    if (!reason) return; // FR-W1: no reason the user typed, no write.
    setPending({ ...move, phase: "saving" });
    try {
      const answer = await transitionWork(move.ref, move.to, reason);
      const updated = answer?.item;
      setColumnTotals((counts) => ({
        ...counts,
        [move.from]: Math.max(0, (counts[move.from] ?? 0) - 1),
        [move.to]: (counts[move.to] ?? 0) + 1,
      }));
      setPending(null);
      setStickyReason(reason);
      if (updated) applyItem(updated);
      else void loadItems(true);
      setAck(`${move.ref} moved to ${humanState(move.to)}.`);
      window.setTimeout(() => {
        if (ack()?.startsWith(move.ref)) setAck(null);
      }, 6000);
    } catch (e) {
      // FR-U12: the server's answer is the answer. The optimistic position is
      // dropped here and nowhere else remembers it — the card is back in the
      // column `items()` says it is in the moment `pending` clears.
      setPending(null);
      setBounced(move.ref);
      window.setTimeout(() => {
        if (bounced() === move.ref) setBounced(null);
      }, 2500);
      if (e instanceof VogtUnavailable) setOutage(serverReason(e));
      setRefusal({
        ref: move.ref,
        from: move.from,
        to: move.to,
        laneKey: move.laneKey,
        reason,
        message: serverReason(e),
      });
    }
  };

  const retryRefusal = () => {
    const refused = refusal();
    if (!refused) return;
    const item = itemByRef().get(refused.ref);
    setRefusal(null);
    if (!item) return;
    setPending({
      ref: item.ref,
      from: item.state,
      to: refused.to,
      laneKey: laneOf(item).key,
      reason: refused.reason,
      phase: "reason",
    });
  };

  // -- quick-create (FR-U15) ------------------------------------------------
  //
  // FR-U15 names the board *first* and the backlog second; the backlog had
  // it and the board did not, which made half of a must-have clause an
  // absence. The rule is the backlog's, unchanged, because it is r6's rule
  // and it does not get a second dialect: title, type, project, and a reason
  // the user typed, inline, without leaving the view — and no reason, no
  // submit. Everything else is deferrable to the detail view.
  //
  // The reason is never prefilled. The board's *move* composer does prefill
  // one, from the last reason this session accepted, and that is a different
  // case with a different justification: a triage run is one decision applied
  // to a sequence of cards. Raising an item is not a repetition of raising
  // the previous one, so `stickyReason` deliberately does not reach here.

  const [createOpen, setCreateOpen] = createSignal(false);
  const [draftTitle, setDraftTitle] = createSignal("");
  const [draftKind, setDraftKind] = createSignal("feature");
  const [draftProject, setDraftProject] = createSignal("");
  const [draftReason, setDraftReason] = createSignal("");
  const [creating, setCreating] = createSignal(false);
  const [created, setCreated] = createSignal<string | null>(null);
  const [createError, setCreateError] = createSignal<string | null>(null);

  // A plain function rather than a memo: `kindOptions` is declared below with
  // the rest of the render helpers, and a memo body runs the moment it is
  // created.
  const kindChoices = (): string[] => {
    const published = kindOptions();
    return published.length ? published : [...WORK_KINDS];
  };

  /** The whole of FR-U15's refusal, in one place. `disabled` says it on
   *  screen and the guard in `submitCreate` says it again for the keyboard. */
  const createReady = createMemo(
    () => draftTitle().trim().length > 0 && draftReason().trim().length > 0,
  );

  const openQuickCreate = () => {
    if (writesDisabled()) return;
    // The filters in force are a good guess at what is being raised; the
    // reason never is.
    const kinds = filters().kinds;
    setDraftKind(kinds[0] ?? kindChoices()[0] ?? "feature");
    setDraftProject(filters().project);
    setCreated(null);
    setCreateError(null);
    setCreateOpen(true);
  };

  const submitCreate = async (event: Event) => {
    event.preventDefault();
    const title = draftTitle().trim();
    const reason = draftReason().trim();
    if (!title || !reason || creating()) return; // FR-W1, again for the keyboard.
    setCreating(true);
    setCreateError(null);
    try {
      const answer = await createWork({
        title,
        kind: draftKind(),
        project: draftProject() || undefined,
        reason,
      });
      const item = answer?.item;
      setCreated(item?.ref ?? null);
      setDraftTitle("");
      setDraftReason("");
      // The new item belongs in its column now, not at the next poll.
      if (item) applyItem(item);
      else void loadItems(true);
    } catch (e) {
      if (e instanceof VogtUnavailable) setOutage(serverReason(e));
      // Vogt's own sentence, where the form is — not a toast somewhere else.
      setCreateError(serverReason(e));
      props.onError?.(`Could not create the work item: ${serverReason(e)}`);
    } finally {
      setCreating(false);
    }
  };

  // -- keyboard (FR-U22, as far as it goes) --------------------------------

  const cardDomId = (ref: string) => `board-card-${ref}`;

  /**
   * Move focus to a card, whether or not it is currently drawn.
   *
   * `focusedRef` is set *first* and that is load-bearing, not bookkeeping:
   * a windowed cell includes the focused card in its slice however far down
   * the column it is (see `slice` below), so setting the signal is what puts
   * the element in the DOM for `focus()` to find. Without it, keyboard
   * navigation would stop at the bottom of the rendered window and the
   * reader would have no way to know why — which is the failure FR-U22 and
   * NFR-S5 have between them, and the reason the cap was easier.
   *
   * Nothing here scrolls: focusing an element inside a scroll container is
   * what scrolls it into view, and the `scroll` handler then tells the cell
   * where it is.
   */
  const focusCard = (ref: string | null) => {
    if (!ref) return;
    setFocusedRef(ref);
    window.setTimeout(() => document.getElementById(cardDomId(ref))?.focus(), 0);
  };

  const onCardKeyDown = (event: KeyboardEvent, item: WorkItem, lane: Lane) => {
    // A control inside the card owns its own keys: Enter on "Show more" has to
    // expand the card in place, not open the item the card is standing on.
    const from = event.target as HTMLElement | null;
    if (
      from &&
      from !== event.currentTarget &&
      from.closest("button, a, input, select, textarea")
    ) {
      return;
    }
    const shown = columns();
    const index = shown.findIndex((one) => one.state === displayState(item));
    if (event.key === "Enter") {
      event.preventDefault();
      openDetail(item.ref);
      return;
    }
    if (event.key === "ArrowUp" || event.key === "ArrowDown") {
      event.preventDefault();
      // The projection already knows which cell this card is in and where in
      // it, so neither of those is a scan — and neither of them is a read of
      // the DOM, which matters now that the cell is windowed: the card above
      // this one may not be rendered yet, and it still has to be reachable.
      const at = projection().where.get(item.ref);
      if (!at) return;
      const siblings = projection().cells.get(at.cell) ?? NO_CARDS;
      const next = siblings[at.index + (event.key === "ArrowDown" ? 1 : -1)];
      focusCard(next?.ref ?? null);
      return;
    }
    if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") return;
    event.preventDefault();
    const step = event.key === "ArrowRight" ? 1 : -1;
    const target = shown[index + step];
    if (!target) return;
    if (event.shiftKey) {
      beginMove(item, target.state);
      return;
    }
    const neighbours = cellItems(lane, target.state);
    focusCard(neighbours[0]?.ref ?? null);
  };

  const openDetail = (ref: string) => {
    openWorkItemTab(ref);
    navigate(`/w/${encodeURIComponent(ref)}`);
  };

  /** FR-U22's last clause: quick-create has a binding, now that there is a
   *  quick-create on this board to bind.
   *
   *  Scoped to the surface rather than to `document`: a document-level `n`
   *  could raise a work item while somebody was reading a terminal. It
   *  listens on the board's own
   *  root and ignores anything typed into a field, which is what makes it
   *  reachable from a focused card, the toolbar, or the board's background
   *  without stealing a keystroke from the composer. */
  const onSurfaceKeyDown = (event: KeyboardEvent) => {
    if (event.key !== "n" || event.metaKey || event.ctrlKey || event.altKey) return;
    const target = event.target as HTMLElement | null;
    if (target?.isContentEditable) return;
    if (target && /^(INPUT|TEXTAREA|SELECT)$/.test(target.tagName)) return;
    if (createOpen()) return;
    event.preventDefault();
    openQuickCreate();
  };

  // -- rendering ------------------------------------------------------------

  const gridTemplate = createMemo(() =>
    columns()
      .map((column) => (collapsedColumn(column.state) ? "46px" : "minmax(240px, 1fr)"))
      .join(" "),
  );

  // The words are `viewAge.tsx`'s now, and the same ones every other Vogt
  // surface says. Nothing about what the board reports changed in the move:
  // the poll interval is still what "Polling" and "Stale" are measured
  // against, and three times the interval is still the point at which this
  // view stops calling itself current.
  const freshness = createViewAge(() => ({
    loadedAt: loadedAt(),
    outage: outage(),
    failed: Boolean(loadError()),
    poll: filters().poll,
    live: true,
  }));

  const toggleKind = (kind: string) => {
    const current = filters().kinds;
    patch({
      kinds: current.includes(kind)
        ? current.filter((one) => one !== kind)
        : [...current, kind],
    });
  };

  const clearFilters = () =>
    patch({
      project: "",
      kinds: [],
      states: [],
      label: "",
      initiative: "",
      assignee: "",
      lanes: "none",
    });

  const hideFinishedColumns = () => {
    const finished = finishedStates();
    patch({
      states: allColumns()
        .filter((column) => !finished.has(column.state))
        .map((column) => column.state),
    });
  };

  // -- saved filters (FR-U14) ----------------------------------------------

  const [savedFilters, setSavedFilters] = createSignal<SavedFilter[]>(readSavedFilters());

  const saveCurrent = (name: string) => {
    // The poll interval is a refresh preference, not a filter, so it is left
    // out of what gets saved and left alone on recall: a named view should
    // not change how often the reader's board refreshes.
    const query = encodeFilters({
      ...filters(),
      column: "",
      poll: DEFAULT_POLL_SECONDS,
    });
    setSavedFilters(
      writeSavedFilters([
        { name, query },
        ...savedFilters().filter((entry) => entry.name !== name),
      ]),
    );
  };

  const recallSaved = (name: string) => {
    const entry = savedFilters().find((one) => one.name === name);
    if (!entry) return;
    applyFilters({
      ...filtersFromQuery(queryFromSearch(entry.query)),
      column: filters().column,
      poll: filters().poll,
    });
  };

  const forgetSaved = (name: string) => {
    setSavedFilters(writeSavedFilters(savedFilters().filter((one) => one.name !== name)));
  };

  const describeSaved = (entry: SavedFilter) =>
    describeFilters(filtersFromQuery(queryFromSearch(entry.query)));

  const filterCount = createMemo(() => {
    const active = filters();
    return (
      (active.project ? 1 : 0) +
      (active.kinds.length ? 1 : 0) +
      (active.states.length ? 1 : 0) +
      (active.label ? 1 : 0) +
      (active.initiative ? 1 : 0) +
      (active.assignee ? 1 : 0) +
      (active.lanes !== "none" ? 1 : 0)
    );
  });

  const activeFilterChips = createMemo(() => {
    const active = filters();
    const chips: { key: string; label: string }[] = [];
    if (active.project) chips.push({ key: "project", label: `Project: ${active.project}` });
    if (active.kinds.length) chips.push({ key: "kind", label: `Type: ${active.kinds.join(", ")}` });
    if (active.states.length) chips.push({ key: "state", label: `State: ${active.states.join(", ")}` });
    if (active.label) chips.push({ key: "label", label: `Label: ${active.label}` });
    if (active.initiative) chips.push({ key: "initiative", label: `Initiative: ${active.initiative}` });
    if (active.assignee) chips.push({ key: "assignee", label: `Assignee: ${active.assignee}` });
    if (active.lanes !== "none") chips.push({ key: "lanes", label: `Swimlanes: ${active.lanes}` });
    return chips;
  });

  const removeFilter = (key: string) => {
    switch (key) {
      case "project": patch({ project: "" }); break;
      case "kind": patch({ kinds: [] }); break;
      case "state": patch({ states: [] }); break;
      case "label": patch({ label: "" }); break;
      case "initiative": patch({ initiative: "" }); break;
      case "assignee": patch({ assignee: "" }); break;
      case "lanes": patch({ lanes: "none" }); break;
    }
  };

  const kindOptions = createMemo(() =>
    (workflows() ?? []).map((workflow) => workflow.kind),
  );

  return (
    <div
      class={`vogt-surface board${outage() ? " board--outage" : ""}`}
      onKeyDown={onSurfaceKeyDown}
    >
      <SurfaceHeader
        class="board-header"
        label="Board header"
        /* The board's controls are its refresh cadence and a Refresh now:
           chrome over the cards, and on a phone the cards come first. */
        collapseControls

        title={<h1>Board</h1>}
        honestyClass={`surface-header-honesty--${freshness().tone === "live" ? "fresh" : freshness().tone === "outage" || freshness().tone === "waiting" ? "outage" : "stale"}`}
        honesty={(
          <div class="board-summary" aria-live="polite">
            <span>{items().length} loaded</span>
            <span>of {total()} matching</span>
            <span>{columns().length} columns</span>
            <ViewAgeBadge
              age={freshness()}
              class={`board-freshness board-freshness--${freshness().tone}`}
            />
          </div>
        )}
        controls={(
          <>
            <label class="board-field board-field--tight">
              <span>Refresh</span>
              <select
                value={String(filters().poll)}
                onInput={(event) =>
                  patch({ poll: Number.parseInt(event.currentTarget.value, 10) })
                }
              >
                <For each={POLL_CHOICES}>
                  {(seconds) => (
                    <option value={String(seconds)}>
                      {seconds === 0 ? "Paused" : `Every ${seconds}s`}
                    </option>
                  )}
                </For>
              </select>
            </label>
            <button onClick={() => void reload()} disabled={loading()}>
              {loading() ? "Loading…" : "Refresh now"}
            </button>
          </>
        )}
        action={(
          <button
            type="button"
            onClick={() => (createOpen() ? setCreateOpen(false) : openQuickCreate())}
            disabled={writesDisabled()}
            title={
              writesDisabled()
                ? "Vogt cannot be asked right now, so nothing can be raised"
                : "Raise a work item without leaving the board (n)"
            }
          >
            Quick create
          </button>
        )}
        detail={(
          <details class="surface-header-disclosure">
            <summary>How this view stays current</summary>
            <p class="board-note">
              Columns come from <code>workflow.list</code>; a drag is a{" "}
              <code>work.transition</code> and Vogt decides it. Changes arrive on
              the event stream and the poll below is the floor under it, so a
              dropped stream cannot look current indefinitely.
            </p>
          </details>
        )}
      />

      <Show when={outage()}>
        {(message) => (
          <div class="board-banner board-banner--outage" role="alert">
            <strong>Vogt is unreachable.</strong>
            <span>{message()}</span>
            <span class="board-banner-detail">
              Moves are disabled while this is true. Anything below is the last
              answer Vogt gave, not the current state of the estate.
            </span>
          </div>
        )}
      </Show>

      <Show when={!outage() && loadError()}>
        {(message) => (
          <div class="board-banner board-banner--error" role="alert">
            <strong>Last refresh failed.</strong>
            <span>{message()}</span>
          </div>
        )}
      </Show>

      <Show when={ack()}>
        {(message) => <div class="board-banner board-banner--ok">{message()}</div>}
      </Show>

      <ProgressiveFilters
        surface="Board"
        prefix="board"
        chips={activeFilterChips()}
        onRemove={removeFilter}
        onClear={clearFilters}
        clearDisabled={filterCount() === 0}
        actions={(
          <button type="button" onClick={hideFinishedColumns}>
            Hide finished columns
          </button>
        )}
        lenses={(
          /* FR-U14's second clause: a combined filter is a named lens. */
          <SavedLenses
            prefix="board"
            lenses={savedFilters().map((entry) => ({
              name: entry.name,
              title: describeSaved(entry),
            }))}
            onSave={saveCurrent}
            onRecall={recallSaved}
            onForget={forgetSaved}
            note={(
              <>
                saved lenses are kept in this browser · the URL above carries the
                same set to somebody else
              </>
            )}
          />
        )}
      >
        <label class="board-field">
          <span>Project</span>
          {/* Selection is declared on the options rather than on the select.
              A `value` set before `<For>` has appended its children is
              silently dropped, which is a hazard whenever the list arrives
              asynchronously — and it does, from `project.list`. */}
          <select
            onInput={(event) => patch({ project: event.currentTarget.value })}
          >
            <option value="" selected={!filters().project}>
              All projects
            </option>
            <Show when={filters().project && !projects().some((one) => one.slug === filters().project)}>
              <option value={filters().project} selected>
                {filters().project}
              </option>
            </Show>
            <For each={projects()}>
              {(project) => (
                <option
                  value={project.slug}
                  selected={project.slug === filters().project}
                >
                  {project.name}
                </option>
              )}
            </For>
          </select>
        </label>

        <label class="board-field">
          <span>Initiative</span>
          <select
            value={optionValue(
              filters().initiative,
              initiatives().map((one) => one.slug),
            )}
            onInput={(event) => patch({ initiative: event.currentTarget.value })}
          >
            <option value="">All initiatives</option>
            <Show when={filters().initiative && !initiatives().some((one) => one.slug === filters().initiative)}>
              <option value={filters().initiative}>{filters().initiative}</option>
            </Show>
            <For each={initiatives()}>
              {(initiative) => (
                <option value={initiative.slug}>{initiative.title}</option>
              )}
            </For>
          </select>
        </label>

        <label class="board-field">
          <span>Label</span>
          <select
            value={optionValue(filters().label, labels())}
            onInput={(event) => patch({ label: event.currentTarget.value })}
          >
            <option value="">All labels</option>
            <Show when={filters().label && !labels().includes(filters().label)}>
              <option value={filters().label}>{filters().label}</option>
            </Show>
            <For each={labels()}>{(name) => <option value={name}>{name}</option>}</For>
          </select>
        </label>

        <label class="board-field">
          <span>Assignee</span>
          <select
            value={optionValue(
              filters().assignee,
              actors().map((one) => one.identity_ref),
            )}
            onInput={(event) => patch({ assignee: event.currentTarget.value })}
          >
            <option value="">Anyone</option>
            <Show when={filters().assignee && !actors().some((one) => one.identity_ref === filters().assignee)}>
              <option value={filters().assignee}>{filters().assignee}</option>
            </Show>
            <For each={actors()}>
              {(actor) => (
                <option value={actor.identity_ref}>{actor.display_name}</option>
              )}
            </For>
          </select>
        </label>

        <label class="board-field">
          <span>Swimlanes</span>
          <select
            value={filters().lanes}
            onInput={(event) =>
              patch({ lanes: asLaneMode(event.currentTarget.value) })
            }
          >
            <option value="none">None</option>
            <option value="project">By project</option>
            <option value="initiative">By initiative</option>
          </select>
        </label>

        <div class="board-field board-field--wide">
          <span>Type</span>
          <div class="board-chips">
            <For each={kindOptions()}>
              {(kind) => (
                <button
                  type="button"
                  class={`board-chip${filters().kinds.includes(kind) ? " active" : ""}`}
                  aria-pressed={filters().kinds.includes(kind)}
                  onClick={() => toggleKind(kind)}
                >
                  {kind}
                </button>
              )}
            </For>
            <Show when={kindOptions().length === 0}>
              <span class="board-muted">no workflows loaded</span>
            </Show>
          </div>
        </div>

        <div class="board-field board-field--wide">
          <span>State</span>
          <div class="board-chips">
            <For each={allColumns()}>
              {(column) => (
                <button
                  type="button"
                  class={`board-chip${filters().states.includes(column.state) ? " active" : ""}`}
                  aria-pressed={filters().states.includes(column.state)}
                  onClick={() => {
                    const active = filters().states;
                    patch({
                      states: active.includes(column.state)
                        ? active.filter((state) => state !== column.state)
                        : [...active, column.state],
                    });
                  }}
                >
                  {humanState(column.state)}
                </button>
              )}
            </For>
          </div>
        </div>

      </ProgressiveFilters>


      <Show when={createOpen()}>
        <form class="board-create" onSubmit={(event) => void submitCreate(event)}>
          <div class="board-create-grid">
            <label class="board-field board-field--wide">
              <span>Title</span>
              <input
                type="text"
                required
                value={draftTitle()}
                placeholder="What needs doing"
                ref={(element) => queueMicrotask(() => element.focus())}
                onInput={(event) => setDraftTitle(event.currentTarget.value)}
              />
            </label>
            <label class="board-field">
              <span>Type</span>
              <select
                value={optionValue(draftKind(), kindChoices())}
                onInput={(event) => setDraftKind(event.currentTarget.value)}
              >
                <For each={kindChoices()}>
                  {(kind) => <option value={kind}>{kind}</option>}
                </For>
              </select>
            </label>
            <label class="board-field">
              <span>Project</span>
              <select
                value={optionValue(
                  draftProject(),
                  projects().map((one) => one.slug),
                )}
                onInput={(event) => setDraftProject(event.currentTarget.value)}
              >
                <option value="">No project</option>
                <For each={projects()}>
                  {(project) => <option value={project.slug}>{project.name}</option>}
                </For>
              </select>
            </label>
            <label class="board-field board-field--wide">
              <span>Reason (recorded in the audit trail)</span>
              <input
                type="text"
                required
                value={draftReason()}
                placeholder="Why is this being raised?"
                onInput={(event) => setDraftReason(event.currentTarget.value)}
              />
            </label>
          </div>
          <div class="board-create-actions">
            <button type="submit" disabled={!createReady() || creating()}>
              {creating() ? "Creating…" : "Create"}
            </button>
            <button
              type="button"
              onClick={() => setCreateOpen(false)}
              disabled={creating()}
            >
              Close
            </button>
            <span class="board-muted">
              Body, priority, effort, labels and assignee are set on the item
              itself.
            </span>
            <Show when={created()}>
              {(ref) => (
                <span class="board-created">
                  Created{" "}
                  <button
                    type="button"
                    class="board-card-ref"
                    onClick={() => openDetail(ref())}
                  >
                    {ref()}
                  </button>
                </span>
              )}
            </Show>
          </div>
          <Show when={createError()}>
            {(message) => (
              <p class="board-create-error" role="alert">
                {message()}
              </p>
            )}
          </Show>
        </form>
      </Show>

      <p class="board-keys">
        Keyboard: <kbd>Tab</kbd> to a card · <kbd>←</kbd> <kbd>→</kbd> across
        columns · <kbd>↑</kbd> <kbd>↓</kbd> within one · <kbd>Shift</kbd>+
        <kbd>←</kbd>/<kbd>→</kbd> proposes a move (same reason prompt as a drop)
        · <kbd>Enter</kbd> opens the item · <kbd>n</kbd> raises one.
      </p>

      <Show
        when={columns().length > 0}
        fallback={
          <div class="board-empty">
            <Show
              when={!outage()}
              fallback={<span>No workflow could be read, so there are no columns to draw.</span>}
            >
              <span>
                {workflows() === null
                  ? "Reading the workflows the columns come from…"
                  : "Vogt published no workflow states, so this board has no columns."}
              </span>
            </Show>
          </div>
        }
      >
        <Show when={phone()}>
          <div
            class="board-phone-states"
            role="group"
            aria-label="Board workflow state"
          >
            <For each={selectableColumns()}>
              {(column) => (
                <button
                  type="button"
                  class="board-phone-state"
                  aria-pressed={columns()[0]?.state === column.state}
                  onClick={() => patch({ column: column.state })}
                >
                  <span>{humanState(column.state)}</span>
                  <span class="board-phone-state-count">{columnCount(column.state)}</span>
                </button>
              )}
            </For>
          </div>
        </Show>
        <div class="board-scroll">
          <div class="board-grid">
            <div class="board-headrow" style={{ "grid-template-columns": gridTemplate() }}>
              <For each={columns()}>
                {(column) => (
                  <div
                    class={`board-colhead${collapsedColumn(column.state) ? " collapsed" : ""}${
                      column.known ? "" : " unknown"
                    }`}
                  >
                    <button
                      type="button"
                      class="board-colhead-toggle"
                      title={
                        collapsedColumn(column.state)
                          ? "Expand column"
                          : "Collapse column"
                      }
                      onClick={() => toggleColumn(column.state)}
                    >
                      {collapsedColumn(column.state) ? "»" : "«"}
                    </button>
                    <span class="board-colhead-name">{humanState(column.state)}</span>
                    <span class="board-wip" title="Work in progress: loaded items in this column">
                      {columnCount(column.state)}
                    </span>
                    <Show when={!collapsedColumn(column.state) && !column.known}>
                      <span class="board-colhead-note">
                        not in any workflow
                      </span>
                    </Show>
                  </div>
                )}
              </For>
            </div>

            <For each={lanes()}>
              {(lane) => (
                <>
                  <Show when={filters().lanes !== "none"}>
                    <button
                      type="button"
                      class="board-lanehead"
                      onClick={() => toggleLane(lane.key)}
                    >
                      <span class="board-lane-caret">
                        {collapsedLane(lane.key) ? "▸" : "▾"}
                      </span>
                      <span class="board-lane-label">{lane.label}</span>
                      <span class="board-lane-count">{laneCount(lane.key)}</span>
                    </button>
                  </Show>
                  <Show when={!collapsedLane(lane.key) || filters().lanes === "none"}>
                    <div
                      class="board-row"
                      style={{ "grid-template-columns": gridTemplate() }}
                      ref={(node) => {
                        if (cellPages().has(cellKey(lane.key, columns()[0]?.state ?? ""))) return;
                        if (typeof IntersectionObserver === "undefined") {
                          void loadLane(lane.key);
                          return;
                        }
                        const observer = new IntersectionObserver((entries) => {
                          if (entries.some((entry) => entry.isIntersecting)) {
                            void loadLane(lane.key);
                            observer.disconnect();
                          }
                        }, { root: node.closest(".board-scroll"), rootMargin: "480px" });
                        observer.observe(node);
                        onCleanup(() => observer.disconnect());
                      }}
                    >
                      <For each={columns()}>
                        {(column) => {
                          /** Which cell the pointer is over, for the drop
                           *  highlight. Not `cellKey`: that one identifies a
                           *  cell to the projection and this one identifies
                           *  it to a drag, and they are allowed to differ. */
                          const dropKey = `${lane.key}||${column.state}`;
                          const mine = cellKey(lane.key, column.state);
                          const cards = createMemo(() => cellItems(lane, column.state));
                          const pageState = createMemo(() => cellPages().get(mine));

                          // -- measured windowing (NFR-S5 / FR-U25) --------
                          // The estimate is only the layout-free seed. Each
                          // rendered card reports its content height back to
                          // the keyed prefix index, so expansion and width
                          // changes alter the runway without clipping a card.
                          const [scrollTop, setScrollTop] = createSignal(
                            cellScroll.get(mine) ?? 0,
                          );
                          const [viewport, setViewport] = createSignal(0);
                          const measured = new MeasuredWindow(CARD_ESTIMATE + CARD_SPACING_PX);
                          const [measurementVersion, setMeasurementVersion] = createSignal(0);
                          const totalHeight = createMemo(() => {
                            measurementVersion();
                            return measured.totalHeight();
                          });
                          createEffect(() => {
                            if (measured.setKeys(cards().map((item) => item.ref))) {
                              setMeasurementVersion((version) => version + 1);
                            }
                          });

                          const windowed = createMemo(() => cards().length > VIRTUALIZE_ABOVE);

                          /** Where the focused card sits in *this* cell, or
                           *  -1. A `Map` read, so every cell on the board can
                           *  ask it on every focus change. */
                          const focusHere = createMemo(() => {
                            const ref = focusedRef();
                            if (!ref) return -1;
                            const at = projection().where.get(ref);
                            return at && at.cell === mine ? at.index : -1;
                          });

                          const slice = createMemo(() => {
                            measurementVersion();
                            const all = cards();
                            if (!windowed()) {
                              return { start: 0, end: all.length, top: 0 };
                            }
                            const range = measured.range(
                              scrollTop(),
                              viewport() || DEFAULT_VIEWPORT_PX,
                              OVERSCAN_PX,
                            );
                            let start = range.start;
                            let end = range.end;
                            // FR-U22 through a windowed column: an element
                            // that is not rendered cannot take focus, so the
                            // card the keyboard is moving to is always in the
                            // slice — even when the scroll offset says
                            // otherwise, which it does until the browser has
                            // scrolled it into view.
                            const at = focusHere();
                            if (at >= 0 && (at < start || at >= end)) {
                              start = Math.max(0, at - 4);
                              end = Math.min(
                                all.length,
                                Math.max(start + 1, measured.range(measured.offsetOf(at), viewport() || DEFAULT_VIEWPORT_PX, OVERSCAN_PX).end),
                              );
                            }
                            return { start, end, top: measured.offsetOf(start) };
                          });

                          const windowCards = createMemo(() => {
                            const { start, end } = slice();
                            const all = cards();
                            return start === 0 && end === all.length
                              ? all
                              : all.slice(start, end);
                          });

                          const move = createMemo(() => {
                            const current = pending();
                            return current &&
                              current.to === column.state &&
                              current.laneKey === lane.key
                              ? current
                              : null;
                          });
                          const refused = createMemo(() => {
                            const current = refusal();
                            return current &&
                              current.to === column.state &&
                              current.laneKey === lane.key
                              ? current
                              : null;
                          });
                          const unlisted = createMemo(() => {
                            const legal = legalTargets();
                            return legal !== null && !legal.has(column.state);
                          });
                          return (
                            <div
                              class={`board-cell${collapsedColumn(column.state) ? " collapsed" : ""}${
                                dragOver() === dropKey ? " dropping" : ""
                              }${dragRef() && unlisted() ? " unlisted" : ""}`}
                              // Below the narrow breakpoint the board is a
                              // list and the column head row is not rendered
                              // (FR-M3), so the state a cell belongs to has
                              // to travel on the cell itself — `styles.css`
                              // draws these two as its heading. They are
                              // duplicated from the head row rather than
                              // moved: at desk widths the head row is still
                              // the thing that labels the column.
                              data-state={humanState(column.state)}
                              data-wip={String(columnCount(column.state))}
                              onDragOver={(event) => {
                                if (!dragRef() || writesDisabled()) return;
                                event.preventDefault();
                                if (event.dataTransfer) {
                                  event.dataTransfer.dropEffect = "move";
                                }
                                setDragOver(dropKey);
                              }}
                              onDragLeave={() => {
                                if (dragOver() === dropKey) setDragOver(null);
                              }}
                              onDrop={(event) => {
                                event.preventDefault();
                                setDragOver(null);
                                const ref =
                                  dragRef() ??
                                  event.dataTransfer?.getData("text/plain") ??
                                  null;
                                setDragRef(null);
                                if (!ref) return;
                                const item = itemByRef().get(ref);
                                if (!item) return;
                                beginMove(item, column.state);
                              }}
                            >
                              <Show when={!collapsedColumn(column.state)}>
                                <Show when={dragRef() && unlisted()}>
                                  <div class="board-hint">
                                    not a listed edge — Vogt still decides
                                  </div>
                                </Show>

                                <Show when={move()}>
                                  {(current) => (
                                    <form
                                      class="board-composer"
                                      onSubmit={(event) => {
                                        event.preventDefault();
                                        void commitMove();
                                      }}
                                    >
                                      <div class="board-composer-head">
                                        <strong>{current().ref}</strong>
                                        <span>
                                          {humanState(current().from)} →{" "}
                                          {humanState(current().to)}
                                        </span>
                                      </div>
                                      <textarea
                                        rows={2}
                                        ref={(element) =>
                                          queueMicrotask(() => {
                                            element.focus();
                                            element.select();
                                          })
                                        }
                                        placeholder="Why is this moving? Vogt audits it."
                                        value={current().reason}
                                        disabled={current().phase === "saving"}
                                        onInput={(event) => {
                                          const open = pending();
                                          if (!open) return;
                                          setPending({
                                            ...open,
                                            reason: event.currentTarget.value,
                                          });
                                        }}
                                        onKeyDown={(event) => {
                                          if (event.key === "Escape") {
                                            event.preventDefault();
                                            cancelMove();
                                            return;
                                          }
                                          if (event.key === "Enter" && !event.shiftKey) {
                                            event.preventDefault();
                                            void commitMove();
                                          }
                                        }}
                                      />
                                      <div class="board-composer-actions">
                                        <button
                                          type="submit"
                                          disabled={
                                            !current().reason.trim() ||
                                            current().phase === "saving"
                                          }
                                        >
                                          {current().phase === "saving"
                                            ? "Moving…"
                                            : "Move"}
                                        </button>
                                        <button
                                          type="button"
                                          onClick={cancelMove}
                                          disabled={current().phase === "saving"}
                                        >
                                          Cancel
                                        </button>
                                        <span class="board-composer-hint">
                                          Enter moves · Esc puts it back
                                        </span>
                                      </div>
                                    </form>
                                  )}
                                </Show>

                                <Show when={refused()}>
                                  {(current) => (
                                    <div class="board-refusal" role="alert">
                                      <div class="board-refusal-head">
                                        <strong>Vogt refused this move</strong>
                                        <button
                                          type="button"
                                          class="board-refusal-close"
                                          onClick={() => setRefusal(null)}
                                          title="Dismiss"
                                        >
                                          ×
                                        </button>
                                      </div>
                                      <p class="board-refusal-message">
                                        {current().message}
                                      </p>
                                      <p class="board-refusal-detail">
                                        {current().ref} is back in{" "}
                                        <b>
                                          {humanState(
                                            itemByRef().get(current().ref)?.state ??
                                              current().from,
                                          )}
                                        </b>
                                        .
                                      </p>
                                      <button type="button" onClick={retryRefusal}>
                                        Try again
                                      </button>
                                    </div>
                                  )}
                                </Show>

                                <div
                                  class="board-cell-cards"
                                  ref={(node) => {
                                    const observer = new ResizeObserver((entries) => {
                                      const height = entries[0]?.contentRect.height ?? node.clientHeight;
                                      if (height > 0) setViewport(height);
                                    });
                                    observer.observe(node);
                                    queueMicrotask(() => {
                                      // A rebuilt cell is a fresh element at
                                      // the top of its list; put it back
                                      // where the reader left it. Not before
                                      // the microtask: an offset set on a
                                      // node that is not in the document yet
                                      // does not stick.
                                      const at = cellScroll.get(mine) ?? 0;
                                      if (at) node.scrollTop = at;
                                      setViewport(node.clientHeight);
                                    });
                                    onCleanup(() => observer.disconnect());
                                  }}
                                  onScroll={(event) => {
                                    const at = event.currentTarget.scrollTop;
                                    cellScroll.set(mine, at);
                                    setScrollTop(at);
                                    if (
                                      event.currentTarget.scrollHeight -
                                        event.currentTarget.clientHeight -
                                        at <
                                      OVERSCAN_PX * 2
                                    ) {
                                      void loadNextCell(lane.key, column.state);
                                    }
                                  }}
                                >
                                  {/* The full height of everything the filter
                                      matched, so the scrollbar is the length
                                      of the column and not the length of the
                                      window. */}
                                  <div
                                    class="board-cell-run"
                                    style={{
                                      height: `${totalHeight()}px`,
                                    }}
                                  >
                                    <div
                                      class="board-cell-window"
                                      style={{ top: `${slice().top}px` }}
                                    >
                                      <For each={windowCards()}>
                                        {(item) => {
                                          const isPending = createMemo(
                                            () => pending()?.ref === item.ref,
                                          );
                                          return (
                                            <div
                                              id={cardDomId(item.ref)}
                                              class={`board-card${isPending() ? " board-card--pending" : ""}${
                                                dragRef() === item.ref ? " board-card--dragging" : ""
                                              }${focusedRef() === item.ref ? " board-card--focused" : ""}${
                                                bounced() === item.ref ? " board-card--bounced" : ""
                                              }`}
                                              ref={(node) => {
                                                const observer = new ResizeObserver((entries) => {
                                                  const entry = entries[0];
                                                  const box = entry?.borderBoxSize;
                                                  const borderHeight = Array.isArray(box)
                                                    ? box[0]?.blockSize
                                                    : box && typeof box === "object" && "blockSize" in box
                                                      ? box.blockSize
                                                      : undefined;
                                                  const cardHeight =
                                                    borderHeight ?? entry?.contentRect.height ?? node.getBoundingClientRect().height;
                                                  if (cardHeight <= 0) return;
                                                  const change = measured.measure(
                                                    item.ref,
                                                    cardHeight + CARD_SPACING_PX,
                                                  );
                                                  if (change) {
                                                    setMeasurementVersion((version) => version + 1);
                                                    if (change.top < scrollTop()) {
                                                      const next = scrollTop() + change.delta;
                                                      cellScroll.set(mine, next);
                                                      setScrollTop(next);
                                                      queueMicrotask(() => {
                                                        const scroller = node.closest<HTMLElement>(".board-cell-cards");
                                                        if (scroller) scroller.scrollTop = next;
                                                      });
                                                    }
                                                  }
                                                });
                                                observer.observe(node);
                                                queueMicrotask(() => {
                                                  const cardHeight = node.getBoundingClientRect().height;
                                                  if (cardHeight > 0) {
                                                    const change = measured.measure(
                                                      item.ref,
                                                      cardHeight + CARD_SPACING_PX,
                                                    );
                                                    if (change) setMeasurementVersion((version) => version + 1);
                                                  }
                                                });
                                                onCleanup(() => observer.disconnect());
                                              }}
                                              role="button"
                                              tabindex={0}
                                              draggable={!writesDisabled() && !pending()}
                                              onFocus={() => setFocusedRef(item.ref)}
                                              onDragStart={(event) => {
                                                if (writesDisabled() || pending()) {
                                                  event.preventDefault();
                                                  return;
                                                }
                                                setDragRef(item.ref);
                                                setRefusal(null);
                                                if (event.dataTransfer) {
                                                  event.dataTransfer.effectAllowed = "move";
                                                  event.dataTransfer.setData(
                                                    "text/plain",
                                                    item.ref,
                                                  );
                                                }
                                              }}
                                              onDragEnd={() => {
                                                setDragRef(null);
                                                setDragOver(null);
                                              }}
                                              onKeyDown={(event) =>
                                                onCardKeyDown(event, item, lane)
                                              }
                                              onDblClick={() => openDetail(item.ref)}
                                            >
                                              <div class="board-card-top">
                                                <button
                                                  type="button"
                                                  class="board-card-ref"
                                                  onClick={() => openDetail(item.ref)}
                                                >
                                                  {item.ref}
                                                </button>
                                                <span
                                                  class={`board-pri board-pri--${item.priority}`}
                                                >
                                                  {item.priority}
                                                </span>
                                                {/* FR-U17: never blank, and
                                                    `unverified` when nobody has
                                                    said otherwise. */}
                                                <span
                                                  class={`board-trust trust-${trustOf(item)}`}
                                                  title={`trust: ${trustOf(item)}`}
                                                >
                                                  {trustOf(item)}
                                                </span>
                                                <span class="board-kind">{item.kind}</span>
                                              </div>
                                              <div
                                                id={`${cardDomId(item.ref)}-content`}
                                                class="board-card-content"
                                              >
                                                <div
                                                  class={`board-card-title${cardExpanded(item.ref) ? " expanded" : ""}`}
                                                  ref={(node) => {
                                                    const inspect = () => {
                                                      if (cardExpanded(item.ref)) return;
                                                      noteTitleClipping(
                                                        item.ref,
                                                        node.scrollHeight > node.clientHeight + 1,
                                                      );
                                                    };
                                                    const observer = new ResizeObserver(inspect);
                                                    observer.observe(node);
                                                    queueMicrotask(inspect);
                                                    onCleanup(() => observer.disconnect());
                                                  }}
                                                >
                                                  {item.title}
                                                </div>
                                                <Show when={cardExpanded(item.ref) && item.body?.trim()}>
                                                  <p class="board-card-body">{item.body}</p>
                                                </Show>
                                              </div>
                                              <div class="board-card-meta">
                                                <Show when={filters().lanes !== "project" && item.project_slug}>
                                                  <span>{item.project_slug}</span>
                                                </Show>
                                                <Show when={item.assignee_identity_ref}>
                                                  <span>{item.assignee_identity_ref}</span>
                                                </Show>
                                                <Show when={item.effort}>
                                                  <span>{item.effort}</span>
                                                </Show>
                                                <Show when={isPending()}>
                                                  <span class="board-card-unsaved">
                                                    unsaved — needs a reason
                                                  </span>
                                                </Show>
                                              </div>
                                              <Show when={(item.labels ?? []).length > 0}>
                                                <div class="board-card-labels">
                                                  <For each={item.labels ?? []}>
                                                    {(label) => (
                                                      <span class="board-label">{label}</span>
                                                    )}
                                                  </For>
                                                </div>
                                              </Show>
                                              <Show when={cardExpandable(item)}>
                                                <button
                                                  type="button"
                                                  class="board-card-expand"
                                                  aria-expanded={cardExpanded(item.ref)}
                                                  aria-controls={`${cardDomId(item.ref)}-content`}
                                                  onClick={(event) => {
                                                    event.stopPropagation();
                                                    toggleCardExpanded(item.ref);
                                                  }}
                                                >
                                                  {cardExpanded(item.ref) ? "Show less" : "Show more"}
                                                </button>
                                              </Show>
                                            </div>
                                          );
                                        }}
                                      </For>
                                    </div>
                                  </div>
                                </div>

                                <Show when={pageState()?.loading}>
                                  <div class="board-cell-status">Loading more…</div>
                                </Show>
                                <Show when={pageState()?.error}>
                                  {(message) => (
                                    <div class="board-cell-status board-cell-status--error" role="alert">
                                      <span>{message()}</span>
                                      <button
                                        type="button"
                                        onClick={() => void loadNextCell(lane.key, column.state)}
                                      >
                                        Retry
                                      </button>
                                    </div>
                                  )}
                                </Show>
                                <Show when={cards().length === 0 && !move() && !refused() && pageState()}>
                                  <div class="board-cell-empty">Nothing here</div>
                                </Show>
                                <Show when={!pageState()}>
                                  <div class="board-cell-status">Loading cell…</div>
                                </Show>
                              </Show>
                              <Show when={collapsedColumn(column.state)}>
                                <div class="board-cell-collapsed">
                                  {pageState()?.total ?? 0}
                                </div>
                              </Show>
                            </div>
                          );
                        }}
                      </For>
                    </div>
                  </Show>
                </>
              )}
            </For>
          </div>
        </div>
      </Show>
    </div>
  );
};

export default Board;
