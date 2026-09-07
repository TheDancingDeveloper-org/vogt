import type { editor } from "monaco-editor";

export interface EditorDraft {
  path: string;
  content: string;
  viewState: editor.ICodeEditorViewState | null;
}

const drafts = new Map<string, EditorDraft>();

export function readEditorDraft(tabId: string, path: string): EditorDraft | null {
  const draft = drafts.get(tabId);
  return draft?.path === path ? draft : null;
}

export function rememberEditorDraft(tabId: string, draft: EditorDraft): void {
  drafts.set(tabId, draft);
}

export function discardEditorDraft(tabId: string): void {
  drafts.delete(tabId);
}

export function clearEditorDrafts(): void {
  drafts.clear();
}
