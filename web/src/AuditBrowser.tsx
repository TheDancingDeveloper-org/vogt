// The audit browser and the notification inbox (FR-U19, FR-N3/FR-U3, NFR-S5).
//
// M11's global surfaces share a tab because they answer the same shape of
// question — "what happened, and who says so" — from the two halves of the
// product: Vogt's own audited writes, and what a forge is trying to say about
// the projects it watches. They are never merged into one list; `/events` and
// the forge inbox have different origins and different owners, and the audit
// log is a third thing again.
//
// Five rules this file exists to keep:
//
//   1. **Every row shows who, what and why.** Vogt refuses a write without a
//      reason precisely so this view can answer "why did this change" months
//      later (FR-S1, FR-W1). A row that showed the operation and dropped the
//      reason would waste the whole mechanism, so the reason is rendered on
//      every row — and a record that somehow carries none says *that*, rather
//      than leaving the space blank.
//   2. **Every filter is the query.** `audit.list` takes an actor, an
//      operation, an entity, a project and a half-open time range, and pages
//      with a limit and an offset. Nothing here narrows rows that were already
//      loaded, and that is the whole difference: a filter applied to a loaded
//      page and one applied to the store look identical on screen and differ
//      entirely in what they can see. If a filter cannot be pushed — an actor
//      this page could not turn into an id, a bound that will not parse — the
//      read does not happen at all, because a wider query rendered under a
//      narrower filter's heading is the failure this surface exists against.
//   3. **A page says how much of what it is.** The count beside the rows is
//      `total`: how many records match the *narrowing*, not how many are on
//      screen. `offset` is what reaches the rest of them, so the log is
//      readable to its beginning rather than to the newest few hundred rows.
//   4. **A query is a place.** The whole filter set lives in the URL (FR-U11),
//      which is what makes FR-U19's second clause work: a work item's detail
//      view links to `#/audit?ref=WI-7` (or `?entity=<id>`) and lands here
//      with that filter restored and pushed to the server.
//   5. **Absence is stated.** An unreachable Vogt renders as an outage with
//      the server's own reason, never as an empty audit log (FR-U21). Of every
//      surface in the product this is the one where an empty list reads as a
//      claim: "nothing has ever been written here".
//
// Everything reaches Vogt through `vogtApi.ts`; there is no fetch in this file
// and there must not be one — `tests/test_pwa.py` is what says so.

import {
  Component,
  For,
  Show,
  createEffect,
  createMemo,
  createResource,
  createSignal,
} from "solid-js";
import { useLocation, useSearchParams } from "@solidjs/router";
import {
  VogtUnavailable,
  getWork,
  listActors,
  listAudit,
  listProjects,
  notifications,
  type AuditRecord,
  type FreshnessSummary,
} from "./vogtApi";
import SurfaceHeader from "./SurfaceHeader";
import { ViewAgeBadge, createLoadStamp, createViewAge, honestyToneClass, onVogtLive } from "./viewAge";

interface Props {
  onError?: (message: string) => void;
}

// -- constants --------------------------------------------------------------

/**
 * How many records one page asks for, and therefore how many are rendered.
 *
 * One dial rather than two. `audit.list` takes `limit` and `offset` and
 * answers with `total`, so a page here is a page of the log itself: there is
 * no window to render a slice of, and no reason for the number fetched and
 * the number shown to differ.
 *
 * 500 is the server-side cap on `ListAuditParams.limit`. Offering a larger
 * size would be the picker lying about what the server did with it.
 *
 * Paged rather than virtualized like the backlog, for the reason it always
 * was: an audit row is variable-height — the reason is a sentence somebody
 * typed and is never truncated to fit a grid — and a fixed row height is what
 * the backlog's windowing arithmetic needs (NFR-S5).
 */
const PAGE_SIZES = [25, 50, 100, 200, 500] as const;
const DEFAULT_PAGE_SIZE = 50;
const MAX_PAGE_SIZE = 500;

/** Notifications the inbox asks for per page. `NotificationsParams` pages properly. */
const INBOX_PAGE_SIZE = 50;

/** The collector behind FR-N3. Its absence from a sweep is a real answer. */
const NOTIFICATION_COLLECTOR = "gh-notifications";


/** The URL keys this surface owns. Anything else in the query is left alone. */
const URL_KEYS = [
  "view",
  "actor",
  "op",
  "entity",
  "ref",
  "project",
  "from",
  "to",
  "nreason",
  "unread",
  "size",
  "page",
] as const;

type ViewName = "audit" | "inbox";

/** Every filter either view offers, in one bag so the URL has one encoder. */
interface Filter {
  view: ViewName;
  /** An actor's `identity_ref`, resolved to the `actor_id` the server takes. */
  actor: string;
  /** An operation name, exact — `audit.list` matches it literally. */
  operation: string;
  /** An entity id, as `audit.list` takes it. */
  entity: string;
  /** A work item ref, resolved to that item's entity id. */
  ref: string;
  /** A project slug. Pushed to the server by both views. */
  project: string;
  /**
   * `datetime-local` text, in this browser's time zone, sent as an instant.
   *
   * `from` becomes `since` and `to` becomes `until`. The interval is
   * half-open the way the server's is — `since` inclusive, `until` exclusive
   * — so two ranges that share a boundary tile the log rather than both
   * claiming the write made exactly at the seam.
   */
  from: string;
  to: string;
  /** GitHub's notification reason. Named apart from an audit record's reason. */
  nreason: string;
  unread: boolean;
}

const EMPTY_FILTER: Filter = {
  view: "audit",
  actor: "",
  operation: "",
  entity: "",
  ref: "",
  project: "",
  from: "",
  to: "",
  nreason: "",
  unread: false,
};

// -- reading what the API actually sends ------------------------------------
//
// `vogtApi.ts` types each response with what its first reader needed, and
// widening those interfaces is a decision for a file this branch does not own.
// The wire carries more than they say: an actor arrives with the `id` that
// `audit.list` filters by. It is read through a local view that makes the
// field optional, so a server that stopped sending it becomes a filter that
// says it could not be pushed rather than a query with `undefined` in it.
//
// A project needs no such treatment: `audit.list` takes the slug, which is
// what the picker already offers and what a link already carries.

interface ActorRow {
  identity_ref: string;
  display_name: string;
  id?: string;
}

interface ProjectRow {
  slug: string;
  name: string;
}

/** One collected notification (`NotificationView`), as the inbox renders it. */
interface NotificationRow {
  thread: string;
  project_slug?: string | null;
  repo?: string | null;
  title: string;
  reason?: string | null;
  subject_type?: string | null;
  unread?: boolean;
  url?: string | null;
  updated_at?: string | null;
  observed_at: string;
}

/** `NotificationsResult`: the inbox, and what it is honestly able to be. */
interface InboxResult {
  notifications: NotificationRow[];
  total: number;
  by_reason?: Record<string, number>;
  unread?: number;
  scope?: string;
  freshness?: FreshnessSummary;
  detail?: string | null;
}

/** The inbox's single read (FR-N3). */
async function readInbox(params: Record<string, unknown>): Promise<InboxResult> {
  return (await notifications(params)) as unknown as InboxResult;
}

// -- results, tagged rather than thrown -------------------------------------
//
// A thrown resource error is a resource whose value cannot be read without
// rethrowing, and an outage is something this surface renders rather than
// escalates (FR-U21). So every fetcher returns its failure as a value, and
// `unavailable` keeps "Vogt could not be asked" apart from "the read failed".

type Loaded<T> =
  | { ok: true; value: T }
  | { ok: false; unavailable: boolean; message: string };

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

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

// -- time, in the reader's zone and on the wire as an instant ----------------
//
// The controls are `datetime-local`, so what a reader types is wall-clock time
// where they are sitting. `since`/`until` are instants, and the server reads a
// bound without a zone as UTC — so the conversion happens here, once, and a
// query written at 09:00 in Lisbon and 09:00 in New York are different
// questions rather than the same one answered differently.

function pad(value: number): string {
  return String(value).padStart(2, "0");
}

/** A `datetime-local` value for an instant, in this browser's zone. */
function toLocalInput(at: Date): string {
  return (
    `${at.getFullYear()}-${pad(at.getMonth() + 1)}-${pad(at.getDate())}` +
    `T${pad(at.getHours())}:${pad(at.getMinutes())}`
  );
}

/** A bound as milliseconds, or null when the field is empty or unreadable. */
function parseLocalInput(value: string): number | null {
  if (!value) return null;
  const at = new Date(value);
  return Number.isNaN(at.valueOf()) ? null : at.valueOf();
}

/** The same bound as the instant `audit.list` takes, or null. */
function toInstant(value: string): string | null {
  const ms = parseLocalInput(value);
  return ms === null ? null : new Date(ms).toISOString();
}

/** Midnight this morning, in this browser's zone. */
function startOfDay(daysAgo = 0): Date {
  const at = new Date();
  at.setHours(0, 0, 0, 0);
  at.setDate(at.getDate() - daysAgo);
  return at;
}

/**
 * A record's instant, or null when the timestamp cannot be read.
 *
 * Null is not treated as "outside the range": a row whose time this client
 * cannot parse is still a row, and dropping it would delete evidence to tidy a
 * filter. It stays, and it says its time is unreadable.
 */
function timeOf(record: AuditRecord): number | null {
  const at = new Date(record.at);
  return Number.isNaN(at.valueOf()) ? null : at.valueOf();
}

function formatWhen(value: string | null | undefined): string {
  if (!value) return "—";
  const at = new Date(value);
  return Number.isNaN(at.valueOf()) ? String(value) : at.toLocaleString();
}

function describeAge(seconds: number | null | undefined): string {
  if (seconds === null || seconds === undefined) return "an unknown time";
  if (seconds < 90) return `${Math.round(seconds)}s`;
  if (seconds < 5400) return `${Math.round(seconds / 60)}m`;
  if (seconds < 172800) return `${Math.round(seconds / 3600)}h`;
  return `${Math.round(seconds / 86400)}d`;
}

/**
 * The shortcuts, and why they are day-aligned.
 *
 * `Yesterday` ends where `Today` begins, and the two are disjoint because
 * `until` is exclusive: a write made at exactly midnight is in `Today` and in
 * nothing else. Presets that both included the seam would let a reader add up
 * two days and count one write twice, which is the arithmetic
 * `test_consecutive_windows_tile_the_log_without_gap_or_overlap` exists to
 * protect. The open-ended ones leave `to` empty rather than pinning it to
 * "now", so the range keeps meaning the same thing as the log grows under it.
 */
const RANGE_PRESETS: { label: string; range: () => { from: string; to: string } }[] = [
  { label: "Today", range: () => ({ from: toLocalInput(startOfDay()), to: "" }) },
  {
    label: "Yesterday",
    range: () => ({
      from: toLocalInput(startOfDay(1)),
      to: toLocalInput(startOfDay()),
    }),
  },
  { label: "Last 7 days", range: () => ({ from: toLocalInput(startOfDay(6)), to: "" }) },
  {
    label: "Last 30 days",
    range: () => ({ from: toLocalInput(startOfDay(29)), to: "" }),
  },
];

// -- freshness and coverage, for the inbox ----------------------------------

/**
 * What the sweep behind the inbox says about itself (FR-U2, FR-N3).
 *
 * The distinction this exists for: an empty inbox with `gh-notifications`
 * missing from the coverage map means nobody has looked, and rendering that as
 * "no notifications" would be the surface inventing good news.
 */
function describeCoverage(freshness: FreshnessSummary | undefined): {
  status: string;
  text: string;
  collectors: [string, string][];
} {
  if (!freshness) {
    return {
      status: "unknown",
      text: "the answer carried no freshness, so whether anything has looked is unknown",
      collectors: [],
    };
  }
  const collectors = Object.entries(freshness.collectors ?? {}).sort((a, b) =>
    a[0].localeCompare(b[0]),
  );
  const status = freshness.status || "never_swept";
  const parts: string[] = [];
  if (status === "never_swept") {
    parts.push(
      "no sweep has run — an empty inbox here is 'not collected', not 'nothing to say'",
    );
  } else {
    parts.push(
      `evidence is ${describeAge(freshness.age_seconds)} old at its oldest`,
    );
    if (status === "partial") parts.push("at least one collector did not complete");
    if (!collectors.some(([name]) => name === NOTIFICATION_COLLECTOR)) {
      parts.push(
        `\`${NOTIFICATION_COLLECTOR}\` is not in this sweep's coverage — the ` +
          "collector did not run, so an empty inbox below means nobody asked GitHub",
      );
    }
  }
  if (freshness.detail) parts.push(freshness.detail);
  return { status, text: parts.join(" · "), collectors };
}

// -- the URL, which is where a query lives (FR-U11) -------------------------

type Query = Partial<Record<(typeof URL_KEYS)[number], string | string[]>>;

function one(value: string | string[] | undefined): string {
  if (Array.isArray(value)) return value[0] ?? "";
  return value ?? "";
}

function filterFromQuery(query: Query): Filter {
  return {
    view: one(query.view) === "inbox" ? "inbox" : "audit",
    actor: one(query.actor),
    operation: one(query.op),
    entity: one(query.entity),
    ref: one(query.ref),
    project: one(query.project),
    from: one(query.from),
    to: one(query.to),
    nreason: one(query.nreason),
    unread: one(query.unread) === "1",
  };
}

function sizeFromQuery(query: Query): number {
  const parsed = Number.parseInt(one(query.size), 10);
  if (!Number.isFinite(parsed)) return DEFAULT_PAGE_SIZE;
  return Math.min(MAX_PAGE_SIZE, Math.max(1, parsed));
}

function pageFromQuery(query: Query): number {
  const parsed = Number.parseInt(one(query.page), 10);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : 0;
}

/** `null` clears a key; every key this surface does not own is left alone. */
function queryFor(
  filter: Filter,
  size: number,
  page: number,
): Record<(typeof URL_KEYS)[number], string | null> {
  return {
    view: filter.view === "audit" ? null : filter.view,
    actor: filter.actor || null,
    op: filter.operation || null,
    entity: filter.entity || null,
    ref: filter.ref || null,
    project: filter.project || null,
    from: filter.from || null,
    to: filter.to || null,
    nreason: filter.nreason || null,
    unread: filter.unread ? "1" : null,
    size: size === DEFAULT_PAGE_SIZE ? null : String(size),
    page: page > 0 ? String(page) : null,
  };
}

/**
 * The canonical text of this surface's slice of the query.
 *
 * Both encoders walk `URL_KEYS` in the same order, so two equal states always
 * produce the same string — which is what lets one effect tell "the user
 * changed a filter" from "somebody handed us a different URL".
 */
function encodeState(filter: Filter, size: number, page: number): string {
  const params = new URLSearchParams();
  const desired = queryFor(filter, size, page);
  for (const key of URL_KEYS) {
    const value = desired[key];
    if (value !== null) params.append(key, value);
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

/**
 * The value of a `<select>` whose options arrive asynchronously.
 *
 * The backlog's helper, for the backlog's reason: Solid re-applies `value` as
 * an effect, and a value applied before its `<option>` exists is dropped by the
 * browser — leaving the control reading "Any" while a filter is in force.
 * Touching `options` makes the re-apply happen.
 */
function optionValue(current: string, options: readonly string[]): string {
  return options.find((option) => option === current) ?? current;
}

// -- the surface ------------------------------------------------------------

/** Where this surface lives. The URL effect below acts only on this route. */
const ROUTE = "/audit";

const AuditBrowser: Component<Props> = (props) => {
  const [query, setQuery] = useSearchParams<Query>();
  const location = useLocation();

  // Read from the URL once, written from the signals afterwards. It has to be
  // this way round in this shell: activating a tab navigates to the tab's bare
  // path (`App.tsx`'s `pathFor`), which drops the query — if the URL were the
  // source of truth, switching tabs and back would clear the filter set. The
  // effect below puts it back instead.
  // …and read only when this surface is the route it is being read on. Every
  // tab is mounted at once, so at mount the query may well be the board's.
  const initial: Query = location.pathname === ROUTE ? query : {};
  const [filter, setFilter] = createSignal<Filter>(filterFromQuery(initial));
  const [pageSize, setPageSize] = createSignal<number>(sizeFromQuery(initial));
  const [page, setPage] = createSignal<number>(pageFromQuery(initial));

  const [reloadKey, setReloadKey] = createSignal(0);
  const refresh = () => setReloadKey((value) => value + 1);

  // -- URL ↔ state (FR-U11) -------------------------------------------------
  //
  // One effect, both directions, with the ambiguity resolved by remembering
  // what this surface last asserted. Two independent effects cannot do it: on a
  // filter change the URL still holds the previous value, and an adopting
  // effect would read that as an instruction and undo the change.
  let lastWritten = encodeQuery(initial);

  createEffect(() => {
    // Guarded on this surface being the route, the way the board's effect is.
    // Every tab in this shell is mounted at once, and `view`, `actor` and
    // `project` are query keys the board and the backlog also own: without the
    // guard this surface would write its filter set into their URL and adopt
    // theirs as an instruction.
    if (location.pathname !== ROUTE) return;
    const desired = encodeState(filter(), pageSize(), page());
    const current = encodeQuery(query);
    if (desired === current) {
      lastWritten = current;
      return;
    }
    if (current !== lastWritten && current !== "") {
      // The query changed to something this surface did not write, and is not
      // empty: a pasted link, the back button, or the work item detail view
      // handing us `?ref=WI-7`. That is an instruction.
      setFilter(filterFromQuery(query));
      setPageSize(sizeFromQuery(query));
      setPage(pageFromQuery(query));
      lastWritten = current;
      return;
    }
    setQuery(queryFor(filter(), pageSize(), page()), { replace: true });
    lastWritten = desired;
  });

  /**
   * Change a filter.
   *
   * The page resets here rather than in an effect on purpose: an effect that
   * watched the filter could not tell a user's edit from the back button
   * restoring a link that named page 3, and would throw the page away.
   */
  const update = <K extends keyof Filter>(key: K, value: Filter[K]) => {
    setFilter({ ...filter(), [key]: value });
    setPage(0);
  };

  /** Change the page size, and go back to the first one.
   *
   *  Keeping the page number would move the reader somewhere they did not ask
   *  to be: page 4 of 25 and page 4 of 200 are different places in the log. */
  const resize = (size: number) => {
    setPageSize(size);
    setPage(0);
  };

  // -- the facets -----------------------------------------------------------
  //
  // Loaded once. A facet list that fails is a smaller picker and a named note,
  // not a broken surface — with one exception: `audit.list` filters by
  // `actor_id`, and the id lives on the actor list. A picker that cannot load
  // is an actor filter that cannot be pushed, and that stops the read rather
  // than widening it (see `unpushable`). The project picker is only a picker:
  // the server takes the slug, so a project filter survives the list failing.

  const [actors] = createResource(() => attempt(() => listActors()));
  const [projects] = createResource(() => attempt(() => listProjects({ limit: 200 })));

  const actorRows = createMemo<ActorRow[]>(() => {
    const result = actors();
    return result && result.ok ? result.value.actors : [];
  });

  const actorByRef = createMemo(() => {
    const found = new Map<string, ActorRow>();
    for (const actor of actorRows()) found.set(actor.identity_ref, actor);
    return found;
  });

  const actorOptions = createMemo(() => {
    const seen = new Set(actorRows().map((actor) => actor.identity_ref));
    // A link is allowed to name an actor this picker did not load; a `<select>`
    // whose value matches no option renders as the first one, which would read
    // "Anyone" while a filter was in force.
    if (filter().actor) seen.add(filter().actor);
    return [...seen].sort();
  });

  const projectRows = createMemo<ProjectRow[]>(() => {
    const result = projects();
    return result && result.ok ? result.value.projects : [];
  });

  const projectOptions = createMemo(() => {
    const seen = new Set(projectRows().map((project) => project.slug));
    if (filter().project) seen.add(filter().project);
    return [...seen].sort();
  });

  /** The `actor_id` the server takes, or null when it could not be resolved. */
  const actorId = createMemo<string | null>(() => {
    const wanted = filter().actor;
    if (!wanted) return null;
    return actorByRef().get(wanted)?.id ?? null;
  });

  /** Whether the actor list has answered, one way or the other. */
  const actorsSettled = createMemo(() => actors() !== undefined);

  /**
   * Set when an actor is filtered on but could not be turned into an id.
   *
   * Only once the list has answered. A list still in flight is not a failed
   * lookup, and saying so would put a caveat on screen that a second later
   * stops being true.
   */
  const actorUnresolved = createMemo(
    () => Boolean(filter().actor) && actorsSettled() && actorId() === null,
  );

  // -- a work item ref, resolved to the entity id the log is keyed by -------
  //
  // FR-U19's second clause is a link from a work item into this browser. The
  // audit log is keyed by entity id, not by ref, so a link may carry either:
  // `?entity=<id>` goes straight to the server, and `?ref=WI-7` is resolved
  // here through `work.get` — which is what lets the link be written from the
  // ref a reader can actually see.

  // Keyed through a memo, and the key carries the reload counter: a ref that
  // failed to resolve has to be retryable, and its source would otherwise never
  // change for "Try again" to act on.
  const refKey = createMemo(() => {
    const ref = filter().ref;
    return ref ? `${reloadKey()}\n${ref}` : null;
  });

  const [refItem] = createResource(refKey, (key) =>
    attempt(() => getWork(key.slice(key.indexOf("\n") + 1))),
  );

  const refId = createMemo<string | null>(() => {
    const result = refItem();
    return result && result.ok ? result.value.item.id : null;
  });

  const refFailure = createMemo<string | null>(() => {
    const result = refItem();
    if (!result || result.ok) return null;
    return result.unavailable
      ? `Vogt cannot be reached, so ${filter().ref} could not be resolved: ${result.message}`
      : `${filter().ref} could not be resolved to an entity id: ${result.message}`;
  });

  /** The entity id to push, if there is one. An explicit id wins over a ref. */
  const entityId = createMemo<string | null>(() => filter().entity || refId());

  /**
   * A ref that is filtered on and has not become an entity id.
   *
   * True while `work.get` is in flight and true again if it failed, because
   * both mean the same thing to the query below: there is no id to push.
   */
  const refBlocked = createMemo(
    () => Boolean(filter().ref) && !filter().entity && !refId(),
  );

  // -- the time range, as the two instants the server takes ------------------

  const since = createMemo(() => toInstant(filter().from));
  const until = createMemo(() => toInstant(filter().to));

  /**
   * A bound that was asked for and cannot be sent.
   *
   * Only a URL can produce one — `?from=whenever` — but a URL is exactly how
   * this surface is arrived at (FR-U11). Dropping the bound and querying
   * anyway would answer a wider question under the narrower one's heading.
   */
  const badBound = createMemo<string | null>(() => {
    if (filter().from && since() === null) return "From";
    if (filter().to && until() === null) return "To";
    return null;
  });

  /** A range whose end is not after its start. The server would agree — and
   *  answer with nothing, which on this surface reads as "nothing happened". */
  const emptyRange = createMemo(() => {
    const from = parseLocalInput(filter().from);
    const to = parseLocalInput(filter().to);
    return from !== null && to !== null && to <= from;
  });

  /** Waiting on a lookup a filter needs, rather than on the log itself. */
  const resolving = createMemo(
    () =>
      (refBlocked() && !refFailure()) ||
      (Boolean(filter().actor) && !actorsSettled()),
  );

  /**
   * A filter that was asked for and could not be pushed.
   *
   * Every one of these stops the read rather than widening it. An actor the
   * picker could not turn into an id used to fall back to matching the loaded
   * rows by identity — which was defensible when the loaded rows were the
   * whole of what any filter could see, and is not now: it would filter one
   * page of a query the server answered *without* the actor, under a page
   * count and a total that describe that wider query. Two filters that mean
   * different things must not share a heading.
   */
  const unpushable = createMemo<string | null>(() => {
    if (actorUnresolved()) {
      return (
        `${filter().actor} is not in the actor list this page could read, so it ` +
        "could not be turned into the actor id audit.list filters by."
      );
    }
    const bound = badBound();
    if (bound) {
      return `The ${bound} bound could not be read as a time, so it could not be sent.`;
    }
    return null;
  });

  /**
   * The read's parameters, or null when it must not fire yet.
   *
   * A filter that has not resolved must not fall through to an unfiltered
   * query: "every write in the estate" is not a wider version of "every write
   * to WI-7", it is a completely different answer, and it would arrive looking
   * like the item's history.
   */
  const queryKey = createMemo(() => {
    const current = filter();
    if (refBlocked()) return null;
    if (current.actor && !actorsSettled()) return null;
    if (unpushable()) return null;
    return {
      reload: reloadKey(),
      actor: actorId() ?? "",
      operation: current.operation,
      entity: entityId() ?? "",
      project: current.project,
      since: since() ?? "",
      until: until() ?? "",
      limit: pageSize(),
      offset: page() * pageSize(),
    };
  });

  const [records] = createResource(queryKey, (key) =>
    attempt(() =>
      listAudit({
        limit: key.limit,
        offset: key.offset || undefined,
        actor_id: key.actor || undefined,
        operation: key.operation || undefined,
        entity_id: key.entity || undefined,
        project: key.project || undefined,
        since: key.since || undefined,
        until: key.until || undefined,
      }),
    ),
  );

  const answer = createMemo(() => {
    const result = records();
    return result && result.ok ? result.value : null;
  });

  const loaded = createMemo<AuditRecord[]>(() => answer()?.records ?? []);

  /**
   * How many records match the narrowing, ignoring limit and offset.
   *
   * `null` when the server did not say — an older core than this build, and
   * the difference between "none match" and "how many match is not known" is
   * exactly the kind of thing this surface must not smooth over.
   */
  const total = createMemo<number | null>(() => {
    const value = answer()?.total;
    return typeof value === "number" ? value : null;
  });

  const outage = createMemo(() => {
    const result = records();
    return result && !result.ok ? result : null;
  });

  createEffect(() => {
    const failure = outage();
    if (failure) props.onError?.(`Audit log: ${failure.message}`);
  });

  // -- paging the log, not a window over it ---------------------------------
  //
  // `total` is the number of records matching the narrowing, so the page count
  // is arithmetic on the store rather than on what happens to be in hand. A
  // server that did not send one leaves the count unknown, and the pager says
  // "there is another page" only when this one came back full — a guess it is
  // honest about rather than a number it invented.

  const pageCount = createMemo<number | null>(() => {
    const matching = total();
    if (matching === null) return null;
    return Math.max(1, Math.ceil(matching / pageSize()));
  });

  const hasOlder = createMemo(() => {
    const count = pageCount();
    if (count === null) return loaded().length >= pageSize();
    return page() < count - 1;
  });

  // A page beyond the end of a freshly narrowed result is a blank screen with
  // rows above it; clamping is what a link to page 9 of a filter that now has
  // two pages should do. It costs one extra read — the clamped page is fetched
  // after the empty one — and the alternative is a reader looking at nothing
  // and concluding there is nothing.
  createEffect(() => {
    const count = pageCount();
    if (count === null) return;
    const last = count - 1;
    if (page() > last) setPage(last);
  });

  /** The rows on this page: what the server sent, unfiltered by this client. */
  const pageRows = createMemo(() => loaded());

  /** The first and last record number on this page, counting from one. */
  const shown = createMemo<{ first: number; last: number } | null>(() => {
    const count = loaded().length;
    if (!count) return null;
    const first = page() * pageSize() + 1;
    return { first, last: first + count - 1 };
  });

  /** What this page spans in time. The narrowing's span is `since`/`until`. */
  const span = createMemo<{ oldest: string; newest: string } | null>(() => {
    const rows = loaded();
    if (!rows.length) return null;
    let oldest = Number.POSITIVE_INFINITY;
    let newest = Number.NEGATIVE_INFINITY;
    for (const row of rows) {
      const at = timeOf(row);
      if (at === null) continue;
      oldest = Math.min(oldest, at);
      newest = Math.max(newest, at);
    }
    if (!Number.isFinite(oldest) || !Number.isFinite(newest)) return null;
    return {
      oldest: new Date(oldest).toLocaleString(),
      newest: new Date(newest).toLocaleString(),
    };
  });

  /** Operations offered by the picker: the ones this page happens to contain.
   *  Which is not the set that exists, so the control is a `datalist` and
   *  typing a name it did not offer is a legitimate query. */
  const operationOptions = createMemo(() => {
    const seen = new Set(loaded().map((row) => row.operation));
    if (filter().operation) seen.add(filter().operation);
    return [...seen].sort();
  });

  const facetNote = createMemo(() => {
    const missing: string[] = [];
    const actorResult = actors();
    const projectResult = projects();
    if (actorResult && !actorResult.ok) missing.push("actors");
    if (projectResult && !projectResult.ok) missing.push("projects");
    return missing.length
      ? `Could not load ${missing.join(" and ")} — those pickers offer only what this page names.`
      : null;
  });

  const applyPreset = (range: { from: string; to: string }) => {
    setFilter({ ...filter(), ...range });
    setPage(0);
  };

  const clearFilters = () => {
    setFilter({ ...EMPTY_FILTER, view: filter().view });
    setPage(0);
  };

  const filtered = createMemo(() => {
    const current = filter();
    return Boolean(
      current.actor ||
        current.operation ||
        current.entity ||
        current.ref ||
        current.project ||
        current.from ||
        current.to,
    );
  });

  // -- the inbox (FR-N3, FR-U3) ---------------------------------------------

  const inboxKey = createMemo(() => {
    if (filter().view !== "inbox") return null;
    const current = filter();
    return {
      reload: reloadKey(),
      project: current.project,
      reason: current.nreason,
      unread: current.unread,
      offset: page() * INBOX_PAGE_SIZE,
    };
  });

  const [inbox] = createResource(inboxKey, (key) =>
    attempt(() =>
      readInbox({
        project: key.project || undefined,
        reason: key.reason || undefined,
        unread_only: key.unread || undefined,
        limit: INBOX_PAGE_SIZE,
        offset: key.offset,
      }),
    ),
  );

  const inboxValue = createMemo<InboxResult | null>(() => {
    const result = inbox();
    return result && result.ok ? result.value : null;
  });

  const inboxFailure = createMemo(() => {
    const result = inbox();
    return result && !result.ok ? result : null;
  });

  const coverage = createMemo(() => describeCoverage(inboxValue()?.freshness));

  const inboxReasons = createMemo(() =>
    Object.keys(inboxValue()?.by_reason ?? {}).sort(),
  );

  const inboxPages = createMemo(() =>
    Math.max(1, Math.ceil((inboxValue()?.total ?? 0) / INBOX_PAGE_SIZE)),
  );

  // -- live, and honest about the difference (FR-U10) -----------------------
  //
  // Both views on this surface show state the server announces, and both used
  // to be read exactly once per filter key: the audit log is the record of
  // the very writes `vogt-changed` is republishing, and the inbox read its
  // unread count on the way in and never again — so a count that said 3 on
  // Monday still said 3 on Friday, with nothing on screen admitting it was
  // Monday's answer.
  //
  // The nudge bumps the reload key, which is the same thing the Refresh
  // button does, so there is one path to a re-read and the filter, the page
  // and the page size survive it.
  //
  // What arrives when: a notification is collected during a sweep, and a
  // sweep publishes `sweep.completed` onto the core's event feed, which the
  // front door republishes here. So the count moves when it can move —
  // GitHub's own reads are not observed until somebody sweeps — and the badge
  // covers the rest by saying how old this answer is.
  onVogtLive(() => refresh());

  const auditLoadedAt = createLoadStamp(records, (result) => result.ok);
  const inboxLoadedAt = createLoadStamp(inbox, (result) => result.ok);

  const viewAge = createViewAge(() => {
    const inboxView = filter().view === "inbox";
    const failure = inboxView ? inboxFailure() : outage();
    return {
      loadedAt: inboxView ? inboxLoadedAt() : auditLoadedAt(),
      outage: failure?.unavailable ? failure.message : null,
      failed: Boolean(failure),
      live: true,
    };
  });

  return (
    <div class="vogt-surface vab">
      {/* The shared working header (FR-U23, Stage 3): this route had no title
          at all, which on a phone left a reader inside a filter form with no
          statement of where they were. */}
      <SurfaceHeader
        class="vab-header"
        label="Audit header"
        title={<h1>{filter().view === "audit" ? "Audit" : "Notifications"}</h1>}
        honestyClass={honestyToneClass(viewAge().tone)}
        honesty={(
          <strong><ViewAgeBadge
            age={viewAge()}
            class="vab-age"
            title="How long ago this view last got an answer from Vogt. It re-reads when Vogt announces a change; a badge that goes stale means the stream is not arriving."
          /></strong>
        )}
        controls={(
          <>
            <div class="vab-views surface-header-tabs" role="group" aria-label="Global views">
              <For each={["audit", "inbox"] as ViewName[]}>
                {(name) => (
                  <button
                    type="button"
                    aria-pressed={filter().view === name}
                    class={`vab-viewtab${filter().view === name ? " active" : ""}`}
                    onClick={() => update("view", name)}
                  >
                    {name === "audit" ? "Audit trail" : "Notifications"}
                  </button>
                )}
              </For>
            </div>
            <button
              type="button"
              onClick={refresh}
              disabled={filter().view === "audit" ? records.loading : inbox.loading}
            >
              {(filter().view === "audit" ? records.loading : inbox.loading)
                ? "Loading…"
                : "Refresh"}
            </button>
          </>
        )}
        detail={(
          <Show when={filter().view === "audit"}>
            <details class="surface-header-disclosure">
              <summary>What this view is asking Vogt</summary>
              <p class="vab-provenance">
                <strong>Every filter here is pushed to Vogt.</strong>{" "}
                <span class="vab-mono">audit.list</span> takes the actor, the
                operation, the entity, the project and a time range, and pages with
                a limit and an offset — so what is below is one query's answer over
                the whole log, and the count beside it is how many records match,
                not how many are on screen.
              </p>
            </details>
          </Show>
        )}
      />

      {/* -- the audit browser (FR-U19) -------------------------------------- */}
      <Show when={filter().view === "audit"}>
        <section class="vab-filters" aria-label="Audit filters">
          <div class="vab-filter-grid">
            <label class="vab-field">
              <span>Actor</span>
              <select
                value={optionValue(filter().actor, actorOptions())}
                onInput={(event) => update("actor", event.currentTarget.value)}
              >
                <option value="">Anyone</option>
                <For each={actorOptions()}>
                  {(ref) => (
                    <option value={ref}>
                      {actorByRef().get(ref)?.display_name
                        ? `${actorByRef().get(ref)?.display_name} (${ref})`
                        : ref}
                    </option>
                  )}
                </For>
              </select>
            </label>

            <label class="vab-field">
              <span>Operation</span>
              <input
                type="text"
                list="vab-operations"
                placeholder="any operation"
                value={filter().operation}
                onChange={(event) => update("operation", event.currentTarget.value.trim())}
              />
              {/* A list rather than a select: `audit.list` matches the operation
                  exactly, and the names this page happens to contain are not
                  the set of operations that exist. Typing one that is not
                  offered is a legitimate query.

                  Committed on change rather than on input, here and on the two
                  fields below: each one is a parameter the server filters by,
                  and a query per keystroke against the largest table in the
                  product is the kind of thing NFR-S5 is about. */}
              <datalist id="vab-operations">
                <For each={operationOptions()}>
                  {(name) => <option value={name} />}
                </For>
              </datalist>
            </label>

            <label class="vab-field">
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

            <label class="vab-field">
              <span>Work item</span>
              <input
                type="text"
                placeholder="WI-7"
                value={filter().ref}
                onChange={(event) => update("ref", event.currentTarget.value.trim())}
              />
            </label>

            <label class="vab-field">
              <span>Entity id</span>
              <input
                type="text"
                placeholder="any entity"
                value={filter().entity}
                onChange={(event) => update("entity", event.currentTarget.value.trim())}
              />
            </label>

            <label class="vab-field">
              <span>From (included)</span>
              <input
                type="datetime-local"
                value={filter().from}
                onInput={(event) => update("from", event.currentTarget.value)}
              />
            </label>

            <label class="vab-field">
              <span>To (excluded)</span>
              <input
                type="datetime-local"
                value={filter().to}
                onInput={(event) => update("to", event.currentTarget.value)}
              />
            </label>

            <label class="vab-field">
              <span>Records a page</span>
              <select
                value={String(pageSize())}
                onInput={(event) =>
                  resize(Number.parseInt(event.currentTarget.value, 10))
                }
              >
                <For each={PAGE_SIZES}>
                  {(size) => <option value={String(size)}>{size} a page</option>}
                </For>
              </select>
            </label>
          </div>

          <div class="vab-chiprow">
            <span class="vab-chiplabel">Range</span>
            <For each={RANGE_PRESETS}>
              {(preset) => (
                <button
                  type="button"
                  class="vab-chip"
                  onClick={() => applyPreset(preset.range())}
                >
                  {preset.label}
                </button>
              )}
            </For>
            <button
              type="button"
              class="vab-chip"
              disabled={!filter().from && !filter().to}
              onClick={() => setFilter({ ...filter(), from: "", to: "" })}
            >
              Any time
            </button>
            <span class="vab-muted">
              read in this browser's time zone and sent as instants — the start is
              included and the end is not, so Yesterday and Today tile the log
              instead of both claiming midnight
            </span>
          </div>

          <div class="vab-chiprow">
            <button type="button" onClick={clearFilters} disabled={!filtered()}>
              Clear filters
            </button>
            <Show when={filter().ref}>
              {(ref) => (
                <a class="vab-link" href={`#/w/${encodeURIComponent(ref())}`}>
                  Back to {ref()}
                </a>
              )}
            </Show>
            <span class="vab-muted">
              this filter set is the URL — the link is the view
            </span>
          </div>

          <Show when={facetNote()}>
            <p class="vab-note">{facetNote()}</p>
          </Show>

          <Show when={emptyRange()}>
            <p class="vab-note">
              This range ends where it starts or before it, so it contains no
              instant at all — the end is excluded. Nothing below is a claim that
              nothing happened.
            </p>
          </Show>

          <Show when={filter().ref && filter().entity}>
            <p class="vab-note">
              Both a work item and an entity id are set; the entity id is what was
              queried, because it is what the log is keyed by.
            </p>
          </Show>

          <Show when={entityId()}>
            <p class="vab-note">
              A per-item query returns every write recorded against that entity,{" "}
              <em>including what was said about it</em>: a comment is audited
              against the comment it created, and the query follows that link, so
              the trail of {filter().ref || "this entity"} is its creation, its
              updates, its transitions and its conversation.
            </p>
          </Show>
        </section>

        <Show when={refFailure()}>
          {(message) => (
            <div class="vab-outage" role="alert">
              <h3>That work item could not be resolved</h3>
              <p>{message()}</p>
              <p class="vab-muted">
                No records were read, so none are shown. An audit log with nothing in
                it is a claim, and this is not one.
              </p>
              <button type="button" onClick={refresh}>
                Try again
              </button>
            </div>
          )}
        </Show>

        <Show when={unpushable()}>
          {(message) => (
            <div class="vab-outage" role="alert">
              <h3>That filter could not be pushed to Vogt</h3>
              <p>{message()}</p>
              <p class="vab-muted">
                Nothing was read, because the only query left to make is a wider
                one — and a wider answer under this filter's heading would be the
                surface showing writes the reader did not ask about and calling
                them the ones they did.
              </p>
              <button type="button" onClick={refresh}>
                Try again
              </button>
            </div>
          )}
        </Show>

        <Show when={outage()}>
          {(failure) => (
            <div class="vab-outage" role="alert">
              <h3>
                {failure().unavailable
                  ? "Vogt is not answering"
                  : "The audit log could not be read"}
              </h3>
              <p>{failure().message}</p>
              <p class="vab-muted">
                No records are listed because none were read. An empty audit log
                would say nothing has ever been written here, which is not what
                happened.
              </p>
              <button type="button" onClick={refresh}>
                Try again
              </button>
            </div>
          )}
        </Show>

        <Show when={!outage() && !refFailure() && !unpushable()}>
          {/* Withheld while a lookup a filter needs is still in flight: "0 of 0"
              before the query has been made is a number that means nothing and
              reads as an answer. */}
          <Show when={!resolving()}>
            <div class="vab-count">
              <span>
                <Show
                  when={shown()}
                  fallback={
                    <>no records on this page{total() === null ? "" : ` of ${total()} matching`}</>
                  }
                >
                  {(range) => (
                    <>
                      records {range().first}–{range().last}
                      <Show when={total() !== null} fallback=" (how many match in all, Vogt did not say)">
                        {" "}
                        of {total()} matching
                      </Show>
                    </>
                  )}
                </Show>
                <Show when={span()}>
                  {(range) => (
                    <>
                      {" "}
                      · this page spans {range().oldest} → {range().newest}
                    </>
                  )}
                </Show>
              </span>
              <span class="vab-muted">
                <Show
                  when={total() !== null}
                  fallback="this build of Vogt did not report a total, so how much of the log this is cannot be said"
                >
                  <Show
                    when={hasOlder() || page() > 0}
                    fallback="every record matching these filters is on this page"
                  >
                    counted over the whole log, not over what is on screen — page
                    through to reach the rest
                  </Show>
                </Show>
              </span>
            </div>
          </Show>

          <div class="vab-list">
            <Show
              when={pageRows().length}
              fallback={
                <p class="vab-empty">
                  {resolving()
                    ? "Resolving what this filter names before asking for the log…"
                    : records.loading
                      ? "Reading the audit log…"
                      : page() > 0
                        ? // Reachable only while the total is unknown; with one
                          // the page is clamped before it can be empty. Saying
                          // "nothing matches" here would be the surface
                          // answering for the whole query from one empty page.
                          "This page is past the end of what matches — go back to reach the records that do."
                        : filtered()
                          ? "Vogt answered, and no audited write matches this query."
                          : "Vogt answered with no audit records at all."}
                </p>
              }
            >
              <ul class="vab-records">
                <For each={pageRows()}>
                  {(row) => {
                    const actor = createMemo(() => actorByRef().get(row.actor_identity_ref));
                    const at = createMemo(() => timeOf(row));
                    return (
                      <li class="vab-record">
                        <div class="vab-record-head">
                          <span class="vab-when" title={row.at}>
                            <Show when={at() !== null} fallback={<>time unreadable</>}>
                              {formatWhen(row.at)}
                            </Show>
                          </span>
                          {/* who */}
                          <button
                            type="button"
                            class="vab-who"
                            title={`Only writes by ${row.actor_identity_ref}`}
                            onClick={() => update("actor", row.actor_identity_ref)}
                          >
                            {actor()?.display_name ?? row.actor_identity_ref}
                            <Show when={actor()?.display_name}>
                              <span class="vab-mono vab-muted">
                                {" "}
                                {row.actor_identity_ref}
                              </span>
                            </Show>
                          </button>
                          {/* what */}
                          <button
                            type="button"
                            class="vab-op vab-mono"
                            title={`Only ${row.operation}`}
                            onClick={() => update("operation", row.operation)}
                          >
                            {row.operation}
                          </button>
                          <button
                            type="button"
                            class="vab-entity"
                            title={`Only writes to ${row.entity_kind} ${row.entity_id}`}
                            onClick={() =>
                              setFilter({
                                ...filter(),
                                entity: row.entity_id,
                                ref: "",
                              })
                            }
                          >
                            {row.entity_kind}
                            <span class="vab-mono vab-muted"> {row.entity_id}</span>
                          </button>
                        </div>
                        {/* why: the point of the whole mechanism */}
                        <Show
                          when={row.reason.trim()}
                          fallback={
                            <p class="vab-absent">
                              No reason is recorded on this row. Vogt refuses a write
                              without one, so this record predates that rule or was
                              written around it — it is not a write that had no reason.
                            </p>
                          }
                        >
                          <p class="vab-reason">“{row.reason}”</p>
                        </Show>
                        <div class="vab-record-meta">
                          <span class="vab-mono">rev {row.revision}</span>
                          <span class="vab-mono" title="transaction">
                            txn {row.txn_id}
                          </span>
                          <span class="vab-mono" title="payload digest">
                            {row.payload_digest}
                          </span>
                        </div>
                      </li>
                    );
                  }}
                </For>
              </ul>
            </Show>
          </div>

          <Show when={!resolving()}>
            <div class="vab-pager">
              <button
                type="button"
                disabled={page() <= 0}
                onClick={() => setPage(Math.max(0, page() - 1))}
              >
                Newer
              </button>
              <span class="vab-muted">
                page {page() + 1}
                <Show when={pageCount() !== null}> of {pageCount()}</Show> ·{" "}
                {pageSize()} records a page, at offset {page() * pageSize()}
              </span>
              <button
                type="button"
                disabled={!hasOlder()}
                onClick={() => setPage(page() + 1)}
              >
                Older
              </button>
            </div>
          </Show>
        </Show>
      </Show>

      {/* -- the notification inbox (FR-N3, FR-U3) --------------------------- */}
      <Show when={filter().view === "inbox"}>
        <p class="vab-scope">
          {inboxValue()?.scope ??
            "these belong to the GitHub account whose token this instance is " +
              "configured with; notifications are instance-scoped, not per-actor"}
          . Nothing here is marked read upstream, and this is not the{" "}
          <span class="vab-mono">/events</span> feed — that is this instance's own
          history and is a different surface.
        </p>

        <p class={`vab-freshness ${coverage().status}`}>
          {coverage().text}
          <Show when={coverage().collectors.length}>
            <span class="vab-collectors">
              <For each={coverage().collectors}>
                {([name, age]) => (
                  <span class="vab-collector">
                    {name}: {age}
                  </span>
                )}
              </For>
            </span>
          </Show>
        </p>

        <section class="vab-filters" aria-label="Notification filters">
          <div class="vab-filter-grid">
            <label class="vab-field">
              <span>Project</span>
              <select
                value={optionValue(filter().project, projectOptions())}
                onInput={(event) => update("project", event.currentTarget.value)}
              >
                <option value="">Every registered project</option>
                <For each={projectOptions()}>
                  {(slug) => <option value={slug}>{slug}</option>}
                </For>
              </select>
            </label>

            <label class="vab-field">
              <span>Reason</span>
              <input
                type="text"
                list="vab-nreasons"
                placeholder="mention, review_requested…"
                value={filter().nreason}
                onChange={(event) => update("nreason", event.currentTarget.value.trim())}
              />
              <datalist id="vab-nreasons">
                <For each={inboxReasons()}>{(name) => <option value={name} />}</For>
              </datalist>
            </label>

            <label class="vab-field vab-field-check">
              <input
                type="checkbox"
                checked={filter().unread}
                onChange={(event) => update("unread", event.currentTarget.checked)}
              />
              <span>Unread only</span>
            </label>
          </div>
          <p class="vab-muted">
            All three are pushed to Vogt — <span class="vab-mono">notifications</span>{" "}
            takes a project, a reason, an unread flag, and pages with a limit and an
            offset.
          </p>
        </section>

        <Show when={inboxFailure()}>
          {(failure) => (
            <div class="vab-outage" role="alert">
              <h3>
                {failure().unavailable
                  ? "Vogt is not answering"
                  : "This client cannot ask for the inbox"}
              </h3>
              <p>{failure().message}</p>
              <p class="vab-muted">
                Nothing is listed because nothing was read. An empty inbox would mean
                GitHub had nothing to say, and that is not what happened here.
              </p>
              <button type="button" onClick={refresh}>
                Try again
              </button>
            </div>
          )}
        </Show>

        <Show when={inboxValue()}>
          {(result) => (
            <>
              <div class="vab-count">
                <span>
                  {result().total} collected · {result().unread ?? 0} unread
                </span>
                <Show when={result().detail}>
                  {(detail) => <span class="vab-muted">{detail()}</span>}
                </Show>
              </div>

              <div class="vab-list">
                <Show
                  when={result().notifications.length}
                  fallback={
                    <p class="vab-empty">
                      {result().detail ??
                        "Nothing matches these filters in what the collector last saw."}
                    </p>
                  }
                >
                  <ul class="vab-records">
                    <For each={result().notifications}>
                      {(entry) => (
                        <li class={`vab-record${entry.unread ? " unread" : ""}`}>
                          <div class="vab-record-head">
                            <span class="vab-when">
                              {formatWhen(entry.updated_at ?? entry.observed_at)}
                            </span>
                            <span class="vab-op">{entry.reason ?? "no reason given"}</span>
                            <span class="vab-entity">
                              {entry.project_slug ?? entry.repo ?? "unlinked repository"}
                              <Show when={entry.subject_type}>
                                {(kind) => (
                                  <span class="vab-mono vab-muted"> {kind()}</span>
                                )}
                              </Show>
                            </span>
                            <Show when={entry.unread}>
                              <span class="vab-unread">unread</span>
                            </Show>
                          </div>
                          <p class="vab-reason">
                            <Show
                              when={entry.url}
                              fallback={<>{entry.title || "untitled thread"}</>}
                            >
                              {(url) => (
                                <a href={url()} target="_blank" rel="noreferrer">
                                  {entry.title || "untitled thread"}
                                </a>
                              )}
                            </Show>
                          </p>
                          <div class="vab-record-meta">
                            <span class="vab-mono">thread {entry.thread}</span>
                            <span>observed {formatWhen(entry.observed_at)}</span>
                          </div>
                        </li>
                      )}
                    </For>
                  </ul>
                </Show>
              </div>

              <div class="vab-pager">
                <button
                  type="button"
                  disabled={page() <= 0}
                  onClick={() => setPage(Math.max(0, page() - 1))}
                >
                  Newer
                </button>
                <span class="vab-muted">
                  page {page() + 1} of {inboxPages()} · asked for {INBOX_PAGE_SIZE} at a
                  time
                </span>
                <button
                  type="button"
                  disabled={page() >= inboxPages() - 1}
                  onClick={() => setPage(page() + 1)}
                >
                  Older
                </button>
              </div>
            </>
          )}
        </Show>
      </Show>
    </div>
  );
};

export default AuditBrowser;
