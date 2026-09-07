import type { HistorySearchResult } from "./api";

const MAX_HISTORY_QUERY_LENGTH = 512;
const SAFE_SESSION_ID = /^[A-Za-z0-9._:-]{1,200}$/;
const SAFE_MATCH_KEY = /^m[0-9a-f]{8}$/;

export interface HistoryRouteState {
  hasState: boolean;
  query: string;
  sessionId: string | null;
  matchKey: string | null;
  focusSearch: boolean;
}

export interface HistoryRouteUpdate {
  query?: string | null;
  sessionId?: string | null;
  matchKey?: string | null;
  focusSearch?: boolean;
}

/**
 * A compact, deterministic identity for the excerpt returned by History FTS.
 *
 * The engine currently returns one ranked excerpt per archived session and no
 * byte offset. Hashing the immutable session id and returned excerpt gives the
 * client a shareable match identity without pretending the API exposed a log
 * position that it did not. A changed/missing excerpt simply fails closed and
 * History keeps the requested session context without marking a different
 * excerpt as though it were the shared one.
 */
export function historyMatchKey(result: Pick<HistorySearchResult, "session_id" | "match_snippet">): string {
  const value = `${result.session_id}\0${result.match_snippet}`;
  let hash = 0x811c9dc5;
  for (let i = 0; i < value.length; i += 1) {
    hash ^= value.charCodeAt(i);
    hash = Math.imul(hash, 0x01000193);
  }
  return `m${(hash >>> 0).toString(16).padStart(8, "0")}`;
}

export function readHistoryRoute(search: string): HistoryRouteState {
  const params = new URLSearchParams(search);
  const hasState = ["q", "session", "match", "focus"].some((key) => params.has(key));
  const rawQuery = params.get("q") ?? "";
  const rawSession = params.get("session");
  const rawMatch = params.get("match");
  const sessionId = rawSession && SAFE_SESSION_ID.test(rawSession) ? rawSession : null;
  const matchKey = sessionId && rawMatch && SAFE_MATCH_KEY.test(rawMatch) ? rawMatch : null;
  return {
    hasState,
    query: rawQuery.slice(0, MAX_HISTORY_QUERY_LENGTH),
    sessionId,
    matchKey,
    focusSearch: params.get("focus") === "search",
  };
}

export function historyUrl(
  update: HistoryRouteUpdate,
  currentSearch = "",
): string {
  const params = new URLSearchParams(currentSearch);
  const setOrDelete = (key: string, value: string | null | undefined): void => {
    if (value === null || value === undefined || value === "") params.delete(key);
    else params.set(key, value);
  };

  if ("query" in update) setOrDelete("q", update.query?.slice(0, MAX_HISTORY_QUERY_LENGTH));
  if ("sessionId" in update) {
    const session = update.sessionId;
    setOrDelete("session", session && SAFE_SESSION_ID.test(session) ? session : null);
  }
  if ("matchKey" in update) {
    const match = update.matchKey;
    setOrDelete("match", match && SAFE_MATCH_KEY.test(match) ? match : null);
  }
  if ("focusSearch" in update) setOrDelete("focus", update.focusSearch ? "search" : null);

  const query = params.toString();
  return query ? `/history?${query}` : "/history";
}

export function historyResultUrl(
  query: string,
  result: Pick<HistorySearchResult, "session_id" | "match_snippet">,
): string {
  return historyUrl({
    query: query.trim(),
    sessionId: result.session_id,
    matchKey: historyMatchKey(result),
    focusSearch: false,
  });
}
