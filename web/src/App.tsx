import {
  Component,
  For,
  Show,
  createMemo,
  createSignal,
  onCleanup,
  onMount,
} from "solid-js";
import { useNavigate, useParams } from "@solidjs/router";
import Terminal from "./Terminal";
import ModKeyRow from "./ModKeyRow";
import Settings from "./Settings";
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

const App: Component = () => {
  const navigate = useNavigate();
  const params = useParams<{ id?: string }>();
  const [drawerOpen, setDrawerOpen] = createSignal(false);
  const [settingsOpen, setSettingsOpen] = createSignal(false);
  const [toast, setToast] = createSignal<string | null>(null);
  const showToast = (m: string) => {
    setToast(m);
    setTimeout(() => setToast(null), 2500);
  };

  // Single shared sender so the modkey row can write into the focused PTY.
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

  const activeId = createMemo(() => params.id ?? null);

  const orderedSessions = createMemo<SessionSummary[]>(() =>
    sessionsStore.order
      .map((id) => sessionsStore.sessions[id])
      .filter((s): s is SessionSummary => Boolean(s)),
  );

  const onCreate = async () => {
    const name = prompt("Session name", `shell-${Date.now() % 1000}`);
    if (!name) return;
    try {
      const s = await createSession(name);
      navigate(`/t/${s.id}`, { replace: false });
      setDrawerOpen(false);
    } catch (e) {
      showToast(`create failed: ${(e as Error).message}`);
    }
  };

  const onRename = async (s: SessionSummary) => {
    const name = prompt("Rename session", s.name);
    if (!name || name === s.name) return;
    try {
      await renameSession(s.id, name);
    } catch (e) {
      showToast(`rename failed: ${(e as Error).message}`);
    }
  };

  const onClose = async (s: SessionSummary) => {
    if (!confirm(`Kill and remove "${s.name}"?`)) return;
    try {
      // Best-effort kill first; ignore if already dead.
      try {
        await killSession(s.id);
      } catch {
        /* may already be dead */
      }
      await deleteSession(s.id);
      if (activeId() === s.id) navigate("/", { replace: true });
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
            <For each={orderedSessions()} fallback={<div class="empty">No sessions yet.</div>}>
              {(s) => (
                <div
                  class={`session-row ${activeId() === s.id ? "active" : ""}`}
                  onClick={() => {
                    navigate(`/t/${s.id}`);
                    setDrawerOpen(false);
                  }}
                >
                  <span class={`activity-dot ${activityClass(s)}`} title={activityLabel(s.activity, s.exit_code)} />
                  <span class="name">{s.name}</span>
                  <span class="meta">{(s.scrollback_bytes / 1024).toFixed(0)}k</span>
                </div>
              )}
            </For>
          </div>
        </aside>

        <div class="tab-strip">
          <button
            class="menu-btn"
            onClick={() => setDrawerOpen((v) => !v)}
            aria-label="Toggle drawer"
          >
            ☰
          </button>
          <For each={orderedSessions()}>
            {(s) => (
              <button
                class={`tab ${activeId() === s.id ? "active" : ""}`}
                onClick={() => navigate(`/t/${s.id}`)}
                onContextMenu={(e) => {
                  e.preventDefault();
                  onRename(s);
                }}
                title={activityLabel(s.activity, s.exit_code)}
              >
                <span class={`activity-dot ${activityClass(s)}`} />
                <span class="label">{s.name}</span>
                <span
                  class="close"
                  onClick={(e) => {
                    e.stopPropagation();
                    void onClose(s);
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
            <Show
              when={activeId() && sessionsStore.sessions[activeId()!]}
              fallback={
                <div class="empty">
                  <div>Select or create a session to begin.</div>
                  <button onClick={onCreate}>+ New session</button>
                </div>
              }
            >
              <Terminal
                sessionId={activeId()!}
                registerSend={(fn) => {
                  activeSend = (data) => fn(data);
                }}
              />
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
