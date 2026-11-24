// Which engine session started which, for the rail's and the session list's
// oversight nesting (WI-1736).
//
// The engine's own session list does not say who started a session; Vogt's
// `session.list` does (`started_by_session`, WI-1323), keyed by the engine id
// it links each session to (`engine_session_id`). Asked for those two fields
// only, the core answers from its declared table, with no engine call and no
// transcript read (WI-4531).
//
// The map is read in full once, then grown from the change feed (WI-4532):
// when the engine's session set gains ids, the newest page is read and merged
// in. A full read happens again only on a stream reconnect, or when the newest
// page cannot cover every new id. Stopped sessions are never re-read on a
// timer. Tabs in one browser share one reader: a leader tab reads and
// broadcasts the map, the others only listen.
//
// No core, or a read that fails, leaves the map as it was: the lists then show
// the sessions flat, the behaviour before nesting existed.

import { createSignal } from "solid-js";
import { listSessions, VogtBusy, VogtUnavailable, type SessionSummary as VogtSessionSummary } from "./vogtApi";
import { onWake } from "./wakeCoordinator";

/** Coalesces a burst of session-list changes into one read. */
const DEBOUNCE_MS = 750;
/** How long a new tab waits for a leader to answer before leading itself. */
export const LEADER_WAIT_MS = 300;
/** Links per page: Vogt's cap for `session.list`. */
export const STARTERS_PAGE_SIZE = 500;
/** Pages read at most (2000 links). Past that, nesting covers what was read. */
export const STARTERS_MAX_PAGES = 4;
/** Links read when new sessions appear: the newest page, newest first. */
export const STARTERS_DELTA_SIZE = 50;
/** How often the leader tab says it is alive. */
export const HEARTBEAT_MS = 5_000;
/** A follower leads itself when the leader has been quiet this long (a tab
 *  frozen or killed without a `bye`). */
export const HEARTBEAT_TIMEOUT_MS = 15_000;
/** Reads that may come back without an id before it is given up on: a session
 *  with no Vogt link never gets one, so the retry must end. */
export const MISSING_ID_RETRIES = 3;
/** The fields that keep `session.list` on the core's cheap starters path. */
export const STARTER_FIELDS = ["engine_session_id", "started_by_session"] as const;
const CHANNEL_NAME = "vogt.railStarters";

/** Engine session id → the engine session id of its starter, for every row
 *  Vogt links that names a starter. Pure, so the mapping is testable. */
export function startersById(sessions: readonly VogtSessionSummary[]): Map<string, string> {
  const starters = new Map<string, string>();
  for (const session of sessions) {
    const child = session.engine_session_id;
    const starter = session.started_by_session;
    if (child && starter) starters.set(child, starter);
  }
  return starters;
}

export interface StartersPage {
  sessions: VogtSessionSummary[];
  truncated?: boolean;
}

/** Read every page of the session list that names a starter. `fetchPage` is
 *  given the offset to ask for. Pure apart from that call, so the paging is
 *  testable without a core. */
export async function readAllStarters(
  fetchPage: (offset: number, limit: number) => Promise<StartersPage>,
  pageSize: number = STARTERS_PAGE_SIZE,
): Promise<Map<string, string>> {
  const starters = new Map<string, string>();
  let offset = 0;
  for (let page = 0; page < STARTERS_MAX_PAGES; page += 1) {
    const answer = await fetchPage(offset, pageSize);
    for (const [child, starter] of startersById(answer.sessions)) starters.set(child, starter);
    // A short page is the last one, whatever `truncated` says: the core
    // reports it from the total, so the page after offset 500 of 506 used to
    // be asked for as well — a fourth full read every refresh (WI-1903).
    if (!answer.truncated || answer.sessions.length < pageSize) break;
    offset += answer.sessions.length;
  }
  return starters;
}

/** The one call the reader makes: the starters path of `session.list`. */
export const fetchStartersPage = (offset: number, limit: number): Promise<StartersPage> =>
  listSessions({
    mode: "full",
    starters_only: true,
    fields: [...STARTER_FIELDS],
    include_stopped: true,
    limit,
    offset,
  });

/** The slice of BroadcastChannel the reader uses, so tests can fake it. */
export interface StartersChannel {
  postMessage(message: unknown): void;
  onmessage: ((event: { data: unknown }) => void) | null;
}

type Message =
  | { type: "hello" }
  | { type: "leader" }
  | { type: "bye" }
  | { type: "beat" }
  | { type: "map"; entries: [string, string][] };

export interface StartersReaderDeps {
  fetchPage: (offset: number, limit: number) => Promise<StartersPage>;
  channel: StartersChannel | null;
  onMap: (map: ReadonlyMap<string, string>) => void;
  debounceMs?: number;
  leaderWaitMs?: number;
  heartbeatMs?: number;
  heartbeatTimeoutMs?: number;
}

/** The reader behind `railStarters()`. Exported for tests: two of these on one
 *  fake channel are two tabs. */
export function createStartersReader(deps: StartersReaderDeps) {
  const debounceMs = deps.debounceMs ?? DEBOUNCE_MS;
  const leaderWaitMs = deps.leaderWaitMs ?? LEADER_WAIT_MS;
  const heartbeatMs = deps.heartbeatMs ?? HEARTBEAT_MS;
  const heartbeatTimeoutMs = deps.heartbeatTimeoutMs ?? HEARTBEAT_TIMEOUT_MS;
  let map = new Map<string, string>();
  /** Engine ids seen in the session set so far. */
  const known = new Set<string>();
  /** Ids seen since the last read that the map has not been checked against. */
  let fresh = new Set<string>();
  let role: "unknown" | "electing" | "leader" | "follower" = "unknown";
  let loadedOnce = false;
  let fullWanted = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let electTimer: ReturnType<typeof setTimeout> | undefined;
  /** The leader's beat while this tab leads. */
  let beatTimer: ReturnType<typeof setInterval> | undefined;
  /** The follower's watch on the leader's beat. */
  let watchdog: ReturnType<typeof setTimeout> | undefined;
  /** Reads an id has been waited on without turning up in the delta. */
  const missed = new Map<string, number>();
  let loading = false;
  let pending = false;

  const post = (message: Message) => deps.channel?.postMessage(message);
  const publish = () => {
    deps.onMap(new Map(map));
    post({ type: "map", entries: [...map] });
  };

  const stopTimers = () => {
    clearInterval(beatTimer);
    clearTimeout(watchdog);
    beatTimer = undefined;
    watchdog = undefined;
  };

  /** A follower hears the leader: any message from it resets the watch. If
   *  the leader goes quiet (its tab froze or died with no `bye`), lead. */
  const watchLeader = () => {
    clearTimeout(watchdog);
    watchdog = setTimeout(() => {
      if (role !== "follower") return;
      role = "unknown";
      loadedOnce = false;
      elect();
    }, heartbeatTimeoutMs);
  };

  const becomeLeader = () => {
    stopTimers();
    role = "leader";
    post({ type: "leader" });
    beatTimer = setInterval(() => post({ type: "beat" }), heartbeatMs);
    schedule();
  };

  const elect = () => {
    if (role !== "unknown") return;
    if (!deps.channel) {
      becomeLeader();
      return;
    }
    role = "electing";
    post({ type: "hello" });
    electTimer = setTimeout(() => {
      if (role === "electing") becomeLeader();
    }, leaderWaitMs);
  };

  if (deps.channel) {
    deps.channel.onmessage = (event) => {
      const message = event.data as Message;
      switch (message?.type) {
        case "hello":
          if (role === "leader") {
            post({ type: "leader" });
            if (loadedOnce) post({ type: "map", entries: [...map] });
          }
          break;
        case "leader":
          if (role === "electing" || role === "unknown") {
            clearTimeout(electTimer);
            role = "follower";
            watchLeader();
          } else if (role === "follower") {
            watchLeader();
          }
          break;
        case "beat":
          if (role === "follower") watchLeader();
          break;
        case "map":
          if (role !== "leader") {
            if (role === "follower") watchLeader();
            map = new Map(message.entries);
            loadedOnce = true;
            deps.onMap(new Map(map));
          }
          break;
        case "bye":
          if (role === "follower") {
            // The leader tab closed: elect again, and the winner reads in full.
            stopTimers();
            role = "unknown";
            loadedOnce = false;
            elect();
          }
          break;
      }
    };
  }

  async function read(): Promise<void> {
    const full = !loadedOnce || fullWanted;
    const wanted = fresh;
    fresh = new Set();
    fullWanted = false;
    if (full) {
      map = await readAllStarters(deps.fetchPage);
      loadedOnce = true;
      missed.clear();
      publish();
      return;
    }
    // Grow the map from the newest page. A session that names no starter has
    // no row worth keeping, so "covered" means the page reached past it.
    const page = await deps.fetchPage(0, STARTERS_DELTA_SIZE);
    for (const [child, starter] of startersById(page.sessions)) map.set(child, starter);
    const seen = new Set(page.sessions.map((s) => s.engine_session_id).filter(Boolean));
    for (const id of seen) if (id) missed.delete(id);
    const absent = [...wanted].filter((id) => !seen.has(id) && !map.has(id));
    if (absent.length > 0 && page.truncated) {
      // More new sessions than one page, or an id the newest page does not
      // hold: one full read settles it.
      map = await readAllStarters(deps.fetchPage);
      missed.clear();
    } else {
      retryMissing(absent.filter((id) => !map.has(id)));
    }
    publish();
  }

  /** A child's link can be written after its session shows in the engine's
   *  list, so the delta may not hold it yet. Ask again for a few reads. */
  function retryMissing(ids: readonly string[]): void {
    let again = false;
    for (const id of ids) {
      const tries = (missed.get(id) ?? 0) + 1;
      if (tries > MISSING_ID_RETRIES) {
        missed.delete(id);
        continue;
      }
      missed.set(id, tries);
      fresh.add(id);
      again = true;
    }
    if (again) pending = true;
  }

  function load(): void {
    if (role !== "leader") return;
    if (loading) {
      pending = true;
      return;
    }
    loading = true;
    read()
      .catch((error: unknown) => {
        if (!(error instanceof VogtUnavailable) && !(error instanceof VogtBusy)) {
          console.warn("rail starters could not be read", error);
        }
        // Read in full next time: what failed may have been the first read.
        fullWanted = true;
      })
      .finally(() => {
        loading = false;
        if (pending) {
          pending = false;
          schedule();
        }
      });
  }

  function schedule(): void {
    clearTimeout(timer);
    timer = setTimeout(load, debounceMs);
  }

  return {
    /** The engine's session set, as ids. Reads only for ids not seen before. */
    noteSessions(ids: readonly string[]): void {
      if (ids.length === 0) return;
      let added = false;
      for (const id of ids) {
        if (known.has(id)) continue;
        known.add(id);
        // A child the map already names needs no read.
        if (!map.has(id)) fresh.add(id);
        added = true;
      }
      if (role === "unknown") {
        elect();
        return;
      }
      if (role === "leader" && added && (fresh.size > 0 || !loadedOnce)) schedule();
    },
    /** The stream reconnected: what changed meanwhile is unknown, read in full. */
    reconnected(): void {
      fullWanted = true;
      if (role === "leader") schedule();
    },
    /** This tab is going away: hand the reading on. */
    close(): void {
      clearTimeout(timer);
      clearTimeout(electTimer);
      stopTimers();
      if (role === "leader") post({ type: "bye" });
      role = "unknown";
    },
    get role() {
      return role;
    },
  };
}

const [starters, setStarters] = createSignal<ReadonlyMap<string, string>>(new Map());
let reader: ReturnType<typeof createStartersReader> | undefined;

function theReader(): ReturnType<typeof createStartersReader> {
  if (reader) return reader;
  let channel: StartersChannel | null = null;
  try {
    if (typeof BroadcastChannel !== "undefined") channel = new BroadcastChannel(CHANNEL_NAME) as unknown as StartersChannel;
  } catch {
    channel = null;
  }
  const made = createStartersReader({ fetchPage: fetchStartersPage, channel, onMap: setStarters });
  reader = made;
  onWake((wake) => {
    if (wake.reason === "sse-reconnect") made.reconnected();
  });
  if (typeof window !== "undefined") window.addEventListener("pagehide", () => made.close());
  return made;
}

/** Tell the reader the engine's session set changed. Call it with the ids,
 *  newline-joined. Nothing is read for an empty set: before the engine has
 *  listed a session there is nothing to nest, and a read at boot would go out
 *  ahead of sign-in. */
export function noteRailSessions(sessionKey: string): void {
  if (sessionKey === "") return;
  theReader().noteSessions(sessionKey.split("\n"));
}

/** The current starters map (engine child id → engine starter id). */
export function railStarters(): ReadonlyMap<string, string> {
  return starters();
}
