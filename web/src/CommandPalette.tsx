import { Component, For, Show, createEffect, createSignal, onMount } from "solid-js";
import { useNavigate } from "@solidjs/router";
import { sessionsStore } from "./store";
import {
  openGitTab,
  openTerminalTab,
  openHistoryTab,
  openEditorTab,
  openGuiTab,
  openTasksTab,
  tabsStore,
} from "./tabs";
import { getRecentFiles } from "./recentFiles";
import { api, type AgentTask, type SessionTemplate } from "./api";
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
  const [agentTasks, setAgentTasks] = createSignal<AgentTask[]>([]);
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
  });

  // When the query starts with ">", search session history (debounced).
  const maybeSearchHistory = (q: string) => {
    if (searchTimer) clearTimeout(searchTimer);
    if (!q.startsWith(">")) {
      setHistoryResults([]);
      return;
    }
    const term = q.slice(1).trim();
    if (!term) {
      setHistoryResults([]);
      return;
    }
    searchTimer = setTimeout(async () => {
      try {
        const res = await fetch(
          `${api.getBase()}/api/history/search?q=${encodeURIComponent(term)}`,
          { headers: { Authorization: `Bearer ${api.getToken()}` } },
        );
        if (res.ok) {
          setHistoryResults((await res.json()) as HistorySearchResult[]);
        }
      } catch {
        setHistoryResults([]);
      }
    }, 250);
  };

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
    // History search mode: ">term" shows session-history matches only.
    if (q.startsWith(">")) return historyCommands();
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
    maybeSearchHistory(value);
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
              placeholder="Type a command, or > to search history..."
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
