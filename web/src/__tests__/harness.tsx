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
  return {
    "GET /workflows": { body: { workflows: [FEATURE_WORKFLOW, BUG_WORKFLOW] } },
    "GET /work": { body: { items: [workItem()], total: 1 } },
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
    "GET /audit": { body: { records: [] } },
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
export function fakeVogt(routes: Routes = {}): FakeVogt {
  const table: Routes = { ...defaults(), ...routes };
  const calls: RecordedCall[] = [];
  const unhandled: RecordedCall[] = [];

  const stub = vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const raw = typeof input === "string" ? input : String(input);
    const url = new URL(raw, "http://vogt.test");
    const method = (init?.method ?? "GET").toUpperCase();
    const body =
      typeof init?.body === "string"
        ? (JSON.parse(init.body) as Record<string, unknown>)
        : null;

    if (!url.pathname.startsWith(VOGT_PREFIX)) {
      // The engine's own API — sessions, files, git. A Vogt surface that
      // reads one (WorkItemDetail asks the engine for a session's activity)
      // gets a 404 rather than a hang: this harness is about Vogt.
      return new Response("not a Vogt path", { status: 404 });
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
    route(key, handler) {
      table[key] = handler;
    },
    matching(key) {
      const [method, path] = key.split(" ");
      return calls.filter((call) => call.method === method && call.path === path);
    },
  };
}

// -- mounting a surface at a URL --------------------------------------------

export interface Mounted {
  /** The current URL, exactly as the router holds it (FR-U11's subject). */
  url(): string;
  /** Navigate as a pasted link would. */
  go(url: string): void;
  container: HTMLElement;
}

/**
 * Render one surface at one path, inside a real router with a real history.
 *
 * The path matters: `Board.tsx` and `Backlog.tsx` both guard their URL effect
 * on `location.pathname`, because every tab in this shell is mounted at once
 * and `view`, `project` and `actor` are keys more than one of them owns. A
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
