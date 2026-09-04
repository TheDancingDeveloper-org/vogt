import {
  Component,
  For,
  Show,
  createEffect,
  createSignal,
  createUniqueId,
  onCleanup,
} from "solid-js";
import { useNavigate } from "@solidjs/router";
import { createSession, sessionsStore } from "./store";
import { listWork, type WorkItem as VogtWorkItem } from "./vogtApi";
import { taxonomy } from "./taxonomyCache";
import {
  focusTab,
  openGitTab,
  openTerminalTab,
  openWorkItemTab,
  openHistoryTab,
  openEditorTab,
  openGuiTab,
  openTasksTab,
  openAssistantTab,
  recentPlacesStore,
  surfaceHref,
  tabsStore,
} from "./tabs";
import { getRecentFiles } from "./recentFiles";
import { KEYBOARD_SHORTCUTS } from "./keyboardShortcuts";
import { rankCommands } from "./commandPaletteScore";
import {
  readRecentCommandIds,
  recordRecentCommand,
} from "./commandPaletteRecent";
import {
  api,
  signOut,
  type AgentTask,
  type FileSearchResult,
  type SessionSummary,
  type SessionTemplate,
} from "./api";
import {
  focusEditorRange,
  hasRegisteredEditor,
  listEditorSymbols,
  type EditorSymbolResult,
} from "./editorRegistry";
import {
  listWorkspaceLayouts,
  workspaceLayoutSummary,
  type SavedWorkspaceLayout,
} from "./workspaceLayouts";
import Dialog from "./Dialog";
import { createNarrow } from "./narrow";
import { historyResultUrl } from "./historyRoute";
import { terminalWorkspaceHandle } from "./paneComposeBus";

interface HistorySearchResult {
  session_id: string;
  session_name: string;
  match_snippet: string;
}

interface PackageJson {
  name?: string;
  scripts?: Record<string, string>;
}

type PackageManager = "pnpm" | "yarn" | "npm";

interface DetectedProject {
  cwd: string;
  displayName: string;
  packageManager: PackageManager;
  packageScripts: string[];
  hasCargo: boolean;
  hasPyproject: boolean;
  hasJustfile: boolean;
  hasMakefile: boolean;
}

type ProviderState = "idle" | "loading" | "ready" | "failed";
type ProviderName = "agent tasks" | "work items" | "projects" | "workspace actions";

interface PaletteProviderCache {
  key: string;
  agentTasks?: AgentTask[];
  workItems?: VogtWorkItem[];
  vogtProjects?: { slug: string; name: string }[];
  detectedProjects?: DetectedProject[];
}

let providerCache: PaletteProviderCache = { key: "" };
let providerCacheGeneration = 0;

/**
 * Explicit invalidation seam for writes, credential changes, and the palette's
 * own Refresh command. Cached answers are never treated as durable storage.
 */
export function invalidateCommandPaletteProviders(): void {
  providerCache = { key: "" };
  providerCacheGeneration += 1;
}

function currentProviderCache(): PaletteProviderCache {
  const key = `${api.getBase()}\u0000${api.getToken()}`;
  if (providerCache.key !== key) providerCache = { key };
  return providerCache;
}

function aborted(error: unknown): boolean {
  return error instanceof Error && error.name === "AbortError";
}

async function mapWithConcurrency<T, R>(
  values: readonly T[],
  limit: number,
  mapper: (value: T) => Promise<R>,
): Promise<R[]> {
  const results = new Array<R>(values.length);
  let cursor = 0;
  const workers = Array.from(
    { length: Math.min(limit, values.length) },
    async () => {
      while (cursor < values.length) {
        const index = cursor++;
        results[index] = await mapper(values[index]!);
      }
    },
  );
  await Promise.all(workers);
  return results;
}

interface MutableDetectedProject {
  cwd: string;
  packageManager: PackageManager;
  packageJsonPath?: string;
  packageName?: string;
  packageScripts: string[];
  hasCargo: boolean;
  hasPyproject: boolean;
  hasJustfile: boolean;
  hasMakefile: boolean;
}

const PROJECT_MANIFEST_QUERIES = [
  "package.json",
  "pnpm-lock.yaml",
  "yarn.lock",
  "package-lock.json",
  "Cargo.toml",
  "pyproject.toml",
  "Justfile",
  "justfile",
  "Makefile",
] as const;

const PREFERRED_PACKAGE_SCRIPTS = ["dev", "typecheck", "build", "test", "lint", "start"];

const IGNORED_PROJECT_SEGMENTS = new Set([
  "node_modules",
  "target",
  "dist",
  "build",
  ".next",
  ".nuxt",
  ".turbo",
  ".venv",
  "venv",
  "__pycache__",
  "coverage",
]);

function parentDir(path: string): string {
  const idx = path.lastIndexOf("/");
  return idx >= 0 ? path.slice(0, idx) : "";
}

function leafName(path: string): string {
  const idx = path.lastIndexOf("/");
  return idx >= 0 ? path.slice(idx + 1) : path;
}

function pathLabel(path: string): string {
  return path || "(workspace root)";
}

function isIgnoredProjectPath(path: string): boolean {
  return path
    .split("/")
    .filter(Boolean)
    .some((segment) => IGNORED_PROJECT_SEGMENTS.has(segment));
}

function comparePathDepth(a: string, b: string): number {
  const aDepth = a ? a.split("/").length : 0;
  const bDepth = b ? b.split("/").length : 0;
  return aDepth - bDepth || a.localeCompare(b);
}

export interface Command {
  id: string;
  label: string;
  description?: string;
  icon?: string;
  /** Keyboard shortcut to display on the row, as the chord tokens from
   *  `KEYBOARD_SHORTCUTS` (e.g. `["Ctrl/Cmd", "K"]`). Display only. */
  shortcut?: string[];
  action: () => void | Promise<void>;
  category?: string;
}

/** The chord tokens for a shortcut id, or undefined if there is no binding. */
function shortcutKeys(shortcutId: string): string[] | undefined {
  const found = KEYBOARD_SHORTCUTS.find((entry) => entry.id === shortcutId);
  return found ? [...found.keys] : undefined;
}

// Recent rows are clones of a real command carrying a distinct DOM id, so the
// same command can appear once under "Recent" and again in its own category
// without colliding. `baseIdOf` recovers the real id for recording and lookup.
const RECENT_ID_PREFIX = "recent-command:";
function baseIdOf(id: string): string {
  return id.startsWith(RECENT_ID_PREFIX) ? id.slice(RECENT_ID_PREFIX.length) : id;
}

// The palette's category tags and glyphs are drawn in a fixed-width column so a
// row's label always starts at the same x, whatever its icon. Word icons that
// used to widen the column ("work", "drift", "run", …) map to a short glyph;
// anything already short (a symbol, one or two characters) is shown as-is.
const ICON_GLYPHS: Record<string, string> = {
  work: "WK",
  drift: "DF",
  import: "IM",
  project: "PJ",
  sym: "{}",
  run: "▶",
  paused: "❚❚",
  git: "GT",
  file: "FL",
  dir: "DR",
  hist: "HS",
  tasks: "TK",
  set: "⚙",
  key: "⌘",
  layout: "LY",
  preset: "PS",
};

function iconGlyph(icon: string): string {
  if (icon in ICON_GLYPHS) return ICON_GLYPHS[icon]!;
  if (icon.length <= 2) return icon;
  return icon.slice(0, 2).toUpperCase();
}

interface Props {
  open: boolean;
  onClose: () => void;
  onCreateSession?: () => void;
  onNewFile?: () => void;
  onChooseFile?: () => void;
  onOpenSettings?: () => void;
  guiEnabled?: boolean;
  assistantEnabled?: boolean;
  onShowShortcuts?: () => void;
  onError?: (message: string) => void;
  templates?: SessionTemplate[];
  onLaunchTemplate?: (template: SessionTemplate) => void | Promise<void>;
  onSaveWorkspaceLayout?: () => boolean | void | Promise<boolean | void>;
  onRestoreWorkspaceLayout?: (
    layoutId: string,
  ) => boolean | void | Promise<boolean | void>;
}

function fuzzyMatch(pattern: string, text: string): boolean {
  const p = pattern.toLowerCase();
  const t = text.toLowerCase();
  let pi = 0;
  for (let ti = 0; ti < t.length && pi < p.length; ti++) {
    if (t[ti] === p[pi]) pi++;
  }
  return pi === p.length;
}

const CommandPalette: Component<Props> = (props) => {
  const navigate = useNavigate();
  const narrow = createNarrow();
  const [query, setQuery] = createSignal("");
  const [selectedIndex, setSelectedIndex] = createSignal(0);
  const [historyResults, setHistoryResults] = createSignal<HistorySearchResult[]>([]);
  const [fileResults, setFileResults] = createSignal<FileSearchResult[]>([]);
  const [symbolResults, setSymbolResults] = createSignal<EditorSymbolResult[]>([]);
  const [symbolMessage, setSymbolMessage] = createSignal<string | null>(null);
  const [agentTasks, setAgentTasks] = createSignal<AgentTask[]>([]);
  const [projectCommands, setProjectCommands] = createSignal<Command[]>([]);
  const [workItems, setWorkItems] = createSignal<VogtWorkItem[]>([]);
  const [vogtProjects, setVogtProjects] = createSignal<
    { slug: string; name: string }[]
  >([]);
  const [providerStates, setProviderStates] = createSignal<
    Partial<Record<ProviderName, ProviderState>>
  >({});
  const [providerErrors, setProviderErrors] = createSignal<
    Partial<Record<ProviderName, string>>
  >({});
  const [specialSearchState, setSpecialSearchState] = createSignal<ProviderState>("idle");
  const [specialSearchError, setSpecialSearchError] = createSignal<string | null>(null);
  const [savedLayouts, setSavedLayouts] = createSignal<SavedWorkspaceLayout[]>([]);
  const paletteId = createUniqueId();
  const inputId = `command-palette-input-${paletteId}`;
  const listboxId = `command-palette-results-${paletteId}`;
  const instructionsId = `command-palette-instructions-${paletteId}`;
  const statusId = `command-palette-status-${paletteId}`;
  let searchTimer: ReturnType<typeof setTimeout> | undefined;
  let specialSearchAbort: AbortController | undefined;
  const providerControllers = new Set<AbortController>();
  let loadedProviderGeneration = providerCacheGeneration;
  let loadedProviderKey = "";

  const setProviderStatus = (
    name: ProviderName,
    state: ProviderState,
    message?: string,
  ) => {
    setProviderStates((current) => ({ ...current, [name]: state }));
    setProviderErrors((current) => {
      const next = { ...current };
      if (message) next[name] = message;
      else delete next[name];
      return next;
    });
  };

  const withProviderController = async <T,>(
    run: (signal: AbortSignal) => Promise<T>,
  ): Promise<T> => {
    const controller = new AbortController();
    providerControllers.add(controller);
    try {
      return await run(controller.signal);
    } finally {
      providerControllers.delete(controller);
    }
  };

  const clearLoadedProviders = () => {
    setAgentTasks([]);
    setWorkItems([]);
    setVogtProjects([]);
    setProjectCommands([]);
    setProviderStates({});
    setProviderErrors({});
  };

  function loadKnownProviders(force = false): void {
    if (force) {
      for (const controller of providerControllers) controller.abort();
      invalidateCommandPaletteProviders();
    }
    const cache = currentProviderCache();
    if (
      force ||
      loadedProviderGeneration !== providerCacheGeneration ||
      (loadedProviderKey && loadedProviderKey !== cache.key)
    ) {
      for (const controller of providerControllers) controller.abort();
      clearLoadedProviders();
    }
    loadedProviderGeneration = providerCacheGeneration;
    loadedProviderKey = cache.key;

    if (cache.agentTasks) {
      setAgentTasks(cache.agentTasks);
      setProviderStatus("agent tasks", "ready");
    } else {
      setProviderStatus("agent tasks", "loading");
      void withProviderController((signal) => api.listAgentTasks(signal))
        .then((tasks) => {
          currentProviderCache().agentTasks = tasks;
          setAgentTasks(tasks);
          setProviderStatus("agent tasks", "ready");
        })
        .catch((error: unknown) => {
          if (aborted(error)) return;
          setProviderStatus("agent tasks", "failed", (error as Error).message);
        });
    }

    if (cache.workItems) {
      setWorkItems(cache.workItems);
      setProviderStatus("work items", "ready");
    } else {
      setProviderStatus("work items", "loading");
      void withProviderController((signal) => listWork({ limit: 200 }, signal))
        .then((answer) => {
          const items = answer.items ?? [];
          currentProviderCache().workItems = items;
          setWorkItems(items);
          setProviderStatus("work items", "ready");
        })
        .catch((error: unknown) => {
          if (aborted(error)) return;
          setProviderStatus("work items", "failed", (error as Error).message);
        });
    }

    if (cache.vogtProjects) {
      setVogtProjects(cache.vogtProjects);
      setProviderStatus("projects", "ready");
    } else {
      setProviderStatus("projects", "loading");
      void withProviderController(() => taxonomy.projects())
        .then((answer) => {
          const projects = answer.projects ?? [];
          currentProviderCache().vogtProjects = projects;
          setVogtProjects(projects);
          setProviderStatus("projects", "ready");
        })
        .catch((error: unknown) => {
          if (aborted(error)) return;
          setProviderStatus("projects", "failed", (error as Error).message);
        });
    }
  }

  createEffect(() => {
    if (!props.open) {
      for (const controller of providerControllers) controller.abort();
      specialSearchAbort?.abort();
      return;
    }
    // Each invocation is a fresh navigation act. A closed palette does not
    // retain a stale query or selection that could make Enter run a command
    // the user cannot see when reopening it later.
    setQuery("");
    setSelectedIndex(0);
    setHistoryResults([]);
    setFileResults([]);
    setSymbolResults([]);
    setSymbolMessage(null);
    setSavedLayouts(listWorkspaceLayouts());
    loadKnownProviders();
  });

  const activeEditorTab = () => {
    const activeId = tabsStore.active;
    const active = tabsStore.tabs.find((tab) => tab.id === activeId);
    return active?.kind === "editor" ? active : null;
  };

  const filterSymbols = (symbols: EditorSymbolResult[], term: string) => {
    const q = term.trim();
    if (!q) return symbols;
    return symbols.filter(
      (symbol) =>
        fuzzyMatch(q, symbol.name) ||
        fuzzyMatch(q, symbol.detail) ||
        fuzzyMatch(q, symbol.containerName ?? "") ||
        fuzzyMatch(q, symbol.path),
    );
  };

  const launchWorkspaceCommand = async (
    label: string,
    commandLine: string,
    cwd = "",
  ) => {
    try {
      const session = await createSession(
        label.toLowerCase().replace(/[^a-z0-9]+/g, "-"),
        ["bash", "-lc", commandLine],
        cwd || undefined,
      );
      openTerminalTab(session.id, session.name);
      navigate(`/t/${session.id}`);
      props.onClose();
    } catch (e) {
      props.onError?.(`Failed to launch command: ${(e as Error).message}`);
    }
  };

  const loadProjectCommands = async (signal: AbortSignal) => {
    try {
      setProviderStatus("workspace actions", "loading");
      const cached = currentProviderCache().detectedProjects;
      let detectedProjects: DetectedProject[];

      if (cached) {
        detectedProjects = cached;
      } else {
        const manifestResults = await mapWithConcurrency(
          PROJECT_MANIFEST_QUERIES,
          3,
          (name) => api.searchFiles(name, "", 40, signal),
        );
        const projects = new Map<string, MutableDetectedProject>();

        const ensureProject = (cwd: string): MutableDetectedProject => {
          let existing = projects.get(cwd);
          if (!existing) {
            existing = {
              cwd,
              packageManager: "npm",
              packageScripts: [],
              hasCargo: false,
              hasPyproject: false,
              hasJustfile: false,
              hasMakefile: false,
            };
            projects.set(cwd, existing);
          }
          return existing;
        };

        for (const matches of manifestResults) {
          for (const file of matches) {
            if (isIgnoredProjectPath(file.path)) continue;
            const name = leafName(file.path);
            const cwd = parentDir(file.path);
            const project = ensureProject(cwd);
            if (name === "pnpm-lock.yaml") {
              project.packageManager = "pnpm";
            } else if (name === "yarn.lock" && project.packageManager !== "pnpm") {
              project.packageManager = "yarn";
            } else if (name === "package-lock.json" && project.packageManager === "npm") {
              project.packageManager = "npm";
            } else if (name === "package.json") {
              project.packageJsonPath = file.path;
            } else if (name === "Cargo.toml") {
              project.hasCargo = true;
            } else if (name === "pyproject.toml") {
              project.hasPyproject = true;
            } else if (name === "Justfile" || name === "justfile") {
              project.hasJustfile = true;
            } else if (name === "Makefile") {
              project.hasMakefile = true;
            }
          }
        }

        await mapWithConcurrency([...projects.values()], 4, async (project) => {
          if (!project.packageJsonPath) return;
          try {
            const pkg = await api.readFile(project.packageJsonPath, signal);
            if (pkg.is_binary || !pkg.content) return;
            const parsed = JSON.parse(pkg.content) as PackageJson;
            project.packageName = parsed.name;
            const scripts = parsed.scripts ?? {};
            project.packageScripts = [
              ...PREFERRED_PACKAGE_SCRIPTS.filter((name) => name in scripts),
              ...Object.keys(scripts)
                .filter((name) => !PREFERRED_PACKAGE_SCRIPTS.includes(name))
                .sort(),
            ].slice(0, 5);
          } catch (error) {
            if (aborted(error)) throw error;
            /* package metadata is optional for shortcuts */
          }
        });

        detectedProjects = [...projects.values()]
          .filter(
            (project) =>
              project.packageJsonPath ||
              project.hasCargo ||
              project.hasPyproject ||
              project.hasJustfile ||
              project.hasMakefile,
          )
          .sort((a, b) => comparePathDepth(a.cwd, b.cwd))
          .map((project) => ({
            cwd: project.cwd,
            displayName: project.packageName || pathLabel(project.cwd),
            packageManager: project.packageManager,
            packageScripts: project.packageScripts,
            hasCargo: project.hasCargo,
            hasPyproject: project.hasPyproject,
            hasJustfile: project.hasJustfile,
            hasMakefile: project.hasMakefile,
          }));
        currentProviderCache().detectedProjects = detectedProjects;
      }

      const commands: Command[] = [];
      for (const project of detectedProjects) {
        const location = pathLabel(project.cwd);

        commands.push({
          id: `project-terminal-${project.cwd || "root"}`,
          label: `Open terminal in ${project.displayName}`,
          description: location,
          icon: ">_",
          action: () => launchWorkspaceCommand(`shell-${project.displayName}`, "bash", project.cwd),
          category: "Project Actions",
        });

        commands.push({
          id: `project-git-${project.cwd || "root"}`,
          label: `Open git status for ${project.displayName}`,
          description: location,
          icon: "git",
          action: () => {
            openGitTab(project.cwd);
            navigate(project.cwd ? `/g/${encodeURIComponent(project.cwd)}` : "/g/");
            props.onClose();
          },
          category: "Project Actions",
        });

        commands.push(
          ...project.packageScripts.map((script) => ({
            id: `project-${project.cwd || "root"}-${project.packageManager}-${script}`,
            label: `Run ${project.packageManager} ${script}`,
            description: `${project.displayName} • ${location}`,
            icon: "run",
            action: () =>
              launchWorkspaceCommand(
                `${project.displayName}-${script}`,
                project.packageManager === "npm"
                  ? `npm run ${script}`
                  : `${project.packageManager} ${script}`,
                project.cwd,
              ),
            category: "Project Actions",
          })),
        );

        if (project.hasCargo) {
          commands.push(
            {
              id: `project-${project.cwd || "root"}-cargo-test`,
              label: "Run cargo test",
              description: `${project.displayName} • ${location}`,
              icon: "Rs",
              action: () =>
                launchWorkspaceCommand(
                  `${project.displayName}-cargo-test`,
                  "cargo test --all",
                  project.cwd,
                ),
              category: "Project Actions",
            },
            {
              id: `project-${project.cwd || "root"}-cargo-clippy`,
              label: "Run cargo clippy",
              description: `${project.displayName} • ${location}`,
              icon: "Rs",
              action: () =>
                launchWorkspaceCommand(
                  `${project.displayName}-cargo-clippy`,
                  "cargo clippy --all-targets --all-features -- -D warnings",
                  project.cwd,
                ),
              category: "Project Actions",
            },
          );
        }

        if (project.hasPyproject) {
          commands.push(
            {
              id: `project-${project.cwd || "root"}-pytest`,
              label: "Run pytest",
              description: `${project.displayName} • ${location}`,
              icon: "Py",
              action: () =>
                launchWorkspaceCommand(`${project.displayName}-pytest`, "pytest", project.cwd),
              category: "Project Actions",
            },
            {
              id: `project-${project.cwd || "root"}-ruff`,
              label: "Run ruff check",
              description: `${project.displayName} • ${location}`,
              icon: "Py",
              action: () =>
                launchWorkspaceCommand(
                  `${project.displayName}-ruff`,
                  "ruff check .",
                  project.cwd,
                ),
              category: "Project Actions",
            },
          );
        }

        if (project.hasJustfile) {
          commands.push({
            id: `project-${project.cwd || "root"}-just`,
            label: "Run just",
            description: `${project.displayName} • ${location}`,
            icon: "run",
            action: () => launchWorkspaceCommand(`${project.displayName}-just`, "just", project.cwd),
            category: "Project Actions",
          });
        }

        if (project.hasMakefile) {
          commands.push({
            id: `project-${project.cwd || "root"}-make`,
            label: "Run make",
            description: `${project.displayName} • ${location}`,
            icon: "run",
            action: () => launchWorkspaceCommand(`${project.displayName}-make`, "make", project.cwd),
            category: "Project Actions",
          });
        }
      }

      setProjectCommands(commands);
      setProviderStatus("workspace actions", "ready");
    } catch (error) {
      if (aborted(error)) {
        setProviderStatus("workspace actions", "idle");
        return;
      }
      setProviderStatus("workspace actions", "failed", (error as Error).message);
    }
  };

  const requestWorkspaceActions = () => {
    const state = providerStates()["workspace actions"];
    if (state === "loading" || state === "ready") return;
    void withProviderController(loadProjectCommands);
  };

  // Specialized indices are deliberately query-driven. Opening the palette
  // renders static commands synchronously and does no workspace scan.
  const maybeSearchSpecial = (q: string) => {
    if (searchTimer) clearTimeout(searchTimer);
    specialSearchAbort?.abort();
    specialSearchAbort = undefined;
    setSpecialSearchState("idle");
    setSpecialSearchError(null);
    if (q.startsWith("#")) {
      requestWorkspaceActions();
      return;
    }
    if (!q.startsWith(">") && !q.startsWith("/") && !q.startsWith("@")) {
      setHistoryResults([]);
      setFileResults([]);
      setSymbolResults([]);
      setSymbolMessage(null);
      return;
    }
    const mode = q[0];
    const term = q.slice(1).trim();
    if (!term && mode !== "@") {
      setHistoryResults([]);
      setFileResults([]);
      setSymbolResults([]);
      setSymbolMessage(null);
      return;
    }
    searchTimer = setTimeout(async () => {
      const controller = new AbortController();
      specialSearchAbort = controller;
      setSpecialSearchState("loading");
      try {
        if (mode === ">") {
          setHistoryResults(await api.searchHistory(term));
          setFileResults([]);
          setSymbolResults([]);
          setSymbolMessage(null);
        } else {
          setHistoryResults([]);
          if (mode === "/") {
            const results = await api.searchFiles(term, "", undefined, controller.signal);
            setFileResults(results);
            setSymbolResults([]);
            setSymbolMessage(null);
            return;
          }
          setFileResults([]);
          const active = activeEditorTab();
          if (!active) {
            setSymbolResults([]);
            setSymbolMessage("Open a file to search symbols");
            return;
          }
          if (!hasRegisteredEditor(active.id)) {
            setSymbolResults([]);
            setSymbolMessage("Current file is still loading");
            return;
          }
          const symbols = await listEditorSymbols(active.id);
          if (controller.signal.aborted) return;
          const filtered = filterSymbols(symbols, term);
          setSymbolResults(filtered);
          setSymbolMessage(
            filtered.length > 0
              ? null
              : symbols.length > 0
                ? "No matching symbols"
                : "Current file does not provide symbol information",
          );
        }
        setSpecialSearchState("ready");
      } catch (error) {
        if (aborted(error)) return;
        setHistoryResults([]);
        setFileResults([]);
        setSymbolResults([]);
        setSymbolMessage(mode === "@" ? "Failed to load symbols" : null);
        setSpecialSearchState("failed");
        setSpecialSearchError((error as Error).message);
      }
    }, 250);
  };

  onCleanup(() => {
    if (searchTimer) clearTimeout(searchTimer);
    specialSearchAbort?.abort();
    for (const controller of providerControllers) controller.abort();
  });

  const historyCommands = (): Command[] => {
    return historyResults().map((r, i) => ({
      id: `history-${i}`,
      label: r.session_name,
      description: r.match_snippet.replace(/<\/?mark>/g, ""),
      icon: "?",
      action: () => {
        openHistoryTab();
        navigate(historyResultUrl(query().slice(1), r));
        props.onClose();
      },
      category: "History Matches",
    }));
  };

  const fileCommands = (): Command[] => {
    return fileResults().map((file, i) => ({
      id: `file-${i}`,
      label: file.name,
      description: file.path,
      icon: "file",
      action: () => {
        openEditorTab(file.path);
        navigate(`/e/${encodeURIComponent(file.path)}`);
        props.onClose();
      },
      category: "File Matches",
    }));
  };

  const symbolCommands = (): Command[] => {
    const active = activeEditorTab();
    if (!active) {
      return [
        {
          id: "symbol-no-editor",
          label: "Open a file to search symbols",
          description: "Symbol search works against the active editor tab",
          icon: "sym",
          action: () => undefined,
          category: "Current File Symbols",
        },
      ];
    }

    if (symbolMessage()) {
      return [
        {
          id: "symbol-message",
          label: symbolMessage()!,
          description: active.path,
          icon: "sym",
          action: () => undefined,
          category: "Current File Symbols",
        },
      ];
    }

    return symbolResults().map((symbol, i) => ({
      id: `symbol-${i}`,
      label: symbol.name,
      description: [
        symbol.containerName || active.path,
        `line ${symbol.line}`,
        symbol.detail,
      ]
        .filter(Boolean)
        .join(" • "),
      icon: "sym",
      action: () => {
        focusTab(active.id);
        navigate(`/e/${encodeURIComponent(active.path)}`);
        props.onClose();
        window.setTimeout(() => {
          focusEditorRange(active.id, symbol.range);
        }, 0);
      },
      category: "Current File Symbols",
    }));
  };

  const baseCommands = (): Command[] => {
    const commands: Command[] = [
      // -- Vogt (M11, FR-U16) ------------------------------------------
      //
      // Every entry here *navigates*. None of them writes: FR-U16 says the
      // palette reaches a mutating verb by opening the view that collects
      // its reason, and never by executing it — a palette entry cannot type
      // a reason any more than a button can, and r6's rule is about where
      // the reason comes from, not how the operation was reached.
      {
        id: "vogt-board",
        label: "Open Board",
        description: "Work items by workflow state",
        icon: "",
        action: () => {
          navigate(surfaceHref(recentPlacesStore.places, "/board"));
          props.onClose();
        },
        category: "Vogt",
      },
      {
        id: "vogt-backlog",
        label: "Open Backlog",
        description: "The ranked backlog and bugs, with the reason for the order",
        icon: "",
        action: () => {
          navigate(surfaceHref(recentPlacesStore.places, "/backlog"));
          props.onClose();
        },
        category: "Vogt",
      },
      {
        id: "vogt-projects",
        label: "Open Projects",
        description: "Per-project state, compliance and the drift inbox",
        icon: "",
        action: () => {
          navigate("/projects");
          props.onClose();
        },
        category: "Vogt",
      },
      {
        id: "vogt-audit",
        label: "Open Audit",
        description: "Who wrote what, and the reason they gave",
        icon: "",
        action: () => {
          navigate("/audit");
          props.onClose();
        },
        category: "Vogt",
      },
      // FR-U16's mutating verbs. Every one of these *opens the view that
      // collects the reason* and none of them performs anything — the rule is
      // the point, and `test_pwa.py` asserts it by import, so a palette entry
      // that called a write would fail the suite rather than ship.
      //
      // Which verbs are here and which are not is a decision worth stating. A
      // verb whose collector is a *place* gets an entry: quick-create, the
      // drift inbox, the import form. A verb that needs a subject first —
      // transition, comment, start a session — does not, because "Comment
      // on..." with no item cannot open a form that collects anything, and an
      // entry per verb per item would multiply this list by five. Those are
      // reached through the item's own entry, which the palette already offers
      // by fuzzy name and which opens the page carrying all three forms.
      // Moving a card is the board's, which has its own entry above.
      {
        id: "vogt-new-work",
        label: "New Work Item...",
        description: "Opens the backlog's quick-create, which collects a reason",
        icon: "+",
        action: () => {
          navigate("/backlog?create=1");
          props.onClose();
        },
        category: "Vogt",
      },
      {
        id: "vogt-resolve-drift",
        label: "Resolve Drift...",
        description:
          "Opens the drift inbox, where each proposal shows both sides and " +
          "takes a typed reason",
        icon: "drift",
        action: () => {
          navigate("/projects?view=drift");
          props.onClose();
        },
        category: "Vogt",
      },
      {
        id: "vogt-import-project",
        label: "Import a Project...",
        description: "Opens the import form, which collects a reason",
        icon: "import",
        action: () => {
          navigate("/projects?view=import");
          props.onClose();
        },
        category: "Vogt",
      },
      // The places the rail and the phone bar reach — the palette reaches them
      // too, so the keyboard is never a poorer map than the chrome (#230). Each
      // navigates and writes nothing, save for Sign out, which is the account's
      // own deliberate hand-back and lives here because the phone More sheet
      // offers it alongside the places.
      {
        id: "open-inbox",
        label: "Open Inbox",
        description: "Attention items awaiting a decision",
        icon: "inbox",
        action: () => {
          navigate("/inbox");
          props.onClose();
        },
        category: "Inbox",
      },
      {
        id: "open-sessions",
        label: "Open Sessions",
        description: "Every terminal session, running or idle",
        icon: ">_",
        action: () => {
          navigate("/sessions");
          props.onClose();
        },
        category: "Sessions",
      },
      {
        id: "open-history",
        label: "Open History",
        description: "Archived terminal output",
        icon: "hist",
        action: () => {
          openHistoryTab();
          navigate("/history");
          props.onClose();
        },
        category: "History",
      },
      {
        id: "open-git",
        label: "Open Git",
        description: "Git status for the workspace root",
        icon: "git",
        action: () => {
          openGitTab("");
          navigate("/g/");
          props.onClose();
        },
        category: "Git",
      },
      {
        id: "open-files-place",
        label: "Open Files",
        description: "The workspace file tree — browse, and upload into any folder",
        icon: "/",
        action: () => {
          navigate("/files");
          props.onClose();
        },
        category: "Files",
      },
      {
        id: "open-tasks-place",
        label: "Open Tasks",
        description: "Recurring agent tasks",
        icon: "tasks",
        action: () => {
          openTasksTab();
          navigate("/tasks");
          props.onClose();
        },
        category: "Tasks",
      },
      ...(props.assistantEnabled
        ? [
            {
              id: "open-assistant",
              label: "Open Assistant",
              description: "The configured voice and chat assistant",
              icon: "assistant",
              action: () => {
                openAssistantTab();
                navigate("/assistant");
                props.onClose();
              },
              category: "Assistant",
            },
          ]
        : []),
      {
        id: "sign-out",
        label: "Sign out",
        description: "Hand the credential back and return to the login screen",
        icon: "out",
        action: () => {
          props.onClose();
          signOut();
        },
        category: "Account",
      },
      {
      id: "new-session",
      label: "New Terminal Session",
      description: "Create a new shell session",
      icon: ">_",
      shortcut: shortcutKeys("new-terminal-session"),
      action: () => {
        props.onClose();
        props.onCreateSession?.();
      },
      category: "Sessions",
      },
      ...(props.onNewFile ? [{
      id: "new-file",
      label: "New File",
      description: "Choose a destination and filename in the workspace",
      icon: "file",
      action: () => {
        props.onClose();
        // Let Dialog restore the palette invoker before the next dialog
        // captures it. Cancellation can then return focus to the real caller.
        window.setTimeout(() => props.onNewFile?.(), 0);
      },
      category: "Files",
      }] : []),
      ...(props.onChooseFile ? [{
      id: "open-file",
      label: "Open File...",
      description: "Search workspace files and open one in the editor",
      icon: "dir",
      action: () => {
        props.onClose();
        window.setTimeout(() => props.onChooseFile?.(), 0);
      },
      category: "Files",
      }] : []),
      {
      id: "git-status",
      label: "Git Status",
      description: "Open git status view for workspace root",
      icon: "git",
      action: () => {
        openGitTab("");
        navigate("/g/");
        props.onClose();
      },
      category: "Git",
      },
      {
      id: "search-history",
      label: "Search History",
      description: "Search through session history",
      icon: "?",
      action: () => {
        openHistoryTab();
        navigate("/history?focus=search");
        props.onClose();
      },
      category: "History",
      },
      {
      id: "open-tasks",
      label: "Open Agent Tasks",
      description: "Inspect and run recurring agent tasks",
      icon: "tasks",
      action: () => {
        openTasksTab();
        navigate("/tasks");
        props.onClose();
      },
      category: "Tasks",
      },
      {
      id: "open-settings",
      label: "Open Settings",
      description: "Configure token, layout, templates, notifications",
      icon: "set",
      action: () => {
        props.onOpenSettings?.();
        props.onClose();
      },
      category: "View",
      },
      {
      id: "show-shortcuts",
      label: "Keyboard Shortcuts",
      description: "View all keyboard shortcuts",
      icon: "key",
      shortcut: shortcutKeys("show-shortcut-help"),
      action: () => {
        props.onShowShortcuts?.();
        props.onClose();
      },
      category: "Help",
      },
      {
      id: "refresh-palette-data",
      label: "Refresh Command Palette Data",
      description: "Invalidate cached tasks, work items, projects, and workspace actions",
      icon: "↻",
      action: () => loadKnownProviders(true),
      category: "Help",
      },
    ];

    if (props.guiEnabled) {
      commands.splice(commands.length - 2, 0, {
        id: "open-gui",
        label: "Open GUI Stream",
        description: "Open the configured GUI stream tab",
        icon: ">_",
        action: () => {
          openGuiTab();
          navigate("/gui");
          props.onClose();
        },
        category: "View",
      });
    }

    if (props.onSaveWorkspaceLayout) {
      commands.splice(commands.length - 2, 0, {
        id: "save-workspace-layout",
        label: "Save Workspace Layout",
        description: "Capture the current tabs and layout mode in this browser",
        icon: "layout",
        action: async () => {
          props.onClose();
          await props.onSaveWorkspaceLayout?.();
          setSavedLayouts(listWorkspaceLayouts());
        },
        category: "Layouts",
      });
    }

    return commands;
  };

  const tabCommands = (): Command[] => {
    return tabsStore.tabs
      .filter((tab) => tab.kind !== "gui" || props.guiEnabled)
      .map((tab) => ({
        id: `tab-${tab.id}`,
        label: tab.label,
        description:
          tab.kind === "terminal"
            ? "Switch to terminal tab"
            : tab.kind === "editor"
              ? (tab.path || "Switch to editor tab")
              : tab.kind === "git"
                ? `Git ${tab.repo || "(workspace root)"}`
                : `Switch to ${tab.kind} tab`,
        icon:
          tab.kind === "editor"
            ? "file"
            : tab.kind === "git"
              ? "git"
              : tab.kind === "gui"
                ? ">_"
                : tab.kind === "history"
                  ? "hist"
                  : tab.kind === "tasks"
                    ? "tasks"
                    : ">_",
        action: () => {
          if (tab.kind === "terminal") {
            openTerminalTab(tab.sessionId, tab.label);
            navigate(`/t/${tab.sessionId}`);
          } else if (tab.kind === "editor") {
            openEditorTab(tab.path);
            navigate(`/e/${encodeURIComponent(tab.path)}`);
          } else if (tab.kind === "git") {
            openGitTab(tab.repo);
            navigate(tab.repo ? `/g/${encodeURIComponent(tab.repo)}` : "/g/");
          } else if (tab.kind === "gui") {
            openGuiTab();
            navigate("/gui");
          } else if (tab.kind === "history") {
            openHistoryTab();
            navigate("/history");
          } else {
            openTasksTab();
            navigate("/tasks");
          }
          props.onClose();
        },
        category: "Open Tabs",
      }));
  };

  const savedLayoutCommands = (): Command[] => {
    if (!props.onRestoreWorkspaceLayout) return [];
    return savedLayouts().map((layout) => ({
      id: `layout-${layout.id}`,
      label: `Restore ${layout.name}`,
      description: workspaceLayoutSummary(layout),
      icon: "layout",
      action: async () => {
        props.onClose();
        await props.onRestoreWorkspaceLayout?.(layout.id);
      },
      category: "Layouts",
    }));
  };

  const providerCommands = (): Command[] => {
    const commands: Command[] = [];
    const status = providerStates();
    const errors = providerErrors();
    const labels: ProviderName[] = ["agent tasks", "work items", "projects"];
    for (const name of labels) {
      if (status[name] === "loading") {
        commands.push({
          id: `provider-${name}-loading`,
          label: `Loading ${name}…`,
          description: "Other commands remain available",
          icon: "…",
          action: () => undefined,
          category: "Providers",
        });
      } else if (status[name] === "failed") {
        commands.push({
          id: `provider-${name}-failed`,
          label: `Retry ${name}`,
          description: errors[name] ?? `Failed to load ${name}`,
          icon: "!",
          action: () => loadKnownProviders(true),
          category: "Providers",
        });
      }
    }
    return commands;
  };

  const sessionCommands = (): Command[] => {
    return sessionsStore.order
      .map((id) => sessionsStore.sessions[id])
      .filter((s) => s != null)
      .map((s) => ({
        id: `session-${s.id}`,
        label: s.name,
        description: `Jump to session • ${s.cwd}`,
        icon: ">_",
        action: () => {
          openTerminalTab(s.id, s.name);
          navigate(`/t/${s.id}`);
          props.onClose();
        },
        category: "Sessions",
      }));
  };

  // Compose an existing session into the active terminal workspace (#212).
  // Only offered when a terminal tab is active and has registered its handle,
  // and only for sessions not already shown there. None of these create a PTY.
  const splitCommands = (): Command[] => {
    const activeTab = tabsStore.tabs.find((tab) => tab.id === tabsStore.active);
    if (activeTab?.kind !== "terminal") return [];
    const handle = terminalWorkspaceHandle(activeTab.id);
    if (!handle) return [];
    const shown = new Set(handle.shownSessionIds());
    const candidates = sessionsStore.order
      .map((id) => sessionsStore.sessions[id])
      .filter(
        (session): session is SessionSummary =>
          session != null && !shown.has(session.id),
      );
    const commands: Command[] = [];
    for (const session of candidates) {
      commands.push({
        id: `split-right-${session.id}`,
        label: `Split right with ${session.name}`,
        description: session.cwd || "Compose this session beside the current pane",
        icon: ">_",
        action: () => {
          handle.splitWithSession("row", session.id);
          props.onClose();
        },
        category: "Sessions",
      });
      commands.push({
        id: `split-down-${session.id}`,
        label: `Split down with ${session.name}`,
        description: session.cwd || "Compose this session below the current pane",
        icon: ">_",
        action: () => {
          handle.splitWithSession("column", session.id);
          props.onClose();
        },
        category: "Sessions",
      });
      commands.push({
        id: `show-in-pane-${session.id}`,
        label: `Show ${session.name} in this pane`,
        description: "Re-target the active pane without changing the layout",
        icon: ">_",
        action: () => {
          handle.showSessionInActivePane(session.id);
          props.onClose();
        },
        category: "Sessions",
      });
    }
    return commands;
  };

  const recentFileCommands = (): Command[] => {
    return getRecentFiles()
      .slice(0, 5)
      .map((f, i) => ({
        id: `recent-${i}`,
        label: f.path.split("/").pop() || f.path,
        description: f.path,
        icon: "file",
        action: () => {
          openEditorTab(f.path);
          navigate(`/e/${encodeURIComponent(f.path)}`);
          props.onClose();
        },
        category: "Recent Files",
      }));
  };

  const templateCommands = (): Command[] => {
    return (props.templates ?? []).map((template, index) => ({
      id: `template-${index}`,
      label: `Launch ${template.name}`,
      description: template.description || "Create a session from this preset",
      icon: "preset",
      action: async () => {
        await props.onLaunchTemplate?.(template);
        props.onClose();
      },
      category: "Presets",
    }));
  };

  const taskCommands = (): Command[] => {
    return agentTasks().map((task) => ({
      id: `task-${task.id}`,
      label: `Run ${task.name}`,
      description: `${task.status} • ${task.run_count} runs • ${task.schedule.kind}`,
      icon: task.status === "active" ? "run" : "paused",
      action: async () => {
        try {
          const run = await api.runAgentTask(task.id);
          openTerminalTab(run.session_id, run.session_name);
          navigate(`/t/${run.session_id}`);
          props.onClose();
        } catch (e) {
          props.onError?.(`Failed to run task: ${(e as Error).message}`);
        }
      },
      category: "Tasks",
    }));
  };

  // `limit` caps the two provider lists that the palette loads 200-deep for
  // name search (work items and projects). On the empty query only ~10 of each
  // are worth showing — enough that Open Tabs, Recent Files and Sessions stay
  // on screen instead of being pushed off by 200 work items. A typed query
  // passes no limit, so the scorer still searches the whole set.
  const allCommands = (limit?: number): Command[] => {
    const projects = limit ? vogtProjects().slice(0, limit) : vogtProjects();
    const items = limit ? workItems().slice(0, limit) : workItems();
    return [
      ...baseCommands(),
      ...projectCommands(),
      ...tabCommands(),
      ...recentFileCommands(),
      // Sessions before work items: a session name is a short, memorable thing
      // the operator typed themselves, so on an equal score it should surface
      // ahead of a work item that merely happens to tie (#230).
      ...sessionCommands(),
      ...splitCommands(),
      // A registered project, opened on its own page (FR-U16). The palette
      // navigates and does not write — `/projects?p=<slug>` is the same deep
      // link the Projects surface writes for itself, so what the keyboard
      // reaches and what a shared link reaches are the same place (FR-U11).
      ...projects.map<Command>((project) => ({
        id: `vogt-project-${project.slug}`,
        label: `Open project ${project.name}`,
        description: project.slug,
        icon: "project",
        action: () => {
          navigate(`/projects?p=${encodeURIComponent(project.slug)}`);
          props.onClose();
        },
        category: "Vogt",
      })),
      ...items.map<Command>((item) => ({
        id: `vogt-work-${item.ref}`,
        label: `${item.ref} — ${item.title}`,
        description: [item.kind, item.state, item.project_slug]
          .filter(Boolean)
          .join(" · "),
        icon: "work",
        action: () => {
          openWorkItemTab(item.ref);
          navigate(`/w/${encodeURIComponent(item.ref)}`);
          props.onClose();
        },
        category: "Vogt",
      })),
      ...templateCommands(),
      ...taskCommands(),
      ...savedLayoutCommands(),
      ...providerCommands(),
    ];
  };

  const EMPTY_QUERY_CAP = 10;

  // The last few commands the operator actually ran, resolved against the live
  // command set (an id whose command is gone this open is simply skipped) and
  // cloned with a distinct DOM id so a Recent row and the same command's own
  // row do not share an id. Shown only on the empty query, at the top.
  const recentCommands = (): Command[] => {
    const ids = readRecentCommandIds();
    if (ids.length === 0) return [];
    const byId = new Map(allCommands().map((command) => [command.id, command]));
    const out: Command[] = [];
    for (const id of ids) {
      const command = byId.get(id);
      if (command) {
        out.push({
          ...command,
          id: `${RECENT_ID_PREFIX}${command.id}`,
          category: "Recent",
        });
      }
    }
    return out;
  };

  const filteredCommands = () => {
    const q = query().trim();
    // History, filename, and symbol search modes.
    if (q.startsWith(">")) return historyCommands();
    if (q.startsWith("/")) return fileCommands();
    if (q.startsWith("@")) return symbolCommands();
    if (q.startsWith("#")) {
      const term = q.slice(1).trim();
      if (providerStates()["workspace actions"] === "loading") return [];
      if (providerStates()["workspace actions"] === "failed") {
        return [{
          id: "retry-workspace-actions",
          label: "Retry workspace actions",
          description: providerErrors()["workspace actions"] ?? "Workspace discovery failed",
          icon: "!",
          action: requestWorkspaceActions,
          category: "Providers",
        }];
      }
      if (!term) return projectCommands();
      return rankCommands(term, projectCommands());
    }
    // The empty query shows the recents first, then the capped base list —
    // with the recents removed from their own categories below, so a command
    // never appears twice (which would also collide two rows on one DOM id).
    if (!q) {
      const recent = recentCommands();
      const recentIds = new Set(recent.map((command) => baseIdOf(command.id)));
      const rest = allCommands(EMPTY_QUERY_CAP).filter(
        (command) => !recentIds.has(command.id),
      );
      return [...recent, ...rest];
    }
    // A typed query is scored, not merely filtered: a label match outranks a
    // description-only one, and the whole (uncapped) set is searched.
    return rankCommands(q, allCommands());
  };

  const selectedCommand = () => filteredCommands()[selectedIndex()];
  const optionId = (command: Command) =>
    `command-palette-option-${paletteId}-${encodeURIComponent(command.id)}`;

  createEffect(() => {
    const last = filteredCommands().length - 1;
    if (last < 0) {
      if (selectedIndex() !== 0) setSelectedIndex(0);
    } else if (selectedIndex() > last) {
      setSelectedIndex(last);
    }
  });

  createEffect(() => {
    const command = selectedCommand();
    if (!command) return;
    queueMicrotask(() => {
      document.getElementById(optionId(command))?.scrollIntoView?.({ block: "nearest" });
    });
  });

  // Every activation path runs a command through here, so recency is recorded
  // once, in one place, whether the command was reached by Enter or by click.
  const execute = (command: Command): void | Promise<void> => {
    recordRecentCommand(baseIdOf(command.id));
    return command.action();
  };

  const handleKeyDown = (e: KeyboardEvent) => {
    const cmds = filteredCommands();
    if (e.key === "ArrowDown") {
      e.preventDefault();
      if (cmds.length > 0) {
        setSelectedIndex((i) => Math.min(i + 1, cmds.length - 1));
      }
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setSelectedIndex((i) => Math.max(i - 1, 0));
    } else if (e.key === "Enter") {
      e.preventDefault();
      const cmd = cmds[selectedIndex()];
      if (cmd) {
        void execute(cmd);
      }
    }
  };

  // Reset selection when query changes
  const handleInput = (e: InputEvent) => {
    const value = (e.target as HTMLInputElement).value;
    setQuery(value);
    setSelectedIndex(0);
    maybeSearchSpecial(value);
  };

  // Typing `?` opens a legend for the prefix modes. Until now the modes were
  // spelled out only in the input placeholder, which vanishes the moment the
  // reader starts typing (#247).
  const PREFIX_MODES: { prefix: string; label: string }[] = [
    { prefix: "#", label: "Workspace actions" },
    { prefix: "@", label: "Symbols in the workspace" },
    { prefix: "/", label: "Files in the workspace" },
    { prefix: ">", label: "Session history" },
  ];
  const showModeHelp = () => query().trim() === "?";

  const specialMessage = () => {
    const q = query().trim();
    if (q.startsWith("#")) {
      const state = providerStates()["workspace actions"];
      if (state === "loading") return "Discovering workspace actions…";
      if (state === "failed") {
        return `Workspace actions unavailable: ${providerErrors()["workspace actions"] ?? "request failed"}`;
      }
    }
    if ([">", "/", "@"].includes(q[0] ?? "")) {
      if (specialSearchState() === "loading") return "Searching…";
      if (specialSearchState() === "failed") {
        return `Search unavailable: ${specialSearchError() ?? "request failed"}`;
      }
    }
    return null;
  };

  return (
    <Show when={props.open}>
      <Dialog
        label="Command palette"
        onClose={props.onClose}
        dialogClass="command-palette"
        backdropClass="command-palette-backdrop"
        dismissOnBackdrop
      >
        <div class="command-palette-header">
          <label class="visually-hidden" for={inputId}>Search commands</label>
          <input
            id={inputId}
            type="text"
            class="command-palette-input"
            placeholder={narrow()
              ? "Search, or # @ / > for modes…"
              : "Type a command, # for workspace actions, @ symbols, / files, or > history..."}
            value={query()}
            onInput={handleInput}
            onKeyDown={handleKeyDown}
            role="combobox"
            aria-autocomplete="list"
            aria-expanded="true"
            aria-controls={listboxId}
            aria-activedescendant={
              selectedCommand() ? optionId(selectedCommand()!) : undefined
            }
            aria-describedby={`${instructionsId} ${statusId}`}
            data-dialog-initial-focus
          />
          {/* A phone has no Escape key and the palette covers the backdrop it
              would otherwise tap, so leaving needs a control of its own. */}
          <button
            type="button"
            class="command-palette-close"
            aria-label="Close command palette"
            tabindex="-1"
            onClick={() => props.onClose()}
          >
            <span aria-hidden="true">×</span>
          </button>
        </div>
        <Show when={showModeHelp()}>
          <div class="command-palette-modehelp" aria-label="Prefix modes">
            <p class="command-palette-modehelp-title">Prefix modes</p>
            <For each={PREFIX_MODES}>
              {(mode) => (
                <div class="command-palette-modehelp-row">
                  <kbd>{mode.prefix}</kbd>
                  <span>{mode.label}</span>
                </div>
              )}
            </For>
          </div>
        </Show>
        <div
          id={listboxId}
          class="command-palette-results"
          role="listbox"
          aria-label="Command results"
          hidden={showModeHelp()}
        >
          <Show
            when={filteredCommands().length > 0}
            fallback={
              <div class="command-palette-empty" role="presentation">
                {specialMessage() ?? "No commands found"}
              </div>
            }
          >
            <For each={filteredCommands()}>
              {(cmd, index) => (
                <div
                  id={optionId(cmd)}
                  role="option"
                  aria-selected={selectedIndex() === index()}
                  class={`command-palette-item ${
                    selectedIndex() === index() ? "selected" : ""
                  }`}
                  onClick={() => void execute(cmd)}
                  onPointerMove={() => setSelectedIndex(index())}
                >
                  <span class="command-icon" aria-hidden="true">
                    {iconGlyph(cmd.icon ?? "")}
                  </span>
                  <div class="command-content">
                    <div class="command-label">{cmd.label}</div>
                    <Show when={cmd.description}>
                      <div class="command-description">{cmd.description}</div>
                    </Show>
                  </div>
                  <Show when={cmd.shortcut && cmd.shortcut.length > 0}>
                    <span class="command-shortcut" aria-hidden="true">
                      <For each={cmd.shortcut}>{(key) => <kbd>{key}</kbd>}</For>
                    </span>
                  </Show>
                  <Show when={cmd.category}>
                    <span class="command-category">{cmd.category}</span>
                  </Show>
                </div>
              )}
            </For>
          </Show>
        </div>
        <div id={statusId} class="visually-hidden" role="status" aria-live="polite">
          {filteredCommands().length === 0
            ? (specialMessage() ?? "No commands found")
            : `${filteredCommands().length} command${filteredCommands().length === 1 ? "" : "s"} found. ${selectedCommand()?.label ?? ""} selected.`}
        </div>
        <div class="command-palette-footer">
          <span id={instructionsId} class="command-palette-hint">↑↓ Navigate</span>
          <span class="command-palette-hint">↵ Select</span>
          <span class="command-palette-hint">Esc Close</span>
          <button
            type="button"
            class="command-palette-footer-close"
            tabindex="-1"
            onClick={() => props.onClose()}
          >
            Close
          </button>
        </div>
      </Dialog>
    </Show>
  );
};

export default CommandPalette;
