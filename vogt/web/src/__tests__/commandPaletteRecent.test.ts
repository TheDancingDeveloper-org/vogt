// Recent-command persistence: the last few executed command ids survive
// a reload, most-recent first, deduped and capped, under a `vogt.` key.
// Volatile index-addressed ids are never kept.

import { afterEach, describe, expect, it } from "vitest";
import {
  clearRecentCommands,
  readRecentCommandIds,
  recordRecentCommand,
} from "../commandPaletteRecent";

const KEY = "vogt.commandPalette.recent.v1";

afterEach(() => {
  localStorage.clear();
});

describe("recent command persistence", () => {
  it("round-trips the ids most-recent-first through storage", () => {
    recordRecentCommand("vogt-board");
    recordRecentCommand("open-inbox");
    expect(readRecentCommandIds()).toEqual(["open-inbox", "vogt-board"]);
    // A fresh reader (no in-memory state) sees the same, from localStorage.
    expect(JSON.parse(localStorage.getItem(KEY)!)).toEqual(["open-inbox", "vogt-board"]);
  });

  it("moves a repeated id to the front without duplicating it", () => {
    recordRecentCommand("a");
    recordRecentCommand("b");
    recordRecentCommand("a");
    expect(readRecentCommandIds()).toEqual(["a", "b"]);
  });

  it("caps the list at eight", () => {
    for (let i = 0; i < 12; i++) recordRecentCommand(`cmd-${i}`);
    const ids = readRecentCommandIds();
    expect(ids).toHaveLength(8);
    expect(ids[0]).toBe("cmd-11");
    expect(ids).not.toContain("cmd-3");
  });

  it("never records volatile index-addressed rows", () => {
    recordRecentCommand("file-0");
    recordRecentCommand("history-2");
    recordRecentCommand("symbol-1");
    recordRecentCommand("recent-3");
    recordRecentCommand("provider-work items-failed");
    recordRecentCommand("vogt-board");
    expect(readRecentCommandIds()).toEqual(["vogt-board"]);
  });

  it("clears the key", () => {
    recordRecentCommand("vogt-board");
    clearRecentCommands();
    expect(readRecentCommandIds()).toEqual([]);
    expect(localStorage.getItem(KEY)).toBeNull();
  });
});
