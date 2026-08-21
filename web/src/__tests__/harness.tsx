// A Vogt the surfaces can be tested against, and the router they need.
//
// **Where the seam is.** Every Vogt surface reaches the server through
// `vogtApi.ts`, which `tests/test_pwa.py` already proves contains exactly one
// `fetch`. So the fake goes *under* that call rather than replacing the module
// above it: the tests then exercise the real route table, the real query
// encoding, and the real mapping of 502/503 onto `VogtUnavailable`, and an
// assertion that "the board asked `workflow.list`" is an assertion about the
// URL that would be sent to a real Vogt.
//
// Mocking the module instead would have made every one of those a thing the
// test asserted about itself.
//
// **Why `src/__tests__/` and not `src/`.** `tests/test_pwa.py` globs
// `web/src/*.ts` and `web/src/*.tsx` and reads every match as a source of URL
// literals and API calls. A test file sitting beside the surfaces would be
// read as one — and a fixture containing `/api/vogt/work` would be checked
// against the operation registry as though a view had asked for it. One
// directory down, the glob does not see them; `tsconfig.json` still does, so
// `pnpm typecheck` covers them.

import type { JSX } from "solid-js";
import { MemoryRouter, Route, createMemoryHistory } from "@solidjs/router";
import { render } from "@solidjs/testing-library";
import { vi } from "vitest";
import { startEventStream, stopEventStream } from "../store";

/** The front door's mount, as `vogtApi.ts` names it. */
const VOGT_PREFIX = "/api/vogt";

export interface RecordedCall {
  method: string;
  /** The operation path with the front-door prefix stripped: `/work/transition`. */
  path: string;
  query: URLSearchParams;
  /** The parsed JSON body of a POST, or `null` for a GET. */
  body: Record<string, unknown> | null;
}

export interface Reply {
  status?: number;
  /** Serialised as JSON. Ignored when `text` is given. */
  body?: unknown;
  /** A raw body, for the proxy-hop case where the answer is not JSON. */
  text?: string;
}

/** A handler may return a promise, so a test can hold the answer open and
 *  look at what the surface drew in the meantime — which is the only way to
 *  see an optimistic render at all. */
export type Handler = Reply | ((call: RecordedCall) => Reply | Promise<Reply>);

/** An answer the test decides when to give. */
export function held(): {
  handler: Handler;
  answer(reply: Reply): void;
  asked: Promise<RecordedCall>;
} {
  let release: (reply: Reply) => void = () => {};
  let arrived: (call: RecordedCall) => void = () => {};
  const asked = new Promise<RecordedCall>((resolve) => {
    arrived = resolve;
  });
  const pending = new Promise<Reply>((resolve) => {
    release = resolve;
  });
  return {
    handler: (call) => {
      arrived(call);
      return pending;
    },
    answer: (reply) => release(reply),
    asked,
  };
}

/** `"GET /work"`, `"POST /work/transition"` — the key a handler is filed under. */
export type Routes = Record<string, Handler>;

function boardFromWork(handler: Handler): Handler {
  return async (call) => {
    const source =
      typeof handler === "function"
        ? await handler({ ...call, method: "GET", path: "/work", body: null })
        : handler;
    if ((source.status ?? 200) >= 400 || source.text !== undefined) return source;
    const work = (source.body ?? {}) as {
      items?: Record<string, unknown>[];
      total?: number;
      backlog_candidates?: number;
      declared_total?: number;
      link_state?: string | null;
      excluded_unlinked?: number;
    };
    const body = call.body ?? {};
    const laneMode = String(body.lane_mode ?? "none");
    const requested = Array.isArray(body.cells)
      ? (body.cells as { lane_key?: string; state?: string; cursor?: string }[])
      : [];
    const items = work.items ?? [];
    // Legacy fixtures describe an already-loaded Board. Preserve that unit
    // test meaning; bounded paging tests install `/board/list` directly.
    const pageSize = items.length;
    const laneOf = (item: Record<string, unknown>) =>
      laneMode === "project"
        ? String(item.project_slug ?? "")
        : laneMode === "initiative"
          ? String(item.initiative_id ?? "")
          : "";
    const columnTotals: Record<string, number> = {};
    const laneTotals: Record<string, number> = {};
    for (const item of items) {
      const state = String(item.state ?? "");
      const lane = laneOf(item);
      columnTotals[state] = (columnTotals[state] ?? 0) + 1;
      laneTotals[lane] = (laneTotals[lane] ?? 0) + 1;
    }
    return {
      body: {
        cells: requested.map((cell) => {
          const matching = items.filter(
            (item) =>
              laneOf(item) === String(cell.lane_key ?? "") &&
              String(item.state ?? "") === String(cell.state ?? ""),
          );
          const offset = Number(/^test:(\d+)$/.exec(cell.cursor ?? "")?.[1] ?? 0);
          const page = matching.slice(offset, offset + pageSize);
          return {
            lane_key: String(cell.lane_key ?? ""),
            state: String(cell.state ?? ""),
            items: page,
            total: matching.length,
            next_cursor:
              offset + page.length < matching.length
                ? `test:${offset + page.length}`
                : null,
          };
        }),
        column_totals: columnTotals,
        lane_totals: laneTotals,
        total: work.total ?? items.length,
        // The #187 honesty pair. A legacy `/work` fixture describes declared
        // work only, so both default to `total` and the Board's candidate
        // banner stays quiet; a test that wants the banner installs
        // `/board/list` directly with a larger `backlog_candidates`.
        backlog_candidates: work.backlog_candidates ?? work.total ?? items.length,
        declared_total: work.declared_total ?? work.total ?? items.length,
        // The #183 link-or-publish marker rides through unchanged, so a
        // fixture can describe an unlinked project scope.
        link_state: work.link_state ?? null,
        excluded_unlinked: work.excluded_unlinked ?? 0,
        snapshot: String(body.snapshot ?? "test-board-snapshot"),
        snapshot_at: "2026-08-17T10:01:00Z",
        revision: 1,
      },
    };
  };
}

/** Vogt's own refusal shape: the message a surface must render verbatim. */
export function refusal(status: number, message: string): Reply {
  return { status, body: { error: { code: "test.refused", message } } };
}

/** What the front door says when vogt-core is not there (FR-U21). */
export function unavailable(message: string): Reply {
  return refusal(503, message);
}

export interface FakeVogt {
  /** Every call the surfaces made, in order, oldest first. */
  calls: RecordedCall[];
  /** Calls to a Vogt path with no handler and no default. */
  unhandled: RecordedCall[];
  /** Replace or add a handler mid-test — a refusal that only the second call gets. */
  route(key: string, handler: Handler): void;
  /** The calls matching `"POST /work/transition"`, in order. */
  matching(key: string): RecordedCall[];
  /** The engine's SSE stream, which FR-U10 is about. */
  stream: FakeStream;
  /** Calls to the *engine's* own API, keyed by full pathname.
   *
   *  Separate from `calls` because they are a different server: this harness
   *  is about Vogt, and the engine paths are answered only for the surfaces
   *  that legitimately read both — a work item showing what a session is
   *  waiting for reads Vogt for the session record and the engine for what
   *  that session has on screen. */
  engineCalls: RecordedCall[];
}

export const INBOX_ENTRY = {
  entry_key: "drift:proposal-1:material-v1",
  source: "drift",
  kind: "state_mismatch",
  occurred_at: "2026-08-17T10:00:00Z",
  observed_at: "2026-08-17T10:01:00Z",
  title: "Work item state differs from observed state",
  summary: "Observed state is done; declared state is in_progress.",
  project_slug: "vogt",
  work_item_ref: "WI-7",
  source_subject_key: "proposal-1",
  trust_state: "disputed",
  freshness: "current",
  triage_state: "active",
  action: { kind: "drift", drift_id: "proposal-1" },
  evidence_snapshot: { observed_state: "done", observed_at: "2026-08-17T10:00:00Z" },
  proposed_change: { state: "done" },
};

export const INBOX_RESULT = {
  entries: [INBOX_ENTRY],
  snapshot_at: "2026-08-17T10:01:00Z",
  high_water: { github: null, drift: "2026-08-17T10:00:00Z", ci: null, agent: null },
  coverage: {
    github: { status: "unswept", count: 0, detail: "not collected" },
    drift: { status: "current", count: 1 },
    ci: { status: "unswept", count: 0, detail: "not collected" },
    agent: { status: "unconfigured", count: 0, detail: "no engine" },
  },
  counts: { active: 1, archived: 0, snoozed: 0 },
  instance_scope: "registered projects only",
  engine_status: "not_configured",
  engine_available: false,
};

// -- the event stream (FR-U10) ----------------------------------------------
//
// The engine's `/api/events` is not a Vogt path, so it is not in the route
// table above — it is the *other* half of this product, and the half FR-U10
// names. `store.ts` reads it with `fetch` and a `ReadableStream` rather than
// with `EventSource` (an `EventSource` cannot carry the bearer token), which
// is lucky: jsdom has no `EventSource` and this harness already owns `fetch`.
//
// So the stub answers `/api/events` with a stream the test holds the writing
// end of. That makes a live-update test an assertion about the *join*: a
// frame in the engine's wire format goes in, `subscribeEvents` parses it,
// `store.ts` routes `vogt-changed` to its listeners, and the surface re-reads
// through the real route table. Nothing in the middle is mocked, which is the
// only reason the test is worth writing — the two ends were already asserted
// separately and the join between them was not.

export interface FakeStream {
  /** How many times a client has opened the stream, reconnects included. */
  opens(): number;
  /** Resolves once a client has it open. */
  opened(): Promise<void>;
  /** Push one frame, in the engine's own wire format. */
  push(event: Record<string, unknown>): void;
  /** Announce that vogt-core changed, as the front door republishes it. */
  changed(over?: Record<string, unknown>): void;
  /** Drop the stream the way a dead socket does: no error, just an end. */
  drop(): void;
}

function fakeStream(): { stream: FakeStream; answer: () => Response } {
  let controller: ReadableStreamDefaultController<Uint8Array> | null = null;
  let opens = 0;
  let announce: () => void = () => {};
  let isOpen = new Promise<void>((resolve) => {
    announce = resolve;
  });
  const encoder = new TextEncoder();
  let seq = 0;

  const answer = () => {
    opens += 1;
    const body = new ReadableStream<Uint8Array>({
      start(open) {
        controller = open;
      },
      cancel() {
        controller = null;
      },
    });
    announce();
    return new Response(body, {
      status: 200,
      headers: { "Content-Type": "text/event-stream" },
    });
  };

  const stream: FakeStream = {
    opens: () => opens,
    opened: () => isOpen,
    push(event) {
      if (!controller) throw new Error("nothing has the event stream open");
      controller.enqueue(encoder.encode(`data: ${JSON.stringify(event)}\n\n`));
    },
    changed(over = {}) {
      seq += 1;
      stream.push({
        type: "vogt-changed",
        kind: "work.transitioned",
        entity_kind: "work_item",
        entity_id: "01JWORKITEM",
        seq,
        ...over,
      });
    },
    drop() {
      controller?.close();
      controller = null;
      isOpen = new Promise<void>((resolve) => {
        announce = resolve;
      });
    },
  };

  return { stream, answer };
}

// -- the default estate ------------------------------------------------------
//
// Small, and deliberately not empty: an empty answer is the thing several of
// these requirements are about, so it has to be a state a test *chooses*.

export const FEATURE_WORKFLOW = {
  kind: "feature",
  initial_state: "open",
  states: ["open", "in_progress", "done", "wont_do"],
  transitions: {
    open: ["in_progress", "wont_do"],
    in_progress: ["done", "open"],
    done: [],
    wont_do: [],
  },
};

export const BUG_WORKFLOW = {
  kind: "bug",
  initial_state: "open",
  states: ["open", "in_progress", "done"],
  transitions: {
    open: ["in_progress"],
    in_progress: ["done"],
    done: [],
  },
};

export function workItem(over: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    id: "01JWORKITEM",
    ref: "WI-1",
    kind: "feature",
    title: "Teach the board to say what it does not know",
    body: "",
    state: "open",
    priority: "p2",
    effort: null,
    project_slug: "alpha",
    initiative_id: null,
    origin: "declared",
    trust_state: "verified",
    assignee_identity_ref: null,
    labels: [],
    relations: [],
    created_at: "2026-08-01T00:00:00Z",
    updated_at: "2026-08-01T00:00:00Z",
    ...over,
  };
}

/** One row of the audit log, as `audit.list` returns it.
 *
 *  Every field is rendered by `AuditBrowser`, `reason` most of all: Vogt
 *  refuses a write without one so that this row can answer "why did this
 *  change" months later, and a fixture with none tests the absent case. */
export function auditRecord(over: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    id: "01JAUDIT1",
    txn_id: "01JTXN1",
    revision: 12,
    actor_id: "01JACTOR1",
    actor_identity_ref: "local:ana",
    operation: "work.transition",
    entity_kind: "work_item",
    entity_id: "01JWORKITEM",
    reason: "the board said it was started",
    payload_digest: "sha256:abc",
    at: "2026-08-01T00:00:00Z",
    ...over,
  };
}

/** One row of the event feed, as `events.list` returns it.
 *
 *  A `work.transitioned` event by default, because that is the kind the item
 *  page's state history is built from: `summary` carries the `from` the
 *  audit's digest cannot say, and `audit_id` names the row that says why.
 *  Both are what make the two feeds joinable, so a fixture missing either
 *  tests the case where the join does not close. */
export function feedEvent(over: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    seq: 1,
    kind: "work.transitioned",
    entity_kind: "work_item",
    entity_id: "01JWORKITEM",
    actor_id: "01JACTOR1",
    audit_id: "01JAUDIT1",
    summary: { ref: "WI-1", from: "open", to: "in_progress" },
    at: "2026-08-02T00:00:00Z",
    ...over,
  };
}

export function rankedEntry(over: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    origin: "declared",
    ref: "WI-1",
    title: "Teach the board to say what it does not know",
    kind: "feature",
    state: "open",
    priority: "p2",
    project_slug: "alpha",
    trust_state: "verified",
    labels: [],
    score: 4.25,
    updated_at: "2026-08-01T00:00:00Z",
    ...over,
  };
}

/** One row of the observed store, as `observations.list` returns it.
 *
 *  The default is a *finished* session's outcome, so a test that wants the
 *  provisional case has to say so — the same reason the default estate is not
 *  empty. `payload.work_item` is how an observation is tied back to an item:
 *  the operation has no work-item parameter, because an observation is filed
 *  under its own subject key. */
export function observation(
  over: Record<string, unknown> = {},
  payload: Record<string, unknown> = {},
): Record<string, unknown> {
  return {
    id: "01JOBS1",
    sweep_id: "01JSWEEP",
    collector: "session-outcomes",
    kind: "session.outcome",
    project_id: "01JPROJECT",
    subject_key: "session:01JSESSION",
    content_digest: "sha256:abc",
    source_url: null,
    promoted: false,
    observed_at: "2026-08-01T00:00:00Z",
    ...over,
    payload: {
      session: "01JSESSION",
      engine_session_id: "eng-1",
      project: "alpha",
      work_item: "WI-1",
      cwd: "/srv/alpha",
      started_at: "2026-08-01T00:00:00Z",
      state: "finished",
      provisional: false,
      exit_code: 0,
      ...payload,
    },
  };
}

/** One open drift proposal, carrying the evidence FR-U18 makes acting on it
 *  conditional upon — without a snapshot the card refuses to offer a resolve
 *  control at all, so a fixture with none tests a different thing. */
export function driftProposal(
  over: Record<string, unknown> = {},
): Record<string, unknown> {
  return {
    id: "dft_01",
    kind: "version_mismatch",
    subject_kind: "project",
    subject_id: "prj_01",
    project_id: "prj_01",
    project_slug: "alpha",
    summary: "alpha declares 1.2.0; the repository says 1.3.0",
    evidence_observation_id: "obs_01",
    evidence_snapshot: {
      subject_key: "project:alpha",
      collector: "git",
      observed_at: "2026-08-01T00:00:00Z",
      payload: { version: "1.3.0" },
    },
    proposed_change: { from: "1.2.0", to: "1.3.0" },
    status: "open",
    opened_at: "2026-08-01T00:00:00Z",
    ...over,
  };
}

export function freshness(over: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    status: "fresh",
    oldest_relevant_sweep: "2026-08-01T00:00:00Z",
    age_seconds: 42,
    collectors: { git: "ok" },
    detail: null,
    ...over,
  };
}

function defaults(): Routes {
  const defaultItem = workItem();
  return {
    "GET /workflows": { body: { workflows: [FEATURE_WORKFLOW, BUG_WORKFLOW] } },
    "GET /work": { body: { items: [workItem()], total: 1 } },
    "POST /board/list": {
      body: {
        cells: [
          { lane_key: "", state: "open", items: [defaultItem], total: 1 },
          { lane_key: "", state: "in_progress", items: [], total: 0 },
          { lane_key: "", state: "done", items: [], total: 0 },
          { lane_key: "", state: "wont_do", items: [], total: 0 },
        ],
        column_totals: { open: 1, in_progress: 0, done: 0, wont_do: 0 },
        lane_totals: { "": 1 },
        total: 1,
        snapshot: "test-board-snapshot",
        snapshot_at: "2026-08-17T10:01:00Z",
        revision: 1,
      },
    },
    "GET /work/get": {
      body: { item: workItem(), comments: [], sessions: [] },
    },
    "GET /projects": {
      body: { projects: [{ slug: "alpha", name: "Alpha" }, { slug: "beta", name: "Beta" }] },
    },
    "GET /projects/brief": { body: { project: { slug: "alpha" }, freshness: freshness() } },
    "GET /labels": { body: { labels: [{ name: "infra" }, { name: "docs" }] } },
    "GET /initiatives": { body: { initiatives: [] } },
    "GET /actors": { body: { actors: [] } },
    "GET /backlog": { body: { items: [rankedEntry()], freshness: freshness() } },
    "GET /bugs": { body: { items: [], freshness: freshness() } },
    "GET /why": {
      body: { ref: "WI-1", title: "t", total: 4.25, contributions: [], inputs_not_yet_available: {} },
    },
    "GET /drift": { body: { proposals: [], freshness: freshness() } },
    "GET /deps": {
      body: { project: "alpha", references_out: [], referenced_by: [], unresolved: 0, freshness: freshness() },
    },
    "GET /compliance": {
      body: { project: "alpha", status: "compliant", contract_version: "1", failing: [] },
    },
    "GET /audit": { body: { records: [], total: 0 } },
    // Empty by default, like the audit log above it: an item that has never
    // moved is a state the item page has to render honestly, so it is the
    // one a test gets without asking.
    "GET /events": { body: { events: [], next_cursor: 0 } },
    "GET /observations": { body: { observations: [], total: 0 } },
    "GET /sessions": { body: { sessions: [], engine: null } },
    "GET /notifications": { body: { notifications: [], unread: 0, freshness: freshness() } },
    "GET /status": { body: { ok: true } },
  };
}

/**
 * Install a fake Vogt for the duration of one test.
 *
 * `restoreMocks` in `vitest.config.ts` puts the real `fetch` back afterwards,
 * so there is nothing to undo here.
 */
export function fakeVogt(routes: Routes = {}, engine: Routes = {}): FakeVogt {
  const compatible = { ...routes };
  if (compatible["GET /work"] !== undefined && compatible["POST /board/list"] === undefined) {
    compatible["POST /board/list"] = boardFromWork(compatible["GET /work"]);
  }
  const table: Routes = { ...defaults(), ...compatible };
  const calls: RecordedCall[] = [];
  const unhandled: RecordedCall[] = [];
  const engineCalls: RecordedCall[] = [];
  const { stream, answer: openStream } = fakeStream();

  const stub = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const raw = typeof input === "string" ? input : String(input);
    const url = new URL(raw, "http://vogt.test");
    const method = (init?.method ?? "GET").toUpperCase();
    const body =
      typeof init?.body === "string"
        ? (JSON.parse(init.body) as Record<string, unknown>)
        : null;

    // Answered whether or not a test drives it: a surface only reaches this
    // path when something called `startEventStream`, and the alternative — a
    // 404 — puts `store.ts` into its reconnect backoff for the rest of the
    // file.
    if (url.pathname === "/api/events") return openStream();

    if (!url.pathname.startsWith(VOGT_PREFIX)) {
      // The engine's own API — sessions, files, git. Recorded either way, so
      // a test can assert what a surface asked the engine for and, just as
      // importantly, that it asked nothing. Unstubbed engine paths keep
      // answering 404, which is what makes FR-U21's "the engine is away"
      // cases the default rather than a special arrangement.
      const engineCall: RecordedCall = {
        method,
        path: url.pathname,
        query: url.searchParams,
        body,
      };
      engineCalls.push(engineCall);
      const engineHandler = engine[`${method} ${url.pathname}`];
      if (engineHandler === undefined) {
        return new Response("not a Vogt path", { status: 404 });
      }
      const answer = await (typeof engineHandler === "function"
        ? engineHandler(engineCall)
        : engineHandler);
      return new Response(
        answer.text ?? (answer.body === undefined ? "" : JSON.stringify(answer.body)),
        { status: answer.status ?? 200 },
      );
    }

    const call: RecordedCall = {
      method,
      path: url.pathname.slice(VOGT_PREFIX.length),
      query: url.searchParams,
      body,
    };
    calls.push(call);

    const handler = table[`${method} ${call.path}`];
    if (handler === undefined) {
      unhandled.push(call);
      return new Response(
        JSON.stringify({ error: { message: `no test handler for ${method} ${call.path}` } }),
        { status: 404 },
      );
    }

    const reply = await (typeof handler === "function" ? handler(call) : handler);
    const text = reply.text ?? (reply.body === undefined ? "" : JSON.stringify(reply.body));
    return new Response(text, { status: reply.status ?? 200 });
  });

  vi.stubGlobal("fetch", stub);

  return {
    calls,
    unhandled,
    engineCalls,
    route(key, handler) {
      table[key] = handler;
      if (key === "GET /work") table["POST /board/list"] = boardFromWork(handler);
    },
    matching(key) {
      let [method, path] = key.split(" ");
      // Board tests written before the bounded projection name the read they
      // meant, not its new transport shape. Keep their request-count claims
      // pointed at the Board read while new tests assert `/board/list`
      // directly.
      if (method === "GET" && path === "/work") {
        method = "POST";
        path = "/board/list";
      }
      return calls.filter((call) => call.method === method && call.path === path);
    },
    stream,
  };
}

/**
 * Open the client's event stream, as `App.tsx` does on mount.
 *
 * `store.ts` holds the subscription in module state, so a test that opens it
 * must close it — `stopLiveStream` in an `afterEach` — or the next test
 * inherits a live stream, a reconnect timer, and a set of listeners belonging
 * to surfaces that are no longer mounted.
 */
export async function liveStream(vogt: FakeVogt): Promise<void> {
  startEventStream();
  await vogt.stream.opened();
  await settle();
}

export const stopLiveStream = stopEventStream;

// -- mounting a surface at a URL --------------------------------------------

export interface Mounted {
  /** The current URL, exactly as the router holds it (FR-U11's subject). */
  url(): string;
  /** Navigate as a pasted link would. */
  go(url: string): void;
  container: HTMLElement;
  /**
   * Tear the surface down.
   *
   * Needed by any test that compares two mounts rather than looking at one:
   * two boards mounted at once are two polls, two clocks, and two sets of
   * `document.getElementById` ids for the same refs — so a measurement taken
   * across them would be a measurement of the overlap.
   */
  unmount(): void;
}

/**
 * Render one surface at one path, inside a real router with a real history.
 *
 * The path matters: `Board.tsx` and `Backlog.tsx` both guard their URL effect
 * on `location.pathname`, because navigation can outlive a surface effect and
 * `view`, `project` and `actor` are keys more than one of them owns. A
 * test that mounted the board at `/` would find its filters never reach the
 * URL — and would be right about the code and wrong about the requirement.
 */
export function mountAt(path: string, url: string, surface: () => JSX.Element): Mounted {
  const history = createMemoryHistory();
  history.set({ value: url });
  const rendered = render(() => (
    <MemoryRouter history={history}>
      <Route path={path} component={surface} />
      <Route path="*rest" component={surface} />
    </MemoryRouter>
  ));
  return {
    url: () => history.get(),
    go: (next: string) => history.set({ value: next }),
    container: rendered.container,
    unmount: () => rendered.unmount(),
  };
}

/** The query half of the current URL, parsed. */
export function queryOf(url: string): URLSearchParams {
  const at = url.indexOf("?");
  return new URLSearchParams(at === -1 ? "" : url.slice(at + 1));
}

/** Let every pending microtask and the surfaces' `queueMicrotask` focus settle. */
export async function settle(times = 3): Promise<void> {
  for (let i = 0; i < times; i += 1) {
    await Promise.resolve();
    await new Promise((resolve) => setTimeout(resolve, 0));
  }
}
