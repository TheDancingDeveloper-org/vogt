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

export const api = {
  listSessions: () => req<SessionSummary[]>("GET", "/api/sessions"),
  createSession: (s: CreateSessionRequest) =>
    req<SessionSummary>("POST", "/api/sessions", s),
  getSession: (id: string) =>
    req<SessionDetail>("GET", `/api/sessions/${id}`),
  renameSession: (id: string, name: string) =>
    req<{ ok: boolean }>("PATCH", `/api/sessions/${id}`, { name }),
  killSession: (id: string) =>
    req<{ ok: boolean }>("POST", `/api/sessions/${id}/kill`),
  deleteSession: (id: string) =>
    req<{ ok: boolean }>("DELETE", `/api/sessions/${id}`),
  health: () => req<{ ok: boolean }>("GET", "/healthz"),
};

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
