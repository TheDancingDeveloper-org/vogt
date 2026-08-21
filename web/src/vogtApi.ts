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

import { ApiError, getBase, getToken, reportAuthResponse } from "./api";

/** Where the front door mounts vogt-core. */
export const VOGT_PREFIX = "/api/vogt";

/** One entry per operation the PWA reads or writes. Names match the registry. */
export const ROUTES = {
  status: "/status",
  "project.list": "/projects",
  "project.get": "/projects/get",
  "project.brief": "/projects/brief",
  "project.import": "/projects/import",
  "forge.repos": "/forge/repos",
  "work.list": "/work",
  "board.list": "/board/list",
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
  "observations.list": "/observations",
  compliance: "/compliance",
  "audit.list": "/audit",
  notifications: "/notifications",
  "inbox.list": "/inbox",
  "inbox.archive": "/inbox/archive",
  "inbox.snooze": "/inbox/snooze",
  "inbox.restore": "/inbox/restore",
  suppress: "/suppressions",
  "work.adopt": "/work/adopt",
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
  signal?: AbortSignal,
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

  const res = await fetch(url, { method, headers, body, signal });
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
    // A 401 from behind the front door is the same fact as a 401 from the
    // engine's own API — the one credential the browser holds is dead — so it
    // ends the session once for the whole shell rather than becoming one
    // stranded panel per Vogt surface (#195). Everything else, a 403 most of
    // all, stays the caller's to render: the credential is fine, and it is
    // the capability the reader needs told about.
    reportAuthResponse(res.status, message);
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

/**
 * The normalized attention contract planned for the canonical Inbox.
 *
 * Inbox.tsx consumes this one server-owned projection and never falls back to
 * merging notifications, drift, or events in the browser.
 */
export interface InboxEntry {
  entry_key: string;
  source: "github" | "drift" | "ci" | "agent" | string;
  kind: string;
  occurred_at?: string | null;
  observed_at?: string | null;
  title: string;
  summary?: string | null;
  project_slug?: string | null;
  work_item_ref?: string | null;
  session_id?: string | null;
  source_subject_key?: string | null;
  source_url?: string | null;
  trust_state?: TrustState;
  freshness?: "current" | "stale" | "provisional" | "live" | "unknown" | string;
  triage_state?: "active" | "archived" | "snoozed" | string;
  snooze_until?: string | null;
  evidence_snapshot?: Record<string, unknown> | null;
  proposed_change?: Record<string, unknown> | null;
  action?: {
    kind?: "drift" | "observation" | "session" | string;
    drift_id?: string;
    subject_key?: string;
  };
}

export interface InboxSourceCoverage {
  status: "current" | "partial" | "unswept" | "unconfigured" | "failed" | string;
  count: number;
  observed_at?: string | null;
  registered?: number;
  detail?: string | null;
}

export interface InboxListResult {
  entries: InboxEntry[];
  next_cursor?: string | null;
  snapshot_at: string;
  high_water?: Record<string, string | null>;
  coverage: Record<string, InboxSourceCoverage>;
  counts?: Record<string, number>;
  github_scope?: string;
  instance_scope?: string | null;
  engine_available?: boolean;
  engine_status?: "not_configured" | "available" | "unreachable" | string;
  engine_detail?: string | null;
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

export interface BoardCellRequest {
  lane_key: string;
  state: string;
  cursor?: string;
}

export interface BoardCellPage {
  lane_key: string;
  state: string;
  items: WorkItem[];
  total: number;
  next_cursor?: string | null;
}

export interface BoardListResult {
  cells: BoardCellPage[];
  column_totals: Record<string, number>;
  lane_totals: Record<string, number>;
  /** Declared cards the Board draws for this filter, terminal columns included. */
  total: number;
  /**
   * How many things the Backlog would consider for this same scope (#187):
   * declared work plus open forge subjects not yet tracked as work items. The
   * Board draws only the declared cards, so this is the honest denominator the
   * surface uses to say what it is leaving out. Optional so an older server is
   * read as "no observed population known", never as zero candidates.
   */
  backlog_candidates?: number;
  /** The declared-only slice of `backlog_candidates` — the population the rail counts. */
  declared_total?: number;
  /**
   * Set for a project scope (#183): "unlinked" is the machine-readable
   * link-or-publish CTA — the cells are empty because the project has no
   * work surface yet, not because there is nothing to do. Null/absent on
   * the global Board and on older servers.
   */
  link_state?: "linked" | "unlinked" | null;
  /**
   * Declared rows excluded because their project is unlinked (#183's
   * withdrawal); on an unlinked project scope, the open native items a
   * link or publish would migrate.
   */
  excluded_unlinked?: number;
  snapshot: string;
  snapshot_at: string;
  revision: number;
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
  /**
   * Set for a project scope (#183): "unlinked" is the machine-readable
   * link-or-publish CTA — empty items because the project has no work
   * surface yet. Null/absent on the global view and on older servers.
   */
  link_state?: "linked" | "unlinked" | null;
  /**
   * Native declared items excluded because their project is unlinked
   * (#183); on an unlinked project scope, the count a link or publish
   * would migrate upstream.
   */
  excluded_unlinked?: number;
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

/** One row of the ordered notification feed, as `events.list` returns it.
 *
 *  `Event` in `core/entities.py`. Three of these fields are what make a work
 *  item's state history answerable at all, and none of them is decoration:
 *
 *  - `summary` is the payload the audit log deliberately does not keep. A
 *    `work.transitioned` event carries `{ref, from, to}` there, which is the
 *    only place the state an item *came from* is recorded — `audit` holds a
 *    `payload_digest`, so it can prove a transition happened and cannot say
 *    what it was. Typed loosely because its shape is the event kind's: a
 *    reader has to say what it expects and cope with it not being there.
 *  - `audit_id` names the audit row that explains this event, which is the
 *    join between "what the state was" and "why somebody changed it".
 *  - `actor_id` is an id, not an identity. It says *that* an actor did this;
 *    the readable name lives on the audit row this event points at.
 *
 *  `seq` is the cursor. Nothing prunes this table, so an entity's slice of
 *  the feed is complete rather than recent — which is the property that lets
 *  a surface page it to the end and say it has the whole story. */
export interface VogtEvent {
  seq: number;
  kind: string;
  entity_kind: string;
  entity_id: string;
  actor_id?: string | null;
  audit_id?: string | null;
  summary?: Record<string, unknown>;
  at: string;
}

export interface EventListResult {
  events: VogtEvent[];
  /** The seq of the last row returned, or the caller's own cursor when the
   *  page was empty — so a poller never rewinds. */
  next_cursor: number;
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
  /** What the session was *asked* to run (FR-T11) — not what the agent inside
   *  it is using now, which nothing here can know. */
  model?: string | null;
  effort?: string | null;
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

/** The evidence a drift proposal carries, copied at raise time (FR-R5).
 *
 *  Self-contained on purpose: retention prunes the two stores independently,
 *  and a proposal that outlived its evidence would be an act with nothing
 *  behind it. This is the *observed* side of every disagreement, and it is
 *  what FR-U18 requires a reader to see before resolving one. */
export interface EvidenceSnapshot {
  subject_key?: string;
  content_digest?: string;
  observed_at?: string;
  collector?: string;
  payload?: Record<string, unknown>;
}

export interface DriftProposal {
  id: string;
  kind: string;
  subject_kind: string;
  subject_id: string;
  project_id?: string | null;
  project_slug?: string | null;
  summary: string;
  evidence_observation_id?: string | null;
  evidence_snapshot?: EvidenceSnapshot;
  /** What accepting would write. `from`/`to` are the declared and observed
   *  values for the kinds that propose a change; the kinds that propose none
   *  say so with an `action` instead. */
  proposed_change?: Record<string, unknown>;
  status: "open" | "accepted" | "rejected" | "contested" | string;
  opened_at: string;
  /** Set where a sweep newer than this proposal no longer reproduces the
   *  condition that raised it (FR-R6). Not a resolution: the proposal is
   *  still open and still needs a person. It clears if the condition
   *  reproduces again. */
  superseded_at?: string | null;
  superseded_detail?: string | null;
  resolved_by_actor_id?: string | null;
  resolved_by_identity_ref?: string | null;
  resolved_at?: string | null;
  resolution_reason?: string | null;
}

export interface DriftListResult {
  proposals: DriftProposal[];
  /** Kinds the shipped policy never auto-accepts, and why. Quoted rather
   *  than restated in a view: two copies of a policy is one too many. */
  human_gated?: Record<string, string>;
  freshness: FreshnessSummary;
}

export interface DriftResult {
  proposal: DriftProposal;
  change_applied: boolean;
}

/** One recorded reference between projects (FR-D1–D4).
 *
 *  Records *which projects reference which* and stops there: no lockfile is
 *  parsed and no package version is resolved (r2 decision), so there is no
 *  ecosystem or constraint here and a view must not imply one. */
export interface DepRef {
  subject_key: string;
  from_project_id: string;
  from_project_slug?: string | null;
  ref_kind: string;
  raw_target: string;
  manifest?: string | null;
  to_project_id?: string | null;
  to_project_slug?: string | null;
  observed_at: string;
}

export interface DepsResult {
  project: string;
  references_out: DepRef[];
  referenced_by: DepRef[];
  unresolved: number;
  freshness: FreshnessSummary;
}

/** One thing a collector found, as the observed store holds it (FR-O2).
 *
 *  The raw evidence row, not a view over it: `observations.list` is the one
 *  operation that returns the observed store itself, and a surface reading it
 *  is reading what was *seen* rather than what a ranking made of it.
 *
 *  `payload` is deliberately `unknown`-valued. Its shape is the collector's,
 *  it differs per `kind`, and a reader that wants a field out of it has to
 *  say what it expects and cope with the field not being there — which is the
 *  honest position, since a sweep from an older build wrote older payloads. */
export interface Observation {
  id: string;
  sweep_id: string;
  collector: string;
  kind: string;
  project_id?: string | null;
  subject_key: string;
  payload?: Record<string, unknown>;
  content_digest: string;
  source_url?: string | null;
  promoted?: boolean;
  observed_at: string;
}

export interface ObservationsResult {
  observations: Observation[];
  /** How many rows this answer carries. Equal to `limit` means the store had
   *  at least that many and the list is cut, not complete. */
  total: number;
}

export interface CriterionView {
  rule: string;
  target: string;
  satisfied: boolean;
  detail: string;
}

export interface ComplianceResult {
  project: string;
  status: string;
  contract_version: string;
  checked_at?: string | null;
  /** How old this answer is. Never refreshed implicitly, so a view that
   *  renders the status without the age is showing a claim of unknown date. */
  age_seconds?: number | null;
  failing: CriterionView[];
  detail?: string | null;
}

// -- reads ------------------------------------------------------------------

export const listWorkflows = () =>
  call<{ workflows: Workflow[] }>("workflow.list");

export const listWork = (
  params: Record<string, unknown> = {},
  signal?: AbortSignal,
) =>
  call<{
    items: WorkItem[];
    total?: number;
    /** "unlinked" on a project scope is the #183 link-or-publish marker. */
    link_state?: "linked" | "unlinked" | null;
  }>("work.list", params, "GET", signal);

export const listBoard = (params: Record<string, unknown>, signal?: AbortSignal) =>
  call<BoardListResult>("board.list", params, "POST", signal);

export const getWork = (ref: string) => call<WorkDetail>("work.get", { ref });

export const backlog = (params: Record<string, unknown> = {}) =>
  call<RankedView>("backlog", params);

export const bugs = (params: Record<string, unknown> = {}) =>
  call<RankedView>("bugs", params);

export const why = (ref: string) => call<WhyResult>("why", { ref });

export interface ProjectListEntry {
  slug: string;
  name: string;
  root_path: string;
}

export const listProjects = (
  params: Record<string, unknown> = {},
  signal?: AbortSignal,
) => call<{ projects: ProjectListEntry[]; total: number }>(
  "project.list",
  params,
  "GET",
  signal,
);

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

export const notifications = (params: Record<string, unknown> = {}) =>
  // The inbox. Its coverage block is the load-bearing half: an empty list
  // under a collector that did not run means "nobody asked", which is a
  // different answer from "nothing to say" (FR-N3).
  call<Record<string, unknown>>("notifications", params);

/** Read the canonical server-owned Inbox. The browser never composes legacy
 * notification, drift, event or engine reads into this answer. */
export const listInbox = (params: Record<string, unknown> = {}) =>
  call<InboxListResult>("inbox.list", params);

export const archiveInbox = (entry_key: string, reason: string) =>
  call<{ entry: InboxEntry }>(
    "inbox.archive",
    { entry_key, reason },
    "POST",
  );

export const snoozeInbox = (entry_key: string, until: string, reason: string) =>
  call<{ entry: InboxEntry }>(
    "inbox.snooze",
    { entry_key, until, reason },
    "POST",
  );

export const restoreInbox = (entry_key: string, reason: string) =>
  call<{ entry: InboxEntry }>(
    "inbox.restore",
    { entry_key, reason },
    "POST",
  );

/** `suppress` and `work.adopt` are subject operations, not Inbox ones: the
 *  Backlog's observed rows reach the same two writes with the same reason
 *  rule. */
export const suppressSubject = (subject: string, reason: string) =>
  call<Record<string, unknown>>("suppress", { subject, reason }, "POST");

export const adoptSubject = (subject: string, reason: string) =>
  call<Record<string, unknown>>("work.adopt", { subject, reason }, "POST");

export const resolveInboxDrift = (
  id: string,
  resolution: "accepted" | "rejected",
  reason: string,
) => call<DriftResult>("drift.resolve", { id, resolution, reason }, "POST");

/** The audit log, narrowed and paged (FR-S6, FR-U19).
 *
 *  `AuditListResult` carries `total` beside the records: how many match the
 *  narrowing, ignoring `limit` and `offset`. It is not decoration — with
 *  `offset` it is what lets a reader page past the newest records and know
 *  whether they are looking at the whole story, and a client that dropped it
 *  from the type would be a client that could not say how much it was hiding.
 *
 *  Optional here and only here: an older core answers without it, and a
 *  surface that read `0` for "the server did not say" would report an empty
 *  log — which on this operation above all is a claim rather than a shrug. */
export const listAudit = (params: Record<string, unknown> = {}) =>
  call<{ records: AuditRecord[]; total?: number }>("audit.list", params);

export const projectBrief = (slug: string) =>
  call<Record<string, unknown> & { freshness: FreshnessSummary }>(
    "project.brief",
    { slug },
  );

export const listDrift = (params: Record<string, unknown> = {}) =>
  call<DriftListResult>("drift.list", params);

export const deps = (project: string) =>
  call<DepsResult>("deps", { project });

/** The observed store, unranked (FR-O2, FR-U17).
 *
 *  Everything else this client reads is a *view*: `backlog` and `bugs` rank,
 *  `why` explains a ranking, and all three filter out subjects a decision hid.
 *  This is the evidence itself, including the rows those views drop, which is
 *  the only place a surface can read what a collector actually saw.
 *
 *  There is no work-item parameter, and that is the registry's shape rather
 *  than an omission: an observation is filed under its own subject key
 *  (`session:01J…`), not under the item it happens to be about. A reader that
 *  wants one item's evidence asks for the kinds it understands and matches on
 *  the payload, which is what `WorkItemDetail` does. */
export const listObservations = (params: Record<string, unknown> = {}) =>
  call<ObservationsResult>("observations.list", params);

export const compliance = (project: string) =>
  call<ComplianceResult>("compliance", { project });

/** The ordered feed, from a cursor and optionally about one entity (FR-N1).
 *
 *  The cursor feed behind FR-U10: a surface that polls this and reports how
 *  old its answer is tells the truth; one that re-lists and calls itself live
 *  does not.
 *
 *  `entity_id` is the second thing it is for, and the parameter is the id
 *  rather than the ref — the same shape `audit.list` takes, so the two feeds
 *  narrow alike. Narrowed, this is a work item's *state history*: the server
 *  applies the filter in SQL, so paging a narrowed feed walks that item's
 *  events and not whichever slice of the whole feed happened to contain some
 *  of them. A caller that filtered a page itself would decide the history
 *  ended at the first quiet stretch, which on a busy estate is immediately.
 *
 *  Takes a bag rather than positional arguments because there are now three
 *  of them and two are optional; a signature where `entity_id` came third
 *  would be one where narrowing meant restating the defaults. */
export const listEvents = (
  params: { after?: number; limit?: number; entity_id?: string } = {},
) => call<EventListResult>("events.list", params);

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

/** Resolve exactly one proposal (FR-R2, FR-U18).
 *
 *  Singular by signature, and that is the requirement rather than an
 *  oversight: bulk accept is deferred by name in `REQUIREMENTS.md` §3, so
 *  there is no list form of this call for a view to reach for. */
export const resolveDrift = (
  id: string,
  resolution: "accepted" | "rejected" | "contested",
  reason: string,
) => call<DriftResult>("drift.resolve", { id, resolution, reason }, "POST");

/** Clone a named repository, register it, and read what is already there.
 *
 *  `repo` may be typed by hand or chosen from {@link listForgeRepos}. The
 *  picker is an *enumeration* of what the acting credential can see (#180,
 *  design #178 decision 5), not the candidate crawl r3 removed: the token is
 *  the scope, and a person still confirms what is imported. */
export const importProject = (
  params: Record<string, unknown> & { repo: string; reason: string },
) => call<Record<string, unknown>>("project.import", params, "POST");

/** One repository the acting credential can see, as the picker renders it. */
export interface ForgeRepoView {
  owner: string;
  name: string;
  default_branch: string | null;
  visibility: string;
  url: string;
  already_registered: boolean;
}

export interface ForgeReposResult {
  repos: ForgeRepoView[];
  login: string | null;
  detail: string | null;
}

/** Enumerate the repositories the linked credential (#179) can see, so a
 *  person can pick which to import (#180). Lists by credential, never by
 *  crawl — an empty list with a `detail` means "not collected", not "you have
 *  no repositories". */
export const listForgeRepos = (host = "github.com") =>
  call<ForgeReposResult>("forge.repos", { host }, "GET");

export const startSession = (
  params: Record<string, unknown> & { reason: string },
) => call<{ session: SessionSummary }>("session.start", params, "POST");

export const stopSession = (id: string, reason: string) =>
  call<{ session: SessionSummary }>("session.stop", { id, reason }, "POST");
