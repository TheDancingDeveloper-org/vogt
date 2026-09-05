// #601: drag-resizing split dividers. The size bookkeeping is a set of pure
// tree transforms, unit-tested here in isolation; the pointer/keyboard drag
// that drives them lives in the workspace component.

import { describe, expect, it } from "vitest";
import {
  MIN_PANE_FRACTION,
  collectPanes,
  insertPane,
  makePane,
  normalizeSizes,
  normalizeTerminalLayout,
  removePane,
  resetDivider,
  resizeSplit,
  type SplitNode,
  type TerminalLayoutNode,
} from "../terminalLayout";

const sum = (xs: number[]) => xs.reduce((a, b) => a + b, 0);

function twoPaneSplit(): { root: SplitNode; id: string } {
  const a = makePane("a");
  const root = insertPane(a, a.id, "row", makePane("b"))! as SplitNode;
  return { root, id: root.id };
}

describe("normalizeSizes (#601)", () => {
  it("returns equal shares when absent, wrong-length or malformed", () => {
    expect(normalizeSizes(2)).toEqual([0.5, 0.5]);
    expect(normalizeSizes(2, [0.7])).toEqual([0.5, 0.5]);
    expect(normalizeSizes(2, [0.7, 0.2, 0.1])).toEqual([0.5, 0.5]);
    expect(normalizeSizes(2, [0.5, 0])).toEqual([0.5, 0.5]);
    expect(normalizeSizes(2, [Number.NaN, 0.5])).toEqual([0.5, 0.5]);
  });

  it("renormalises a valid array to sum 1", () => {
    const s = normalizeSizes(3, [1, 2, 1]);
    expect(sum(s)).toBeCloseTo(1);
    expect(s).toEqual([0.25, 0.5, 0.25]);
  });
});

describe("resizeSplit (#601)", () => {
  it("moves a fraction from one child to its neighbour, others untouched", () => {
    const a = makePane("a");
    const ab = insertPane(a, a.id, "row", makePane("b"))! as SplitNode;
    // Three children in one row via a flat, hand-built split.
    const root: SplitNode = {
      type: "split",
      id: "s",
      direction: "row",
      children: [makePane("x"), makePane("y"), makePane("z")],
    };
    const out = resizeSplit(root, "s", 0, 0.1) as SplitNode;
    expect(out.sizes![0]).toBeCloseTo(1 / 3 + 0.1);
    expect(out.sizes![1]).toBeCloseTo(1 / 3 - 0.1);
    expect(out.sizes![2]).toBeCloseTo(1 / 3); // the far child is undisturbed
    expect(sum(out.sizes!)).toBeCloseTo(1);
    void ab;
  });

  it("clamps both neighbours to the minimum fraction", () => {
    const { root, id } = twoPaneSplit();
    // Drag far past the edge: the near pane cannot exceed 1 - min.
    const out = resizeSplit(root, id, 0, 5) as SplitNode;
    expect(out.sizes![0]).toBeCloseTo(1 - MIN_PANE_FRACTION);
    expect(out.sizes![1]).toBeCloseTo(MIN_PANE_FRACTION);
    // …and the other direction.
    const out2 = resizeSplit(root, id, 0, -5) as SplitNode;
    expect(out2.sizes![0]).toBeCloseTo(MIN_PANE_FRACTION);
    expect(out2.sizes![1]).toBeCloseTo(1 - MIN_PANE_FRACTION);
  });

  it("leaves nodes other than the target split untouched by reference", () => {
    const inner: SplitNode = {
      type: "split",
      id: "inner",
      direction: "column",
      children: [makePane("b"), makePane("c")],
    };
    const root: SplitNode = {
      type: "split",
      id: "outer",
      direction: "row",
      children: [makePane("a"), inner],
    };
    const out = resizeSplit(root, "inner", 0, 0.1) as SplitNode;
    // The outer split's other child ("a") and the outer node's untouched
    // subtree keep their references, so the renderer leaves them mounted.
    expect(out.children[0]).toBe(root.children[0]);
    expect(out).not.toBe(root);
  });
});

describe("resetDivider (#601)", () => {
  it("equalises the two neighbours a divider separates", () => {
    const { root, id } = twoPaneSplit();
    const dragged = resizeSplit(root, id, 0, 0.3) as SplitNode;
    expect(dragged.sizes![0]).toBeCloseTo(0.8);
    const reset = resetDivider(dragged, id, 0) as SplitNode;
    expect(reset.sizes![0]).toBeCloseTo(0.5);
    expect(reset.sizes![1]).toBeCloseTo(0.5);
  });
});

describe("structural edits keep sizes consistent (#601)", () => {
  it("normalises sizes carried through persistence", () => {
    const stored: TerminalLayoutNode = {
      type: "split",
      id: "s",
      direction: "row",
      children: [makePane("a"), makePane("b")],
      sizes: [0.7, 0.3],
    };
    const reloaded = normalizeTerminalLayout(
      JSON.parse(JSON.stringify(stored)),
    ) as SplitNode;
    expect(reloaded.sizes).toEqual([0.7, 0.3]);
  });

  it("renormalises the survivors when a child is removed", () => {
    const root: SplitNode = {
      type: "split",
      id: "s",
      direction: "row",
      children: [makePane("a"), makePane("b"), makePane("c")],
      sizes: [0.2, 0.3, 0.5],
    };
    const bId = collectPanes(root).find((p) => p.sessionId === "b")!.id;
    const out = removePane(root, bId) as SplitNode;
    // "a" and "c" kept 0.2 : 0.5, renormalised to sum 1.
    const [first, second] = out.sizes!;
    expect(out.sizes!.length).toBe(2);
    expect(sum(out.sizes!)).toBeCloseTo(1);
    expect(first! / second!).toBeCloseTo(0.2 / 0.5);
  });
});
