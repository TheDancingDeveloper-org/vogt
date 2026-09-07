import { Component, For, Show, createSignal } from "solid-js";
import Dialog from "./Dialog";
import { KEYBOARD_SHORTCUTS, type KeyboardShortcut } from "./keyboardShortcuts";

interface Props {
  open: boolean;
  onClose: () => void;
}

const KeyboardShortcuts: Component<Props> = (props) => {
  const [searchQuery, setSearchQuery] = createSignal("");

  const categories = () => {
    const cats = new Set(KEYBOARD_SHORTCUTS.map((s) => s.category));
    return Array.from(cats);
  };

  const filteredShortcuts = () => {
    const query = searchQuery().toLowerCase();
    if (!query) return KEYBOARD_SHORTCUTS;
    return KEYBOARD_SHORTCUTS.filter(
      (s) =>
        s.description.toLowerCase().includes(query) ||
        s.keys.some((k) => k.toLowerCase().includes(query)) ||
        s.contextLabel.toLowerCase().includes(query),
    );
  };

  const shortcutsByCategory = () => {
    const filtered = filteredShortcuts();
    const byCategory: Record<string, KeyboardShortcut[]> = {};
    filtered.forEach((s) => {
      if (!byCategory[s.category]) byCategory[s.category] = [];
      byCategory[s.category]!.push(s);
    });
    return byCategory;
  };

  return (
    <Show when={props.open}>
      <Dialog
        labelledBy="shortcuts-title"
        onClose={props.onClose}
        dialogClass="shortcuts-modal"
        dismissOnBackdrop
      >
        <div class="shortcuts-header">
          <h2 id="shortcuts-title">Keyboard Shortcuts</h2>
          <button
            class="shortcuts-close"
            aria-label="Close keyboard shortcuts"
            onClick={props.onClose}
          >
            ×
          </button>
        </div>

        <input
          type="search"
          class="shortcuts-search"
          placeholder="Search shortcuts..."
          value={searchQuery()}
          onInput={(e) => setSearchQuery(e.currentTarget.value)}
          data-dialog-initial-focus
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
                              <span class="shortcut-description">
                                {shortcut.description}
                                <small class="shortcut-context">{shortcut.contextLabel}</small>
                              </span>
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
      </Dialog>
    </Show>
  );
};

export default KeyboardShortcuts;
