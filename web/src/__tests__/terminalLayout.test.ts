import { describe, expect, it, vi } from "vitest";
import {
  collectPanes,
  commitCreatedPane,
  insertPane,
  makePane,
  normalizeTerminalLayout,
  paneIdFor,
  pruneTerminalLayout,
  removePane,
  type TerminalLayoutNode,
} from "../terminalLayout";

describe("terminal layout transactions", () => {
  it("atomically changes a pane to a split and supports nested insertion", () => {
    const first = makePane("one");
    const split = insertPane(first, first.id, "row", makePane("two"));
    expect(split?.type).toBe("split");

    const nested = insertPane(split!, paneIdFor("two"), "column", makePane("three"));
    expect(collectPanes(nested!).map((pane) => pane.sessionId))
      .toEqual(["one", "two", "three"]);
  });

  it("returns a failed insertion instead of silently retaining an orphan", () => {
    expect(insertPane(makePane("one"), "pane:gone", "row", makePane("two")))
      .toBeNull();
  });

  it("rolls back a created session if insertion fails", async () => {
    const rollback = vi.fn(async () => undefined);
    await expect(commitCreatedPane("two", () => false, rollback))
      .rejects.toThrow("target pane changed");
    expect(rollback).toHaveBeenCalledWith("two");
  });

  it("preserves nested structure through persistence, close and pruning", () => {
    const stored: TerminalLayoutNode = {
      type: "split",
      id: "split:outer",
      direction: "row",
      children: [
        makePane("one"),
        {
          type: "split",
          id: "split:inner",
          direction: "column",
          children: [makePane("two"), makePane("three")],
        },
      ],
    };
    const reloaded = normalizeTerminalLayout(JSON.parse(JSON.stringify(stored)));
    expect(collectPanes(reloaded!).map((pane) => pane.sessionId))
      .toEqual(["one", "two", "three"]);

    const closed = removePane(reloaded!, paneIdFor("two"));
    expect(collectPanes(closed!).map((pane) => pane.sessionId))
      .toEqual(["one", "three"]);

    const pruned = pruneTerminalLayout(reloaded!, (id) => id !== "three");
    expect(collectPanes(pruned!).map((pane) => pane.sessionId))
      .toEqual(["one", "two"]);
  });
});
