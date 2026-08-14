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
import AgentTasks from "./AgentTasks";
import Assistant from "./Assistant";
import Backlog from "./Backlog";
import Board from "./Board";
import GitTab from "./Git";
import GuiTab from "./Gui";
import WorkItemDetail from "./WorkItemDetail";
import History from "./History";
import KeyboardShortcuts from "./KeyboardShortcuts";
import ModKeyRow from "./ModKeyRow";
import Settings from "./Settings";
import FileTree from "./FileTree";
import CommandPalette from "./CommandPalette";
import TemplateSelector from "./TemplateSelector";
import { getLayoutMode, setLayoutMode } from "./layout";
import {
  buildDefaultSessionName,
  mergeTemplates,
  resolveTemplateContext,
  resolveTemplateLaunch,
  sortTemplatesForContext,
  type TemplateContext,
} from "./customTemplates";
import { isBookmarked, toggleBookmark, bookmarks } from "./bookmarks";
import { api as apiModule, ApiError, getBase, validateCredentials } from "./api";
import type { PublicConfig, SessionTemplate } from "./api";
import { subscribeAuthState } from "./api";
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
  openAssistantTab,
  openEditorTab,
  openGitTab,
  openGuiTab,
  openHistoryTab,
  openTasksTab,
  openTerminalTab,
  replaceTabs,
  snapshotTabs,
  tabsStore,
  type Tab,
} from "./tabs";
import type { ActivityState, SessionSummary } from "./api";
import { getToken, setBase, setToken } from "./api";
import {
  deleteWorkspaceLayout,
  getWorkspaceLayout,
  saveWorkspaceLayout,
} from "./workspaceLayouts";

interface LoginScreenProps {
  initialToken: string;
  initialBase: string;
  error: string | null;
  onAuthenticated: (token: string, base: string) => Promise<void>;
}

const LoginScreen: Component<LoginScreenProps> = (props) => {
  const [token, setTokenDraft] = createSignal(props.initialToken);
  const [base, setBaseDraft] = createSignal(props.initialBase);
  const [showToken, setShowToken] = createSignal(false);
  const [busy, setBusy] = createSignal(false);
  const [error, setError] = createSignal<string | null>(props.error);

  const submit = async (event: SubmitEvent) => {
    event.preventDefault();
    const candidateToken = token().trim();
    const candidateBase = base().trim().replace(/\/+$/, "");
    if (!candidateToken) {
      setError("A bearer token is required to continue.");
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await props.onAuthenticated(candidateToken, candidateBase);
    } catch (value) {
      setError(
        value instanceof ApiError && value.status === 401
          ? "That token was rejected (401). Check the current MyDevEnv2 token and try again."
          : value instanceof ApiError
            ? `The server rejected the login (HTTP ${value.status}).`
            : `Could not reach the MyDevEnv2 server: ${value instanceof Error ? value.message : String(value)}`,
      );
    } finally {
      setBusy(false);
    }
  };

  return (
    <main class="login-screen">
      <form class="login-card" onSubmit={submit}>
        <div class="login-eyebrow">MyDevEnv2</div>
        <h1>Sign in to your development environment</h1>
        <p class="login-copy">
          Enter a valid bearer token to continue. The workspace stays locked until
          the server confirms your credentials.
        </p>
        <label>
          Bearer token
          <input
            type={showToken() ? "text" : "password"}
            value={token()}
            onInput={(event) => {
              setTokenDraft(event.currentTarget.value);
              setError(null);
            }}
            autocomplete="off"
            spellcheck={false}
            autofocus
          />
        </label>
        <label>
          Backend URL
          <input
            type="url"
            value={base()}
            onInput={(event) => setBaseDraft(event.currentTarget.value)}
            placeholder="https://mydevenv2.sprooty.com (blank = this site)"
            autocomplete="url"
            spellcheck={false}
          />
        </label>
        <label class="login-checkbox">
          <input
            type="checkbox"
            checked={showToken()}
            onChange={(event) => setShowToken(event.currentTarget.checked)}
          />
          Show token
        </label>
        <Show when={error()}>
          <div class="login-error" role="alert">{error()}</div>
        </Show>
        <button class="login-submit" type="submit" disabled={busy()}>
          {busy() ? "Signing in…" : "Sign in"}
        </button>
        <p class="login-help">
          Your token is stored only in this browser profile and is sent over the
          configured HTTPS connection.
        </p>
      </form>
    </main>
  );
};

function activityClass(s: SessionSummary): string {
  if (s.exit_code !== null) {
    return s.exit_code === 0 ? "done" : "errored";
  }
  return s.activity;
}

/**
 * A one-glyph protection marker. Absent continuity means unprotected, which is
 * also what a ContextKeeper outage looks like — deliberately, because in both
 * cases there is no recovery to offer and the terminal is otherwise fine.
 */
function continuityBadge(s: SessionSummary): { glyph: string; cls: string; title: string } {
  const continuity = s.continuity;
  if (!continuity) {
    return {
      glyph: "○",
      cls: "unprotected",
      title: "Unprotected: no captured agent session is bound to this terminal",
    };
  }
  if (continuity.state === "recovering") {
    return {
      glyph: "◆",
      cls: "recovering",
      title: `Recovery available (${continuity.provider}, ${continuity.failure_count} failure(s))`,
    };
  }
  if (continuity.state === "protected") {
    const lag = continuity.capture_lag_seconds;
    const fresh =
      continuity.capture_status === "catching-up"
        ? "capture catching up"
        : lag == null
          ? "capture freshness unknown"
          : `captured ${Math.round(lag)}s ago`;
    return {
      glyph: "●",
      cls: "protected",
      title: `Protected (${continuity.provider}, ${fresh})`,
    };
  }
  return { glyph: "○", cls: "unprotected", title: "Unprotected" };
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
  if (tab.kind === "gui") return "/gui";
  if (tab.kind === "history") return "/history";
  if (tab.kind === "board") return "/board";
  if (tab.kind === "backlog") return "/backlog";
  if (tab.kind === "workitem") return `/w/${encodeURIComponent(tab.ref)}`;
  if (tab.kind === "assistant") return "/assistant";
  return "/tasks";
}

const App: Component = () => {
  const navigate = useNavigate();
  const params = useParams<{ id?: string; path?: string }>();
  const location = useLocation();
  const [drawerOpen, setDrawerOpen] = createSignal(false);
  const [mobileTabsOpen, setMobileTabsOpen] = createSignal(false);
  const [settingsOpen, setSettingsOpen] = createSignal(false);
  const [commandPaletteOpen, setCommandPaletteOpen] = createSignal(false);
  const [templateSelectorOpen, setTemplateSelectorOpen] = createSignal(false);
  const [templateSelectorContext, setTemplateSelectorContext] =
    createSignal<TemplateContext | null>(null);
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
  const activeSelectAll = () => {
    const id = tabsStore.active;
    if (id) actions.get(id)?.selectAll();
  };
  const activeFocusComposer = () => {
    const id = tabsStore.active;
    if (id) actions.get(id)?.focusComposer?.();
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
  const [authState, setAuthState] = createSignal<"checking" | "unauthenticated" | "authenticated">("checking");
  const [authError, setAuthError] = createSignal<string | null>(null);
  const layoutMode = getLayoutMode();
  const activeTab = () => tabsStore.tabs.find((tab) => tab.id === tabsStore.active) ?? null;

  // Check if we're in IDE mode
  const isIDEMode = layoutMode === "ide";
  const activeKind = () =>
    tabsStore.tabs.find((tab) => tab.id === tabsStore.active)?.kind ?? null;
  const editorWorkspaceActive = () => isIDEMode && activeKind() === "editor";

  onMount(() => {
    const unsubscribeAuthState = subscribeAuthState(() => {
      stopEventStream();
      window.location.reload();
    });
    onCleanup(unsubscribeAuthState);
    apiModule
      .publicConfig()
      .then((c) => setPublicCfg(c))
      .catch(() => {
        /* server may be down; non-fatal */
      });

    void (async () => {
      const token = getToken();
      if (!token) {
        setAuthState("unauthenticated");
        return;
      }
      try {
        await validateCredentials(token, getBase());
        setAuthError(null);
        setAuthState("authenticated");
        await refreshSessions();
        startEventStream();
      } catch (error) {
        if (error instanceof ApiError && error.status === 401) {
          setAuthError("Your saved token was rejected. Sign in with the current token to continue.");
          setAuthState("unauthenticated");
          return;
        }
        setAuthError(`Could not validate your session: ${error instanceof Error ? error.message : String(error)}`);
        setAuthState("unauthenticated");
      }
    })();
  });

  const authenticate = async (token: string, base: string) => {
    await validateCredentials(token, base);
    setToken(token);
    setBase(base);
    await refreshSessions();
    startEventStream();
    setAuthError(null);
    setAuthState("authenticated");
  };

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
    } else if (path === "/history") {
      openHistoryTab();
    } else if (path === "/tasks") {
      openTasksTab();
    } else if (path === "/assistant" || path.startsWith("/assistant/")) {
      openAssistantTab();
    }
  });

  // Server defaults merged with user's custom templates from localStorage.
  const allTemplates = () => mergeTemplates(publicCfg()?.session_templates ?? []);
  const availableTemplates = () =>
    sortTemplatesForContext(allTemplates(), templateSelectorContext());

  const activeTemplatePath = () => {
    const active = tabsStore.tabs.find((tab) => tab.id === tabsStore.active);
    if (!active) return undefined;
    if (active.kind === "editor") {
      const idx = active.path.lastIndexOf("/");
      return idx >= 0 ? active.path.slice(0, idx) : "";
    }
    if (active.kind === "git") {
      return active.repo || "";
    }
    return undefined;
  };

  const launchTemplateDirect = async (template: SessionTemplate) => {
    const context = await resolveTemplateContext(activeTemplatePath());
    const suggested = buildDefaultSessionName(template, context);
    const name = await promptUser("New session from preset", suggested, "name");
    if (!name) return;
    try {
      const launch = resolveTemplateLaunch(template, context, name);
      const session = await createSession(name, launch.command, launch.cwd, launch.env);
      openTerminalTab(session.id, session.name);
      navigate(`/t/${session.id}`, { replace: false });
    } catch (e) {
      showToast(`create failed: ${(e as Error).message}`, { kind: "error" });
    }
  };

  const retryRealtimeConnection = async () => {
    stopEventStream();
    await refreshSessions();
    startEventStream();
    showToast("Refreshing sessions and event stream...");
  };

  const onCreate = async (cwd?: string, template?: SessionTemplate) => {
    // If template selector should be shown
    if (!template && allTemplates().length > 1) {
      setTemplateSelectorContext(await resolveTemplateContext(cwd));
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

  const onTemplateSelect = async (template: SessionTemplate, name: string) => {
    setTemplateSelectorOpen(false);
    try {
      const launch = resolveTemplateLaunch(
        template,
        templateSelectorContext(),
        name,
      );
      const s = await createSession(
        name,
        launch.command,
        launch.cwd,
        launch.env,
      );
      openTerminalTab(s.id, s.name);
      navigate(`/t/${s.id}`, { replace: false });
      setDrawerOpen(false);
      setTemplateSelectorContext(null);
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
      let dup: SessionSummary;
      try {
        dup = await createSession(name, undefined, s.cwd || undefined);
      } catch (e) {
        const message = e instanceof Error ? e.message : String(e);
        if (!message.includes("escapes workspace_root")) throw e;
        dup = await createSession(name);
        showToast("Duplicate opened at the default cwd");
      }
      openTerminalTab(dup.id, dup.name);
      navigate(`/t/${dup.id}`);
      setDrawerOpen(false);
    } catch (e) {
      showToast(`duplicate failed: ${(e as Error).message}`, { kind: "error" });
    }
  };

  const onSaveWorkspaceLayout = async () => {
    const now = new Date();
    const suggestedName = `Layout ${now.toLocaleDateString()} ${now.toLocaleTimeString([], {
      hour: "2-digit",
      minute: "2-digit",
    })}`;
    const name = await promptUser(
      "Save workspace layout",
      suggestedName,
      "layout name",
    );
    if (!name?.trim()) return false;

    const snapshot = snapshotTabs();
    const layout = saveWorkspaceLayout({
      name,
      layout_mode: getLayoutMode(),
      tabs: snapshot.tabs,
      active: snapshot.active,
    });
    showToast(`Saved layout "${layout.name}"`);
    return true;
  };

  const onRestoreWorkspaceLayout = async (layoutId: string) => {
    const layout = getWorkspaceLayout(layoutId);
    if (!layout) {
      showToast("Saved layout not found", { kind: "error" });
      return false;
    }

    const tabs = layout.tabs.filter(
      (tab) =>
        tab.kind !== "terminal" ||
        !sessionsStore.ready ||
        Boolean(sessionsStore.sessions[tab.sessionId]),
    );
    const skipped = layout.tabs.length - tabs.length;
    const active =
      layout.active && tabs.some((tab) => tab.id === layout.active)
        ? layout.active
        : tabs[0]?.id ?? null;
    const nextActiveTab = tabs.find((tab) => tab.id === active) ?? null;
    const modeChanged = layout.layout_mode !== getLayoutMode();

    senders.clear();
    actions.clear();
    replaceTabs({ tabs, active });
    setDrawerOpen(false);
    setSettingsOpen(false);
    setCommandPaletteOpen(false);
    setLayoutMode(layout.layout_mode);

    navigate(nextActiveTab ? pathFor(nextActiveTab) : "/", { replace: true });

    if (modeChanged) {
      window.location.reload();
      return true;
    }

    if (skipped > 0) {
      showToast(
        `Restored "${layout.name}" and skipped ${skipped} missing live session${skipped === 1 ? "" : "s"}.`,
      );
    } else {
      showToast(`Restored layout "${layout.name}"`);
    }
    return true;
  };

  const onDeleteWorkspaceLayout = async (layoutId: string) => {
    const layout = getWorkspaceLayout(layoutId);
    if (!layout) return false;
    const ok = await confirmUser(
      `Delete layout "${layout.name}"?`,
      "This removes only the saved browser-local snapshot.",
    );
    if (!ok) return false;
    deleteWorkspaceLayout(layoutId);
    showToast(`Deleted layout "${layout.name}"`);
    return true;
  };

  const closeTabAndNavigate = (tabId: string) => {
    const tab = tabsStore.tabs.find((t) => t.id === tabId);
    const onThisTab =
      (location.pathname.startsWith("/t/") && `term:${params.id}` === tabId) ||
      (location.pathname.startsWith("/e/") &&
        `edit:${decodeURIComponent(params.path ?? "")}` === tabId) ||
      (location.pathname.startsWith("/g") &&
        `git:${decodeURIComponent(params.path ?? "")}` === tabId) ||
      (location.pathname === "/gui" && tabId === "gui") ||
      (location.pathname === "/history" && tabId === "history") ||
      (location.pathname === "/tasks" && tabId === "tasks") ||
      ((location.pathname === "/assistant" || location.pathname.startsWith("/assistant/")) &&
        tabId === "assistant");

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
      <Show when={authState() === "authenticated"} fallback={
        <Show when={authState() === "unauthenticated"} fallback={<main class="login-loading">Checking your session…</main>}>
          <LoginScreen
            initialToken={getToken()}
            initialBase={getBase()}
            error={authError()}
            onAuthenticated={authenticate}
          />
        </Show>
      }>
      <div class="app">
        <Show when={drawerOpen()}>
          <div
            class="drawer-scrim"
            onPointerDown={() => setDrawerOpen(false)}
          />
        </Show>
        <Show when={mobileTabsOpen()}>
          <div
            class="drawer-scrim"
            onPointerDown={() => setMobileTabsOpen(false)}
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
                openTasksTab();
                navigate("/tasks");
                setDrawerOpen(false);
              }}
              title="Open recurring agent tasks"
            >
              Tasks
            </button>
            <Show when={publicCfg()?.assistant_enabled}>
              <button
                onClick={() => {
                  openAssistantTab();
                  navigate("/assistant");
                  setDrawerOpen(false);
                }}
                title="Talk to the assistant about your sessions"
              >
                🎙 Assistant
              </button>
            </Show>
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
                .filter((s): s is SessionSummary => Boolean(s))
                .sort((a, b) => {
                  // Bookmarked sessions float to the top. `bookmarks()` is read
                  // here so this re-sorts reactively when a bookmark toggles.
                  const set = new Set(bookmarks());
                  const aB = set.has(a.id) ? 0 : 1;
                  const bB = set.has(b.id) ? 0 : 1;
                  return aB - bB;
                })}
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
                  <span
                    class={`continuity-dot ${continuityBadge(s).cls}`}
                    title={continuityBadge(s).title}
                  >
                    {continuityBadge(s).glyph}
                  </span>
                  <div class="session-row-body">
                    <span class="name">{s.name}</span>
                    <Show when={s.cwd}>
                      <span class="cwd">{s.cwd}</span>
                    </Show>
                  </div>
                  <span
                    class="row-btn"
                    title={isBookmarked(s.id) ? "Remove bookmark" : "Bookmark"}
                    onClick={(e) => {
                      e.stopPropagation();
                      toggleBookmark(s.id);
                    }}
                  >
                    {isBookmarked(s.id) ? "★" : "☆"}
                  </span>
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
            onCreatePresetHere={(path) => {
              void onCreate(path);
            }}
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
          <button
            class="menu-btn"
            onClick={() => setMobileTabsOpen(true)}
            title="Show open tabs"
            aria-label="Show open tabs"
          >
            {tabsStore.tabs.length}
          </button>
          <For each={tabsStore.tabs}>
            {(t) => (
              <div
                class={`tab ${tabsStore.active === t.id ? "active" : ""}`}
                onAuxClick={(e) => {
                  // Middle-click closes the tab (browser/editor convention).
                  if (e.button === 1) {
                    e.preventDefault();
                    void requestCloseTab(t.id);
                  }
                }}
                title={t.label}
              >
                <button
                  type="button"
                  class="tab-main"
                  onClick={() => {
                    focusTab(t.id);
                    navigate(pathFor(t));
                  }}
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
                          : t.kind === "tasks"
                            ? "≡ "
                            : t.kind === "assistant"
                              ? "🎙 "
                          : ""}
                    {t.label}
                  </span>
                  <Show when={t.kind === "editor" && t.dirty}>
                    <span class="dirty-dot" title="unsaved changes" />
                  </Show>
                </button>
                <button
                  type="button"
                  class="close"
                  aria-label={`Close ${t.label}`}
                  onClick={(e) => {
                    e.stopPropagation();
                    void requestCloseTab(t.id);
                  }}
                >
                  ×
                </button>
              </div>
            )}
          </For>
        </div>

        <main class="main">
          <Show when={!isConnected() && getToken()}>
            <div class="connection-banner">
              <div class="connection-banner-copy">
                <strong>Realtime connection lost</strong>
                <span>{sessionsError() || "Trying to reconnect to the session event stream."}</span>
              </div>
              <button type="button" onClick={() => void retryRealtimeConnection()}>
                Retry now
              </button>
            </div>
          </Show>
          <Show when={editorWorkspaceActive()}>
            <EditorWorkspace
              promptPath={promptUser}
              onRequestCloseTab={(tabId) => void requestCloseTab(tabId)}
              onNotify={(message, kind) => showToast(message, { kind })}
            />
          </Show>
          <div
            class="tab-view"
            style={{ display: editorWorkspaceActive() ? "none" : "flex" }}
          >
            <For each={tabsStore.tabs}>
              {(t) => (
                <div
                  style={{
                    display:
                      tabsStore.active === t.id &&
                      !(isIDEMode && t.kind === "editor")
                        ? "flex"
                        : "none",
                    "flex-direction": "column",
                    flex: 1,
                    "min-height": 0,
                    "min-width": 0,
                  }}
                >
                  <Show when={t.kind === "terminal" && t}>
                    {(tab) => (
                      <TerminalWorkspace
                        tabId={tab().id}
                        sessionId={
                          (tab() as Extract<Tab, { kind: "terminal" }>).sessionId
                        }
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
                        onFocusSession={(sessionId) => {
                          // A recovery replaces the terminal, so navigation is
                          // part of the action: the user should land in the
                          // session that continues their work.
                          const session = sessionsStore.sessions[sessionId];
                          openTerminalTab(sessionId, session?.name ?? sessionId.slice(0, 8));
                          navigate(`/t/${sessionId}`, { replace: false });
                        }}
                      />
                    )}
                  </Show>
                  <Show when={t.kind === "editor" && !isIDEMode && t}>
                    {(tab) => (
                      <Editor
                        tabId={tab().id}
                        path={(tab() as Extract<Tab, { kind: "editor" }>).path}
                      />
                    )}
                  </Show>
                  <Show when={t.kind === "git" && t}>
                    {(tab) => (
                      <GitTab
                        repo={(tab() as Extract<Tab, { kind: "git" }>).repo}
                      />
                    )}
                  </Show>
                  <Show when={t.kind === "gui"}>
                    <GuiTab streamUrl={publicCfg()?.gui_stream_url ?? null} />
                  </Show>
                  <Show when={t.kind === "history"}>
                    <History
                      onError={(msg) => showToast(msg, { kind: "error" })}
                    />
                  </Show>
                  <Show when={t.kind === "assistant"}>
                    <Assistant
                      onError={(msg) => showToast(msg, { kind: "error" })}
                    />
                  </Show>
                  <Show when={t.kind === "board"}>
                    <Board onError={(msg) => showToast(msg, { kind: "error" })} />
                  </Show>
                  <Show when={t.kind === "backlog"}>
                    <Backlog
                      onError={(msg) => showToast(msg, { kind: "error" })}
                    />
                  </Show>
                  <Show when={t.kind === "workitem" && t}>
                    {(tab) => (
                      <WorkItemDetail
                        itemRef={
                          (tab() as Extract<Tab, { kind: "workitem" }>).ref
                        }
                        onError={(msg) => showToast(msg, { kind: "error" })}
                      />
                    )}
                  </Show>
                  <Show when={t.kind === "tasks"}>
                    <AgentTasks
                      onError={(msg) => showToast(msg, { kind: "error" })}
                      onOpenSession={(sessionId, label) => {
                        openTerminalTab(sessionId, label);
                        navigate(`/t/${sessionId}`);
                      }}
                    />
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
          <Show when={activeKind() === "terminal"}>
            <ModKeyRow
              send={(d) => activeSend(d)}
              onCopy={() => void activeCopy()}
              onPaste={() => void activePaste()}
              onSelectAll={() => activeSelectAll()}
              onFocusComposer={() => activeFocusComposer()}
            />
          </Show>
        </main>
      </div>

      <Show when={mobileTabsOpen()}>
        <div class="mobile-tabs-sheet">
          <div class="mobile-tabs-header">
            <div>
              <strong>Open tabs</strong>
              <div class="mobile-tabs-meta">
                {tabsStore.tabs.length} total
                <Show when={activeTab()}>
                  {(tab) => <span> • active: {tab().label}</span>}
                </Show>
              </div>
            </div>
            <button type="button" onClick={() => setMobileTabsOpen(false)}>
              Close
            </button>
          </div>
          <div class="mobile-tabs-list">
            <For each={tabsStore.tabs}>
              {(tab) => (
                <div
                  class={`mobile-tab-row ${tabsStore.active === tab.id ? "active" : ""}`}
                >
                  <button
                    type="button"
                    class="mobile-tab-main"
                    onClick={() => {
                      focusTab(tab.id);
                      navigate(pathFor(tab));
                      setMobileTabsOpen(false);
                    }}
                  >
                    <span class="mobile-tab-title">
                      <Show when={tab.kind === "terminal"}>
                        <span class={`activity-dot ${tabActivityClass(tab) ?? "idle"}`} />
                      </Show>
                      {tab.label}
                    </span>
                    <span class="mobile-tab-kind">{tab.kind}</span>
                  </button>
                  <button
                    type="button"
                    class="mobile-tab-close"
                    onClick={() => void requestCloseTab(tab.id)}
                  >
                    ×
                  </button>
                </div>
              )}
            </For>
          </div>
        </div>
      </Show>

      <Settings
        open={settingsOpen()}
        onClose={() => setSettingsOpen(false)}
        onSaveWorkspaceLayout={() => onSaveWorkspaceLayout()}
        onRestoreWorkspaceLayout={(layoutId) => onRestoreWorkspaceLayout(layoutId)}
        onDeleteWorkspaceLayout={(layoutId) => onDeleteWorkspaceLayout(layoutId)}
      />

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
        onOpenSettings={() => setSettingsOpen(true)}
        onShowShortcuts={() => setShortcutsOpen(true)}
        onError={(message) => showToast(message, { kind: "error" })}
        templates={availableTemplates()}
        onLaunchTemplate={(template) => void launchTemplateDirect(template)}
        onSaveWorkspaceLayout={() => onSaveWorkspaceLayout()}
        onRestoreWorkspaceLayout={(layoutId) => onRestoreWorkspaceLayout(layoutId)}
      />

      <TemplateSelector
        open={templateSelectorOpen()}
        onClose={() => {
          setTemplateSelectorOpen(false);
          setTemplateSelectorContext(null);
        }}
        onSelect={onTemplateSelect}
        templates={availableTemplates()}
        context={templateSelectorContext()}
      />

      <KeyboardShortcuts
        open={shortcutsOpen()}
        onClose={() => setShortcutsOpen(false)}
      />
      </Show>
    </>
  );
};

export default App;
