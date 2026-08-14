import { Component, For, Show, createSignal } from "solid-js";

interface Shortcut {
  keys: string[];
  description: string;
  category: string;
}

interface Props {
  open: boolean;
  onClose: () => void;
}

const shortcuts: Shortcut[] = [
  // Global Navigation
  { keys: ["Ctrl", "K"], description: "Open command palette", category: "Navigation" },
  { keys: ["Cmd", "K"], description: "Open command palette (Mac)", category: "Navigation" },
  { keys: ["Ctrl", "Shift", "T"], description: "New terminal session", category: "Navigation" },
  { keys: ["Ctrl", "Shift", "W"], description: "Close active tab", category: "Navigation" },
  { keys: ["Ctrl", "Alt", "←"], description: "Previous tab", category: "Navigation" },
  { keys: ["Ctrl", "Alt", "→"], description: "Next tab", category: "Navigation" },

  // Terminal
  { keys: ["Ctrl", "Shift", "C"], description: "Copy selection", category: "Terminal" },
  { keys: ["Ctrl", "Shift", "V"], description: "Paste", category: "Terminal" },
  { keys: ["Ctrl", "Shift", "A"], description: "Select all", category: "Terminal" },
  { keys: ["Ctrl", "C"], description: "Copy (with selection) or interrupt", category: "Terminal" },
  { keys: ["Right-click"], description: "Copy or paste", category: "Terminal" },
  { keys: ["Middle-click"], description: "Paste (Linux)", category: "Terminal" },

  // Editor
  { keys: ["Ctrl", "S"], description: "Save file", category: "Editor" },
  { keys: ["Ctrl", "F"], description: "Find", category: "Editor" },
  { keys: ["Ctrl", "H"], description: "Find and replace", category: "Editor" },
  { keys: ["Ctrl", "G"], description: "Go to line", category: "Editor" },
  { keys: ["Ctrl", "/"], description: "Toggle comment", category: "Editor" },
  { keys: ["Alt", "↑"], description: "Move line up", category: "Editor" },
  { keys: ["Alt", "↓"], description: "Move line down", category: "Editor" },
  { keys: ["Ctrl", "D"], description: "Add cursor to next match", category: "Editor" },

  // Command Palette
  { keys: ["↑"], description: "Navigate up", category: "Command Palette" },
  { keys: ["↓"], description: "Navigate down", category: "Command Palette" },
  { keys: ["Enter"], description: "Execute command", category: "Command Palette" },
  { keys: ["Esc"], description: "Close palette", category: "Command Palette" },

  // Special
  { keys: ["?"], description: "Show this help", category: "Help" },
];

const KeyboardShortcuts: Component<Props> = (props) => {
  const [searchQuery, setSearchQuery] = createSignal("");

  const categories = () => {
    const cats = new Set(shortcuts.map((s) => s.category));
    return Array.from(cats);
  };

  const filteredShortcuts = () => {
    const query = searchQuery().toLowerCase();
    if (!query) return shortcuts;
    return shortcuts.filter(
      (s) =>
        s.description.toLowerCase().includes(query) ||
        s.keys.some((k) => k.toLowerCase().includes(query)),
    );
  };

  const shortcutsByCategory = () => {
    const filtered = filteredShortcuts();
    const byCategory: Record<string, Shortcut[]> = {};
    filtered.forEach((s) => {
      if (!byCategory[s.category]) byCategory[s.category] = [];
      byCategory[s.category]!.push(s);
    });
    return byCategory;
  };

  const handleKeyDown = (e: KeyboardEvent) => {
    if (e.key === "Escape") {
      props.onClose();
    }
  };

  return (
    <Show when={props.open}>
      <div
        class="modal-backdrop"
        onClick={(e) => {
          if (e.target === e.currentTarget) props.onClose();
        }}
      >
        <div class="shortcuts-modal" onClick={(e) => e.stopPropagation()}>
          <div class="shortcuts-header">
            <h2>Keyboard Shortcuts</h2>
            <button class="shortcuts-close" onClick={props.onClose}>
              ×
            </button>
          </div>

          <input
            type="search"
            class="shortcuts-search"
            placeholder="Search shortcuts..."
            value={searchQuery()}
            onInput={(e) => setSearchQuery(e.currentTarget.value)}
            onKeyDown={handleKeyDown}
            autofocus
          />

          <div class="shortcuts-content">
            <For each={categories()}>
              {(category) => {
                const categoryShortcuts = shortcutsByCategory()[category];
                return (
                  <Show when={categoryShortcuts && categoryShortcuts.length > 0}>
                    <div class="shortcuts-category">
                      <h3>{category}</h3>
                      <div class="shortcuts-list">
                        <For each={categoryShortcuts}>
                          {(shortcut) => (
                            <div class="shortcut-item">
                              <span class="shortcut-description">{shortcut.description}</span>
                              <div class="shortcut-keys">
                                <For each={shortcut.keys}>
                                  {(key, idx) => (
                                    <>
                                      <kbd class="shortcut-key">{key}</kbd>
                                      <Show when={idx() < shortcut.keys.length - 1}>
                                        <span class="shortcut-plus">+</span>
                                      </Show>
                                    </>
                                  )}
                                </For>
                              </div>
                            </div>
                          )}
                        </For>
                      </div>
                    </div>
                  </Show>
                );
              }}
            </For>

            <Show when={filteredShortcuts().length === 0}>
              <div class="shortcuts-empty">No shortcuts found</div>
            </Show>
          </div>

          <div class="shortcuts-footer">
            Press <kbd>Esc</kbd> to close
          </div>
        </div>
      </div>
    </Show>
  );
};

export default KeyboardShortcuts;
