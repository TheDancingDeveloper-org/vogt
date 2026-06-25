import { createSignal } from "solid-js";
import { createStore, produce } from "solid-js/store";
import type { ActivityState, SessionSummary, ServerEvent } from "./api";
import { api, subscribeEvents } from "./api";

interface SessionsStore {
  sessions: Record<string, SessionSummary>;
  order: string[];
  /** True once we've loaded the initial list at least once. */
  ready: boolean;
}

const [store, setStore] = createStore<SessionsStore>({
  sessions: {},
  order: [],
  ready: false,
});

const [error, setError] = createSignal<string | null>(null);
const [connected, setConnected] = createSignal<boolean>(false);

export const sessionsStore = store;
export const sessionsError = error;
export const isConnected = connected;

export async function refreshSessions(): Promise<void> {
  try {
    const list = await api.listSessions();
    setStore(
      produce((s) => {
        s.sessions = {};
        s.order = [];
        for (const sess of list) {
          s.sessions[sess.id] = sess;
          s.order.push(sess.id);
        }
        s.ready = true;
      }),
    );
    setError(null);
    setConnected(true);
  } catch (e) {
    setError((e as Error).message);
    setConnected(false);
  }
}

export async function createSession(
  name: string,
  command?: string[],
  cwd?: string,
  env?: [string, string][],
): Promise<SessionSummary> {
  const s = await api.createSession({ name, command, cwd, env });
  setStore(
    produce((st) => {
      st.sessions[s.id] = s;
      if (!st.order.includes(s.id)) st.order.push(s.id);
    }),
  );
  return s;
}

export async function killSession(id: string): Promise<void> {
  await api.killSession(id);
}

export async function deleteSession(id: string): Promise<void> {
  await api.deleteSession(id);
  setStore(
    produce((s) => {
      delete s.sessions[id];
      s.order = s.order.filter((x) => x !== id);
    }),
  );
}

export async function renameSession(id: string, name: string): Promise<void> {
  await api.renameSession(id, name);
  setStore(
    produce((s) => {
      const sess = s.sessions[id];
      if (sess) sess.name = name;
    }),
  );
}

export function updateActivity(id: string, state: ActivityState) {
  setStore(
    produce((s) => {
      const sess = s.sessions[id];
      if (sess) sess.activity = state;
    }),
  );
}

let unsubscribeEvents: (() => void) | null = null;
let reconnectAttempts = 0;
let reconnectTimer: ReturnType<typeof setTimeout> | null = null;

function nextReconnectDelay(): number {
  // Exponential backoff with jitter, capped at ~30s. Starts at 1s.
  // Sustained outages with many open clients otherwise hit the server with
  // a thundering herd every 2s.
  const base = Math.min(30_000, 1_000 * 2 ** reconnectAttempts);
  reconnectAttempts = Math.min(reconnectAttempts + 1, 6);
  const jitter = Math.random() * base * 0.3;
  return Math.floor(base + jitter);
}

export function startEventStream(): void {
  if (unsubscribeEvents) return;
  if (reconnectTimer !== null) {
    clearTimeout(reconnectTimer);
    reconnectTimer = null;
  }
  unsubscribeEvents = subscribeEvents(
    (ev: ServerEvent) => {
      setConnected(true);
      reconnectAttempts = 0;
      switch (ev.type) {
        case "session-created":
          // Skip the refetch if we already know this id (the local create
          // already inserted it). Only refresh for sessions that some *other*
          // client made.
          if (!store.sessions[ev.id]) void refreshSessions();
          break;
        case "session-renamed":
          setStore(
            produce((s) => {
              const sess = s.sessions[ev.id];
              if (sess) sess.name = ev.name;
            }),
          );
          break;
        case "session-killed":
          setStore(
            produce((s) => {
              const sess = s.sessions[ev.id];
              if (sess) sess.exit_code = ev.exit_code;
            }),
          );
          break;
        case "activity":
          updateActivity(ev.id, ev.state);
          break;
      }
    },
    () => {
      setConnected(false);
      unsubscribeEvents = null;
      const delay = nextReconnectDelay();
      reconnectTimer = setTimeout(() => {
        reconnectTimer = null;
        startEventStream();
      }, delay);
    },
  );
}

export function stopEventStream(): void {
  unsubscribeEvents?.();
  unsubscribeEvents = null;
  if (reconnectTimer !== null) {
    clearTimeout(reconnectTimer);
    reconnectTimer = null;
  }
  reconnectAttempts = 0;
}
