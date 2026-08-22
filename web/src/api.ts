// Thin typed wrapper over the MyDevEnv2 HTTP+SSE+WS API.

export type ActivityState =
  | "idle"
  | "running"
  | "waiting-for-input"
  | "errored";

export interface SessionSummary {
  id: string;
  name: string;
  activity: ActivityState;
  exit_code: number | null;
  scrollback_bytes: number;
  cwd: string;
  /** Explicit command the session was created with (absent for default-shell sessions). */
  command?: string | null;
  created_at: string;
  /** Wall-clock instant when the current activity state began. */
  activity_changed_at?: string;
}

export interface CreateSessionRequest {
  name: string;
  command?: string[];
  cwd?: string;
  env?: [string, string][];
  cols?: number;
  rows?: number;
  scrollback_bytes?: number;
}

export interface SessionDetail {
  summary: SessionSummary;
  scrollback_pos: number;
  scrollback_base64: string;
}

export interface OkResponse {
  ok: boolean;
}

export interface WriteFileResponse extends OkResponse {
  bytes: number;
  /** SHA-256 hex of the bytes just written — the client's new save baseline. */
  hash?: string;
  /** On-disk mtime after the write, milliseconds since the Unix epoch. */
  mtime?: number;
}

export interface FileOpResponse extends OkResponse {
  path?: string;
}

export type GitOpRequest =
  | { op: "stage"; repo?: string; path: string }
  | { op: "unstage"; repo?: string; path: string }
  | { op: "discard"; repo?: string; path: string }
  | { op: "commit"; repo?: string; message: string }
  | { op: "checkout"; repo?: string; branch: string; create?: boolean };

export interface GitOpResponse extends OkResponse {
  branch?: string;
  commit?: string;
}

export type ServerEvent =
  | { type: "session-created"; id: string; name: string }
  | { type: "session-renamed"; id: string; name: string }
  | { type: "session-killed"; id: string; exit_code: number | null }
  | { type: "activity"; id: string; state: ActivityState }
  /**
   * Something changed in vogt-core, republished by the front door.
   *
   * Deliberately thin: it says there is something to read, not what. A
   * surface that wants the change reads it from Vogt — this is what lets a
   * board stop polling and still be honest about being current (FR-U10).
   */
  | {
      type: "vogt-changed";
      kind: string;
      entity_kind: string;
      entity_id: string;
      seq: number;
    };

const TOKEN_KEY = "mydevenv2.token";
const BASE_KEY = "mydevenv2.base";
const AUTH_CHANNEL_NAME = "mydevenv2.auth";
const AUTH_SOURCE_ID = `auth-${Math.random().toString(36).slice(2)}`;

interface AuthStateMessage {
  type: "auth-state";
  source: string;
  revision: number;
}

/**
 * Why a session ended: a credential the server refused, or one the reader
 * handed back. `status` is the rejecting HTTP status — 401, and only 401 —
 * or 0 for a deliberate sign-out, and `detail` carries the server's own words
 * when it gave any.
 *
 * Nothing else is published on this channel, and the omissions are the point
 * (#195). A 403 says the credential is good and the capability is missing; a
 * 502, a 503 or a dead socket says the engine is away. Both are the caller's
 * to render where it happened, and collapsing either into "signed out" is the
 * FR-O4 conflation of "offline" with "unauthorized" that this path exists to
 * keep apart.
 */
export interface AuthRejection {
  status: number;
  detail: string;
}

interface AuthRejectedMessage extends AuthRejection {
  type: "auth-rejected";
  source: string;
}

type AuthMessage = AuthStateMessage | AuthRejectedMessage;

function postAuthMessage(message: AuthMessage) {
  try {
    if (typeof BroadcastChannel !== "undefined") {
      const channel = new BroadcastChannel(AUTH_CHANNEL_NAME);
      channel.postMessage(message);
      channel.close();
    }
  } catch {
    /* BroadcastChannel unavailable */
  }
}

function broadcastAuthState() {
  postAuthMessage({
    type: "auth-state",
    source: AUTH_SOURCE_ID,
    revision: Date.now(),
  });
}

const authRejectedListeners = new Set<(rejection: AuthRejection) => void>();

function publishAuthRejection(rejection: AuthRejection) {
  // The tab that *got* the 401 has to sign out too, and a broadcast is
  // filtered by `source` precisely so a tab never hears its own — so this
  // tab's listeners are called here, in-process, and the channel carries the
  // same fact to the others.
  for (const listener of [...authRejectedListeners]) listener(rejection);
  postAuthMessage({ type: "auth-rejected", source: AUTH_SOURCE_ID, ...rejection });
}

/**
 * Report the status of an authenticated response, so one rejected credential
 * ends the session once instead of leaving N panels each holding their own
 * dead error and a shell that still believes it is signed in (#195).
 *
 * Anything that is not a 401 returns without a word — see `AuthRejection` for
 * why that silence is deliberate rather than an omission.
 */
export function reportAuthResponse(status: number, detail = ""): void {
  if (status !== 401) return;
  publishAuthRejection({ status, detail });
}

/**
 * Hand the credential back deliberately: the sign-out control. Same
 * session-level fact as a refusal, so it travels the same way and every
 * surface that listens for one reacts to both.
 */
export function signOut(detail = ""): void {
  clearStoredAuth();
  publishAuthRejection({ status: 0, detail });
}

export function getToken(): string {
  return localStorage.getItem(TOKEN_KEY) ?? "";
}

export function setToken(token: string) {
  if (token) localStorage.setItem(TOKEN_KEY, token);
  else localStorage.removeItem(TOKEN_KEY);
  broadcastAuthState();
}

/**
 * HTTP base. Empty string in dev → same-origin (Vite proxies /api to backend).
 * Override via the settings modal when the PWA is served from a different
 * origin to the API (e.g. running the built bundle against a remote box).
 */
export function getBase(): string {
  return localStorage.getItem(BASE_KEY) ?? "";
}

export function setBase(base: string) {
  if (base) localStorage.setItem(BASE_KEY, base.replace(/\/+$/, ""));
  else localStorage.removeItem(BASE_KEY);
  broadcastAuthState();
}

export function clearStoredAuth() {
  let hadCredential = false;
  try {
    hadCredential =
      localStorage.getItem(TOKEN_KEY) !== null ||
      localStorage.getItem(BASE_KEY) !== null;
    localStorage.removeItem(TOKEN_KEY);
    localStorage.removeItem(BASE_KEY);
  } catch {
    /* localStorage unavailable */
  }
  // Announcing a clear that cleared nothing makes every other tab reload for
  // a change that did not happen — which is what two tabs handling the same
  // 401 do, each clearing after the other, reloading the tab that is already
  // showing the reader why they were signed out.
  if (hadCredential) broadcastAuthState();
}

export function subscribeAuthState(onChange: () => void): () => void {
  const onStorage = (event: StorageEvent) => {
    if (event.key === null || event.key === TOKEN_KEY || event.key === BASE_KEY) {
      onChange();
    }
  };
  window.addEventListener("storage", onStorage);

  let channel: BroadcastChannel | null = null;
  try {
    if (typeof BroadcastChannel !== "undefined") {
      channel = new BroadcastChannel(AUTH_CHANNEL_NAME);
      channel.addEventListener("message", (event: MessageEvent<AuthStateMessage>) => {
        const data = event.data;
        if (data?.type === "auth-state" && data.source !== AUTH_SOURCE_ID) {
          onChange();
        }
      });
    }
  } catch {
    channel = null;
  }

  return () => {
    window.removeEventListener("storage", onStorage);
    channel?.close();
  };
}

/**
 * Subscribe to credentials being refused or handed back.
 *
 * Separate from `subscribeAuthState` above because the two are different
 * facts with different answers: that one says the stored credential changed
 * (reload and use it), this one says there is no usable credential any more
 * (return to the login screen and say so). Riding the same channel means one
 * tab's 401 signs the others out for free.
 */
export function subscribeAuthRejected(
  onRejected: (rejection: AuthRejection) => void,
): () => void {
  authRejectedListeners.add(onRejected);

  let channel: BroadcastChannel | null = null;
  try {
    if (typeof BroadcastChannel !== "undefined") {
      channel = new BroadcastChannel(AUTH_CHANNEL_NAME);
      channel.addEventListener("message", (event: MessageEvent<AuthMessage>) => {
        const data = event.data;
        if (data?.type === "auth-rejected" && data.source !== AUTH_SOURCE_ID) {
          onRejected({ status: data.status, detail: data.detail });
        }
      });
    }
  } catch {
    channel = null;
  }

  return () => {
    authRejectedListeners.delete(onRejected);
    channel?.close();
  };
}

function authHeaders(extra: Record<string, string> = {}): Record<string, string> {
  const tok = getToken();
  return tok ? { Authorization: `Bearer ${tok}`, ...extra } : extra;
}

export class ApiError extends Error {
  constructor(public status: number, public body: string) {
    super(`HTTP ${status}: ${body}`);
  }
}

/**
 * The error for a refused response, reporting a rejected credential on the
 * way out. Every authenticated call in this module raises its failure through
 * here, so the session-level fact is noticed once, centrally, rather than by
 * whichever caller happened to be first (#195).
 */
function refused(status: number, body: string): ApiError {
  reportAuthResponse(status, body);
  return new ApiError(status, body);
}

async function req<T>(
  method: string,
  path: string,
  body?: unknown,
  signal?: AbortSignal,
): Promise<T> {
  const res = await fetch(`${getBase()}${path}`, {
    method,
    headers: authHeaders(
      body !== undefined ? { "Content-Type": "application/json" } : {},
    ),
    body: body !== undefined ? JSON.stringify(body) : undefined,
    signal,
  });
  const text = await res.text();
  if (!res.ok) throw refused(res.status, text);
  return text ? (JSON.parse(text) as T) : (undefined as T);
}

/**
 * Test credentials without changing the active browser session. This is used
 * by Settings so a bad token or backend URL is never persisted optimistically.
 */
export async function validateCredentials(
  token: string,
  base: string,
): Promise<OperationalStatus> {
  const candidateToken = token.trim();
  const candidateBase = base.trim().replace(/\/+$/, "");
  if (!candidateToken) throw new ApiError(401, "Bearer token is required");

  const res = await fetch(`${candidateBase}/api/status`, {
    method: "GET",
    headers: {
      Authorization: `Bearer ${candidateToken}`,
      Accept: "application/json",
    },
  });
  const text = await res.text();
  // Deliberately not `refused()`: this asks about a *candidate* credential
  // the reader has just typed, so a 401 here is that form's answer and not a
  // statement about the session. Reporting it would sign the reader out of a
  // working session for mistyping a token into Settings.
  if (!res.ok) throw new ApiError(res.status, text);
  return JSON.parse(text) as OperationalStatus;
}

export interface FileEntry {
  name: string;
  path: string;
  is_dir: boolean;
  size: number;
}

export interface TreeNode {
  name: string;
  path: string;
  is_dir: boolean;
  children?: TreeNode[] | null;
}

export interface FileRead {
  path: string;
  size: number;
  content: string | null;
  content_base64: string | null;
  is_binary: boolean;
  /** On-disk mtime, ms since the Unix epoch. 0 from a server too old to send it. */
  mtime?: number;
  /** SHA-256 hex of the file's bytes; the editor's optimistic-concurrency token. */
  hash?: string;
}

export type FileOpRequest =
  | { op: "move"; from: string; to: string; create_parents?: boolean }
  | { op: "delete"; path: string; recursive?: boolean }
  | { op: "mkdir"; path: string; parents?: boolean }
  | { op: "duplicate"; from: string; to: string; create_parents?: boolean };

export interface SearchHit {
  path: string;
  line: number;
  text: string;
}

export interface FileSearchResult {
  path: string;
  name: string;
}

export interface HistorySessionMetadata {
  id: string;
  name: string;
  created_at: string;
  ended_at: string | null;
  exit_code: number | null;
  cwd: string | null;
  command: string | null;
  scrollback_bytes: number;
}

export interface HistorySearchResult {
  session_id: string;
  session_name: string;
  created_at: string;
  match_snippet: string;
  rank: number;
}

export interface HistoryLogPreview {
  session_id: string;
  text: string;
  bytes: number;
  total_bytes: number;
  truncated: boolean;
}

export interface PushQuietHours {
  enabled: boolean;
  start_minute: number;
  end_minute: number;
  utc_offset_minutes: number;
  digest: boolean;
}

export interface PushPreferences {
  waiting_for_input: boolean;
  errored: boolean;
  idle_stall: boolean;
  agent_task_started: boolean;
  agent_task_notify: boolean;
  // New drift raised in vogt-core (FR-M2). Absent from any subscription
  // stored before the engine grew the kind; the server defaults it on.
  drift: boolean;
  quiet_hours: PushQuietHours;
}

export interface PushSubscriptionEntry {
  id: string;
  label: string | null;
  created_at: string;
  kind: {
    kind: "web-push" | "fcm";
    endpoint_host?: string;
  };
  prefs: PushPreferences;
  pending_digest_count: number;
  pending_digest_since?: string | null;
}

export type AgentTaskStatus = "active" | "paused";
export type AgentTaskRunTrigger = "manual" | "scheduled";

export type AgentTaskSchedule =
  | { kind: "manual" }
  | { kind: "interval"; minutes: number }
  | { kind: "daily"; times: string[] };

/** One thing a run reported about itself (engine `AgentTaskFinding`, FR-E7). */
export interface AgentTaskFinding {
  at: string;
  text: string;
  source: string;
}

export interface AgentTaskRun {
  id: string;
  task_id: string;
  started_at: string;
  trigger: AgentTaskRunTrigger;
  session_id: string;
  session_name: string;
  prompt_file: string;
  context_file: string;
  status: "running" | "completed" | "errored";
  completed_at: string | null;
  exit_code: number | null;
  summary: string | null;
  findings: AgentTaskFinding[];
}

export interface AgentTask {
  id: string;
  name: string;
  prompt: string;
  schedule: AgentTaskSchedule;
  status: AgentTaskStatus;
  command: string[] | null;
  cwd: string | null;
  env: [string, string][];
  context: string | null;
  vogt_project: string | null;
  vogt_work_item: string | null;
  notify_on_start: boolean;
  notify_on_phrase: string | null;
  auto_retry_on_rate_limit: boolean;
  next_run: string | null;
  last_run: string | null;
  run_count: number;
  runs: AgentTaskRun[];
  created_at: string;
  updated_at: string;
}

export interface AgentTaskUpsertRequest {
  name: string;
  prompt: string;
  schedule: AgentTaskSchedule;
  command?: string[] | null;
  cwd?: string | null;
  env?: [string, string][];
  context?: string | null;
  vogt_project?: string | null;
  vogt_work_item?: string | null;
  enabled?: boolean;
  notify_on_start?: boolean;
  notify_on_phrase?: string | null;
  auto_retry_on_rate_limit?: boolean;
}

export const api = {
  listSessions: () => req<SessionSummary[]>("GET", "/api/sessions"),
  createSession: (s: CreateSessionRequest) =>
    req<SessionSummary>("POST", "/api/sessions", s),
  getSession: (id: string) => req<SessionDetail>("GET", `/api/sessions/${id}`),
  renameSession: (id: string, name: string) =>
    req<OkResponse>("PATCH", `/api/sessions/${id}`, { name }),
  killSession: (id: string) =>
    req<OkResponse>("POST", `/api/sessions/${id}/kill`),
  deleteSession: (id: string) =>
    req<OkResponse>("DELETE", `/api/sessions/${id}`),
  health: () => req<OkResponse>("GET", "/healthz"),

  listDir: (path = "") =>
    req<FileEntry[]>("GET", `/api/dir?path=${encodeURIComponent(path)}`),
  tree: (path = "", depth = 1) =>
    req<TreeNode[]>(
      "GET",
      `/api/tree?path=${encodeURIComponent(path)}&depth=${depth}`,
    ),
  readFile: (path: string, signal?: AbortSignal) =>
    req<FileRead>("GET", `/api/files?path=${encodeURIComponent(path)}`, undefined, signal),
  writeFile: (
    path: string,
    content: string,
    create_parents = false,
    if_match?: string,
  ) =>
    req<WriteFileResponse>("PUT", "/api/files", {
      path,
      content,
      create_parents,
      // Only sent when the caller has a baseline to guard against: absent
      // preserves the server's last-writer-wins behaviour (new files, uploads).
      ...(if_match ? { if_match } : {}),
    }),
  writeFileBase64: (
    path: string,
    content_base64: string,
    create_parents = false,
  ) =>
    req<WriteFileResponse>("PUT", "/api/files", {
      path,
      content_base64,
      create_parents,
    }),
  fileOp: (request: FileOpRequest) =>
    req<FileOpResponse>("POST", "/api/files/op", request),
  downloadFile: async (path: string): Promise<void> => {
    const url = `${getBase()}/api/files/download?path=${encodeURIComponent(path)}`;
    const tok = getToken();
    const res = await fetch(url, {
      headers: tok ? { Authorization: `Bearer ${tok}` } : {},
    });
    if (!res.ok) throw refused(res.status, await res.text());
    const blob = await res.blob();
    const objUrl = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = objUrl;
    a.download = path.split("/").pop() || "download";
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
    URL.revokeObjectURL(objUrl);
  },
  search: (q: string, path = "") =>
    req<SearchHit[]>(
      "GET",
      `/api/search?q=${encodeURIComponent(q)}&path=${encodeURIComponent(path)}`,
    ),
  searchFiles: (q: string, path = "", max?: number, signal?: AbortSignal) =>
    req<FileSearchResult[]>(
      "GET",
      `/api/search/files?q=${encodeURIComponent(q)}&path=${encodeURIComponent(path)}${
        typeof max === "number" ? `&max=${max}` : ""
      }`,
      undefined,
      signal,
    ),

  listAgentTasks: (signal?: AbortSignal) =>
    req<AgentTask[]>("GET", "/api/agent-tasks", undefined, signal),
  getAgentTask: (id: string) => req<AgentTask>("GET", `/api/agent-tasks/${id}`),
  createAgentTask: (task: AgentTaskUpsertRequest) =>
    req<AgentTask>("POST", "/api/agent-tasks", task),
  updateAgentTask: (id: string, task: Partial<AgentTaskUpsertRequest>) =>
    req<AgentTask>("PATCH", `/api/agent-tasks/${id}`, task),
  deleteAgentTask: (id: string) =>
    req<OkResponse>("DELETE", `/api/agent-tasks/${id}`),
  pauseAgentTask: (id: string) =>
    req<AgentTask>("POST", `/api/agent-tasks/${id}/pause`),
  resumeAgentTask: (id: string) =>
    req<AgentTask>("POST", `/api/agent-tasks/${id}/resume`),
  runAgentTask: (id: string) =>
    req<AgentTaskRun>("POST", `/api/agent-tasks/${id}/run`),
  cleanupAgentTaskArtifacts: (keepLatestRunsPerTask: number) =>
    req<{
      removed_task_dir_count: number;
      removed_prompt_file_count: number;
      removed_context_file_count: number;
      removed_bytes: number;
    }>("POST", "/api/agent-tasks/artifacts/cleanup", {
      keep_latest_runs_per_task: keepLatestRunsPerTask,
    }),

  listHistorySessions: (limit = 50, offset = 0) =>
    req<HistorySessionMetadata[]>(
      "GET",
      `/api/history/sessions?limit=${limit}&offset=${offset}`,
    ),
  searchHistory: (query: string, limit = 20) =>
    req<HistorySearchResult[]>(
      "GET",
      `/api/history/search?q=${encodeURIComponent(query)}&limit=${limit}`,
    ),
  getHistorySession: (id: string) =>
    req<HistorySessionMetadata>("GET", `/api/history/${id}`),
  getHistorySessionLog: (id: string, tailBytes = 64 * 1024) =>
    req<HistoryLogPreview>(
      "GET",
      `/api/history/${id}/log?tail_bytes=${tailBytes}`,
    ),
  deleteHistorySession: (id: string) =>
    req<OkResponse>("DELETE", `/api/history/${id}`),
  cleanupHistorySessions: (retentionDays: number) =>
    req<{ ok: boolean; removed_sessions: number; retention_days: number }>(
      "POST",
      "/api/history/cleanup",
      { retention_days: retentionDays },
    ),
  downloadHistorySession: async (id: string): Promise<void> => {
    const url = `${getBase()}/api/history/${id}/download`;
    const tok = getToken();
    const res = await fetch(url, {
      headers: tok ? { Authorization: `Bearer ${tok}` } : {},
    });
    if (!res.ok) throw refused(res.status, await res.text());
    const blob = await res.blob();
    const disposition = res.headers.get("content-disposition") ?? "";
    const match = disposition.match(/filename=\"([^\"]+)\"/i);
    const filename = match?.[1] || `${id}.log`;
    const objUrl = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = objUrl;
    a.download = filename;
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
    URL.revokeObjectURL(objUrl);
  },

  gitStatus: (repo = "") =>
    req<GitStatusResp>("GET", `/api/git/status?repo=${encodeURIComponent(repo)}`),
  gitLog: (repo = "", n = 50) =>
    req<GitLogEntry[]>(
      "GET",
      `/api/git/log?repo=${encodeURIComponent(repo)}&n=${n}`,
    ),
  gitBranch: (repo = "") =>
    req<GitBranch>("GET", `/api/git/branch?repo=${encodeURIComponent(repo)}`),
  gitDiff: (repo: string, path: string, staged = false) =>
    req<GitDiffResp>(
      "GET",
      `/api/git/diff?repo=${encodeURIComponent(repo)}&path=${encodeURIComponent(path)}&staged=${staged}`,
    ),
  gitOp: (request: GitOpRequest) =>
    req<GitOpResponse>("POST", "/api/git/op", request),

  // Public — no token required.
  publicConfig: () =>
    fetch(`${getBase()}/api/config`).then((r) => r.json() as Promise<PublicConfig>),
  operationalStatus: () => req<OperationalStatus>("GET", "/api/status"),

  guiLaunch: (command: string[], via_sway = true) =>
    req<GuiProc>("POST", "/api/gui/launch", { command, via_sway }),
  guiProcesses: () => req<GuiProc[]>("GET", "/api/gui/processes"),
  guiKill: (pid: number) =>
    req<{ ok: boolean }>("POST", `/api/gui/kill?pid=${pid}`),

  listPushSubscriptions: () =>
    req<PushSubscriptionEntry[]>("GET", "/api/push/list"),
  updatePushSubscription: (
    id: string,
    update: {
      label?: string | null;
      clear_label?: boolean;
      prefs?: PushPreferences;
    },
  ) => req<{ ok: boolean; id: string; label: string | null; prefs: PushPreferences }>(
    "POST",
    "/api/push/update",
    { id, ...update },
  ),
  pushTest: (title = "Vogt test", body = "Push notifications are working.") =>
    req<{ ok: number; fail: number; queued: number }>("POST", "/api/push/test", { title, body }),
  flushPushDigests: () =>
    req<{ ok: number; fail: number; queued: number }>("POST", "/api/push/flush-digests"),

  // `profile` names which configured backend runs this turn (FR-T9). Omitted
  // means the deployment's default, which is what every caller sent before
  // profiles existed.
  assistantMessage: (text: string, profile?: string) =>
    req<AssistantReply>("POST", "/api/assistant/message", {
      text,
      ...(profile ? { profile } : {}),
    }),
  assistantAction: (id: string, approve: boolean) =>
    req<AssistantReply>("POST", `/api/assistant/actions/${id}`, { approve }),
  assistantReplaceReason: (id: string, reason: string) =>
    req<AssistantReasonPreview>("PATCH", `/api/assistant/actions/${id}`, { reason }),
  assistantHistory: () => req<AssistantHistory>("GET", "/api/assistant/history"),
  assistantReset: () => req<OkResponse>("POST", "/api/assistant/reset"),
  /**
   * Server-side STT (FR-T12): upload captured audio, get text back. Used by a
   * client with no on-device recognizer. Throws `ApiError` with `status: 404`
   * when the route is unconfigured, which the caller reads as "fall back"
   * (FR-T6) — audio is proxied, never stored.
   */
  assistantStt: async (audio: Blob): Promise<{ text: string }> => {
    const form = new FormData();
    // The engine forwards the first file-bearing field regardless of name; the
    // filename's extension hints the provider at the container.
    form.append("file", audio, "take.webm");
    const res = await fetch(`${getBase()}/api/assistant/stt`, {
      method: "POST",
      headers: authHeaders(),
      body: form,
    });
    const text = await res.text();
    if (!res.ok) throw refused(res.status, text);
    return text ? (JSON.parse(text) as { text: string }) : { text: "" };
  },
  /**
   * Server-side TTS (FR-T12): send reply text, get an audio blob to play. Used
   * by a client with no on-device synthesis. Throws `ApiError` with
   * `status: 404` when unconfigured, so the caller falls back (FR-T6).
   */
  assistantTts: async (text: string): Promise<Blob> => {
    const res = await fetch(`${getBase()}/api/assistant/tts`, {
      method: "POST",
      headers: authHeaders({ "Content-Type": "application/json" }),
      body: JSON.stringify({ text }),
    });
    if (!res.ok) throw refused(res.status, await res.text());
    return res.blob();
  },
  sessionInput: (id: string, text: string, submit = false) =>
    req<OkResponse>("POST", `/api/sessions/${id}/input`, { text, submit }),

  getBase: () => getBase(),
  getToken: () => getToken(),
};

export interface SessionTemplate {
  name: string;
  description: string;
  command: string[] | null;
  cwd: string | null;
  env: [string, string][];
  default_name?: string | null;
  match_repo_names?: string[];
  match_path_prefixes?: string[];
  tags?: string[];
}

export interface PublicConfig {
  gui_stream_url: string | null;
  /** Server-owned proof that the configured stream is safe to advertise. */
  gui_stream_available?: boolean;
  version: string;
  /** Build-time feature availability, e.g. `{ selkies: "1.6.2" | null }`. */
  features?: Record<string, string | null | undefined>;
  session_templates?: SessionTemplate[];
  /** True when the server has an assistant backend key provisioned. */
  assistant_enabled?: boolean;
  /**
   * Whether the server-side speech routes are configured (FR-T12). A client
   * with no on-device recognizer/synthesis uses them as its speech path, and
   * these flags let it choose that path by capability rather than by provoking
   * a 404. Presence only — never a key or a base URL.
   */
  assistant_stt_enabled?: boolean;
  assistant_tts_enabled?: boolean;
  /**
   * Whether this front door has a vogt-core behind it, and where its surfaces
   * are mounted. Presence only — never a token. Read before offering a Vogt
   * tab: one that opens and then reports an outage is a worse answer than no
   * tab at all (FR-U21).
   */
  vogt?: {
    configured: boolean;
    api_prefix?: string;
    mcp_prefix?: string;
    legacy_gui_prefix?: string;
  };
  assistant_model?: string | null;
  /**
   * The provider profiles a request may name (FR-T9). A name and the model
   * that name runs, and nothing else: the key would be spendable and the base
   * URL is an exposure value, so neither is advertised.
   */
  assistant_profiles?: { name: string; model: string; default: boolean }[];
}

export interface AssistantTranscriptEntry {
  role: "user" | "assistant";
  text: string;
  tool_trace?: string[];
}

/** Keystrokes the assistant wants to type into a terminal. */
export interface AssistantSendInputAction {
  kind: "send_input";
  id: string;
  session_id: string;
  session_name: string;
  text: string;
  submit: boolean;
}

/**
 * A Vogt write the assistant wants to make. `reason` is what Vogt records in
 * its audit log, so it is approved as deliberately as the payload is.
 */
export interface AssistantVogtWriteAction {
  kind: "vogt_write";
  id: string;
  operation: string;
  target: string;
  reason: string;
  payload: string;
}

/** The exact card returned after a Vogt-write reason preview/update. */
export type AssistantReasonPreview = AssistantVogtWriteAction;

/**
 * Discriminated because the two effectors have nothing in common but the gate:
 * a card that renders one as the other would ask for approval of the wrong
 * thing.
 */
export type AssistantPendingAction =
  | AssistantSendInputAction
  | AssistantVogtWriteAction;

export interface AssistantReply {
  reply: string | null;
  pending_action?: AssistantPendingAction;
  tool_trace?: string[];
}

export interface AssistantHistory {
  transcript: AssistantTranscriptEntry[];
  pending_action?: AssistantPendingAction;
}

export interface OperationalStatus {
  version: string;
  session_count: number;
  push_subscription_count: number;
  gui_process_count: number;
  gui_stream_configured: boolean;
  fcm_enabled: boolean;
  history: {
    enabled: boolean;
    archived_session_count: number | null;
    log_file_count: number | null;
    log_bytes: number | null;
    db_bytes: number | null;
  };
  agent_tasks: {
    task_count: number;
    prompt_task_dir_count: number;
    prompt_file_count: number;
    context_file_count: number;
    prompt_bytes: number;
    orphan_task_dir_count: number;
  };
  auth_broker: {
    auto_agent_auth: boolean;
    helper: string;
  };
  storage: {
    state_dir: string;
    workspace_root: string;
  };
}

export interface GuiProc {
  pid: number;
  command: string[];
  launched_at: string;
}

export type GitStatusKind =
  | "untracked"
  | "modified"
  | "staged"
  | "conflicted"
  | "renamed"
  | "deleted";

export interface GitStatusEntry {
  path: string;
  index: string;
  worktree: string;
  kind: GitStatusKind;
}

export interface GitStatusResp {
  repo: string;
  is_repo?: boolean;
  branch: string;
  ahead: number;
  behind: number;
  entries: GitStatusEntry[];
}

export interface GitLogEntry {
  hash: string;
  short: string;
  author: string;
  date: string;
  subject: string;
}

export interface GitBranch {
  current: string;
  all: string[];
}

export interface GitDiffResp {
  path: string;
  current: string;
  head: string;
}

/**
 * Subscribe to server-wide events via SSE. Returns an unsubscribe function.
 *
 * Uses fetch + ReadableStream rather than the native EventSource so the
 * Authorization header can be sent (EventSource has no header support).
 */
export function subscribeEvents(
  onEvent: (ev: ServerEvent) => void,
  onError?: (e: Event) => void,
): () => void {
  let cancelled = false;
  const controller = new AbortController();

  (async () => {
    try {
      const res = await fetch(`${getBase()}/api/events`, {
        headers: authHeaders({ Accept: "text/event-stream" }),
        signal: controller.signal,
      });
      // The stream is an authenticated read like any other, and the one
      // most likely to meet a rotated token first — it outlives every panel.
      // Reported through the same path rather than a second one, so a stream
      // that dies on 401 signs the reader out instead of reconnecting on a
      // dead credential until the backoff gives up (#195).
      if (!res.ok || !res.body) {
        if (!res.ok) reportAuthResponse(res.status, "the event stream was refused");
        throw new Error(`SSE failed: ${res.status}`);
      }
      const reader = res.body.getReader();
      const decoder = new TextDecoder();
      let buf = "";
      while (!cancelled) {
        const { value, done } = await reader.read();
        // A server that closes the stream — a restart, a proxy's idle
        // timeout, a network drop the body ends cleanly on — used to break
        // out of this loop and return, which reported nothing: the store
        // still believed it was connected, no reconnect was ever scheduled,
        // and every surface that re-reads on `vogt-changed` went quiet while
        // looking live. That is exactly FR-U10's "a lost stream shall be
        // indicated and shall reconcile on reconnect", so an end is a
        // failure here and goes out through the same path a failed connect
        // does. An end caused by *this* client unsubscribing is caught by
        // the `cancelled` guard below, as an abort already was.
        if (done) throw new Error("the event stream ended");
        buf += decoder.decode(value, { stream: true });
        let idx;
        while ((idx = buf.indexOf("\n\n")) !== -1) {
          const frame = buf.slice(0, idx);
          buf = buf.slice(idx + 2);
          const lines = frame.split("\n");
          let data = "";
          for (const line of lines) {
            if (line.startsWith("data:")) data += line.slice(5).trimStart();
          }
          if (!data) continue;
          try {
            onEvent(JSON.parse(data) as ServerEvent);
          } catch {
            /* ignore malformed frames */
          }
        }
      }
    } catch (e) {
      if (!cancelled && onError) onError(e as Event);
    }
  })();

  return () => {
    cancelled = true;
    controller.abort();
  };
}

/**
 * Open a WebSocket attached to a session. The bearer token is sent as the
 * first text frame after `open` (`{"type":"auth","token":"..."}`) — browsers
 * can't set Authorization on a WS handshake, and we don't want the token
 * leaking into proxy/access logs via the query string.
 */
export function openAttach(id: string, resumeFrom?: number): WebSocket {
  const base = getBase() || `${location.protocol}//${location.host}`;
  const wsBase = base.replace(/^http/, "ws");
  const ws = new WebSocket(`${wsBase}/api/sessions/${id}/attach`);
  ws.binaryType = "arraybuffer";
  const tok = getToken();
  ws.addEventListener(
    "open",
    () => {
      ws.send(JSON.stringify({
        type: "auth",
        token: tok,
        ...(resumeFrom === undefined ? {} : { resume_from: resumeFrom }),
      }));
    },
    { once: true },
  );
  return ws;
}
