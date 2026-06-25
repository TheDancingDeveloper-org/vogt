import { Component, For, Show, createResource, createSignal } from "solid-js";
import { api } from "./api";

interface SessionMetadata {
  id: string;
  name: string;
  created_at: string;
  ended_at: string | null;
  exit_code: number | null;
  cwd: string | null;
  command: string | null;
  scrollback_bytes: number;
}

interface SearchResult {
  session_id: string;
  session_name: string;
  created_at: string;
  match_snippet: string;
  rank: number;
}

interface Props {
  onError?: (message: string) => void;
}

const History: Component<Props> = (props) => {
  const [searchQuery, setSearchQuery] = createSignal("");
  const [isSearchMode, setIsSearchMode] = createSignal(false);

  // List recent sessions
  const [sessions, { refetch: refetchSessions }] = createResource(async () => {
    if (isSearchMode()) return [];
    try {
      const response = await fetch(`${api.getBase()}/api/history/sessions?limit=50`, {
        headers: { Authorization: `Bearer ${api.getToken()}` },
      });
      if (!response.ok) throw new Error(`HTTP ${response.status}`);
      return (await response.json()) as SessionMetadata[];
    } catch (e) {
      props.onError?.(`Failed to load history: ${(e as Error).message}`);
      return [];
    }
  });

  // Search sessions
  const [searchResults, { refetch: refetchSearch }] = createResource(
    searchQuery,
    async (query) => {
      if (!query.trim() || !isSearchMode()) return [];
      try {
        const response = await fetch(
          `${api.getBase()}/api/history/search?q=${encodeURIComponent(query)}`,
          {
            headers: { Authorization: `Bearer ${api.getToken()}` },
          }
        );
        if (!response.ok) throw new Error(`HTTP ${response.status}`);
        return (await response.json()) as SearchResult[];
      } catch (e) {
        props.onError?.(`Search failed: ${(e as Error).message}`);
        return [];
      }
    }
  );

  const handleSearch = (query: string) => {
    setSearchQuery(query);
    setIsSearchMode(query.trim().length > 0);
    if (query.trim().length > 0) {
      refetchSearch();
    } else {
      refetchSessions();
    }
  };

  const formatDate = (dateStr: string) => {
    try {
      const date = new Date(dateStr);
      return date.toLocaleString();
    } catch {
      return dateStr;
    }
  };

  const formatSize = (bytes: number) => {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  };

  const deleteSession = async (id: string) => {
    try {
      const response = await fetch(`${api.getBase()}/api/history/${id}`, {
        method: "DELETE",
        headers: { Authorization: `Bearer ${api.getToken()}` },
      });
      if (!response.ok) throw new Error(`HTTP ${response.status}`);
      refetchSessions();
    } catch (e) {
      props.onError?.(`Delete failed: ${(e as Error).message}`);
    }
  };

  return (
    <div class="history-view">
      <div class="history-header">
        <h2>Session History</h2>
        <input
          type="search"
          class="history-search"
          placeholder="Search session output..."
          value={searchQuery()}
          onInput={(e) => handleSearch(e.currentTarget.value)}
        />
      </div>

      <Show
        when={!isSearchMode()}
        fallback={
          <div class="history-search-results">
            <Show
              when={!searchResults.loading && searchResults()}
              fallback={<div class="history-loading">Searching...</div>}
            >
              <Show
                when={searchResults()!.length > 0}
                fallback={<div class="history-empty">No matches found</div>}
              >
                <For each={searchResults()}>
                  {(result) => (
                    <div class="history-search-result">
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
                    </div>
                  )}
                </For>
              </Show>
            </Show>
          </div>
        }
      >
        <div class="history-list">
          <Show
            when={!sessions.loading && sessions()}
            fallback={<div class="history-loading">Loading sessions...</div>}
          >
            <Show
              when={sessions()!.length > 0}
              fallback={
                <div class="history-empty">
                  No archived sessions yet. Sessions are automatically archived when they exit.
                </div>
              }
            >
              <For each={sessions()}>
                {(session) => (
                  <div class="history-session">
                    <div class="history-session-header">
                      <div class="history-session-name">{session.name}</div>
                      <button
                        class="history-delete-btn"
                        onClick={() => {
                          if (confirm(`Delete archived session "${session.name}"?`)) {
                            void deleteSession(session.id);
                          }
                        }}
                        title="Delete"
                      >
                        ×
                      </button>
                    </div>
                    <div class="history-session-meta">
                      <span>Created: {formatDate(session.created_at)}</span>
                      <Show when={session.ended_at}>
                        <span>Ended: {formatDate(session.ended_at!)}</span>
                      </Show>
                      <Show when={session.exit_code !== null}>
                        <span
                          class={`history-exit-code ${
                            session.exit_code === 0 ? "success" : "error"
                          }`}
                        >
                          Exit: {session.exit_code}
                        </span>
                      </Show>
                      <span>Size: {formatSize(session.scrollback_bytes)}</span>
                    </div>
                    <Show when={session.cwd}>
                      <div class="history-session-cwd">📁 {session.cwd}</div>
                    </Show>
                    <Show when={session.command}>
                      <div class="history-session-command">
                        <code>{session.command}</code>
                      </div>
                    </Show>
                  </div>
                )}
              </For>
            </Show>
          </Show>
        </div>
      </Show>
    </div>
  );
};

export default History;
