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

  // Pointer events, not mouse events, so a touch drag resizes the split too
  // (#240). The handle captures the pointer and listens on `document`, so a
  // finger that slides off the thin handle keeps driving the resize.
  const handleSplitterDrag = (index: number, e: PointerEvent) => {
    e.preventDefault();
    const container = containerRef;
    if (!container) return;
    const handle = e.currentTarget as HTMLElement | null;

    const startPos =
      splitStore.direction === "horizontal" ? e.clientY : e.clientX;
    const rect = container.getBoundingClientRect();
    const totalSize =
      splitStore.direction === "horizontal" ? rect.height : rect.width;
    if (totalSize <= 0) return;
    const startPanes = splitStore.panes.map((pane) => ({ ...pane }));

    let pendingFrame: number | null = null;
    let pendingPos = startPos;

    const applyResize = () => {
      pendingFrame = null;
      const deltaPercent = ((pendingPos - startPos) / totalSize) * 100;
      resizePanePair(index, deltaPercent, startPanes);
    };

    const onMove = (moveEvent: PointerEvent) => {
      pendingPos =
        splitStore.direction === "horizontal"
          ? moveEvent.clientY
          : moveEvent.clientX;
      if (pendingFrame === null) {
        pendingFrame = requestAnimationFrame(applyResize);
      }
    };

    const onUp = () => {
      if (pendingFrame !== null) {
        cancelAnimationFrame(pendingFrame);
        pendingFrame = null;
      }
      document.removeEventListener("pointermove", onMove);
      document.removeEventListener("pointerup", onUp);
      document.removeEventListener("pointercancel", onUp);
      try {
        handle?.releasePointerCapture?.(e.pointerId);
      } catch {
        // No active capture (e.g. a synthetic event) — nothing to release.
      }
      document.body.classList.remove("is-resizing-split");
      cleanupDrag = undefined;
    };

    cleanupDrag?.();
    cleanupDrag = onUp;
    try {
      handle?.setPointerCapture?.(e.pointerId);
    } catch {
      // Capture is a nicety; the document listeners drive the resize regardless.
    }
    document.body.classList.add("is-resizing-split");
    document.addEventListener("pointermove", onMove);
    document.addEventListener("pointerup", onUp);
    document.addEventListener("pointercancel", onUp);
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
                onPointerDown={(event) => handleSplitterDrag(index(), event)}
              />
            </Show>
          </>
        )}
      </For>
    </div>
  );
};

export default SplitEditor;
