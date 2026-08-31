// The ranked backlog and bugs (FR-U6, FR-U14, FR-U15, NFR-S5).
//
// One tab, two ranked views over the same filter set. Four things this file
// exists to keep, in the order they are easy to lose:
//
//   1. **The ranking is explainable.** A score with no explanation is a
//      number the reader has to take on faith, which is the thing `why`
//      exists to prevent. Every row can open its contributions, and two rows
//      can be opened together so "why is this above that" has an answer with
//      a per-input delta rather than a shrug.
//   2. **A filtered view is a place.** Filters live in the URL (FR-U11), so
//      a reload restores them and a link carries them to somebody else. The
//      local signal is the source of truth and the URL is written from it;
//      the effect under "URL" says why it cannot be the other way round in
//      this shell.
//   3. **A write says who asked for it.** Quick-create and bulk transition
//      collect a reason the *user* typed and refuse to submit without one
//      (FR-W1, r6). Neither has a default, a placeholder that doubles as a
//      value, or a path that submits an empty string.
//   4. **Absence is stated, never rendered as emptiness.** Freshness on every
//      aggregate, a trust state on every row that is never blank, and an
//      unreachable Vogt that says so with the server's own reason (FR-U2,
//      FR-U17, FR-U21).
//
// Everything here reaches Vogt through `vogtApi.ts`; there is no fetch in
// this file and there must not be one — `tests/test_pwa.py` is what says so.

import {
  Component,
  For,
  Show,
  createEffect,
  createMemo,
  createResource,
  createSignal,
  on,
  onCleanup,
  onMount,
} from "solid-js";
import { useLocation, useNavigate, useSearchParams } from "@solidjs/router";
import {
  VogtUnavailable,
  adoptSubject,
  backlog,
  bugs,
  createWork,
  startSession,
  suppressSubject,
  transitionWork,
  updateWork,
  why,
  type RankedEntry,
  type RankedView,
} from "./vogtApi";
import { taxonomy } from "./taxonomyCache";
import { openWorkItemTab } from "./tabs";
import {
  ViewAgeBadge,
  createLoadStamp,
  createViewAge,
  honestyToneClass,
  onVogtLive,
} from "./viewAge";
import { MeasuredWindow } from "./measuredWindow";
import SurfaceHeader from "./SurfaceHeader";
import { ProgressiveFilters, SavedLenses } from "./ProgressiveFilters";
import { actorName as resolveActorName, projectName as resolveProjectName } from "./refNames";

interface Props {
  onError?: (message: string) => void;
}

// -- constants --------------------------------------------------------------

/** Saved filters are per-client state in v2 (`REQUIREMENTS.md` §3, FR-U14). */
const SAVED_FILTERS_KEY = "vogt.vogtSavedFilters.v1";

/** Enough that a runaway list cannot be saved into a full storage quota. */
const MAX_SAVED_FILTERS = 40;

/** Layout-free seed only; actual rows report their measured border-box height. */
export const ROW_ESTIMATE = 40;

/**
 * Below this many rows the list renders whole.
 *
 * Virtualization costs the browser's own find-in-page and costs a reader the
 * ability to select the list — worth paying at length, not worth paying for
 * a screenful. Above it, only the visible window plus `OVERSCAN` is in the
 * DOM, so the row count stops driving layout cost (NFR-S5).
 */
const VIRTUALIZE_ABOVE = 60;

/** Rows rendered beyond each edge of the viewport, so a fast scroll is not blank. */

/** `BacklogParams.limit` is capped at 200 server-side; offering more would lie. */
const PAGE_SIZES = [50, 100, 200] as const;
const MAX_PAGE_SIZE = 200;
const DEFAULT_PAGE_SIZE = 100;

/** Kinds are a closed set in Vogt (`WorkKind`), so the picker is not derived. */
const WORK_KINDS = ["feature", "bug", "chore", "question"] as const;

/** The URL keys this surface owns. Anything else in the query is left alone.
 *
 *  `q` is the free-text search (#350); the `not_*` keys carry the exclusion
 *  set (#351). An older build has none of them in its own `URL_KEYS`, so it
 *  reads them as keys it does not own and drops them — the tolerance rule. */
const URL_KEYS = [
  "view",
  "project",
  "kind",
  "state",
  "label",
  "initiative",
  "actor",
  "q",
  "not_project",
  "not_kind",
  "not_state",
  "not_label",
  "limit",
  "why",
] as const;

type ViewName = "backlog" | "bugs";

/** The Board's refresh cadences, offered here too (#228) so a ranked view left
 *  open can keep itself current rather than only saying how stale it is. `0`
 *  is paused, and is the default: re-ranking the estate under a reader's
 *  cursor is opt-in, but the badge's honesty about the age is not. */
const POLL_CHOICES = [10, 20, 60, 0] as const;
const POLL_STORAGE_KEY = "vogt.backlog.poll.v1";

function readPoll(): number {
  try {
    const raw = localStorage.getItem(POLL_STORAGE_KEY);
    const parsed = raw === null ? NaN : Number.parseInt(raw, 10);
    if (Number.isFinite(parsed) && (POLL_CHOICES as readonly number[]).includes(parsed)) {
      return parsed;
    }
  } catch {
    // localStorage unavailable — the paused default still works.
  }
  return 0;
}

/**
 * The exclusion (negative) half of the filter set (#351).
 *
 * Only the facets a ranked row actually carries — project, kind, state,
 * label — can be excluded here, because the exclusion is applied over the
 * loaded page (the ranked views take no such parameter) and a `RankedEntry`
 * reports no initiative or actor to test against. Encoded under its own
 * `not_*` key so a stale reader drops it rather than misreads it.
 */
interface ExcludeFilter {
  projects: string[];
  kinds: string[];
  states: string[];
  labels: string[];
}

const EMPTY_EXCLUDE: ExcludeFilter = { projects: [], kinds: [], states: [], labels: [] };

function excludeCount(exclude: ExcludeFilter): number {
  return (
    exclude.projects.length + exclude.kinds.length + exclude.states.length + exclude.labels.length
  );
}

/** The six filters FR-U14 names, plus the view they apply to, plus the free
 *  text and exclusions the triage grammar gained (#350, #351). */
interface Filter {
  view: ViewName;
  project: string;
  kinds: string[];
  states: string[];
  label: string;
  initiative: string;
  actor: string;
  q: string;
  exclude: ExcludeFilter;
}

interface SavedFilter {
  name: string;
  filter: Filter;
}

const EMPTY_FILTER: Filter = {
  view: "backlog",
  project: "",
  kinds: [],
  states: [],
  label: "",
  initiative: "",
  actor: "",
  q: "",
  exclude: EMPTY_EXCLUDE,
};

// -- reading what the API actually sends ------------------------------------
//
// `RankedView` and `why()`'s result are deliberately partial in `vogtApi.ts`,
// and widening them is a change to a file this surface does not own. The
// readers below take the response as data and check each field at runtime,
// which is honest in a way a cast would not be: a field the server stops
// sending becomes "not reported" here rather than `undefined` rendered as a
// blank. See the report note about typing `WhyResult` properly.

function record(value: unknown): Record<string, unknown> {
  return value && typeof value === "object" ? (value as Record<string, unknown>) : {};
}

function readString(source: unknown, key: string): string | null {
  const value = record(source)[key];
  return typeof value === "string" && value ? value : null;
}

function readNumber(source: unknown, key: string): number | null {
  const value = record(source)[key];
  return typeof value === "number" && Number.isFinite(value) ? value : null;
}

function readStringMap(source: unknown, key: string): [string, string][] {
  const value = record(source)[key];
  if (!value || typeof value !== "object") return [];
  return Object.entries(value as Record<string, unknown>)
    .filter((entry): entry is [string, string] => typeof entry[1] === "string")
    .sort((a, b) => a[0].localeCompare(b[0]));
}

interface Contribution {
  input: string;
  detail: string;
  value: number;
  weight: number;
  contribution: number;
}

interface Explanation {
  ref: string;
  title: string;
  total: number;
  contributions: Contribution[];
  pending: [string, string][];
}

function readExplanation(ref: string, payload: unknown): Explanation {
  const raw = record(payload)["contributions"];
  const contributions: Contribution[] = (Array.isArray(raw) ? raw : []).map((entry) => ({
    input: readString(entry, "input") ?? "unnamed input",
    detail: readString(entry, "detail") ?? "",
    value: readNumber(entry, "value") ?? 0,
    weight: readNumber(entry, "weight") ?? 0,
    contribution: readNumber(entry, "contribution") ?? 0,
  }));
  return {
    ref: readString(payload, "ref") ?? ref,
    title: readString(payload, "title") ?? ref,
    total: readNumber(payload, "total") ?? 0,
    contributions,
    pending: readStringMap(payload, "inputs_not_yet_available"),
  };
}

// -- freshness and trust, which never render blank --------------------------

/**
 * How old the evidence behind this answer is (FR-U2, FR-U17).
 *
 * Rendered even when everything is fine, for the reason the legacy GUI gives:
 * the value of the line is that an empty answer and a stale answer stop
 * looking alike. A backlog with nothing in it is reassuring only if something
 * has looked recently.
 */
function describeFreshness(view: unknown): {
  status: string;
  text: string;
  collectors: [string, string][];
} {
  const raw = record(view)["freshness"];
  if (!raw || typeof raw !== "object") {
    return { status: "unknown", text: "freshness: not reported", collectors: [] };
  }
  const status = readString(raw, "status") ?? "never_swept";
  const detail = readString(raw, "detail");
  const parts: string[] = [];
  if (status === "never_swept") {
    parts.push(
      "nothing has been swept yet — this is 'not collected', not 'nothing found'",
    );
  } else {
    parts.push(`evidence is ${describeAge(readNumber(raw, "age_seconds"))} old at its oldest`);
    if (status === "partial") parts.push("at least one collector did not complete");
  }
  if (detail) parts.push(detail);
  return {
    status,
    text: parts.join(" · "),
    collectors: readStringMap(raw, "collectors"),
  };
}

function describeAge(seconds: number | null): string {
  if (seconds === null) return "an unknown time";
  if (seconds < 90) return `${seconds}s`;
  if (seconds < 5400) return `${Math.round(seconds / 60)}m`;
  if (seconds < 172800) return `${Math.round(seconds / 3600)}h`;
  return `${Math.round(seconds / 86400)}d`;
}

/**
 * Trust, on every row (FR-U2, FR-U17).
 *
 * `unverified` rather than a blank, always: a blank cell says "no opinion",
 * and the honest answer is "nobody has verified this".
 */
function trustOf(entry: RankedEntry): string {
  const state = entry.trust_state;
  return typeof state === "string" && state ? state : "unverified";
}

function formatWhen(value: string | undefined): string {
  if (!value) return "—";
  const at = new Date(value);
  return Number.isNaN(at.valueOf())
    ? String(value)
    : at.toISOString().replace("T", " ").slice(0, 16);
}

/**
 * The value of a `<select>` whose options arrive asynchronously.
 *
 * Reading the option list here is load-bearing rather than decorative. Solid
 * compiles `value={...}` to an effect that re-applies the value when the
 * expression's dependencies change; a value applied before its `<option>`
 * exists — or while `For` is reshuffling the list — is silently dropped by
 * the browser, and the control then reads "Any" while a filter is in force.
 * Touching `options` makes the re-apply happen.
 */
function optionValue(current: string, options: readonly string[]): string {
  return options.find((option) => option === current) ?? current;
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

// -- saved filters, in localStorage alongside the other per-client state ----

function loadSavedFilters(): SavedFilter[] {
  try {
    const raw = localStorage.getItem(SAVED_FILTERS_KEY);
    if (!raw) return [];
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed
      .map((entry) => ({
        name: readString(entry, "name") ?? "",
        filter: normalizeFilter(record(entry)["filter"]),
      }))
      .filter((entry) => entry.name);
  } catch {
    return [];
  }
}

function persistSavedFilters(entries: SavedFilter[]): SavedFilter[] {
  const next = entries.slice(0, MAX_SAVED_FILTERS);
  try {
    localStorage.setItem(SAVED_FILTERS_KEY, JSON.stringify(next));
  } catch {
    /* storage full or unavailable; the in-memory list still recalls */
  }
  return next;
}

function normalizeFilter(value: unknown): Filter {
  const raw = record(value);
  const list = (key: string): string[] => {
    const found = raw[key];
    return Array.isArray(found) ? found.filter((item): item is string => typeof item === "string") : [];
  };
  const text = (key: string): string => {
    const found = raw[key];
    return typeof found === "string" ? found : "";
  };
  const excludeRaw = record(raw.exclude);
  const excludeList = (key: string): string[] => {
    const found = excludeRaw[key];
    return Array.isArray(found)
      ? found.filter((item): item is string => typeof item === "string")
      : [];
  };
  return {
    view: text("view") === "bugs" ? "bugs" : "backlog",
    project: text("project"),
    kinds: list("kinds"),
    states: list("states"),
    label: text("label"),
    initiative: text("initiative"),
    actor: text("actor"),
    q: text("q"),
    exclude: {
      projects: excludeList("projects"),
      kinds: excludeList("kinds"),
      states: excludeList("states"),
      labels: excludeList("labels"),
    },
  };
}

// -- the URL, which is where a filtered view lives (FR-U11) -----------------

type Query = Partial<Record<(typeof URL_KEYS)[number], string | string[]>>;

function one(value: string | string[] | undefined): string {
  if (Array.isArray(value)) return value[0] ?? "";
  return value ?? "";
}

function many(value: string | string[] | undefined): string[] {
  if (Array.isArray(value)) return value.filter(Boolean);
  return value ? [value] : [];
}

function filterFromQuery(query: Query): Filter {
  return {
    view: one(query.view) === "bugs" ? "bugs" : "backlog",
    project: one(query.project),
    kinds: many(query.kind),
    states: many(query.state),
    label: one(query.label),
    initiative: one(query.initiative),
    actor: one(query.actor),
    q: one(query.q),
    exclude: {
      projects: many(query.not_project),
      kinds: many(query.not_kind),
      states: many(query.not_state),
      labels: many(query.not_label),
    },
  };
}

function limitFromQuery(query: Query): number {
  const parsed = Number.parseInt(one(query.limit), 10);
  if (!Number.isFinite(parsed)) return DEFAULT_PAGE_SIZE;
  return Math.min(MAX_PAGE_SIZE, Math.max(1, parsed));
}

/** `null` clears a key; `mergeSearchString` leaves every other key alone. */
function queryFor(
  filter: Filter,
  limit: number,
  explained: string[],
): Record<(typeof URL_KEYS)[number], string | string[] | null> {
  return {
    view: filter.view === "backlog" ? null : filter.view,
    project: filter.project || null,
    kind: filter.kinds.length ? filter.kinds : null,
    state: filter.states.length ? filter.states : null,
    label: filter.label || null,
    initiative: filter.initiative || null,
    actor: filter.actor || null,
    q: filter.q || null,
    not_project: filter.exclude.projects.length ? filter.exclude.projects : null,
    not_kind: filter.exclude.kinds.length ? filter.exclude.kinds : null,
    not_state: filter.exclude.states.length ? filter.exclude.states : null,
    not_label: filter.exclude.labels.length ? filter.exclude.labels : null,
    limit: limit === DEFAULT_PAGE_SIZE ? null : String(limit),
    why: explained.length ? explained : null,
  };
}

/**
 * The canonical text of this surface's slice of the query.
 *
 * Both encoders walk `URL_KEYS` in the same order, so two equal states always
 * produce the same string — which is what lets one effect tell "the user
 * changed a filter" from "somebody handed us a different URL".
 */
function encodeState(filter: Filter, limit: number, explained: string[]): string {
  const params = new URLSearchParams();
  const desired = queryFor(filter, limit, explained);
  for (const key of URL_KEYS) {
    const value = desired[key];
    if (value === null) continue;
    for (const item of Array.isArray(value) ? value : [value]) params.append(key, item);
  }
  return params.toString();
}

function encodeQuery(query: Query): string {
  const params = new URLSearchParams();
  for (const key of URL_KEYS) {
    const value = query[key];
    if (value === undefined) continue;
    for (const item of Array.isArray(value) ? value : [value]) params.append(key, item);
  }
  return params.toString();
}

function sameFilter(a: Filter, b: Filter): boolean {
  return (
    a.view === b.view &&
    a.project === b.project &&
    a.label === b.label &&
    a.initiative === b.initiative &&
    a.actor === b.actor &&
    a.q === b.q &&
    a.kinds.join("\u0000") === b.kinds.join("\u0000") &&
    a.states.join("\u0000") === b.states.join("\u0000") &&
    a.exclude.projects.join("\u0000") === b.exclude.projects.join("\u0000") &&
    a.exclude.kinds.join("\u0000") === b.exclude.kinds.join("\u0000") &&
    a.exclude.states.join("\u0000") === b.exclude.states.join("\u0000") &&
    a.exclude.labels.join("\u0000") === b.exclude.labels.join("\u0000")
  );
}

function filterIsEmpty(filter: Filter): boolean {
  return sameFilter(filter, EMPTY_FILTER);
}

function describeFilter(filter: Filter): string {
  const parts: string[] = [filter.view === "bugs" ? "bugs" : "backlog"];
  if (filter.project) parts.push(`project ${filter.project}`);
  if (filter.kinds.length) parts.push(`type ${filter.kinds.join("/")}`);
  if (filter.states.length) parts.push(`state ${filter.states.join("/")}`);
  if (filter.label) parts.push(`label ${filter.label}`);
  if (filter.initiative) parts.push(`initiative ${filter.initiative}`);
  if (filter.actor) parts.push(`actor ${filter.actor}`);
  if (filter.q) parts.push(`search “${filter.q}”`);
  const ex = filter.exclude;
  if (ex.projects.length) parts.push(`not project ${ex.projects.join("/")}`);
  if (ex.kinds.length) parts.push(`not type ${ex.kinds.join("/")}`);
  if (ex.states.length) parts.push(`not state ${ex.states.join("/")}`);
  if (ex.labels.length) parts.push(`not label ${ex.labels.join("/")}`);
  return parts.join(" · ");
}

/** Free-text match for the backlog search (#350): the ranked row's title and
 *  ref, and the adopted work item's body when the ranking carried it. */
function entryMatchesText(entry: RankedEntry, needle: string): boolean {
  if (!needle) return true;
  const haystack = `${entry.title ?? ""} ${entry.ref ?? ""} ${entry.item?.body ?? ""}`;
  return haystack.toLowerCase().includes(needle);
}

// -- the default lens (#352) ------------------------------------------------
//
// The named lens applied on a bare load. Stored by name in a key of its own,
// beside the saved list, so a build that predates it never reads it and the
// saved lenses still recall. A non-empty query always wins.

const DEFAULT_FILTER_KEY = "vogt.backlogDefaultFilter.v1";

function readDefaultLens(): string {
  try {
    return localStorage.getItem(DEFAULT_FILTER_KEY) ?? "";
  } catch {
    return "";
  }
}

function writeDefaultLens(name: string): string {
  try {
    if (name) localStorage.setItem(DEFAULT_FILTER_KEY, name);
    else localStorage.removeItem(DEFAULT_FILTER_KEY);
  } catch {
    /* private mode / no storage: the choice still holds for this session */
  }
  return name;
}

// -- results, tagged rather than thrown -------------------------------------
//
// A thrown resource error is a resource whose value cannot be read without
// rethrowing, and an outage is something this surface renders rather than
// escalates (FR-U21). So the fetchers return the failure as a value.

type Loaded<T> =
  | { ok: true; value: T }
  | { ok: false; unavailable: boolean; message: string };

async function attempt<T>(work: () => Promise<T>): Promise<Loaded<T>> {
  try {
    return { ok: true, value: await work() };
  } catch (error) {
    return {
      ok: false,
      unavailable: error instanceof VogtUnavailable,
      message: errorMessage(error),
    };
  }
}

interface BulkOutcome {
  ref: string;
  ok: boolean;
  message: string;
}

// -- the surface ------------------------------------------------------------

const Backlog: Component<Props> = (props) => {
  const navigate = useNavigate();
  const [query, setQuery] = useSearchParams<Query>();
  const location = useLocation();

  // The URL is read once here and written from the signal afterwards. It has
  // to be this way round in this shell: activating a tab navigates to the
  // tab's bare path (`App.tsx`'s `pathFor`), which drops the query — if the
  // URL were the source of truth, switching tabs and switching back would
  // silently clear the filters, which is the "filter that resets on reload"
  // r9 names as a GUI failure. The effect below puts them back instead.
  // The default lens (#352) is applied only on a bare load; a non-empty query
  // is an explicit instruction and always wins. Resolved once, here, so the
  // address the effect below writes carries the default's chips.
  const initialFilter = (): Filter => {
    const fromUrl = filterFromQuery(query);
    if (encodeQuery(query) !== "") return fromUrl;
    const defaultName = readDefaultLens();
    if (!defaultName) return fromUrl;
    const saved = loadSavedFilters().find((entry) => entry.name === defaultName);
    return saved ? saved.filter : fromUrl;
  };

  const [filter, setFilter] = createSignal<Filter>(initialFilter());
  const [limit, setLimit] = createSignal<number>(limitFromQuery(query));
  const [explained, setExplained] = createSignal<string[]>(many(query.why).slice(0, 2));

  const [savedFilters, setSavedFilters] = createSignal<SavedFilter[]>(loadSavedFilters());

  // #352: which saved lens (by name) is the default. Persisted alongside the
  // saved list; dropped once it names a lens no longer saved.
  const [defaultLens, setDefaultLens] = createSignal<string>(readDefaultLens());
  createEffect(() => {
    const name = defaultLens();
    if (name && !savedFilters().some((entry) => entry.name === name)) {
      setDefaultLens(writeDefaultLens(""));
    }
  });
  const toggleDefaultLens = (name: string) => {
    setDefaultLens(writeDefaultLens(defaultLens() === name ? "" : name));
  };

  const [selected, setSelected] = createSignal<string[]>([]);
  const selectedSet = createMemo(() => new Set(selected()));

  const [reloadKey, setReloadKey] = createSignal(0);
  const refresh = () => setReloadKey((value) => value + 1);

  // The refresh cadence, a per-device preference like the Board's (#228). It
  // is not a filter, so it lives outside the URL and the saved lenses: a
  // shared link or a recalled lens should not change how often the reader's
  // backlog refreshes.
  const [poll, setPoll] = createSignal<number>(readPoll());
  const setPollSeconds = (seconds: number) => {
    setPoll(seconds);
    try {
      localStorage.setItem(POLL_STORAGE_KEY, String(seconds));
    } catch {
      /* private mode / quota — the session still polls, just does not remember */
    }
  };

  createEffect(() => {
    const seconds = poll();
    if (seconds <= 0) return;
    const timer = window.setInterval(() => {
      // A hidden tab is not a view anybody is being misled by, and it
      // reconciles the moment it comes back to the front.
      if (typeof document !== "undefined" && document.hidden) return;
      refresh();
    }, seconds * 1000);
    onCleanup(() => window.clearInterval(timer));
  });

  // -- URL ↔ state (FR-U11) -------------------------------------------------

  // One effect, both directions, with the ambiguity resolved by remembering
  // what this surface last asserted. Two independent effects cannot do it: on
  // a filter change the URL still holds the *previous* value, and an adopting
  // effect would read that as an instruction and undo the change.
  let lastWritten = encodeQuery(query);

  createEffect(() => {
    // Every tab is mounted at once, and `view`, `actor` and `project` are keys
    // more than one Vogt surface owns. Without this guard the backlog writes
    // its filters into whichever route happens to be active, and reads the
    // other surface's values back as an instruction — the board guards its
    // own effect the same way and for the same reason.
    if (location.pathname !== "/backlog") return;
    const desired = encodeState(filter(), limit(), explained());
    const current = encodeQuery(query);
    if (desired === current) {
      lastWritten = current;
      return;
    }
    if (current !== lastWritten && current !== "") {
      // The query changed to something this surface did not write, and is
      // not empty: a pasted link, or the back button. That is an instruction.
      setFilter(filterFromQuery(query));
      setLimit(limitFromQuery(query));
      setExplained(many(query.why).slice(0, 2));
      lastWritten = current;
      return;
    }
    // Otherwise the signals are authoritative, which is also what restores
    // the query after a tab switch emptied it.
    setQuery(queryFor(filter(), limit(), explained()), { replace: true });
    lastWritten = desired;
  });

  // -- the ranked view ------------------------------------------------------

  // Only the parameters the server understands are in the key: the workflow
  // state filter is applied client-side (see `visible`), so changing it must
  // not cost a round trip.
  const queryKey = createMemo(() => {
    const current = filter();
    return {
      reload: reloadKey(),
      view: current.view,
      project: current.project,
      kinds: current.kinds.join(","),
      label: current.label,
      initiative: current.initiative,
      actor: current.actor,
      limit: limit(),
    };
  });

  const [ranked] = createResource(queryKey, async (key): Promise<Loaded<RankedView>> => {
    const params: Record<string, unknown> = {
      limit: key.limit,
      project: key.project || undefined,
      label: key.label || undefined,
      assignee: key.actor || undefined,
    };
    if (key.view === "bugs") {
      // `bugs` is kind=bug by definition and takes no initiative; the two
      // controls are disabled there rather than quietly dropped.
      return attempt(() => bugs(params));
    }
    return attempt(() =>
      backlog({
        ...params,
        kinds: key.kinds ? key.kinds.split(",") : undefined,
        initiative: key.initiative || undefined,
      }),
    );
  });

  const view = createMemo<RankedView | null>(() => {
    const result = ranked();
    return result && result.ok ? result.value : null;
  });

  const outage = createMemo(() => {
    const result = ranked();
    return result && !result.ok ? result : null;
  });

  // -- how old this *view* is (FR-U10) --------------------------------------
  //
  // Distinct from the freshness line below it, which is how old the evidence
  // Vogt ranked from is. Both are needed and neither substitutes for the
  // other: a sweep that ran a minute ago, rendered by a tab that last asked
  // an hour ago, is a fresh answer on a stale screen — and until this badge
  // existed that tab looked exactly like one opened a second ago, which is
  // the clause FR-U10 is actually about.
  //
  // No poll and no subscription here, deliberately: the ranked views are a
  // sweep product and re-ranking the estate under a reader's cursor on every
  // announced change is a different decision from telling them the truth
  // about the age of what they are reading. So the badge says "press
  // Refresh", which is a thing this header has.
  const loadedAt = createLoadStamp(ranked, (result) => result.ok);

  const viewAge = createViewAge(() => {
    const failure = outage();
    const seconds = poll();
    return {
      loadedAt: loadedAt(),
      outage: failure?.unavailable ? failure.message : null,
      failed: Boolean(failure),
      // Off (the default) is a view with no poll, which FR-U10 renders as
      // "press Refresh" when it goes stale — the ranked views are a sweep
      // product and re-ranking under the cursor is opt-in. Only a chosen
      // cadence turns the badge into a polling one.
      poll: seconds > 0 ? seconds : undefined,
    };
  });

  const entries = createMemo<RankedEntry[]>(() => view()?.items ?? []);

  // The #183 link-or-publish CTA: a project scope whose server answer says
  // "unlinked" has no backlog — the items are empty because linking or
  // publishing is the way forward, not because there is nothing to do.
  const scopeUnlinked = createMemo(() => view()?.link_state === "unlinked");
  const unlinkedPending = createMemo(() => view()?.excluded_unlinked ?? 0);

  /**
   * FR-U14's workflow-state filter, applied here rather than in the query.
   *
   * `BacklogParams` and `BugsParams` take no `states`: the ranked views
   * already exclude terminal states server-side and offer no state parameter
   * at all. Narrowing the fetched page is therefore the only honest way to
   * offer the filter, and the count line says so rather than implying the
   * estate was searched.
   */
  const visible = createMemo<RankedEntry[]>(() => {
    const active = filter();
    const needle = active.q.trim().toLowerCase();
    const ex = active.exclude;
    const wanted = active.states.length ? new Set(active.states) : null;
    if (!wanted && !needle && excludeCount(ex) === 0) return entries();
    // Search (#350) and exclusion (#351) join the page-only state filter over
    // the loaded ranked page: the ranked views take no free-text or negative
    // parameter, so this narrows what was loaded, and the count line says so.
    return entries().filter((entry) => {
      if (wanted && !wanted.has(entry.state)) return false;
      if (needle && !entryMatchesText(entry, needle)) return false;
      if (ex.projects.length && entry.project_slug && ex.projects.includes(entry.project_slug))
        return false;
      if (ex.kinds.includes(entry.kind)) return false;
      if (ex.states.includes(entry.state)) return false;
      if (ex.labels.length && (entry.labels ?? []).some((one) => ex.labels.includes(one)))
        return false;
      return true;
    });
  });

  /** Whether a client-side filter (state, search, or exclusion) is narrowing
   *  the loaded page — so the count line can say "N of M loaded" (#226). */
  const pageNarrowed = () => {
    const active = filter();
    return Boolean(active.states.length || active.q || excludeCount(active.exclude) > 0);
  };

  const stateOptions = createMemo(() => {
    const seen = new Set<string>(entries().map((entry) => entry.state));
    // A state named in the URL stays offered even when this page has none of
    // it, so a shared link does not silently drop half of what it encoded.
    for (const state of filter().states) seen.add(state);
    return [...seen].sort();
  });

  // -- the facet lists ------------------------------------------------------
  //
  // Loaded once. A facet list that fails is a smaller picker and a named
  // note, not a broken surface — the ranked view is the thing the reader came
  // for, and it has its own outage state.

  const [projects] = createResource(() =>
    attempt(() => taxonomy.projects()),
  );
  const [labels] = createResource(() => attempt(() => taxonomy.labels()));
  const [initiatives] = createResource(() => attempt(() => taxonomy.initiatives()));
  const [actors] = createResource(() => attempt(() => taxonomy.actors()));
  const [workflows] = createResource(() => attempt(() => taxonomy.workflows()));

  const projectOptions = createMemo(() => {
    const result = projects();
    const named = result && result.ok ? result.value.projects.map((p) => p.slug) : [];
    const seen = new Set<string>(named);
    for (const entry of entries()) if (entry.project_slug) seen.add(entry.project_slug);
    if (filter().project) seen.add(filter().project);
    return [...seen].sort();
  });

  const labelOptions = createMemo(() => {
    const result = labels();
    const named = result && result.ok ? result.value.labels.map((l) => l.name) : [];
    const seen = new Set<string>(named);
    for (const entry of entries()) for (const label of entry.labels ?? []) seen.add(label);
    if (filter().label) seen.add(filter().label);
    return [...seen].sort();
  });

  // The value currently filtered on is always an option, even when it is not
  // in the page the picker loaded: a `<select>` whose value matches no option
  // renders as the first one, which would show "Any" while a filter was in
  // force. A link is allowed to name something beyond the first page.
  const initiativeOptions = createMemo(() => {
    const result = initiatives();
    const named = result && result.ok ? result.value.initiatives.map((i) => i.slug) : [];
    const seen = new Set(named);
    if (filter().initiative) seen.add(filter().initiative);
    return [...seen].sort();
  });

  const actorOptions = createMemo(() => {
    const result = actors();
    const named = result && result.ok ? result.value.actors.map((a) => a.identity_ref) : [];
    const seen = new Set(named);
    if (filter().actor) seen.add(filter().actor);
    return [...seen].sort();
  });

  // The loaded rows, for resolving a slug or an identity ref to a name. The
  // ranked entries carry only the slug, so the name comes from the registry.
  const projectRows = createMemo(() => {
    const result = projects();
    return result && result.ok ? result.value.projects : [];
  });
  const projectLabel = (slug: string) => resolveProjectName(projectRows(), slug);
  const actorRows = createMemo(() => {
    const result = actors();
    return result && result.ok ? result.value.actors : [];
  });
  const actorLabel = (ref: string) => resolveActorName(actorRows(), ref);

  const facetNote = createMemo(() => {
    const missing: string[] = [];
    for (const [name, resource] of [
      ["projects", projects()],
      ["labels", labels()],
      ["initiatives", initiatives()],
      ["actors", actors()],
    ] as const) {
      if (resource && !resource.ok) missing.push(name);
    }
    return missing.length
      ? `Could not load ${missing.join(", ")} — those pickers offer only what this page contains.`
      : null;
  });

  /** Target states for a bulk transition: the union across the loaded kinds. */
  const workflowStates = createMemo(() => {
    const result = workflows();
    if (!(result && result.ok)) return [];
    const kinds = new Set(
      visible()
        .filter((entry) => selectedSet().has(entry.ref))
        .map((entry) => entry.kind),
    );
    const states = new Set<string>();
    for (const workflow of result.value.workflows) {
      if (kinds.size && !kinds.has(workflow.kind)) continue;
      for (const state of workflow.states) {
        states.add(state);
      }
    }
    return [...states].sort();
  });

  // -- selection ------------------------------------------------------------
  //
  // Only declared work can be selected: `work.transition` addresses a work
  // item, and an observed subject has none until it is adopted. A checkbox
  // that submits a call which cannot succeed is a button that lies.

  const selectableRefs = createMemo(() =>
    visible().filter((entry) => entry.origin === "declared").map((entry) => entry.ref),
  );

  // What can still be selected is what the *loaded page* declares, not what the
  // client-side state filter currently shows: a ref hidden by toggling a State
  // chip has not left the estate, so a selection made before the toggle is kept
  // (#226). A genuinely new page — a refetch, a different query — is a different
  // list, and a ref that is no longer on it can no longer be acted on, so it
  // stops being selected.
  const loadedSelectable = createMemo(
    () =>
      new Set(
        entries()
          .filter((entry) => entry.origin === "declared")
          .map((entry) => entry.ref),
      ),
  );

  createEffect(
    on(loadedSelectable, (live) => {
      setSelected((current) => current.filter((ref) => live.has(ref)));
    }),
  );

  const toggleSelected = (ref: string) => {
    setSelected((current) =>
      current.includes(ref) ? current.filter((item) => item !== ref) : [...current, ref],
    );
  };

  const allSelected = createMemo(
    () => selectableRefs().length > 0 && selected().length === selectableRefs().length,
  );

  // -- why (FR-U6) ----------------------------------------------------------

  const [explanations] = createResource(explained, async (refs) => {
    if (!refs.length) return [] as Loaded<Explanation>[];
    return Promise.all(
      refs.map((ref) => attempt(async () => readExplanation(ref, await why(ref)))),
    );
  });

  const toggleWhy = (ref: string) => {
    setExplained((current) => {
      if (current.includes(ref)) return current.filter((item) => item !== ref);
      // Two at a time: one explains a score, two explain an ordering, and a
      // third is a table nobody reads.
      return current.length < 2 ? [...current, ref] : [current[1] ?? ref, ref];
    });
  };

  /** The explanations that answered, in the order they were opened. */
  const shown = createMemo<Explanation[]>(() =>
    (explanations() ?? []).flatMap((entry) => (entry.ok ? [entry.value] : [])),
  );

  /** The ones that did not, reported rather than left as a gap in the table. */
  const whyFailures = createMemo(() =>
    (explanations() ?? []).flatMap((entry) => (entry.ok ? [] : [entry.message])),
  );

  /** Inputs named by either explanation, so a missing one shows as absent. */
  const comparedInputs = createMemo(() => {
    const names: string[] = [];
    for (const explanation of shown()) {
      for (const contribution of explanation.contributions) {
        if (!names.includes(contribution.input)) names.push(contribution.input);
      }
    }
    return names;
  });

  const contributionScale = createMemo(() => {
    let largest = 0;
    for (const explanation of shown()) {
      for (const contribution of explanation.contributions) {
        largest = Math.max(largest, Math.abs(contribution.contribution));
      }
    }
    return largest || 1;
  });

  // -- quick-create (FR-U15) ------------------------------------------------

  const [createOpen, setCreateOpen] = createSignal(false);
  const [draftTitle, setDraftTitle] = createSignal("");
  const [draftKind, setDraftKind] = createSignal<string>("feature");
  const [draftProject, setDraftProject] = createSignal("");
  const [draftReason, setDraftReason] = createSignal("");
  const [creating, setCreating] = createSignal(false);
  const [created, setCreated] = createSignal<string | null>(null);

  // The whole of FR-U15's refusal, in one place: no title, no reason, no
  // submit. `disabled` says it on screen and the guard in `submitCreate`
  // says it again for the keyboard path.
  const createReady = createMemo(
    () => draftTitle().trim().length > 0 && draftReason().trim().length > 0,
  );

  const openQuickCreate = () => {
    // The view and the project filter are a good guess at what is being
    // raised; the reason never is, so it is never prefilled.
    setDraftKind(filter().view === "bugs" ? "bug" : "feature");
    setDraftProject(filter().project);
    setCreated(null);
    setCreateOpen(true);
  };

  const submitCreate = async (event: Event) => {
    event.preventDefault();
    const title = draftTitle().trim();
    const reason = draftReason().trim();
    if (!title || !reason || creating()) return;
    setCreating(true);
    try {
      const result = await createWork({
        title,
        kind: draftKind(),
        project: draftProject() || undefined,
        reason,
      });
      setCreated(result.item.ref);
      setDraftTitle("");
      setDraftReason("");
      refresh();
    } catch (error) {
      props.onError?.(`Could not create the work item: ${errorMessage(error)}`);
    } finally {
      setCreating(false);
    }
  };

  // -- a row's own content and acts (FR-U25, Stage 7) ----------------------
  //
  // The collapsed row keeps the facts a reader ranks by — rank, ref, trust,
  // age and score — and lets the title wrap to whatever it says. Everything
  // else is one control away, in the row rather than in a dialog: provenance,
  // the ranking factors, and the acts this row actually has.
  //
  // "Actually has" is the load-bearing half. A declared row has a work item
  // to open, select and start a session against. An observed row has no work
  // item at all, so it offers the two writes that exist for a subject —
  // adopt and suppress — and neither of them is drawn on a declared row.

  const [expandedRows, setExpandedRows] = createSignal<Set<string>>(new Set());

  const toggleRow = (ref: string) => {
    setExpandedRows((current) => {
      const next = new Set(current);
      if (next.has(ref)) next.delete(ref);
      else next.add(ref);
      return next;
    });
  };

  type RowActKind = "session" | "adopt" | "suppress";

  const [rowAct, setRowAct] = createSignal<{ ref: string; kind: RowActKind } | null>(null);
  const [rowReason, setRowReason] = createSignal("");
  const [rowRunning, setRowRunning] = createSignal(false);

  // -- reconcile on tab return (FR-U10, #223) ------------------------------
  //
  // The ranked view is a sweep product, so it is deliberately *not* re-ranked
  // on every stream nudge — a list re-ordering itself under a reader's cursor
  // on someone else's transition is the thing the age badge exists to make
  // unnecessary (see the badge note above, and `live.test.tsx`). But a tab
  // left in the background and brought back is the moment its answer is
  // furthest from current, so that one case reconciles: `onNudge: false`
  // keeps the stream from touching the list while `onVisible` (the default)
  // reloads it on return — and never while a create or a row action is
  // mid-reason.
  onVogtLive(() => refresh(), {
    when: () => !createOpen() && rowAct() === null,
    onNudge: false,
  });

  const beginRowAct = (ref: string, kind: RowActKind) => {
    setRowAct({ ref, kind });
    // Each act collects its own reason: one left in the field could otherwise
    // justify a different write on a different row (r6).
    setRowReason("");
    // "Start a session…" is reachable from the collapsed row now (#226); the
    // reason form it opens lives in the row's detail, so the row is expanded to
    // put it on screen. A row already open is left as it is.
    setExpandedRows((current) => {
      if (current.has(ref)) return current;
      const next = new Set(current);
      next.add(ref);
      return next;
    });
  };

  const ROW_ACT_WORDS: Record<RowActKind, string> = {
    session: "Start a session on this item",
    adopt: "Adopt this subject as a work item",
    suppress: "Suppress this subject from ranked views",
  };

  const submitRowAct = async (event: Event) => {
    event.preventDefault();
    const act = rowAct();
    const reason = rowReason().trim();
    if (!act || !reason || rowRunning()) return;
    setRowRunning(true);
    try {
      if (act.kind === "session") await startSession({ work_item: act.ref, reason });
      else if (act.kind === "adopt") await adoptSubject(act.ref, reason);
      else await suppressSubject(act.ref, reason);
      setRowAct(null);
      setRowReason("");
      refresh();
    } catch (error) {
      props.onError?.(`${act.ref}: ${errorMessage(error)}`);
    } finally {
      setRowRunning(false);
    }
  };

  const rowActions = (entry: RankedEntry) => (
    <div class="vogt-backlog-row-actions">
      <Show
        when={entry.origin === "declared"}
        fallback={
          <>
            <button type="button" onClick={() => beginRowAct(entry.ref, "adopt")}>
              Adopt as work item…
            </button>
            <button type="button" onClick={() => beginRowAct(entry.ref, "suppress")}>
              Suppress source…
            </button>
          </>
        }
      >
        <button type="button" onClick={() => open(entry)}>
          Open
        </button>
        <button type="button" onClick={() => toggleSelected(entry.ref)}>
          {selectedSet().has(entry.ref) ? "Deselect" : "Select"}
        </button>
        {/* "Start a session…" lives in the collapsed facts row on a desk (#226);
            on a phone that row is a single compact meta line, so the act is here
            in the detail instead. CSS shows exactly one of the two per width. */}
        <button
          type="button"
          class="vogt-backlog-row-session-detail"
          onClick={() => beginRowAct(entry.ref, "session")}
        >
          Start a session…
        </button>
      </Show>

      <Show when={rowAct()?.ref === entry.ref ? rowAct() : null}>
        {(act) => (
          <form class="vogt-backlog-row-reason" onSubmit={(event) => void submitRowAct(event)}>
            <label class="vogt-backlog-field vogt-backlog-field-wide">
              <span>{ROW_ACT_WORDS[act().kind]} — why?</span>
              <textarea
                required
                rows="2"
                value={rowReason()}
                onInput={(event) => setRowReason(event.currentTarget.value)}
              />
            </label>
            <div class="vogt-backlog-row-reason-actions">
              <button type="submit" disabled={!rowReason().trim() || rowRunning()}>
                {rowRunning() ? "Asking Vogt…" : "Confirm"}
              </button>
              <button type="button" onClick={() => setRowAct(null)}>
                Cancel
              </button>
            </div>
          </form>
        )}
      </Show>
    </div>
  );

  // -- bulk transition (FR-U6) ---------------------------------------------

  const [bulkState, setBulkState] = createSignal("");
  const [bulkReason, setBulkReason] = createSignal("");
  const [bulkRunning, setBulkRunning] = createSignal(false);
  const [bulkOutcomes, setBulkOutcomes] = createSignal<BulkOutcome[]>([]);

  const bulkReady = createMemo(
    () => selected().length > 0 && bulkState() !== "" && bulkReason().trim().length > 0,
  );

  /**
   * One reason, typed for this batch, recorded against every item in it.
   *
   * r6's rule allows this and no more: the same reason may cover a batch
   * *because the user typed it for that batch*. Nothing here composes a
   * reason, and the field is cleared afterwards so the next batch cannot
   * inherit the last one's.
   *
   * Sequential on purpose. The calls are separate audited writes, a rejected
   * transition is reported with the server's own reason (FR-W2 names the rule
   * it violated), and a partial batch has to be readable as a partial batch.
   */
  const submitBulk = async (event: Event) => {
    event.preventDefault();
    const target = bulkState();
    const reason = bulkReason().trim();
    const refs = selected();
    if (!target || !reason || !refs.length || bulkRunning()) return;
    setBulkRunning(true);
    setBulkOutcomes([]);
    const outcomes: BulkOutcome[] = [];
    for (const ref of refs) {
      try {
        await transitionWork(ref, target, reason);
        outcomes.push({ ref, ok: true, message: `→ ${target}` });
      } catch (error) {
        outcomes.push({ ref, ok: false, message: errorMessage(error) });
      }
      setBulkOutcomes([...outcomes]);
    }
    setBulkRunning(false);
    setBulkReason("");
    const failed = outcomes.filter((outcome) => !outcome.ok).length;
    if (failed) {
      props.onError?.(
        `${failed} of ${outcomes.length} transitions were refused; each reason is listed.`,
      );
    }
    setSelected(outcomes.filter((outcome) => !outcome.ok).map((outcome) => outcome.ref));
    refresh();
  };

  // -- bulk label (FR-U6) ---------------------------------------------------
  //
  // FR-U6 says "bulk transition/label". Bulk transition was built and bulk
  // label was not — `updateWork` existed in `vogtApi.ts` and no surface called
  // it, which §6 records as the worse of the two possible failures: not a
  // missing binding but an unused one.
  //
  // Everything about the shape is the bulk transition's, deliberately: one
  // reason typed for this batch, recorded against every item in it, cleared
  // afterwards so the next batch cannot inherit it; sequential rather than
  // parallel, because each call is a separate audited write and a partial
  // batch has to read as a partial batch; and the outcomes list is the same
  // list, so a labelling and a transition report the same way.
  //
  // The reason field is this action's own rather than the transition's. One
  // field serving two buttons would mean a reason typed for a state change
  // could be submitted as the justification for a labelling, which is r6's
  // rule kept in letter and lost in substance.

  const [bulkLabel, setBulkLabel] = createSignal("");
  const [bulkLabelMode, setBulkLabelMode] = createSignal<"add" | "remove">("add");
  const [bulkLabelReason, setBulkLabelReason] = createSignal("");
  const [bulkLabelRunning, setBulkLabelRunning] = createSignal(false);

  /** What can be *removed* is what the selection actually carries; offering
   *  the whole vocabulary would be offering writes that change nothing. */
  const removableLabels = createMemo(() => {
    const seen = new Set<string>();
    for (const entry of visible()) {
      if (!selectedSet().has(entry.ref)) continue;
      for (const label of entry.labels ?? []) seen.add(label);
    }
    return [...seen].sort();
  });

  const bulkLabelOptions = createMemo(() =>
    bulkLabelMode() === "remove" ? removableLabels() : labelOptions(),
  );

  const bulkLabelReady = createMemo(
    () =>
      selected().length > 0 &&
      bulkLabel() !== "" &&
      bulkLabelReason().trim().length > 0,
  );

  const submitBulkLabel = async (event: Event) => {
    event.preventDefault();
    const label = bulkLabel();
    const reason = bulkLabelReason().trim();
    const refs = selected();
    const mode = bulkLabelMode();
    if (!label || !reason || !refs.length || bulkLabelRunning()) return;
    setBulkLabelRunning(true);
    setBulkOutcomes([]);
    const outcomes: BulkOutcome[] = [];
    for (const ref of refs) {
      try {
        await updateWork({
          ref,
          reason,
          ...(mode === "add" ? { add_labels: [label] } : { remove_labels: [label] }),
        });
        outcomes.push({
          ref,
          ok: true,
          message: mode === "add" ? `+ ${label}` : `− ${label}`,
        });
      } catch (error) {
        outcomes.push({ ref, ok: false, message: errorMessage(error) });
      }
      setBulkOutcomes([...outcomes]);
    }
    setBulkLabelRunning(false);
    setBulkLabelReason("");
    const failed = outcomes.filter((outcome) => !outcome.ok).length;
    if (failed) {
      props.onError?.(
        `${failed} of ${outcomes.length} label writes were refused; each reason is listed.`,
      );
    }
    setSelected(outcomes.filter((outcome) => !outcome.ok).map((outcome) => outcome.ref));
    refresh();
  };

  // The batch controls, rendered inside the list scroller and pinned to its top
  // while anything is selected (#226). Above the list, the bar scrolled out of
  // sight the moment the reader moved down the ranked page; here it follows the
  // selection it acts on.
  const bulkBar = () => (
    <Show when={selected().length > 0 || bulkOutcomes().length > 0}>
      {/* Two rows, two forms, two reasons. A single form with two submit
          buttons would let a reason typed for a state change be recorded as
          the justification for a labelling. */}
      <div class="vogt-backlog-bulk">
        <Show when={selected().length > 0}>
          <form
            class="vogt-backlog-bulk-row"
            onSubmit={(event) => void submitBulk(event)}
          >
            <strong>{selected().length} selected</strong>
            <label class="vogt-backlog-field">
              <span>Transition to</span>
              <select
                value={optionValue(bulkState(), workflowStates())}
                onInput={(event) => setBulkState(event.currentTarget.value)}
              >
                <option value="">Pick a state</option>
                <For each={workflowStates()}>
                  {(state) => <option value={state}>{state}</option>}
                </For>
              </select>
            </label>
            <label class="vogt-backlog-field vogt-backlog-field-wide">
              <span>Reason for this batch (recorded against every item in it)</span>
              <input
                type="text"
                required
                value={bulkReason()}
                placeholder="Why are these moving?"
                onInput={(event) => setBulkReason(event.currentTarget.value)}
              />
            </label>
            <button type="submit" disabled={!bulkReady() || bulkRunning()}>
              {bulkRunning() ? "Transitioning…" : "Transition"}
            </button>
            <button
              type="button"
              onClick={() => setSelected([])}
              disabled={bulkRunning()}
            >
              Deselect
            </button>
          </form>

          {/* Bulk label (FR-U6), on the same batch and under the same rule. */}
          <form
            class="vogt-backlog-bulk-row"
            onSubmit={(event) => void submitBulkLabel(event)}
          >
            <label class="vogt-backlog-field">
              <span>Label</span>
              <select
                value={bulkLabelMode()}
                onInput={(event) =>
                  setBulkLabelMode(
                    event.currentTarget.value === "remove" ? "remove" : "add",
                  )
                }
              >
                <option value="add">Add</option>
                <option value="remove">Remove</option>
              </select>
            </label>
            <label class="vogt-backlog-field">
              <span>{bulkLabelMode() === "add" ? "To apply" : "To take off"}</span>
              <select
                value={optionValue(bulkLabel(), bulkLabelOptions())}
                onInput={(event) => setBulkLabel(event.currentTarget.value)}
              >
                <option value="">Pick a label</option>
                <For each={bulkLabelOptions()}>
                  {(name) => <option value={name}>{name}</option>}
                </For>
              </select>
            </label>
            <label class="vogt-backlog-field vogt-backlog-field-wide">
              <span>Reason for this batch (recorded against every item in it)</span>
              <input
                type="text"
                required
                value={bulkLabelReason()}
                placeholder="Why are these being labelled?"
                onInput={(event) => setBulkLabelReason(event.currentTarget.value)}
              />
            </label>
            <button type="submit" disabled={!bulkLabelReady() || bulkLabelRunning()}>
              {bulkLabelRunning()
                ? "Labelling…"
                : bulkLabelMode() === "add"
                  ? "Add label"
                  : "Remove label"}
            </button>
            <Show when={bulkLabelMode() === "remove" && removableLabels().length === 0}>
              <span class="vogt-backlog-muted">
                nothing selected carries a label, so there is none to take off
              </span>
            </Show>
          </form>
        </Show>
        {/* The outcomes outlive the selection: a batch that succeeded empties
            the selection, and the report of what was written to whom is the
            part the user still needs. */}
        <Show when={bulkOutcomes().length}>
          <ul class="vogt-backlog-outcomes">
            <For each={bulkOutcomes()}>
              {(outcome) => (
                <li class={outcome.ok ? "ok" : "failed"}>
                  <span class="vogt-backlog-mono">{outcome.ref}</span> {outcome.message}
                </li>
              )}
            </For>
          </ul>
          <div>
            <button
              type="button"
              onClick={() => setBulkOutcomes([])}
              disabled={bulkRunning() || bulkLabelRunning()}
            >
              Dismiss
            </button>
          </div>
        </Show>
      </div>
    </Show>
  );

  // -- virtualization (NFR-S5) ---------------------------------------------

  let scroller: HTMLDivElement | undefined;
  const [scrollTop, setScrollTop] = createSignal(0);
  const [viewportHeight, setViewportHeight] = createSignal(0);
  const measured = new MeasuredWindow(ROW_ESTIMATE);
  const [measurementVersion, setMeasurementVersion] = createSignal(0);

  createEffect(() => {
    measurementVersion();
    if (measured.setKeys(visible().map((entry) => entry.ref))) {
      setMeasurementVersion((version) => version + 1);
    }
  });

  onMount(() => {
    const node = scroller;
    if (!node) return;
    // A ResizeObserver rather than a window listener: this surface is a tab,
    // and an inactive tab is `display: none` with a height of zero. The
    // observer fires when it comes back, which a resize event would not.
    const observer = new ResizeObserver(() => setViewportHeight(node.clientHeight));
    observer.observe(node);
    setViewportHeight(node.clientHeight);
    onCleanup(() => observer.disconnect());
  });

  // A new result set is a new list; keeping the old scroll offset would land
  // the reader somewhere arbitrary in it.
  createEffect(
    on(queryKey, () => {
      if (scroller) scroller.scrollTop = 0;
      setScrollTop(0);
    }),
  );

  const virtualized = createMemo(() => visible().length > VIRTUALIZE_ABOVE);

  const window_ = createMemo(() => {
    measurementVersion();
    const rows = visible();
    if (!virtualized()) return { start: 0, end: rows.length };
    const range = measured.range(
      scrollTop(),
      viewportHeight() || ROW_ESTIMATE * 20,
      320,
    );
    return { start: range.start, end: range.end, top: range.top };
  });

  const windowRows = createMemo(() => {
    const { start, end } = window_();
    return visible().slice(start, end);
  });

  // -- opening an item ------------------------------------------------------

  const open = (entry: RankedEntry) => {
    if (entry.origin !== "declared") return;
    openWorkItemTab(entry.ref);
    navigate(`/w/${encodeURIComponent(entry.ref)}`);
  };

  // -- filter plumbing ------------------------------------------------------

  const update = <K extends keyof Filter>(key: K, value: Filter[K]) => {
    setFilter({ ...filter(), [key]: value });
  };

  const toggleIn = (key: "kinds" | "states", value: string) => {
    const current = filter()[key];
    update(
      key,
      current.includes(value)
        ? current.filter((item) => item !== value)
        : [...current, value],
    );
  };

  const saveCurrent = (name: string) => {
    const next = [
      { name, filter: filter() },
      ...savedFilters().filter((entry) => entry.name !== name),
    ];
    setSavedFilters(persistSavedFilters(next));
  };

  const recallSaved = (name: string) => {
    const entry = savedFilters().find((one) => one.name === name);
    if (entry) setFilter(entry.filter);
  };

  const removeSaved = (name: string) => {
    setSavedFilters(persistSavedFilters(savedFilters().filter((e) => e.name !== name)));
  };

  // -- what is filtering this view, said as chips (FR-U14, Stage 7) --------
  //
  // The view itself is not a chip: `backlog` and `bugs` are two ranked views
  // over the same filter set, chosen by the tabs above, and dropping the view
  // is not something a reader can mean. Page size is not a chip either — it
  // is how much of the ranked answer is loaded, not which part of it.

  const activeFilterChips = createMemo(() => {
    const active = filter();
    const chips: { key: string; label: string; title?: string }[] = [];
    if (active.project)
      chips.push({
        key: "project",
        label: `Project: ${projectLabel(active.project)}`,
        title: active.project,
      });
    if (active.kinds.length)
      chips.push({ key: "kinds", label: `Type: ${active.kinds.join(", ")}` });
    if (active.states.length)
      // The suffix is the whole honesty of this filter: it narrows the loaded
      // page rather than asking the estate, because the ranked views take no
      // state parameter (#226).
      chips.push({
        key: "states",
        label: `State: ${active.states.join(", ")} · this page only`,
      });
    if (active.label) chips.push({ key: "label", label: `Label: ${active.label}` });
    if (active.initiative)
      chips.push({ key: "initiative", label: `Initiative: ${active.initiative}` });
    if (active.actor)
      chips.push({
        key: "actor",
        label: `Actor: ${actorLabel(active.actor)}`,
        title: active.actor,
      });
    if (active.q) chips.push({ key: "q", label: `Search: “${active.q}” · this page only` });
    const ex = active.exclude;
    // #351: exclusions read as negations, so a reader never mistakes one for
    // the inclusion it is the opposite of.
    if (ex.projects.length)
      chips.push({
        key: "not:project",
        label: `Not project: ${ex.projects.map(projectLabel).join(", ")}`,
        title: ex.projects.join(", "),
      });
    if (ex.kinds.length)
      chips.push({ key: "not:kind", label: `Not type: ${ex.kinds.join(", ")}` });
    if (ex.states.length)
      chips.push({ key: "not:state", label: `Not state: ${ex.states.join(", ")}` });
    if (ex.labels.length)
      chips.push({ key: "not:label", label: `Not label: ${ex.labels.join(", ")}` });
    return chips;
  });

  const patchExclude = (next: Partial<ExcludeFilter>) =>
    update("exclude", { ...filter().exclude, ...next });

  const removeFilter = (key: string) => {
    switch (key) {
      case "project": update("project", ""); break;
      case "kinds": update("kinds", []); break;
      case "states": update("states", []); break;
      case "label": update("label", ""); break;
      case "initiative": update("initiative", ""); break;
      case "actor": update("actor", ""); break;
      case "q": update("q", ""); break;
      case "not:project": patchExclude({ projects: [] }); break;
      case "not:kind": patchExclude({ kinds: [] }); break;
      case "not:state": patchExclude({ states: [] }); break;
      case "not:label": patchExclude({ labels: [] }); break;
    }
  };

  // -- the exclusion builder (#351): one facet, one value, over the loaded page.
  type ExcludeField = keyof ExcludeFilter;
  interface ExcludeFacet {
    field: ExcludeField;
    label: string;
    options: () => { value: string; label: string }[];
  }
  const excludeFacets: ExcludeFacet[] = [
    {
      field: "projects",
      label: "Project",
      options: () => projectOptions().map((one) => ({ value: one, label: projectLabel(one) })),
    },
    {
      field: "kinds",
      label: "Type",
      options: () => WORK_KINDS.map((one) => ({ value: one, label: one })),
    },
    {
      field: "states",
      label: "State",
      options: () => stateOptions().map((one) => ({ value: one, label: one })),
    },
    {
      field: "labels",
      label: "Label",
      options: () => labelOptions().map((one) => ({ value: one, label: one })),
    },
  ];
  const [excludeField, setExcludeField] = createSignal<ExcludeField>("projects");
  const [excludeValue, setExcludeValue] = createSignal("");
  const currentExcludeFacet = () =>
    excludeFacets.find((one) => one.field === excludeField()) ?? excludeFacets[0]!;
  const excludeOptions = createMemo(() => {
    const already = new Set(filter().exclude[excludeField()]);
    return currentExcludeFacet()
      .options()
      .filter((one) => !already.has(one.value));
  });
  const addExclude = () => {
    const value = excludeValue();
    if (!value) return;
    const field = excludeField();
    const current = filter().exclude[field];
    if (current.includes(value)) return;
    patchExclude({ [field]: [...current, value] } as Partial<ExcludeFilter>);
    setExcludeValue("");
  };

  const counts = createMemo(() => {
    const result = view();
    if (!result) return null;
    return {
      considered: readNumber(result, "total_considered"),
      declared: readNumber(result, "declared"),
      observed: readNumber(result, "observed"),
      suppressed: readNumber(result, "suppressed"),
    };
  });

  const freshness = createMemo(() => describeFreshness(view()));

  return (
    <div class="vogt-surface vogt-backlog">
      <SurfaceHeader
        class="vogt-backlog-header"
        label="Backlog header"
        title={<h1>Backlog</h1>}
        // On a phone the view-age line, the Backlog/Bugs switch, Refresh and
        // the freshness disclosure fold behind one control so the first screen
        // belongs to the ranked work rather than to the chrome over it (#226,
        // the same first-viewport treatment #229 gave the Board).
        collapseControls
        collapseHonesty
        honestyClass={honestyToneClass(viewAge().tone)}
        honesty={(
          <div class="vogt-backlog-honesty" aria-live="polite">
          <strong><ViewAgeBadge
            age={viewAge()}
            class="vogt-backlog-age"
            title="How long ago this page last got an answer from Vogt — not how old the evidence behind that answer is, which the detail below breaks down"
          /></strong>
          </div>
        )}
        controls={(
          <>
            <div class="vogt-backlog-views surface-header-tabs" role="group" aria-label="Ranked views">
              <For each={["backlog", "bugs"] as ViewName[]}>
                {(name) => (
                  <button
                    type="button"
                    aria-pressed={filter().view === name}
                    class={`vogt-backlog-viewtab${filter().view === name ? " active" : ""}`}
                    onClick={() => update("view", name)}
                  >
                    {name === "backlog" ? "Backlog" : "Bugs"}
                  </button>
                )}
              </For>
            </div>
            <label class="vogt-backlog-field vogt-backlog-field--tight">
              <span>Refresh</span>
              <select
                value={String(poll())}
                onInput={(event) =>
                  setPollSeconds(Number.parseInt(event.currentTarget.value, 10))
                }
              >
                <For each={POLL_CHOICES}>
                  {(seconds) => (
                    <option value={String(seconds)}>
                      {seconds === 0 ? "Off" : `Every ${seconds}s`}
                    </option>
                  )}
                </For>
              </select>
            </label>
            <button type="button" onClick={refresh} disabled={ranked.loading}>
              {ranked.loading ? "Loading…" : "Refresh now"}
            </button>
          </>
        )}
        action={(
          <button type="button" onClick={openQuickCreate}>
            Quick create
          </button>
        )}
        detail={(
          <details class="surface-header-disclosure">
            <summary>How fresh the evidence behind this ranking is</summary>
            <p class={`vogt-backlog-freshness ${freshness().status}`}>
              {freshness().text}
              <Show when={freshness().collectors.length}>
                <span class="vogt-backlog-collectors">
                  <For each={freshness().collectors}>
                    {([name, age]) => (
                      <span class="vogt-backlog-collector">
                        {name}: {age}
                      </span>
                    )}
                  </For>
                </span>
              </Show>
            </p>
          </details>
        )}
      />

      <ProgressiveFilters
        surface="Backlog"
        prefix="vogt-backlog"
        chips={activeFilterChips()}
        onRemove={removeFilter}
        onClear={() => setFilter({ ...EMPTY_FILTER, view: filter().view })}
        clearDisabled={filterIsEmpty({ ...filter(), view: "backlog" })}
        lenses={(
          /* FR-U14's second clause: a combination worth keeping is a lens. */
          <SavedLenses
            prefix="vogt-backlog"
            lenses={savedFilters().map((entry) => ({
              name: entry.name,
              title: describeFilter(entry.filter),
              isDefault: entry.name === defaultLens(),
            }))}
            onSave={saveCurrent}
            onRecall={recallSaved}
            onForget={removeSaved}
            onDefault={toggleDefaultLens}
            note="saved lenses are kept in this browser · a starred lens loads on a bare /backlog"
          />
        )}
      >
        <label class="vogt-backlog-field vogt-backlog-field-wide">
          <span>Search</span>
          {/* #350: free text over the loaded page's titles (and adopted item
              bodies). A filter like every other — it deep-links and saves into
              a lens — so it is in the URL, not a separate box. */}
          <input
            type="search"
            class="vogt-backlog-search"
            placeholder="Match title or body — this page only"
            aria-label="Search ranked work"
            value={filter().q}
            onInput={(event) => update("q", event.currentTarget.value)}
          />
        </label>

        <label class="vogt-backlog-field">
          <span>Project</span>
          <select
            value={optionValue(filter().project, projectOptions())}
            onInput={(event) => update("project", event.currentTarget.value)}
          >
            <option value="">Any project</option>
            <For each={projectOptions()}>
              {(slug) => <option value={slug}>{slug}</option>}
            </For>
          </select>
        </label>

        <label class="vogt-backlog-field">
          <span>Label</span>
          <select
            value={optionValue(filter().label, labelOptions())}
            onInput={(event) => update("label", event.currentTarget.value)}
          >
            <option value="">Any label</option>
            <For each={labelOptions()}>
              {(name) => <option value={name}>{name}</option>}
            </For>
          </select>
        </label>

        <label class="vogt-backlog-field">
          <span>Initiative</span>
          <select
            value={optionValue(filter().initiative, initiativeOptions())}
            disabled={filter().view === "bugs"}
            title={
              filter().view === "bugs"
                ? "The bugs view takes no initiative parameter"
                : undefined
            }
            onInput={(event) => update("initiative", event.currentTarget.value)}
          >
            <option value="">Any initiative</option>
            <For each={initiativeOptions()}>
              {(slug) => <option value={slug}>{slug}</option>}
            </For>
          </select>
        </label>

        <label class="vogt-backlog-field">
          <span>Actor</span>
          <select
            value={optionValue(filter().actor, actorOptions())}
            onInput={(event) => update("actor", event.currentTarget.value)}
          >
            <option value="">Anyone</option>
            <For each={actorOptions()}>
              {(ref) => <option value={ref}>{ref}</option>}
            </For>
          </select>
        </label>

        <label class="vogt-backlog-field">
          <span>Page size</span>
          <select
            value={String(limit())}
            onInput={(event) => setLimit(Number.parseInt(event.currentTarget.value, 10))}
          >
            <For each={PAGE_SIZES}>
              {(size) => <option value={String(size)}>{size} rows</option>}
            </For>
          </select>
        </label>

        <div class="vogt-backlog-field vogt-backlog-field-wide">
          <span>Type</span>
          <div class="vogt-backlog-chiprow">
            <For each={WORK_KINDS}>
              {(kind) => (
                <button
                  type="button"
                  class={`vogt-backlog-chip${filter().kinds.includes(kind) ? " on" : ""}`}
                  aria-pressed={filter().kinds.includes(kind)}
                  disabled={filter().view === "bugs"}
                  title={
                    filter().view === "bugs"
                      ? "The bugs view is kind=bug by definition"
                      : undefined
                  }
                  onClick={() => toggleIn("kinds", kind)}
                >
                  {kind}
                </button>
              )}
            </For>
          </div>
        </div>

        <div class="vogt-backlog-field vogt-backlog-field-wide">
          <span>State</span>
          <div class="vogt-backlog-chiprow">
            <Show
              when={stateOptions().length}
              fallback={<span class="vogt-backlog-muted">no states on this page</span>}
            >
              <For each={stateOptions()}>
                {(state) => (
                  <button
                    type="button"
                    class={`vogt-backlog-chip${filter().states.includes(state) ? " on" : ""}`}
                    aria-pressed={filter().states.includes(state)}
                    onClick={() => toggleIn("states", state)}
                  >
                    {state}
                  </button>
                )}
              </For>
            </Show>
            <span class="vogt-backlog-muted">
              narrows the loaded page — the ranked views take no state parameter
            </span>
          </div>
        </div>

        <div class="vogt-backlog-field vogt-backlog-field-wide vogt-backlog-exclude">
          <span>Exclude</span>
          {/* #351: a negation is a facet plus a value. It lands as a "Not …"
              chip and round-trips under its own `not_*` URL key, so a stale
              reader drops it rather than misreading it. Over the loaded page,
              like the state and search filters. */}
          <div class="vogt-backlog-exclude-builder">
            <select
              aria-label="Exclude which facet"
              value={excludeField()}
              onInput={(event) => {
                setExcludeField(event.currentTarget.value as ExcludeField);
                setExcludeValue("");
              }}
            >
              <For each={excludeFacets}>
                {(facet) => <option value={facet.field}>{facet.label}</option>}
              </For>
            </select>
            <select
              aria-label="Exclude which value"
              value={excludeValue()}
              onInput={(event) => setExcludeValue(event.currentTarget.value)}
            >
              <option value="">Choose a value…</option>
              <For each={excludeOptions()}>
                {(option) => <option value={option.value}>{option.label}</option>}
              </For>
            </select>
            <button type="button" onClick={addExclude} disabled={!excludeValue()}>
              Exclude
            </button>
          </div>
        </div>
      </ProgressiveFilters>

      <Show when={facetNote()}>
        <p class="vogt-backlog-note">{facetNote()}</p>
      </Show>

      <Show when={createOpen()}>
        <form class="vogt-backlog-create" onSubmit={(event) => void submitCreate(event)}>
          <div class="vogt-backlog-create-grid">
            <label class="vogt-backlog-field vogt-backlog-field-wide">
              <span>Title</span>
              <input
                type="text"
                required
                value={draftTitle()}
                placeholder="What needs doing"
                onInput={(event) => setDraftTitle(event.currentTarget.value)}
              />
            </label>
            <label class="vogt-backlog-field">
              <span>Type</span>
              <select
                value={draftKind()}
                onInput={(event) => setDraftKind(event.currentTarget.value)}
              >
                <For each={WORK_KINDS}>
                  {(kind) => <option value={kind}>{kind}</option>}
                </For>
              </select>
            </label>
            <label class="vogt-backlog-field">
              <span>Project</span>
              <select
                value={optionValue(draftProject(), projectOptions())}
                onInput={(event) => setDraftProject(event.currentTarget.value)}
              >
                <option value="">No project</option>
                <For each={projectOptions()}>
                  {(slug) => <option value={slug}>{slug}</option>}
                </For>
              </select>
            </label>
            <label class="vogt-backlog-field vogt-backlog-field-wide">
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
          <div class="vogt-backlog-create-actions">
            <button type="submit" disabled={!createReady() || creating()}>
              {creating() ? "Creating…" : "Create"}
            </button>
            <button type="button" onClick={() => setCreateOpen(false)}>
              Close
            </button>
            <span class="vogt-backlog-muted">
              Everything else — body, priority, effort, labels, assignee — is set on
              the item itself.
            </span>
            <Show when={created()}>
              {(ref) => (
                <span class="vogt-backlog-created">
                  Created{" "}
                  <button
                    type="button"
                    class="vogt-backlog-link"
                    onClick={() => {
                      openWorkItemTab(ref());
                      navigate(`/w/${encodeURIComponent(ref())}`);
                    }}
                  >
                    {ref()}
                  </button>
                </span>
              )}
            </Show>
          </div>
        </form>
      </Show>

      <div class="vogt-backlog-count">
        <Show when={counts()} fallback={<span>—</span>}>
          {(summary) => (
            <span>
              <Show
                when={pageNarrowed()}
                fallback={<>{visible().length} shown</>}
              >
                {/* State, search and exclusion are page-only, so the count says
                    so out loud: N of the M rows this page loaded (#226). */}
                {visible().length} of {entries().length} loaded rows
              </Show>
              <Show when={summary().considered !== null}>
                {" "}
                · {summary().considered} considered
              </Show>
              <Show when={summary().declared !== null}>
                {" "}
                · {summary().declared} declared, {summary().observed} observed,{" "}
                {summary().suppressed} suppressed
              </Show>
            </span>
          )}
        </Show>
        <Show when={entries().length >= limit()}>
          <span class="vogt-backlog-muted">
            {limit() >= MAX_PAGE_SIZE
              ? "this is the largest page the ranked views serve — narrow the filters to see further down the estate"
              : "the page is full; raise the page size to load more"}
          </span>
        </Show>
      </div>

      <Show when={outage()}>
        {(failure) => (
          <div class="vogt-backlog-outage" role="alert">
            <h3>
              {failure().unavailable
                ? "Vogt is not answering"
                : "This view failed to load"}
            </h3>
            <p>{failure().message}</p>
            <p class="vogt-backlog-muted">
              No rows are shown because none were read. An empty backlog and an
              unreachable one are not the same answer.
            </p>
            <button type="button" onClick={refresh}>
              Try again
            </button>
          </div>
        )}
      </Show>

      <Show when={!outage() && scopeUnlinked()}>
        <div class="board-banner board-banner--candidates" role="note">
          <strong>
            This project is not linked to a forge, so it has no backlog.
          </strong>
          <span class="board-banner-detail">
            Link or publish this project to track work upstream: pick its
            repository from <a href="#/projects">Projects</a> (import or{" "}
            <code>forge link</code>), or create one with{" "}
            <code>forge publish</code>.
            {unlinkedPending() > 0
              ? ` ${unlinkedPending()} existing native ${
                  unlinkedPending() === 1 ? "item" : "items"
                } will migrate upstream when you do.`
              : ""}
          </span>
        </div>
      </Show>

      <Show when={!outage()}>
        <div class="vogt-backlog-listwrap">
          <div class="vogt-backlog-headrow">
            <label class="vogt-backlog-cell-select vogt-tickbox">
              <input
                type="checkbox"
                aria-label="Select every declared row"
                checked={allSelected()}
                disabled={selectableRefs().length === 0}
                onChange={(event) =>
                  setSelected(event.currentTarget.checked ? selectableRefs() : [])
                }
              />
            </label>
            {/* The rows are no longer columns, so neither is this: it says
                what a row carries rather than naming cells that moved. */}
            <span>Rank</span>
            <span class="vogt-backlog-headnote">
                Vogt's order · every row keeps its ref, trust, age and score
            </span>
          </div>

          <div
            class="vogt-backlog-list"
            ref={(node) => (scroller = node)}
            onScroll={(event) => setScrollTop(event.currentTarget.scrollTop)}
          >
            {bulkBar()}
            <Show
              when={visible().length}
              fallback={
                <div class="vogt-backlog-empty">
                  <Show
                    when={!ranked.loading && !entries().length}
                    fallback={
                      <p>
                        {ranked.loading
                          ? "Loading the ranked view…"
                          : "Nothing on this page is in the selected states."}
                      </p>
                    }
                  >
                    {/* An empty ranking that offers a way forward (#246): it
                        names the collector freshness so "nothing ranked" and
                        "nothing has looked" stop reading alike, then offers the
                        one write that changes it. */}
                    <p>Nothing is ranked here.</p>
                    <p class={`vogt-backlog-empty-freshness ${freshness().status}`}>
                      {freshness().text}
                    </p>
                    <button
                      type="button"
                      class="vogt-backlog-empty-action"
                      onClick={openQuickCreate}
                    >
                      Quick create
                    </button>
                  </Show>
                </div>
              }
            >
              <div style={{ height: `${measured.totalHeight()}px`, position: "relative" }}>
                <div
                  style={{
                    position: "absolute",
                    top: `${window_().top ?? measured.offsetOf(window_().start)}px`,
                    left: "0",
                    right: "0",
                  }}
                >
                  <For each={windowRows()}>
                    {(entry, index) => {
                      const sourceUrl = readString(entry, "source_url");
                      // The ranked position, not the position in the window:
                      // "3rd" has to mean 3rd in the server's order however
                      // far down the list the reader has scrolled.
                      const rank = () => window_().start + index() + 1;
                      const expanded = () => expandedRows().has(entry.ref);
                      const detailId = `backlog-row-${entry.ref}-detail`;
                      const ownWhy = () =>
                        shown().find((one) => one.ref === entry.ref) ?? null;
                      return (
                        <div
                          ref={(node) => {
                            const observer = new ResizeObserver(() => {
                              const change = measured.measure(
                                entry.ref,
                                node.getBoundingClientRect().height,
                              );
                              if (!change) return;
                              setMeasurementVersion((version) => version + 1);
                              if (change.index < window_().start) {
                                const next = Math.max(0, scrollTop() + change.delta);
                                setScrollTop(next);
                                if (scroller) scroller.scrollTop = next;
                              }
                            });
                            observer.observe(node);
                            onCleanup(() => observer.disconnect());
                          }}
                          class={`vogt-backlog-row${
                            explained().includes(entry.ref) ? " explained" : ""
                          }`}
                        >
                          <div class="vogt-backlog-row-facts">
                            <label class="vogt-backlog-cell-select vogt-tickbox">
                              <input
                                type="checkbox"
                                aria-label={`Select ${entry.ref}`}
                                checked={selectedSet().has(entry.ref)}
                                disabled={entry.origin !== "declared"}
                                title={
                                  entry.origin === "declared"
                                    ? undefined
                                    : "Observed subjects have no work item to transition"
                                }
                                onChange={() => toggleSelected(entry.ref)}
                              />
                            </label>
                            <span class="vogt-backlog-rank" title="Rank in the server's order">
                              {rank()}
                            </span>
                            <span class="vogt-backlog-cell-ref vogt-backlog-mono">
                              <Show
                                when={entry.origin === "declared"}
                                fallback={
                                  <Show when={sourceUrl} fallback={<span>{entry.ref}</span>}>
                                    {(url) => (
                                      <a href={url()} target="_blank" rel="noreferrer">
                                        {entry.ref}
                                      </a>
                                    )}
                                  </Show>
                                }
                              >
                                <button
                                  type="button"
                                  class="vogt-backlog-link"
                                  onClick={() => open(entry)}
                                >
                                  {entry.ref}
                                </button>
                              </Show>
                            </span>
                            <span class="vogt-backlog-cell-trust">
                              <span
                                class={`vogt-backlog-trust trust-${trustOf(entry)}`}
                                title={`trust: ${trustOf(entry)}`}
                              >
                                {trustOf(entry)}
                              </span>
                            </span>
                            {/* Kind and state ride the facts line so that on a
                                phone, where the secondary facts fold away, the
                                one surviving meta line reads ref · kind · state
                                · score (#226). */}
                            <span class="vogt-backlog-cell-kind">{entry.kind}</span>
                            <span class="vogt-backlog-cell-state">{entry.state}</span>
                            <span class="vogt-backlog-age">{formatWhen(entry.updated_at)}</span>
                            <span class="vogt-backlog-cell-score">
                              <button
                                type="button"
                                class="vogt-backlog-score"
                                title="Why is this ranked here?"
                                onClick={() => toggleWhy(entry.ref)}
                              >
                                {entry.score.toFixed(2)}
                                <span class="vogt-backlog-why">why</span>
                              </button>
                            </span>
                            <div class="vogt-backlog-row-quick">
                              {/* A declared row's most common act is reachable
                                  without opening the detail first (#226). */}
                              <Show when={entry.origin === "declared"}>
                                <button
                                  type="button"
                                  class="vogt-backlog-row-session"
                                  onClick={() => beginRowAct(entry.ref, "session")}
                                >
                                  Start a session…
                                </button>
                              </Show>
                              <button
                                type="button"
                                class="vogt-backlog-row-toggle"
                                aria-expanded={expanded()}
                                aria-controls={detailId}
                                onClick={() => toggleRow(entry.ref)}
                              >
                                {expanded() ? "Less" : "More"}
                              </button>
                            </div>
                          </div>

                          {/* FR-U25: the title wraps to what it says, and the
                              row is as tall as that makes it. */}
                          <div class="vogt-backlog-row-title">
                            {entry.title}
                            <Show when={entry.origin === "observed"}>
                              <span class="vogt-backlog-origin">observed</span>
                            </Show>
                          </div>

                          <div class="vogt-backlog-row-tags">
                            <span>{entry.priority}</span>
                            <Show
                              when={entry.project_slug}
                              fallback={<span>no project</span>}
                            >
                              {(slug) => (
                                <a
                                  class="vogt-backlog-rowproject"
                                  href={`#/projects?p=${encodeURIComponent(slug())}`}
                                  title={slug()}
                                >
                                  {projectLabel(slug())}
                                </a>
                              )}
                            </Show>
                            <For each={entry.labels ?? []}>
                              {(label) => <span class="vogt-backlog-rowlabel">{label}</span>}
                            </For>
                          </div>

                          <Show when={expanded()}>
                            <div class="vogt-backlog-row-detail" id={detailId}>
                              <Show when={entry.origin === "observed"}>
                                <p class="vogt-backlog-provenance">
                                  <span>
                                    Observed{" "}
                                    {readString(entry, "observation_kind") ?? "subject"}
                                  </span>
                                  <Show when={readString(entry, "observed_at")}>
                                    {(at) => <span>seen {formatWhen(at())}</span>}
                                  </Show>
                                  <Show when={readString(entry, "adopted_as")}>
                                    {(ref) => <span>adopted as {ref()}</span>}
                                  </Show>
                                  <Show when={sourceUrl}>
                                    {(url) => (
                                      <a href={url()} target="_blank" rel="noreferrer">
                                        source
                                      </a>
                                    )}
                                  </Show>
                                </p>
                              </Show>

                              {/* The ranking evidence, attributed to the row it
                                  explains rather than only to a table below. */}
                              <Show
                                when={ownWhy()}
                                fallback={
                                  <p class="vogt-backlog-muted">
                                    {explained().includes(entry.ref)
                                      ? "Asking Vogt why this is ranked here…"
                                      : "Open why to see the inputs behind this score."}
                                  </p>
                                }
                              >
                                {(explanation) => (
                                  <ul class="vogt-backlog-why-factors">
                                    <For each={explanation().contributions}>
                                      {(contribution) => (
                                        <li>
                                          <span class="vogt-backlog-why-input">
                                            {contribution.input}
                                          </span>
                                          <span class="vogt-backlog-why-detail">
                                            {contribution.detail}
                                          </span>
                                          <span class="vogt-backlog-delta">
                                            {contribution.contribution >= 0 ? "+" : ""}
                                            {contribution.contribution.toFixed(2)}
                                          </span>
                                        </li>
                                      )}
                                    </For>
                                    <Show when={explanation().pending.length}>
                                      <li class="vogt-backlog-muted">
                                        not yet available:{" "}
                                        {explanation()
                                          .pending.map(([name]) => name)
                                          .join(", ")}
                                      </li>
                                    </Show>
                                  </ul>
                                )}
                              </Show>

                              {rowActions(entry)}
                            </div>
                          </Show>
                        </div>
                      );
                    }}
                  </For>
                </div>
              </div>
            </Show>
          </div>
        </div>
      </Show>

      <Show when={explained().length}>
        <section class="vogt-backlog-explain" aria-label="Ranking explanation">
          <div class="vogt-backlog-explain-head">
            <strong>Why these are ranked where they are</strong>
            <button type="button" onClick={() => setExplained([])}>
              Close
            </button>
          </div>
          <Show
            when={!explanations.loading}
            fallback={<p class="vogt-backlog-muted">Asking Vogt for the contributions…</p>}
          >
            <For each={whyFailures()}>
              {(message) => <p class="vogt-backlog-note">{message}</p>}
            </For>
            <Show when={shown().length}>
              <table class="vogt-backlog-explain-table">
                <thead>
                  <tr>
                    <th>Input</th>
                    <For each={shown()}>
                      {(explanation) => (
                        <th>
                          <span class="vogt-backlog-mono">{explanation.ref}</span>
                          <span class="vogt-backlog-explain-total">
                            total {explanation.total.toFixed(2)}
                          </span>
                          <span class="vogt-backlog-explain-title">{explanation.title}</span>
                        </th>
                      )}
                    </For>
                    <Show when={shown().length === 2}>
                      <th>Difference</th>
                    </Show>
                  </tr>
                </thead>
                <tbody>
                  <For each={comparedInputs()}>
                    {(input) => {
                      const cells = createMemo(() =>
                        shown().map(
                          (explanation) =>
                            explanation.contributions.find((c) => c.input === input) ?? null,
                        ),
                      );
                      // Two open explanations turn a pair of scores into an
                      // ordering with a cause: this column is the whole of
                      // "why is this above that", input by input.
                      const delta = createMemo(() => {
                        const found = cells();
                        if (found.length !== 2) return null;
                        return (found[0]?.contribution ?? 0) - (found[1]?.contribution ?? 0);
                      });
                      return (
                        <tr>
                          <th scope="row">{input}</th>
                          <For each={cells()}>
                            {(cell) => (
                              <td>
                                <Show
                                  when={cell}
                                  fallback={
                                    <span class="vogt-backlog-muted">
                                      not an input for this entry
                                    </span>
                                  }
                                >
                                  {(contribution) => (
                                    <>
                                      <span class="vogt-backlog-contrib">
                                        {contribution().contribution.toFixed(2)}
                                      </span>
                                      <span class="vogt-backlog-muted">
                                        {" "}
                                        = {contribution().value.toFixed(2)} x{" "}
                                        {contribution().weight.toFixed(2)}
                                      </span>
                                      <span class="vogt-backlog-bar-track">
                                        <span
                                          class={`vogt-backlog-bar${
                                            contribution().contribution < 0 ? " negative" : ""
                                          }`}
                                          style={{
                                            width: `${Math.min(
                                              100,
                                              (Math.abs(contribution().contribution) /
                                                contributionScale()) *
                                                100,
                                            )}%`,
                                          }}
                                        />
                                      </span>
                                      <Show when={contribution().detail}>
                                        <span class="vogt-backlog-detail">
                                          {contribution().detail}
                                        </span>
                                      </Show>
                                    </>
                                  )}
                                </Show>
                              </td>
                            )}
                          </For>
                          <Show when={delta() !== null}>
                            <td class="vogt-backlog-delta">
                              {(delta() ?? 0) > 0 ? "+" : ""}
                              {(delta() ?? 0).toFixed(2)}
                            </td>
                          </Show>
                        </tr>
                      );
                    }}
                  </For>
                </tbody>
              </table>
              <For each={shown()}>
                {(explanation) => (
                  <Show when={explanation.pending.length}>
                    <p class="vogt-backlog-note">
                      {explanation.ref}: documented inputs that cannot fire in this build,
                      so their absence is not a zero:{" "}
                      <For each={explanation.pending}>
                        {([name, reason]) => (
                          <span class="vogt-backlog-pending">
                            {name} - {reason}
                          </span>
                        )}
                      </For>
                    </p>
                  </Show>
                )}
              </For>
              <Show when={shown().length < 2}>
                <p class="vogt-backlog-muted">
                  Open a second row's score to compare two entries input by input.
                </p>
              </Show>
            </Show>
          </Show>
        </section>
      </Show>
    </div>
  );
};

export default Backlog;
