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
//  5. **Bounded reads.** `work.list` is paged and ordered oldest-first, so a
//     truncated read is a *misleading* read on a board. The load pages until
//     the filter is exhausted or a cap is reached, and if the cap wins the
//     board says so and stops claiming its column counts are counts of the
//     estate (NFR-S5).
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
//     a fixed card height pinned by `styles.css`, a `ResizeObserver` on the
//     cell's own scroller, and a slice of the list with overscan either
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
import { onVogtChanged } from "./store";
import { openWorkItemTab } from "./tabs";
import {
  VogtUnavailable,
  createWork,
  listActors,
  listInitiatives,
  listLabels,
  listProjects,
  listWork,
  listWorkflows,
  transitionWork,
  type WorkItem,
  type Workflow,
  type WorkflowState,
} from "./vogtApi";

interface Props {
  onError?: (message: string) => void;
}

/** Grouping for swimlanes (FR-U13). */
type LaneMode = "none" | "project" | "initiative";

/** One page of `work.list`; the server caps this at 500. */
const PAGE_SIZE = 500;

/** How much of a filter the board will pull before it admits truncation. */
const MAX_ITEMS = 2000;

/**
 * Card height in px, and the gap under it. Windowing needs them fixed; the
 * CSS pins them too.
 *
 * `--board-card-h` and `--board-card-gap` in `styles.css` must match these,
 * exactly as `--vogt-row-h` must match `Backlog.tsx`'s `ROW_HEIGHT`: the
 * window's arithmetic is the only thing holding the scroll offset and the
 * drawn cards in agreement, and it has no way to notice if the stylesheet
 * disagrees with it.
 */
const CARD_HEIGHT = 116;
const CARD_GAP = 8;

/** What one card costs a column, top to top. Exported because it is the one
 *  number a test can use to check that the scrollbar is as long as the whole
 *  column rather than as long as the window. */
export const CARD_SLOT = CARD_HEIGHT + CARD_GAP;

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

/** Cards drawn beyond each edge of the cell, so a fast scroll is not blank. */
const OVERSCAN = 6;

/** The window's height when nothing has measured the cell yet — a screenful
 *  of cards, and the only thing a layout-free environment can assume. */
const FALLBACK_CARDS = 8;

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

function formatAgo(ms: number): string {
  const seconds = Math.max(0, Math.round(ms / 1000));
  if (seconds < 60) return `${seconds}s ago`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  return `${Math.floor(hours / 24)}d ago`;
}

function formatClock(at: number): string {
  try {
    return new Date(at).toLocaleTimeString();
  } catch {
    return String(at);
  }
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
  const [items, setItems] = createSignal<WorkItem[]>([]);
  const [total, setTotal] = createSignal(0);
  const [loadedAt, setLoadedAt] = createSignal<number | null>(null);
  const [loading, setLoading] = createSignal(false);
  const [outage, setOutage] = createSignal<string | null>(null);
  const [loadError, setLoadError] = createSignal<string | null>(null);
  const [now, setNow] = createSignal(Date.now());

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
  // Guarded on the pathname because every tab in this shell is mounted at
  // once and `project`, `label` and `assignee` are keys more than one Vogt
  // surface owns.
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

  const loadItems = async (quiet = false) => {
    const seq = ++loadSeq;
    if (!quiet) setLoading(true);
    const active = filters();
    try {
      const collected: WorkItem[] = [];
      let reported = 0;
      for (let offset = 0; offset < MAX_ITEMS; offset += PAGE_SIZE) {
        const page = await listWork({
          project: active.project || undefined,
          kinds: active.kinds.length ? active.kinds : undefined,
          states: active.states.length ? active.states : undefined,
          label: active.label || undefined,
          initiative: active.initiative || undefined,
          assignee: active.assignee || undefined,
          // Always: the board draws a column for every workflow state, and a
          // finished column that is empty because the *query* dropped it is a
          // lie. Which states are shown is the `state` filter's job, and that
          // is in the URL where it can be read.
          include_finished: true,
          limit: PAGE_SIZE,
          offset,
        });
        collected.push(...(page.items ?? []));
        reported = page.total ?? collected.length;
        if ((page.items ?? []).length < PAGE_SIZE) break;
        if (collected.length >= reported) break;
      }
      if (seq !== loadSeq) return;
      setItems(mergeItems(collected));
      setTotal(Math.max(reported, collected.length));
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

  const reload = async () => {
    await Promise.all([loadWorkflows(), loadItems()]);
  };

  onMount(() => {
    void loadTaxonomy();
    void reload();
  });

  // Refetch whenever the server-side half of the filter changes. The lane
  // mode and poll interval are client-side and deliberately not in here.
  createEffect<string>((previous) => {
    const active = filters();
    const key = JSON.stringify([
      active.project,
      active.kinds,
      active.states,
      active.label,
      active.initiative,
      active.assignee,
    ]);
    if (previous !== undefined && previous !== key) void loadItems();
    return key;
  });

  // The freshness clock, and the poll it describes.
  onMount(() => {
    const tick = window.setInterval(() => setNow(Date.now()), 1000);
    onCleanup(() => window.clearInterval(tick));
  });

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

  onMount(() => {
    const onVisible = () => {
      if (document.visibilityState === "visible") void loadItems(true);
    };
    document.addEventListener("visibilitychange", onVisible);
    onCleanup(() => document.removeEventListener("visibilitychange", onVisible));
  });

  // Vogt's own changes, pushed. The front door follows the core's event
  // cursor and republishes onto the stream this client already has open
  // (FR-U10), so a transition somebody else made arrives here rather than
  // waiting for the next poll. The poll stays as the floor: this stream can
  // drop, and a board that stopped refreshing because a socket died would be
  // stale while looking current — which is the thing the requirement is
  // actually about.
  onMount(() => {
    const stop = onVogtChanged(() => {
      if (typeof document !== "undefined" && document.hidden) return;
      if (pending()) return; // never race a write the user is composing
      void loadItems(true);
    });
    onCleanup(stop);
  });

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
      items().map((item) => item.state),
    ),
  );

  const columns = createMemo<Column[]>(() => {
    const wanted = filters().states;
    if (!wanted.length) return allColumns();
    const shown = allColumns().filter((column) => wanted.includes(column.state));
    return shown.length ? shown : allColumns();
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

  const lanes = createMemo<Lane[]>(() => {
    const grouped = new Map<string, Lane>();
    for (const item of items()) {
      const { key, label } = laneOf(item);
      const lane = grouped.get(key) ?? { key, label, items: [] };
      lane.items.push(item);
      grouped.set(key, lane);
    }
    if (grouped.size === 0) {
      grouped.set("", { key: "", label: filters().lanes === "none" ? "All work" : "No matching work", items: [] });
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

  const columnCount = (state: string) => projection().counts.get(state) ?? 0;

  const truncated = createMemo(() => total() > items().length);

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
   *  Scoped to the surface rather than to `document`: every tab in this shell
   *  is mounted at once, so a document-level `n` would raise a work item
   *  while somebody was reading a terminal. It listens on the board's own
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

  const freshness = createMemo(() => {
    const at = loadedAt();
    if (outage()) {
      return {
        tone: "outage",
        text: at
          ? `Vogt unreachable — last answer ${formatClock(at)}, not current`
          : "Vogt unreachable",
      };
    }
    if (!at) return { tone: "waiting", text: "Not loaded yet" };
    const age = now() - at;
    if (filters().poll === 0) {
      return { tone: "paused", text: `Paused — updated ${formatAgo(age)}` };
    }
    if (loadError()) {
      return { tone: "stale", text: `Stale — updated ${formatAgo(age)}, retrying` };
    }
    const overdue = age > filters().poll * 3000;
    return {
      tone: overdue ? "stale" : "live",
      text: `${overdue ? "Stale" : "Polling"} — updated ${formatAgo(age)}`,
    };
  });

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
  const [saveName, setSaveName] = createSignal("");

  const saveCurrent = () => {
    const name = saveName().trim();
    if (!name) return;
    // The poll interval is a refresh preference, not a filter, so it is left
    // out of what gets saved and left alone on recall: a named view should
    // not change how often the reader's board refreshes.
    const query = encodeFilters({ ...filters(), poll: DEFAULT_POLL_SECONDS });
    setSavedFilters(
      writeSavedFilters([
        { name, query },
        ...savedFilters().filter((entry) => entry.name !== name),
      ]),
    );
    setSaveName("");
  };

  const recallSaved = (entry: SavedFilter) => {
    applyFilters({
      ...filtersFromQuery(queryFromSearch(entry.query)),
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
      (active.assignee ? 1 : 0)
    );
  });

  const kindOptions = createMemo(() =>
    (workflows() ?? []).map((workflow) => workflow.kind),
  );

  return (
    <div
      class={`vogt-surface board${outage() ? " board--outage" : ""}`}
      onKeyDown={onSurfaceKeyDown}
    >
      <header class="board-header">
        <div class="board-heading">
          <h2>Board</h2>
          <div class="board-summary">
            <span>{items().length} loaded</span>
            <Show when={truncated()}>
              <span>of {total()} matching</span>
            </Show>
            <span>{columns().length} columns</span>
            <span class={`board-freshness board-freshness--${freshness().tone}`}>
              {freshness().text}
            </span>
          </div>
          <p class="board-note">
            Columns come from <code>workflow.list</code>; a drag is a{" "}
            <code>work.transition</code> and Vogt decides it. Changes arrive on
            the event stream and the poll below is the floor under it — a
            stream can drop, and a board that stopped refreshing because a
            socket died would be stale while looking current.
          </p>
        </div>
        <div class="board-header-actions">
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
        </div>
      </header>

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

      <Show when={truncated()}>
        <div class="board-banner board-banner--warn">
          <strong>Showing {items().length} of {total()} matching items.</strong>
          <span>
            <code>work.list</code> returns oldest first, so this is the oldest
            slice of the filter, not a sample of it. Column counts below count
            what is loaded. Narrow the filters to see the whole set.
          </span>
        </div>
      </Show>

      <Show when={ack()}>
        {(message) => <div class="board-banner board-banner--ok">{message()}</div>}
      </Show>

      <div class="board-toolbar">
        <label class="board-field">
          <span>Project</span>
          <select
            value={optionValue(
              filters().project,
              projects().map((one) => one.slug),
            )}
            onInput={(event) => patch({ project: event.currentTarget.value })}
          >
            <option value="">All projects</option>
            <Show when={filters().project && !projects().some((one) => one.slug === filters().project)}>
              <option value={filters().project}>{filters().project}</option>
            </Show>
            <For each={projects()}>
              {(project) => <option value={project.slug}>{project.name}</option>}
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
          <span>Kind</span>
          <div class="board-chips">
            <For each={kindOptions()}>
              {(kind) => (
                <button
                  type="button"
                  class={`board-chip${filters().kinds.includes(kind) ? " active" : ""}`}
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

        <div class="board-toolbar-actions">
          <button type="button" onClick={hideFinishedColumns}>
            Hide finished columns
          </button>
          <button type="button" onClick={clearFilters} disabled={filterCount() === 0}>
            Clear filters ({filterCount()})
          </button>
        </div>
      </div>

      {/* FR-U14's second clause: a combined filter is nameable and recalled. */}
      <div class="board-savedrow">
        <input
          type="text"
          class="board-savedname"
          placeholder="Name this filter set"
          value={saveName()}
          onInput={(event) => setSaveName(event.currentTarget.value)}
          onKeyDown={(event) => {
            if (event.key !== "Enter") return;
            event.preventDefault();
            saveCurrent();
          }}
        />
        <button type="button" onClick={saveCurrent} disabled={!saveName().trim()}>
          Save filter
        </button>
        <For each={savedFilters()}>
          {(entry) => (
            <span class="board-saved">
              <button
                type="button"
                class="board-saved-recall"
                title={describeSaved(entry)}
                onClick={() => recallSaved(entry)}
              >
                {entry.name}
              </button>
              <button
                type="button"
                class="board-saved-drop"
                aria-label={`Forget the saved filter ${entry.name}`}
                onClick={() => forgetSaved(entry.name)}
              >
                ×
              </button>
            </span>
          )}
        </For>
        <span class="board-muted">
          saved filters are kept in this browser · the URL above carries the
          same set to somebody else
        </span>
      </div>

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
                      {truncated() ? "+" : ""}
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
                      <span class="board-lane-count">{lane.items.length}</span>
                    </button>
                  </Show>
                  <Show when={!collapsedLane(lane.key) || filters().lanes === "none"}>
                    <div
                      class="board-row"
                      style={{ "grid-template-columns": gridTemplate() }}
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

                          // -- windowing (NFR-S5) -------------------------
                          //
                          // `Backlog.tsx`'s mechanism, on a cell instead of
                          // a page: a fixed card height, a `ResizeObserver`
                          // on the cell's own scroller, and a slice with
                          // overscan either side. The observer rather than a
                          // window listener for the reason the backlog gives
                          // — a board in an unselected tab is `display:none`
                          // with a height of zero, and comes back without a
                          // resize event ever firing.

                          // Seeded from `cellScroll`, and that is the whole
                          // reason `cellScroll` exists — see it for why a
                          // cell cannot keep its own offset.
                          const [scrollTop, setScrollTop] = createSignal(
                            cellScroll.get(mine) ?? 0,
                          );
                          const [viewport, setViewport] = createSignal(0);

                          const windowed = createMemo(
                            () => cards().length > VIRTUALIZE_ABOVE,
                          );

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
                            const all = cards();
                            if (!windowed()) return { start: 0, end: all.length };
                            const height = viewport() || CARD_SLOT * FALLBACK_CARDS;
                            const count =
                              Math.ceil(height / CARD_SLOT) + OVERSCAN * 2;
                            const last = Math.max(0, all.length - count);
                            let start = Math.min(
                              last,
                              Math.max(0, Math.floor(scrollTop() / CARD_SLOT) - OVERSCAN),
                            );
                            let end = Math.min(all.length, start + count);
                            // FR-U22 through a windowed column: an element
                            // that is not rendered cannot take focus, so the
                            // card the keyboard is moving to is always in the
                            // slice — even when the scroll offset says
                            // otherwise, which it does until the browser has
                            // scrolled it into view.
                            const at = focusHere();
                            if (at >= 0 && (at < start || at >= end)) {
                              start = Math.max(0, Math.min(at - OVERSCAN, last));
                              end = Math.min(all.length, start + count);
                            }
                            return { start, end };
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
                              data-wip={`${columnCount(column.state)}${truncated() ? "+" : ""}`}
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
                                    const observer = new ResizeObserver(() =>
                                      setViewport(node.clientHeight),
                                    );
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
                                  }}
                                >
                                  {/* The full height of everything the filter
                                      matched, so the scrollbar is the length
                                      of the column and not the length of the
                                      window. */}
                                  <div
                                    class="board-cell-run"
                                    style={{
                                      height: `${cards().length * CARD_SLOT}px`,
                                    }}
                                  >
                                    <div
                                      class="board-cell-window"
                                      style={{ top: `${slice().start * CARD_SLOT}px` }}
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
                                              <div class="board-card-title">{item.title}</div>
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
                                            </div>
                                          );
                                        }}
                                      </For>
                                    </div>
                                  </div>
                                </div>

                                <Show when={cards().length === 0 && !move() && !refused()}>
                                  <div class="board-cell-empty">Nothing here</div>
                                </Show>
                              </Show>
                              <Show when={collapsedColumn(column.state)}>
                                <div class="board-cell-collapsed">
                                  {cards().length}
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
