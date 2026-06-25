import {
  Component,
  For,
  Show,
  createEffect,
  createSignal,
  onCleanup,
  onMount,
  untrack,
} from "solid-js";
import { useLocation, useNavigate, useParams } from "@solidjs/router";
import type { TerminalActions } from "./Terminal";
import TerminalWorkspace from "./TerminalWorkspace";
import Editor from "./Editor";
import EditorWorkspace from "./EditorWorkspace";
import GitTab from "./Git";
import GuiTab from "./Gui";
import History from "./History";
import KeyboardShortcuts from "./KeyboardShortcuts";
import ModKeyRow from "./ModKeyRow";
import Settings from "./Settings";
import FileTree from "./FileTree";
import CommandPalette from "./CommandPalette";
import TemplateSelector from "./TemplateSelector";
import { getLayoutMode } from "./layout";
import { api as apiModule } from "./api";
import type { PublicConfig, SessionTemplate } from "./api";
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
  openGitTab,
  openGuiTab,
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

function pathFor(tab: Tab): string {
  if (tab.kind === "terminal") return `/t/${tab.sessionId}`;
  if (tab.kind === "editor") return `/e/${encodeURIComponent(tab.path)}`;
  if (tab.kind === "git") return `/g/${encodeURIComponent(tab.repo)}`;
  return "/gui";
}

const App: Component = () => {
  const navigate = useNavigate();
  const params = useParams<{ id?: string; path?: string }>();
  const location = useLocation();
  const [drawerOpen, setDrawerOpen] = createSignal(false);
  const [settingsOpen, setSettingsOpen] = createSignal(false);
  const [commandPaletteOpen, setCommandPaletteOpen] = createSignal(false);
  const [templateSelectorOpen, setTemplateSelectorOpen] = createSignal(false);
  const [shortcutsOpen, setShortcutsOpen] = createSignal(false);

  // Toast: errors stay longer than info messages.
  const [toast, setToast] = createSignal<string | null>(null);
  let toastTimer: number | undefined;
  const showToast = (m: string, opts: { kind?: "info" | "error" } = {}) => {
    if (toastTimer !== undefined) window.clearTimeout(toastTimer);
    setToast(m);
    const ms = opts.kind === "error" ? 6000 : 2500;
    toastTimer = window.setTimeout(() => setToast(null), ms);
  };

  // Per-tab terminal action registry. Solid keeps Terminal components mounted
  // across tab switches, so we register actions by tab id on mount and read
  // them via a getter keyed on the active tab. This avoids the stale-closure
  // problem of single mutable `activeSend` / `activeCopy` refs.
  const senders = new Map<string, (data: string | ArrayBuffer) => void>();
  const actions = new Map<string, TerminalActions>();
  const activeSend = (data: string) => {
    const id = tabsStore.active;
    if (id) senders.get(id)?.(data);
  };
  const activeCopy = async () => {
    const id = tabsStore.active;
    if (id) await actions.get(id)?.copy();
  };
  const activePaste = async () => {
    const id = tabsStore.active;
    if (id) await actions.get(id)?.paste();
  };

  // Custom prompt / confirm modals — native `prompt()` / `confirm()` are
  // blocked or ignored in some PWA contexts (iOS standalone) and look out of
  // place on mobile. These promises resolve when the user picks an option.
  interface PromptReq {
    title: string;
    defaultValue?: string;
    placeholder?: string;
    resolve: (v: string | null) => void;
  }
  interface ConfirmReq {
    title: string;
    body?: string;
    resolve: (ok: boolean) => void;
  }
  const [promptReq, setPromptReq] = createSignal<PromptReq | null>(null);
  const [confirmReq, setConfirmReq] = createSignal<ConfirmReq | null>(null);
  const [promptDraft, setPromptDraft] = createSignal("");
  const promptUser = (
    title: string,
    defaultValue = "",
    placeholder = "",
  ): Promise<string | null> => {
    setPromptDraft(defaultValue);
    return new Promise((resolve) =>
      setPromptReq({ title, defaultValue, placeholder, resolve }),
    );
  };
  const confirmUser = (title: string, body?: string): Promise<boolean> => {
    return new Promise((resolve) => setConfirmReq({ title, body, resolve }));
  };

  const [publicCfg, setPublicCfg] = createSignal<PublicConfig | null>(null);
  const layoutMode = getLayoutMode();

  // Check if we're in IDE mode
  const isIDEMode = layoutMode === "ide";

  onMount(() => {
    apiModule
      .publicConfig()
      .then((c) => setPublicCfg(c))
      .catch(() => {
        /* server may be down; non-fatal */
      });

    if (!getToken()) {
      setSettingsOpen(true);
      return;
    }
    void refreshSessions();
    startEventStream();
  });

  onCleanup(() => {
    stopEventStream();
    if (toastTimer !== undefined) window.clearTimeout(toastTimer);
  });

  // URL → tabs syncing. createEffect (not createMemo) — we want side effects,
  // not a memoised value.
  createEffect(() => {
    const path = location.pathname;
    if (path.startsWith("/t/") && params.id) {
      const sess = sessionsStore.sessions[params.id];
      // Don't auto-create phantoms for ids the server doesn't know about.
      if (!sess && sessionsStore.ready) return;
      const label = sess?.name ?? params.id.slice(0, 6);
      openTerminalTab(params.id, label);
    } else if (path.startsWith("/e/") && params.path) {
      openEditorTab(decodeURIComponent(params.path));
    } else if (path.startsWith("/g/") && params.path !== undefined) {
      openGitTab(decodeURIComponent(params.path));
    } else if (path === "/g") {
      openGitTab("");
    } else if (path === "/gui") {
      openGuiTab();
    }
  });

  const onCreate = async (cwd?: string, template?: SessionTemplate) => {
    // If template selector should be shown
    if (!template && publicCfg()?.session_templates && publicCfg()!.session_templates!.length > 1) {
      setTemplateSelectorOpen(true);
      return;
    }

    // Default session creation (no template or single template)
    const name = await promptUser(
      cwd ? `New session in ${cwd}` : "New session",
      `shell-${Date.now() % 1000}`,
      "name",
    );
    if (!name) return;
    try {
      const s = await createSession(
        name,
        template?.command || undefined,
        cwd,
        template?.env,
      );
      openTerminalTab(s.id, s.name);
      navigate(`/t/${s.id}`, { replace: false });
      setDrawerOpen(false);
    } catch (e) {
      showToast(`create failed: ${(e as Error).message}`, { kind: "error" });
    }
  };

  const onTemplateSelect = async (template: SessionTemplate, name: string, cwd?: string) => {
    setTemplateSelectorOpen(false);
    try {
      const s = await createSession(
        name,
        template.command || undefined,
        cwd || template.cwd || undefined,
        template.env,
      );
      openTerminalTab(s.id, s.name);
      navigate(`/t/${s.id}`, { replace: false });
      setDrawerOpen(false);
    } catch (e) {
      showToast(`create failed: ${(e as Error).message}`, { kind: "error" });
    }
  };

  const onRenameSession = async (s: SessionSummary) => {
    const name = await promptUser("Rename session", s.name);
    if (!name || name === s.name) return;
    try {
      await renameSession(s.id, name);
    } catch (e) {
      showToast(`rename failed: ${(e as Error).message}`, { kind: "error" });
    }
  };

  const onDuplicateSession = async (s: SessionSummary) => {
    try {
      const name = `${s.name}-copy`;
      const dup = await createSession(name, undefined, s.cwd || undefined);
      openTerminalTab(dup.id, dup.name);
      navigate(`/t/${dup.id}`);
      setDrawerOpen(false);
    } catch (e) {
      showToast(`duplicate failed: ${(e as Error).message}`, { kind: "error" });
    }
  };

  const closeTabAndNavigate = (tabId: string) => {
    const tab = tabsStore.tabs.find((t) => t.id === tabId);
    const onThisTab =
      (location.pathname.startsWith("/t/") && `term:${params.id}` === tabId) ||
      (location.pathname.startsWith("/e/") &&
        `edit:${decodeURIComponent(params.path ?? "")}` === tabId) ||
      (location.pathname.startsWith("/g") &&
        `git:${decodeURIComponent(params.path ?? "")}` === tabId) ||
      (location.pathname === "/gui" && tabId === "gui");

    // Drop any per-tab registrations so we don't leak references.
    senders.delete(tabId);
    actions.delete(tabId);

    closeTab(tabId);
    if (tab) {
      // Persist tab list — already done inside closeTab. No-op here.
    }
    if (!onThisTab) return;
    const next = tabsStore.tabs.find((t) => t.id === tabsStore.active);
    navigate(next ? pathFor(next) : "/", { replace: true });
  };

  // Once the server session list is loaded, remove tabs for sessions that no
  // longer exist (e.g. after a server restart). Runs exactly once: sessionsStore.ready
  // is the only tracked dependency. tabsStore and sessionsStore.sessions are read via
  // untrack so that subsequent tab opens/session arrivals don't re-trigger this and
  // incorrectly treat a brand-new tab (whose SSE event hasn't arrived yet) as stale.
  createEffect(() => {
    if (!sessionsStore.ready) return;
    const stale = untrack(() =>
      tabsStore.tabs.filter(
        (t) => t.kind === "terminal" && !sessionsStore.sessions[(t as Extract<Tab, { kind: "terminal" }>).sessionId],
      )
    );
    for (const tab of stale) {
      closeTabAndNavigate(tab.id);
    }
  });

  const requestCloseTab = async (tabId: string) => {
    const tab = tabsStore.tabs.find((t) => t.id === tabId);
    if (tab && tab.kind === "editor" && tab.dirty) {
      const ok = await confirmUser(
        "Discard unsaved changes?",
        `${tab.path} has unsaved edits.`,
      );
      if (!ok) return;
    }
    closeTabAndNavigate(tabId);
  };

  const onCloseSession = async (s: SessionSummary) => {
    const ok = await confirmUser(
      `Kill and remove "${s.name}"?`,
      "The shell and its scrollback will be discarded.",
    );
    if (!ok) return;
    closeTabAndNavigate(`term:${s.id}`);
    try {
      try {
        await killSession(s.id);
      } catch {
        /* may already be dead */
      }
      await deleteSession(s.id);
    } catch (e) {
      showToast(`close failed: ${(e as Error).message}`, { kind: "error" });
    }
  };

  // Keyboard shortcuts. Browser reserves Ctrl+T / Ctrl+W / Ctrl+Tab so we use
  // Ctrl+Shift+T (new), Ctrl+Shift+W (close active), Ctrl+Alt+Arrow (cycle).
  // Ctrl+K / Cmd+K opens the command palette.
  const onKeyDown = (e: KeyboardEvent) => {
    // Command palette: Ctrl+K or Cmd+K
    if ((e.ctrlKey || e.metaKey) && e.key === "k") {
      e.preventDefault();
      setCommandPaletteOpen(true);
      return;
    }

    if (!e.ctrlKey && !e.metaKey) return;
    const k = e.key.toLowerCase();
    if (e.shiftKey && k === "t") {
      e.preventDefault();
      void onCreate();
      return;
    }
    if (e.shiftKey && k === "w") {
      const active = tabsStore.active;
      if (active) {
        e.preventDefault();
        void requestCloseTab(active);
      }
      return;
    }
    if (e.altKey && (k === "arrowright" || k === "arrowleft")) {
      const tabs = tabsStore.tabs;
      if (tabs.length === 0) return;
      const idx = Math.max(
        0,
        tabs.findIndex((t) => t.id === tabsStore.active),
      );
      const delta = k === "arrowright" ? 1 : -1;
      const next = tabs[(idx + delta + tabs.length) % tabs.length];
      if (next) {
        e.preventDefault();
        focusTab(next.id);
        navigate(pathFor(next));
      }
    }
  };
  onMount(() => window.addEventListener("keydown", onKeyDown));
  onCleanup(() => window.removeEventListener("keydown", onKeyDown));

  return (
    <>
      <div class="app">
        <Show when={drawerOpen()}>
          <div
            class="drawer-scrim"
            onPointerDown={() => setDrawerOpen(false)}
          />
        </Show>

        <aside class={`drawer ${drawerOpen() ? "open" : ""}`}>
          <div class="drawer-header">
            <span>MyDevEnv2</span>
            <span style={{ color: isConnected() ? "#7ee787" : "#ff7b72" }}>
              {isConnected() ? "●" : "○"}
            </span>
          </div>
          <div class="drawer-actions">
            <button onClick={() => onCreate()}>+ Session</button>
            <button
              onClick={() => {
                openGitTab("");
                navigate("/g/");
                setDrawerOpen(false);
              }}
              title="Open git tab for workspace root"
            >
              ⎇ Git
            </button>
            <button
              onClick={() => {
                openGuiTab();
                navigate("/gui");
                setDrawerOpen(false);
              }}
              title="Open the GUI stream tab"
            >
              🖥 GUI
            </button>
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
                    void onRenameSession(s);
                  }}
                  title={`${s.name}\ncwd: ${s.cwd}`}
                >
                  <span
                    class={`activity-dot ${activityClass(s)}`}
                    title={activityLabel(s.activity, s.exit_code)}
                  />
                  <div class="session-row-body">
                    <span class="name">{s.name}</span>
                    <Show when={s.cwd}>
                      <span class="cwd">{s.cwd}</span>
                    </Show>
                  </div>
                  <span
                    class="row-btn"
                    title="Duplicate (same cwd)"
                    onClick={(e) => {
                      e.stopPropagation();
                      void onDuplicateSession(s);
                    }}
                  >
                    ⧉
                  </span>
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
          <FileTree
            onOpen={() => setDrawerOpen(false)}
            promptPath={promptUser}
            onError={(message) => showToast(message, { kind: "error" })}
          />
        </aside>

        <div class="tab-strip">
          <button
            class="menu-btn"
            onClick={() => setDrawerOpen((v) => !v)}
            aria-label="Toggle drawer"
          >
            ☰
          </button>
          <button
            class="menu-btn"
            onClick={() => void onCreate()}
            title="New terminal (Ctrl+Shift+T)"
            aria-label="New terminal"
          >
            +
          </button>
          <For each={tabsStore.tabs}>
            {(t) => (
              <button
                class={`tab ${tabsStore.active === t.id ? "active" : ""}`}
                onClick={() => {
                  focusTab(t.id);
                  navigate(pathFor(t));
                }}
                onAuxClick={(e) => {
                  // Middle-click closes the tab (browser/editor convention).
                  if (e.button === 1) {
                    e.preventDefault();
                    void requestCloseTab(t.id);
                  }
                }}
                title={t.label}
              >
                <Show when={t.kind === "terminal"}>
                  <span class={`activity-dot ${tabActivityClass(t) ?? "idle"}`} />
                </Show>
                <span class="label">
                  {t.kind === "editor"
                    ? "📄 "
                    : t.kind === "git"
                      ? "⎇ "
                      : t.kind === "gui"
                        ? "🖥 "
                        : ""}
                  {t.label}
                </span>
                <Show when={t.kind === "editor" && t.dirty}>
                  <span class="dirty-dot" title="unsaved changes" />
                </Show>
                <span
                  class="close"
                  onClick={(e) => {
                    e.stopPropagation();
                    void requestCloseTab(t.id);
                  }}
                >
                  ×
                </span>
              </button>
            )}
          </For>
        </div>

        <main class="main">
          <Show
            when={!isIDEMode}
            fallback={
              <EditorWorkspace
                onNotify={(message, kind) => showToast(message, { kind })}
              />
            }
          >
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
                      <TerminalWorkspace
                        tabId={tab().id}
                        sessionId={(tab() as Extract<Tab, { kind: "terminal" }>).sessionId}
                        registerSend={(fn) => {
                          if (fn) senders.set(t.id, fn);
                          else senders.delete(t.id);
                        }}
                        registerActions={(a) => {
                          if (a) actions.set(t.id, a);
                          else actions.delete(t.id);
                        }}
                        confirmClosePane={(session) =>
                          confirmUser(
                            session
                              ? `Kill pane "${session.name}"?`
                              : "Kill pane?",
                            "The shell and its scrollback will be discarded.",
                          )
                        }
                        onError={(message) =>
                          showToast(message, { kind: "error" })
                        }
                        onNotify={(message, kind) =>
                          showToast(message, { kind })
                        }
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
                  <Show when={t.kind === "git" && t}>
                    {(tab) => (
                      <GitTab repo={(tab() as Extract<Tab, { kind: "git" }>).repo} />
                    )}
                  </Show>
                  <Show when={t.kind === "gui"}>
                    <GuiTab streamUrl={publicCfg()?.gui_stream_url ?? null} />
                  </Show>
                  <Show when={t.kind === "history"}>
                    <History onError={(msg) => showToast(msg, { kind: "error" })} />
                  </Show>
                </div>
              )}
            </For>
            <Show when={tabsStore.tabs.length === 0}>
              <div class="empty">
                <div>Open a file from the drawer or create a session.</div>
                <button onClick={() => void onCreate()}>+ New session</button>
              </div>
            </Show>
          </div>
          </Show>
          <ModKeyRow
            send={(d) => activeSend(d)}
            onCopy={() => void activeCopy()}
            onPaste={() => void activePaste()}
          />
        </main>
      </div>

      <Settings open={settingsOpen()} onClose={() => setSettingsOpen(false)} />

      <Show when={promptReq()}>
        {(req) => (
          <div
            class="modal-backdrop"
            onPointerDown={() => {
              req().resolve(null);
              setPromptReq(null);
            }}
          >
            <div class="modal" onPointerDown={(e) => e.stopPropagation()}>
              <h2>{req().title}</h2>
              <input
                type="text"
                autofocus
                value={promptDraft()}
                placeholder={req().placeholder ?? ""}
                onInput={(e) => setPromptDraft(e.currentTarget.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") {
                    req().resolve(promptDraft());
                    setPromptReq(null);
                  } else if (e.key === "Escape") {
                    req().resolve(null);
                    setPromptReq(null);
                  }
                }}
              />
              <div class="modal-actions">
                <button
                  onClick={() => {
                    req().resolve(null);
                    setPromptReq(null);
                  }}
                >
                  Cancel
                </button>
                <button
                  onClick={() => {
                    req().resolve(promptDraft());
                    setPromptReq(null);
                  }}
                >
                  OK
                </button>
              </div>
            </div>
          </div>
        )}
      </Show>

      <Show when={confirmReq()}>
        {(req) => (
          <div
            class="modal-backdrop"
            onPointerDown={() => {
              req().resolve(false);
              setConfirmReq(null);
            }}
          >
            <div class="modal" onPointerDown={(e) => e.stopPropagation()}>
              <h2>{req().title}</h2>
              <Show when={req().body}>
                <p style={{ color: "var(--fg-muted)", "font-size": "13px" }}>
                  {req().body}
                </p>
              </Show>
              <div class="modal-actions">
                <button
                  onClick={() => {
                    req().resolve(false);
                    setConfirmReq(null);
                  }}
                >
                  Cancel
                </button>
                <button
                  onClick={() => {
                    req().resolve(true);
                    setConfirmReq(null);
                  }}
                >
                  OK
                </button>
              </div>
            </div>
          </div>
        )}
      </Show>

      <Show when={toast()}>
        <div class="toast">{toast()}</div>
      </Show>

      <CommandPalette
        open={commandPaletteOpen()}
        onClose={() => setCommandPaletteOpen(false)}
        onCreateSession={() => void onCreate()}
        onOpenFile={() => setDrawerOpen(true)}
      />

      <TemplateSelector
        open={templateSelectorOpen()}
        onClose={() => setTemplateSelectorOpen(false)}
        onSelect={onTemplateSelect}
        templates={publicCfg()?.session_templates || []}
      />

      <KeyboardShortcuts
        open={shortcutsOpen()}
        onClose={() => setShortcutsOpen(false)}
      />
    </>
  );
};

export default App;
