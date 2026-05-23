import { Component, createSignal, onCleanup, onMount, Show } from "solid-js";
import { api } from "./api";
import { setEditorDirty } from "./tabs";

interface Props {
  tabId: string;
  path: string;
}

// Lazy Monaco import keeps the initial bundle small (~90 KB gz vs ~900 KB
// gz with Monaco eagerly bundled). The first editor tab takes ~200 ms to
// initialise; subsequent tabs are instant.
type MonacoNs = typeof import("monaco-editor");
type StandaloneEditor = import("monaco-editor").editor.IStandaloneCodeEditor;
type TextModel = import("monaco-editor").editor.ITextModel;

let monacoP: Promise<MonacoNs> | null = null;
function loadMonaco(): Promise<MonacoNs> {
  if (!monacoP) {
    monacoP = (async () => {
      // Tell Monaco's web-worker loader to use blob URLs — works without
      // configuring extra worker entry points in Vite.
      (self as unknown as { MonacoEnvironment: object }).MonacoEnvironment = {
        getWorker: () => {
          // Inline noop worker; we don't ship the rich language workers for
          // Phase 3 — basic editing only. TypeScript/JS will still highlight
          // (Monaco does that on the main thread) but no IntelliSense.
          const blob = new Blob(["self.onmessage=()=>{}"], {
            type: "text/javascript",
          });
          return new Worker(URL.createObjectURL(blob));
        },
      };
      return import("monaco-editor");
    })();
  }
  return monacoP;
}

function languageFor(path: string): string {
  const ext = path.split(".").pop()?.toLowerCase() ?? "";
  const map: Record<string, string> = {
    rs: "rust",
    ts: "typescript",
    tsx: "typescript",
    js: "javascript",
    jsx: "javascript",
    py: "python",
    go: "go",
    md: "markdown",
    json: "json",
    yml: "yaml",
    yaml: "yaml",
    toml: "ini", // close enough for highlighting
    sh: "shell",
    bash: "shell",
    html: "html",
    css: "css",
    scss: "scss",
    sql: "sql",
    dockerfile: "dockerfile",
    Dockerfile: "dockerfile",
  };
  return map[ext] ?? "plaintext";
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
    try {
      const [monaco, file] = await Promise.all([
        loadMonaco(),
        api.readFile(props.path),
      ]);
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
      editor = monaco.editor.create(host, {
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
      editor.onDidChangeModelContent(() => {
        if (!editor) return;
        setEditorDirty(props.tabId, editor.getValue() !== savedContent);
      });
      editor.addCommand(
        monaco.KeyMod.CtrlCmd | monaco.KeyCode.KeyS,
        () => void save(),
      );
      resizeObserver = new ResizeObserver(() => editor?.layout());
      resizeObserver.observe(host);
      setStatus("ready");
    } catch (e) {
      setError((e as Error).message);
      setStatus("error");
    }
  });

  onCleanup(() => {
    resizeObserver?.disconnect();
    editor?.dispose();
    model?.dispose();
    editor = null;
    model = null;
  });

  return (
    <div class="editor-shell">
      <div class="editor-toolbar">
        <span class="editor-path">{props.path}</span>
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
          Save (Ctrl+S)
        </button>
      </div>
      <div class="editor-host" ref={host} />
    </div>
  );
};

export default Editor;
