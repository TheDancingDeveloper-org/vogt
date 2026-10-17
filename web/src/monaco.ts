type MonacoNs = typeof import("monaco-editor");
import { activeMonacoTheme, APP_THEME_CHANGE_EVENT } from "./appThemes";
import EditorWorker from "monaco-editor/esm/vs/editor/editor.worker?worker";
import JsonWorker from "monaco-editor/esm/vs/language/json/json.worker?worker";
import CssWorker from "monaco-editor/esm/vs/language/css/css.worker?worker";
import HtmlWorker from "monaco-editor/esm/vs/language/html/html.worker?worker";
import TsWorker from "monaco-editor/esm/vs/language/typescript/ts.worker?worker";

export type MonacoNamespace = MonacoNs;
export type StandaloneEditor = import("monaco-editor").editor.IStandaloneCodeEditor;
export type DiffEditor = import("monaco-editor").editor.IStandaloneDiffEditor;
export type TextModel = import("monaco-editor").editor.ITextModel;
export type DocumentSymbol = import("monaco-editor").languages.DocumentSymbol;
export type EditorRange = import("monaco-editor").IRange;

let monacoP: Promise<MonacoNs> | null = null;

function getWorker(_: string, label: string): Worker {
  switch (label) {
    case "json":
      return new JsonWorker();
    case "css":
    case "scss":
    case "less":
      return new CssWorker();
    case "html":
    case "handlebars":
    case "razor":
      return new HtmlWorker();
    case "typescript":
    case "javascript":
      return new TsWorker();
    default:
      return new EditorWorker();
  }
}

export function loadMonaco(): Promise<MonacoNs> {
  if (!monacoP) {
    monacoP = (async () => {
      (self as unknown as { MonacoEnvironment?: object }).MonacoEnvironment ??= {
        getWorker,
      };
      return import("monaco-editor");
    })();
  }
  return monacoP;
}

let monacoThemeBound = false;

/** The built-in Monaco theme that matches the active shell theme (#299). */
export function monacoThemeForApp(): string {
  return activeMonacoTheme();
}

/**
 * Apply the app-matched Monaco theme now and, once per session, keep it in sync
 * with the shell theme. Monaco's theme is global to all editors, so a single
 * listener covers every mounted editor and diff view.
 */
export function syncMonacoTheme(monaco: MonacoNs): void {
  monaco.editor.setTheme(activeMonacoTheme());
  if (monacoThemeBound || typeof window === "undefined") return;
  monacoThemeBound = true;
  window.addEventListener(APP_THEME_CHANGE_EVENT, () => {
    monaco.editor.setTheme(activeMonacoTheme());
  });
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
