import { describe, expect, it, vi } from "vitest";
import {
  collectPanes,
  commitCreatedPane,
  containsSession,
  findPane,
  insertPane,
  makePane,
  normalizeTerminalLayout,
  paneIdFor,
  pruneTerminalLayout,
  removePane,
  retargetPane,
  type SavedTerminalLayout,
  type SplitNode,
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

// #212: composing existing sessions into a split, re-targeting a pane and
// detaching one are all pure tree transforms — no session is ever created.
describe("composing existing sessions (#212)", () => {
  it("inserts an existing session as a new pane without spawning one", () => {
    const root = makePane("one");
    const split = insertPane(root, root.id, "row", makePane("two"));
    expect(collectPanes(split!).map((pane) => pane.sessionId))
      .toEqual(["one", "two"]);
    // Every pane is bound to a session the caller already had, and each pane id
    // is derived from that session — nothing here manufactures a new one.
    expect(collectPanes(split!).map((pane) => pane.id))
      .toEqual([paneIdFor("one"), paneIdFor("two")]);
  });

  it("re-targets a pane at a session not already shown", () => {
    const root = insertPane(makePane("one"), paneIdFor("one"), "row", makePane("two"))!;
    const result = retargetPane(root, paneIdFor("two"), "three");
    expect(result).not.toBeNull();
    expect(collectPanes(result!.root).map((pane) => pane.sessionId))
      .toEqual(["one", "three"]);
    // The pane's id follows its new session, and the layout is otherwise intact.
    expect(result!.activePaneId).toBe(paneIdFor("three"));
    expect(containsSession(result!.root, "two")).toBe(false);
  });

  it("swaps two panes when the target session is already on screen", () => {
    const root = insertPane(makePane("one"), paneIdFor("one"), "row", makePane("two"))!;
    // Point the first pane at "two", which the second pane already shows.
    const result = retargetPane(root, paneIdFor("one"), "two");
    expect(collectPanes(result!.root).map((pane) => pane.sessionId))
      .toEqual(["two", "one"]);
    // A swap, not a duplication: both sessions survive, exactly once each.
    expect(collectPanes(result!.root)).toHaveLength(2);
    expect(result!.activePaneId).toBe(paneIdFor("two"));
  });

  it("re-targeting a missing pane changes nothing", () => {
    const root = makePane("one");
    expect(retargetPane(root, "pane:gone", "two")).toBeNull();
  });

  it("detaches a pane by dropping it from the tree, leaving its session alone", () => {
    const root = insertPane(makePane("one"), paneIdFor("one"), "row", makePane("two"))!;
    const detached = removePane(root, paneIdFor("two"));
    expect(collectPanes(detached!).map((pane) => pane.sessionId)).toEqual(["one"]);
    // `removePane` is a layout function only: it names no session-kill verb, so
    // detaching cannot reach the kill/DELETE path.
    expect(findPane(detached!, paneIdFor("two"))).toBeNull();
  });

  it("round-trips a saved layout's session bindings", () => {
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
            children: [makePane("beta"), makePane("gamma")],
          },
        ],
      },
      activePaneId: paneIdFor("beta"),
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

// #600: a re-target rebuilds only the spine down to the pane that changed and
// returns every untouched node by the same object reference. TerminalWorkspace
// renders split children through Solid's `<For>`, which is keyed by reference,
// so this identity is exactly what keeps sibling panes — their xterm, scrollback
// and socket — mounted while one pane switches sessions.
describe("re-target preserves untouched pane identity (#600)", () => {
  it("leaves an untouched sibling subtree referentially identical", () => {
    const inner: SplitNode = {
      type: "split",
      id: "split:inner",
      direction: "column",
      children: [makePane("two"), makePane("three")],
    };
    const root: TerminalLayoutNode = {
      type: "split",
      id: "split:outer",
      direction: "row",
      children: [makePane("one"), inner],
    };

    const result = retargetPane(root, paneIdFor("one"), "four");
    expect(result).not.toBeNull();
    // The whole inner split (and both its panes) is the same object, so `<For>`
    // never disposes it; only the "one" pane is replaced.
    expect((result!.root as SplitNode).children[1]).toBe(inner);
    expect(result!.activePaneId).toBe(paneIdFor("four"));
    expect(collectPanes(result!.root).map((pane) => pane.sessionId))
      .toEqual(["four", "two", "three"]);
  });

  it("swaps two panes and leaves the third untouched by reference", () => {
    const third = makePane("c");
    const root: TerminalLayoutNode = {
      type: "split",
      id: "split:row",
      direction: "row",
      children: [makePane("a"), makePane("b"), third],
    };

    // Point pane "a" at "b", which pane 2 already shows: the two swap.
    const result = retargetPane(root, paneIdFor("a"), "b");
    expect(result).not.toBeNull();
    expect((result!.root as SplitNode).children[2]).toBe(third);
    expect(collectPanes(result!.root).map((pane) => pane.sessionId))
      .toEqual(["b", "a", "c"]);
    expect(result!.activePaneId).toBe(paneIdFor("b"));
  });
});
