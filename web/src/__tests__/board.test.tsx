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

describe("FR-U14 — a combined filter is nameable and recalled", () => {
  function nameField(container: HTMLElement): HTMLInputElement {
    return container.querySelector<HTMLInputElement>(".board-savedname")!;
  }

  function saveButton(container: HTMLElement): HTMLButtonElement {
    return [...container.querySelectorAll("button")].find(
      (node) => node.textContent === "Save filter",
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
    const clear = [...view.container.querySelectorAll("button")].find((node) =>
      node.textContent?.startsWith("Clear filters"),
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

    const clear = [...view.container.querySelectorAll("button")].find((node) =>
      node.textContent?.startsWith("Clear filters"),
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
    expect(second.container.textContent).toContain("saved filters are kept in this browser");
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
