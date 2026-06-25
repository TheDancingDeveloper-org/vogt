import { Component, For, Show, createSignal, onMount } from "solid-js";
import { useNavigate } from "@solidjs/router";
import { sessionsStore } from "./store";
import { openGitTab, openTerminalTab, openHistoryTab, openEditorTab } from "./tabs";
import { getRecentFiles } from "./recentFiles";

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
  let inputRef: HTMLInputElement | undefined;

  const baseCommands = (): Command[] => [
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
  ];

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

  const allCommands = (): Command[] => {
    return [...baseCommands(), ...recentFileCommands(), ...sessionCommands()];
  };

  const filteredCommands = () => {
    const q = query().trim();
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
    setQuery((e.target as HTMLInputElement).value);
    setSelectedIndex(0);
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
              placeholder="Type a command or search..."
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
