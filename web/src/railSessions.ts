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

/** Engine order, oversight first, bookmarks lifted between the two. */
export const railSessions = createMemo<SessionSummary[]>((prev) => {
  const inputs = sortInputs();
  const bookmarked = new Set(bookmarks());
  const key = railSortKey(inputs, bookmarked);
  if (prev.length > 0 && railSortKey(prev, bookmarked) === key) return prev;
  return sortSessionsForRail(listedSessions(), bookmarked);
}, []);
