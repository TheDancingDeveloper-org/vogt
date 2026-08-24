import { describe, expect, it } from "vitest";
import {
  collectPanes,
  dropSessionIntoPane,
  insertPane,
  makePane,
  paneIdFor,
} from "../terminalLayout";
import {
  directionForZone,
  dropZoneForPoint,
  zoneInsertsBefore,
} from "../terminalDnd";

// #355: dragging a session from the rail onto a pane mirrors it into a split.
// Edge hit-testing picks the direction; the duplicate guard focuses an existing
// pane rather than showing a session twice.
describe("drop-zone edge hit-testing (#355)", () => {
  const rect = { left: 0, top: 0, width: 100, height: 100 };

  it("maps the nearest edge to a zone", () => {
    expect(dropZoneForPoint(rect, 5, 50)).toBe("left");
    expect(dropZoneForPoint(rect, 95, 50)).toBe("right");
    expect(dropZoneForPoint(rect, 50, 5)).toBe("top");
    expect(dropZoneForPoint(rect, 50, 95)).toBe("bottom");
  });

  it("derives a row split for left/right and a column split for top/bottom", () => {
    expect(directionForZone("left")).toBe("row");
    expect(directionForZone("right")).toBe("row");
    expect(directionForZone("top")).toBe("column");
    expect(directionForZone("bottom")).toBe("column");
  });

  it("inserts ahead of the target only on a left/top drop", () => {
    expect(zoneInsertsBefore("left")).toBe(true);
    expect(zoneInsertsBefore("top")).toBe(true);
    expect(zoneInsertsBefore("right")).toBe(false);
    expect(zoneInsertsBefore("bottom")).toBe(false);
  });

  it("degrades gracefully for a zero-size rect", () => {
    expect(dropZoneForPoint({ left: 0, top: 0, width: 0, height: 0 }, 0, 0)).toBe(
      "left",
    );
  });
});

describe("dropping a session into a pane (#355)", () => {
  it("inserts a new pane in the hit-tested direction", () => {
    const root = makePane("one");
    const outcome = dropSessionIntoPane(root, paneIdFor("one"), "two", "column");
    expect(outcome).not.toBeNull();
    expect(outcome!.inserted).toBe(true);
    expect(outcome!.root.type).toBe("split");
    expect(outcome!.root.type === "split" && outcome!.root.direction).toBe("column");
    expect(collectPanes(outcome!.root).map((p) => p.sessionId)).toEqual([
      "one",
      "two",
    ]);
    // The new pane is focused, and its id derives from the session.
    expect(outcome!.activePaneId).toBe(paneIdFor("two"));
  });

  it("places the new pane before the target when dropped on a left/top edge", () => {
    const root = makePane("one");
    const outcome = dropSessionIntoPane(root, paneIdFor("one"), "two", "row", true);
    expect(collectPanes(outcome!.root).map((p) => p.sessionId)).toEqual([
      "two",
      "one",
    ]);
  });

  it("focuses the existing pane instead of duplicating a session already shown", () => {
    const root = insertPane(makePane("one"), paneIdFor("one"), "row", makePane("two"))!;
    const outcome = dropSessionIntoPane(root, paneIdFor("one"), "two", "column");
    expect(outcome).not.toBeNull();
    // No insertion: the session was already on screen, so its pane is focused.
    expect(outcome!.inserted).toBe(false);
    expect(outcome!.root).toBe(root);
    expect(outcome!.activePaneId).toBe(paneIdFor("two"));
    // Exactly one pane per session — no duplicate.
    expect(collectPanes(outcome!.root).map((p) => p.sessionId)).toEqual([
      "one",
      "two",
    ]);
  });

  it("mirrors, never moves: the source session stays wherever else it renders", () => {
    // A session already in pane "two" is dropped onto pane "one". Because the
    // guard focuses rather than inserts, "two" keeps rendering it — a mirror,
    // not a detach/move.
    const root = insertPane(makePane("one"), paneIdFor("one"), "row", makePane("two"))!;
    const outcome = dropSessionIntoPane(root, paneIdFor("one"), "two", "row");
    expect(collectPanes(outcome!.root)).toHaveLength(2);
    expect(collectPanes(outcome!.root).map((p) => p.sessionId).sort()).toEqual([
      "one",
      "two",
    ]);
  });

  it("returns null when the target pane is gone", () => {
    expect(dropSessionIntoPane(makePane("one"), "pane:gone", "two", "row")).toBeNull();
  });
});
