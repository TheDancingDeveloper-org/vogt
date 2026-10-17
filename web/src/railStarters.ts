// Which engine session started which, for the rail's and the session list's
// oversight nesting (WI-1736).
//
// The engine's own session list does not say who started a session; Vogt's
// `session.list` does (`started_by_session`, WI-1323), keyed by the engine id
// it links each session to (`engine_session_id`). Vogt returns at most 500 links
// a page and 50 by default, so this pages until the page says it is not
// truncated, up to a cap. One reader serves every surface that nests (the rail,
// the phone and overview list), so there is one poll loop, not one per surface.
// No core, or a read that fails, leaves the map as it was: the lists then show
// the sessions flat, the behaviour before nesting existed.

import { createSignal } from "solid-js";
import { listSessions, VogtBusy, VogtUnavailable, type SessionSummary as VogtSessionSummary } from "./vogtApi";

/** How often the starters are re-read while a list is open. */
const REFRESH_MS = 30_000;
/** Coalesces a burst of session-list changes into one read. */
const DEBOUNCE_MS = 750;
/** Links per page: Vogt's cap for `session.list`. */
export const STARTERS_PAGE_SIZE = 500;
/** Pages read at most (2000 links). Past that, nesting covers what was read. */
export const STARTERS_MAX_PAGES = 4;

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

interface StartersPage {
  sessions: VogtSessionSummary[];
  truncated?: boolean;
}

/** Read every page of the session list that names a starter. `fetchPage` is
 *  given the offset to ask for. Pure apart from that call, so the paging is
 *  testable without a core. */
export async function readAllStarters(
  fetchPage: (offset: number) => Promise<StartersPage>,
): Promise<Map<string, string>> {
  const starters = new Map<string, string>();
  let offset = 0;
  for (let page = 0; page < STARTERS_MAX_PAGES; page += 1) {
    const answer = await fetchPage(offset);
    for (const [child, starter] of startersById(answer.sessions)) starters.set(child, starter);
    if (!answer.truncated || answer.sessions.length === 0) break;
    offset += answer.sessions.length;
  }
  return starters;
}

const [starters, setStarters] = createSignal<ReadonlyMap<string, string>>(new Map());
let timer: ReturnType<typeof setTimeout> | undefined;
let interval: ReturnType<typeof setInterval> | undefined;
let loading = false;
let pending = false;

function load(): void {
  if (loading) {
    // A change arrived mid-read: read again once this one lands.
    pending = true;
    return;
  }
  loading = true;
  readAllStarters((offset) =>
    listSessions({ include_stopped: true, limit: STARTERS_PAGE_SIZE, offset }),
  )
    .then((map) => setStarters(map))
    .catch((error: unknown) => {
      // No core or a busy door: keep what we had, and read again later.
      if (!(error instanceof VogtUnavailable) && !(error instanceof VogtBusy)) {
        console.warn("rail starters could not be read", error);
      }
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
  timer = setTimeout(load, DEBOUNCE_MS);
}

/** Tell the reader the engine's session set changed. Call it with a key that
 *  changes whenever the set does. Nothing is read for an empty set: before the
 *  engine has listed a session there is nothing to nest, and a read at boot
 *  would go out ahead of sign-in. The first real call starts the refresh cycle. */
export function noteRailSessions(sessionKey: string): void {
  if (sessionKey === "") return;
  schedule();
  if (interval === undefined) interval = setInterval(load, REFRESH_MS);
}

/** The current starters map (engine child id → engine starter id). */
export function railStarters(): ReadonlyMap<string, string> {
  return starters();
}
