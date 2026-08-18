import {
  Component,
  For,
  Show,
  createEffect,
  createSignal,
  onCleanup,
  onMount,
} from "solid-js";
import { useNavigate } from "@solidjs/router";
import { createSession, sessionsStore } from "./store";
import {
  listProjects,
  listWork,
  type WorkItem as VogtWorkItem,
} from "./vogtApi";
import {
  focusTab,
  openGitTab,
  openTerminalTab,
  openWorkItemTab,
  openHistoryTab,
  openEditorTab,
  openGuiTab,
  openTasksTab,
  tabsStore,
} from "./tabs";
import { getRecentFiles } from "./recentFiles";
import {
  api,
  type AgentTask,
  type FileSearchResult,
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
  action: () => void | Promise<void>;
  category?: string;
}

interface Props {
  open: boolean;
  onClose: () => void;
  onCreateSession?: () => void;
  onOpenFile?: () => void;
  onOpenSettings?: () => void;
  guiEnabled?: boolean;
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
  const [savedLayouts, setSavedLayouts] = createSignal<SavedWorkspaceLayout[]>([]);
  let inputRef: HTMLInputElement | undefined;
  let searchTimer: ReturnType<typeof setTimeout> | undefined;

  createEffect(() => {
    if (!props.open) return;
    setSavedLayouts(listWorkspaceLayouts());
    void api
      .listAgentTasks()
      .then((tasks) => setAgentTasks(tasks))
      .catch((e) => {
        setAgentTasks([]);
        props.onError?.(`Failed to load agent tasks: ${(e as Error).message}`);
      });
    void loadProjectCommands();
    // Work items, so the palette reaches them by name and not only the
    // surfaces that list them (FR-U16). Loaded once per opening: the list is
    // small, the filter is local, and a keystroke-per-request palette is how
    // a tracker becomes slow. A Vogt that cannot be asked simply contributes
    // nothing here — the surfaces are where an outage is reported, not a
    // command list somebody is typing into.
    void listWork({ limit: 200 })
      .then((answer) => setWorkItems(answer.items ?? []))
      .catch(() => setWorkItems([]));
    // Projects, for the same reason and on the same terms (FR-U16). "Every
    // read surface (projects, work items, sessions, views) by fuzzy name" —
    // and until now the palette imported `listWork` and nothing else, so
    // "open project rustnzb" was not a thing the keyboard could do. The
    // estate is a short list; 200 is the same cap the surfaces use.
    void listProjects({ limit: 200 })
      .then((answer) => setVogtProjects(answer.projects ?? []))
      .catch(() => setVogtProjects([]));
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

  const loadProjectCommands = async () => {
    try {
      const manifestResults = await Promise.all(
        PROJECT_MANIFEST_QUERIES.map((name) => api.searchFiles(name, "", 40)),
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

      await Promise.all(
        [...projects.values()].map(async (project) => {
          if (!project.packageJsonPath) return;
          try {
            const pkg = await api.readFile(project.packageJsonPath);
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
          } catch {
            /* package metadata is optional for shortcuts */
          }
        }),
      );

      const detectedProjects: DetectedProject[] = [...projects.values()]
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
    } catch {
      setProjectCommands([]);
    }
  };

  // When the query starts with ">" or "/", search specialized indices
  // (history, filenames, or current-file symbols) without mixing them into
  // the base command list.
  const maybeSearchSpecial = (q: string) => {
    if (searchTimer) clearTimeout(searchTimer);
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
      try {
        if (mode === ">") {
          const res = await fetch(
            `${api.getBase()}/api/history/search?q=${encodeURIComponent(term)}`,
            { headers: { Authorization: `Bearer ${api.getToken()}` } },
          );
          if (res.ok) {
            setHistoryResults((await res.json()) as HistorySearchResult[]);
          } else {
            setHistoryResults([]);
          }
          setFileResults([]);
          setSymbolResults([]);
          setSymbolMessage(null);
        } else {
          setHistoryResults([]);
          if (mode === "/") {
            const results = await api.searchFiles(term);
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
      } catch {
        setHistoryResults([]);
        setFileResults([]);
        setSymbolResults([]);
        setSymbolMessage(mode === "@" ? "Failed to load symbols" : null);
      }
    }, 250);
  };

  onCleanup(() => {
    if (searchTimer) clearTimeout(searchTimer);
  });

  const historyCommands = (): Command[] => {
    return historyResults().map((r, i) => ({
      id: `history-${i}`,
      label: r.session_name,
      description: r.match_snippet.replace(/<\/?mark>/g, ""),
      icon: "?",
      action: () => {
        openHistoryTab();
        navigate("/history");
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
          navigate("/board");
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
          navigate("/backlog");
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
      {
      id: "new-session",
      label: "New Terminal Session",
      description: "Create a new shell session",
      icon: ">_",
      action: () => {
        props.onClose();
        props.onCreateSession?.();
      },
      category: "Sessions",
      },
      {
      id: "new-file",
      label: "New File",
      description: "Create a new file in the workspace",
      icon: "file",
      action: () => {
        props.onClose();
        props.onOpenFile?.();
      },
      category: "Files",
      },
      {
      id: "open-file",
      label: "Open File...",
      description: "Browse and open a file from workspace",
      icon: "dir",
      action: () => {
        props.onClose();
        // This would trigger file tree focus or file picker
        props.onOpenFile?.();
      },
      category: "Files",
      },
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
        navigate("/history");
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
      action: () => {
        props.onShowShortcuts?.();
        props.onClose();
      },
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

  const allCommands = (): Command[] => {
    return [
      ...baseCommands(),
      ...projectCommands(),
      // A registered project, opened on its own page (FR-U16). The palette
      // navigates and does not write — `/projects?p=<slug>` is the same deep
      // link the Projects surface writes for itself, so what the keyboard
      // reaches and what a shared link reaches are the same place (FR-U11).
      ...vogtProjects().map<Command>((project) => ({
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
      ...workItems().map<Command>((item) => ({
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
      ...tabCommands(),
      ...recentFileCommands(),
      ...sessionCommands(),
      ...templateCommands(),
      ...taskCommands(),
      ...savedLayoutCommands(),
    ];
  };

  const filteredCommands = () => {
    const q = query().trim();
    // History, filename, and symbol search modes.
    if (q.startsWith(">")) return historyCommands();
    if (q.startsWith("/")) return fileCommands();
    if (q.startsWith("@")) return symbolCommands();
    if (!q) return allCommands();
    return allCommands().filter(
      (cmd) =>
        fuzzyMatch(q, cmd.label) ||
        (cmd.description && fuzzyMatch(q, cmd.description)),
    );
  };

  const handleKeyDown = (e: KeyboardEvent) => {
    const cmds = filteredCommands();
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setSelectedIndex((i) => Math.min(i + 1, cmds.length - 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setSelectedIndex((i) => Math.max(i - 1, 0));
    } else if (e.key === "Enter") {
      e.preventDefault();
      const cmd = cmds[selectedIndex()];
      if (cmd) {
        void cmd.action();
      }
    } else if (e.key === "Escape") {
      e.preventDefault();
      props.onClose();
    }
  };

  onMount(() => {
    if (props.open) {
      inputRef?.focus();
    }
  });

  // Reset selection when query changes
  const handleInput = (e: InputEvent) => {
    const value = (e.target as HTMLInputElement).value;
    setQuery(value);
    setSelectedIndex(0);
    maybeSearchSpecial(value);
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
            <input
              ref={inputRef}
              type="text"
              class="command-palette-input"
              placeholder="Type a command, @ for symbols, / for files, or > for history..."
              value={query()}
              onInput={handleInput}
              onKeyDown={handleKeyDown}
              autofocus
            />
          </div>
          <div class="command-palette-results">
            <Show
              when={filteredCommands().length > 0}
              fallback={
                <div class="command-palette-empty">No commands found</div>
              }
            >
              <For each={filteredCommands()}>
                {(cmd, index) => (
                  <button
                    class={`command-palette-item ${
                      selectedIndex() === index() ? "selected" : ""
                    }`}
                    onClick={() => void cmd.action()}
                    onMouseEnter={() => setSelectedIndex(index())}
                  >
                    <Show when={cmd.icon}>
                      <span class="command-icon">{cmd.icon}</span>
                    </Show>
                    <div class="command-content">
                      <div class="command-label">{cmd.label}</div>
                      <Show when={cmd.description}>
                        <div class="command-description">{cmd.description}</div>
                      </Show>
                    </div>
                    <Show when={cmd.category}>
                      <span class="command-category">{cmd.category}</span>
                    </Show>
                  </button>
                )}
              </For>
            </Show>
          </div>
          <div class="command-palette-footer">
            <span>↑↓ Navigate</span>
            <span>↵ Select</span>
            <span>ESC Close</span>
          </div>
      </Dialog>
    </Show>
  );
};

export default CommandPalette;
