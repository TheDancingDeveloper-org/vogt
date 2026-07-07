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
  focusTab,
  openGitTab,
  openTerminalTab,
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

interface HistorySearchResult {
  session_id: string;
  session_name: string;
  match_snippet: string;
}

interface PackageJson {
  name?: string;
  scripts?: Record<string, string>;
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
      const entries = await api.listDir("");
      const names = new Set(entries.map((entry) => entry.name));
      const commands: Command[] = [];

      const lockfileRunner = names.has("pnpm-lock.yaml")
        ? "pnpm"
        : names.has("yarn.lock")
          ? "yarn"
          : "npm";
      const hasPackageJson = names.has("package.json");
      const hasCargoToml = names.has("Cargo.toml");
      const hasJustfile = names.has("Justfile") || names.has("justfile");
      const hasMakefile = names.has("Makefile");

      if (hasPackageJson) {
        try {
          const pkg = await api.readFile("package.json");
          if (!pkg.is_binary && pkg.content) {
            const parsed = JSON.parse(pkg.content) as PackageJson;
            const scripts = parsed.scripts ?? {};
            const preferred = ["dev", "typecheck", "build", "test", "lint", "start"];
            const ordered = [
              ...preferred.filter((name) => name in scripts),
              ...Object.keys(scripts)
                .filter((name) => !preferred.includes(name))
                .sort(),
            ].slice(0, 6);
            commands.push(
              ...ordered.map((script) => ({
                id: `project-script-${script}`,
                label: `Run ${lockfileRunner} ${script}`,
                description: parsed.name
                  ? `${parsed.name} • package.json script`
                  : "package.json script",
                icon: "▶",
                action: () =>
                  launchWorkspaceCommand(
                    `${lockfileRunner}-${script}`,
                    lockfileRunner === "npm"
                      ? `npm run ${script}`
                      : `${lockfileRunner} ${script}`,
                  ),
                category: "Project Actions",
              })),
            );
          }
        } catch {
          /* package metadata is optional for shortcuts */
        }
      }

      if (hasCargoToml) {
        commands.push(
          {
            id: "project-cargo-test",
            label: "Run cargo test",
            description: "Rust workspace checks",
            icon: "🦀",
            action: () => launchWorkspaceCommand("cargo-test", "cargo test --all"),
            category: "Project Actions",
          },
          {
            id: "project-cargo-clippy",
            label: "Run cargo clippy",
            description: "Rust lint pass",
            icon: "🦀",
            action: () =>
              launchWorkspaceCommand(
                "cargo-clippy",
                "cargo clippy --all-targets --all-features -- -D warnings",
              ),
            category: "Project Actions",
          },
        );
      }

      if (hasJustfile) {
        commands.push({
          id: "project-just",
          label: "Run just",
          description: "Task runner from Justfile",
          icon: "▶",
          action: () => launchWorkspaceCommand("just", "just"),
          category: "Project Actions",
        });
      }

      if (hasMakefile) {
        commands.push({
          id: "project-make",
          label: "Run make",
          description: "Task runner from Makefile",
          icon: "▶",
          action: () => launchWorkspaceCommand("make", "make"),
          category: "Project Actions",
        });
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
      icon: "🔍",
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
      icon: "📄",
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
          icon: "@",
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
          icon: "@",
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
      icon: "@",
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
      {
      id: "new-session",
      label: "New Terminal Session",
      description: "Create a new shell session",
      icon: "🖥",
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
      icon: "📄",
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
      icon: "📁",
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
      icon: "⎇",
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
      icon: "🔍",
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
      icon: "≡",
      action: () => {
        openTasksTab();
        navigate("/tasks");
        props.onClose();
      },
      category: "Tasks",
      },
      {
      id: "open-gui",
      label: "Open GUI Stream",
      description: "Open the GUI stream tab",
      icon: "🖥",
      action: () => {
        openGuiTab();
        navigate("/gui");
        props.onClose();
      },
      category: "View",
      },
      {
      id: "open-settings",
      label: "Open Settings",
      description: "Configure token, layout, templates, notifications",
      icon: "⚙",
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
      icon: "⌨",
      action: () => {
        props.onShowShortcuts?.();
        props.onClose();
      },
      category: "Help",
      },
    ];

    if (props.onSaveWorkspaceLayout) {
      commands.splice(commands.length - 2, 0, {
        id: "save-workspace-layout",
        label: "Save Workspace Layout",
        description: "Capture the current tabs and layout mode in this browser",
        icon: "◫",
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
    return tabsStore.tabs.map((tab) => ({
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
          ? "📄"
          : tab.kind === "git"
            ? "⎇"
            : tab.kind === "gui"
              ? "🖥"
              : tab.kind === "history"
                ? "📜"
                : tab.kind === "tasks"
                  ? "≡"
                  : "💻",
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
      icon: "◫",
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
        icon: "💻",
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
        icon: "📄",
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
      icon: "◇",
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
      icon: task.status === "active" ? "▶" : "⏸",
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
      <div
        class="command-palette-backdrop"
        onPointerDown={(e) => {
          if (e.target === e.currentTarget) props.onClose();
        }}
      >
        <div class="command-palette">
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
        </div>
      </div>
    </Show>
  );
};

export default CommandPalette;
