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
  /** Timestamp of the last successful list or stream answer. */
  lastAnswerAt: string | null;
}

const [store, setStore] = createStore<SessionsStore>({
  sessions: {},
  order: [],
  ready: false,
  lastAnswerAt: null,
});

const [error, setError] = createSignal<string | null>(null);
const [connected, setConnected] = createSignal<boolean>(false);

export const sessionsStore = store;
export const sessionsError = error;
export const isConnected = connected;

/**
 * How long the rail may sit on "…" with no answer of any kind before it says
 * so. The list itself gives up at the PWA's request deadline, so this is the
 * case that deadline cannot see: every list that was started got *aborted*
 * by a newer wake, or hung in a transport that never settled, and nothing
 * ever reported success or failure (#591). Past this, the badge reads
 * `unavailable` with a reason instead of an ellipsis, and a fresh list is
 * asked for.
 */
export const SESSION_LIST_STALL_MS = 15_000;
let stallTimer: ReturnType<typeof setTimeout> | null = null;

function disarmStallTimer(): void {
  if (stallTimer !== null) {
    clearTimeout(stallTimer);
    stallTimer = null;
  }
}

/** Arm once per "not yet ready" episode; a definitive answer disarms it. */
function armStallTimer(): void {
  if (stallTimer !== null || store.ready) return;
  stallTimer = setTimeout(() => {
    stallTimer = null;
    if (store.ready) return;
    setError(
      `The session list has not answered in ${Math.round(SESSION_LIST_STALL_MS / 1000)}s.`,
    );
    void refreshSessions();
  }, SESSION_LIST_STALL_MS);
}

/**
 * Whether the rail still needs a list: nothing has ever landed, or the last
 * attempt failed. Both are retried the moment the same origin answers on the
 * stream — the first because an aborted read (superseded by a newer wake, see
 * `wakeCoordinator.reconcile`) reports neither success nor failure and would
 * otherwise leave the badge on "…" for good (#591); the second because a list
 * that failed while the network was still waking is now likely to succeed.
 */
function sessionListPending(): boolean {
  return !store.ready || error() !== null;
}

export async function refreshSessions(signal?: AbortSignal): Promise<void> {
  armStallTimer();
  try {
    const list = await api.listSessions(signal);
    // Superseded by a newer read, not answered: say nothing, and leave the
    // stall timer armed so *some* list still has to land.
    if (signal?.aborted) return;
    disarmStallTimer();
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
    setStore("lastAnswerAt", new Date().toISOString());
  } catch (e) {
    if (signal?.aborted) return;
    disarmStallTimer();
    setError((e as Error).message);
    // A list that failed is reported as such; whether the front door is
    // reachable is the stream's to say while it is open (WI-77).
    if (!streamOpen) setConnected(false);
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

export function updateActivity(id: string, state: ActivityState, activityChangedAt?: string) {
  setStore(
    produce((s) => {
      const sess = s.sessions[id];
      if (sess) {
        sess.activity = state;
        if (activityChangedAt) sess.activity_changed_at = activityChangedAt;
      }
    }),
  );
}

function markAnswered(): void {
  setStore("lastAnswerAt", new Date().toISOString());
}

let unsubscribeEvents: (() => void) | null = null;
let reconnectAttempts = 0;
let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
let streamStarted = false;
/** The stream has been accepted and not yet lost — liveness the header can trust. */
let streamOpen = false;

/**
 * How long an open stream may stay silent before it is presumed dead. The
 * engine sends a `:ka` keep-alive every 15s, so this is three missed ones:
 * long enough that one delayed frame is not a reconnect, short enough that
 * Android's habit of letting a backgrounded socket die without an error
 * (see `forceReconnectEventStream`) is caught within a minute of resume even
 * when no lifecycle event fires.
 */
export const EVENT_STREAM_STALE_MS = 45_000;
let staleTimer: ReturnType<typeof setTimeout> | null = null;

function disarmStaleTimer(): void {
  if (staleTimer !== null) {
    clearTimeout(staleTimer);
    staleTimer = null;
  }
}

function armStaleTimer(): void {
  disarmStaleTimer();
  staleTimer = setTimeout(() => {
    staleTimer = null;
    if (!unsubscribeEvents) return;
    // Silence past the deadline: drop the stream and come straight back.
    // Straight back, not through the backoff — the stream *was* up, so
    // this is a dead socket, not a refusing server.
    unsubscribeEvents();
    unsubscribeEvents = null;
    streamOpen = false;
    setConnected(false);
    reconnectAttempts = 0;
    noteForeground("sse-reconnect");
    startEventStream();
  }, EVENT_STREAM_STALE_MS);
}

/**
 * The stream is up. That alone is the connection the header reports: a
 * quiet session emits no event for as long as it runs, and before this the
 * flag could only be set by an event or a session-list refresh — so a warm
 * open whose first refresh met a still-waking network (a tunnel coming back
 * after resume) showed "Disconnected" until something happened to change
 * state. A list that failed to load is retried here, now that the same
 * origin has just answered.
 */
function noteStreamAlive(): void {
  streamOpen = true;
  setConnected(true);
  reconnectAttempts = 0;
  markAnswered();
  armStaleTimer();
}

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
export type VogtChangedEvent = Extract<ServerEvent, { type: "vogt-changed" }>;
const vogtEventListeners = new Set<(event: VogtChangedEvent) => void>();

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

export function onVogtChangedEvent(
  listener: (event: VogtChangedEvent) => void,
): () => void {
  vogtEventListeners.add(listener);
  return () => vogtEventListeners.delete(listener);
}

function notifyVogtChanged(event: VogtChangedEvent): void {
  for (const listener of vogtListeners) listener(event.seq);
  for (const listener of vogtEventListeners) listener(event);
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
      noteStreamAlive();
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
          updateActivity(ev.id, ev.state, ev.activity_changed_at);
          break;
        case "vogt-changed":
          // The Vogt surfaces subscribe to this themselves; the session store
          // has no opinion about a work item. Recorded here so the switch is
          // exhaustive and a future reader is not left wondering whether the
          // event was forgotten or ignored.
          notifyVogtChanged(ev);
          break;
      }
    },
    () => {
      disarmStaleTimer();
      streamOpen = false;
      setConnected(false);
      unsubscribeEvents = null;
      noteForeground("sse-reconnect");
      const delay = nextReconnectDelay();
      reconnectTimer = setTimeout(() => {
        reconnectTimer = null;
        startEventStream();
      }, delay);
    },
    {
      onOpen: () => {
        noteStreamAlive();
        // The first load is the boot wake's; this retries a list that failed
        // — or one that never landed at all — now that the same origin has
        // just answered (#591).
        if (sessionListPending()) void refreshSessions();
      },
      onHeartbeat: noteStreamAlive,
    },
  );
}

export function stopEventStream(): void {
  streamStarted = false;
  streamOpen = false;
  disarmStaleTimer();
  disarmStallTimer();
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
  streamOpen = false;
  disarmStaleTimer();
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
  void reconcile("sessions", wake, (signal) => refreshSessions(signal));
});
