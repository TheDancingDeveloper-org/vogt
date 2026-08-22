import type {
  DocumentSymbol,
  EditorRange,
  StandaloneEditor,
  TextModel,
} from "./monaco";

interface EditorRegistration {
  path: string;
  getEditor: () => StandaloneEditor | null;
  getModel: () => TextModel | null;
  /** Persist the editor's current buffer. Registered so a Ctrl/Cmd+S that
   *  lands outside Monaco (the tab bar, the file tree, a split header) can
   *  still save the active editor instead of falling through to the browser's
   *  Save-Page dialog (#237). */
  save?: () => void | Promise<void>;
}

export interface EditorSymbolResult {
  name: string;
  detail: string;
  containerName?: string;
  path: string;
  line: number;
  range: EditorRange;
}

const editors = new Map<string, EditorRegistration>();

function flattenSymbols(
  path: string,
  symbols: readonly DocumentSymbol[],
  parents: string[] = [],
): EditorSymbolResult[] {
  const results: EditorSymbolResult[] = [];
  for (const symbol of symbols) {
    const trail = [...parents, symbol.name];
    results.push({
      name: symbol.name,
      detail: symbol.detail,
      containerName: parents.length > 0 ? parents.join(" > ") : undefined,
      path,
      line: symbol.selectionRange.startLineNumber,
      range: symbol.selectionRange,
    });
    if (symbol.children?.length) {
      results.push(...flattenSymbols(path, symbol.children, trail));
    }
  }
  return results;
}

async function loadCommandService() {
  const [{ StandaloneServices }, { ICommandService }] = await Promise.all([
    import("monaco-editor/esm/vs/editor/standalone/browser/standaloneServices.js"),
    import("monaco-editor/esm/vs/platform/commands/common/commands.js"),
  ]);
  return StandaloneServices.get(ICommandService) as {
    executeCommand(id: string, ...args: unknown[]): Promise<unknown>;
  };
}

export function registerEditor(tabId: string, registration: EditorRegistration): () => void {
  editors.set(tabId, registration);
  return () => {
    if (editors.get(tabId) === registration) {
      editors.delete(tabId);
    }
  };
}

export function hasRegisteredEditor(tabId: string): boolean {
  return editors.has(tabId);
}

/**
 * Save the editor registered for `tabId`, if any. Returns whether a save was
 * dispatched — the caller uses the result to decide whether it still needs to
 * let the keystroke through (it never does when we saved).
 */
export function saveRegisteredEditor(tabId: string): boolean {
  const save = editors.get(tabId)?.save;
  if (!save) return false;
  void save();
  return true;
}

export async function listEditorSymbols(tabId: string): Promise<EditorSymbolResult[]> {
  const registration = editors.get(tabId);
  const model = registration?.getModel();
  if (!registration || !model) return [];

  const commandService = await loadCommandService();
  const raw = await commandService.executeCommand(
    "_executeDocumentSymbolProvider",
    model.uri,
  );
  const symbols = Array.isArray(raw) ? (raw as DocumentSymbol[]) : [];
  return flattenSymbols(registration.path, symbols);
}

export function focusEditorRange(tabId: string, range: EditorRange): boolean {
  const editor = editors.get(tabId)?.getEditor();
  if (!editor) return false;
  editor.setSelection(range);
  editor.revealPositionInCenter({
    lineNumber: range.startLineNumber,
    column: range.startColumn,
  });
  editor.focus();
  return true;
}

