type MonacoNs = typeof import("monaco-editor");

export type MonacoNamespace = MonacoNs;
export type StandaloneEditor = import("monaco-editor").editor.IStandaloneCodeEditor;
export type DiffEditor = import("monaco-editor").editor.IStandaloneDiffEditor;
export type TextModel = import("monaco-editor").editor.ITextModel;

let monacoP: Promise<MonacoNs> | null = null;
let noopWorkerUrl: string | null = null;

function noopWorker(): Worker {
  if (!noopWorkerUrl) {
    noopWorkerUrl = URL.createObjectURL(
      new Blob(["self.onmessage=()=>{}"], { type: "text/javascript" }),
    );
  }
  return new Worker(noopWorkerUrl);
}

export function loadMonaco(): Promise<MonacoNs> {
  if (!monacoP) {
    monacoP = (async () => {
      (self as unknown as { MonacoEnvironment?: object }).MonacoEnvironment ??= {
        getWorker: noopWorker,
      };
      return import("monaco-editor");
    })();
  }
  return monacoP;
}

export function languageFor(path: string): string {
  const name = path.split("/").pop()?.toLowerCase() ?? "";
  if (name === "dockerfile" || name.endsWith(".dockerfile")) return "dockerfile";

  const ext = name.split(".").pop() ?? "";
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
    toml: "ini",
    sh: "shell",
    bash: "shell",
    html: "html",
    css: "css",
    scss: "scss",
    sql: "sql",
  };
  return map[ext] ?? "plaintext";
}
