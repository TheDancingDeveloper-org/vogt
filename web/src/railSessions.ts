// One sorted session rail, shared by the Places rail and the Sessions list.
//
// Both used to sort the store on their own (App via sortSessionsForRail,
// SessionList / Sessions via sortSessionsOversightFirst). The sort is a
// stable partition on role and bookmark membership, so the memo reads only
// those fields: a resource sample, activity tick or renamed row updates the
// object in place without allocating a new ordered array (WI-2696).

import { createMemo } from "solid-js";
import type { SessionSummary } from "./api";
import { bookmarks } from "./bookmarks";
import { railSortKey, sortSessionsForRail } from "./sessionRowModel";
import { sessionsStore } from "./store";

function listedSessions(): SessionSummary[] {
  const out: SessionSummary[] = [];
  for (const id of sessionsStore.order) {
    const session = sessionsStore.sessions[id];
    if (session) out.push(session);
  }
  return out;
}

/**
 * The only store reads this memo tracks. Touching `name`, `resources` or
 * `activity` here would re-sort the rail on every sample.
 */
function sortInputs(): { id: string; role: SessionSummary["role"] }[] {
  const out: { id: string; role: SessionSummary["role"] }[] = [];
  for (const id of sessionsStore.order) {
    const session = sessionsStore.sessions[id];
    if (session) out.push({ id: session.id, role: session.role });
  }
  return out;
}

const EMPTY_BOOKMARKS: ReadonlySet<string> = new Set();

function memoisedOrder(
  prev: SessionSummary[],
  bookmarked: ReadonlySet<string>,
): SessionSummary[] {
  const inputs = sortInputs();
  const key = railSortKey(inputs, bookmarked);
  if (prev.length > 0 && railSortKey(prev, bookmarked) === key) return prev;
  return sortSessionsForRail(listedSessions(), bookmarked);
}

/** Engine order, oversight first, bookmarks lifted between the two.
 *  The Places rail is the only consumer that lifts bookmarks (WI-1289). */
export const railSessions = createMemo<SessionSummary[]>((prev) => {
  return memoisedOrder(prev, new Set(bookmarks()));
}, []);

/** Engine order, oversight first, no bookmark partition.
 *  Sessions and SessionList used `sortSessionsOversightFirst` before the
 *  shared memo; lifting bookmarks here would change their order (WI-2696). */
export const oversightFirstSessions = createMemo<SessionSummary[]>((prev) => {
  return memoisedOrder(prev, EMPTY_BOOKMARKS);
}, []);
