import {
  Component,
  For,
  Show,
  createEffect,
  createMemo,
  createResource,
  createSignal,
  onCleanup,
  onMount,
} from "solid-js";
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

interface Props {
  onError?: (message: string) => void;
  confirmAction?: (title: string, body?: string) => Promise<boolean>;
}

type StatusFilter = "all" | "success" | "error" | "unfinished";
type SortMode = "recent" | "oldest" | "largest";

interface HistoryDraft {
  selectedId: string | null;
  metadataQuery: string;
  outputQuery: string;
  statusFilter: StatusFilter;
  sortMode: SortMode;
  showPinnedOnly: boolean;
  tailBytes: number;
}

const EMPTY_HISTORY_DRAFT: HistoryDraft = {
  selectedId: null,
  metadataQuery: "",
  outputQuery: "",
  statusFilter: "all",
  sortMode: "recent",
  showPinnedOnly: false,
  tailBytes: 64 * 1024,
};

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

function matchesStatus(
  session: HistorySessionMetadata,
  filter: StatusFilter,
): boolean {
  switch (filter) {
    case "all":
      return true;
    case "success":
      return session.exit_code === 0;
    case "error":
      return session.exit_code !== null && session.exit_code !== 0;
    case "unfinished":
      return session.exit_code === null;
  }
}

const History: Component<Props> = (props) => {
  const restored = readToolDraft("history", EMPTY_HISTORY_DRAFT);
  const [reloadKey, setReloadKey] = createSignal(0);
  const [selectedId, setSelectedId] = createSignal<string | null>(restored.selectedId);
  const [metadataQuery, setMetadataQuery] = createSignal(restored.metadataQuery);
  const [outputQuery, setOutputQuery] = createSignal(restored.outputQuery);
  const [statusFilter, setStatusFilter] = createSignal<StatusFilter>(restored.statusFilter);
  const [sortMode, setSortMode] = createSignal<SortMode>(restored.sortMode);
  const [showPinnedOnly, setShowPinnedOnly] = createSignal(restored.showPinnedOnly);
  const [pinnedIds, setPinnedIds] = createSignal<string[]>(getPinnedHistoryIds());
  const [tailBytes, setTailBytes] = createSignal(restored.tailBytes);

  onCleanup(() => writeToolDraft<HistoryDraft>("history", {
    selectedId: selectedId(),
    metadataQuery: metadataQuery(),
    outputQuery: outputQuery(),
    statusFilter: statusFilter(),
    sortMode: sortMode(),
    showPinnedOnly: showPinnedOnly(),
    tailBytes: tailBytes(),
  }));

  const refresh = () => setReloadKey((value) => value + 1);
  const refreshPins = () => setPinnedIds(getPinnedHistoryIds());

  onMount(() => {
    const onStorage = (event: StorageEvent) => {
      if (event.key === null || event.key === "mydevenv2.historyPins.v1") {
        refreshPins();
      }
    };
    const onFocus = () => refreshPins();
    window.addEventListener("storage", onStorage);
    window.addEventListener("focus", onFocus);
    onCleanup(() => {
      window.removeEventListener("storage", onStorage);
      window.removeEventListener("focus", onFocus);
    });
  });

  const [sessions] = createResource(
    reloadKey,
    async (): Promise<HistorySessionMetadata[]> => {
      try {
        return await api.listHistorySessions(200, 0);
      } catch (e) {
        props.onError?.(`Failed to load history: ${(e as Error).message}`);
        return [];
      }
    },
  );

  const outputSearchEnabled = createMemo(() => outputQuery().trim().length > 0);

  const [searchResults] = createResource(
    () => [reloadKey(), outputQuery().trim()] as const,
    async ([, query]): Promise<HistorySearchResult[]> => {
      if (!query) return [];
      try {
        return await api.searchHistory(query, 50);
      } catch (e) {
        props.onError?.(`Search failed: ${(e as Error).message}`);
        return [];
      }
    },
  );

  const selectedSearchMatches = createMemo(() =>
    (searchResults() ?? []).filter((result) => result.session_id === selectedId()),
  );

  const [selectedSession] = createResource(
    selectedId,
    async (id): Promise<HistorySessionMetadata | null> => {
      if (!id) return null;
      try {
        return await api.getHistorySession(id);
      } catch (e) {
        props.onError?.(`Failed to load session detail: ${(e as Error).message}`);
        return null;
      }
    },
  );

  const [logPreview] = createResource(
    () => {
      const id = selectedId();
      return id ? ([id, tailBytes()] as const) : null;
    },
    async (key): Promise<HistoryLogPreview | null> => {
      if (!key) return null;
      const [id, bytes] = key;
      try {
        return await api.getHistorySessionLog(id, bytes);
      } catch (e) {
        props.onError?.(`Failed to load replay: ${(e as Error).message}`);
        return null;
      }
    },
  );

  const filteredSessions = createMemo(() => {
    const items = [...(sessions() ?? [])];
    const metadataNeedle = metadataQuery().trim().toLowerCase();
    const pinned = new Set(pinnedIds());
    const filtered = items.filter((session) => {
      if (showPinnedOnly() && !pinned.has(session.id)) return false;
      if (!matchesStatus(session, statusFilter())) return false;
      if (!metadataNeedle) return true;
      const haystack = [
        session.name,
        session.cwd ?? "",
        session.command ?? "",
      ]
        .join("\n")
        .toLowerCase();
      return haystack.includes(metadataNeedle);
    });

    filtered.sort((a, b) => {
      const pinDelta =
        Number(pinned.has(b.id)) - Number(pinned.has(a.id));
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

  createEffect(() => {
    const loadedSessions = sessions();
    if (loadedSessions === undefined) return;
    const current = selectedId();
    if (current && loadedSessions.some((session) => session.id === current)) {
      return;
    }
    const fallback = outputSearchEnabled()
      ? searchResults()?.[0]?.session_id ?? null
      : filteredSessions()[0]?.id ?? null;
    setSelectedId(fallback);
  });

  createEffect(() => {
    if (!outputSearchEnabled()) return;
    const first = searchResults()?.[0]?.session_id ?? null;
    if (first && !(selectedId() && searchResults()?.some((result) => result.session_id === selectedId()))) {
      setSelectedId(first);
    }
  });

  const togglePin = (id: string) => {
    setPinnedIds(toggleHistoryPin(id));
  };

  const deleteSession = async (session: HistorySessionMetadata) => {
    if (!props.confirmAction) return;
    if (!await props.confirmAction(
      `Delete archived session "${session.name}"?`,
      "Its metadata and saved scrollback will be permanently deleted.",
    )) return;
    try {
      await api.deleteHistorySession(session.id);
      setPinnedIds(removeHistoryPin(session.id));
      if (selectedId() === session.id) {
        setSelectedId(null);
      }
      refresh();
    } catch (e) {
      props.onError?.(`Delete failed: ${(e as Error).message}`);
    }
  };

  const exportSession = async (session: HistorySessionMetadata) => {
    try {
      await api.downloadHistorySession(session.id);
    } catch (e) {
      props.onError?.(`Export failed: ${(e as Error).message}`);
    }
  };

  return (
    <div class="history-view">
      <div class="history-header">
        <div>
          <h2>Session History</h2>
          <div class="history-summary">
            <span>{sessions()?.length ?? 0} archived sessions</span>
            <span>{pinnedIds().length} pinned</span>
            <Show when={outputSearchEnabled()}>
              <span>{searchResults()?.length ?? 0} output matches</span>
            </Show>
          </div>
        </div>
        <button onClick={refresh}>Refresh</button>
      </div>

      <div class="history-toolbar">
        <div class="history-filter-grid">
          <label class="history-field history-field-wide">
            <span>Filter sessions</span>
            <input
              type="search"
              class="history-search"
              placeholder="Name, cwd, command"
              value={metadataQuery()}
              onInput={(event) => setMetadataQuery(event.currentTarget.value)}
            />
          </label>
          <label class="history-field history-field-wide">
            <span>Search archived output</span>
            <input
              type="search"
              class="history-search"
              placeholder="Needle inside scrollback"
              value={outputQuery()}
              onInput={(event) => setOutputQuery(event.currentTarget.value)}
            />
          </label>
          <label class="history-field">
            <span>Status</span>
            <select
              value={statusFilter()}
              onInput={(event) =>
                setStatusFilter(event.currentTarget.value as StatusFilter)
              }
            >
              <option value="all">All</option>
              <option value="success">Success</option>
              <option value="error">Errored</option>
              <option value="unfinished">No exit code</option>
            </select>
          </label>
          <label class="history-field">
            <span>Sort</span>
            <select
              value={sortMode()}
              onInput={(event) =>
                setSortMode(event.currentTarget.value as SortMode)
              }
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
      </div>

      <div class="history-layout">
        <div class="history-sidebar">
          <Show
            when={!outputSearchEnabled()}
            fallback={
              <div class="history-search-results">
                <Show
                  when={!searchResults.loading}
                  fallback={<div class="history-loading">Searching archived output...</div>}
                >
                  <Show
                    when={(searchResults()?.length ?? 0) > 0}
                    fallback={<div class="history-empty">No output matches found.</div>}
                  >
                    <For each={searchResults()}>
                      {(result) => (
                        <button
                          class={`history-search-result ${
                            selectedId() === result.session_id ? "active" : ""
                          }`}
                          onClick={() => setSelectedId(result.session_id)}
                        >
                          <div class="history-result-header">
                            <strong>{result.session_name}</strong>
                            <span class="history-result-date">
                              {formatDate(result.created_at)}
                            </span>
                          </div>
                          <div
                            class="history-result-snippet"
                            innerHTML={result.match_snippet}
                          />
                        </button>
                      )}
                    </For>
                  </Show>
                </Show>
              </div>
            }
          >
            <div class="history-list">
              <Show
                when={!sessions.loading}
                fallback={<div class="history-loading">Loading archived sessions...</div>}
              >
                <Show
                  when={filteredSessions().length > 0}
                  fallback={<div class="history-empty">No archived sessions match these filters.</div>}
                >
                  <For each={filteredSessions()}>
                    {(session) => {
                      const pinned = createMemo(() =>
                        pinnedIds().includes(session.id),
                      );
                      return (
                        <button
                          class={`history-session ${
                            selectedId() === session.id ? "active" : ""
                          }`}
                          onClick={() => setSelectedId(session.id)}
                          title={session.cwd || session.name}
                        >
                          <div class="history-session-header">
                            <div class="history-session-name-row">
                              <span class="history-session-name">{session.name}</span>
                              <Show when={pinned()}>
                                <span class="history-pin-indicator">Pinned</span>
                              </Show>
                            </div>
                            <span
                              class={`history-exit-code ${
                                session.exit_code === 0 ? "success" : session.exit_code === null ? "" : "error"
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
              </Show>
            </div>
          </Show>
        </div>

        <div class="history-detail">
          <Show
            when={selectedId()}
            fallback={<div class="history-empty">Select an archived session to inspect it.</div>}
          >
            <Show
              when={!selectedSession.loading && selectedSession()}
              fallback={<div class="history-loading">Loading archived session...</div>}
            >
              {(session) => (
                <>
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
                      <button onClick={() => togglePin(session().id)}>
                        {pinnedIds().includes(session().id) ? "Unpin" : "Pin"}
                      </button>
                      <button onClick={() => void exportSession(session())}>Export</button>
                      <button
                        class="danger"
                        onClick={() => void deleteSession(session())}
                      >
                        Delete
                      </button>
                    </div>
                  </div>

                  <div class="history-detail-grid">
                    <div class="history-detail-card">
                      <div class="history-detail-label">Working directory</div>
                      <div class="history-detail-value">
                        {session().cwd || "Unknown"}
                      </div>
                    </div>
                    <div class="history-detail-card">
                      <div class="history-detail-label">Command</div>
                      <div class="history-detail-value">
                        {session().command || "Default shell"}
                      </div>
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
                          {(result) => (
                            <div
                              class="history-result-snippet"
                              innerHTML={result.match_snippet}
                            />
                          )}
                        </For>
                      </div>
                    </div>
                  </Show>

                  <div class="history-replay-toolbar">
                    <div>
                      <strong>Replay preview</strong>
                      <div class="history-replay-note">
                        Tail view of the archived raw terminal log.
                      </div>
                    </div>
                    <label class="history-field">
                      <span>Tail size</span>
                      <select
                        value={String(tailBytes())}
                        onInput={(event) =>
                          setTailBytes(Number.parseInt(event.currentTarget.value, 10))
                        }
                      >
                        <option value={16 * 1024}>16 KB</option>
                        <option value={64 * 1024}>64 KB</option>
                        <option value={256 * 1024}>256 KB</option>
                      </select>
                    </label>
                  </div>

                  <div class="history-replay">
                    <Show
                      when={!logPreview.loading}
                      fallback={<div class="history-loading">Loading archived output...</div>}
                    >
                      <Show
                        when={logPreview()}
                        fallback={<div class="history-empty">No archived output available.</div>}
                      >
                        <div class="history-replay-meta">
                          <span>
                            Showing {formatSize(logPreview()!.bytes)} of{" "}
                            {formatSize(logPreview()!.total_bytes)}
                          </span>
                          <Show when={logPreview()!.truncated}>
                            <span>Preview is truncated to the requested tail size.</span>
                          </Show>
                        </div>
                        <pre class="history-replay-output">
                          {logPreview()!.text || "No output captured."}
                        </pre>
                      </Show>
                    </Show>
                  </div>
                </>
              )}
            </Show>
          </Show>
        </div>
      </div>
    </div>
  );
};

export default History;
