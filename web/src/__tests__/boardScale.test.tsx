// NFR-S5 on the board, both halves: the columns window rather than truncate,
// and the projection behind them does not grow with the board's own shape.
//
// `REQUIREMENTS.md` §6.2 carried these as two rows. The first said the board
// "caps at 60 cards per cell with an explicit '+N more'" while the backlog
// genuinely windows. The second said the filter and drag paths were
// "unevidenced, and there is reason to doubt it" — the cell and column
// projections were linear scans, run per cell and per column, over as many as
// 2,000 items.
//
// **Why these tests count rather than time.** jsdom has no layout engine and
// no rendering: nothing here has a height, a scroll offset does not stick to
// an element with no box, and a wall-clock measurement would mostly be
// measuring vitest. A number in a commit message would be worse still — it
// proves nothing on anyone else's machine and it cannot fail. So what is
// asserted is the *algorithmic* property directly: how many times the board
// reads a work item's `state`, which is the field and the only field its
// projection walks the loaded set for. Counting is a fair proxy where timing
// is not, and it is a strictly sharper one for the thing that actually went
// wrong here — the old cost was quadratic in the board's shape, so it shows up
// as a ratio between two boards of the same size, which no timing test in a
// headless DOM could see.
//
// The count is taken by instrumenting `JSON.parse`, which is where the loaded
// items come into existence (`vogtApi.ts` parses the response text itself).
// That is deliberately *outside* the surface: `Board.tsx` carries no counter,
// no hook and no test-only branch, so what is measured is the shipped code.

import { describe, expect, it, vi } from "vitest";
import { fireEvent, waitFor } from "@solidjs/testing-library";
import Board, { CARD_ESTIMATE, cellKey, projectBoard, type Lane } from "../Board";
import type { WorkItem } from "../vogtApi";
import { fakeVogt, mountAt, refusal, settle, workItem } from "./harness";

function board(url = "/board") {
  return mountAt("/board", url, () => <Board />);
}

function cell(container: HTMLElement, state: string): HTMLElement {
  const found = container.querySelector<HTMLElement>(
    `.board-cell[data-state="${state}"]`,
  );
  if (!found) throw new Error(`no cell for ${state}`);
  return found;
}

function drawnRefs(container: HTMLElement, state: string): string[] {
  return [...cell(container, state).querySelectorAll<HTMLElement>(".board-card")].map(
    (node) => node.id.replace("board-card-", ""),
  );
}

/** A straight-line machine of `states`, so the columns come out in order. */
function chain(kind: string, states: string[]): Record<string, unknown> {
  const transitions: Record<string, string[]> = {};
  states.forEach((state, at) => {
    transitions[state] = at + 1 < states.length ? [states[at + 1]!] : [];
  });
  return { kind, initial_state: states[0], states, transitions };
}

/** `count` items, all in `state`, refs `WI-0` upward and in that order: they
 *  share a priority and an `updated_at`, and the lane sort is stable. */
function column(count: number, over: (at: number) => Record<string, unknown> = () => ({})) {
  return Array.from({ length: count }, (_unused, at) =>
    workItem({ ref: `WI-${at}`, state: "open", ...over(at) }),
  );
}

/**
 * A `ResizeObserver` the test can fire, and a height to fire it with.
 *
 * The surface measures its cells rather than assuming a height. This helper
 * supplies the content-box entry a real observer would provide while keeping
 * jsdom's layout-free behavior explicit.
 *
 * Must be installed before the board mounts; the observer is created when the
 * cell's scroller is.
 */
function measuredCells(): (height: number) => void {
  const watched: { node: Element; fire: (height: number) => void }[] = [];
  vi.stubGlobal(
    "ResizeObserver",
    class {
      readonly #callback: ResizeObserverCallback;
      constructor(callback: ResizeObserverCallback) {
        this.#callback = callback;
      }
      observe(node: Element): void {
        watched.push({
          node,
          fire: (height) => this.#callback(
            [{ target: node, contentRect: { height } } as ResizeObserverEntry],
            this as unknown as ResizeObserver,
          ),
        });
      }
      unobserve(): void {}
      disconnect(): void {}
    },
  );
  return (height: number) => {
    for (const one of watched) {
      if (one.node.classList.contains("board-cell-cards")) {
        Object.defineProperty(one.node, "clientHeight", {
          value: height,
          configurable: true,
        });
        one.fire(height);
      }
    }
  };
}

/** Fire a content-size change for a rendered card. */
function measuredCards(): (ref: string, height: number) => void {
  const watched: { node: Element; callback: ResizeObserverCallback }[] = [];
  vi.stubGlobal(
    "ResizeObserver",
    class {
      readonly #callback: ResizeObserverCallback;
      constructor(callback: ResizeObserverCallback) {
        this.#callback = callback;
      }
      observe(node: Element): void {
        watched.push({ node, callback: this.#callback });
      }
      unobserve(): void {}
      disconnect(): void {}
    },
  );
  return (ref: string, height: number) => {
    const one = watched.find((entry) => entry.node.id === `board-card-${ref}`);
    if (!one) throw new Error(`card ${ref} is not rendered`);
    one.callback(
      [{
        target: one.node,
        contentRect: { height },
        borderBoxSize: [{ blockSize: height }],
      } as unknown as ResizeObserverEntry],
      {} as ResizeObserver,
    );
  };
}

/**
 * Scroll a cell's list.
 *
 * jsdom has no layout, so the offset is defined on the element rather than
 * produced by scrolling it — nothing here can make the scroller 600px tall
 * and an assignment to `scrollTop` on a box-less element does not stick. The
 * event the surface listens for is then dispatched, which is exactly what a
 * real scroll delivers. The measured prefix-sum tests cover the arithmetic
 * that keeps browser scroll anchoring aligned with the rendered cards.
 */
function scrollCell(container: HTMLElement, state: string, offset: number): void {
  const scroller = cell(container, state).querySelector<HTMLElement>(
    ".board-cell-cards",
  );
  if (!scroller) throw new Error(`the ${state} cell has no scroller`);
  Object.defineProperty(scroller, "scrollTop", {
    value: offset,
    configurable: true,
    writable: true,
  });
  fireEvent.scroll(scroller);
}

// -- half one: the columns window -------------------------------------------

describe("NFR-S5 — a long column windows rather than truncating", () => {
  it("draws a slice of a long column, not all of it and not the first sixty", async () => {
    const total = 400;
    fakeVogt({
      "GET /workflows": { body: { workflows: [chain("feature", ["open", "done"])] } },
      "GET /work": { body: { items: column(total), total } },
    });
    const { container } = board();

    await waitFor(() => expect(drawnRefs(container, "open").length).toBeGreaterThan(0));
    await settle();

    const drawn = drawnRefs(container, "open");
    // A window, not the whole column: the reactive graph and the DOM stop
    // growing with the estate, which is the requirement's actual subject.
    expect(drawn.length).toBeLessThan(60);
    // And not a cap either — the cap drew its 60 and then said so out loud.
    expect(container.textContent).not.toContain("more here");
    expect(container.querySelector(".board-more")).toBeNull();
    // In the order the filter matched them, from the top.
    expect(drawn[0]).toBe("WI-0");
    expect(drawn).toEqual([...drawn].sort((a, b) => Number(a.slice(3)) - Number(b.slice(3))));
  });

  it("draws a short column whole, because windowing is not free", async () => {
    // Under the threshold the cell renders every card, which is the backlog's
    // reasoning unchanged: a window costs the browser's own find-in-page and
    // costs a reader the ability to select across the list, and that is worth
    // paying at length and not worth paying for a screenful.
    const total = 40;
    fakeVogt({
      "GET /workflows": { body: { workflows: [chain("feature", ["open", "done"])] } },
      "GET /work": { body: { items: column(total), total } },
    });
    const { container } = board();

    await waitFor(() => expect(drawnRefs(container, "open")).toHaveLength(total));
    expect(drawnRefs(container, "open")[total - 1]).toBe(`WI-${total - 1}`);
  });

  it("draws as much of the column as the cell turns out to be tall, and more", async () => {
    const resize = measuredCells();
    try {
      const total = 400;
      fakeVogt({
        "GET /workflows": { body: { workflows: [chain("feature", ["open", "done"])] } },
        "GET /work": { body: { items: column(total), total } },
      });
      const { container } = board();

      await waitFor(() =>
        expect(drawnRefs(container, "open").length).toBeGreaterThan(0),
      );
      // Nothing has measured the cell yet, so the window is the screenful the
      // surface assumes when it has no other information.
      const assumed = drawnRefs(container, "open").length;

      // A cell thirty cards tall.
      resize(30 * (CARD_ESTIMATE + 8));
      await waitFor(() =>
        expect(drawnRefs(container, "open").length).toBeGreaterThan(assumed),
      );

      // Thirty fit, and strictly more than thirty are drawn: the overscan
      // either side is what stops a fast scroll showing blank space, and a
      // window sized exactly to the viewport would have none.
      expect(drawnRefs(container, "open").length).toBeGreaterThan(30);
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("keeps a reader's place in a column across a reload", async () => {
    const total = 400;
    const vogt = fakeVogt({
      "GET /workflows": { body: { workflows: [chain("feature", ["open", "done"])] } },
      "GET /work": { body: { items: column(total), total } },
    });
    const { container } = board();

    await waitFor(() => expect(drawnRefs(container, "open").length).toBeGreaterThan(0));
    scrollCell(container, "open", 200 * (CARD_ESTIMATE + 8));
    await waitFor(() => expect(drawnRefs(container, "open")).toContain("WI-200"));

    // A load is a new set of item objects and a new set of lanes, and `For`
    // maps its rows by reference — so every cell on the board is destroyed
    // and rebuilt. With the poll running that happens every twenty seconds,
    // and a cell that kept its offset to itself would drag the reader back to
    // the top of the column while they were reading it.
    const refresh = [...container.querySelectorAll("button")].find(
      (node) => node.textContent === "Refresh now",
    );
    fireEvent.click(refresh!);
    await waitFor(() => expect(vogt.matching("GET /work").length).toBeGreaterThan(1));
    await settle();

    expect(drawnRefs(container, "open")).toContain("WI-200");
    expect(drawnRefs(container, "open")).not.toContain("WI-0");
  });

  it("anchors scroll when a rendered card above the viewport grows", async () => {
    const resizeCard = measuredCards();
    try {
      const total = 400;
      fakeVogt({
        "GET /workflows": { body: { workflows: [chain("feature", ["open", "done"])] } },
        "GET /work": { body: { items: column(total), total } },
      });
      const { container } = board();

      await waitFor(() => expect(drawnRefs(container, "open").length).toBeGreaterThan(0));
      scrollCell(container, "open", 200 * (CARD_ESTIMATE + 8));
      await waitFor(() => expect(drawnRefs(container, "open")).toContain("WI-200"));
      const scroller = cell(container, "open").querySelector<HTMLElement>(".board-cell-cards")!;
      const before = scroller.scrollTop;

      // WI-198 is in the overscan slice immediately above the viewport. Its
      // growth must move the scroll offset by the same delta or the reader's
      // focused card would jump while ResizeObserver catches up.
      resizeCard("WI-198", 212);
      await waitFor(() => expect(scroller.scrollTop).toBe(before + 100));
    } finally {
      vi.unstubAllGlobals();
    }
  });

  it("puts a windowed column back to the top when the filter changes", async () => {
    const total = 400;
    fakeVogt({
      "GET /workflows": { body: { workflows: [chain("feature", ["open", "done"])] } },
      "GET /work": { body: { items: column(total), total } },
    });
    const { container } = board();

    await waitFor(() => expect(drawnRefs(container, "open").length).toBeGreaterThan(0));
    scrollCell(container, "open", 200 * (CARD_ESTIMATE + 8));
    await waitFor(() => expect(drawnRefs(container, "open")).toContain("WI-200"));

    const label = [...container.querySelectorAll<HTMLElement>(".board-field")].find(
      (node) => node.querySelector("span")?.textContent === "Label",
    );
    fireEvent.input(label!.querySelector("select")!, { target: { value: "infra" } });

    // A new filter set is a new column. Keeping the old offset would land the
    // reader two hundred cards into a list they have not seen the top of.
    await waitFor(() => expect(drawnRefs(container, "open")[0]).toBe("WI-0"));
  });

  it("still counts the whole column, and gives the scrollbar its whole length", async () => {
    const total = 400;
    fakeVogt({
      "GET /workflows": { body: { workflows: [chain("feature", ["open", "done"])] } },
      "GET /work": { body: { items: column(total), total } },
    });
    const { container } = board();

    await waitFor(() => expect(drawnRefs(container, "open").length).toBeGreaterThan(0));

    // The WIP badge counts what was loaded, not what was drawn: a windowed
    // column that reported its window would be the cap wearing a disguise.
    const wip = container.querySelector(".board-colhead .board-wip");
    expect(wip?.textContent).toBe(String(total));
    expect(cell(container, "open").dataset.wip).toBe(String(total));

    // And the scroll runway is the length of the column, so the scrollbar is
    // the length of the column.
    const run = cell(container, "open").querySelector<HTMLElement>(".board-cell-run");
    expect(run?.style.height).toBe(`${total * (CARD_ESTIMATE + 8)}px`);
  });

  it("reaches the last card the filter matched, which the cap never could", async () => {
    const total = 400;
    fakeVogt({
      "GET /workflows": { body: { workflows: [chain("feature", ["open", "done"])] } },
      "GET /work": { body: { items: column(total), total } },
    });
    const { container } = board();

    await waitFor(() => expect(drawnRefs(container, "open").length).toBeGreaterThan(0));
    expect(drawnRefs(container, "open")).not.toContain("WI-399");

    // Under the cap, WI-60 onwards were not reachable at all: the instruction
    // was to narrow the filter until the board agreed to show them.
    scrollCell(container, "open", 200 * (CARD_ESTIMATE + 8));
    await waitFor(() => expect(drawnRefs(container, "open")).toContain("WI-200"));

    scrollCell(container, "open", total * (CARD_ESTIMATE + 8));
    await waitFor(() => expect(drawnRefs(container, "open")).toContain("WI-399"));
    expect(drawnRefs(container, "open")).not.toContain("WI-0");
  });

  it("moves focus to a card the window has not drawn (FR-U22)", async () => {
    const total = 300;
    fakeVogt({
      "GET /workflows": { body: { workflows: [chain("feature", ["open", "done"])] } },
      "GET /work": { body: { items: column(total), total } },
    });
    const { container } = board();

    await waitFor(() => expect(drawnRefs(container, "open").length).toBeGreaterThan(0));
    const drawn = drawnRefs(container, "open");
    const last = drawn[drawn.length - 1]!;
    const beyond = `WI-${Number(last.slice(3)) + 1}`;

    // The card the keyboard is about to move to is not in the DOM, and an
    // element that is not in the DOM cannot take focus. This is the failure a
    // windowed column introduces and the cap did not have.
    expect(container.querySelector(`#board-card-${beyond}`)).toBeNull();

    const from = container.querySelector<HTMLElement>(`#board-card-${last}`)!;
    from.focus();
    fireEvent.keyDown(from, { key: "ArrowDown" });

    await waitFor(() =>
      expect(document.activeElement?.id).toBe(`board-card-${beyond}`),
    );
    expect(drawnRefs(container, "open")).toContain(beyond);
  });

  it("rolls a refused move back into a windowed column and says why there", async () => {
    const total = 300;
    const refused =
      "transition.not_allowed: feature has no open -> done edge " +
      "(allowed from open: in_progress)";
    fakeVogt({
      "GET /workflows": {
        body: { workflows: [chain("feature", ["open", "in_progress", "done"])] },
      },
      "GET /work": { body: { items: column(total), total } },
      "POST /work/transition": refusal(409, refused),
    });
    const { container } = board();

    await waitFor(() => expect(drawnRefs(container, "open").length).toBeGreaterThan(0));

    // FR-U12 and FR-U21 together, on the column shape most likely to lose
    // them: the card is drawn in the target cell while the reason is
    // collected, and both cells are windowed.
    fireEvent.dragStart(container.querySelector<HTMLElement>("#board-card-WI-0")!);
    fireEvent.drop(cell(container, "done"));
    await waitFor(() =>
      expect(cell(container, "done").querySelector("textarea")).toBeTruthy(),
    );
    expect(drawnRefs(container, "done")).toEqual(["WI-0"]);
    expect(drawnRefs(container, "open")).not.toContain("WI-0");

    const composer = cell(container, "done").querySelector("textarea")!;
    fireEvent.input(composer, { target: { value: "it looks finished to me" } });
    fireEvent.click(
      cell(container, "done").querySelector<HTMLButtonElement>("button[type=submit]")!,
    );

    const notice = await waitFor(() => {
      const node = cell(container, "done").querySelector(".board-refusal");
      expect(node).toBeTruthy();
      return node as HTMLElement;
    });
    expect(notice.textContent).toContain(refused);
    // Back in `open`, at the top of the window, and drawn — a rollback into a
    // column whose window had scrolled past it would be a card that
    // disappeared.
    expect(drawnRefs(container, "open")[0]).toBe("WI-0");
    expect(drawnRefs(container, "done")).toEqual([]);
  });
});

// -- half two: the projection does not grow with the board's shape ----------

/**
 * Count every read of `state` on the work items the board loaded.
 *
 * `state` is the field the projection exists to walk — it is what decides
 * which cell a card is in and what a column's WIP count is — and nothing that
 * *draws* a card reads it. So the count is the number of times the loaded set
 * was walked, times its size, which is exactly the quantity NFR-S5's second
 * clause is about.
 *
 * The getter is installed on the way in, in `JSON.parse`, because that is
 * where `vogtApi.ts` turns the response text into the objects the board holds
 * — and it holds those objects, so the instrumentation survives the merge on
 * every poll.
 */
function stateReads(): () => number {
  let reads = 0;
  const parse = JSON.parse.bind(JSON) as typeof JSON.parse;
  vi.spyOn(JSON, "parse").mockImplementation(((text: string, reviver?: never) => {
    const value: unknown = parse(text, reviver);
    if (!value || typeof value !== "object") return value;
    const items = (value as { items?: unknown }).items;
    if (!Array.isArray(items)) return value;
    for (const entry of items) {
      if (!entry || typeof entry !== "object") continue;
      const item = entry as Record<string, unknown>;
      if (typeof item.state !== "string") continue;
      const state = item.state;
      Object.defineProperty(item, "state", {
        configurable: true,
        enumerable: true,
        get: () => {
          reads += 1;
          return state;
        },
      });
    }
    return value;
  }) as typeof JSON.parse);
  return () => reads;
}

async function settled(container: HTMLElement, columns: number): Promise<void> {
  await waitFor(() =>
    expect(container.querySelectorAll(".board-colhead")).toHaveLength(columns),
  );
  await waitFor(() =>
    expect(container.querySelectorAll(".board-card").length).toBeGreaterThan(0),
  );
  await settle();
}

describe("NFR-S5 — the filter and drag paths do not degrade with backlog size", () => {
  it("does not project once per cell: tripling the columns does not triple the work", async () => {
    const size = 120;
    const four = ["open", "in_progress", "review", "done"];
    const twelve = [...four, "a", "b", "c", "d", "e", "f", "g", "h"];
    const vogt = fakeVogt({
      "GET /workflows": { body: { workflows: [chain("feature", four)] } },
      "GET /work": { body: { items: column(size), total: size } },
    });
    const reads = stateReads();

    const narrow = board();
    await settled(narrow.container, four.length);
    const narrowReads = reads();
    narrow.unmount();

    vogt.route("GET /workflows", { body: { workflows: [chain("feature", twelve)] } });
    const before = reads();
    const wide = board();
    await settled(wide.container, twelve.length);
    const wideReads = reads() - before;
    wide.unmount();

    // One pass over the loaded set costs `size` reads. The claim is that
    // eight more columns cost *no* extra passes; the bound allows one, so a
    // rerender does not make this red on its own. The projection this
    // replaced cost eight extra passes per column — twenty-four in all, or
    // `wideReads > narrowReads + 24 * size`.
    expect(narrowReads).toBeLessThanOrEqual(size * 5);
    expect(wideReads).toBeLessThanOrEqual(narrowReads + size);
  });

  it("does not project once per swimlane: a bigger estate does not cost more per item", async () => {
    const states = ["open", "in_progress", "review", "done"];
    const small = column(40, (at) => ({ project_slug: `p${at % 5}` }));
    const large = column(160, (at) => ({ project_slug: `p${at % 20}` }));
    const vogt = fakeVogt({
      "GET /workflows": { body: { workflows: [chain("feature", states)] } },
      "GET /work": { body: { items: small, total: small.length } },
      "GET /projects": {
        body: {
          projects: Array.from({ length: 20 }, (_, index) => ({
            slug: `p${index}`,
            name: `Project ${index}`,
          })),
        },
      },
    });
    const reads = stateReads();

    const few = board("/board?lanes=project");
    await settled(few.container, states.length);
    const perItemFew = reads() / small.length;
    few.unmount();

    vogt.route("GET /work", { body: { items: large, total: large.length } });
    const before = reads();
    const many = board("/board?lanes=project");
    await settled(many.container, states.length);
    const perItemMany = (reads() - before) / large.length;
    many.unmount();

    // Four times the items in four times the lanes. The work per item is the
    // number of passes over the loaded set, and it is supposed to be a
    // constant — the same handful whichever board this is. Under the
    // per-cell projection it was `1 + columns * (2 + lanes)`, so it went from
    // about 29 reads per item to about 89: growing the estate made every
    // item on it more expensive, which is precisely "degrades with backlog
    // size".
    expect(perItemFew).toBeLessThanOrEqual(5);
    expect(perItemMany).toBeLessThanOrEqual(perItemFew + 1);
  });

  it("re-projects nothing while a reason is being typed into the composer", async () => {
    const size = 200;
    const states = ["open", "in_progress", "review", "done"];
    fakeVogt({
      "GET /workflows": { body: { workflows: [chain("feature", states)] } },
      "GET /work": { body: { items: column(size), total: size } },
    });
    const reads = stateReads();
    const { container } = board();
    await settled(container, states.length);

    fireEvent.dragStart(container.querySelector<HTMLElement>("#board-card-WI-0")!);
    fireEvent.drop(cell(container, "in progress"));
    await waitFor(() =>
      expect(cell(container, "in progress").querySelector("textarea")).toBeTruthy(),
    );
    await settle();

    // The drop itself moves a card, so it re-projects — that is correct and
    // is not what this measures. What follows is typing, which changes the
    // reason and nothing else.
    const composer = cell(container, "in progress").querySelector("textarea")!;
    const before = reads();
    for (const reason of ["p", "pi", "pic", "pick", "picke", "picked", "picked ", "picked u"]) {
      fireEvent.input(composer, { target: { value: reason } });
    }
    await settle();

    // Exactly none. The pending move carries the position *and* the reason,
    // so a projection that read it directly re-ran on every keystroke: eight
    // characters cost twelve passes each over two hundred items, about
    // twenty thousand comparisons, in the middle of a sentence somebody was
    // typing.
    expect(reads() - before).toBe(0);
    // And the card is still where the drop put it, which is the behaviour
    // that had to survive the optimisation (FR-U12).
    expect(drawnRefs(container, "in progress")).toEqual(["WI-0"]);
  });
});

// -- the projection on its own ----------------------------------------------

describe("projectBoard — one walk, whatever the board's shape", () => {
  function lane(key: string, items: WorkItem[]): Lane {
    return { key, label: key, items };
  }

  function counted(refs: string[], state: string): { items: WorkItem[]; reads: () => number } {
    let reads = 0;
    const items = refs.map((ref) => {
      const item = { ...(workItem({ ref }) as unknown as WorkItem) };
      Object.defineProperty(item, "state", {
        configurable: true,
        enumerable: true,
        get: () => {
          reads += 1;
          return state;
        },
      });
      return item;
    });
    return { items, reads: () => reads };
  }

  it("reads each item exactly once, however many cells come out of it", () => {
    const { items, reads } = counted(["A", "B", "C", "D"], "open");
    const projected = projectBoard([lane("x", items.slice(0, 2)), lane("y", items.slice(2))], null);

    expect(reads()).toBe(items.length);
    expect(projected.cells.get(cellKey("x", "open"))?.map((one) => one.ref)).toEqual(["A", "B"]);
    expect(projected.cells.get(cellKey("y", "open"))?.map((one) => one.ref)).toEqual(["C", "D"]);
    expect(projected.counts.get("open")).toBe(4);
    expect(projected.where.get("D")).toEqual({ cell: cellKey("y", "open"), index: 1 });
  });

  it("draws an unsaved drop where it was dropped, and counts it there", () => {
    const { items } = counted(["A", "B"], "open");
    const projected = projectBoard([lane("", items)], { ref: "B", to: "done" });

    expect(projected.cells.get(cellKey("", "open"))?.map((one) => one.ref)).toEqual(["A"]);
    expect(projected.cells.get(cellKey("", "done"))?.map((one) => one.ref)).toEqual(["B"]);
    expect(projected.counts.get("open")).toBe(1);
    expect(projected.counts.get("done")).toBe(1);
  });

  it("keeps one lane's cards out of another's cell", () => {
    const { items } = counted(["A", "B"], "open");
    const projected = projectBoard(
      [lane("alpha", [items[0]!]), lane("beta", [items[1]!])],
      null,
    );

    expect(projected.cells.get(cellKey("alpha", "open"))?.map((one) => one.ref)).toEqual(["A"]);
    expect(projected.cells.get(cellKey("beta", "open"))?.map((one) => one.ref)).toEqual(["B"]);
    // The WIP count is the column across every lane, which is what the head
    // row says it is.
    expect(projected.counts.get("open")).toBe(2);
  });
});
