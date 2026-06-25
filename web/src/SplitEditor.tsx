import { Component, For, Show } from "solid-js";
import { splitStore, removePane, setActivePane, resizePanes } from "./editorSplit";
import Editor from "./Editor";

interface Props {
  onError?: (message: string) => void;
}

const SplitEditor: Component<Props> = (_props) => {
  let containerRef: HTMLDivElement | undefined;

  const handleSplitterDrag = (index: number, e: MouseEvent) => {
    e.preventDefault();
    const container = containerRef;
    if (!container) return;

    const startPos =
      splitStore.direction === "horizontal" ? e.clientY : e.clientX;
    const rect = container.getBoundingClientRect();
    const totalSize =
      splitStore.direction === "horizontal" ? rect.height : rect.width;

    const startSizes = splitStore.panes.map((p) => p.size);

    const onMove = (moveEvent: MouseEvent) => {
      const currentPos =
        splitStore.direction === "horizontal"
          ? moveEvent.clientY
          : moveEvent.clientX;
      const delta = currentPos - startPos;
      const deltaPercent = (delta / totalSize) * 100;

      // Resize the pane before and after the splitter
      const newSizes = [...startSizes];
      newSizes[index] = Math.max(10, Math.min(90, startSizes[index]! + deltaPercent));
      newSizes[index + 1] = Math.max(
        10,
        Math.min(90, startSizes[index + 1]! - deltaPercent),
      );

      resizePanes(newSizes);
    };

    const onUp = () => {
      document.removeEventListener("mousemove", onMove);
      document.removeEventListener("mouseup", onUp);
    };

    document.addEventListener("mousemove", onMove);
    document.addEventListener("mouseup", onUp);
  };

  return (
    <div
      ref={containerRef}
      class={`split-editor split-${splitStore.direction}`}
    >
      <For each={splitStore.panes}>
        {(pane, index) => (
          <>
            <div
              class={`split-pane ${
                splitStore.activePane === pane.id ? "active" : ""
              }`}
              style={{ [splitStore.direction === "horizontal" ? "height" : "width"]: `${pane.size}%` }}
              onClick={() => setActivePane(pane.id)}
            >
              <div class="split-pane-header">
                <span class="split-pane-path">{pane.path}</span>
                <button
                  class="split-pane-close"
                  onClick={(e) => {
                    e.stopPropagation();
                    removePane(pane.id);
                  }}
                  title="Close pane"
                >
                  ×
                </button>
              </div>
              <div class="split-pane-editor">
                <Editor tabId={pane.id} path={pane.path} />
              </div>
            </div>
            <Show when={index() < splitStore.panes.length - 1}>
              <div
                class="split-handle"
                onMouseDown={(e) => handleSplitterDrag(index(), e)}
              />
            </Show>
          </>
        )}
      </For>
    </div>
  );
};

export default SplitEditor;
