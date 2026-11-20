import { createSignal } from "solid-js";
import { createStore, produce, reconcile as reconcileStore } from "solid-js/store";
import type {
  ActivityState,
  CreateSessionRequest,
  SessionSummary,
  ServerEvent,
} from "./api";
import { api, subscribeEvents } from "./api";
import { recordTransportSuccess } from "./connectionHealth";
import { getStoragePrefs } from "./storagePrefs";
import { railSections } from "./railSections";
import { noteForeground, onWake, reconcile, type Wake } from "./wakeCoordinator";

interface SessionsStore {
  sessions: Record<string, SessionSummary>;
  order: string[];
  /** How many the engine left out of the default list (WI-1289). */
  hiddenCount: number;
  /** True once we've loaded the initial list at least once. */
  ready: boolean;
  /** Timestamp of the last successful list or stream answer. */
  lastAnswerAt: string | null;
}

const [store, setStore] = createStore<SessionsStore>({
  sessions: {},
  order: [],
  hiddenCount: 0,
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
 * ever reported success or failure. Past this, the badge reads
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
    // Asked directly, not through the coalescer: the read it would join
    // may be the one that never answered.
    void loadSessions();
  }, SESSION_LIST_STALL_MS);
}

/**
 * Whether the rail still needs a list: nothing has ever landed, or the last
 * attempt failed. Both are retried the moment the same origin answers on the
 * stream — the first because an aborted read (superseded by a newer wake, see
 * `wakeCoordinator.reconcile`) reports neither success nor failure and would
 * otherwise leave the badge on "…" for good; the second because a list
 * that failed while the network was still waking is now likely to succeed.
 */
function sessionListPending(): boolean {
  return !store.ready || error() !== null;
}

/** The unsignalled list read in flight, shared by every caller meanwhile. */
let sessionsInFlight: Promise<void> | null = null;
/** A trigger arrived while a read was in flight: read once more after it. */
let sessionsRerun = false;

/**
 * Read the session list again. Unsignalled callers (stream events, panels)
 * are coalesced: one read in flight at a time, and any number of triggers
 * during it collapse into a single follow-up read (WI-1413). A signalled
 * read belongs to the wake coordinator, which supersedes by aborting, so it
 * runs as asked.
 */
export function refreshSessions(signal?: AbortSignal): Promise<void> {
  if (signal) return loadSessions(signal);
  if (sessionsInFlight) {
    sessionsRerun = true;
    return sessionsInFlight;
  }
  sessionsInFlight = (async () => {
    try {
      do {
        sessionsRerun = false;
        await loadSessions();
      } while (sessionsRerun);
    } finally {
      sessionsInFlight = null;
    }
  })();
  return sessionsInFlight;
}

/** How long a `lagged` waits for more of its kind before one re-read. */
export const LAGGED_REFRESH_DEBOUNCE_MS = 1000;
let laggedTimer: ReturnType<typeof setTimeout> | null = null;

function refreshAfterLag(): void {
  if (laggedTimer !== null) clearTimeout(laggedTimer);
  laggedTimer = setTimeout(() => {
    laggedTimer = null;
    void refreshSessions();
  }, LAGGED_REFRESH_DEBOUNCE_MS);
}

/** The rail list is the slim view (WI-2692): it leaves out `command`,
 *  `economics` and `conversation`. Keep what an earlier full read (create,
 *  detail) put on the row rather than letting the merge erase it. */
export function withKnownDetail(sess: SessionSummary, known: SessionSummary | undefined): SessionSummary {
  if (!known) return sess;
  return {
    ...sess,
    command: sess.command ?? known.command,
    conversation: sess.conversation ?? known.conversation,
  };
}

async function loadSessions(signal?: AbortSignal): Promise<void> {
  armStallTimer();
  try {
    const list = await api.listSessions(signal, railSections.showHidden);
    // Superseded by a newer read, not answered: say nothing, and leave the
    // stall timer armed so *some* list still has to land.
    if (signal?.aborted) return;
    disarmStallTimer();
    // Merged, not replaced: a poll that answers what the rail already shows
    // keeps every row's identity, so nothing re-renders. Clearing the map and
    // rebuilding it handed every row a new object on every read, and with a
    // busy roster that re-laid-out the whole rail each second (WI-1903).
    const next: Record<string, SessionSummary> = {};
    const ids: string[] = [];
    for (const sess of list.sessions) {
      next[sess.id] = withKnownDetail(sess, store.sessions[sess.id]);
      ids.push(sess.id);
    }
    setStore("sessions", reconcileStore(next, { key: "id", merge: true }));
    setStore("order", reconcileStore(ids));
    setStore("hiddenCount", list.hidden_count);
    setStore("ready", true);
    setError(null);
    markConnected();
    setStore("lastAnswerAt", new Date().toISOString());
  } catch (e) {
    if (signal?.aborted) return;
    disarmStallTimer();
    setError((e as Error).message);
    // A list that failed is reported as such; whether the front door is
    // reachable is the stream's to say while it is open (WI-77).
    if (!streamOpen) markDisconnected();
  }
}

export async function createSession(
  name: string,
  command?: string[],
  cwd?: string,
  env?: [string, string][],
  launch?: Pick<CreateSessionRequest, "template" | "resume" | "role" | "work_item">,
): Promise<SessionSummary> {
  const scrollbackBytes = getStoragePrefs().defaultSessionScrollbackBytes;
  const s = await api.createSession({
    name,
    command,
    cwd,
    env,
    ...launch,
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

/**
 * A stream with no frame for this long is presumed down, so a foreground
 * wake may force a reconnect. A healthy stream heartbeats every 15 s, so
 * this is one keep-alive of slack past a normal gap.
 */
export const EVENT_STREAM_QUIET_MS = 20_000;
/**
 * How long a lost stream may be out before the header says "Disconnected".
 * A handoff between networks is usually over inside this, and a flash to
 * stale-and-back read as a broken app; the actions stay enabled meanwhile.
 */
export const DISCONNECT_GRACE_MS = 2_500;

/**
 * How long a reopened stream must stay up before it counts as back (WI-1903).
 * Until then it neither resets the reconnect backoff, nor wakes the app to
 * re-read every surface, nor clears "Disconnected". A stream lost moments
 * after each open used to do all three on every open: the backoff never grew
 * past its first second, each reopen aborted and refired every list, and the
 * banner flipped on and off every two seconds.
 */
export const STREAM_SETTLE_MS = 1_500;
/** When the open stream was accepted, or null while there is none. */
let openedAt: number | null = null;
let settleTimer: ReturnType<typeof setTimeout> | null = null;

function streamSettled(): boolean {
  return streamOpen && openedAt !== null && Date.now() - openedAt >= STREAM_SETTLE_MS;
}

function disarmSettleTimer(): void {
  if (settleTimer !== null) {
    clearTimeout(settleTimer);
    settleTimer = null;
  }
}

/** The stream is gone: it no longer counts toward a settle. */
function noteStreamGone(): void {
  streamOpen = false;
  openedAt = null;
  connectStartedAt = null;
  disarmSettleTimer();
}

let disconnectTimer: ReturnType<typeof setTimeout> | null = null;
/** Last frame or keep-alive from the stream, for the quiet check. */
let lastAliveAt = 0;
/** When the stream was last lost and not yet reopened. */
let lostAt: number | null = null;
/**
 * A drop at least this long re-reads the session list on reconnect; a shorter
 * one leaves it to the stream (WI-1254).
 */
export const RECONNECT_GAP_MS = 10_000;
/** The settled reopen's verdict, consumed by the next `sse-reconnect` wake. */
let reconnectGap = false;
/** When the stream last spoke before the current loss. */
let lostSince: number | null = null;

/** Show "Disconnected" only once the loss has outlasted the grace period. */
function markDisconnected(): void {
  if (disconnectTimer !== null || !connected()) return;
  disconnectTimer = setTimeout(() => {
    disconnectTimer = null;
    setConnected(false);
  }, DISCONNECT_GRACE_MS);
}

function markConnected(): void {
  if (disconnectTimer !== null) {
    clearTimeout(disconnectTimer);
    disconnectTimer = null;
  }
  if (connected()) return;
  // Already showing "Disconnected": only a stream that has stayed up clears
  // it, so a stream that keeps dropping does not flip the banner per open.
  // The settle timer calls back here once it has.
  // A first open (nothing lost yet) connects at once, so boot does not flash.
  if (streamStarted && lostAt !== null && !streamSettled()) return;
  setConnected(true);
}

function markLost(): void {
  if (lostAt !== null) return;
  lostAt = Date.now();
  // A socket that died quietly was gone since it last spoke, not since now.
  lostSince = lastAliveAt > 0 ? Math.min(lastAliveAt, lostAt) : lostAt;
}

/** The stream is open and has spoken recently: no reconnect needed. */
function streamHealthy(): boolean {
  return (
    streamOpen
    && unsubscribeEvents !== null
    && Date.now() - lastAliveAt < EVENT_STREAM_QUIET_MS
  );
}

/** When the pending connect was started, or null when none is pending. */
let connectStartedAt: number | null = null;

/**
 * A connect is out and has not had its answer yet, for less than the quiet
 * window. A wake leaves it alone: aborting it only starts the same wait over.
 * That was the WI-2324 loop — a connect held by the browser (a second tab's
 * stream holding Firefox's cache entry for the URL) never opened, so every
 * focus edge found the stream "unhealthy", tore the connect down and sent
 * another, a few seconds apart for as long as the page was used. A connect
 * that never answers is dropped by the stale timer instead.
 */
function streamConnecting(): boolean {
  return (
    !streamOpen
    && unsubscribeEvents !== null
    && connectStartedAt !== null
    && Date.now() - connectStartedAt < EVENT_STREAM_QUIET_MS
  );
}

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
    noteStreamGone();
    markDisconnected();
    reconnectAttempts = 0;
    // Not a wake: the reopen answers it (see onOpen).
    markLost();
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
  lastAliveAt = Date.now();
  if (openedAt === null) openedAt = lastAliveAt;
  markConnected();
  // The backoff resets only for a stream that has proven itself; see
  // STREAM_SETTLE_MS.
  if (streamSettled()) reconnectAttempts = 0;
  markAnswered();
  armStaleTimer();
  // The stream is the one reader that keeps probing while the transport breaker
  // is open (its own bounded reconnect backoff is the probe cadence). A live
  // stream means the connection has been replaced, so close the breaker and let
  // the paused pollers resume (#681).
  recordTransportSuccess();
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

/** Callers waiting to hear that vogt-core changed. */
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

/**
 * How long a `vogt-changed` waits for the rest of its burst before the
 * listeners hear it (WI-1254). A transition fans out several core events;
 * each one re-read every open surface. Each event restarts the wait, up to
 * `VOGT_CHANGED_MAX_WAIT_MS` after the burst's first event.
 */
export const VOGT_CHANGED_COALESCE_MS = 150;
export const VOGT_CHANGED_MAX_WAIT_MS = 1_000;
let vogtTimer: ReturnType<typeof setTimeout> | null = null;
let vogtBurstStartedAt = 0;
/** The burst so far: the latest event per entity and kind, in arrival order. */
const vogtPending = new Map<string, VogtChangedEvent>();

function notifyVogtChanged(event: VogtChangedEvent): void {
  const key = `${event.kind}\u0000${event.entity_kind}\u0000${event.entity_id}`;
  vogtPending.delete(key);
  vogtPending.set(key, event);
  const now = Date.now();
  if (vogtTimer === null) vogtBurstStartedAt = now;
  else clearTimeout(vogtTimer);
  const left = vogtBurstStartedAt + VOGT_CHANGED_MAX_WAIT_MS - now;
  vogtTimer = setTimeout(flushVogtChanged, Math.max(0, Math.min(VOGT_CHANGED_COALESCE_MS, left)));
}

function flushVogtChanged(): void {
  vogtTimer = null;
  const events = [...vogtPending.values()];
  vogtPending.clear();
  if (events.length === 0) return;
  // A seq listener re-reads everything it shows, so one nudge per burst.
  const seq = Math.max(...events.map((e) => e.seq));
  for (const listener of [...vogtListeners]) listener(seq);
  // An event listener reads the payload, so it hears each distinct change.
  for (const event of events) {
    for (const listener of [...vogtEventListeners]) listener(event);
  }
}

function cancelVogtChanged(): void {
  if (vogtTimer !== null) clearTimeout(vogtTimer);
  vogtTimer = null;
  vogtPending.clear();
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
  connectStartedAt = Date.now();
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
        case "session-resources":
          setStore(
            produce((s) => {
              for (const sample of ev.samples) {
                const sess = s.sessions[sample.id];
                if (sess) sess.resources = sample.resources;
              }
            }),
          );
          break;
        case "lagged":
          // This stream missed events; the list it maintains may be wrong
          // anywhere, so read it whole again — once per burst, not per lag.
          refreshAfterLag();
          break;
        case "session-hibernated":
          // Patched in place like `session-blocked`: a re-read per event
          // turned a burst into a storm of full lists (WI-1413).
          setStore(
            produce((s) => {
              const sess = s.sessions[ev.id];
              if (!sess) return;
              sess.activity = "hibernated";
              sess.hibernation = {
                at: new Date().toISOString(),
                trigger: ev.trigger,
                resumable: sess.hibernation?.resumable ?? sess.conversation != null,
              };
            }),
          );
          break;
        case "session-woken":
          // The next `activity` event says what it is doing now.
          setStore(
            produce((s) => {
              const sess = s.sessions[ev.id];
              if (!sess) return;
              sess.hibernation = null;
              if (sess.activity === "hibernated") sess.activity = "idle";
            }),
          );
          break;
        case "session-blocked":
          setStore(
            produce((s) => {
              const sess = s.sessions[ev.id];
              if (sess) sess.blocked = ev.blocked ?? null;
            }),
          );
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
      // A stream that stayed up was a healthy connection that broke: start
      // the backoff over. One that died inside the settle window keeps
      // growing it.
      if (streamSettled()) reconnectAttempts = 0;
      noteStreamGone();
      markDisconnected();
      unsubscribeEvents = null;
      // Every failed retry lands here. Waking the app on each one refetched
      // every surface against a server that was still away, so the loss is
      // only noted; the reopen decides whether the app needs a wake.
      markLost();
      const delay = nextReconnectDelay();
      reconnectTimer = setTimeout(() => {
        reconnectTimer = null;
        startEventStream();
      }, delay);
    },
    {
      onOpen: () => {
        connectStartedAt = null;
        openedAt = null;
        noteStreamAlive();
        disarmSettleTimer();
        if (lostAt === null) {
          // A first open lost nothing, so there is no wake to hold back: the
          // first load is the boot wake's, and this only retries a list that
          // failed — or never landed — now that the same origin has answered.
          if (sessionListPending()) void refreshSessions();
          return;
        }
        // Nothing below runs until the stream has stayed up: a reopen that
        // drops again at once must not wake every surface (WI-1903). The
        // loss stays recorded, so whichever open does settle answers it.
        settleTimer = setTimeout(() => {
          settleTimer = null;
          if (!streamSettled()) return;
          reconnectAttempts = 0;
          const droppedFor = lostSince === null ? 0 : Date.now() - lostSince;
          markConnected();
          lostAt = null;
          lostSince = null;
          // Events sent while the stream was down are gone, so the open
          // surfaces are read again: one wake for the whole drop. The session
          // list is re-read only for a drop long enough to have hidden a
          // create or remove; a blip (Firefox churns its stream every few
          // seconds) re-reading the whole list each time was the storm in
          // WI-1254. A `lagged` still answers a gap the engine reports.
          reconnectGap = droppedFor >= RECONNECT_GAP_MS;
          noteForeground("sse-reconnect");
        }, STREAM_SETTLE_MS);
      },
      onHeartbeat: noteStreamAlive,
    },
  );
  // The connect is held to the same deadline as a silent stream: one that
  // never answers is dropped and sent again, not waited on for good. An open
  // (or a failure) re-arms or clears this, and the demo opens synchronously.
  if (unsubscribeEvents !== null && !streamOpen) armStaleTimer();
}

export function stopEventStream(): void {
  streamStarted = false;
  noteStreamGone();
  lostAt = null;
  lostSince = null;
  reconnectGap = false;
  disarmStaleTimer();
  disarmStallTimer();
  unsubscribeEvents?.();
  unsubscribeEvents = null;
  if (reconnectTimer !== null) {
    clearTimeout(reconnectTimer);
    reconnectTimer = null;
  }
  if (disconnectTimer !== null) {
    clearTimeout(disconnectTimer);
    disconnectTimer = null;
  }
  if (laggedTimer !== null) {
    clearTimeout(laggedTimer);
    laggedTimer = null;
  }
  cancelVogtChanged();
  reconnectAttempts = 0;
}

// Android frequently lets the SSE connection go silently dead on
// backgrounding without ever firing an error, so the stream can look
// "connected" while stale indefinitely. Force a reconnect whenever the app
// comes back to the foreground, instead of waiting on error-driven backoff.
export function forceReconnectEventStream(): void {
  if (!streamStarted) return;
  reconnectAttempts = 0;
  noteStreamGone();
  disarmStaleTimer();
  // Events sent while this stream is torn down are lost like any drop's, so
  // the reopen reads the lists again.
  markLost();
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

/**
 * A page hidden at least this long is read again on its return even while
 * its stream looks healthy. The stream would have carried every change, so
 * this is only a backstop for a socket that died quietly within the quiet
 * window, at one list read per real return rather than one per focus edge.
 */
export const RECONCILE_AFTER_HIDDEN_MS = 60_000;

/**
 * Whether a wake has to read the session list again. The stream patches the
 * list in place from its events, so while it is healthy the list is current
 * and a focus edge has nothing to add. A connect in flight is answered by its
 * own reopen (`sse-reconnect`, or the first-open retry), so a read here would
 * only be a second copy of that one. Before WI-2324 every focus edge — focus
 * moving between the page, a terminal and DevTools — re-read the whole list,
 * 208 KB a time on a busy deployment.
 */
function wakeNeedsSessionList(wake: Wake): boolean {
  if (wake.reason === "boot") return !sessionsStore.ready;
  if (wake.reason === "sse-reconnect") {
    const gap = reconnectGap;
    reconnectGap = false;
    return gap || sessionListPending();
  }
  if (sessionListPending()) return true;
  if (wake.reason === "focus" || wake.reason === "visibility") {
    if (wake.hiddenMs >= RECONCILE_AFTER_HIDDEN_MS) return true;
    return !(streamHealthy() || streamConnecting());
  }
  return true;
}

// Lifecycle ownership lives in wakeCoordinator: one reconnect and one session
// reconciliation for a burst of visibility/focus/resume events.
//
// A wake is not answered with a forced reconnect when the stream is already
// healthy or still connecting, or when the wake is `boot` (the stream was just
// opened; tearing it down 250 ms later was a second handshake on the startup
// path). A wake the stream raised about its own loss is never answered with
// one either: the stream is already retrying on its backoff. Android's silent
// socket death is still caught: that stream stops speaking, so after the quiet
// window it is no longer healthy and the first wake reconnects it.
//
// The session list is decided after the reconnect, so a focus edge that had
// to reconnect leaves the read to that reopen's own wake instead of reading
// the list twice.
onWake((wake: Wake) => {
  if (wake.reason !== "sse-reconnect" && wake.reason !== "boot"
    && !streamHealthy() && !streamConnecting()) {
    forceReconnectEventStream();
  }
  if (!wakeNeedsSessionList(wake)) return;
  void reconcile("sessions", wake, (signal) => refreshSessions(signal));
});
