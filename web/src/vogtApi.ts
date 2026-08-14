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
  "events.list": "/events",
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
  /** The oldest sweep any of this answer's evidence came from. */
  oldest_relevant_sweep?: string | null;
  age_seconds?: number | null;
  /** Per-collector coverage: collector name to its state for this answer.
   *  A collector that did not run is *in* this map saying so — which is the
   *  difference between "nothing found" and "not collected". */
  collectors?: Record<string, string>;
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
  /** Set on observed subjects: what kind of thing was seen, and where. */
  observation_kind?: string | null;
  source_url?: string | null;
  observed_at?: string | null;
  /** The work item ref an observed subject was adopted as, if it was. */
  adopted_as?: string | null;
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
  /** How many candidates were ranked to produce `items`, before the cut. */
  total_considered?: number;
  declared?: number;
  observed?: number;
  suppressed?: number;
  scope?: string | null;
  freshness: FreshnessSummary;
}

export interface ScoreContribution {
  input: string;
  detail?: string | null;
  value: number;
  weight: number;
  contribution: number;
}

export interface WhyResult {
  ref: string;
  title: string;
  total: number;
  contributions: ScoreContribution[];
  /** Documented inputs this build cannot compute, each with the note saying
   *  why. Their absence is not a zero, and rendering it as one would be the
   *  explanation lying. */
  inputs_not_yet_available: Record<string, string>;
}

export interface AuditRecord {
  id: string;
  txn_id: string;
  revision: number;
  actor_id: string;
  actor_identity_ref: string;
  operation: string;
  entity_kind: string;
  entity_id: string;
  reason: string;
  payload_digest: string;
  at: string;
}

export interface WorkflowState {
  name: string;
  is_initial?: boolean;
  is_terminal?: boolean;
}

export interface Workflow {
  kind: string;
  initial_state: string;
  states: string[];
  /** Adjacency, not edge pairs: `{open: ["in_progress", "wont_do"], …}`.
   *  Read only as a hint — the server decides what is legal, and a client
   *  that pre-empts it will eventually be wrong about a workflow somebody
   *  edited (FR-U4). */
  transitions?: Record<string, string[]>;
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

export const why = (ref: string) => call<WhyResult>("why", { ref });

export const listProjects = (params: Record<string, unknown> = {}) =>
  call<{ projects: { slug: string; name: string }[] }>("project.list", params);

export const listLabels = () =>
  call<{ labels: { name: string; color?: string }[] }>("label.list");

export const listInitiatives = () =>
  // `id` is not decoration: work items carry `initiative_id`, so a swimlane
  // cannot be labelled from the slug alone.
  call<{ initiatives: { id: string; slug: string; title: string }[] }>(
    "initiative.list",
  );

export const listActors = () =>
  call<{ actors: { identity_ref: string; display_name: string }[] }>(
    "actor.list",
  );

export const listAudit = (params: Record<string, unknown> = {}) =>
  call<{ records: AuditRecord[] }>("audit.list", params);

export const projectBrief = (slug: string) =>
  call<Record<string, unknown> & { freshness: FreshnessSummary }>(
    "project.brief",
    { slug },
  );

export const listEvents = (after: number, limit = 100) =>
  // The cursor feed behind FR-U10. A surface that polls this and reports how
  // old its answer is tells the truth; one that re-lists and calls itself
  // live does not.
  call<{ events: Record<string, unknown>[]; next_cursor: number }>(
    "events.list",
    { after, limit },
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

export const updateWork = (
  params: Record<string, unknown> & { ref: string; reason: string },
) => call<WorkDetail>("work.update", params, "POST");

export const createWork = (
  params: Record<string, unknown> & { reason: string },
) => call<WorkDetail>("work.create", params, "POST");

export const startSession = (
  params: Record<string, unknown> & { reason: string },
) => call<{ session: SessionSummary }>("session.start", params, "POST");

export const stopSession = (id: string, reason: string) =>
  call<{ session: SessionSummary }>("session.stop", { id, reason }, "POST");
