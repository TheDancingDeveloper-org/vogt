import {
  Component,
  For,
  Show,
  createMemo,
  createSignal,
  onCleanup,
  onMount,
} from "solid-js";
import { useLocation, useNavigate, useParams } from "@solidjs/router";
import Terminal from "./Terminal";
import Editor from "./Editor";
import ModKeyRow from "./ModKeyRow";
import Settings from "./Settings";
import FileTree from "./FileTree";
import {
  createSession,
  deleteSession,
  isConnected,
  killSession,
  refreshSessions,
  renameSession,
  sessionsError,
  sessionsStore,
  startEventStream,
  stopEventStream,
} from "./store";
import {
  closeTab,
  focusTab,
  openEditorTab,
  openTerminalTab,
  tabsStore,
  type Tab,
} from "./tabs";
import type { ActivityState, SessionSummary } from "./api";
import { getToken } from "./api";

function activityClass(s: SessionSummary): string {
  if (s.exit_code !== null) {
    return s.exit_code === 0 ? "done" : "errored";
  }
  return s.activity;
}

function activityLabel(s: ActivityState, exit: number | null): string {
  if (exit !== null) return exit === 0 ? "exited (0)" : `errored (${exit})`;
  switch (s) {
    case "waiting-for-input":
      return "waiting for input";
    default:
      return s;
  }
}

function tabActivityClass(tab: Tab): string | null {
  if (tab.kind !== "terminal") return null;
  const s = sessionsStore.sessions[tab.sessionId];
  return s ? activityClass(s) : "idle";
}

const App: Component = () => {
  const navigate = useNavigate();
  const params = useParams<{ id?: string; path?: string }>();
  const location = useLocation();
  const [drawerOpen, setDrawerOpen] = createSignal(false);
  const [settingsOpen, setSettingsOpen] = createSignal(false);
  const [toast, setToast] = createSignal<string | null>(null);
  const showToast = (m: string) => {
    setToast(m);
    setTimeout(() => setToast(null), 2500);
  };

  let activeSend: ((data: string) => void) | null = null;

  onMount(() => {
    if (!getToken()) {
      setSettingsOpen(true);
      return;
    }
    void refreshSessions();
    startEventStream();
  });

  onCleanup(() => stopEventStream());

  // Sync URL → tabs. /t/:id focuses or opens a terminal tab; /e/:path opens
  // an editor tab. Empty route keeps whichever tab was last active.
  createMemo(() => {
    const path = location.pathname;
    if (path.startsWith("/t/") && params.id) {
      const sess = sessionsStore.sessions[params.id];
      const label = sess?.name ?? params.id.slice(0, 6);
      openTerminalTab(params.id, label);
    } else if (path.startsWith("/e/") && params.path) {
      // Decode the wildcard segment back into a real path.
      openEditorTab(decodeURIComponent(params.path));
    }
  });

  const onCreate = async () => {
    const name = prompt("Session name", `shell-${Date.now() % 1000}`);
    if (!name) return;
    try {
      const s = await createSession(name);
      openTerminalTab(s.id, s.name);
      navigate(`/t/${s.id}`, { replace: false });
      setDrawerOpen(false);
    } catch (e) {
      showToast(`create failed: ${(e as Error).message}`);
    }
  };

  const onRenameSession = async (s: SessionSummary) => {
    const name = prompt("Rename session", s.name);
    if (!name || name === s.name) return;
    try {
      await renameSession(s.id, name);
    } catch (e) {
      showToast(`rename failed: ${(e as Error).message}`);
    }
  };

  const onCloseSession = async (s: SessionSummary) => {
    if (!confirm(`Kill and remove "${s.name}"?`)) return;
    try {
      try {
        await killSession(s.id);
      } catch {
        /* may already be dead */
      }
      await deleteSession(s.id);
      // Also close any tab pointing to this session.
      const id = `term:${s.id}`;
      closeTab(id);
    } catch (e) {
      showToast(`close failed: ${(e as Error).message}`);
    }
  };

  return (
    <>
      <div class="app">
        <Show when={drawerOpen()}>
          <div class="drawer-scrim" onClick={() => setDrawerOpen(false)} />
        </Show>

        <aside class={`drawer ${drawerOpen() ? "open" : ""}`}>
          <div class="drawer-header">
            <span>MyDevEnv2</span>
            <span style={{ color: isConnected() ? "#7ee787" : "#ff7b72" }}>
              {isConnected() ? "●" : "○"}
            </span>
          </div>
          <div class="drawer-actions">
            <button onClick={onCreate}>+ New session</button>
            <button onClick={() => setSettingsOpen(true)} title="Settings">
              ⚙
            </button>
          </div>
          <Show when={sessionsError()}>
            <div style={{ padding: "8px 10px", color: "#ff7b72", "font-size": "12px" }}>
              {sessionsError()}
            </div>
          </Show>
          <div class="session-list">
            <For
              each={sessionsStore.order
                .map((id) => sessionsStore.sessions[id])
                .filter((s): s is SessionSummary => Boolean(s))}
              fallback={<div class="empty">No sessions yet.</div>}
            >
              {(s) => (
                <div
                  class={`session-row ${
                    tabsStore.active === `term:${s.id}` ? "active" : ""
                  }`}
                  onClick={() => {
                    openTerminalTab(s.id, s.name);
                    navigate(`/t/${s.id}`);
                    setDrawerOpen(false);
                  }}
                  onContextMenu={(e) => {
                    e.preventDefault();
                    onRenameSession(s);
                  }}
                >
                  <span
                    class={`activity-dot ${activityClass(s)}`}
                    title={activityLabel(s.activity, s.exit_code)}
                  />
                  <span class="name">{s.name}</span>
                  <span
                    class="close"
                    title="Kill & remove"
                    onClick={(e) => {
                      e.stopPropagation();
                      void onCloseSession(s);
                    }}
                  >
                    ×
                  </span>
                </div>
              )}
            </For>
          </div>
          <FileTree onOpen={() => setDrawerOpen(false)} />
        </aside>

        <div class="tab-strip">
          <button
            class="menu-btn"
            onClick={() => setDrawerOpen((v) => !v)}
            aria-label="Toggle drawer"
          >
            ☰
          </button>
          <For each={tabsStore.tabs}>
            {(t) => (
              <button
                class={`tab ${tabsStore.active === t.id ? "active" : ""}`}
                onClick={() => {
                  focusTab(t.id);
                  if (t.kind === "terminal") navigate(`/t/${t.sessionId}`);
                  else navigate(`/e/${encodeURIComponent(t.path)}`);
                }}
              >
                <Show when={t.kind === "terminal"}>
                  <span class={`activity-dot ${tabActivityClass(t) ?? "idle"}`} />
                </Show>
                <span class="label">
                  {t.kind === "editor" ? "📄 " : ""}
                  {t.label}
                </span>
                <Show when={t.kind === "editor" && t.dirty}>
                  <span class="dirty-dot" title="unsaved changes" />
                </Show>
                <span
                  class="close"
                  onClick={(e) => {
                    e.stopPropagation();
                    closeTab(t.id);
                  }}
                >
                  ×
                </span>
              </button>
            )}
          </For>
        </div>

        <main class="main">
          <div class="tab-view">
            <For each={tabsStore.tabs}>
              {(t) => (
                <div
                  style={{
                    display: tabsStore.active === t.id ? "flex" : "none",
                    "flex-direction": "column",
                    flex: 1,
                    "min-height": 0,
                    "min-width": 0,
                  }}
                >
                  <Show when={t.kind === "terminal" && t.kind === "terminal" && t}>
                    {(tab) => (
                      <Terminal
                        sessionId={(tab() as Extract<Tab, { kind: "terminal" }>).sessionId}
                        registerSend={(fn) => {
                          if (tabsStore.active === t.id) {
                            activeSend = (data) => fn(data);
                          }
                        }}
                      />
                    )}
                  </Show>
                  <Show when={t.kind === "editor" && t}>
                    {(tab) => (
                      <Editor
                        tabId={tab().id}
                        path={(tab() as Extract<Tab, { kind: "editor" }>).path}
                      />
                    )}
                  </Show>
                </div>
              )}
            </For>
            <Show when={tabsStore.tabs.length === 0}>
              <div class="empty">
                <div>Open a file from the drawer or create a session.</div>
                <button onClick={onCreate}>+ New session</button>
              </div>
            </Show>
          </div>
          <ModKeyRow send={(d) => activeSend?.(d)} />
        </main>
      </div>

      <Settings open={settingsOpen()} onClose={() => setSettingsOpen(false)} />

      <Show when={toast()}>
        <div class="toast">{toast()}</div>
      </Show>
    </>
  );
};

export default App;
