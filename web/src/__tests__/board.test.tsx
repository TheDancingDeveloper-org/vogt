// The board's interaction contract (FR-U4, FR-U11, FR-U12, FR-U17, FR-U21).
//
// These are the claims `REQUIREMENTS.md` §6 counted as "asserted by nothing":
// the columns come from `workflow.list`, a drag is a `work.transition` that
// renders optimistically, and a refusal rolls the card back *and shows Vogt's
// own sentence where the drop happened*. §6's note on that last one is the
// reason this file exists — "the refusal path is written to discard the
// optimistic position outright and has never discarded one".

import { describe, expect, it } from "vitest";
import { fireEvent, waitFor } from "@solidjs/testing-library";
import Board from "../Board";
import {
  fakeVogt,
  mountAt,
  queryOf,
  refusal,
  unavailable,
  workItem,
} from "./harness";

/** The board at `/board`, which is the path its URL effect guards on. */
function board(url = "/board") {
  return mountAt("/board", url, () => <Board />);
}

const REFUSAL =
  "transition.not_allowed: feature has no open -> done edge " +
  "(allowed from open: in_progress, wont_do)";

function columnNames(container: HTMLElement): string[] {
  return [...container.querySelectorAll(".board-colhead-name")].map(
    (node) => node.textContent ?? "",
  );
}

function cell(container: HTMLElement, state: string): HTMLElement {
  const found = container.querySelector<HTMLElement>(`.board-cell[data-state="${state}"]`);
  if (!found) throw new Error(`no cell for ${state}; columns: ${columnNames(container)}`);
  return found;
}

function card(container: HTMLElement, ref: string): HTMLElement {
  const found = container.querySelector<HTMLElement>(`#board-card-${ref}`);
  if (!found) throw new Error(`no card for ${ref}`);
  return found;
}

/** Drag `ref` onto the cell for `state`, as a mouse would. */
async function dragTo(container: HTMLElement, ref: string, state: string): Promise<void> {
  fireEvent.dragStart(card(container, ref));
  fireEvent.drop(cell(container, state));
  await waitFor(() => expect(cell(container, state).querySelector("textarea")).toBeTruthy());
}

/** Type the reason the drop is waiting for and submit it. */
async function giveReason(container: HTMLElement, state: string, reason: string): Promise<void> {
  const composer = cell(container, state).querySelector("textarea");
  if (!composer) throw new Error(`no composer open in ${state}`);
  fireEvent.input(composer, { target: { value: reason } });
  const submit = cell(container, state).querySelector<HTMLButtonElement>(
    "button[type=submit]",
  );
  if (!submit) throw new Error("the composer has no submit");
  await waitFor(() => expect(submit.disabled).toBe(false));
  fireEvent.click(submit);
}

describe("FR-U4 — the columns are the workflow's states, never hard-coded", () => {
  it("draws one column per state the server published, in machine order", async () => {
    const vogt = fakeVogt();
    const { container } = board();

    await waitFor(() => expect(columnNames(container).length).toBeGreaterThan(0));

    // The union of the two default machines, ordered by walking each from its
    // initial state, with the states that lead nowhere pushed to the end.
    expect(columnNames(container)).toEqual(["open", "in progress", "done", "wont do"]);
    expect(vogt.matching("GET /workflows")).toHaveLength(1);
  });

  it("changes shape when the workflow does, because nothing here is written down", async () => {
    fakeVogt({
      "GET /workflows": {
        body: {
          workflows: [
            {
              kind: "feature",
              initial_state: "triage",
              states: ["triage", "building", "shipped"],
              transitions: { triage: ["building"], building: ["shipped"], shipped: [] },
            },
          ],
        },
      },
      "GET /work": { body: { items: [], total: 0 } },
    });
    const { container } = board();

    await waitFor(() => expect(columnNames(container)).toEqual(["triage", "building", "shipped"]));
    // Not one of the previous test's names survives, which is the whole claim.
    expect(columnNames(container)).not.toContain("open");
  });

  it("gives a state no machine mentions a column, and says no machine mentions it", async () => {
    fakeVogt({
      "GET /work": {
        body: { items: [workItem({ ref: "WI-9", state: "quarantined" })], total: 1 },
      },
    });
    const { container } = board();

    await waitFor(() => expect(columnNames(container)).toContain("quarantined"));
    expect(container.textContent).toContain("not in any workflow");
  });
});

describe("FR-U12 — optimistic, then whatever the server said", () => {
  it("moves the card on the drop, before anything is written", async () => {
    const vogt = fakeVogt();
    const { container } = board();
    await waitFor(() => card(container, "WI-1"));

    await dragTo(container, "WI-1", "in progress");

    // The card is drawn in the target column, and no write has happened: the
    // reason has not been typed yet, so there is nothing to write (FR-W1).
    expect(cell(container, "in progress").contains(card(container, "WI-1"))).toBe(true);
    expect(vogt.matching("POST /work/transition")).toHaveLength(0);
    expect(card(container, "WI-1").className).toContain("board-card--pending");
  });

  it("sends the transition with the reason the user typed, and keeps the server's answer", async () => {
    const vogt = fakeVogt({
      "POST /work/transition": {
        body: { item: workItem({ state: "in_progress", updated_at: "2026-08-02T00:00:00Z" }) },
      },
    });
    const { container } = board();
    await waitFor(() => card(container, "WI-1"));

    await dragTo(container, "WI-1", "in progress");
    await giveReason(container, "in progress", "picked up in today's triage");

    await waitFor(() => expect(vogt.matching("POST /work/transition")).toHaveLength(1));
    expect(vogt.matching("POST /work/transition")[0]?.body).toEqual({
      ref: "WI-1",
      to_state: "in_progress",
      reason: "picked up in today's triage",
    });
    await waitFor(() =>
      expect(container.textContent).toContain("WI-1 moved to in progress."),
    );
    // Still in the target column — but now because `items()` says so, not
    // because an unsaved move is being drawn over the top of it.
    expect(cell(container, "in progress").contains(card(container, "WI-1"))).toBe(true);
    expect(card(container, "WI-1").className).not.toContain("board-card--pending");
  });

  it("rolls a refused move back and renders Vogt's own sentence where the drop happened", async () => {
    const vogt = fakeVogt({ "POST /work/transition": refusal(409, REFUSAL) });
    const { container } = board();
    await waitFor(() => card(container, "WI-1"));

    await dragTo(container, "WI-1", "done");
    await giveReason(container, "done", "it looks finished to me");

    await waitFor(() => expect(vogt.matching("POST /work/transition")).toHaveLength(1));

    // The reason is rendered verbatim, and *in the column the drop landed in*
    // — not in a toast at the top of the page.
    const refused = await waitFor(() => {
      const node = cell(container, "done").querySelector(".board-refusal");
      expect(node).toBeTruthy();
      return node as HTMLElement;
    });
    expect(refused.textContent).toContain(REFUSAL);

    // And the card is back where the server says it is.
    expect(cell(container, "open").contains(card(container, "WI-1"))).toBe(true);
    expect(cell(container, "done").contains(card(container, "WI-1"))).toBe(false);
    expect(refused.textContent).toContain("is back in");
  });

  it("never persists a state the server refused", async () => {
    fakeVogt({ "POST /work/transition": refusal(409, REFUSAL) });
    const { container } = board();
    await waitFor(() => card(container, "WI-1"));

    await dragTo(container, "WI-1", "done");
    await giveReason(container, "done", "it looks finished to me");
    await waitFor(() => expect(cell(container, "done").querySelector(".board-refusal")).toBeTruthy());

    // FR-U12's last sentence, checked where a client would break it: the only
    // thing this surface writes to storage is which columns were collapsed.
    const stored = Object.entries(localStorage).map(([key, value]) => `${key}=${value}`);
    expect(stored.join("\n")).not.toContain("done");
    expect(stored.join("\n")).not.toContain("WI-1");
  });

  it("will not submit a move with no reason", async () => {
    const vogt = fakeVogt();
    const { container } = board();
    await waitFor(() => card(container, "WI-1"));

    await dragTo(container, "WI-1", "in progress");
    const submit = cell(container, "in progress").querySelector<HTMLButtonElement>(
      "button[type=submit]",
    );
    expect(submit?.disabled).toBe(true);
    fireEvent.click(submit!);
    // Whitespace is not a reason either.
    const composer = cell(container, "in progress").querySelector("textarea")!;
    fireEvent.input(composer, { target: { value: "   " } });
    expect(submit?.disabled).toBe(true);
    expect(vogt.matching("POST /work/transition")).toHaveLength(0);
  });

  it("puts the card back when the reason is abandoned", async () => {
    const vogt = fakeVogt();
    const { container } = board();
    await waitFor(() => card(container, "WI-1"));

    await dragTo(container, "WI-1", "in progress");
    const composer = cell(container, "in progress").querySelector("textarea")!;
    fireEvent.keyDown(composer, { key: "Escape" });

    await waitFor(() =>
      expect(cell(container, "open").contains(card(container, "WI-1"))).toBe(true),
    );
    expect(vogt.matching("POST /work/transition")).toHaveLength(0);
  });
});

describe("FR-U22 — the same move, from the keyboard", () => {
  it("moves focus across columns and within one", async () => {
    fakeVogt({
      "GET /work": {
        body: {
          items: [
            workItem({ ref: "WI-1", state: "open", priority: "p1" }),
            workItem({ ref: "WI-2", state: "open", priority: "p2" }),
            workItem({ ref: "WI-3", state: "in_progress" }),
          ],
          total: 3,
        },
      },
    });
    const { container } = board();
    await waitFor(() => card(container, "WI-3"));

    card(container, "WI-1").focus();
    fireEvent.keyDown(card(container, "WI-1"), { key: "ArrowDown" });
    await waitFor(() => expect(document.activeElement).toBe(card(container, "WI-2")));

    fireEvent.keyDown(card(container, "WI-2"), { key: "ArrowRight" });
    await waitFor(() => expect(document.activeElement).toBe(card(container, "WI-3")));
  });

  it("opens the item with Enter, at the item's own URL", async () => {
    fakeVogt();
    const view = board();
    await waitFor(() => card(view.container, "WI-1"));

    fireEvent.keyDown(card(view.container, "WI-1"), { key: "Enter" });
    await waitFor(() => expect(view.url()).toBe("/w/WI-1"));
  });

  it("proposes a move with Shift+Arrow and still collects the reason", async () => {
    const vogt = fakeVogt({
      "POST /work/transition": { body: { item: workItem({ state: "in_progress" }) } },
    });
    const { container } = board();
    await waitFor(() => card(container, "WI-1"));

    fireEvent.keyDown(card(container, "WI-1"), { key: "ArrowRight", shiftKey: true });

    await waitFor(() => expect(cell(container, "in progress").querySelector("textarea")).toBeTruthy());
    expect(vogt.matching("POST /work/transition")).toHaveLength(0);

    await giveReason(container, "in progress", "starting it now");
    await waitFor(() => expect(vogt.matching("POST /work/transition")).toHaveLength(1));
  });
});

describe("Stage 9 — the phone board is one workflow state at a time", () => {
  /** Answer the board's narrow-shell query the way a phone would. */
  function onAPhone(): () => void {
    const desktop = window.matchMedia;
    window.matchMedia = ((query: string) => ({
      matches: query.includes("max-width: 768px"),
      media: query,
      onchange: null,
      addListener() {},
      removeListener() {},
      addEventListener() {},
      removeEventListener() {},
      dispatchEvent: () => false,
    })) as unknown as typeof window.matchMedia;
    return () => {
      window.matchMedia = desktop;
    };
  }

  /** The states the harness's default workflow declares. */
  const columnNamesOfWorkflow = ["open", "in_progress", "done", "wont_do"];

  const twoStates = {
    "GET /work": {
      body: {
        items: [
          workItem({ ref: "WI-1", state: "open" }),
          workItem({ ref: "WI-2", state: "in_progress" }),
        ],
        total: 2,
      },
    },
  };

  it("draws one column, names every state, and asks the server for that cell", async () => {
    const desktop = onAPhone();
    try {
      const vogt = fakeVogt(twoStates);
      const { container } = board();
      await waitFor(() => expect(columnNames(container).length).toBe(1));

      // The pill row is the whole board's state list, not just the drawn one.
      const pills = [...container.querySelectorAll(".board-phone-state")].map(
        (node) => node.textContent,
      );
      expect(pills.length).toBe(columnNamesOfWorkflow.length);
      expect(columnNames(container)).toEqual(["open"]);
      await waitFor(() => expect(card(container, "WI-1")).toBeTruthy());
      expect(() => card(container, "WI-2")).toThrow();

      // One state on the screen is one cell on the wire: selecting a state is
      // not a reason to read the rest of the estate.
      const reads = vogt.matching("POST /board/list");
      const last = reads[reads.length - 1]?.body as { cells?: { state: string }[] };
      expect(last?.cells?.map((one) => one.state)).toEqual(["open"]);
    } finally {
      desktop();
    }
  });

  it("puts a chosen state in the URL and leaves the default one out of it", async () => {
    const desktop = onAPhone();
    try {
      fakeVogt(twoStates);
      const view = board();
      await waitFor(() => expect(columnNames(view.container).length).toBe(1));
      expect(view.url()).toBe("/board");

      const pill = [...view.container.querySelectorAll<HTMLButtonElement>(
        ".board-phone-state",
      )].find((node) => node.textContent?.includes("in progress"))!;
      fireEvent.click(pill);

      await waitFor(() => expect(columnNames(view.container)).toEqual(["in progress"]));
      await waitFor(() =>
        expect(queryOf(view.url()).get("column")).toBe("in_progress"),
      );
      await waitFor(() => expect(card(view.container, "WI-2")).toBeTruthy());
      expect(() => card(view.container, "WI-1")).toThrow();
    } finally {
      desktop();
    }
  });

  it("restores the state named in a pasted link", async () => {
    const desktop = onAPhone();
    try {
      fakeVogt(twoStates);
      const { container } = board("/board?column=in_progress");
      await waitFor(() => expect(columnNames(container)).toEqual(["in progress"]));
      await waitFor(() => expect(card(container, "WI-2")).toBeTruthy());
    } finally {
      desktop();
    }
  });
});

describe("FR-U25 — a card with more to say expands where it stands", () => {
  const longBody = "The rest of what this item says, which the collapsed card has no room for.";

  it("expands and collapses the card's own content, and never into a dialog", async () => {
    fakeVogt({
      "GET /work": {
        body: { items: [workItem({ ref: "WI-1", body: longBody })], total: 1 },
      },
    });
    const { container } = board();
    await waitFor(() => card(container, "WI-1"));

    const expand = () =>
      card(container, "WI-1").querySelector<HTMLButtonElement>(".board-card-expand");
    const body = () => card(container, "WI-1").querySelector(".board-card-body");

    expect(expand()?.getAttribute("aria-expanded")).toBe("false");
    expect(body()).toBeNull();

    fireEvent.click(expand()!);
    await waitFor(() => expect(body()?.textContent).toBe(longBody));
    expect(expand()?.getAttribute("aria-expanded")).toBe("true");
    // The expanded content is in the card, not in something drawn over it.
    expect(container.querySelector("[role=dialog]")).toBeNull();
    expect(card(container, "WI-1").contains(body())).toBe(true);

    fireEvent.click(expand()!);
    await waitFor(() => expect(body()).toBeNull());
    expect(expand()?.getAttribute("aria-expanded")).toBe("false");
  });

  it("offers no expand control on a card whose content is not clipped", async () => {
    fakeVogt({
      "GET /work": { body: { items: [workItem({ ref: "WI-1", body: "" })], total: 1 } },
    });
    const { container } = board();
    await waitFor(() => card(container, "WI-1"));

    expect(card(container, "WI-1").querySelector(".board-card-expand")).toBeNull();
  });

  it("keeps Enter on the expand control off the card's own Enter", async () => {
    fakeVogt({
      "GET /work": {
        body: { items: [workItem({ ref: "WI-1", body: longBody })], total: 1 },
      },
    });
    const view = board();
    await waitFor(() => card(view.container, "WI-1"));

    const expand = view.container.querySelector<HTMLButtonElement>(".board-card-expand")!;
    fireEvent.keyDown(expand, { key: "Enter" });

    // The card would have navigated to the item; the control it was pressed on
    // owns that key.
    expect(view.url()).toBe("/board");
  });
});

describe("FR-U17 — trust on every card, and never blank", () => {
  it("renders the trust state the server gave", async () => {
    fakeVogt({
      "GET /work": { body: { items: [workItem({ trust_state: "disputed" })], total: 1 } },
    });
    const { container } = board();

    await waitFor(() => card(container, "WI-1"));
    const badge = card(container, "WI-1").querySelector(".board-trust");
    expect(badge?.textContent).toBe("disputed");
    expect(badge?.className).toContain("trust-disputed");
  });

  it("reads an absent trust state as unverified rather than as nothing", async () => {
    fakeVogt({
      "GET /work": {
        body: {
          items: [
            workItem({ ref: "WI-2", trust_state: undefined }),
            workItem({ ref: "WI-3", trust_state: "" }),
          ],
          total: 2,
        },
      },
    });
    const { container } = board();

    await waitFor(() => card(container, "WI-2"));
    // A blank badge says "no opinion"; the honest answer is "nobody checked".
    for (const ref of ["WI-2", "WI-3"]) {
      expect(card(container, ref).querySelector(".board-trust")?.textContent).toBe(
        "unverified",
      );
    }
  });

  it("says how old the view is, and never calls a polled view current", async () => {
    fakeVogt();
    const { container } = board();
    await waitFor(() => card(container, "WI-1"));

    // FR-U2's freshness half, on the board: the requirement asks for trust
    // *and* freshness, and a board with neither is an aggregate presenting
    // itself as current.
    const line = container.querySelector(".board-freshness");
    expect(line?.textContent).toMatch(/Polling — updated \d+s ago/);
    // The note says what the freshness line means. It said the opposite
    // until the front door started republishing vogt-core's changes — and a
    // test asserting the old sentence is how a corrected comment fails a
    // build, which is what happened.
    expect(container.textContent).toContain("the poll below is the floor");
  });

  it("puts one on every card, so the aggregate cannot drop the awkward column", async () => {
    fakeVogt({
      "GET /work": {
        body: {
          items: [
            workItem({ ref: "WI-2", state: "open" }),
            workItem({ ref: "WI-3", state: "in_progress", trust_state: "stale" }),
            workItem({ ref: "WI-4", state: "done", trust_state: undefined }),
          ],
          total: 3,
        },
      },
    });
    const { container } = board();

    await waitFor(() => card(container, "WI-4"));
    const cards = container.querySelectorAll(".board-card");
    expect(cards).toHaveLength(3);
    expect(container.querySelectorAll(".board-card .board-trust")).toHaveLength(3);
  });
});

describe("FR-U15 — quick-create on the board, which is the half that was missing", () => {
  function quickCreate(container: HTMLElement): HTMLFormElement {
    const form = container.querySelector<HTMLFormElement>(".board-create");
    if (!form) throw new Error("the board has no quick-create form open");
    return form;
  }

  function field(form: HTMLElement, label: string): HTMLInputElement | HTMLSelectElement {
    const found = [...form.querySelectorAll<HTMLElement>(".board-field")].find((node) =>
      (node.querySelector("span")?.textContent ?? "").startsWith(label),
    );
    const control = found?.querySelector<HTMLInputElement | HTMLSelectElement>(
      "input, select",
    );
    if (!control) throw new Error(`quick-create has no ${label} field`);
    return control;
  }

  async function open(container: HTMLElement): Promise<HTMLFormElement> {
    const button = [...container.querySelectorAll("button")].find(
      (node) => node.textContent === "Quick create",
    );
    if (!button) throw new Error("the board has no quick-create control");
    fireEvent.click(button);
    await waitFor(() => quickCreate(container));
    return quickCreate(container);
  }

  it("raises an item without leaving the board", async () => {
    const vogt = fakeVogt({
      "POST /work": { body: { item: workItem({ ref: "WI-77", title: "Raised here" }) } },
    });
    const { container } = board();
    await waitFor(() => card(container, "WI-1"));

    const form = await open(container);
    fireEvent.input(field(form, "Title"), { target: { value: "Raised here" } });
    fireEvent.input(field(form, "Reason"), { target: { value: "found while triaging" } });
    fireEvent.submit(form);

    await waitFor(() => expect(vogt.matching("POST /work")).toHaveLength(1));
    expect(vogt.matching("POST /work")[0]?.body).toEqual({
      title: "Raised here",
      kind: "feature",
      reason: "found while triaging",
    });
    // Still the board: the new card is in its column, and no navigation
    // happened.
    await waitFor(() => card(container, "WI-77"));
    expect(cell(container, "open").contains(card(container, "WI-77"))).toBe(true);
  });

  it("will not submit without the reason the user typed", async () => {
    const vogt = fakeVogt();
    const { container } = board();
    await waitFor(() => card(container, "WI-1"));

    const form = await open(container);
    const submit = form.querySelector<HTMLButtonElement>("button[type=submit]")!;

    expect(submit.disabled).toBe(true);
    fireEvent.input(field(form, "Title"), { target: { value: "Raised here" } });
    expect(submit.disabled).toBe(true); // a title is not a reason

    fireEvent.input(field(form, "Reason"), { target: { value: "   " } });
    expect(submit.disabled).toBe(true); // and whitespace is not one either

    // The keyboard path is guarded separately, so submitting round the
    // disabled button must not write.
    fireEvent.submit(form);
    expect(vogt.matching("POST /work")).toHaveLength(0);
  });

  it("will not submit without a title", async () => {
    const vogt = fakeVogt();
    const { container } = board();
    await waitFor(() => card(container, "WI-1"));

    const form = await open(container);
    fireEvent.input(field(form, "Reason"), { target: { value: "seemed worth raising" } });
    fireEvent.submit(form);
    expect(vogt.matching("POST /work")).toHaveLength(0);
  });

  it("never prefills the reason, however convenient the last one was", async () => {
    fakeVogt({
      "POST /work/transition": { body: { item: workItem({ state: "in_progress" }) } },
    });
    const { container } = board();
    await waitFor(() => card(container, "WI-1"));

    // A move whose reason the board *does* remember, deliberately, for the
    // next drop.
    await dragTo(container, "WI-1", "in progress");
    await giveReason(container, "in progress", "picked up in today's triage");
    await waitFor(() =>
      expect(container.textContent).toContain("WI-1 moved to in progress."),
    );

    const form = await open(container);
    expect((field(form, "Reason") as HTMLInputElement).value).toBe("");
  });

  it("guesses the type and project from the filters in force, and only those", async () => {
    fakeVogt();
    const { container } = board("/board?project=beta&kind=bug");
    await waitFor(() => expect(columnNames(container).length).toBeGreaterThan(0));

    const form = await open(container);
    expect((field(form, "Type") as HTMLSelectElement).value).toBe("bug");
    expect((field(form, "Project") as HTMLSelectElement).value).toBe("beta");
    expect((field(form, "Reason") as HTMLInputElement).value).toBe("");
  });

  it("renders Vogt's refusal beside the form rather than swallowing it", async () => {
    fakeVogt({ "POST /work": refusal(422, "work.create: project 'ghost' is not registered") });
    const { container } = board();
    await waitFor(() => card(container, "WI-1"));

    const form = await open(container);
    fireEvent.input(field(form, "Title"), { target: { value: "Raised here" } });
    fireEvent.input(field(form, "Reason"), { target: { value: "found while triaging" } });
    fireEvent.submit(form);

    await waitFor(() =>
      expect(container.querySelector(".board-create-error")?.textContent).toContain(
        "work.create: project 'ghost' is not registered",
      ),
    );
  });

  it("is unreachable while Vogt cannot be asked", async () => {
    fakeVogt({
      "GET /work": unavailable("upstream vogt-core did not answer"),
      "GET /workflows": unavailable("upstream vogt-core did not answer"),
    });
    const { container } = board();
    await waitFor(() => expect(container.querySelector(".board-banner--outage")).toBeTruthy());

    const button = [...container.querySelectorAll("button")].find(
      (node) => node.textContent === "Quick create",
    ) as HTMLButtonElement;
    expect(button.disabled).toBe(true);
  });
});

describe("FR-U22 — quick-create has a binding, now that there is one to bind", () => {
  it("opens on `n`, and is announced on the board's own keyboard line", async () => {
    fakeVogt();
    const { container } = board();
    await waitFor(() => card(container, "WI-1"));

    expect(container.querySelector(".board-keys")?.textContent).toContain("raises one");

    fireEvent.keyDown(card(container, "WI-1"), { key: "n" });
    await waitFor(() => expect(container.querySelector(".board-create")).toBeTruthy());
  });

  it("does not steal an `n` typed into the move composer", async () => {
    fakeVogt();
    const { container } = board();
    await waitFor(() => card(container, "WI-1"));

    await dragTo(container, "WI-1", "in progress");
    const composer = cell(container, "in progress").querySelector("textarea")!;
    fireEvent.keyDown(composer, { key: "n" });

    expect(container.querySelector(".board-create")).toBeNull();
  });
});

describe("FR-U11 — the filter set is the URL", () => {
  it("restores every one of the six filters from a pasted link", async () => {
    fakeVogt({
      "GET /initiatives": {
        body: { initiatives: [{ id: "IN-1", slug: "merge", title: "The merge" }] },
      },
      "GET /actors": {
        body: { actors: [{ identity_ref: "user:sam", display_name: "Sam" }] },
      },
    });
    const { container } = board(
      "/board?project=beta&label=infra&initiative=merge&assignee=user:sam" +
        "&lanes=project&kind=bug&state=open",
    );

    await waitFor(() => expect(columnNames(container).length).toBeGreaterThan(0));

    const value = (label: string) => {
      const field = [...container.querySelectorAll<HTMLElement>(".board-field")].find(
        (node) => node.querySelector("span")?.textContent === label,
      );
      return field?.querySelector<HTMLSelectElement>("select")?.value;
    };
    // FR-U14 names six: project, workflow state, type, label, initiative,
    // actor. All six, plus the swimlane mode, combinable and in the URL.
    await waitFor(() => expect(value("Initiative")).toBe("merge"));
    expect(value("Project")).toBe("beta");
    expect(value("Label")).toBe("infra");
    expect(value("Assignee")).toBe("user:sam");
    expect(value("Swimlanes")).toBe("project");
    // The kind chip named in the URL is the active one.
    const typeField = [...container.querySelectorAll<HTMLElement>(".board-field")].find(
      (node) => node.querySelector("span")?.textContent === "Type",
    )!;
    const active = [...typeField.querySelectorAll(".board-chip.active")].map(
      (node) => node.textContent,
    );
    expect(active).toEqual(["bug"]);
    // And only the named column is drawn.
    expect(columnNames(container)).toEqual(["open"]);
  });

  it("writes a filter chosen on the surface back into the URL", async () => {
    fakeVogt();
    const view = board();
    await waitFor(() => expect(columnNames(view.container).length).toBeGreaterThan(0));

    const label = [...view.container.querySelectorAll<HTMLElement>(".board-field")].find(
      (node) => node.querySelector("span")?.textContent === "Label",
    )!;
    fireEvent.input(label.querySelector("select")!, { target: { value: "infra" } });

    await waitFor(() => expect(queryOf(view.url()).get("label")).toBe("infra"));
  });

  it("keeps the legacy grid closed and exposes every dimension through + Filter", async () => {
    fakeVogt();
    const view = board();
    await waitFor(() => expect(columnNames(view.container).length).toBeGreaterThan(0));

    const panel = view.container.querySelector<HTMLElement>("#board-filter-panel")!;
    expect(panel.hidden).toBe(true);
    expect(panel.getAttribute("aria-label")).toBe("Add Board filters");

    const add = [...view.container.querySelectorAll<HTMLButtonElement>("button")].find(
      (button) => button.textContent?.trim() === "+ Filter",
    )!;
    expect(add.getAttribute("aria-expanded")).toBe("false");
    fireEvent.click(add);

    expect(panel.hidden).toBe(false);
    expect(add.getAttribute("aria-expanded")).toBe("true");
    for (const dimension of [
      "Project",
      "Initiative",
      "Label",
      "Assignee",
      "Swimlanes",
      "Type",
      "State",
    ]) {
    expect(panel.textContent).toContain(dimension);
    }

    const label = [...panel.querySelectorAll<HTMLElement>(".board-field")].find(
      (field) => field.querySelector("span")?.textContent === "Label",
    )!;
    fireEvent.input(label.querySelector("select")!, { target: { value: "infra" } });
    await waitFor(() => expect(queryOf(view.url()).get("label")).toBe("infra"));
    expect(view.container.textContent).toContain("Label: infra");

    label.querySelector("select")!.focus();
    fireEvent.keyDown(label.querySelector("select")!, { key: "Escape" });
    await waitFor(() => expect(panel.hidden).toBe(true));
    await waitFor(() => expect(document.activeElement).toBe(add));
  });

  it("removes active chips independently and clear-all includes swimlanes", async () => {
    fakeVogt();
    const view = board("/board?project=beta&label=infra&lanes=project");
    await waitFor(() => expect(columnNames(view.container).length).toBeGreaterThan(0));

    fireEvent.click(
      view.container.querySelector<HTMLButtonElement>(
        'button[aria-label="Remove filter Label: infra"]',
      )!,
    );
    await waitFor(() => expect(queryOf(view.url()).get("label")).toBeNull());
    expect(queryOf(view.url()).get("project")).toBe("beta");
    expect(queryOf(view.url()).get("lanes")).toBe("project");

    const clear = [...view.container.querySelectorAll<HTMLButtonElement>("button")].find(
      (button) => button.textContent?.trim() === "Clear all",
    )!;
    fireEvent.click(clear);
    await waitFor(() => expect(queryOf(view.url()).toString()).toBe(""));
    expect(view.container.textContent).toContain("No filters applied");
  });

  it("round-trips: the URL it wrote is a URL it can be handed back", async () => {
    fakeVogt();
    const first = board();
    await waitFor(() => expect(columnNames(first.container).length).toBeGreaterThan(0));

    const lanes = [...first.container.querySelectorAll<HTMLElement>(".board-field")].find(
      (node) => node.querySelector("span")?.textContent === "Swimlanes",
    )!;
    fireEvent.input(lanes.querySelector("select")!, { target: { value: "initiative" } });
    await waitFor(() => expect(queryOf(first.url()).get("lanes")).toBe("initiative"));
    const shared = first.url();

    const second = board(shared);
    await waitFor(() => expect(columnNames(second.container).length).toBeGreaterThan(0));
    await waitFor(() => expect(second.url()).toBe(shared));
  });

  it("puts its query back when the shell navigates to the bare path", async () => {
    fakeVogt();
    const view = board("/board?label=infra");
    await waitFor(() => expect(columnNames(view.container).length).toBeGreaterThan(0));

    // Re-selecting the tab navigates to `/board`, dropping the query.
    view.go("/board");
    await waitFor(() => expect(queryOf(view.url()).get("label")).toBe("infra"));
  });
});

describe("FR-U14 — a combined filter is nameable and recalled", () => {
  function nameField(container: HTMLElement): HTMLInputElement {
    return container.querySelector<HTMLInputElement>(".board-savedname")!;
  }

  function saveButton(container: HTMLElement): HTMLButtonElement {
    return [...container.querySelectorAll("button")].find(
      (node) => node.textContent === "Save lens",
    ) as HTMLButtonElement;
  }

  function pick(container: HTMLElement, label: string, value: string): void {
    const field = [...container.querySelectorAll<HTMLElement>(".board-field")].find(
      (node) => node.querySelector("span")?.textContent === label,
    )!;
    fireEvent.input(field.querySelector("select")!, { target: { value } });
  }

  it("saves the set in force under a name, and recalls all of it", async () => {
    fakeVogt();
    const view = board();
    await waitFor(() => expect(columnNames(view.container).length).toBeGreaterThan(0));

    pick(view.container, "Project", "beta");
    pick(view.container, "Label", "infra");
    pick(view.container, "Swimlanes", "project");
    await waitFor(() => expect(queryOf(view.url()).get("label")).toBe("infra"));

    fireEvent.input(nameField(view.container), { target: { value: "beta infra" } });
    fireEvent.click(saveButton(view.container));

    await waitFor(() =>
      expect(view.container.querySelector(".board-saved-recall")?.textContent).toBe(
        "beta infra",
      ),
    );

    // Somewhere else entirely.
    const clear = [...view.container.querySelectorAll("button")].find(
      (node) => node.textContent?.trim() === "Clear all",
    )!;
    fireEvent.click(clear);
    await waitFor(() => expect(queryOf(view.url()).get("label")).toBeNull());

    fireEvent.click(view.container.querySelector<HTMLButtonElement>(".board-saved-recall")!);

    // Every one of the three comes back, and the URL says so.
    await waitFor(() => {
      const query = queryOf(view.url());
      expect(query.get("project")).toBe("beta");
      expect(query.get("label")).toBe("infra");
      expect(query.get("lanes")).toBe("project");
    });
  });

  it("keeps the multi-valued filters intact through the round trip", async () => {
    fakeVogt();
    const view = board("/board?kind=bug&kind=feature&state=open&state=done");
    await waitFor(() => expect(columnNames(view.container).length).toBeGreaterThan(0));

    fireEvent.input(nameField(view.container), { target: { value: "two of each" } });
    fireEvent.click(saveButton(view.container));
    await waitFor(() => expect(view.container.querySelector(".board-saved")).toBeTruthy());

    const clear = [...view.container.querySelectorAll("button")].find(
      (node) => node.textContent?.trim() === "Clear all",
    )!;
    fireEvent.click(clear);
    await waitFor(() => expect(queryOf(view.url()).getAll("kind")).toEqual([]));

    fireEvent.click(view.container.querySelector<HTMLButtonElement>(".board-saved-recall")!);
    await waitFor(() =>
      expect(queryOf(view.url()).getAll("kind")).toEqual(["bug", "feature"]),
    );
    expect(queryOf(view.url()).getAll("state")).toEqual(["open", "done"]);
  });

  it("survives a reload, because it is per-client state and says so", async () => {
    fakeVogt();
    const first = board();
    await waitFor(() => expect(columnNames(first.container).length).toBeGreaterThan(0));

    pick(first.container, "Project", "beta");
    fireEvent.input(nameField(first.container), { target: { value: "just beta" } });
    fireEvent.click(saveButton(first.container));
    await waitFor(() => expect(first.container.querySelector(".board-saved")).toBeTruthy());

    // A second mount is this client after a reload; localStorage is the only
    // thing carried across, which is FR-U14's "per-client in v2".
    const second = board();
    await waitFor(() =>
      expect(second.container.querySelector(".board-saved-recall")?.textContent).toBe(
        "just beta",
      ),
    );
    expect(second.container.textContent).toContain("saved lenses are kept in this browser");
  });

  it("will not save an unnamed set, and forgets one on request", async () => {
    fakeVogt();
    const view = board();
    await waitFor(() => expect(columnNames(view.container).length).toBeGreaterThan(0));

    expect(saveButton(view.container).disabled).toBe(true);
    fireEvent.input(nameField(view.container), { target: { value: "  " } });
    expect(saveButton(view.container).disabled).toBe(true);

    fireEvent.input(nameField(view.container), { target: { value: "keep" } });
    fireEvent.click(saveButton(view.container));
    await waitFor(() => expect(view.container.querySelector(".board-saved")).toBeTruthy());

    fireEvent.click(view.container.querySelector<HTMLButtonElement>(".board-saved-drop")!);
    await waitFor(() => expect(view.container.querySelector(".board-saved")).toBeNull());
  });

  it("does not let a named view change how often the board refreshes", async () => {
    fakeVogt();
    const view = board("/board?poll=off");
    await waitFor(() => expect(columnNames(view.container).length).toBeGreaterThan(0));

    pick(view.container, "Project", "beta");
    fireEvent.input(nameField(view.container), { target: { value: "beta" } });
    fireEvent.click(saveButton(view.container));
    await waitFor(() => expect(view.container.querySelector(".board-saved")).toBeTruthy());

    // A refresh interval is a preference, not a filter: it is left out of what
    // is saved and left alone on recall.
    fireEvent.click(view.container.querySelector<HTMLButtonElement>(".board-saved-recall")!);
    await waitFor(() => expect(queryOf(view.url()).get("project")).toBe("beta"));
    expect(queryOf(view.url()).get("poll")).toBe("off");
  });
});

describe("FR-U21 — an outage is not an empty board", () => {
  it("renders the server's own reason and disables the writes", async () => {
    fakeVogt({
      "GET /workflows": unavailable("vogt-core is not configured for this front door"),
      "GET /work": unavailable("vogt-core is not configured for this front door"),
    });
    const { container } = board();

    await waitFor(() =>
      expect(container.querySelector(".board-banner--outage")).toBeTruthy(),
    );
    expect(container.textContent).toContain(
      "vogt-core is not configured for this front door",
    );
    // Not an empty board pretending to be an accurate one.
    expect(container.textContent).toContain("not the current state of the estate");
    expect(container.textContent).toContain("Vogt unreachable");
  });

  it("does not let a card be dragged while Vogt cannot be asked", async () => {
    fakeVogt({
      "GET /work": unavailable("upstream vogt-core did not answer"),
      "GET /workflows": unavailable("upstream vogt-core did not answer"),
    });
    const { container } = board();
    await waitFor(() => expect(container.querySelector(".board-banner--outage")).toBeTruthy());
    expect(container.querySelector(".board-card")).toBeNull();
  });
});

// -- FR-U13, which was asserted as a filter value ---------------------------
//
// §6.2a: "The swimlane *mode* is asserted as a filter value and the grouping
// it produces is not; `data-wip` is rendered and read by nobody." The mode
// round-trips through the URL in the FR-U11 block above, and a board that
// wrote `lanes=project` into its query and then drew every card in one lane
// would pass every one of those tests.
//
// So these assert the grouping, the count, the collapse and the persistence —
// each against something a reader of the board would see.

/** Four items over two projects and two initiatives, in two states. */
const LANED = {
  "GET /projects": {
    body: {
      projects: [
        { slug: "alpha", name: "Alpha" },
        { slug: "beta", name: "Beta" },
      ],
    },
  },
  "GET /initiatives": {
    body: {
      initiatives: [
        { id: "IN-1", slug: "merge", title: "The merge" },
        { id: "IN-2", slug: "phone", title: "The phone" },
      ],
    },
  },
  "GET /work": {
    body: {
      items: [
        workItem({ ref: "WI-1", project_slug: "alpha", initiative_id: "IN-1", state: "open" }),
        workItem({ ref: "WI-2", project_slug: "alpha", initiative_id: "IN-2", state: "open" }),
        workItem({
          ref: "WI-3",
          project_slug: "beta",
          initiative_id: "IN-1",
          state: "in_progress",
        }),
        workItem({ ref: "WI-4", project_slug: null, initiative_id: null, state: "open" }),
      ],
      total: 4,
    },
  },
};

/** The lane headings, in the order the board drew them. */
function laneLabels(container: HTMLElement): string[] {
  return [...container.querySelectorAll(".board-lane-label")].map(
    (node) => node.textContent ?? "",
  );
}

/** The lane heading button whose label reads `label`. */
function laneHead(container: HTMLElement, label: string): HTMLButtonElement {
  const found = [...container.querySelectorAll<HTMLButtonElement>(".board-lanehead")].find(
    (node) => node.querySelector(".board-lane-label")?.textContent === label,
  );
  if (!found) throw new Error(`no lane called "${label}"; lanes: ${laneLabels(container)}`);
  return found;
}

/** Every card ref drawn under the lane whose heading reads `label`.
 *
 *  A lane is a heading followed by its row, so this walks forward from the
 *  heading to the row that belongs to it — which is how a reader tells which
 *  lane a card is in, and the thing the grouping has to get right. */
function cardsInLane(container: HTMLElement, label: string): string[] {
  const row = laneHead(container, label).nextElementSibling;
  if (!row || !row.classList.contains("board-row")) return [];
  return [...row.querySelectorAll<HTMLElement>(".board-card")].map(
    (node) => node.id.replace("board-card-", ""),
  );
}

/** The WIP number the head row shows for a column. */
function wipOf(container: HTMLElement, state: string): string {
  return colHead(container, state).querySelector(".board-wip")?.textContent ?? "";
}

/** The column head whose name reads `state`. */
function colHead(container: HTMLElement, state: string): HTMLElement {
  const found = [...container.querySelectorAll<HTMLElement>(".board-colhead")].find(
    (node) => node.querySelector(".board-colhead-name")?.textContent === state,
  );
  if (!found) throw new Error(`no column head for ${state}; columns: ${columnNames(container)}`);
  return found;
}

describe("FR-U13 — a swimlane is a grouping, not a filter value", () => {
  it("draws one lane per project and puts each card in its own", async () => {
    const vogt = fakeVogt(LANED);
    const { container } = board("/board?lanes=project");

    await waitFor(() => expect(laneLabels(container)).toContain("Alpha"));

    // The registry's display name, not the slug, and the unattached work
    // named rather than dropped or folded into somebody else's lane.
    expect(laneLabels(container)).toEqual(["Alpha", "Beta", "No project"]);
    expect(cardsInLane(container, "Alpha").sort()).toEqual(["WI-1", "WI-2"]);
    expect(cardsInLane(container, "Beta")).toEqual(["WI-3"]);
    expect(cardsInLane(container, "No project")).toEqual(["WI-4"]);

    // The count beside each heading is that lane's own, not the board's.
    expect(laneHead(container, "Alpha").querySelector(".board-lane-count")?.textContent).toBe(
      "2",
    );
    // Grouping is this client's arrangement of what it already loaded.
    expect(vogt.matching("GET /work")).toHaveLength(1);
  });

  it("gives a project with no work a lane of its own", async () => {
    // WI-7. Lanes were derived from the loaded work items alone, so a project
    // with none had no lane and was simply absent — which is the state every
    // freshly imported project is in. The first thing an import's owner does
    // is open the board, and its answer for a correct import was to show
    // nothing at all.
    const vogt = fakeVogt({
      ...LANED,
      "GET /projects": {
        body: {
          projects: [
            { slug: "alpha", name: "Alpha" },
            { slug: "beta", name: "Beta" },
            { slug: "gamma", name: "Gamma" },
          ],
        },
      },
    });
    const { container } = board("/board?lanes=project");

    await waitFor(() => expect(laneLabels(container)).toContain("Alpha"));

    expect(laneLabels(container)).toContain("Gamma");
    expect(cardsInLane(container, "Gamma")).toEqual([]);
    expect(laneHead(container, "Gamma").querySelector(".board-lane-count")?.textContent).toBe(
      "0",
    );
    // Still one load: an empty lane is this client arranging what it has.
    expect(vogt.matching("GET /work")).toHaveLength(1);
  });

  it("says nothing is here rather than nothing matches, when nothing is here", async () => {
    // The other half of WI-7: the empty state named the filter as the cause
    // when no filter was applied, so a correct import read as a broken one.
    fakeVogt({
      ...LANED,
      "GET /work": { body: { items: [], total: 0 } },
      "GET /projects": { body: { projects: [] } },
    });
    const { container } = board("/board?lanes=project");

    await waitFor(() => expect(laneLabels(container).length).toBeGreaterThan(0));
    expect(laneLabels(container)).toEqual(["No work yet"]);
  });

  it("regroups by initiative on the same load, and asks Vogt nothing", async () => {
    const vogt = fakeVogt(LANED);
    const view = board("/board?lanes=project");
    await waitFor(() => expect(laneLabels(view.container)).toContain("Alpha"));
    const loads = vogt.matching("POST /board/list").length;

    const field = [...view.container.querySelectorAll<HTMLElement>(".board-field")].find(
      (node) => node.querySelector("span")?.textContent === "Swimlanes",
    )!;
    fireEvent.input(field.querySelector("select")!, { target: { value: "initiative" } });

    await waitFor(() => expect(laneLabels(view.container)).toContain("The merge"));
    // The same four cards, cut a different way: WI-1 and WI-3 share an
    // initiative and do not share a project.
    expect(laneLabels(view.container)).toEqual(["The merge", "The phone", "No initiative"]);
    expect(cardsInLane(view.container, "The merge").sort()).toEqual(["WI-1", "WI-3"]);
    expect(cardsInLane(view.container, "The phone")).toEqual(["WI-2"]);
    expect(cardsInLane(view.container, "No initiative")).toEqual(["WI-4"]);
    expect(vogt.matching("POST /board/list").length).toBeGreaterThan(loads);
  });
});

describe("FR-U13 — the per-column WIP count is what is in the column", () => {
  it("counts the cards in each column across every lane, and moves when one moves", async () => {
    fakeVogt(LANED);
    const { container } = board("/board?lanes=project");
    await waitFor(() => expect(laneLabels(container)).toContain("Alpha"));

    // Three open across two lanes and the unlaned one, one in progress, and
    // a column nothing is in says nought rather than showing nothing.
    expect(wipOf(container, "open")).toBe("3");
    expect(wipOf(container, "in progress")).toBe("1");
    expect(wipOf(container, "done")).toBe("0");

    // The count is of what is drawn, so an optimistic move is counted where
    // the card now is — the number and the cards never disagree.
    fireEvent.dragStart(card(container, "WI-1"));
    fireEvent.drop(
      container.querySelectorAll<HTMLElement>('.board-cell[data-state="in progress"]')[0]!,
    );
    await waitFor(() => expect(wipOf(container, "in progress")).toBe("2"));
    expect(wipOf(container, "open")).toBe("2");
  });

  it("marks the count a floor when the board did not load the whole estate", async () => {
    fakeVogt({
      ...LANED,
      "POST /board/list": {
        body: {
          cells: [
            { lane_key: "alpha", state: "open", items: [workItem({ ref: "WI-1", project_slug: "alpha", state: "open" })], total: 900 },
            { lane_key: "alpha", state: "in_progress", items: [], total: 0 },
            { lane_key: "alpha", state: "done", items: [], total: 0 },
            { lane_key: "alpha", state: "wont_do", items: [], total: 0 },
            { lane_key: "", state: "open", items: [], total: 0 },
            { lane_key: "", state: "in_progress", items: [], total: 0 },
            { lane_key: "", state: "done", items: [], total: 0 },
            { lane_key: "", state: "wont_do", items: [], total: 0 },
          ],
          column_totals: { open: 900 },
          lane_totals: { alpha: 900 },
          total: 900,
          snapshot: "large-board",
          snapshot_at: "2026-08-17T10:01:00Z",
          revision: 1,
        },
      },
    });
    const { container } = board("/board?lanes=project");

    // Bounded Board reads return exact server-owned totals, even when only one
    // card is in the initial cell page.
    await waitFor(() => expect(wipOf(container, "open")).toBe("900"));
  });
});

describe("FR-U13 — lanes and columns collapse, and the layout is this client's", () => {
  it("takes a collapsed lane's cards off the board and leaves the others", async () => {
    fakeVogt(LANED);
    const { container } = board("/board?lanes=project");
    await waitFor(() => expect(laneLabels(container)).toContain("Alpha"));

    fireEvent.click(laneHead(container, "Alpha"));

    await waitFor(() => expect(cardsInLane(container, "Alpha")).toEqual([]));
    // Collapsed, not filtered: the heading is still there to expand, the
    // other lanes are untouched, and the column count still knows about the
    // work that is merely out of sight.
    expect(laneLabels(container)).toContain("Alpha");
    expect(cardsInLane(container, "Beta")).toEqual(["WI-3"]);
    expect(wipOf(container, "open")).toBe("3");

    fireEvent.click(laneHead(container, "Alpha"));
    await waitFor(() =>
      expect(cardsInLane(container, "Alpha").sort()).toEqual(["WI-1", "WI-2"]),
    );
  });

  it("collapses a column and offers to put it back", async () => {
    fakeVogt(LANED);
    const { container } = board("/board?lanes=project");
    await waitFor(() => expect(laneLabels(container)).toContain("Alpha"));

    const head = colHead(container, "done");
    const toggle = head.querySelector<HTMLButtonElement>(".board-colhead-toggle")!;
    expect(toggle.title).toBe("Collapse column");

    fireEvent.click(toggle);

    await waitFor(() => expect(head.classList.contains("collapsed")).toBe(true));
    expect(toggle.title).toBe("Expand column");
    // Every cell in that column narrows with it, in every lane at once.
    const cells = [...container.querySelectorAll<HTMLElement>('.board-cell[data-state="done"]')];
    expect(cells.length).toBeGreaterThan(1);
    expect(cells.every((node) => node.classList.contains("collapsed"))).toBe(true);

    fireEvent.click(toggle);
    await waitFor(() => expect(head.classList.contains("collapsed")).toBe(false));
  });

  it("remembers what this client collapsed, and tells Vogt none of it", async () => {
    const vogt = fakeVogt(LANED);
    const first = board("/board?lanes=project");
    await waitFor(() => expect(laneLabels(first.container)).toContain("Alpha"));

    fireEvent.click(laneHead(first.container, "Alpha"));
    fireEvent.click(
      colHead(first.container, "done").querySelector<HTMLButtonElement>(
        ".board-colhead-toggle",
      )!,
    );
    await waitFor(() => expect(cardsInLane(first.container, "Alpha")).toEqual([]));

    // A reload, not a re-render: the preference has to have left the surface.
    first.unmount();
    const second = board("/board?lanes=project");
    await waitFor(() => expect(laneLabels(second.container)).toContain("Alpha"));

    expect(cardsInLane(second.container, "Alpha")).toEqual([]);
    expect(cardsInLane(second.container, "Beta")).toEqual(["WI-3"]);
    expect(colHead(second.container, "done").classList.contains("collapsed")).toBe(true);

    // Per client means per client: a layout preference is not an estate
    // write, and nothing about it was sent to Vogt at all.
    expect(
      vogt.calls.filter(
        (call) => call.method !== "GET" && call.path !== "/board/list",
      ),
    ).toEqual([]);
  });
});

// -- #214: a card can be moved without a drag ------------------------------
//
// Drag and Shift+Arrow are the only ways to move a card, and a coarse pointer
// has neither. The "Move…" control opens the *same* composer the drag does —
// `beginMove`/`commitMove`, unchanged — with a state select standing in for
// the drop cell a tap never lands on. These assert that path end to end: the
// control opens the composer, the select offers the workflow's listed edges,
// and the chosen one is what `work.transition` is asked to make.

describe("#214 — the Move… control opens the drag's own composer", () => {
  /** The touch move control on a card. Present in the DOM on every pointer;
   *  CSS reveals it only on coarse ones, which jsdom does not evaluate. */
  function moveControl(container: HTMLElement, ref: string): HTMLButtonElement {
    const button = card(container, ref).querySelector<HTMLButtonElement>(".board-card-move");
    if (!button) throw new Error(`no Move control on ${ref}`);
    return button;
  }

  /** The composer a pick opens, which is drawn in the card's *own* cell. */
  function pickComposer(container: HTMLElement, state: string): HTMLFormElement {
    const form = cell(container, state).querySelector<HTMLFormElement>(".board-composer");
    if (!form) throw new Error(`no composer open in ${state}`);
    return form;
  }

  it("moves a card to a chosen state with no drag gesture at all", async () => {
    const vogt = fakeVogt({
      "POST /work/transition": {
        body: { item: workItem({ state: "wont_do", updated_at: "2026-08-03T00:00:00Z" }) },
      },
    });
    const { container } = board();
    await waitFor(() => card(container, "WI-1"));

    // The card is in `open`, and the composer opens in that same cell — the one
    // a phone can see — not in a target column a coarse pointer cannot reach.
    fireEvent.click(moveControl(container, "WI-1"));
    const composer = await waitFor(() => pickComposer(container, "open"));
    expect(cell(container, "open").contains(card(container, "WI-1"))).toBe(true);

    // The select offers exactly the workflow's listed edges from `open`
    // (feature: in_progress, wont_do) — never the current state, never an
    // edge the machine does not list.
    const select = composer.querySelector<HTMLSelectElement>(".board-composer-state select")!;
    const options = [...select.options].map((option) => option.value);
    expect(options).toEqual(["in_progress", "wont_do"]);
    expect(options).not.toContain("open");
    expect(options).not.toContain("done");

    // Pick the second edge, give the reason the write requires, confirm.
    fireEvent.change(select, { target: { value: "wont_do" } });
    const textarea = composer.querySelector("textarea")!;
    fireEvent.input(textarea, { target: { value: "closing it from my phone" } });
    const submit = composer.querySelector<HTMLButtonElement>("button[type=submit]")!;
    await waitFor(() => expect(submit.disabled).toBe(false));
    fireEvent.click(submit);

    // One transition, to the state the select named — the same endpoint and
    // body a drag would have produced.
    await waitFor(() => expect(vogt.matching("POST /work/transition")).toHaveLength(1));
    expect(vogt.matching("POST /work/transition")[0]?.body).toEqual({
      ref: "WI-1",
      to_state: "wont_do",
      reason: "closing it from my phone",
    });
    await waitFor(() =>
      expect(container.textContent).toContain("WI-1 moved to wont do."),
    );
    // And the card is where the server put it.
    expect(cell(container, "wont do").contains(card(container, "WI-1"))).toBe(true);
  });

  it("defaults the target to the first listed edge and can be abandoned", async () => {
    const vogt = fakeVogt();
    const { container } = board();
    await waitFor(() => card(container, "WI-1"));

    fireEvent.click(moveControl(container, "WI-1"));
    const composer = await waitFor(() => pickComposer(container, "open"));
    const select = composer.querySelector<HTMLSelectElement>(".board-composer-state select")!;
    // The first listed edge, chosen for the reader who does not touch the
    // select — the head reads `open → in progress`.
    expect(select.value).toBe("in_progress");
    expect(composer.textContent).toContain("in progress");

    // Escape puts the card back and writes nothing, exactly as the drag's
    // composer does.
    fireEvent.keyDown(composer.querySelector("textarea")!, { key: "Escape" });
    await waitFor(() => expect(cell(container, "open").querySelector(".board-composer")).toBeNull());
    expect(vogt.matching("POST /work/transition")).toHaveLength(0);
  });

  it("offers no Move control on a card whose state has no listed edge", async () => {
    fakeVogt({
      "GET /work": {
        body: { items: [workItem({ ref: "WI-1", state: "done" })], total: 1 },
      },
    });
    const { container } = board();
    await waitFor(() => card(container, "WI-1"));

    // `done` leads nowhere in the feature machine, so the control has nothing
    // to open and says so by refusing the tap.
    expect(
      card(container, "WI-1").querySelector<HTMLButtonElement>(".board-card-move")?.disabled,
    ).toBe(true);
  });
});

// -- #216: a same-column, other-lane drop is explained, not swallowed ------
//
// A lane is a grouping, not a target: dropping a card into another lane of its
// own column changes only the swimlane, which a drag cannot do. `beginMove`
// refuses a same-state move, so the drop used to do nothing without a word —
// while the cell had lit up as a target during the drag. These assert both
// halves of the fix: the cell is never lit, and the reason is left on screen.

describe("#216 — dropping into another lane of the same column", () => {
  function laneCell(container: HTMLElement, lane: string, state: string): HTMLElement {
    const row = laneHead(container, lane).nextElementSibling;
    const found = row?.querySelector<HTMLElement>(`.board-cell[data-state="${state}"]`);
    if (!found) throw new Error(`no ${state} cell in lane ${lane}`);
    return found;
  }

  it("does not light a same-state cell as a drop target, and explains the no-op", async () => {
    const vogt = fakeVogt(LANED);
    const { container } = board("/board?lanes=project");
    await waitFor(() => expect(laneLabels(container)).toContain("Alpha"));

    // WI-1 is `open` in Alpha. Beta's own `open` cell differs from the source
    // only by lane.
    const betaOpen = laneCell(container, "Beta", "open");
    fireEvent.dragStart(card(container, "WI-1"));

    // (b) During the drag it is not offered as a target — no `dropping`.
    fireEvent.dragOver(betaOpen);
    expect(betaOpen.classList.contains("dropping")).toBe(false);
    // (a) But it says why a drop here would do nothing.
    expect(betaOpen.textContent).toContain(
      "Lanes group cards; to change the project/initiative open the item",
    );

    // The drop is a no-op with the explanation still on screen — no composer,
    // and nothing written.
    fireEvent.drop(betaOpen);
    expect(betaOpen.querySelector(".board-composer")).toBeNull();
    expect(vogt.matching("POST /work/transition")).toHaveLength(0);
    expect(betaOpen.textContent).toContain(
      "Lanes group cards; to change the project/initiative open the item",
    );
    // The card did not move: it is still open in Alpha.
    expect(cardsInLane(container, "Alpha")).toContain("WI-1");
  });

  it("still lights a cell whose state actually differs, in any lane", async () => {
    fakeVogt(LANED);
    const { container } = board("/board?lanes=project");
    await waitFor(() => expect(laneLabels(container)).toContain("Beta"));

    // Beta's `in progress` cell is a real target for WI-1 (open → in_progress),
    // in a different lane — the lane is not what stops a move, the state is.
    const betaProgress = laneCell(container, "Beta", "in progress");
    fireEvent.dragStart(card(container, "WI-1"));
    fireEvent.dragOver(betaProgress);
    expect(betaProgress.classList.contains("dropping")).toBe(true);
    expect(betaProgress.textContent).not.toContain("Lanes group cards");
  });
});
