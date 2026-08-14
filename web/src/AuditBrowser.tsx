// The audit browser and the notification inbox (FR-U19, FR-N3/FR-U3, NFR-S5).
//
// M11's global surfaces share a tab because they answer the same shape of
// question — "what happened, and who says so" — from the two halves of the
// product: Vogt's own audited writes, and what a forge is trying to say about
// the projects it watches. They are never merged into one list; `/events` and
// the forge inbox have different origins and different owners, and the audit
// log is a third thing again.
//
// Four rules this file exists to keep:
//
//   1. **Every row shows who, what and why.** Vogt refuses a write without a
//      reason precisely so this view can answer "why did this change" months
//      later (FR-S1, FR-W1). A row that showed the operation and dropped the
//      reason would waste the whole mechanism, so the reason is rendered on
//      every row — and a record that somehow carries none says *that*, rather
//      than leaving the space blank.
//   2. **A filter says where it was applied.** `audit.list` takes an actor, an
//      operation and an entity, and nothing else. FR-U19 also asks for project
//      and time range, so those are applied to the records this page loaded —
//      and the surface says so in as many words, the way the backlog says it
//      of its workflow-state filter. A filter that silently means something
//      narrower than it says is worse than one that is missing.
//   3. **A query is a place.** The whole filter set lives in the URL (FR-U11),
//      which is what makes FR-U19's second clause work: a work item's detail
//      view links to `#/audit?ref=WI-7` (or `?entity=<id>`) and lands here
//      with that filter restored and pushed to the server.
//   4. **Absence is stated.** An unreachable Vogt renders as an outage with
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
  listWork,
  type AuditRecord,
  type FreshnessSummary,
} from "./vogtApi";

interface Props {
  onError?: (message: string) => void;
}

// -- constants --------------------------------------------------------------

/**
 * How many records one fetch asks for.
 *
 * `ListAuditParams.limit` is capped at 500 server-side and there is no offset
 * and no cursor, so the window is the only dial there is: `audit.list` always
 * answers with the newest `limit` records. Offering a size the server would
 * clamp would be the picker lying about what it did.
 */
const WINDOW_SIZES = [50, 100, 200, 500] as const;
const DEFAULT_WINDOW = 100;
const MAX_WINDOW = 500;

/**
 * Rows rendered at once (NFR-S5).
 *
 * The audit log is the largest table in the product and grows forever, so the
 * page is what is rendered rather than the window. Deliberately paged instead
 * of virtualized like the backlog: an audit row is variable-height — the
 * reason is a sentence somebody typed and is never truncated to fit a grid —
 * and a fixed row height is what the backlog's windowing arithmetic needs.
 */
const PAGE_SIZE = 50;

/** Notifications the inbox asks for per page. `NotificationsParams` pages properly. */
const INBOX_PAGE_SIZE = 50;

/** How many of a project's work items are resolved to scope the project filter. */
const PROJECT_SCOPE_LIMIT = 500;

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
  "window",
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
  /** A project slug. Server-side in the inbox; page-local in the audit view. */
  project: string;
  /** `datetime-local` text, in this browser's time zone. */
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
// `audit.list` filters by, a project arrives with the id its audit rows carry.
// Both are read through local views that make them optional, so a field the
// server stops sending becomes a filter that says it could not be pushed
// rather than a query with `undefined` in it.

interface ActorRow {
  identity_ref: string;
  display_name: string;
  id?: string;
}

interface ProjectRow {
  slug: string;
  name: string;
  id?: string;
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

// -- time, which the server does not filter by ------------------------------

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

const RANGE_PRESETS: { label: string; seconds: number }[] = [
  { label: "Last hour", seconds: 3600 },
  { label: "Last 24 hours", seconds: 86400 },
  { label: "Last 7 days", seconds: 604800 },
  { label: "Last 30 days", seconds: 2592000 },
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

function windowFromQuery(query: Query): number {
  const parsed = Number.parseInt(one(query.window), 10);
  if (!Number.isFinite(parsed)) return DEFAULT_WINDOW;
  return Math.min(MAX_WINDOW, Math.max(1, parsed));
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
    window: size === DEFAULT_WINDOW ? null : String(size),
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
  const [windowSize, setWindowSize] = createSignal<number>(windowFromQuery(initial));
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
    const desired = encodeState(filter(), windowSize(), page());
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
      setWindowSize(windowFromQuery(query));
      setPage(pageFromQuery(query));
      lastWritten = current;
      return;
    }
    setQuery(queryFor(filter(), windowSize(), page()), { replace: true });
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

  const resize = (size: number) => {
    setWindowSize(size);
    setPage(0);
  };

  // -- the facets -----------------------------------------------------------
  //
  // Loaded once. A facet list that fails is a smaller picker and a named note,
  // not a broken surface — but here it is more than cosmetic: `audit.list`
  // filters by `actor_id`, and the id lives on the actor list. A picker that
  // cannot load is a filter that cannot be pushed, and the surface says so.

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

  /** Waiting on a lookup a filter needs, rather than on the log itself. */
  const resolving = createMemo(
    () =>
      (refBlocked() && !refFailure()) ||
      (Boolean(filter().actor) && !actorsSettled()),
  );

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
    return {
      reload: reloadKey(),
      actor: actorId() ?? "",
      operation: current.operation,
      entity: entityId() ?? "",
      limit: windowSize(),
    };
  });

  const [records] = createResource(queryKey, (key) =>
    attempt(() =>
      listAudit({
        limit: key.limit,
        actor_id: key.actor || undefined,
        operation: key.operation || undefined,
        entity_id: key.entity || undefined,
      }),
    ),
  );

  const loaded = createMemo<AuditRecord[]>(() => {
    const result = records();
    return result && result.ok ? result.value.records : [];
  });

  const outage = createMemo(() => {
    const result = records();
    return result && !result.ok ? result : null;
  });

  createEffect(() => {
    const failure = outage();
    if (failure) props.onError?.(`Audit log: ${failure.message}`);
  });

  // -- the project filter, which the log cannot answer directly -------------
  //
  // An audit record names an entity, not a project: the row for a transition
  // carries the work item's id and nothing about where that item lives. So the
  // filter is resolved from the other side — the project's work items are
  // listed (server-side, one bounded page) and rows are kept when their entity
  // is one of them, or the project row itself. Everything about that is a
  // narrowing, and the note under the filter says which.

  // The key is a string rather than an object so the resource refetches when
  // the slug or the resolved project id changes and not merely because a
  // reactive read produced a new object.
  const [scope] = createResource(
    () => {
      const slug = filter().project;
      if (!slug) return null;
      const known = projectRows().find((row) => row.slug === slug);
      // Newline-separated because a slug cannot contain one; a space could.
      return `${slug}\n${known?.id ?? ""}`;
    },
    (key) =>
      attempt(async () => {
        const split = key.indexOf("\n");
        const slug = key.slice(0, split);
        const projectId = key.slice(split + 1);
        const work = await listWork({
          project: slug,
          limit: PROJECT_SCOPE_LIMIT,
          include_finished: true,
        });
        const ids = new Set(work.items.map((item) => item.id));
        if (projectId) ids.add(projectId);
        return {
          ids,
          resolved: work.items.length,
          total: work.total ?? work.items.length,
          projectKnown: Boolean(projectId),
        };
      }),
  );

  const scopeIds = createMemo<Set<string> | null>(() => {
    const result = scope();
    return result && result.ok ? result.value.ids : null;
  });

  const scopeNote = createMemo<string | null>(() => {
    if (!filter().project) return null;
    const result = scope();
    if (!result) return `Resolving which entities belong to ${filter().project}…`;
    if (!result.ok) {
      return (
        `The project filter could not be resolved: ${result.message} — no rows ` +
        "are hidden by it, because hiding rows on a failed lookup would be a filter " +
        "pretending to have worked."
      );
    }
    const parts = [
      `Audit rows name an entity, not a project. ${result.value.resolved} of ` +
        `${result.value.total} work items in ${filter().project} were resolved` +
        (result.value.projectKnown ? ", plus the project row itself" : "") +
        "; rows whose entity is not one of them are hidden.",
    ];
    if (result.value.total > result.value.resolved) {
      parts.push(
        `Only the first ${PROJECT_SCOPE_LIMIT} work items can be listed in one ` +
          "call, so writes to items beyond that are hidden too.",
      );
    }
    parts.push(
      "Comments, sessions and sweeps carry their own ids and are never matched by " +
        "this filter.",
    );
    return parts.join(" ");
  });

  // -- the filters the server does not take ---------------------------------

  const fromMs = createMemo(() => parseLocalInput(filter().from));
  const toMs = createMemo(() => parseLocalInput(filter().to));

  const visible = createMemo<AuditRecord[]>(() => {
    let rows = loaded();
    const after = fromMs();
    const before = toMs();
    if (after !== null) {
      rows = rows.filter((row) => {
        const at = timeOf(row);
        return at === null || at >= after;
      });
    }
    if (before !== null) {
      rows = rows.filter((row) => {
        const at = timeOf(row);
        return at === null || at <= before;
      });
    }
    // The fallback for an actor the picker could not turn into an id: match the
    // identity the records themselves carry. Narrower than the server's filter —
    // it only sees the loaded window — and the note above the list says so.
    if (actorUnresolved()) {
      const wanted = filter().actor;
      rows = rows.filter((row) => row.actor_identity_ref === wanted);
    }
    const ids = scopeIds();
    if (ids) rows = rows.filter((row) => ids.has(row.entity_id));
    return rows;
  });

  const pageCount = createMemo(() => Math.max(1, Math.ceil(visible().length / PAGE_SIZE)));

  // A page beyond the end of a freshly narrowed result is a blank screen with
  // rows above it; clamping is what a link to page 9 of a filter that now has
  // two pages should do.
  createEffect(() => {
    const last = pageCount() - 1;
    if (page() > last) setPage(last);
  });

  const pageRows = createMemo(() =>
    visible().slice(page() * PAGE_SIZE, page() * PAGE_SIZE + PAGE_SIZE),
  );

  /** The span the loaded window covers, which is what bounds the time filter. */
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

  const windowFull = createMemo(() => loaded().length >= windowSize());

  /** Operations offered by the picker: the ones this window contains. */
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

  const applyPreset = (seconds: number) => {
    setFilter({
      ...filter(),
      from: toLocalInput(new Date(Date.now() - seconds * 1000)),
      to: "",
    });
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

  return (
    <div class="vogt-surface vab">
      <header class="vab-header">
        <div class="vab-views" role="group" aria-label="Global views">
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
        <div class="vab-header-actions">
          <button
            type="button"
            onClick={refresh}
            disabled={filter().view === "audit" ? records.loading : inbox.loading}
          >
            {(filter().view === "audit" ? records.loading : inbox.loading)
              ? "Loading…"
              : "Refresh"}
          </button>
        </div>
      </header>

      {/* -- the audit browser (FR-U19) -------------------------------------- */}
      <Show when={filter().view === "audit"}>
        <p class="vab-provenance">
          <strong>Actor, operation and entity</strong> are pushed to Vogt —{" "}
          <span class="vab-mono">audit.list</span> takes those three and a limit.{" "}
          <strong>Project and time range</strong> are applied to the{" "}
          {loaded().length} record{loaded().length === 1 ? "" : "s"} this page
          loaded, because the operation takes neither; they narrow the window, not
          the log.
        </p>

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
                  exactly, and the names this window happens to contain are not
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
              <span>From</span>
              <input
                type="datetime-local"
                value={filter().from}
                onInput={(event) => update("from", event.currentTarget.value)}
              />
            </label>

            <label class="vab-field">
              <span>To</span>
              <input
                type="datetime-local"
                value={filter().to}
                onInput={(event) => update("to", event.currentTarget.value)}
              />
            </label>

            <label class="vab-field">
              <span>Window</span>
              <select
                value={String(windowSize())}
                onInput={(event) =>
                  resize(Number.parseInt(event.currentTarget.value, 10))
                }
              >
                <For each={WINDOW_SIZES}>
                  {(size) => <option value={String(size)}>newest {size}</option>}
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
                  onClick={() => applyPreset(preset.seconds)}
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
              in this browser's time zone, over the loaded window
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

          <Show when={actorUnresolved()}>
            <p class="vab-note">
              {filter().actor} could not be resolved to the actor id{" "}
              <span class="vab-mono">audit.list</span> filters by, so the filter was
              applied to the loaded window instead of being pushed to Vogt. Writes by
              this actor further back than the window are not shown.
            </p>
          </Show>

          <Show when={scopeNote()}>
            <p class="vab-note">{scopeNote()}</p>
          </Show>

          <Show when={filter().ref && filter().entity}>
            <p class="vab-note">
              Both a work item and an entity id are set; the entity id is what was
              queried, because it is what the log is keyed by.
            </p>
          </Show>

          <Show when={entityId()}>
            <p class="vab-note">
              A per-item query returns the writes recorded{" "}
              <em>against that entity</em>. A comment is audited against the comment
              it created, not against the item, so comments on{" "}
              {filter().ref || "this entity"} are not in this list.
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

        <Show when={!outage() && !refFailure()}>
          {/* The counts describe the loaded window, so they are withheld until
              there is one: "0 matching" while a lookup is still in flight is a
              number that means nothing and reads as an answer. */}
          <Show when={!resolving()}>
            <div class="vab-count">
              <span>
                {visible().length} matching
                <Show when={visible().length !== loaded().length}>
                  {" "}
                  of {loaded().length} loaded
                </Show>
                <Show when={span()}>
                  {(range) => (
                    <>
                      {" "}
                      · window spans {range().oldest} → {range().newest}
                    </>
                  )}
                </Show>
              </span>
              <span class="vab-muted">
                <Show
                  when={windowFull()}
                  fallback="the whole log matching the pushed filters is in this window"
                >
                  {windowSize() >= MAX_WINDOW
                    ? "this is the largest window audit.list serves — it has no offset and no cursor, so reaching further back needs a narrower actor, operation or entity"
                    : "the window is full; older records exist beyond it — raise the window to reach them"}
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
                      : loaded().length
                        ? "No record in this window matches the project or time filter."
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
                page {page() + 1} of {pageCount()} · {PAGE_SIZE} rows a page
              </span>
              <button
                type="button"
                disabled={page() >= pageCount() - 1}
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
