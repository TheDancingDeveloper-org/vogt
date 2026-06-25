import { Component, createSignal, onCleanup, onMount, Show, For } from "solid-js";
import { api } from "./api";
import {
  languageFor,
  loadMonaco,
  type StandaloneEditor,
  type TextModel,
} from "./monaco";
import { setEditorDirty } from "./tabs";

interface Props {
  tabId: string;
  path: string;
}

const Editor: Component<Props> = (props) => {
  let host: HTMLDivElement | undefined;
  let editor: StandaloneEditor | null = null;
  let model: TextModel | null = null;
  const [status, setStatus] = createSignal<"loading" | "ready" | "saving" | "error">(
    "loading",
  );
  const [error, setError] = createSignal<string | null>(null);
  const [savedAt, setSavedAt] = createSignal<number | null>(null);
  let resizeObserver: ResizeObserver | null = null;
  let savedContent = "";
  let disposed = false;
  let contentChangeDisposable: { dispose: () => void } | null = null;

  const save = async () => {
    if (!editor) return;
    const content = editor.getValue();
    setStatus("saving");
    try {
      await api.writeFile(props.path, content);
      savedContent = content;
      setEditorDirty(props.tabId, false);
      setSavedAt(Date.now());
      setStatus("ready");
      setError(null);
    } catch (e) {
      setError((e as Error).message);
      setStatus("error");
    }
  };

  onMount(async () => {
    if (!host) return;
    const mountedHost = host;
    try {
      const [monaco, file] = await Promise.all([
        loadMonaco(),
        api.readFile(props.path),
      ]);
      if (disposed) return;
      if (file.is_binary) {
        setError("binary file (cannot edit)");
        setStatus("error");
        return;
      }
      savedContent = file.content ?? "";
      model = monaco.editor.createModel(
        savedContent,
        languageFor(props.path),
        monaco.Uri.parse(`inmemory://workspace/${props.path}`),
      );
      if (disposed) {
        model.dispose();
        model = null;
        return;
      }
      editor = monaco.editor.create(mountedHost, {
        model,
        theme: "vs-dark",
        automaticLayout: false,
        fontFamily:
          '"JetBrainsMono Nerd Font", "JetBrains Mono", ui-monospace, monospace',
        fontSize: 13,
        minimap: { enabled: false },
        wordWrap: "on",
        scrollBeyondLastLine: false,
        renderWhitespace: "selection",
      });
      contentChangeDisposable = editor.onDidChangeModelContent(() => {
        if (!editor) return;
        setEditorDirty(props.tabId, editor.getValue() !== savedContent);
      });
      editor.addCommand(
        monaco.KeyMod.CtrlCmd | monaco.KeyCode.KeyS,
        () => void save(),
      );
      resizeObserver = new ResizeObserver(() => editor?.layout());
      resizeObserver.observe(mountedHost);
      setStatus("ready");
    } catch (e) {
      if (disposed) return;
      setError((e as Error).message);
      setStatus("error");
    }
  });

  onCleanup(() => {
    disposed = true;
    resizeObserver?.disconnect();
    contentChangeDisposable?.dispose();
    editor?.dispose();
    model?.dispose();
    resizeObserver = null;
    contentChangeDisposable = null;
    editor = null;
    model = null;
  });

  return (
    <div class="editor-shell">
      <div class="editor-toolbar">
        <div class="editor-breadcrumb">
          <For each={props.path.split("/").filter(Boolean)}>
            {(segment, idx) => (
              <>
                <Show when={idx() > 0}>
                  <span class="breadcrumb-separator">/</span>
                </Show>
                <span class="breadcrumb-segment">{segment}</span>
              </>
            )}
          </For>
        </div>
        <span class="editor-status">
          <Show when={status() === "loading"}>loading…</Show>
          <Show when={status() === "saving"}>saving…</Show>
          <Show when={status() === "error"}>
            <span style={{ color: "#ff7b72" }}>{error()}</span>
          </Show>
          <Show when={status() === "ready" && savedAt()}>
            <span style={{ color: "var(--fg-muted)" }}>
              saved {new Date(savedAt()!).toLocaleTimeString()}
            </span>
          </Show>
        </span>
        <button onClick={save} disabled={status() === "loading"}>
          Save
        </button>
      </div>
      <div class="editor-host" ref={host} />
    </div>
  );
};

export default Editor;
