import {
  Component,
  For,
  Show,
  createEffect,
  createSignal,
  untrack,
} from "solid-js";
import { useNavigate } from "@solidjs/router";
import Editor from "./Editor";
import FileTree from "./FileTree";
import SplitEditor from "./SplitEditor";
import {
  setSplitDirection,
  showTabInActivePane,
  splitStore,
  syncSplitPanes,
} from "./editorSplit";
import type { Tab } from "./tabs";
import { focusTab, tabsStore } from "./tabs";

interface Props {
  promptPath?: (
    title: string,
    defaultValue?: string,
    placeholder?: string,
  ) => Promise<string | null>;
  confirmAction?: (title: string, body?: string) => Promise<boolean>;
  onRequestCloseTab?: (tabId: string) => void;
  onNotify?: (message: string, kind?: "info" | "error") => void;
}

const EditorWorkspace: Component<Props> = (props) => {
  const navigate = useNavigate();
  const [sidebarCollapsed, setSidebarCollapsed] = createSignal(false);

  const editorTabs = () =>
    tabsStore.tabs.filter((tab): tab is Extract<Tab, { kind: "editor" }> => {
      return tab.kind === "editor";
    });
  const activeEditorTab = () => {
    const active = tabsStore.active;
    return editorTabs().find((tab) => tab.id === active);
  };
  const canSplit = () => Boolean(activeEditorTab()) && editorTabs().length >= 2;
  const splitActive = () => splitStore.direction !== "none";

  const focusEditorTab = (tab: Extract<Tab, { kind: "editor" }>) => {
    focusTab(tab.id);
    if (splitActive()) showTabInActivePane(tab.id);
    navigate(`/e/${encodeURIComponent(tab.path)}`);
  };

  const setSplit = (direction: "horizontal" | "vertical") => {
    setSplitDirection(direction, editorTabs(), activeEditorTab()?.id ?? null);
  };

  createEffect(() => {
    const tabs = editorTabs();
    const active = activeEditorTab()?.id ?? null;
    untrack(() => syncSplitPanes(tabs, active));
  });

  createEffect(() => {
    const active = activeEditorTab();
    const direction = splitStore.direction;
    if (active && direction !== "none") {
      untrack(() => showTabInActivePane(active.id));
    }
  });

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
              {"<"}
            </button>
          </div>
          <div class="editor-sidebar-content">
            <FileTree
              onOpen={() => undefined}
              promptPath={props.promptPath}
              confirmAction={props.confirmAction}
              onError={(message) => props.onNotify?.(message, "error")}
            />
          </div>
        </aside>
      </Show>

      <Show when={sidebarCollapsed()}>
        <button
          class="editor-sidebar-expand"
          onClick={() => setSidebarCollapsed(false)}
          title="Expand sidebar"
        >
          {">"}
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
                <div
                  class={`editor-tab ${
                    tabsStore.active === tab.id ? "active" : ""
                  }`}
                  title={tab.path}
                >
                  <button
                    type="button"
                    class="editor-tab-main"
                    onClick={() => focusEditorTab(tab)}
                  >
                    <span class="editor-tab-label">{tab.label}</span>
                    <Show when={tab.dirty}>
                      <span class="editor-tab-dirty" title="Unsaved changes">
                        *
                      </span>
                    </Show>
                  </button>
                  <button
                    type="button"
                    class="editor-tab-close"
                    onClick={(event) => {
                      event.stopPropagation();
                      props.onRequestCloseTab?.(tab.id);
                    }}
                    aria-label={`Close ${tab.label}`}
                    title="Close file"
                  >
                    x
                  </button>
                </div>
              )}
            </For>
            <div class="editor-tab-actions">
              <button
                onClick={() => setSplit("vertical")}
                disabled={!canSplit()}
                class={splitStore.direction === "vertical" ? "active" : ""}
                title="Split right"
              >
                ||
              </button>
              <button
                onClick={() => setSplit("horizontal")}
                disabled={!canSplit()}
                class={splitStore.direction === "horizontal" ? "active" : ""}
                title="Split down"
              >
                =
              </button>
              <button
                onClick={() =>
                  setSplitDirection("none", editorTabs(), activeEditorTab()?.id ?? null)
                }
                disabled={!splitActive()}
                title="Unsplit"
              >
                1
              </button>
            </div>
          </div>

          <div class="editor-content">
            <Show
              when={splitActive()}
              fallback={
                <Show when={activeEditorTab()}>
                  {(tab) => <Editor tabId={tab().id} path={tab().path} />}
                </Show>
              }
            >
              <SplitEditor
                tabs={editorTabs()}
                onFocusTab={(tabId) => {
                  const tab = editorTabs().find((candidate) => candidate.id === tabId);
                  if (tab) focusEditorTab(tab);
                }}
                onClosePane={(tabId) => {
                  if (tabsStore.active === tabId) {
                    const next = splitStore.panes
                      .map((pane) => editorTabs().find((tab) => tab.id === pane.tabId))
                      .find(Boolean);
                    if (next) focusEditorTab(next);
                  }
                }}
              />
            </Show>
          </div>
        </Show>
      </main>
    </div>
  );
};

export default EditorWorkspace;
