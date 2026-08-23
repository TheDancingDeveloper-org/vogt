import { describe, it, expect, beforeEach } from "vitest";
import {
  migrateStorageKeys,
  STORAGE_KEY_RENAMES,
  STORAGE_MIGRATION_SENTINEL_KEY,
} from "../storageMigration";

describe("migrateStorageKeys (#271 identity rename)", () => {
  beforeEach(() => {
    localStorage.clear();
  });

  it("moves every renamed mydevenv2.* key to its vogt.* key and removes the old one", () => {
    // Seed each old key with a distinguishable value.
    for (const oldKey of Object.keys(STORAGE_KEY_RENAMES)) {
      localStorage.setItem(oldKey, `value:${oldKey}`);
    }

    migrateStorageKeys();

    for (const [oldKey, newKey] of Object.entries(STORAGE_KEY_RENAMES)) {
      expect(localStorage.getItem(oldKey), `${oldKey} should be gone`).toBeNull();
      expect(localStorage.getItem(newKey), `${newKey} should hold the value`).toBe(
        `value:${oldKey}`,
      );
    }
  });

  it("preserves the auth credential across the rename (no sign-out)", () => {
    localStorage.setItem("mydevenv2.token", "secret-token");
    localStorage.setItem("mydevenv2.base", "https://api.example");

    migrateStorageKeys();

    expect(localStorage.getItem("vogt.token")).toBe("secret-token");
    expect(localStorage.getItem("vogt.base")).toBe("https://api.example");
    expect(localStorage.getItem("mydevenv2.token")).toBeNull();
    expect(localStorage.getItem("mydevenv2.base")).toBeNull();
  });

  it("migrates dynamic per-pane keys by their shared prefix", () => {
    localStorage.setItem("mydevenv2.pane.places.width.v1", "320");
    localStorage.setItem("mydevenv2.pane.sessions.collapsed.v1", "1");

    migrateStorageKeys();

    expect(localStorage.getItem("vogt.pane.places.width.v1")).toBe("320");
    expect(localStorage.getItem("vogt.pane.sessions.collapsed.v1")).toBe("1");
    expect(localStorage.getItem("mydevenv2.pane.places.width.v1")).toBeNull();
    expect(localStorage.getItem("mydevenv2.pane.sessions.collapsed.v1")).toBeNull();
  });

  it("never clobbers a value already present under the new key", () => {
    localStorage.setItem("mydevenv2.token", "old");
    localStorage.setItem("vogt.token", "new");

    migrateStorageKeys();

    expect(localStorage.getItem("vogt.token")).toBe("new");
    // The old key is left untouched when the new key already exists.
    expect(localStorage.getItem("mydevenv2.token")).toBe("old");
  });

  it("leaves intentionally-kept historic keys alone", () => {
    // Legacy tab data, notification-channel-style ids and the like are not in
    // the rename map and must survive untouched.
    localStorage.setItem("mydevenv2.tabs.v1", "legacy-tabs");

    migrateStorageKeys();

    expect(localStorage.getItem("mydevenv2.tabs.v1")).toBe("legacy-tabs");
    expect(localStorage.getItem("vogt.tabs.v1")).toBeNull();
  });

  it("sets a sentinel and does not run a second time", () => {
    localStorage.setItem("mydevenv2.token", "first");

    migrateStorageKeys();
    expect(localStorage.getItem(STORAGE_MIGRATION_SENTINEL_KEY)).not.toBeNull();
    expect(localStorage.getItem("vogt.token")).toBe("first");

    // Simulate an old key reappearing (e.g. another tab wrote it): a second
    // run must be a no-op because the sentinel is already set.
    localStorage.setItem("mydevenv2.token", "second");
    migrateStorageKeys();

    expect(localStorage.getItem("mydevenv2.token")).toBe("second");
    expect(localStorage.getItem("vogt.token")).toBe("first");
  });

  it("is a cheap no-op on a clean (already-migrated) store", () => {
    migrateStorageKeys();
    const sentinel = localStorage.getItem(STORAGE_MIGRATION_SENTINEL_KEY);
    expect(sentinel).not.toBeNull();

    // Running again should not change the sentinel or add any keys.
    const before = localStorage.length;
    migrateStorageKeys();
    expect(localStorage.length).toBe(before);
    expect(localStorage.getItem(STORAGE_MIGRATION_SENTINEL_KEY)).toBe(sentinel);
  });
});
