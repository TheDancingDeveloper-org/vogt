// Keep the editor core separate from the language contributions. Importing the
// package root also registers every bundled language service, even though most
// files opened in Vogt only need a tokenizer.
type MonacoNs = typeof import("monaco-editor/esm/vs/editor/editor.api");
import { activeMonacoTheme, APP_THEME_CHANGE_EVENT } from "./appThemes";

export type MonacoNamespace = MonacoNs;
export type StandaloneEditor = import("monaco-editor").editor.IStandaloneCodeEditor;
export type DiffEditor = import("monaco-editor").editor.IStandaloneDiffEditor;
export type TextModel = import("monaco-editor").editor.ITextModel;
export type DocumentSymbol = import("monaco-editor").languages.DocumentSymbol;
export type EditorRange = import("monaco-editor").IRange;

let monacoP: Promise<MonacoNs> | null = null;

type WorkerConstructor = new () => Worker;
type WorkerLoader = () => Promise<{ default: WorkerConstructor }>;

const workerLoaders: Record<string, WorkerLoader> = {
  editor: () => import("monaco-editor/esm/vs/editor/editor.worker?worker"),
  json: () => import("monaco-editor/esm/vs/language/json/json.worker?worker"),
  css: () => import("monaco-editor/esm/vs/language/css/css.worker?worker"),
  html: () => import("monaco-editor/esm/vs/language/html/html.worker?worker"),
  typescript: () =>
    import("monaco-editor/esm/vs/language/typescript/ts.worker?worker"),
};
const workerConstructors = new Map<string, Promise<WorkerConstructor>>();

function getWorkerConstructor(kind: string): Promise<WorkerConstructor> {
  let constructor = workerConstructors.get(kind);
  if (!constructor) {
    const load: WorkerLoader = workerLoaders[kind] ?? workerLoaders.editor!;
    constructor = load().then((module) => module.default);
    workerConstructors.set(kind, constructor);
  }
  return constructor;
}

function getWorker(_: string, label: string): Promise<Worker> {
  let kind = "editor";
  if (label === "json") kind = "json";
  else if (["css", "scss", "less"].includes(label)) kind = "css";
  else if (["html", "handlebars", "razor"].includes(label)) kind = "html";
  else if (["typescript", "javascript"].includes(label)) kind = "typescript";
  return getWorkerConstructor(kind).then((WorkerCtor) => new WorkerCtor());
}

export function loadMonaco(): Promise<MonacoNs> {
  if (!monacoP) {
    monacoP = (async () => {
      (self as unknown as { MonacoEnvironment?: object }).MonacoEnvironment ??= {
        getWorker,
      };
      const editor = await import("monaco-editor/esm/vs/editor/editor.api");
      performance.mark("monaco:core");
      return editor;
    })();
  }
  return monacoP;
}

type LanguageLoader = () => Promise<unknown>;

// These imports are intentionally explicit. A glob/imported package root
// would put every tokenizer and language service back on the editor path.
// Keep this table aligned with languageFor; unknown languages deliberately
// remain plain text and do not load a contribution.
const languageLoaders: Partial<Record<string, LanguageLoader>> = {
  typescript: () =>
    import("monaco-editor/esm/vs/language/typescript/monaco.contribution"),
  javascript: () =>
    import("monaco-editor/esm/vs/language/typescript/monaco.contribution"),
  json: () => import("monaco-editor/esm/vs/language/json/monaco.contribution"),
  css: () => import("monaco-editor/esm/vs/language/css/monaco.contribution"),
  scss: () => import("monaco-editor/esm/vs/language/css/monaco.contribution"),
  less: () => import("monaco-editor/esm/vs/language/css/monaco.contribution"),
  html: () => import("monaco-editor/esm/vs/language/html/monaco.contribution"),
  rust: () => import("monaco-editor/esm/vs/basic-languages/rust/rust.contribution"),
  python: () => import("monaco-editor/esm/vs/basic-languages/python/python.contribution"),
  go: () => import("monaco-editor/esm/vs/basic-languages/go/go.contribution"),
  markdown: () =>
    import("monaco-editor/esm/vs/basic-languages/markdown/markdown.contribution"),
  yaml: () => import("monaco-editor/esm/vs/basic-languages/yaml/yaml.contribution"),
  ini: () => import("monaco-editor/esm/vs/basic-languages/ini/ini.contribution"),
  shell: () => import("monaco-editor/esm/vs/basic-languages/shell/shell.contribution"),
  sql: () => import("monaco-editor/esm/vs/basic-languages/sql/sql.contribution"),
  dockerfile: () =>
    import("monaco-editor/esm/vs/basic-languages/dockerfile/dockerfile.contribution"),
};
const languagePromises = new Map<string, Promise<void>>();

export function loadLanguage(language: string): Promise<void> {
  const loader = languageLoaders[language];
  if (!loader) return Promise.resolve();
  let loaded = languagePromises.get(language);
  if (!loaded) {
    loaded = loadMonaco().then(() => loader()).then(() => {
      performance.mark(`monaco:lang:${language}`);
    });
    languagePromises.set(language, loaded);
  }
  return loaded;
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
