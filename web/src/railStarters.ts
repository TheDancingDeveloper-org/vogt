// Which engine session started which, for the rail's oversight nesting (WI-1736).
//
// The engine's own session list does not say who started a session; Vogt's
// `session.list` does (`started_by_session`, WI-1323), keyed by the engine id
// it links each session to (`engine_session_id`). This reads that list on its
// own short cycle and hands the rail a map. No core behind the front door, or
// a read that fails, leaves the map empty: the rail then shows the sessions
// flat, which is the behaviour before nesting existed.

import { createEffect, createSignal, on, onCleanup } from "solid-js";
import { listSessions, VogtBusy, VogtUnavailable, type SessionSummary as VogtSessionSummary } from "./vogtApi";

/** How often the starters are re-read while the rail is open. */
const REFRESH_MS = 30_000;
/** Coalesces a burst of session-list changes into one read. */
const DEBOUNCE_MS = 750;

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

/**
 * The starters map, kept fresh while the owner is mounted. `sessionKey` changes
 * whenever the set of engine sessions changes, so a new child is linked soon
 * after it appears rather than at the next refresh. Nothing is read until the
 * engine has listed a session: before that there is nothing to nest, and a
 * read at boot would go out ahead of sign-in.
 */
export function createRailStarters(sessionKey: () => string): () => ReadonlyMap<string, string> {
  const [starters, setStarters] = createSignal<ReadonlyMap<string, string>>(new Map());
  let timer: ReturnType<typeof setTimeout> | undefined;
  let interval: ReturnType<typeof setInterval> | undefined;
  let alive = true;

  const load = () => {
    listSessions({ include_stopped: true })
      .then((answer) => {
        if (alive) setStarters(startersById(answer.sessions));
      })
      .catch((error: unknown) => {
        // No core or a busy door: keep what we had, and read again later.
        if (!(error instanceof VogtUnavailable) && !(error instanceof VogtBusy)) {
          console.warn("rail starters could not be read", error);
        }
      });
  };

  const schedule = () => {
    clearTimeout(timer);
    timer = setTimeout(load, DEBOUNCE_MS);
  };

  createEffect(on(sessionKey, (key) => {
    if (key === "") return;
    schedule();
    if (interval === undefined) interval = setInterval(load, REFRESH_MS);
  }));

  onCleanup(() => {
    alive = false;
    clearTimeout(timer);
    clearInterval(interval);
  });

  return starters;
}
