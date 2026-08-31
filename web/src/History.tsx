import {
  Component,
  For,
  Show,
  createEffect,
  createMemo,
  createSignal,
  onCleanup,
  onMount,
  untrack,
} from "solid-js";
import { useLocation, useNavigate } from "@solidjs/router";
import {
  api,
  type HistoryLogPreview,
  type HistorySearchResult,
  type HistorySessionMetadata,
} from "./api";
import {
  getPinnedHistoryIds,
  removeHistoryPin,
  toggleHistoryPin,
} from "./historyPins";
import { readToolDraft, writeToolDraft } from "./toolDrafts";
import { toReadableTranscript } from "./historyReplay";
import { sessionsStore } from "./store";
import { SafeSnippet } from "./SafeSnippet";
import { onWake } from "./wakeCoordinator";
import {
  historyMatchKey,
  historyResultUrl,
  historyUrl,
  readHistoryRoute,
} from "./historyRoute";

interface Props {
  onError?: (message: string) => void;
  confirmAction?: (title: string, body?: string) => Promise<boolean>;
}

type StatusFilter = "all" | "running" | "exited" | "unfinished";
type SortMode = "recent" | "oldest" | "largest";
type SessionLoad = "initial" | "refresh" | "more" | null;
type SessionRetry = "refresh" | "more";

/**
 * A row in the merged History list. History is the one place that lists every
 * session, live and dead (#477): archived rows read from the history store are
 * unioned with the live session registry, and `live` records which side a row
 * came from so liveness is a filterable facet rather than a gate. A live row's
 * `ended_at` is always null and its output is not yet in the search index.
 */
type HistoryRow = HistorySessionMetadata & { live: boolean };

interface HistoryDraft {
  selectedId: string | null;
  metadataQuery: string;
  outputQuery: string;
  statusFilter: StatusFilter;
  sortMode: SortMode;
  showPinnedOnly: boolean;
  tailBytes: number;
}

interface PanelErrorProps {
  message: string;
  retryLabel: string;
  onRetry: () => void;
  stale?: boolean;
}

const HISTORY_PAGE_SIZE = 200;
const HISTORY_SEARCH_LIMIT = 100;
/** How long the reader must pause before an output search reaches the server
 *  (#225). One search per settled query, not one per keystroke. */
const SEARCH_DEBOUNCE_MS = 250;

const EMPTY_HISTORY_DRAFT: HistoryDraft = {
  selectedId: null,
  metadataQuery: "",
  outputQuery: "",
  statusFilter: "all",
  sortMode: "recent",
  showPinnedOnly: false,
  tailBytes: 64 * 1024,
};

const PanelError: Component<PanelErrorProps> = (props) => (
  <div class="history-panel-error" role="alert">
    <div>
      <strong>{props.message}</strong>
      <Show when={props.stale}>
        <span> Last successful data is retained and may be stale.</span>
      </Show>
    </div>
    <button type="button" onClick={props.onRetry}>
      {props.retryLabel}
    </button>
  </div>
);

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function formatDate(value: string | null): string {
  if (!value) return "Unknown";
  try {
    return new Date(value).toLocaleString();
  } catch {
    return value;
  }
}

function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

function matchesStatus(row: HistoryRow, filter: StatusFilter): boolean {
  switch (filter) {
    case "all":
      return true;
    case "running":
      // Live in the registry and not yet reaped: an actual running shell.
      return row.live && row.exit_code === null;
    case "exited":
      // Anything that carries an exit code — an archived exit, or a live row
      // the engine has already killed but not yet archived.
      return row.exit_code !== null;
    case "unfinished":
      // An archived row with no recorded exit code (crashed, killed, or an
      // older record predating exit capture). Previously unmatchable.
      return !row.live && row.exit_code === null;
  }
}

const History: Component<Props> = (props) => {
  const location = useLocation();
  const navigate = useNavigate();
  const restored = readToolDraft("history", EMPTY_HISTORY_DRAFT);
  const initialRoute = readHistoryRoute(location.search);
  const [selectedId, setSelectedId] = createSignal<string | null>(
    initialRoute.hasState ? initialRoute.sessionId : restored.selectedId,
  );
  const [selectedMatch, setSelectedMatch] = createSignal<string | null>(
    initialRoute.hasState ? initialRoute.matchKey : null,
  );
  const [metadataQuery, setMetadataQuery] = createSignal(restored.metadataQuery);
  const [outputQuery, setOutputQuery] = createSignal(
    initialRoute.hasState ? initialRoute.query : restored.outputQuery,
  );
  const [statusFilter, setStatusFilter] = createSignal<StatusFilter>(restored.statusFilter);
  const [sortMode, setSortMode] = createSignal<SortMode>(restored.sortMode);
  const [showPinnedOnly, setShowPinnedOnly] = createSignal(restored.showPinnedOnly);
  const [pinnedIds, setPinnedIds] = createSignal<string[]>(getPinnedHistoryIds());
  const [tailBytes, setTailBytes] = createSignal(restored.tailBytes);
  let outputSearchInput: HTMLInputElement | undefined;
  let routeEffectReady = false;
  let lastFocusedRoute = "";
  // The output search is debounced (#225): these track whether the pending
  // change is one the reader typed — which drops the selection and moves the
  // URL — or a route-driven one that set both itself, and the last settled
  // query already acted on so a re-run does not clear a selection twice.
  let searchWasTyped = false;
  let committedQuery: string | null = null;

  const [sessions, setSessions] = createSignal<HistorySessionMetadata[]>([]);
  const [sessionsLoaded, setSessionsLoaded] = createSignal(false);
  const [sessionsLoading, setSessionsLoading] = createSignal<SessionLoad>(null);
  const [sessionsError, setSessionsError] = createSignal<string | null>(null);
  const [sessionsStale, setSessionsStale] = createSignal(false);
  const [sessionsRetry, setSessionsRetry] = createSignal<SessionRetry>("refresh");

  // History is the one place that lists every session, live and dead (#477).
  // The live registry is unioned into the archived list, keyed by id with the
  // live entry winning on conflict, so a session that is both running and
  // partially archived appears once and reads as live. The archived list keeps
  // its `created_at DESC` order; sort/filter happen downstream in
  // `filteredSessions`.
  const mergedSessions = createMemo<HistoryRow[]>(() => {
    const byId = new Map<string, HistoryRow>();
    for (const session of sessions()) {
      byId.set(session.id, { ...session, live: false });
    }
    for (const id of sessionsStore.order) {
      const live = sessionsStore.sessions[id];
      if (!live) continue;
      byId.set(id, {
        id: live.id,
        name: live.name,
        created_at: live.created_at,
        ended_at: null,
        exit_code: live.exit_code,
        cwd: live.cwd ?? null,
        command: live.command ?? null,
        scrollback_bytes: live.scrollback_bytes,
        live: true,
      });
    }
    return [...byId.values()];
  });

  const liveSessionCount = createMemo(
    () => mergedSessions().filter((row) => row.live).length,
  );
  const [archiveTotal, setArchiveTotal] = createSignal<number | null>(null);
  const [archiveComplete, setArchiveComplete] = createSignal(false);
  const [hasMoreSessions, setHasMoreSessions] = createSignal(false);

  const [searchResults, setSearchResults] = createSignal<HistorySearchResult[]>([]);
  const [searchKey, setSearchKey] = createSignal("");
  const [searchLoaded, setSearchLoaded] = createSignal(false);
  const [searchLoading, setSearchLoading] = createSignal(false);
  const [searchError, setSearchError] = createSignal<string | null>(null);
  const [searchStale, setSearchStale] = createSignal(false);

  const [selectedSession, setSelectedSession] = createSignal<HistorySessionMetadata | null>(null);
  const [detailKey, setDetailKey] = createSignal<string | null>(null);
  const [detailLoading, setDetailLoading] = createSignal(false);
  const [detailError, setDetailError] = createSignal<string | null>(null);
  const [detailStale, setDetailStale] = createSignal(false);

  const [logPreview, setLogPreview] = createSignal<HistoryLogPreview | null>(null);
  const [logKey, setLogKey] = createSignal("");
  const [logLoading, setLogLoading] = createSignal(false);
  const [logError, setLogError] = createSignal<string | null>(null);
  const [logStale, setLogStale] = createSignal(false);

  let sessionsRequest = 0;
  let searchRequest = 0;
  let detailRequest = 0;
  let logRequest = 0;

  onCleanup(() => {
    sessionsRequest += 1;
    searchRequest += 1;
    detailRequest += 1;
    logRequest += 1;
    writeToolDraft<HistoryDraft>("history", {
      selectedId: selectedId(),
      metadataQuery: metadataQuery(),
      outputQuery: outputQuery(),
      statusFilter: statusFilter(),
      sortMode: sortMode(),
      showPinnedOnly: showPinnedOnly(),
      tailBytes: tailBytes(),
    });
  });

  const refreshPins = () => setPinnedIds(getPinnedHistoryIds());

  onMount(() => {
    const onStorage = (event: StorageEvent) => {
      if (event.key === null || event.key === "vogt.historyPins.v1") {
        refreshPins();
      }
    };
    window.addEventListener("storage", onStorage);
    const stopWake = onWake(() => refreshPins());
    onCleanup(() => {
      window.removeEventListener("storage", onStorage);
      stopWake();
    });
  });

  // The route is authoritative when a shared link, Back or Forward changes
  // it. The first render was already initialised from the same URL above; a
  // generic first mount deliberately keeps the existing browser-local draft.
  createEffect(() => {
    const search = location.search;
    const route = readHistoryRoute(search);
    if (!routeEffectReady) {
      routeEffectReady = true;
    } else if (route.hasState) {
      // The route is authoritative and carries its own selection, so the
      // debounced search effect must not treat this as typing and clear it.
      searchWasTyped = false;
      setOutputQuery(route.query);
      setSelectedId(route.sessionId);
      setSelectedMatch(route.matchKey);
    } else {
      searchWasTyped = false;
      setOutputQuery("");
      setSelectedId(null);
      setSelectedMatch(null);
    }

    if (routeEffectReady && route.focusSearch && lastFocusedRoute !== search) {
      lastFocusedRoute = search;
      queueMicrotask(() => outputSearchInput?.focus());
    }
  });

  const loadFirstPage = async (): Promise<void> => {
    const request = ++sessionsRequest;
    setSessionsLoading(sessionsLoaded() ? "refresh" : "initial");
    const statusRequest = api.operationalStatus().catch(() => null);
    try {
      const items = await api.listHistorySessions(HISTORY_PAGE_SIZE, 0);
      if (request !== sessionsRequest) return;
      const pageComplete = items.length < HISTORY_PAGE_SIZE;
      setSessions(items);
      setSessionsLoaded(true);
      setSessionsError(null);
      setSessionsStale(false);
      setSessionsRetry("refresh");
      setArchiveTotal(null);
      setArchiveComplete(pageComplete);
      setHasMoreSessions(!pageComplete);

      void statusRequest.then((status) => {
        if (request !== sessionsRequest || status === null) return;
        const reportedTotal = status.history.archived_session_count;
        const usableTotal = reportedTotal !== null
          && reportedTotal >= items.length
          && (!pageComplete || reportedTotal === items.length)
          ? reportedTotal
          : null;
        setArchiveTotal(usableTotal);
        setArchiveComplete(
          pageComplete || (usableTotal !== null && items.length >= usableTotal),
        );
        setHasMoreSessions(
          !pageComplete && (usableTotal === null || items.length < usableTotal),
        );
      });
    } catch (error) {
      if (request !== sessionsRequest) return;
      setSessionsError(`Failed to load history: ${errorMessage(error)}`);
      setSessionsStale(sessionsLoaded());
      setSessionsRetry("refresh");
    } finally {
      if (request === sessionsRequest) setSessionsLoading(null);
    }
  };

  const loadMore = async (): Promise<void> => {
    if (sessionsLoading() !== null || !sessionsLoaded()) return;
    const request = ++sessionsRequest;
    const offset = sessions().length;
    setSessionsLoading("more");
    try {
      const items = await api.listHistorySessions(HISTORY_PAGE_SIZE, offset);
      if (request !== sessionsRequest) return;
      const existing = new Set(sessions().map((session) => session.id));
      const additions = items.filter((session) => !existing.has(session.id));
      const combined = [...sessions(), ...additions];
      const total = archiveTotal();
      const pageComplete = items.length < HISTORY_PAGE_SIZE;
      const usableTotal = total !== null
        && total >= combined.length
        && (!pageComplete || total === combined.length)
        ? total
        : null;
      setSessions(combined);
      setSessionsError(null);
      setSessionsStale(false);
      setSessionsRetry("more");
      setArchiveTotal(usableTotal);
      setArchiveComplete(
        pageComplete || (usableTotal !== null && combined.length >= usableTotal),
      );
      setHasMoreSessions(
        !pageComplete && (usableTotal === null || combined.length < usableTotal),
      );
    } catch (error) {
      if (request !== sessionsRequest) return;
      setSessionsError(`Failed to load the next history page: ${errorMessage(error)}`);
      setSessionsStale(true);
      setSessionsRetry("more");
    } finally {
      if (request === sessionsRequest) setSessionsLoading(null);
    }
  };

  const retrySessions = (): void => {
    if (sessionsRetry() === "more") {
      void loadMore();
    } else {
      void loadFirstPage();
    }
  };

  const loadSearch = async (query: string): Promise<void> => {
    const request = ++searchRequest;
    if (!query) {
      setSearchKey("");
      setSearchResults([]);
      setSearchLoaded(false);
      setSearchLoading(false);
      setSearchError(null);
      setSearchStale(false);
      return;
    }

    const sameQuery = searchKey() === query;
    if (!sameQuery) {
      setSearchKey(query);
      setSearchResults([]);
      setSearchLoaded(false);
      setSearchError(null);
      setSearchStale(false);
    }
    setSearchLoading(true);
    try {
      const results = await api.searchHistory(query, HISTORY_SEARCH_LIMIT);
      if (request !== searchRequest || outputQuery().trim() !== query) return;
      setSearchResults(results);
      setSearchLoaded(true);
      setSearchError(null);
      setSearchStale(false);
    } catch (error) {
      if (request !== searchRequest || outputQuery().trim() !== query) return;
      setSearchError(`Search failed: ${errorMessage(error)}`);
      setSearchStale(sameQuery && searchLoaded());
    } finally {
      if (request === searchRequest) setSearchLoading(false);
    }
  };

  const replaceQualifiedRoute = (
    result: HistorySearchResult,
    replace: boolean,
  ): void => {
    const matchKey = historyMatchKey(result);
    setSelectedId(result.session_id);
    setSelectedMatch(matchKey);
    navigate(historyResultUrl(outputQuery(), result), { replace });
  };

  const loadDetail = async (id: string): Promise<void> => {
    // A live row has no archive DB record yet — `GET /api/history/{id}` would
    // 404. Serve its detail straight from the merged registry row instead; the
    // replay pane still tails the on-disk log below, which works for a running
    // session (#477).
    const liveRow = mergedSessions().find((row) => row.id === id && row.live);
    if (liveRow) {
      detailRequest += 1;
      setDetailKey(id);
      setSelectedSession(liveRow);
      setDetailError(null);
      setDetailStale(false);
      setDetailLoading(false);
      return;
    }
    const request = ++detailRequest;
    const sameSession = detailKey() === id;
    if (!sameSession) {
      setDetailKey(id);
      setSelectedSession(null);
      setDetailError(null);
      setDetailStale(false);
    }
    setDetailLoading(true);
    try {
      const session = await api.getHistorySession(id);
      if (request !== detailRequest || selectedId() !== id) return;
      setSelectedSession(session);
      setDetailError(null);
      setDetailStale(false);
    } catch (error) {
      if (request !== detailRequest || selectedId() !== id) return;
      setDetailError(`Failed to load session detail: ${errorMessage(error)}`);
      setDetailStale(sameSession && selectedSession() !== null);
    } finally {
      if (request === detailRequest) setDetailLoading(false);
    }
  };

  const loadLog = async (id: string, bytes: number): Promise<void> => {
    const request = ++logRequest;
    const key = `${id}:${bytes}`;
    const sameLog = logKey() === key;
    if (!sameLog) {
      setLogKey(key);
      setLogPreview(null);
      setLogError(null);
      setLogStale(false);
    }
    setLogLoading(true);
    try {
      const preview = await api.getHistorySessionLog(id, bytes);
      if (
        request !== logRequest
        || selectedId() !== id
        || tailBytes() !== bytes
      ) return;
      setLogPreview(preview);
      setLogError(null);
      setLogStale(false);
    } catch (error) {
      if (
        request !== logRequest
        || selectedId() !== id
        || tailBytes() !== bytes
      ) return;
      setLogError(`Failed to load replay: ${errorMessage(error)}`);
      setLogStale(sameLog && logPreview() !== null);
    } finally {
      if (request === logRequest) setLogLoading(false);
    }
  };

  const outputSearchEnabled = createMemo(() => outputQuery().trim().length > 0);

  const selectedSearchMatches = createMemo(() =>
    searchResults().filter((result) => result.session_id === selectedId()),
  );

  const selectedIsLive = createMemo(() => {
    const id = selectedId();
    return !!id && mergedSessions().some((row) => row.id === id && row.live);
  });

  const filteredSessions = createMemo(() => {
    const items = [...mergedSessions()];
    const metadataNeedle = metadataQuery().trim().toLowerCase();
    const pinned = new Set(pinnedIds());
    const filtered = items.filter((session) => {
      if (showPinnedOnly() && !pinned.has(session.id)) return false;
      if (!matchesStatus(session, statusFilter())) return false;
      if (!metadataNeedle) return true;
      const haystack = [session.name, session.cwd ?? "", session.command ?? ""]
        .join("\n")
        .toLowerCase();
      return haystack.includes(metadataNeedle);
    });

    filtered.sort((a, b) => {
      const pinDelta = Number(pinned.has(b.id)) - Number(pinned.has(a.id));
      if (pinDelta !== 0) return pinDelta;
      if (sortMode() === "largest") {
        return b.scrollback_bytes - a.scrollback_bytes;
      }
      const aTs = Date.parse(a.created_at);
      const bTs = Date.parse(b.created_at);
      return sortMode() === "oldest" ? aTs - bTs : bTs - aTs;
    });
    return filtered;
  });

  const archiveSummary = createMemo(() => {
    if (!sessionsLoaded()) {
      return sessionsLoading() === "initial"
        ? "Loading archived sessions…"
        : "Archive count unavailable";
    }
    const loaded = sessions().length;
    const total = archiveTotal();
    if (total === 0) return "0 archived sessions";
    if (total !== null) return `${loaded} of ${total} archived sessions loaded`;
    if (archiveComplete()) return `${loaded} archived sessions loaded (all reached)`;
    return `${loaded} archived sessions loaded; more may be available`;
  });

  // The output search runs server-wide, so it is debounced (#225): a keystroke
  // no longer fires a search, a 250ms pause does. `debouncedQuery` follows
  // `outputQuery` a beat behind, and each new keystroke clears the pending
  // timer — so a burst of typing settles to a single server call.
  const [debouncedQuery, setDebouncedQuery] = createSignal(untrack(outputQuery));
  createEffect(() => {
    const query = outputQuery();
    const handle = window.setTimeout(
      () => setDebouncedQuery(query),
      SEARCH_DEBOUNCE_MS,
    );
    onCleanup(() => window.clearTimeout(handle));
  });

  createEffect(() => {
    const query = debouncedQuery().trim();
    untrack(() => {
      // Selection and the URL follow the *settled* query, not each keystroke:
      // clearing the selected result and rewriting the address on every
      // character made both flicker and buried the real query under a stack
      // of history entries. Only a query the reader actually changed does
      // this — a route-driven change (Back/Forward, a shared link) set the
      // selection itself and must not be undone here.
      if (searchWasTyped && query !== committedQuery) {
        setSelectedId(null);
        setSelectedMatch(null);
        navigate(
          historyUrl(
            { query, sessionId: null, matchKey: null, focusSearch: true },
            location.search,
          ),
          { replace: true },
        );
      }
      committedQuery = query;
      searchWasTyped = false;
      void loadSearch(query);
    });
  });

  createEffect(() => {
    const id = selectedId();
    if (!id) {
      detailRequest += 1;
      setDetailKey(null);
      setSelectedSession(null);
      setDetailLoading(false);
      setDetailError(null);
      setDetailStale(false);
      return;
    }
    void untrack(() => loadDetail(id));
  });

  createEffect(() => {
    const id = selectedId();
    const bytes = tailBytes();
    if (!id) {
      logRequest += 1;
      setLogKey("");
      setLogPreview(null);
      setLogLoading(false);
      setLogError(null);
      setLogStale(false);
      return;
    }
    void untrack(() => loadLog(id, bytes));
  });

  createEffect(() => {
    if (!selectedId() && !outputSearchEnabled() && sessionsLoaded()) {
      setSelectedId(filteredSessions()[0]?.id ?? null);
    }
  });

  createEffect(() => {
    if (!outputSearchEnabled() || !searchLoaded()) return;
    const results = searchResults();
    if (!results.length) return;
    const routedMatch = selectedMatch();
    const exact = routedMatch
      ? results.find((result) =>
          result.session_id === selectedId() && historyMatchKey(result) === routedMatch)
      : undefined;
    if (exact) return;
    // A shared URL remains authoritative even when its excerpt is no longer
    // returned (for example after archive maintenance). Keep its session and
    // query without silently qualifying a different result.
    if (readHistoryRoute(location.search).matchKey) return;
    const sameSession = results.find((result) => result.session_id === selectedId());
    const fallback = sameSession ?? results[0]!;
    replaceQualifiedRoute(fallback, true);
  });

  createEffect(() => {
    const match = selectedMatch();
    if (!match || !selectedSearchMatches().some((result) => historyMatchKey(result) === match)) {
      return;
    }
    queueMicrotask(() => {
      const element = document.querySelector<HTMLElement>(`[data-history-match="${match}"]`);
      element?.scrollIntoView?.({ block: "center" });
    });
  });

  onMount(() => void loadFirstPage());

  const refresh = (): void => {
    void loadFirstPage();
    const query = outputQuery().trim();
    if (query) void loadSearch(query);
    const id = selectedId();
    if (id) {
      void loadDetail(id);
      void loadLog(id, tailBytes());
    }
  };

  const togglePin = (id: string): void => {
    setPinnedIds(toggleHistoryPin(id));
  };

  const deleteSession = async (session: HistorySessionMetadata): Promise<void> => {
    if (!props.confirmAction) return;
    if (!await props.confirmAction(
      `Delete archived session "${session.name}"?`,
      "Its metadata and saved scrollback will be permanently deleted.",
    )) return;
    try {
      await api.deleteHistorySession(session.id);
      setPinnedIds(removeHistoryPin(session.id));
      if (selectedId() === session.id) setSelectedId(null);
      await loadFirstPage();
    } catch (error) {
      props.onError?.(`Delete failed: ${errorMessage(error)}`);
    }
  };

  const exportSession = async (session: HistorySessionMetadata): Promise<void> => {
    try {
      await api.downloadHistorySession(session.id);
    } catch (error) {
      props.onError?.(`Export failed: ${errorMessage(error)}`);
    }
  };

  return (
    <div class="history-view">
      <div class="history-header">
        <div>
          <h2>Session History</h2>
          <div class="history-summary">
            <span>{archiveSummary()}</span>
            <span>{pinnedIds().length} pinned</span>
            <Show when={outputSearchEnabled()}>
              <Show
                when={searchLoaded()}
                fallback={<span>Output search unavailable</span>}
              >
                <span>{searchResults().length} output matches (up to {HISTORY_SEARCH_LIMIT})</span>
              </Show>
            </Show>
          </div>
        </div>
        <button
          type="button"
          disabled={sessionsLoading() === "refresh"}
          onClick={refresh}
        >
          {sessionsLoading() === "refresh" ? "Refreshing…" : "Refresh"}
        </button>
      </div>

      <div class="history-toolbar">
        <div class="history-filter-grid">
          <label class="history-field history-field-wide">
            <span>Filter loaded sessions</span>
            <input
              type="search"
              class="history-search"
              placeholder="Name, cwd, command"
              value={metadataQuery()}
              onInput={(event) => setMetadataQuery(event.currentTarget.value)}
            />
          </label>
          <label class="history-field history-field-wide">
            <span>Search all archived output</span>
            <input
              ref={outputSearchInput}
              type="search"
              autofocus={initialRoute.focusSearch}
              class="history-search"
              placeholder="Needle inside scrollback"
              value={outputQuery()}
              onInput={(event) => {
                // A keystroke only updates the field; the debounced effect
                // above runs the search, drops the selection and moves the
                // URL once the query settles (#225).
                searchWasTyped = true;
                setOutputQuery(event.currentTarget.value);
              }}
            />
          </label>
          <label class="history-field">
            <span>Status</span>
            <select
              value={statusFilter()}
              onInput={(event) => setStatusFilter(event.currentTarget.value as StatusFilter)}
            >
              <option value="all">All</option>
              <option value="running">Running</option>
              <option value="exited">Exited</option>
              <option value="unfinished">No exit code</option>
            </select>
          </label>
          <label class="history-field">
            <span>Sort</span>
            <select
              value={sortMode()}
              onInput={(event) => setSortMode(event.currentTarget.value as SortMode)}
            >
              <option value="recent">Newest first</option>
              <option value="oldest">Oldest first</option>
              <option value="largest">Largest output</option>
            </select>
          </label>
          <label class="history-toggle">
            <input
              type="checkbox"
              checked={showPinnedOnly()}
              onChange={(event) => setShowPinnedOnly(event.currentTarget.checked)}
            />
            <span>Pinned only</span>
          </label>
        </div>
        <div class="history-scope-note">
          Metadata filters apply to loaded pages. Output search runs server-wide across the full archive.
          <Show when={liveSessionCount() > 0}>
            {" "}Running sessions are searched too — a match still in progress is badged Live.
          </Show>
        </div>
      </div>

      <div class="history-layout">
        <div class="history-sidebar">
          <Show
            when={!outputSearchEnabled()}
            fallback={
              <div class={`history-search-results ${searchStale() ? "stale" : ""}`}>
                <Show when={searchError()}>
                  {(message) => (
                    <PanelError
                      message={message()}
                      retryLabel="Retry output search"
                      stale={searchStale()}
                      onRetry={() => void loadSearch(outputQuery().trim())}
                    />
                  )}
                </Show>
                <Show when={searchLoading() && !searchLoaded()}>
                  <div class="history-loading">Searching all archived output...</div>
                </Show>
                <Show when={searchLoaded()}>
                  <Show
                    when={searchResults().length > 0}
                    fallback={<div class="history-empty">No output matches found.</div>}
                  >
                    <For each={searchResults()}>
                      {(result) => {
                        const matchKey = historyMatchKey(result);
                        return (
                        <button
                          type="button"
                          class={`history-search-result ${
                            selectedMatch() === matchKey ? "active" : ""
                          }`}
                          aria-current={selectedMatch() === matchKey ? "true" : undefined}
                          onClick={() => replaceQualifiedRoute(result, false)}
                        >
                          <div class="history-result-header">
                            <strong>{result.session_name}</strong>
                            <Show when={result.live}>
                              <span class="history-liveness-badge live">Live</span>
                            </Show>
                            <span class="history-result-date">{formatDate(result.created_at)}</span>
                          </div>
                          <div class="history-result-snippet">
                            <SafeSnippet text={result.match_snippet} query={outputQuery()} />
                          </div>
                        </button>
                        );
                      }}
                    </For>
                  </Show>
                </Show>
              </div>
            }
          >
            <div class={`history-list ${sessionsStale() ? "stale" : ""}`}>
              <Show when={sessionsError()}>
                {(message) => (
                  <PanelError
                    message={message()}
                    retryLabel={sessionsRetry() === "more" ? "Retry next page" : "Retry history"}
                    stale={sessionsStale()}
                    onRetry={retrySessions}
                  />
                )}
              </Show>
              <Show when={sessionsLoading() === "initial"}>
                <div class="history-loading">Loading archived sessions...</div>
              </Show>
              <Show when={sessionsLoaded()}>
                <Show
                  when={filteredSessions().length > 0}
                  fallback={
                    <div class="history-empty">
                      {mergedSessions().length === 0
                        ? "No sessions yet."
                        : "No sessions match these filters."}
                    </div>
                  }
                >
                  <For each={filteredSessions()}>
                    {(session) => {
                      const pinned = createMemo(() => pinnedIds().includes(session.id));
                      return (
                        <button
                          type="button"
                          class={`history-session ${selectedId() === session.id ? "active" : ""}`}
                          onClick={() => setSelectedId(session.id)}
                          title={session.cwd || session.name}
                        >
                          <div class="history-session-header">
                            <div class="history-session-name-row">
                              <span class="history-session-name">{session.name}</span>
                              <span
                                class={`history-liveness-badge ${
                                  session.live && session.exit_code === null
                                    ? "live"
                                    : "exited"
                                }`}
                              >
                                {session.live && session.exit_code === null
                                  ? "Live"
                                  : "Exited"}
                              </span>
                              <Show when={pinned()}>
                                <span class="history-pin-indicator">Pinned</span>
                              </Show>
                            </div>
                            <span
                              class={`history-exit-code ${
                                session.exit_code === 0
                                  ? "success"
                                  : session.exit_code === null
                                    ? ""
                                    : "error"
                              }`}
                            >
                              {session.exit_code === null ? "No exit" : `Exit ${session.exit_code}`}
                            </span>
                          </div>
                          <div class="history-session-meta">
                            <span>{formatDate(session.created_at)}</span>
                            <span>{formatSize(session.scrollback_bytes)}</span>
                          </div>
                          <Show when={session.cwd}>
                            <div class="history-session-cwd">{session.cwd}</div>
                          </Show>
                        </button>
                      );
                    }}
                  </For>
                </Show>
                <Show when={hasMoreSessions() || sessionsLoading() === "more"}>
                  <button
                    type="button"
                    class="history-load-more"
                    disabled={sessionsLoading() === "more"}
                    onClick={() => void loadMore()}
                  >
                    {sessionsLoading() === "more" ? "Loading more…" : "Load more"}
                  </button>
                </Show>
              </Show>
            </div>
          </Show>
        </div>

        <div class="history-detail">
          <Show
            when={selectedId()}
            fallback={<div class="history-empty">Select a session to inspect it.</div>}
          >
            <Show when={detailError()}>
              {(message) => (
                <PanelError
                  message={message()}
                  retryLabel="Retry session detail"
                  stale={detailStale()}
                  onRetry={() => {
                    const id = selectedId();
                    if (id) void loadDetail(id);
                  }}
                />
              )}
            </Show>
            <Show when={detailLoading() && selectedSession() === null}>
              <div class="history-loading">Loading archived session...</div>
            </Show>
            <Show when={selectedSession()}>
              {(session) => (
                <div class={`history-detail-content ${detailStale() ? "stale" : ""}`}>
                  <div class="history-detail-header">
                    <div class="history-detail-heading">
                      <h3>{session().name}</h3>
                      <div class="history-detail-meta">
                        <span>Created {formatDate(session().created_at)}</span>
                        <Show when={session().ended_at}>
                          <span>Ended {formatDate(session().ended_at)}</span>
                        </Show>
                        <span>{formatSize(session().scrollback_bytes)}</span>
                      </div>
                    </div>
                    <div class="history-detail-actions">
                      <button type="button" onClick={() => togglePin(session().id)}>
                        {pinnedIds().includes(session().id) ? "Unpin" : "Pin"}
                      </button>
                      {/* Export and Delete act on the archive record, which a
                          live session does not have yet — hide them until it
                          exits and is archived (#477). */}
                      <Show when={!selectedIsLive()}>
                        <button type="button" onClick={() => void exportSession(session())}>Export</button>
                        <button
                          type="button"
                          class="danger"
                          onClick={() => void deleteSession(session())}
                        >
                          Delete
                        </button>
                      </Show>
                    </div>
                  </div>

                  <div class="history-detail-grid">
                    <div class="history-detail-card">
                      <div class="history-detail-label">Working directory</div>
                      <div class="history-detail-value">{session().cwd || "Unknown"}</div>
                    </div>
                    <div class="history-detail-card">
                      <div class="history-detail-label">Command</div>
                      <div class="history-detail-value">{session().command || "Default shell"}</div>
                    </div>
                  </div>

                  <Show when={selectedSearchMatches().length > 0}>
                    <div class="history-match-panel">
                      <div class="history-match-header">
                        <strong>Output matches</strong>
                        <span>{selectedSearchMatches().length} hit(s)</span>
                      </div>
                      <div class="history-match-list">
                        <For each={selectedSearchMatches()}>
                          {(result) => {
                            const matchKey = historyMatchKey(result);
                            const qualified = () => selectedMatch() === matchKey;
                            return (
                            <div
                              class={`history-result-snippet ${qualified() ? "qualified-match" : ""}`}
                              data-history-match={matchKey}
                              aria-current={qualified() ? "true" : undefined}
                            >
                              <SafeSnippet text={result.match_snippet} query={outputQuery()} />
                            </div>
                            );
                          }}
                        </For>
                      </div>
                    </div>
                  </Show>

                  <div class="history-replay-toolbar">
                    <div>
                      <strong>Replay preview</strong>
                      <div class="history-replay-note">
                        {selectedIsLive()
                          ? "Tail of the live session, rendered readable — escape codes resolved."
                          : "Tail of the archived session, rendered readable — escape codes resolved."}
                      </div>
                    </div>
                    <label class="history-field">
                      <span>Tail size</span>
                      <select
                        value={String(tailBytes())}
                        onInput={(event) => setTailBytes(Number.parseInt(event.currentTarget.value, 10))}
                      >
                        <option value={16 * 1024}>16 KB</option>
                        <option value={64 * 1024}>64 KB</option>
                        <option value={256 * 1024}>256 KB</option>
                      </select>
                    </label>
                  </div>

                  <div class={`history-replay ${logStale() ? "stale" : ""}`}>
                    <Show when={logError()}>
                      {(message) => (
                        <PanelError
                          message={message()}
                          retryLabel="Retry replay"
                          stale={logStale()}
                          onRetry={() => {
                            const id = selectedId();
                            if (id) void loadLog(id, tailBytes());
                          }}
                        />
                      )}
                    </Show>
                    <Show when={logLoading() && logPreview() === null}>
                      <div class="history-loading">Loading terminal output...</div>
                    </Show>
                    <Show when={logPreview()}>
                      {(preview) => (
                        <>
                          <div class="history-replay-meta">
                            <span>
                              Showing {formatSize(preview().bytes)} of {formatSize(preview().total_bytes)}
                            </span>
                            <Show when={preview().truncated}>
                              <span>Preview is truncated to the requested tail size.</span>
                            </Show>
                          </div>
                          <pre class="history-replay-output">{toReadableTranscript(preview().text) || "No output captured."}</pre>
                        </>
                      )}
                    </Show>
                  </div>
                </div>
              )}
            </Show>
          </Show>
        </div>
      </div>
    </div>
  );
};

export default History;
