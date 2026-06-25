import { Component, For, Show, createSignal } from "solid-js";
import Editor from "./Editor";
import type { Tab } from "./tabs";
import { focusTab, tabsStore } from "./tabs";

interface Props {
  onNotify?: (message: string, kind?: "info" | "error") => void;
}

/**
 * EditorWorkspace provides an IDE-like layout with:
 * - Persistent file tree sidebar
 * - Main editor area with tabs
 * - Breadcrumb navigation
 *
 * This is an alternative to the default tabbed layout.
 */
const EditorWorkspace: Component<Props> = (_props) => {
  const [sidebarCollapsed, setSidebarCollapsed] = createSignal(false);

  const editorTabs = () => tabsStore.tabs.filter((t) => t.kind === "editor");
  const activeEditorTab = () => {
    const active = tabsStore.active;
    return editorTabs().find((t) => t.id === active);
  };

  return (
    <div class="editor-workspace">
      <Show when={!sidebarCollapsed()}>
        <aside class="editor-sidebar">
          <div class="editor-sidebar-header">
            <span class="editor-sidebar-title">Files</span>
            <button
              class="editor-sidebar-collapse"
              onClick={() => setSidebarCollapsed(true)}
              title="Collapse sidebar"
            >
              ‹
            </button>
          </div>
          <div class="editor-sidebar-content">
            {/* FileTree will be integrated here */}
            <div class="editor-sidebar-placeholder">
              File tree integration pending
            </div>
          </div>
        </aside>
      </Show>

      <Show when={sidebarCollapsed()}>
        <button
          class="editor-sidebar-expand"
          onClick={() => setSidebarCollapsed(false)}
          title="Expand sidebar"
        >
          ›
        </button>
      </Show>

      <main class="editor-main">
        <Show
          when={editorTabs().length > 0}
          fallback={
            <div class="editor-empty">
              <div class="editor-empty-message">
                <h3>No files open</h3>
                <p>Open a file from the sidebar to start editing</p>
              </div>
            </div>
          }
        >
          <div class="editor-tabs">
            <For each={editorTabs()}>
              {(tab) => (
                <button
                  class={`editor-tab ${tabsStore.active === tab.id ? "active" : ""}`}
                  onClick={() => focusTab(tab.id)}
                >
                  <span class="editor-tab-label">{tab.label}</span>
                  <Show when={tab.kind === "editor" && (tab as Extract<Tab, { kind: "editor" }>).dirty}>
                    <span class="editor-tab-dirty" title="Unsaved changes">●</span>
                  </Show>
                </button>
              )}
            </For>
          </div>

          <div class="editor-content">
            <Show when={activeEditorTab()}>
              {(tab) => (
                <Editor
                  tabId={tab().id}
                  path={(tab() as Extract<Tab, { kind: "editor" }>).path}
                />
              )}
            </Show>
          </div>
        </Show>
      </main>
    </div>
  );
};

export default EditorWorkspace;
