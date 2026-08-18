/** Ephemeral per-window state for Sessions tools that intentionally unmount. */
const toolDrafts = new Map<string, unknown>();

export function readToolDraft<T>(key: string, fallback: T): T {
  return (toolDrafts.get(key) as T | undefined) ?? fallback;
}

export function writeToolDraft<T>(key: string, value: T): void {
  toolDrafts.set(key, value);
}

export function clearToolDrafts(): void {
  toolDrafts.clear();
}
