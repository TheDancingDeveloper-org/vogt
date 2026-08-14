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

describe("FR-U11 — the filter set is the URL", () => {
  it("restores every filter from a pasted link", async () => {
    fakeVogt();
    const { container } = board(
      "/board?project=beta&label=infra&lanes=project&kind=bug&state=open",
    );

    await waitFor(() => expect(columnNames(container).length).toBeGreaterThan(0));

    const value = (label: string) => {
      const field = [...container.querySelectorAll<HTMLElement>(".board-field")].find(
        (node) => node.querySelector("span")?.textContent === label,
      );
      return field?.querySelector<HTMLSelectElement>("select")?.value;
    };
    expect(value("Project")).toBe("beta");
    expect(value("Label")).toBe("infra");
    expect(value("Swimlanes")).toBe("project");
    // The kind chip named in the URL is the active one.
    const active = [...container.querySelectorAll(".board-chip.active")].map(
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
