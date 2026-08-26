import { createSignal } from "solid-js";
import { createStore, produce } from "solid-js/store";
import type { ActivityState, SessionSummary, ServerEvent } from "./api";
import { api, subscribeEvents } from "./api";
import { getStoragePrefs } from "./storagePrefs";
import { noteForeground, onWake, reconcile, type Wake } from "./wakeCoordinator";

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
  const scrollbackBytes = getStoragePrefs().defaultSessionScrollbackBytes;
  const s = await api.createSession({
    name,
    command,
    cwd,
    env,
    scrollback_bytes: scrollbackBytes > 0 ? scrollbackBytes : undefined,
  });
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
let streamStarted = false;

function nextReconnectDelay(): number {
  // Exponential backoff with jitter, capped at ~30s. Starts at 1s.
  // Sustained outages with many open clients otherwise hit the server with
  // a thundering herd every 2s.
  const base = Math.min(30_000, 1_000 * 2 ** reconnectAttempts);
  reconnectAttempts = Math.min(reconnectAttempts + 1, 6);
  const jitter = Math.random() * base * 0.3;
  return Math.floor(base + jitter);
}

/** Callers waiting to hear that vogt-core changed (FR-U10). */
const vogtListeners = new Set<(seq: number) => void>();

/**
 * Subscribe to vogt-core's changes, as republished on the engine's stream.
 *
 * A surface gets a nudge and re-reads what it cares about; the event carries
 * no state, so there is nothing here for a view to render stale. Returns its
 * own unsubscribe, which every caller must use on cleanup — a board that
 * outlives its tab and keeps refetching is the failure this shape invites.
 */
export function onVogtChanged(listener: (seq: number) => void): () => void {
  vogtListeners.add(listener);
  return () => vogtListeners.delete(listener);
}

function notifyVogtChanged(seq: number): void {
  for (const listener of vogtListeners) listener(seq);
}

/** Callers waiting to hear that a session exited (a PTY the engine killed). */
const sessionKilledListeners = new Set<
  (id: string, exitCode: number | null) => void
>();

/**
 * Subscribe to session exits, as republished on the engine's stream.
 *
 * A surface that tracks a run's session — Agent Tasks, whose run rows say
 * "Still running" until the PTY ends — gets a nudge with the session id that
 * exited and re-reads what it owns. Returns its own unsubscribe, which every
 * caller must use on cleanup.
 */
export function onSessionKilled(
  listener: (id: string, exitCode: number | null) => void,
): () => void {
  sessionKilledListeners.add(listener);
  return () => sessionKilledListeners.delete(listener);
}

function notifySessionKilled(id: string, exitCode: number | null): void {
  for (const listener of sessionKilledListeners) listener(id, exitCode);
}

export function startEventStream(): void {
  streamStarted = true;
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
          notifySessionKilled(ev.id, ev.exit_code);
          break;
        case "activity":
          updateActivity(ev.id, ev.state);
          break;
        case "vogt-changed":
          // The Vogt surfaces subscribe to this themselves; the session store
          // has no opinion about a work item. Recorded here so the switch is
          // exhaustive and a future reader is not left wondering whether the
          // event was forgotten or ignored.
          notifyVogtChanged(ev.seq);
          break;
      }
    },
    () => {
      setConnected(false);
      unsubscribeEvents = null;
      noteForeground("sse-reconnect");
      const delay = nextReconnectDelay();
      reconnectTimer = setTimeout(() => {
        reconnectTimer = null;
        startEventStream();
      }, delay);
    },
  );
}

export function stopEventStream(): void {
  streamStarted = false;
  unsubscribeEvents?.();
  unsubscribeEvents = null;
  if (reconnectTimer !== null) {
    clearTimeout(reconnectTimer);
    reconnectTimer = null;
  }
  reconnectAttempts = 0;
}

// Android frequently lets the SSE connection go silently dead on
// backgrounding without ever firing an error, so the stream can look
// "connected" while stale indefinitely. Force a reconnect whenever the app
// comes back to the foreground, instead of waiting on error-driven backoff.
export function forceReconnectEventStream(): void {
  if (!streamStarted) return;
  reconnectAttempts = 0;
  if (unsubscribeEvents) {
    unsubscribeEvents();
    unsubscribeEvents = null;
  }
  if (reconnectTimer !== null) {
    clearTimeout(reconnectTimer);
    reconnectTimer = null;
  }
  startEventStream();
}

// Lifecycle ownership lives in wakeCoordinator: one reconnect and one session
// reconciliation for a burst of visibility/focus/resume events.
onWake(() => forceReconnectEventStream());
onWake((wake: Wake) => {
  if (wake.reason === "boot" && sessionsStore.ready) return;
  void reconcile("sessions", wake, () => refreshSessions());
});
