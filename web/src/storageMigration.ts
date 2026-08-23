// One-shot rename of browser-storage keys from the historic `mydevenv2.*`
// prefix to `vogt.*` (issue #271, web/mobile half). It runs once at startup —
// before the app reads any preference — and copies each old value to its new
// key so no user is signed out and no preference is lost.
//
// The rename is an EXPLICIT map, not a blind prefix rewrite, so it stays
// auditable and testable: every renamed key is listed here by hand. Keys that
// deliberately keep their historic name are intentionally NOT in this map:
//
//   * `mydevenv2.tabs.v1` — a legacy read-only key that still holds real user
//     data on disk; `tabs.ts` migrates it into the current tab state itself, so
//     it must keep pointing at its historic value (renaming it would orphan
//     that data). See the comment in `tabs.ts`.
//   * `mydevenv2.appTheme.v1`, `mydevenv2.commandPalette.recent.v1`,
//     `mydevenv2.fileTree.sidebarCollapsed.v1` — three keys already renamed to
//     `vogt.*` by the app-theme work (#299) with their own inline legacy-read
//     fallback. Their current key is already `vogt.*`; the historic key is read
//     once by the owning module, so it is not this pass's job.
//   * `mydevenv2-alerts` (Android notification-channel id) and
//     `mydevenv2-terminal-cache` (IndexedDB database name) — neither is a
//     localStorage/sessionStorage key, and both are contracts with layers this
//     web-only rename does not own; changing them would strand native settings
//     or orphan the terminal cache.
//   * `mydevenv2:native-insets` — an event the human-gated Android shell
//     dispatches (#265). The web listener now prefers `vogt:native-insets` but
//     still listens for the historic name so the shipped Android app keeps
//     working until its native half is renamed under #265.
//
// Ephemeral custom-event / BroadcastChannel names need no migration and are
// simply renamed on both dispatch and listen sides.

/**
 * Sentinel recording that the one-shot migration has already run. Guards the
 * whole pass so it costs a single `getItem` on every load after the first.
 */
export const STORAGE_MIGRATION_SENTINEL_KEY = "vogt.identity.migrated.v1";

/**
 * Fixed old→new renames for every statically-named localStorage key. Only the
 * `mydevenv2` prefix changes; version suffixes (`.v1`, `.v2`) are preserved
 * verbatim.
 */
export const STORAGE_KEY_RENAMES: Readonly<Record<string, string>> = {
  "mydevenv2.token": "vogt.token",
  "mydevenv2.base": "vogt.base",
  "mydevenv2.assistant.tts": "vogt.assistant.tts",
  "mydevenv2.inbox.seen.v1": "vogt.inbox.seen.v1",
  "mydevenv2.rail.sections.v1": "vogt.rail.sections.v1",
  "mydevenv2.editor.minimap.v1": "vogt.editor.minimap.v1",
  "mydevenv2.layoutMode.v1": "vogt.layoutMode.v1",
  "mydevenv2.places.v1": "vogt.places.v1",
  "mydevenv2.places.migrated.v1": "vogt.places.migrated.v1",
  "mydevenv2.tabs.v2": "vogt.tabs.v2",
  "mydevenv2.terminalFontSize.v1": "vogt.terminalFontSize.v1",
  "mydevenv2.terminalTheme.v1": "vogt.terminalTheme.v1",
  "mydevenv2.vogtSavedFilters.v1": "vogt.vogtSavedFilters.v1",
  "mydevenv2.backlog.poll.v1": "vogt.backlog.poll.v1",
  "mydevenv2.boardLayout.v1": "vogt.boardLayout.v1",
  "mydevenv2.boardFilters.v1": "vogt.boardFilters.v1",
  "mydevenv2.guiLaunchers": "vogt.guiLaunchers",
  "mydevenv2.historyPins.v1": "vogt.historyPins.v1",
  "mydevenv2.terminalLayouts.v1": "vogt.terminalLayouts.v1",
  "mydevenv2.customTemplates.v1": "vogt.customTemplates.v1",
  "mydevenv2.editorSplit.v1": "vogt.editorSplit.v1",
  "mydevenv2.fileTree.expanded.v1": "vogt.fileTree.expanded.v1",
  "mydevenv2.push.webSubId": "vogt.push.webSubId",
  "mydevenv2.push.nativeSubId": "vogt.push.nativeSubId",
  "mydevenv2.recentFiles.v1": "vogt.recentFiles.v1",
  "mydevenv2.workspaceLayouts.v1": "vogt.workspaceLayouts.v1",
  "mydevenv2.authProfiles.v1": "vogt.authProfiles.v1",
  "mydevenv2.sessionBookmarks.v1": "vogt.sessionBookmarks.v1",
  "mydevenv2.storagePrefs.v1": "vogt.storagePrefs.v1",
};

// Per-pane keys are `mydevenv2.pane.<pane>.<field>.v1` where <pane> is chosen
// at runtime (see `resizablePane.ts`), so they cannot be enumerated by hand.
// They are the sole exception to the explicit-map rule and are migrated by this
// one shared prefix only.
const PANE_PREFIX_OLD = "mydevenv2.pane.";
const PANE_PREFIX_NEW = "vogt.pane.";

/**
 * Run the one-shot storage-key rename. Idempotent and cheap after the first
 * successful pass. Safe to call when localStorage is unavailable (private mode,
 * SSR): it simply does nothing.
 */
export function migrateStorageKeys(): void {
  let store: Storage;
  try {
    store = window.localStorage;
    // eslint-disable-next-line @typescript-eslint/no-unnecessary-condition
    if (!store) return;
  } catch {
    return; // localStorage unavailable
  }

  try {
    if (store.getItem(STORAGE_MIGRATION_SENTINEL_KEY) !== null) return;
  } catch {
    return;
  }

  const move = (oldKey: string, newKey: string) => {
    try {
      // Never clobber a value already present under the new key.
      if (store.getItem(newKey) !== null) return;
      const value = store.getItem(oldKey);
      if (value === null) return;
      store.setItem(newKey, value);
      store.removeItem(oldKey);
    } catch {
      // A quota or security error on one key must not abort the rest.
    }
  };

  for (const [oldKey, newKey] of Object.entries(STORAGE_KEY_RENAMES)) {
    move(oldKey, newKey);
  }

  // Snapshot pane keys before mutating: removing entries mid-iteration of
  // `localStorage.key(i)` reindexes the store and skips keys.
  try {
    const paneKeys: string[] = [];
    for (let i = 0; i < store.length; i += 1) {
      const key = store.key(i);
      if (key && key.startsWith(PANE_PREFIX_OLD)) paneKeys.push(key);
    }
    for (const oldKey of paneKeys) {
      move(oldKey, PANE_PREFIX_NEW + oldKey.slice(PANE_PREFIX_OLD.length));
    }
  } catch {
    // Enumeration unavailable — pane sizes simply reset to their defaults.
  }

  try {
    store.setItem(STORAGE_MIGRATION_SENTINEL_KEY, new Date().toISOString());
  } catch {
    // If the sentinel cannot be written the pass just re-runs next load, which
    // is harmless: every `move` is a no-op once the values have moved.
  }
}
