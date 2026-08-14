// The Vogt half of the product, as the PWA sees it (FR-U8, NFR-D11).
//
// Everything here goes through the front door: the engine holds the only
// published port and proxies `/api/vogt/...` to vogt-core, injecting the core
// token paired with this client's front-door token (FR-S9). The browser
// therefore holds one credential, the same one `api.ts` already uses, and
// never sees a core token.
//
// Two rules this module exists to keep, both checkable rather than trusted:
//
//   1. **Every path is an operation.** `ROUTES` names each one after the
//      operation in Vogt's registry that serves it, and a test in the Python
//      suite reads this file and resolves every path against that registry.
//      A view that grows its own endpoint fails the build, not the review —
//      the same rule the legacy GUI has kept since M6.
//   2. **A write says who asked for it.** Every mutating call takes a
//      `reason`, because Vogt refuses one without it; the GUI's job is to
//      have collected a reason the *user* typed (FR-W1, and r6's restatement
//      that a mutating operation appears only through a view that collects
//      one). `call()` cannot invent one — there is no default.

import { ApiError, getBase, getToken } from "./api";

/** Where the front door mounts vogt-core. */
export const VOGT_PREFIX = "/api/vogt";

/** One entry per operation the PWA reads or writes. Names match the registry. */
export const ROUTES = {
  status: "/status",
  "project.list": "/projects",
  "project.get": "/projects/get",
  "project.brief": "/projects/brief",
  "work.list": "/work",
  "work.get": "/work/get",
  "work.create": "/work",
  "work.transition": "/work/transition",
  "work.comment": "/work/comment",
  "work.update": "/work/update",
  backlog: "/backlog",
  bugs: "/bugs",
  why: "/why",
  "workflow.list": "/workflows",
  "label.list": "/labels",
  "initiative.list": "/initiatives",
  "actor.list": "/actors",
  "drift.list": "/drift",
  "drift.resolve": "/drift/resolve",
  deps: "/deps",
  compliance: "/compliance",
  "audit.list": "/audit",
  notifications: "/notifications",
  "session.list": "/sessions",
  "session.start": "/sessions",
  "session.stop": "/sessions/stop",
} as const;

export type VogtOperation = keyof typeof ROUTES;

/** What the front door says when vogt-core is not there (FR-U21). */
export class VogtUnavailable extends Error {
  constructor(
    public readonly status: number,
    message: string,
  ) {
    super(message);
  }
}

function isAbsent(status: number): boolean {
  // 503: this front door has no core configured. 502: it has one and the
  // core did not answer. Both mean "Vogt cannot be asked right now", which
  // every Vogt surface renders as an outage with the server's own reason
  // rather than as empty data (FR-U21).
  return status === 502 || status === 503;
}

async function call<T>(
  operation: VogtOperation,
  params: Record<string, unknown> = {},
  method: "GET" | "POST" = "GET",
): Promise<T> {
  const path = ROUTES[operation];
  if (!path) throw new Error(`no route for ${operation}`);

  const token = getToken();
  const headers: Record<string, string> = { Accept: "application/json" };
  if (token) headers.Authorization = `Bearer ${token}`;

  let url = `${getBase()}${VOGT_PREFIX}${path}`;
  let body: string | undefined;
  if (method === "GET") {
    const query = new URLSearchParams();
    for (const [key, value] of Object.entries(params)) {
      if (value === undefined || value === null || value === "") continue;
      for (const one of Array.isArray(value) ? value : [value]) {
        query.append(key, String(one));
      }
    }
    const qs = query.toString();
    if (qs) url += `?${qs}`;
  } else {
    headers["Content-Type"] = "application/json";
    body = JSON.stringify(params);
  }

  const res = await fetch(url, { method, headers, body });
  const text = await res.text();
  if (!res.ok) {
    let message = text;
    try {
      const parsed = JSON.parse(text);
      message = parsed?.error?.message ?? text;
    } catch {
      // A non-JSON body from a proxy hop; the status still carries meaning.
    }
    if (isAbsent(res.status)) throw new VogtUnavailable(res.status, message);
    throw new ApiError(res.status, message);
  }
  return text ? (JSON.parse(text) as T) : (undefined as T);
}

// -- the shapes the surfaces read ------------------------------------------
//
// Deliberately partial: each interface carries what a view renders, not
// everything the API returns. Widening one is a decision to render more.

export type TrustState = string;

export interface FreshnessSummary {
  status: "fresh" | "partial" | "never_swept" | string;
  swept_at?: string | null;
  detail?: string | null;
}

export interface WorkItem {
  id: string;
  ref: string;
  kind: string;
  title: string;
  body?: string;
  state: string;
  priority: string;
  effort?: string | null;
  project_slug?: string | null;
  initiative_id?: string | null;
  origin?: string;
  trust_state?: TrustState;
  assignee_identity_ref?: string | null;
  labels?: string[];
  relations?: { kind: string; related_id: string }[];
  created_at: string;
  updated_at: string;
}

export interface RankedEntry {
  origin: string;
  ref: string;
  title: string;
  kind: string;
  state: string;
  priority: string;
  project_slug?: string | null;
  trust_state?: TrustState;
  labels?: string[];
  score: number;
  updated_at: string;
  item?: WorkItem | null;
}

export interface RankedView {
  items: RankedEntry[];
  freshness: FreshnessSummary;
}

export interface WorkflowState {
  name: string;
  is_initial?: boolean;
  is_terminal?: boolean;
}

export interface Workflow {
  kind: string;
  initial_state: string;
  states: (WorkflowState | string)[];
  transitions?: { from: string; to: string }[];
}

export interface SessionSummary {
  id: string;
  engine_session_id: string;
  project?: string | null;
  work_item?: string | null;
  actor: string;
  cwd: string;
  template?: string | null;
  reason: string;
  started_at: string;
  stopped_at?: string | null;
  activity?: string | null;
  alive?: boolean | null;
}

export interface WorkDetail {
  item: WorkItem;
  comments: { id: string; body: string; created_at: string }[];
  sessions: SessionSummary[];
}

// -- reads ------------------------------------------------------------------

export const listWorkflows = () =>
  call<{ workflows: Workflow[] }>("workflow.list");

export const listWork = (params: Record<string, unknown> = {}) =>
  call<{ items: WorkItem[]; total?: number }>("work.list", params);

export const getWork = (ref: string) => call<WorkDetail>("work.get", { ref });

export const backlog = (params: Record<string, unknown> = {}) =>
  call<RankedView>("backlog", params);

export const bugs = (params: Record<string, unknown> = {}) =>
  call<RankedView>("bugs", params);

export const why = (ref: string) =>
  call<Record<string, unknown>>("why", { ref });

export const listProjects = (params: Record<string, unknown> = {}) =>
  call<{ projects: { slug: string; name: string }[] }>("project.list", params);

export const listLabels = () =>
  call<{ labels: { name: string; color?: string }[] }>("label.list");

export const listInitiatives = () =>
  call<{ initiatives: { slug: string; title: string }[] }>("initiative.list");

export const listActors = () =>
  call<{ actors: { identity_ref: string; display_name: string }[] }>(
    "actor.list",
  );

export const listSessions = (params: Record<string, unknown> = {}) =>
  call<{ sessions: SessionSummary[]; engine?: string | null }>(
    "session.list",
    params,
  );

// -- writes, each carrying the reason its view collected --------------------

export const transitionWork = (ref: string, to_state: string, reason: string) =>
  call<WorkDetail>("work.transition", { ref, to_state, reason }, "POST");

export const commentWork = (ref: string, body: string, reason: string) =>
  call<Record<string, unknown>>(
    "work.comment",
    { ref, body, reason },
    "POST",
  );

export const createWork = (
  params: Record<string, unknown> & { reason: string },
) => call<WorkDetail>("work.create", params, "POST");

export const startSession = (
  params: Record<string, unknown> & { reason: string },
) => call<{ session: SessionSummary }>("session.start", params, "POST");

export const stopSession = (id: string, reason: string) =>
  call<{ session: SessionSummary }>("session.stop", { id, reason }, "POST");
