import { describe, expect, it, vi } from "vitest";
import {
  collectPanes,
  commitCreatedPane,
  containsSession,
  findPane,
  findPaneBySession,
  insertPane,
  makePane,
  normalizeTerminalLayout,
  paneIdFor,
  pruneTerminalLayout,
  removePane,
  retargetPane,
  type SavedTerminalLayout,
  type TerminalLayoutNode,
} from "../terminalLayout";

describe("terminal layout transactions", () => {
  it("atomically changes a pane to a split and supports nested insertion", () => {
    const first = makePane("one");
    const split = insertPane(first, first.id, "row", makePane("two"));
    expect(split?.type).toBe("split");

    const two = findPaneBySession(split!, "two")!;
    const nested = insertPane(split!, two.id, "column", makePane("three"));
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

    const two = findPaneBySession(reloaded!, "two")!;
    const closed = removePane(reloaded!, two.id);
    expect(collectPanes(closed!).map((pane) => pane.sessionId))
      .toEqual(["one", "three"]);

    const pruned = pruneTerminalLayout(reloaded!, (id) => id !== "three");
    expect(collectPanes(pruned!).map((pane) => pane.sessionId))
      .toEqual(["one", "two"]);
  });
});

// #212: composing existing sessions into a split, re-targeting a pane and
// detaching one are all pure tree transforms — no session is ever created.
describe("composing existing sessions (#212)", () => {
  it("inserts an existing session as a new pane without spawning one", () => {
    const root = makePane("one");
    const split = insertPane(root, root.id, "row", makePane("two"));
    expect(collectPanes(split!).map((pane) => pane.sessionId))
      .toEqual(["one", "two"]);
    // Every pane is bound to a session the caller already had — nothing here
    // manufactures a new one. Pane ids are independent of the session (#600)
    // but still unique across the tree.
    const ids = collectPanes(split!).map((pane) => pane.id);
    expect(new Set(ids).size).toBe(ids.length);
  });

  it("re-targets a pane at a session not already shown", () => {
    const one = makePane("one");
    const root = insertPane(one, one.id, "row", makePane("two"))!;
    const paneTwoId = findPaneBySession(root, "two")!.id;
    const result = retargetPane(root, paneTwoId, "three");
    expect(result).not.toBeNull();
    expect(collectPanes(result!.root).map((pane) => pane.sessionId))
      .toEqual(["one", "three"]);
    // The pane keeps its id and only the session it shows changes; the layout
    // is otherwise intact and the retargeted pane is focused (#600).
    expect(result!.activePaneId).toBe(paneTwoId);
    expect(findPane(result!.root, paneTwoId)?.sessionId).toBe("three");
    expect(containsSession(result!.root, "two")).toBe(false);
  });

  it("swaps two panes when the target session is already on screen", () => {
    const one = makePane("one");
    const root = insertPane(one, one.id, "row", makePane("two"))!;
    const paneOneId = findPaneBySession(root, "one")!.id;
    const paneTwoId = findPaneBySession(root, "two")!.id;
    // Point the first pane at "two", which the second pane already shows.
    const result = retargetPane(root, paneOneId, "two");
    expect(collectPanes(result!.root).map((pane) => pane.sessionId))
      .toEqual(["two", "one"]);
    // A swap, not a duplication: both sessions survive, exactly once each.
    expect(collectPanes(result!.root)).toHaveLength(2);
    // Both panes keep their ids; only the sessions they show swap. The target
    // pane (which now shows "two") is focused.
    expect(result!.activePaneId).toBe(paneOneId);
    expect(findPane(result!.root, paneOneId)?.sessionId).toBe("two");
    expect(findPane(result!.root, paneTwoId)?.sessionId).toBe("one");
  });

  it("re-targeting leaves every other pane's id untouched (#600)", () => {
    // Three panes: retargeting the middle one must not disturb the ids of its
    // neighbours, so the renderer can leave those panes mounted.
    const a = makePane("a");
    const ab = insertPane(a, a.id, "row", makePane("b"))!;
    const root = insertPane(ab, findPaneBySession(ab, "b")!.id, "row", makePane("c"))!;
    const before = collectPanes(root).map((p) => ({ id: p.id, s: p.sessionId }));
    const paneBId = findPaneBySession(root, "b")!.id;
    const result = retargetPane(root, paneBId, "d");
    const after = collectPanes(result!.root);
    // The two untouched panes keep both id and session; only "b"→"d" changed.
    for (const pane of before) {
      if (pane.s === "b") continue;
      const match = after.find((p) => p.id === pane.id);
      expect(match?.sessionId).toBe(pane.s);
    }
    expect(findPane(result!.root, paneBId)?.sessionId).toBe("d");
  });

  it("re-targeting a missing pane changes nothing", () => {
    const root = makePane("one");
    expect(retargetPane(root, "pane:gone", "two")).toBeNull();
  });

  it("detaches a pane by dropping it from the tree, leaving its session alone", () => {
    const one = makePane("one");
    const root = insertPane(one, one.id, "row", makePane("two"))!;
    const paneTwoId = findPaneBySession(root, "two")!.id;
    const detached = removePane(root, paneTwoId);
    expect(collectPanes(detached!).map((pane) => pane.sessionId)).toEqual(["one"]);
    // `removePane` is a layout function only: it names no session-kill verb, so
    // detaching cannot reach the kill/DELETE path.
    expect(findPaneBySession(detached!, "two")).toBeNull();
  });

  it("loads a saved layout whose ids predate #600 (pane:<sessionId>)", () => {
    // Layouts persisted before #600 carry session-derived ids. They must still
    // load, keep their bindings and be findable by both id and session.
    const legacy: TerminalLayoutNode = {
      type: "split",
      id: "split:legacy",
      direction: "row",
      children: [
        { type: "pane", id: paneIdFor("alpha"), sessionId: "alpha" },
        { type: "pane", id: paneIdFor("beta"), sessionId: "beta" },
      ],
    };
    const root = normalizeTerminalLayout(JSON.parse(JSON.stringify(legacy)))!;
    expect(collectPanes(root).map((pane) => pane.sessionId)).toEqual(["alpha", "beta"]);
    expect(findPane(root, paneIdFor("beta"))?.sessionId).toBe("beta");
    expect(findPaneBySession(root, "beta")?.id).toBe(paneIdFor("beta"));
  });

  it("round-trips a saved layout's session bindings", () => {
    const beta = makePane("beta");
    const saved: SavedTerminalLayout = {
      root: {
        type: "split",
        id: "split:outer",
        direction: "row",
        children: [
          makePane("alpha"),
          {
            type: "split",
            id: "split:inner",
            direction: "column",
            children: [beta, makePane("gamma")],
          },
        ],
      },
      activePaneId: beta.id,
      broadcast: true,
    };
    // Persist and reload the way `TerminalWorkspace` does.
    const reloaded = JSON.parse(JSON.stringify(saved)) as SavedTerminalLayout;
    const root = normalizeTerminalLayout(reloaded.root);
    expect(collectPanes(root!).map((pane) => pane.sessionId))
      .toEqual(["alpha", "beta", "gamma"]);
    expect(findPane(root!, reloaded.activePaneId)?.sessionId).toBe("beta");
    expect(reloaded.broadcast).toBe(true);
  });
});
