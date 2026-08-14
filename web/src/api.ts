// Thin typed wrapper over the MyDevEnv2 HTTP+SSE+WS API.

export type ActivityState =
  | "idle"
  | "running"
  | "waiting-for-input"
  | "errored";

/** ContextKeeper's protection state for a terminal's agent session. */
export type ProtectionState = "protected" | "unprotected" | "recovering";

/**
 * ContextKeeper's view of the agent session in a terminal. Absent when
 * ContextKeeper is not configured, is unreachable, or has nothing bound to
 * this PTY — all three mean "unprotected", never an error.
 */
export interface SessionContinuity {
  state: ProtectionState;
  /** ContextKeeper's registry id: every continuity call is keyed by it. */
  session_id: string;
  provider: string;
  native_session_id: string;
  work_id?: string | null;
  lifecycle: string;
  event_count: number;
  failure_count: number;
  capture_lag_seconds?: number | null;
  capture_status?: string | null;
}

export interface SessionSummary {
  id: string;
  name: string;
  activity: ActivityState;
  continuity?: SessionContinuity | null;
  exit_code: number | null;
  scrollback_bytes: number;
  cwd: string;
  /** Explicit command the session was created with (absent for default-shell sessions). */
  command?: string | null;
  created_at: string;
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

function broadcastAuthState() {
  const message: AuthStateMessage = {
    type: "auth-state",
    source: AUTH_SOURCE_ID,
    revision: Date.now(),
  };
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
  try {
    localStorage.removeItem(TOKEN_KEY);
    localStorage.removeItem(BASE_KEY);
  } catch {
    /* localStorage unavailable */
  }
  broadcastAuthState();
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

function authHeaders(extra: Record<string, string> = {}): Record<string, string> {
  const tok = getToken();
  return tok ? { Authorization: `Bearer ${tok}`, ...extra } : extra;
}

export class ApiError extends Error {
  constructor(public status: number, public body: string) {
    super(`HTTP ${status}: ${body}`);
  }
}

async function req<T>(
  method: string,
  path: string,
  body?: unknown,
): Promise<T> {
  const res = await fetch(`${getBase()}${path}`, {
    method,
    headers: authHeaders(
      body !== undefined ? { "Content-Type": "application/json" } : {},
    ),
    body: body !== undefined ? JSON.stringify(body) : undefined,
  });
  const text = await res.text();
  if (!res.ok) throw new ApiError(res.status, text);
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
  enabled?: boolean;
  notify_on_start?: boolean;
  notify_on_phrase?: string | null;
  auto_retry_on_rate_limit?: boolean;
}

/** One rung of the continuation ladder. ContextKeeper picks; MyDevEnv2 runs. */
export interface ContinuationRecipe {
  kind: "reattach" | "resume" | "fork" | "bundle" | string;
  provider: string;
  reason: string;
  requires_approval: boolean;
  native_session_id?: string | null;
  bundle_id?: string | null;
  mydevenv2?: CreateSessionRequest | null;
  copyable_command: string;
}

export interface ContinuationPlan {
  session_id: string;
  work_id?: string | null;
  provider: string;
  lifecycle: string;
  attempt_id: string;
  primary: ContinuationRecipe;
  alternatives: ContinuationRecipe[];
}

export interface ContinuityHealth {
  configured: boolean;
  reachable: boolean;
  capture_status?: string | null;
  capture_lag_seconds?: number | null;
  protected_sessions?: number;
}

export interface BundlePreview {
  session_id: string;
  bundle_id: string;
  checksum: string;
  bundle: string;
}

export interface RecoveryLaunch {
  status: "launched" | "manual";
  session_id: string;
  bundle_id: string;
  child_session_id?: string;
  mydevenv2_session?: { id: string };
  copyable_command?: string;
}

export interface WorkAttempt {
  id: string;
  provider: string;
  native_session_id: string;
  lifecycle: string;
  mydevenv2_session_id?: string | null;
  event_count?: number;
  failure_count?: number;
  created_at?: string;
}

export interface WorkSession {
  work_id: string;
  provider: string;
  workspace?: string | null;
  attempts: WorkAttempt[];
  latest_attempt: string;
  lifecycle: string;
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

  // ContextKeeper continuity. Every call is proxied server-side: the browser
  // never holds ContextKeeper's control token.
  continuityHealth: () =>
    req<ContinuityHealth>("GET", "/api/contextkeeper/health"),
  continuation: (sessionId: string) =>
    req<ContinuationPlan>(
      "GET",
      `/api/contextkeeper/sessions/${encodeURIComponent(sessionId)}/continuation`,
    ),
  previewBundle: (sessionId: string) =>
    req<BundlePreview>(
      "GET",
      `/api/contextkeeper/sessions/${encodeURIComponent(sessionId)}/preview`,
    ),
  approveBundle: (sessionId: string, bundleId: string, requestId: string) =>
    req<{ bundle_id: string; approved_at: string }>(
      "POST",
      `/api/contextkeeper/sessions/${encodeURIComponent(sessionId)}/approve`,
      { bundle_id: bundleId, request_id: requestId },
    ),
  launchRecovery: (sessionId: string, bundleId: string, requestId: string) =>
    req<RecoveryLaunch>(
      "POST",
      `/api/contextkeeper/sessions/${encodeURIComponent(sessionId)}/launch`,
      { bundle_id: bundleId, request_id: requestId },
    ),
  workSession: (workId: string) =>
    req<WorkSession>(
      "GET",
      `/api/contextkeeper/work/${encodeURIComponent(workId)}`,
    ),

  listDir: (path = "") =>
    req<FileEntry[]>("GET", `/api/dir?path=${encodeURIComponent(path)}`),
  tree: (path = "", depth = 1) =>
    req<TreeNode[]>(
      "GET",
      `/api/tree?path=${encodeURIComponent(path)}&depth=${depth}`,
    ),
  readFile: (path: string) =>
    req<FileRead>("GET", `/api/files?path=${encodeURIComponent(path)}`),
  writeFile: (path: string, content: string, create_parents = false) =>
    req<WriteFileResponse>("PUT", "/api/files", {
      path,
      content,
      create_parents,
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
    if (!res.ok) throw new ApiError(res.status, await res.text());
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
  searchFiles: (q: string, path = "", max?: number) =>
    req<FileSearchResult[]>(
      "GET",
      `/api/search/files?q=${encodeURIComponent(q)}&path=${encodeURIComponent(path)}${
        typeof max === "number" ? `&max=${max}` : ""
      }`,
    ),

  listAgentTasks: () => req<AgentTask[]>("GET", "/api/agent-tasks"),
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
    if (!res.ok) throw new ApiError(res.status, await res.text());
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
  pushTest: (title = "MyDevEnv2 test", body = "Push notifications are working.") =>
    req<{ ok: number; fail: number; queued: number }>("POST", "/api/push/test", { title, body }),
  flushPushDigests: () =>
    req<{ ok: number; fail: number; queued: number }>("POST", "/api/push/flush-digests"),

  assistantMessage: (text: string) =>
    req<AssistantReply>("POST", "/api/assistant/message", { text }),
  assistantAction: (id: string, approve: boolean) =>
    req<AssistantReply>("POST", `/api/assistant/actions/${id}`, { approve }),
  assistantHistory: () => req<AssistantHistory>("GET", "/api/assistant/history"),
  assistantReset: () => req<OkResponse>("POST", "/api/assistant/reset"),
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
  version: string;
  /** Build-time feature availability, e.g. `{ selkies: "1.6.2" | null }`. */
  features?: Record<string, string | null | undefined>;
  session_templates?: SessionTemplate[];
  /** True when the server has an assistant backend key provisioned. */
  assistant_enabled?: boolean;
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
}

export interface AssistantTranscriptEntry {
  role: "user" | "assistant";
  text: string;
  tool_trace?: string[];
}

export interface AssistantPendingAction {
  id: string;
  session_id: string;
  session_name: string;
  text: string;
  submit: boolean;
}

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
      if (!res.ok || !res.body) throw new Error(`SSE failed: ${res.status}`);
      const reader = res.body.getReader();
      const decoder = new TextDecoder();
      let buf = "";
      while (!cancelled) {
        const { value, done } = await reader.read();
        if (done) break;
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
