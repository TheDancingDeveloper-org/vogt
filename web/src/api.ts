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
  created_at: string;
}

export interface CreateSessionRequest {
  name: string;
  command?: string[];
  cwd?: string;
  cols?: number;
  rows?: number;
}

export interface SessionDetail {
  summary: SessionSummary;
  scrollback_pos: number;
  scrollback_base64: string;
}

export type ServerEvent =
  | { type: "session-created"; id: string; name: string }
  | { type: "session-renamed"; id: string; name: string }
  | { type: "session-killed"; id: string; exit_code: number | null }
  | { type: "activity"; id: string; state: ActivityState };

const TOKEN_KEY = "mydevenv2.token";
const BASE_KEY = "mydevenv2.base";

export function getToken(): string {
  return localStorage.getItem(TOKEN_KEY) ?? "";
}

export function setToken(token: string) {
  if (token) localStorage.setItem(TOKEN_KEY, token);
  else localStorage.removeItem(TOKEN_KEY);
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

export interface SearchHit {
  path: string;
  line: number;
  text: string;
}

export const api = {
  listSessions: () => req<SessionSummary[]>("GET", "/api/sessions"),
  createSession: (s: CreateSessionRequest) =>
    req<SessionSummary>("POST", "/api/sessions", s),
  getSession: (id: string) => req<SessionDetail>("GET", `/api/sessions/${id}`),
  renameSession: (id: string, name: string) =>
    req<{ ok: boolean }>("PATCH", `/api/sessions/${id}`, { name }),
  killSession: (id: string) =>
    req<{ ok: boolean }>("POST", `/api/sessions/${id}/kill`),
  deleteSession: (id: string) =>
    req<{ ok: boolean }>("DELETE", `/api/sessions/${id}`),
  health: () => req<{ ok: boolean }>("GET", "/healthz"),

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
    req<{ ok: boolean; bytes: number }>("PUT", "/api/files", {
      path,
      content,
      create_parents,
    }),
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

  // Public — no token required.
  publicConfig: () =>
    fetch(`${getBase()}/api/config`).then((r) => r.json() as Promise<PublicConfig>),

  guiLaunch: (command: string[], via_sway = true) =>
    req<GuiProc>("POST", "/api/gui/launch", { command, via_sway }),
  guiProcesses: () => req<GuiProc[]>("GET", "/api/gui/processes"),
  guiKill: (pid: number) =>
    req<{ ok: boolean }>("POST", `/api/gui/kill?pid=${pid}`),

  pushTest: (title = "MyDevEnv2 test", body = "Push notifications are working.") =>
    req<{ ok: number; fail: number }>("POST", "/api/push/test", { title, body }),
};

export interface PublicConfig {
  gui_stream_url: string | null;
  version: string;
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
 * Open a WebSocket attached to a session. Token via query string because
 * browsers cannot set Authorization on a WS handshake.
 */
export function openAttach(id: string): WebSocket {
  const base = getBase() || `${location.protocol}//${location.host}`;
  const wsBase = base.replace(/^http/, "ws");
  const tok = encodeURIComponent(getToken());
  const ws = new WebSocket(`${wsBase}/api/sessions/${id}/attach?token=${tok}`);
  ws.binaryType = "arraybuffer";
  return ws;
}
