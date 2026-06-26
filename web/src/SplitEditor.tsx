import { Component, For, Show, onCleanup } from "solid-js";
import {
  removePane,
  resizePanePair,
  setActivePane,
  splitStore,
} from "./editorSplit";
import Editor from "./Editor";
import type { Tab } from "./tabs";

interface Props {
  tabs: Extract<Tab, { kind: "editor" }>[];
  onFocusTab: (tabId: string) => void;
  onClosePane: (tabId: string) => void;
}

const SplitEditor: Component<Props> = (props) => {
  let containerRef: HTMLDivElement | undefined;
  let cleanupDrag: (() => void) | undefined;

  const tabForPane = (tabId: string) => props.tabs.find((tab) => tab.id === tabId);

  const focusPane = (tabId: string) => {
    setActivePane(tabId);
    props.onFocusTab(tabId);
  };

  const handleSplitterDrag = (index: number, e: MouseEvent) => {
    e.preventDefault();
    const container = containerRef;
    if (!container) return;

    const startPos =
      splitStore.direction === "horizontal" ? e.clientY : e.clientX;
    const rect = container.getBoundingClientRect();
    const totalSize =
      splitStore.direction === "horizontal" ? rect.height : rect.width;
    if (totalSize <= 0) return;
    const startPanes = splitStore.panes.map((pane) => ({ ...pane }));

    const onMove = (moveEvent: MouseEvent) => {
      const currentPos =
        splitStore.direction === "horizontal"
          ? moveEvent.clientY
          : moveEvent.clientX;
      const deltaPercent = ((currentPos - startPos) / totalSize) * 100;
      resizePanePair(index, deltaPercent, startPanes);
    };

    const onUp = () => {
      document.removeEventListener("mousemove", onMove);
      document.removeEventListener("mouseup", onUp);
      document.body.classList.remove("is-resizing-split");
      cleanupDrag = undefined;
    };

    cleanupDrag?.();
    cleanupDrag = onUp;
    document.body.classList.add("is-resizing-split");
    document.addEventListener("mousemove", onMove);
    document.addEventListener("mouseup", onUp);
  };

  onCleanup(() => cleanupDrag?.());

  return (
    <div ref={containerRef} class={`split-editor split-${splitStore.direction}`}>
      <For each={splitStore.panes}>
        {(pane, index) => (
          <>
            <Show when={tabForPane(pane.tabId)}>
              {(tab) => (
                <div
                  class={`split-pane ${
                    splitStore.activePane === pane.tabId ? "active" : ""
                  }`}
                  style={{
                    [splitStore.direction === "horizontal" ? "height" : "width"]:
                      `${pane.size}%`,
                  }}
                  onPointerDown={() => focusPane(pane.tabId)}
                >
                  <div class="split-pane-header">
                    <span class="split-pane-path">{tab().path}</span>
                    <button
                      class="split-pane-close"
                      onClick={(e) => {
                        e.stopPropagation();
                        removePane(pane.tabId);
                        props.onClosePane(pane.tabId);
                      }}
                      title="Remove pane"
                    >
                      x
                    </button>
                  </div>
                  <div class="split-pane-editor">
                    <Editor tabId={tab().id} path={tab().path} />
                  </div>
                </div>
              )}
            </Show>
            <Show when={index() < splitStore.panes.length - 1}>
              <div
                class="split-handle"
                onMouseDown={(event) => handleSplitterDrag(index(), event)}
              />
            </Show>
          </>
        )}
      </For>
    </div>
  );
};

export default SplitEditor;
