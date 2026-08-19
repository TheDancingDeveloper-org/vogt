import {
  Component,
  For,
  Show,
  createEffect,
  createSignal,
  lazy,
  onCleanup,
  onMount,
  untrack,
} from "solid-js";
import { useLocation, useNavigate, useParams } from "@solidjs/router";
import type { TerminalActions } from "./Terminal";
import type { AgentTaskDraftGuard } from "./AgentTasks";
import Board from "./Board";
import Sessions from "./Sessions";
import RouteOutcomeView from "./RouteOutcome";
import { matchAppShortcut } from "./keyboardShortcuts";
import ModKeyRow from "./ModKeyRow";
import FileTree from "./FileTree";
import type { FileWorkflow } from "./FileWorkflowDialog";
import CommandPalette, { invalidateCommandPaletteProviders } from "./CommandPalette";
import Dialog from "./Dialog";
import FeedbackCenter, {
  createFeedbackQueue,
  type FeedbackOptions,
} from "./FeedbackCenter";
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
  onVogtChanged,
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
  initialRoute,
  recentPlacesStore,
  rememberPlace,
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
import {
  describeRoute,
  documentTitleForRoute,
  isCurrentPlace,
  isCurrentTool,
  settingsReturnRoute,
  type PrimaryPlace,
} from "./routeModel";
import { productDocumentTitle } from "./identity";
import {
  hasUnsavedWork,
  protectDirtyEditorExit,
  shouldMountTab,
} from "./tabLifecycle";
import { discardEditorDraft } from "./editorDrafts";
import {
  createPlaceMetrics,
  type PlaceMetric,
} from "./placeMetrics";

// -- what the first screen does not have to carry (NFR-S5, #104) -----------
//
// Every place below is reached by a route, and no route needs all of them.
// Kept eager: the shell itself, the Board and the Sessions place, which are
// the two landing routes — a lazy landing place is a second round trip
// before anything is drawn. Everything else, including the terminal (xterm)
// and the editor (Monaco and its language workers), arrives when the route
// that needs it does. This is what took the initial bundle from 848kB to
// 218kB; `scripts/check_bundle.py` is what keeps it there.
//
// Deliberately *without* a `Suspense` boundary around them. Suspense catches
// every resource beneath it, not only the one that is lazy: with a boundary
// here, a Backlog filter change re-suspends the surface's own ranked read,
// the boundary swaps in its fallback, and the control the reader just
// clicked is destroyed under their finger. A place with nothing drawn for
// the few milliseconds its chunk takes is the cheaper of the two.
const TerminalWorkspace = lazy(() => import("./TerminalWorkspace"));
const Editor = lazy(() => import("./Editor"));
const EditorWorkspace = lazy(() => import("./EditorWorkspace"));
const AgentTasks = lazy(() => import("./AgentTasks"));
const Assistant = lazy(() => import("./Assistant"));
const AuditBrowser = lazy(() => import("./AuditBrowser"));
const Backlog = lazy(() => import("./Backlog"));
const Inbox = lazy(() => import("./Inbox"));
const GitTab = lazy(() => import("./Git"));
const GuiTab = lazy(() => import("./Gui"));
const Projects = lazy(() => import("./Projects"));
const WorkItemDetail = lazy(() => import("./WorkItemDetail"));
const History = lazy(() => import("./History"));
const KeyboardShortcuts = lazy(() => import("./KeyboardShortcuts"));
const Settings = lazy(() => import("./Settings"));
const TemplateSelector = lazy(() => import("./TemplateSelector"));
const FileWorkflowDialog = lazy(() => import("./FileWorkflowDialog"));


const PlaceCount: Component<{ metric: PlaceMetric; label: string }> = (props) => {
  const copy = () => {
    if (props.metric.state === "loading") return { glyph: "…", label: `${props.label} loading` };
    if (props.metric.state === "unavailable") return { glyph: "—", label: `${props.label} unavailable` };
    const value = props.metric.value ?? 0;
    const glyph = value > 999 ? "999+" : `${value}`;
    return props.metric.state === "stale"
      ? { glyph, label: `${value} ${props.label}, refreshing` }
      : { glyph, label: `${value} ${props.label}` };
  };
  return (
    <span
      class="place-count"
      classList={{ "place-count--attention": (props.metric.value ?? 0) > 0 && props.label.includes("waiting") }}
      data-state={props.metric.state}
      aria-label={copy().label}
      title={copy().label}
    >
      {copy().glyph}
    </span>
  );
};

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
          ? "That token was rejected (401). Check the current Vogt token and try again."
          : value instanceof ApiError
            ? `The server rejected the login (HTTP ${value.status}).`
            : `Could not reach Vogt: ${value instanceof Error ? value.message : String(value)}`,
      );
    } finally {
      setBusy(false);
    }
  };

  return (
    <main class="login-screen">
      <form class="login-card" onSubmit={submit}>
        <div class="login-eyebrow">Vogt</div>
        <h1>Sign in to Vogt</h1>
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
            placeholder="https://your-vogt.example (blank = this site)"
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

/**
 * Where each tab was last seen, including its query string.
 *
 * `pathFor` gives a tab's canonical path and nothing more, so activating a
 * tab used to drop everything after the `?` — which on a Vogt surface is the
 * filter set, the thing FR-U11 requires a link to restore. Held in memory
 * rather than persisted: it describes this window's history, and a filter
 * worth keeping across restarts is a saved filter, which is a different
 * feature with a name.
 */
const lastUrlByTab = new Map<string, string>();

function urlForTab(tab: Tab): string {
  return lastUrlByTab.get(tab.id) ?? pathFor(tab);
}

function pathFor(tab: Tab): string {
  if (tab.kind === "terminal") return `/t/${tab.sessionId}`;
  if (tab.kind === "editor") return `/e/${encodeURIComponent(tab.path)}`;
  if (tab.kind === "git") return `/g/${encodeURIComponent(tab.repo)}`;
  if (tab.kind === "gui") return "/gui";
  if (tab.kind === "history") return "/history";
  if (tab.kind === "workitem") return `/w/${encodeURIComponent(tab.ref)}`;
  if (tab.kind === "assistant") return "/assistant";
  return "/tasks";
}

const App: Component = () => {
  const navigate = useNavigate();
  const params = useParams<{ id?: string; path?: string; ref?: string }>();
  const location = useLocation();
  const [settingsOpen, setSettingsOpen] = createSignal(false);
  let settingsReturnUrl = "/sessions";
  let settingsRouted = false;
  let settingsHasHistoryReturn = false;
  const [commandPaletteOpen, setCommandPaletteOpen] = createSignal(false);
  const [fileWorkflow, setFileWorkflow] = createSignal<FileWorkflow | null>(null);
  const [templateSelectorOpen, setTemplateSelectorOpen] = createSignal(false);
  const [templateSelectorContext, setTemplateSelectorContext] =
    createSignal<TemplateContext | null>(null);
  const [shortcutsOpen, setShortcutsOpen] = createSignal(false);
  const placeMetrics = createPlaceMetrics();
  const waitingSessions = () =>
    sessionsStore.ready
      ? sessionsStore.order.filter(
          (id) => sessionsStore.sessions[id]?.activity === "waiting-for-input",
        ).length
      : null;
  const sessionMetric = (): PlaceMetric => ({
    value: sessionsStore.ready ? sessionsStore.order.length : null,
    state: sessionsStore.ready
      ? isConnected()
        ? "ready"
        : "stale"
      : sessionsError()
        ? "unavailable"
        : "loading",
  });
  const waitingMetric = (): PlaceMetric => ({
    value: waitingSessions(),
    state: sessionMetric().state,
  });

  const openSettings = () => {
    const here = `${location.pathname}${location.search}`;
    settingsReturnUrl = settingsReturnRoute(here, settingsReturnUrl);
    settingsHasHistoryReturn = true;
    navigate("/settings");
  };

  const feedback = createFeedbackQueue();
  const showToast = (message: string, options: FeedbackOptions = {}) =>
    feedback.push(message, options);

  // Per-tab terminal action registry. Solid keeps Terminal components mounted
  // across tab switches, so we register actions by tab id on mount and read
  // them via a getter keyed on the active tab. This avoids the stale-closure
  // problem of single mutable `activeSend` / `activeCopy` refs.
  const senders = new Map<string, (data: string | ArrayBuffer) => void>();
  const actions = new Map<string, TerminalActions>();
  let taskDraftGuard: AgentTaskDraftGuard | null = null;
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
  const [configReady, setConfigReady] = createSignal(false);
  const [authState, setAuthState] = createSignal<"checking" | "unauthenticated" | "authenticated">("checking");
  const [authError, setAuthError] = createSignal<string | null>(null);
  const layoutMode = getLayoutMode();

  // Check if we're in IDE mode
  const isIDEMode = layoutMode === "ide";
  const activeKind = () =>
    tabsStore.tabs.find((tab) => tab.id === tabsStore.active)?.kind ?? null;
  const editorWorkspaceActive = () =>
    isIDEMode && activeKind() === "editor" && location.pathname.startsWith("/e/");
  const sessionWorkspaceActive = () => routeOutcome()?.kind === "tool";
  const guiEnabled = () => Boolean(publicCfg()?.gui_stream_available);
  const routeOutcome = () => describeRoute(
    location.pathname,
    {
      configReady: configReady(),
      sessionsState: sessionsStore.ready
        ? "ready"
        : sessionsError()
          ? "unavailable"
          : "loading",
      sessionExists: (id) => Boolean(sessionsStore.sessions[id]),
      assistantEnabled: Boolean(publicCfg()?.assistant_enabled),
      guiAvailable: guiEnabled(),
    },
    settingsReturnUrl,
  );
  const currentPlace = (place: PrimaryPlace) =>
    isCurrentPlace(routeOutcome(), place);
  const currentTool = () => {
    const outcome = routeOutcome();
    return outcome?.kind === "tool" || outcome?.kind === "settings"
      ? outcome.tool ?? null
      : null;
  };
  const routeProblem = () => {
    const outcome = routeOutcome();
    return outcome && (
      outcome.kind === "loading" ||
      outcome.kind === "unavailable" ||
      outcome.kind === "not-found"
    )
      ? outcome
      : null;
  };

  createEffect(() => {
    const state = authState();
    document.title = state === "checking"
      ? productDocumentTitle("Checking session")
      : state === "unauthenticated"
        ? productDocumentTitle("Sign in")
        : documentTitleForRoute(routeOutcome(), location.pathname);
  });

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
      })
      .finally(() => setConfigReady(true));

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
        void placeMetrics.refresh();
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
    void placeMetrics.refresh();
  };

  onCleanup(() => {
    stopEventStream();
  });
  onCleanup(onVogtChanged(() => void placeMetrics.refresh()));

  // Remember where the active tab is, so re-selecting it comes back here and
  // not to the surface's default view.
  createEffect(() => {
    const active = tabsStore.tabs.find((tab) => tab.id === tabsStore.active);
    if (!active) return;
    const here = `${location.pathname}${location.search}`;
    if (location.pathname === pathFor(active)) lastUrlByTab.set(active.id, here);
  });

  // Browser and installed-PWA lifecycle exits bypass the app's close-tab
  // confirmation. Ask the browser to guard the window only while real
  // unsaved work exists, then remove the listener immediately after
  // the last save so clean exits remain silent.
  createEffect(() => {
    if (!hasUnsavedWork(tabsStore.tabs)) return;
    window.addEventListener("beforeunload", protectDirtyEditorExit);
    onCleanup(() =>
      window.removeEventListener("beforeunload", protectDirtyEditorExit),
    );
  });

  createEffect(() => {
    if (!configReady() || guiEnabled()) return;
    for (const tab of tabsStore.tabs.filter((candidate) => candidate.kind === "gui")) {
      closeTab(tab.id);
    }
  });

  // URL syncing. createEffect (not createMemo) — we want side effects,
  // not a memoised value.
  createEffect(() => {
    const path = location.pathname;
    const currentSearch = location.search;
    if (path !== "/settings") {
      settingsReturnUrl = settingsReturnRoute(
        `${path}${currentSearch}`,
        settingsReturnUrl,
      );
    }
    if (path !== "/settings" && settingsOpen() && settingsRouted) {
      settingsRouted = false;
      settingsHasHistoryReturn = false;
      setSettingsOpen(false);
    }
    if (path === "/") {
      const narrow = window.matchMedia("(max-width: 768px)").matches ||
        (navigator.maxTouchPoints > 0 && window.innerWidth < 1024);
      navigate(initialRoute() ?? (narrow ? "/sessions" : "/board"), { replace: true });
    } else if (path === "/sessions") {
      // Sessions is a stable place; its panes are opened by the explicit tool
      // routes below and are never represented as a product tab.
    } else if (path.startsWith("/t/") && params.id) {
      const sess = sessionsStore.sessions[params.id];
      // Resolve the live roster before opening anything. On a cold deep link,
      // treating "not loaded yet" as a session and persisting that phantom
      // made the later stale-tab cleanup navigate away from the not-found
      // route as soon as the roster arrived.
      if (!sessionsStore.ready && !sessionsError()) return;
      if (!sess) return;
      const label = sess?.name ?? params.id.slice(0, 6);
      openTerminalTab(params.id, label);
    } else if (path.startsWith("/e/") && params.path) {
      openEditorTab(decodeURIComponent(params.path));
    } else if (path.startsWith("/g/") && params.path !== undefined) {
      openGitTab(decodeURIComponent(params.path));
    } else if (path === "/g") {
      openGitTab("");
    } else if (path === "/gui") {
      if (guiEnabled()) openGuiTab();
    } else if (path === "/history") {
      openHistoryTab();
    } else if (path === "/tasks") {
      openTasksTab();
    } else if (path === "/assistant" || path.startsWith("/assistant/")) {
      // The same condition the rail carries. Without it a hand-typed
      // `#/assistant` opened a tab against routes that answer 404 when no key
      // is configured — FR-T6 says the assistant does not exist unless it is
      // provisioned, and a tab that opens and then fails is a worse answer
      // than no tab.
      if (publicCfg()?.assistant_enabled) openAssistantTab();
    } else if (path === "/settings") {
      settingsRouted = true;
      setSettingsOpen(true);
    } else if (path.startsWith("/w/") && params.ref) {
      // Work items are addressable stable views. The URL selects the item;
      // unlike terminal/editor panes it does not create a product-level tab.
    }
    const outcome = routeOutcome();
    if (
      path !== "/" &&
      path !== "/settings" &&
      outcome?.kind !== "loading" &&
      outcome?.kind !== "unavailable" &&
      outcome?.kind !== "not-found"
    ) {
      const labels: Record<string, string> = {
        "/board": "Board",
        "/backlog": "Backlog",
        "/inbox": "Inbox",
        "/projects": "Projects",
        "/audit": "Audit",
        "/sessions": "Sessions",
        "/g": "Git",
        "/history": "History",
        "/tasks": "Tasks",
        "/gui": "GUI stream",
        "/assistant": "Assistant",
      };
      const label = labels[path] ?? (path.startsWith("/w/") ? decodeURIComponent(path.slice(3)) : path);
      rememberPlace(`${path}${currentSearch}`, label);
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

  const onCreate = async (
    cwd?: string,
    template?: SessionTemplate,
  ): Promise<void> => {
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
    } catch (e) {
      showToast("Session creation failed", {
        kind: "error",
        key: "session-create",
        details: (e as Error).message,
        actionLabel: "Retry",
        action: () => void onCreate(cwd, template),
      });
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
        (tab.kind !== "gui" || guiEnabled()) &&
        (
          tab.kind !== "terminal" ||
          !sessionsStore.ready ||
          Boolean(sessionsStore.sessions[tab.sessionId])
        ),
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
        tabId === "assistant") ||
      (location.pathname.startsWith("/w/") &&
        `workitem:${decodeURIComponent(params.ref ?? "")}` === tabId);

    // Drop any per-tab registrations so we don't leak references.
    senders.delete(tabId);
    actions.delete(tabId);
    if (tab?.kind === "editor") {
      queueMicrotask(() => discardEditorDraft(tabId));
    }

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
      if (
        tab.kind === "terminal"
        && location.pathname === `/t/${encodeURIComponent(tab.sessionId)}`
      ) {
        senders.delete(tab.id);
        actions.delete(tab.id);
        closeTab(tab.id);
      } else {
        closeTabAndNavigate(tab.id);
      }
    }
  });

  const requestCloseTab = async (tabId: string) => {
    const tab = tabsStore.tabs.find((t) => t.id === tabId);
    if (tab?.kind === "tasks" && taskDraftGuard?.dirty()) {
      taskDraftGuard.requestLeave(() => closeTabAndNavigate(tabId));
      return;
    }
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

  // App-level shortcuts are matched from the same registry the help dialog
  // renders. Browser-reserved alternatives and editable-surface contexts live
  // beside the binding instead of drifting between this handler and the UI.
  const onKeyDown = (e: KeyboardEvent) => {
    const shortcut = matchAppShortcut(e);
    if (!shortcut) return;

    if (shortcut.id === "open-command-palette") {
      e.preventDefault();
      setCommandPaletteOpen(true);
      return;
    }
    if (shortcut.id === "show-shortcut-help") {
      e.preventDefault();
      setShortcutsOpen(true);
      return;
    }
    if (shortcut.id === "new-terminal-session") {
      e.preventDefault();
      void onCreate();
      return;
    }
    if (shortcut.id === "close-active-tab") {
      const active = tabsStore.active;
      if (active) {
        e.preventDefault();
        void requestCloseTab(active);
      }
      return;
    }
    if (shortcut.id === "next-tab" || shortcut.id === "previous-tab") {
      const tabs = tabsStore.tabs;
      if (tabs.length === 0) return;
      const idx = Math.max(
        0,
        tabs.findIndex((t) => t.id === tabsStore.active),
      );
      const delta = shortcut.id === "next-tab" ? 1 : -1;
      const next = tabs[(idx + delta + tabs.length) % tabs.length];
      if (next) {
        e.preventDefault();
        focusTab(next.id);
        navigate(urlForTab(next));
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
        <aside class="places-rail" aria-label="Places">
          <div class="places-brand">Vogt</div>
          <button type="button" class="rail-go-to" onClick={() => setCommandPaletteOpen(true)}>Go to…</button>
          <nav class="places-nav">
            <div class="places-group">
              <span class="places-group-label">Work</span>
              <a class={currentPlace("board") ? "active" : ""} aria-current={currentPlace("board") ? "page" : undefined} href="#/board"><span>Board</span><PlaceCount metric={placeMetrics.metrics.board} label="Board work items" /></a>
              <a class={currentPlace("backlog") ? "active" : ""} aria-current={currentPlace("backlog") ? "page" : undefined} href="#/backlog"><span>Backlog</span><PlaceCount metric={placeMetrics.metrics.backlog} label="Backlog candidates" /></a>
              <a class={currentPlace("inbox") ? "active" : ""} aria-current={currentPlace("inbox") ? "page" : undefined} href="#/inbox"><span>Inbox</span><PlaceCount metric={placeMetrics.metrics.inbox} label="active Inbox entries" /></a>
            </div>
            <Show when={publicCfg()?.vogt?.configured}>
              <div class="places-group">
                <span class="places-group-label">Estate</span>
                <a class={currentPlace("projects") ? "active" : ""} aria-current={currentPlace("projects") ? "page" : undefined} href="#/projects"><span>Projects</span><PlaceCount metric={placeMetrics.metrics.projects} label="Projects" /></a>
                <a class={currentPlace("audit") ? "active" : ""} aria-current={currentPlace("audit") ? "page" : undefined} href="#/audit">Audit</a>
              </div>
            </Show>
            <div class="places-group">
              <span class="places-group-label">Machine</span>
              <a
                class={currentPlace("sessions") ? "active" : ""}
                aria-current={currentPlace("sessions") && !["git", "history", "tasks", "gui", "assistant"].includes(currentTool() ?? "") ? "page" : undefined}
                href="#/sessions"
              ><span>Sessions</span><PlaceCount metric={waitingMetric()} label="sessions waiting for input" /></a>
              <a class={isCurrentTool(routeOutcome(), "git") ? "active" : ""} aria-current={isCurrentTool(routeOutcome(), "git") ? "page" : undefined} href="#/g">Git</a>
              <a class={isCurrentTool(routeOutcome(), "history") ? "active" : ""} aria-current={isCurrentTool(routeOutcome(), "history") ? "page" : undefined} href="#/history">History</a>
              <a class={isCurrentTool(routeOutcome(), "tasks") ? "active" : ""} aria-current={isCurrentTool(routeOutcome(), "tasks") ? "page" : undefined} href="#/tasks">Tasks</a>
              <Show when={guiEnabled()}><a class={isCurrentTool(routeOutcome(), "gui") ? "active" : ""} aria-current={isCurrentTool(routeOutcome(), "gui") ? "page" : undefined} href="#/gui">GUI stream</a></Show>
              <Show when={publicCfg()?.assistant_enabled}><a class={isCurrentTool(routeOutcome(), "assistant") ? "active" : ""} aria-current={isCurrentTool(routeOutcome(), "assistant") ? "page" : undefined} href="#/assistant">Assistant</a></Show>
            </div>
          </nav>
          <div class="places-rail-session-area">
            <div class="places-section-label places-section-label--counted"><span>Running</span><PlaceCount metric={sessionMetric()} label="running sessions" /></div>
            <For
              each={sessionsStore.order
                .map((id) => sessionsStore.sessions[id])
                .filter((s): s is SessionSummary => Boolean(s))
                .sort((a, b) => {
                  const set = new Set(bookmarks());
                  return (set.has(a.id) ? 0 : 1) - (set.has(b.id) ? 0 : 1);
                })}
              fallback={<div class="empty">No sessions yet.</div>}
            >
              {(s) => (
                <div
                  role="link"
                  tabIndex={0}
                  aria-current={tabsStore.active === `term:${s.id}` ? "page" : undefined}
                  aria-label={`${s.name}, ${activityLabel(s.activity, s.exit_code)}`}
                  class={`session-row ${tabsStore.active === `term:${s.id}` ? "active" : ""} ${s.activity === "waiting-for-input" ? "waiting" : ""}`}
                  onClick={() => {
                    openTerminalTab(s.id, s.name);
                    navigate(`/t/${s.id}`);
                  }}
                  onKeyDown={(event) => {
                    if (event.target !== event.currentTarget) return;
                    if (event.key !== "Enter" && event.key !== " ") return;
                    event.preventDefault();
                    openTerminalTab(s.id, s.name);
                    navigate(`/t/${s.id}`);
                  }}
                  onContextMenu={(e) => {
                    e.preventDefault();
                    void onRenameSession(s);
                  }}
                  title={`${s.name}\ncwd: ${s.cwd}`}
                >
                  <span class={`activity-dot ${activityClass(s)}`} title={activityLabel(s.activity, s.exit_code)} />
                  <span class={`continuity-dot ${continuityBadge(s).cls}`} title={continuityBadge(s).title}>
                    {continuityBadge(s).glyph}
                  </span>
                  <div class="session-row-body">
                    <span class="name">{s.name}</span>
                    <Show when={s.cwd}><span class="cwd">{s.cwd}</span></Show>
                  </div>
                  <button
                    type="button"
                    class="row-btn"
                    aria-label={isBookmarked(s.id) ? `Remove bookmark from ${s.name}` : `Bookmark ${s.name}`}
                    title={isBookmarked(s.id) ? "Remove bookmark" : "Bookmark"}
                    onClick={(e) => {
                      e.stopPropagation();
                      toggleBookmark(s.id);
                    }}
                  >{isBookmarked(s.id) ? "★" : "☆"}</button>
                  <button
                    type="button"
                    class="row-btn"
                    aria-label={`Duplicate ${s.name}`}
                    title="Duplicate (same cwd)"
                    onClick={(e) => {
                      e.stopPropagation();
                      void onDuplicateSession(s);
                    }}
                  >⧉</button>
                  <button
                    type="button"
                    class="close"
                    aria-label={`Close ${s.name}`}
                    title="Kill & remove"
                    onClick={(e) => {
                      e.stopPropagation();
                      void onCloseSession(s);
                    }}
                  >×</button>
                </div>
              )}
            </For>
          </div>
          <Show when={recentPlacesStore.places.length > 0}>
            <div class="places-recent" aria-label="Recent places">
              <div class="places-section-label">Recent places</div>
              <For each={recentPlacesStore.places.filter((place) => place.path !== "/gui" || guiEnabled()).slice(0, 6)}>
                {(place) => <a href={`#${place.path}`}>{place.label}</a>}
              </For>
            </div>
          </Show>
          <FileTree
            onOpen={() => navigate("/sessions")}
            promptPath={promptUser}
            confirmAction={confirmUser}
            onCreatePresetHere={(path) => { void onCreate(path); }}
            onError={(message) => showToast(message, { kind: "error" })}
          />
          <div class="places-rail-footer">
            <button type="button" onClick={openSettings}>Settings</button>
            <span class={`rail-connection ${isConnected() ? "connected" : "offline"}`}>
              <span class="rail-connection-dot" aria-hidden="true" />
              {isConnected() ? "Connected" : "Offline"}
            </span>
          </div>
        </aside>
        <main class="main">
          <button type="button" class="mobile-go-to" onClick={() => setCommandPaletteOpen(true)}>
            Go to…
          </button>
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
          <Show when={location.pathname === "/board"}>
            <div class="stable-place"><Board onError={(msg) => showToast(msg, { kind: "error" })} /></div>
          </Show>
          <Show when={location.pathname === "/backlog"}>
            <div class="stable-place"><Backlog onError={(msg) => showToast(msg, { kind: "error" })} /></div>
          </Show>
          <Show when={location.pathname === "/inbox"}>
            <div class="stable-place"><Inbox onError={(msg) => showToast(msg, { kind: "error" })} /></div>
          </Show>
          <Show when={location.pathname === "/projects"}>
            <div class="stable-place"><Projects onError={(msg) => showToast(msg, { kind: "error" })} /></div>
          </Show>
          <Show when={location.pathname === "/audit"}>
            <div class="stable-place"><AuditBrowser onError={(msg) => showToast(msg, { kind: "error" })} /></div>
          </Show>
          <Show when={location.pathname.startsWith("/w/") && params.ref}>
            <div class="stable-place">
              <WorkItemDetail
                itemRef={decodeURIComponent(params.ref ?? "")}
                onError={(msg) => showToast(msg, { kind: "error" })}
              />
            </div>
          </Show>
          <Show when={routeProblem()} keyed>
            {(problem) => (
              <div class="stable-place">
                <RouteOutcomeView
                  title={problem.title}
                  message={problem.message}
                  onRecover={problem.kind === "loading" ? undefined : () => navigate("/sessions")}
                />
              </div>
            )}
          </Show>
          <div
            class="sessions-shell"
            style={{
              display:
                currentPlace("sessions") && !routeProblem() ? "flex" : "none",
            }}
          >
            <Sessions
              currentTool={currentTool()}
              guiEnabled={guiEnabled()}
              assistantEnabled={Boolean(publicCfg()?.assistant_enabled)}
              hasActiveWorkspace={sessionWorkspaceActive()}
              onCreateSession={() => void onCreate()}
            >
              <div class="tab-view">
                {/* The terminal is xterm and the editor is Monaco; both are
                    fetched when a pane that needs one is opened. */}
                <Show when={isIDEMode && editorWorkspaceActive()}>
                  <div
                    class="retained-tab-pane"
                    data-tab-kind="editor-workspace"
                    style={{ display: editorWorkspaceActive() ? "flex" : "none" }}
                  >
                    <EditorWorkspace
                      promptPath={promptUser}
                      confirmAction={confirmUser}
                      onRequestCloseTab={(tabId) => void requestCloseTab(tabId)}
                      onNotify={(message, kind) => showToast(message, { kind })}
                    />
                  </div>
                </Show>
                <For each={tabsStore.tabs.filter((tab) =>
                  tab.kind !== "workitem"
                    && !(isIDEMode && tab.kind === "editor")
                    && shouldMountTab(
                      tab,
                      sessionWorkspaceActive() ? tabsStore.active : null,
                    )
                )}>
                  {(t) => (
                    <div
                      class="retained-tab-pane"
                      data-tab-kind={t.kind}
                      data-tab-id={t.id}
                      style={{
                        display: tabsStore.active === t.id ? "flex" : "none",
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
                        confirmAction={confirmUser}
                      />
                    )}
                  </Show>
                  <Show when={t.kind === "gui"}>
                    <GuiTab
                      streamUrl={publicCfg()?.gui_stream_url ?? null}
                      onError={(msg) => showToast(msg, { kind: "error" })}
                    />
                  </Show>
                  <Show when={t.kind === "history"}>
                    <History
                      onError={(msg) => showToast(msg, { kind: "error" })}
                      confirmAction={confirmUser}
                    />
                  </Show>
                  <Show when={t.kind === "assistant"}>
                    <Assistant
                      pendingHosted
                      onError={(msg) => showToast(msg, { kind: "error" })}
                    />
                  </Show>
                  <Show when={t.kind === "tasks"}>
                    <AgentTasks
                      onError={(msg) => showToast(msg, { kind: "error" })}
                      confirmAction={confirmUser}
                      registerDraftGuard={(guard) => {
                        taskDraftGuard = guard;
                      }}
                      onOpenSession={(sessionId, label) => {
                        openTerminalTab(sessionId, label);
                        navigate(`/t/${sessionId}`);
                      }}
                    />
                  </Show>
                    </div>
                  )}
                </For>
              </div>
            </Sessions>
          </div>
          <Show when={activeKind() === "terminal" && location.pathname.startsWith("/t/")}>
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

      <nav class="phone-bottom-nav" aria-label="Primary navigation">
        <a href="#/sessions" class={currentPlace("sessions") ? "active" : ""} aria-current={currentPlace("sessions") ? "page" : undefined}><span>Sessions</span><PlaceCount metric={sessionMetric()} label="sessions" /></a>
        <a href="#/inbox" class={currentPlace("inbox") ? "active" : ""} aria-current={currentPlace("inbox") ? "page" : undefined}><span>Inbox</span><PlaceCount metric={placeMetrics.metrics.inbox} label="active Inbox entries" /></a>
        <a href="#/board" class={currentPlace("board") ? "active" : ""} aria-current={currentPlace("board") ? "page" : undefined}><span>Board</span><PlaceCount metric={placeMetrics.metrics.board} label="Board work items" /></a>
        <a href="#/backlog" class={currentPlace("backlog") ? "active" : ""} aria-current={currentPlace("backlog") ? "page" : undefined}><span>Backlog</span><PlaceCount metric={placeMetrics.metrics.backlog} label="Backlog candidates" /></a>
      </nav>

      <Settings
        open={settingsOpen()}
        onClose={() => {
          setSettingsOpen(false);
          if (location.pathname === "/settings") {
            settingsRouted = false;
            if (settingsHasHistoryReturn) {
              settingsHasHistoryReturn = false;
              navigate(-1);
            } else {
              navigate(settingsReturnUrl, { replace: true });
            }
          }
        }}
        onSaveWorkspaceLayout={() => onSaveWorkspaceLayout()}
        onRestoreWorkspaceLayout={(layoutId) => onRestoreWorkspaceLayout(layoutId)}
        onDeleteWorkspaceLayout={(layoutId) => onDeleteWorkspaceLayout(layoutId)}
      />

      <Show when={promptReq()}>
        {(req) => (
          <Dialog
            title={req().title}
            onClose={() => {
              req().resolve(null);
              setPromptReq(null);
            }}
          >
            <input
              type="text"
              data-dialog-initial-focus
              value={promptDraft()}
              placeholder={req().placeholder ?? ""}
              onInput={(e) => setPromptDraft(e.currentTarget.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  req().resolve(promptDraft());
                  setPromptReq(null);
                }
              }}
            />
            <div class="modal-actions">
              <button
                type="button"
                onClick={() => {
                  req().resolve(null);
                  setPromptReq(null);
                }}
              >
                Cancel
              </button>
              <button
                type="button"
                onClick={() => {
                  req().resolve(promptDraft());
                  setPromptReq(null);
                }}
              >
                Save
              </button>
            </div>
          </Dialog>
        )}
      </Show>

      <Show when={confirmReq()}>
        {(req) => (
          <Dialog
            title={req().title}
            description={req().body}
            onClose={() => {
              req().resolve(false);
              setConfirmReq(null);
            }}
          >
            <div class="modal-actions">
              <button
                type="button"
                data-dialog-initial-focus
                onClick={() => {
                  req().resolve(false);
                  setConfirmReq(null);
                }}
              >
                Cancel
              </button>
              <button
                type="button"
                class="danger"
                onClick={() => {
                  req().resolve(true);
                  setConfirmReq(null);
                }}
              >
                Confirm
              </button>
            </div>
          </Dialog>
        )}
      </Show>

      <FeedbackCenter queue={feedback} />

      <CommandPalette
        open={commandPaletteOpen()}
        onClose={() => setCommandPaletteOpen(false)}
        onCreateSession={() => void onCreate()}
        onNewFile={() => setFileWorkflow("new")}
        onChooseFile={() => setFileWorkflow("open")}
        onOpenSettings={openSettings}
        guiEnabled={guiEnabled()}
        onShowShortcuts={() => setShortcutsOpen(true)}
        onError={(message) => showToast(message, { kind: "error" })}
        templates={availableTemplates()}
        onLaunchTemplate={(template) => void launchTemplateDirect(template)}
        onSaveWorkspaceLayout={() => onSaveWorkspaceLayout()}
        onRestoreWorkspaceLayout={(layoutId) => onRestoreWorkspaceLayout(layoutId)}
      />

      <Show when={fileWorkflow()} keyed>
        {(workflow) => (
          <FileWorkflowDialog
            workflow={workflow}
            onClose={() => setFileWorkflow(null)}
            onFileCreated={() => invalidateCommandPaletteProviders()}
            onOpenFile={(path) => {
              openEditorTab(path);
              navigate(`/e/${encodeURIComponent(path)}`);
            }}
          />
        )}
      </Show>

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
