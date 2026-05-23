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
): Promise<SessionSummary> {
  const s = await api.createSession({ name, command });
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

export function startEventStream(): void {
  if (unsubscribeEvents) return;
  unsubscribeEvents = subscribeEvents(
    (ev: ServerEvent) => {
      setConnected(true);
      switch (ev.type) {
        case "session-created":
          // Refresh to pick up full summary fields (created_at etc.).
          void refreshSessions();
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
      // Auto-reconnect with a short backoff if the stream drops.
      unsubscribeEvents = null;
      setTimeout(() => startEventStream(), 2_000);
    },
  );
}

export function stopEventStream(): void {
  unsubscribeEvents?.();
  unsubscribeEvents = null;
}
