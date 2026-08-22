// The ranked views: the write rules, the filter URL, and the absent state
// (FR-U6, FR-U11, FR-U14, FR-U15, FR-U17, FR-U21).
//
// The write rules first, because they are the cheapest thing to assert and
// the thing most likely to erode: `test_pwa.py` can prove that no *exported*
// write can be called without a reason, and cannot prove that the form
// refuses to submit without one. That gap is what these fill.

import { describe, expect, it } from "vitest";
import { fireEvent, waitFor } from "@solidjs/testing-library";
import Backlog from "../Backlog";
import {
  fakeVogt,
  freshness,
  mountAt,
  queryOf,
  rankedEntry,
  refusal,
  unavailable,
  workItem,
} from "./harness";

function backlog(url = "/backlog") {
  return mountAt("/backlog", url, () => <Backlog />);
}

function rows(container: HTMLElement): HTMLElement[] {
  return [...container.querySelectorAll<HTMLElement>(".vogt-backlog-row")];
}

function field(scope: HTMLElement, label: string): HTMLInputElement | HTMLSelectElement {
  const found = [...scope.querySelectorAll<HTMLElement>(".vogt-backlog-field")].find(
    (node) => (node.querySelector("span")?.textContent ?? "").startsWith(label),
  );
  const control = found?.querySelector<HTMLInputElement | HTMLSelectElement>(
    "input, select",
  );
  if (!control) throw new Error(`no ${label} field here`);
  return control;
}

function button(scope: HTMLElement, text: string): HTMLButtonElement {
  const found = [...scope.querySelectorAll("button")].find(
    (node) => node.textContent === text,
  );
  if (!found) throw new Error(`no "${text}" button here`);
  return found as HTMLButtonElement;
}

/** Tick every declared row's checkbox. */
async function selectAll(container: HTMLElement): Promise<void> {
  const head = container.querySelector<HTMLInputElement>(
    '.vogt-backlog-headrow input[type="checkbox"]',
  )!;
  fireEvent.click(head);
  await waitFor(() =>
    expect(container.querySelector(".vogt-backlog-bulk")).toBeTruthy(),
  );
}

function bulkFormFor(container: HTMLElement, submitText: string): HTMLFormElement {
  const found = [...container.querySelectorAll<HTMLFormElement>(".vogt-backlog-bulk-row")].find(
    (form) =>
      [...form.querySelectorAll("button")].some((node) => node.textContent === submitText),
  );
  if (!found) throw new Error(`no bulk form with a "${submitText}" button`);
  return found;
}

const TWO = {
  "GET /backlog": {
    body: {
      items: [
        rankedEntry({ ref: "WI-1", labels: ["infra"] }),
        rankedEntry({ ref: "WI-2", title: "Another", state: "in_progress", labels: [] }),
      ],
      freshness: freshness(),
    },
  },
};

describe("FR-U6 — bulk label, which was the half of the clause nothing called", () => {
  it("writes `work.update` per item with the reason typed for the batch", async () => {
    const vogt = fakeVogt({
      ...TWO,
      "POST /work/update": { body: { item: workItem(), comments: [], sessions: [] } },
    });
    const { container } = backlog();
    await waitFor(() => expect(rows(container)).toHaveLength(2));

    await selectAll(container);
    const form = bulkFormFor(container, "Add label");
    fireEvent.input(field(form, "To apply"), { target: { value: "docs" } });
    fireEvent.input(field(form, "Reason"), { target: { value: "sorting the docs sweep" } });
    fireEvent.submit(form);

    await waitFor(() => expect(vogt.matching("POST /work/update")).toHaveLength(2));
    // One audited write per item, each carrying the batch's reason — the same
    // shape the bulk transition has, which is what FR-U6 asks for.
    expect(vogt.matching("POST /work/update").map((call) => call.body)).toEqual([
      { ref: "WI-1", reason: "sorting the docs sweep", add_labels: ["docs"] },
      { ref: "WI-2", reason: "sorting the docs sweep", add_labels: ["docs"] },
    ]);
  });

  it("takes a label off with `remove_labels`, offering only what the batch carries", async () => {
    const vogt = fakeVogt({
      ...TWO,
      "POST /work/update": { body: { item: workItem(), comments: [], sessions: [] } },
    });
    const { container } = backlog();
    await waitFor(() => expect(rows(container)).toHaveLength(2));

    await selectAll(container);
    const form = bulkFormFor(container, "Add label");
    fireEvent.input(field(form, "Label"), { target: { value: "remove" } });

    const removable = bulkFormFor(container, "Remove label");
    const options = [
      ...field(removable, "To take off").querySelectorAll("option"),
    ].map((node) => (node as HTMLOptionElement).value);
    // `docs` exists in the vocabulary and on nothing selected, so removing it
    // would be a write that changes nothing.
    expect(options).toEqual(["", "infra"]);

    fireEvent.input(field(removable, "To take off"), { target: { value: "infra" } });
    fireEvent.input(field(removable, "Reason"), { target: { value: "wrong label" } });
    fireEvent.submit(removable);

    await waitFor(() => expect(vogt.matching("POST /work/update")).toHaveLength(2));
    expect(vogt.matching("POST /work/update")[0]?.body).toEqual({
      ref: "WI-1",
      reason: "wrong label",
      remove_labels: ["infra"],
    });
  });

  it("will not label without a reason, or without a label", async () => {
    const vogt = fakeVogt(TWO);
    const { container } = backlog();
    await waitFor(() => expect(rows(container)).toHaveLength(2));

    await selectAll(container);
    const form = bulkFormFor(container, "Add label");
    const submit = button(form, "Add label");

    expect(submit.disabled).toBe(true);
    fireEvent.input(field(form, "To apply"), { target: { value: "docs" } });
    expect(submit.disabled).toBe(true); // a label is not a reason

    fireEvent.input(field(form, "Reason"), { target: { value: "  " } });
    expect(submit.disabled).toBe(true);

    fireEvent.submit(form);
    expect(vogt.matching("POST /work/update")).toHaveLength(0);
  });

  it("does not let a reason typed for a transition justify a labelling", async () => {
    fakeVogt(TWO);
    const { container } = backlog();
    await waitFor(() => expect(rows(container)).toHaveLength(2));

    await selectAll(container);
    const transition = bulkFormFor(container, "Transition");
    fireEvent.input(field(transition, "Reason"), {
      target: { value: "closing out the sprint" },
    });

    // Two acts, two forms, two reasons: the labelling's field is still empty
    // and its button is still refused.
    const labelling = bulkFormFor(container, "Add label");
    expect((field(labelling, "Reason") as HTMLInputElement).value).toBe("");
    fireEvent.input(field(labelling, "To apply"), { target: { value: "docs" } });
    expect(button(labelling, "Add label").disabled).toBe(true);
  });

  it("reports a partial batch as a partial batch, in Vogt's words", async () => {
    const vogt = fakeVogt({
      ...TWO,
      "POST /work/update": (call) =>
        call.body?.ref === "WI-2"
          ? refusal(422, "work.update: label 'docs' is not defined")
          : { body: { item: workItem(), comments: [], sessions: [] } },
    });
    const { container } = backlog();
    await waitFor(() => expect(rows(container)).toHaveLength(2));

    await selectAll(container);
    const form = bulkFormFor(container, "Add label");
    fireEvent.input(field(form, "To apply"), { target: { value: "docs" } });
    fireEvent.input(field(form, "Reason"), { target: { value: "sorting the docs sweep" } });
    fireEvent.submit(form);

    await waitFor(() => expect(vogt.matching("POST /work/update")).toHaveLength(2));
    const outcomes = await waitFor(() => {
      const list = container.querySelector(".vogt-backlog-outcomes");
      expect(list?.querySelectorAll("li")).toHaveLength(2);
      return list as HTMLElement;
    });
    expect(outcomes.querySelector("li.ok")?.textContent).toContain("WI-1");
    expect(outcomes.querySelector("li.failed")?.textContent).toContain(
      "work.update: label 'docs' is not defined",
    );
  });

  it("clears the reason afterwards, so the next batch cannot inherit it", async () => {
    fakeVogt({
      ...TWO,
      "POST /work/update": { body: { item: workItem(), comments: [], sessions: [] } },
    });
    const { container } = backlog();
    await waitFor(() => expect(rows(container)).toHaveLength(2));

    await selectAll(container);
    const form = bulkFormFor(container, "Add label");
    fireEvent.input(field(form, "To apply"), { target: { value: "docs" } });
    fireEvent.input(field(form, "Reason"), { target: { value: "sorting the docs sweep" } });
    fireEvent.submit(form);

    await waitFor(() =>
      expect((field(form, "Reason") as HTMLInputElement).value).toBe(""),
    );
  });
});

describe("FR-U6 — bulk transition, which was built and asserted by nothing", () => {
  it("transitions every selected item with the batch's reason", async () => {
    const vogt = fakeVogt({
      ...TWO,
      "POST /work/transition": { body: { item: workItem(), comments: [], sessions: [] } },
    });
    const { container } = backlog();
    await waitFor(() => expect(rows(container)).toHaveLength(2));

    await selectAll(container);
    const form = bulkFormFor(container, "Transition");
    fireEvent.input(field(form, "Transition to"), { target: { value: "done" } });
    fireEvent.input(field(form, "Reason"), { target: { value: "closing out the sprint" } });
    fireEvent.submit(form);

    await waitFor(() => expect(vogt.matching("POST /work/transition")).toHaveLength(2));
    expect(vogt.matching("POST /work/transition").map((call) => call.body)).toEqual([
      { ref: "WI-1", to_state: "done", reason: "closing out the sprint" },
      { ref: "WI-2", to_state: "done", reason: "closing out the sprint" },
    ]);
  });

  it("will not transition without a target state or a reason", async () => {
    const vogt = fakeVogt(TWO);
    const { container } = backlog();
    await waitFor(() => expect(rows(container)).toHaveLength(2));

    await selectAll(container);
    const form = bulkFormFor(container, "Transition");
    const submit = button(form, "Transition");

    expect(submit.disabled).toBe(true);
    fireEvent.input(field(form, "Transition to"), { target: { value: "done" } });
    expect(submit.disabled).toBe(true);
    fireEvent.input(field(form, "Reason"), { target: { value: " " } });
    expect(submit.disabled).toBe(true);

    fireEvent.submit(form);
    expect(vogt.matching("POST /work/transition")).toHaveLength(0);
  });

  it("leaves the refused items selected and the rest not", async () => {
    fakeVogt({
      ...TWO,
      "POST /work/transition": (call) =>
        call.body?.ref === "WI-2"
          ? refusal(409, "transition.not_allowed: feature has no open -> done edge")
          : { body: { item: workItem(), comments: [], sessions: [] } },
    });
    const { container } = backlog();
    await waitFor(() => expect(rows(container)).toHaveLength(2));

    await selectAll(container);
    const form = bulkFormFor(container, "Transition");
    fireEvent.input(field(form, "Transition to"), { target: { value: "done" } });
    fireEvent.input(field(form, "Reason"), { target: { value: "closing out the sprint" } });
    fireEvent.submit(form);

    // A partial batch reads as a partial batch: the refusal is quoted, and
    // the item it belongs to is the one still selected to try again.
    await waitFor(() =>
      expect(container.querySelector(".vogt-backlog-outcomes li.failed")?.textContent).toContain(
        "transition.not_allowed",
      ),
    );
    await waitFor(() =>
      expect(container.querySelector(".vogt-backlog-bulk strong")?.textContent).toBe(
        "1 selected",
      ),
    );
  });
});

describe("FR-U6 / FR-U15 — quick-create refuses to submit without a typed reason", () => {
  it("takes the title, type, project and reason, and sends only those", async () => {
    const vogt = fakeVogt({
      "POST /work": { body: { item: workItem({ ref: "WI-42" }) } },
    });
    const { container } = backlog();
    await waitFor(() => expect(rows(container)).toHaveLength(1));

    fireEvent.click(button(container, "Quick create"));
    const form = await waitFor(() => {
      const node = container.querySelector<HTMLFormElement>(".vogt-backlog-create");
      expect(node).toBeTruthy();
      return node!;
    });

    fireEvent.input(field(form, "Title"), { target: { value: "Something to do" } });
    fireEvent.input(field(form, "Reason"), { target: { value: "raised in review" } });
    fireEvent.submit(form);

    await waitFor(() => expect(vogt.matching("POST /work")).toHaveLength(1));
    expect(vogt.matching("POST /work")[0]?.body).toEqual({
      title: "Something to do",
      kind: "feature",
      reason: "raised in review",
    });
  });

  it("stays refused until a reason is typed", async () => {
    const vogt = fakeVogt();
    const { container } = backlog();
    await waitFor(() => expect(rows(container)).toHaveLength(1));

    fireEvent.click(button(container, "Quick create"));
    const form = await waitFor(() => {
      const node = container.querySelector<HTMLFormElement>(".vogt-backlog-create");
      expect(node).toBeTruthy();
      return node!;
    });
    const submit = button(form, "Create");

    expect(submit.disabled).toBe(true);
    fireEvent.input(field(form, "Title"), { target: { value: "Something to do" } });
    expect(submit.disabled).toBe(true);
    fireEvent.input(field(form, "Reason"), { target: { value: "\t " } });
    expect(submit.disabled).toBe(true);

    fireEvent.submit(form);
    expect(vogt.matching("POST /work")).toHaveLength(0);
  });
});

describe("#226 — the State filter is page-only, says so, and keeps the selection", () => {
  const TWO_STATES = {
    "GET /backlog": {
      body: {
        items: [
          rankedEntry({ ref: "WI-1", state: "open" }),
          rankedEntry({ ref: "WI-2", title: "Second", state: "in_progress" }),
        ],
        freshness: freshness(),
      },
    },
  };

  /** A State chip, disambiguated from the Type chips by its state name. */
  function stateChip(container: HTMLElement, name: string): HTMLButtonElement {
    const found = [...container.querySelectorAll<HTMLButtonElement>(".vogt-backlog-chip")].find(
      (node) => node.textContent === name,
    );
    if (!found) throw new Error(`no state chip "${name}"`);
    return found;
  }

  it("suffixes the State chip with 'this page only' and counts N of M loaded rows", async () => {
    fakeVogt(TWO_STATES);
    const { container } = backlog();
    await waitFor(() => expect(rows(container)).toHaveLength(2));

    fireEvent.click(stateChip(container, "open"));
    await waitFor(() => expect(rows(container)).toHaveLength(1));

    // The count states the filter narrowed the loaded page rather than the
    // estate: one of the two rows this page loaded.
    expect(container.querySelector(".vogt-backlog-count")?.textContent).toContain(
      "1 of 2 loaded rows",
    );

    // The chip carries the same honesty the summary does.
    const chip = [...container.querySelectorAll(".vogt-filter-chip")].find((node) =>
      node.textContent?.includes("State:"),
    );
    expect(chip?.textContent).toContain("this page only");
  });

  it("keeps a selection across a State-chip change rather than dropping it silently", async () => {
    fakeVogt(TWO_STATES);
    const { container } = backlog();
    await waitFor(() => expect(rows(container)).toHaveLength(2));

    await selectAll(container);
    expect(container.querySelector(".vogt-backlog-bulk strong")?.textContent).toBe(
      "2 selected",
    );

    // Narrowing to one state hides the other row but must not deselect it: the
    // ref is still on the loaded page, only filtered from view.
    fireEvent.click(stateChip(container, "open"));
    await waitFor(() => expect(rows(container)).toHaveLength(1));
    expect(container.querySelector(".vogt-backlog-bulk strong")?.textContent).toBe(
      "2 selected",
    );
  });
});

describe("FR-U17 — trust on every ranked row, and never blank", () => {
  it("reads an absent trust state as unverified", async () => {
    fakeVogt({
      "GET /backlog": {
        body: {
          items: [
            rankedEntry({ ref: "WI-1", trust_state: undefined }),
            rankedEntry({ ref: "WI-2", trust_state: "stale" }),
          ],
          freshness: freshness(),
        },
      },
    });
    const { container } = backlog();
    await waitFor(() => expect(rows(container)).toHaveLength(2));

    const badges = [...container.querySelectorAll(".vogt-backlog-trust")].map(
      (node) => node.textContent,
    );
    expect(badges).toEqual(["unverified", "stale"]);
  });

  it("says nothing has been swept rather than showing an empty list as an answer", async () => {
    fakeVogt({
      "GET /backlog": {
        body: { items: [], freshness: freshness({ status: "never_swept" }) },
      },
    });
    const { container } = backlog();

    await waitFor(() =>
      expect(container.querySelector(".vogt-backlog-freshness")?.textContent).toContain(
        "nothing has been swept yet",
      ),
    );
    expect(container.textContent).toContain(
      "this is 'not collected', not 'nothing found'",
    );
  });
});

describe("FR-U11 / FR-U14 — the filter set is the URL, and can be named", () => {
  it("restores a pasted filter set and keeps it on the surface", async () => {
    fakeVogt();
    const view = backlog("/backlog?view=bugs&project=beta&label=infra&limit=50");
    await waitFor(() =>
      expect(
        view.container.querySelector(".vogt-backlog-viewtab.active")?.textContent,
      ).toBe("Bugs"),
    );
    expect((field(view.container, "Project") as HTMLSelectElement).value).toBe("beta");
    expect((field(view.container, "Label") as HTMLSelectElement).value).toBe("infra");
    expect((field(view.container, "Page size") as HTMLSelectElement).value).toBe("50");
  });

  it("round-trips a filter chosen on the surface through the URL", async () => {
    fakeVogt();
    const view = backlog();
    await waitFor(() => expect(rows(view.container)).toHaveLength(1));

    fireEvent.input(field(view.container, "Project"), { target: { value: "beta" } });
    await waitFor(() => expect(queryOf(view.url()).get("project")).toBe("beta"));

    const second = backlog(view.url());
    await waitFor(() =>
      expect((field(second.container, "Project") as HTMLSelectElement).value).toBe("beta"),
    );
  });

  it("saves a named filter set and recalls it", async () => {
    fakeVogt();
    const view = backlog();
    await waitFor(() => expect(rows(view.container)).toHaveLength(1));

    fireEvent.input(field(view.container, "Project"), { target: { value: "beta" } });
    await waitFor(() => expect(queryOf(view.url()).get("project")).toBe("beta"));

    const name = view.container.querySelector<HTMLInputElement>(
      '.vogt-backlog-savedrow input[type="text"]',
    )!;
    fireEvent.input(name, { target: { value: "beta only" } });
    fireEvent.click(button(view.container, "Save lens"));
    await waitFor(() =>
      expect(view.container.querySelector(".vogt-backlog-saved-recall")?.textContent).toBe(
        "beta only",
      ),
    );

    fireEvent.click(button(view.container, "Clear all"));
    await waitFor(() => expect(queryOf(view.url()).get("project")).toBeNull());

    fireEvent.click(
      view.container.querySelector<HTMLButtonElement>(".vogt-backlog-saved-recall")!,
    );
    await waitFor(() => expect(queryOf(view.url()).get("project")).toBe("beta"));
  });
});

describe("FR-U25 — a ranked row is content-sized and expands where it stands", () => {
  const twoRows = {
    "GET /backlog": {
      body: {
        items: [
          rankedEntry({ ref: "WI-1", title: "A declared row", score: 4.25 }),
          rankedEntry({
            ref: "gh:alpha#12",
            origin: "observed",
            title: "An observed subject",
            state: "observed",
            observation_kind: "forge issue",
            observed_at: "2026-08-01T00:00:00Z",
            source_url: "https://example.invalid/12",
            score: 1.5,
          }),
        ],
        total_considered: 2,
      },
    },
  };

  it("keeps rank, ref, trust, age and score on the collapsed row", async () => {
    fakeVogt(twoRows);
    const { container } = backlog();
    await waitFor(() => expect(rows(container)).toHaveLength(2));

    const first = rows(container)[0]!;
    expect(first.querySelector(".vogt-backlog-rank")?.textContent).toBe("1");
    expect(first.querySelector(".vogt-backlog-cell-ref")?.textContent).toBe("WI-1");
    expect(first.querySelector(".vogt-backlog-trust")?.textContent).toBe("verified");
    expect(first.querySelector(".vogt-backlog-age")?.textContent).toBeTruthy();
    expect(first.querySelector(".vogt-backlog-score")?.textContent).toContain("4.25");
    // Nothing in the row asks the browser to hide what does not fit.
    expect(first.querySelector(".vogt-backlog-row-title")).toBeTruthy();
    expect(rows(container)[1]?.querySelector(".vogt-backlog-rank")?.textContent).toBe("2");
  });

  it("expands in the row rather than in a dialog, and collapses again", async () => {
    fakeVogt(twoRows);
    const { container } = backlog();
    await waitFor(() => expect(rows(container)).toHaveLength(2));

    const first = () => rows(container)[0]!;
    expect(first().querySelector(".vogt-backlog-row-detail")).toBeNull();

    fireEvent.click(button(first(), "More"));
    await waitFor(() =>
      expect(first().querySelector(".vogt-backlog-row-detail")).toBeTruthy(),
    );
    expect(container.querySelector("[role=dialog]")).toBeNull();
    expect(button(first(), "Less")).toBeTruthy();

    fireEvent.click(button(first(), "Less"));
    await waitFor(() =>
      expect(first().querySelector(".vogt-backlog-row-detail")).toBeNull(),
    );
  });

  it("offers a declared row's acts and an observed subject's, never both", async () => {
    fakeVogt(twoRows);
    const { container } = backlog();
    await waitFor(() => expect(rows(container)).toHaveLength(2));

    fireEvent.click(button(rows(container)[0]!, "More"));
    await waitFor(() => expect(button(rows(container)[0]!, "Open")).toBeTruthy());
    const declaredActs = [...rows(container)[0]!.querySelectorAll("button")].map(
      (node) => node.textContent,
    );
    expect(declaredActs).toContain("Select");
    expect(declaredActs).toContain("Start a session…");
    expect(declaredActs).not.toContain("Adopt as work item…");

    fireEvent.click(button(rows(container)[1]!, "More"));
    await waitFor(() =>
      expect(button(rows(container)[1]!, "Adopt as work item…")).toBeTruthy(),
    );
    const observedActs = [...rows(container)[1]!.querySelectorAll("button")].map(
      (node) => node.textContent,
    );
    expect(observedActs).toContain("Suppress source…");
    expect(observedActs).not.toContain("Start a session…");
    expect(observedActs).not.toContain("Open");
  });

  it("refuses a row's write until a reason for that write is typed", async () => {
    const vogt = fakeVogt({
      ...twoRows,
      "POST /work/adopt": { body: { ok: true } },
    });
    const { container } = backlog();
    await waitFor(() => expect(rows(container)).toHaveLength(2));

    const observed = () => rows(container)[1]!;
    fireEvent.click(button(observed(), "More"));
    await waitFor(() => expect(button(observed(), "Adopt as work item…")).toBeTruthy());
    fireEvent.click(button(observed(), "Adopt as work item…"));

    const confirm = () => button(observed(), "Confirm");
    await waitFor(() => expect(confirm().disabled).toBe(true));
    expect(vogt.matching("POST /work/adopt")).toHaveLength(0);

    const reason = observed().querySelector("textarea")!;
    fireEvent.input(reason, { target: { value: "this is real work" } });
    await waitFor(() => expect(confirm().disabled).toBe(false));
    fireEvent.click(confirm());

    await waitFor(() => expect(vogt.matching("POST /work/adopt")).toHaveLength(1));
    const sent = vogt.matching("POST /work/adopt")[0]?.body as Record<string, unknown>;
    expect(sent.subject).toBe("gh:alpha#12");
    expect(sent.reason).toBe("this is real work");
  });
});

describe("FR-U21 — an unreachable backlog is not an empty one", () => {
  it("renders Vogt's own reason instead of rows", async () => {
    fakeVogt({
      "GET /backlog": unavailable("vogt-core is not configured for this front door"),
    });
    const { container } = backlog();

    await waitFor(() =>
      expect(container.querySelector(".vogt-backlog-outage")).toBeTruthy(),
    );
    expect(container.textContent).toContain("Vogt is not answering");
    expect(container.textContent).toContain(
      "vogt-core is not configured for this front door",
    );
    expect(container.textContent).toContain(
      "An empty backlog and an unreachable one are not the same answer.",
    );
    expect(rows(container)).toHaveLength(0);
  });

  it("calls a plain failure a failure and not an outage", async () => {
    fakeVogt({ "GET /backlog": refusal(500, "backlog: the ranker blew up") });
    const { container } = backlog();

    await waitFor(() =>
      expect(container.querySelector(".vogt-backlog-outage")).toBeTruthy(),
    );
    expect(container.textContent).toContain("This view failed to load");
    expect(container.textContent).not.toContain("Vogt is not answering");
  });
});

// -- FR-U6, the explainable half -------------------------------------------
//
// §6.2a: "An assertion that the `why` panel renders the contributions
// `GET /why` returned. The harness answers that route and no test looks at
// what was drawn with it." The route was answered by the default estate with
// an empty `contributions` array, so every existing mount opened the panel's
// route and none of them could have noticed the panel drawing nothing.
//
// The point of `why` is that a rank is not a number taken on faith, so a
// panel that renders the *total* and none of the inputs is the failure worth
// catching: it looks like an explanation and explains nothing. Every
// assertion below is against the numbers this test handed the route.

/** One explanation, keyed by ref, as `GET /why` answers per entry. */
const EXPLANATIONS: Record<string, Record<string, unknown>> = {
  "WI-1": {
    ref: "WI-1",
    title: "Teach the board to say what it does not know",
    total: 4.25,
    contributions: [
      {
        input: "age",
        detail: "open for 9 days",
        value: 9,
        weight: 0.25,
        contribution: 2.25,
      },
      { input: "priority", detail: "p2", value: 2, weight: 1, contribution: 2 },
    ],
    inputs_not_yet_available: {},
  },
  "WI-2": {
    ref: "WI-2",
    title: "Another",
    total: 1.5,
    contributions: [
      { input: "age", detail: "open for 2 days", value: 2, weight: 0.25, contribution: 0.5 },
      { input: "priority", detail: "p3", value: 1, weight: 1, contribution: 1 },
    ],
    inputs_not_yet_available: {},
  },
};

/** Answer `GET /why` from `EXPLANATIONS`, per the `ref` the client asked for. */
const WHY = {
  "GET /why": (call: { query: URLSearchParams }) => {
    const ref = call.query.get("ref") ?? "";
    const found = EXPLANATIONS[ref];
    if (!found) return refusal(404, `no explanation for ${ref}`);
    return { body: found };
  },
};

/** Open the `why` panel for a ranked row by pressing its score. */
async function openWhy(container: HTMLElement, ref: string): Promise<void> {
  const row = rows(container).find((node) => node.textContent?.includes(ref));
  if (!row) throw new Error(`no ranked row for ${ref}`);
  const score = row.querySelector<HTMLButtonElement>(".vogt-backlog-score");
  if (!score) throw new Error(`the row for ${ref} has no score to ask about`);
  fireEvent.click(score);
}

/** The explanation panel, or a failure that says it never opened. */
function explainPanel(container: HTMLElement): HTMLElement {
  const found = container.querySelector<HTMLElement>(".vogt-backlog-explain");
  if (!found) throw new Error("no ranking explanation is open");
  return found;
}

/** The panel's rows, as `input` → the text of each entry's cell. */
function explainRows(container: HTMLElement): Record<string, string[]> {
  const table = explainPanel(container).querySelector("tbody");
  const out: Record<string, string[]> = {};
  for (const row of table?.querySelectorAll("tr") ?? []) {
    const input = row.querySelector("th")?.textContent ?? "";
    out[input] = [...row.querySelectorAll("td")].map((cell) => cell.textContent ?? "");
  }
  return out;
}

describe("FR-U6 — the `why` panel is the contributions, not a second copy of the score", () => {
  it("draws one row per contribution Vogt returned, with the arithmetic behind it", async () => {
    const vogt = fakeVogt({ ...TWO, ...WHY });
    const { container } = backlog();
    await waitFor(() => expect(rows(container)).toHaveLength(2));

    await openWhy(container, "WI-1");
    await waitFor(() => expect(explainPanel(container).querySelector("tbody")).toBeTruthy());

    // It asked about the row that was pressed, and only that row.
    expect(vogt.matching("GET /why").map((call) => call.query.get("ref"))).toEqual(["WI-1"]);

    // Every named input is a row, and no input the server did not name is.
    const drawn = explainRows(container);
    expect(Object.keys(drawn)).toEqual(["age", "priority"]);

    // The contribution, and the value and weight it is made of — a panel
    // showing only the total would pass an assertion about the total.
    expect(drawn["age"]?.[0]).toContain("2.25");
    expect(drawn["age"]?.[0]).toContain("9.00");
    expect(drawn["age"]?.[0]).toContain("0.25");
    expect(drawn["age"]?.[0]).toContain("open for 9 days");
    expect(drawn["priority"]?.[0]).toContain("2.00");
    expect(explainPanel(container).textContent).toContain("total 4.25");
  });

  it("compares two entries input by input, which is what makes an order explainable", async () => {
    fakeVogt({ ...TWO, ...WHY });
    const { container } = backlog();
    await waitFor(() => expect(rows(container)).toHaveLength(2));

    await openWhy(container, "WI-1");
    await openWhy(container, "WI-2");
    await waitFor(() =>
      expect(explainPanel(container).textContent).toContain("Difference"),
    );
    await waitFor(() => expect(explainRows(container)["age"]?.length).toBe(3));

    const drawn = explainRows(container);
    // WI-1's column, WI-2's column, and the gap between them per input —
    // 2.25 − 0.50 on age and 2.00 − 1.00 on priority, which together are the
    // whole of "why is this one above that one".
    expect(drawn["age"]?.[0]).toContain("2.25");
    expect(drawn["age"]?.[1]).toContain("0.50");
    expect(drawn["age"]?.[2]).toBe("+1.75");
    expect(drawn["priority"]?.[2]).toBe("+1.00");
  });

  it("says an input this build cannot compute is absent, and never scores it zero", async () => {
    fakeVogt({
      ...TWO,
      "GET /why": {
        body: {
          ref: "WI-1",
          title: "Teach the board to say what it does not know",
          total: 4.25,
          contributions: [
            { input: "age", detail: "", value: 9, weight: 0.25, contribution: 2.25 },
          ],
          inputs_not_yet_available: {
            blast_radius: "no dependency sweep has run against this project",
          },
        },
      },
    });
    const { container } = backlog();
    await waitFor(() => expect(rows(container)).toHaveLength(2));

    await openWhy(container, "WI-1");
    await waitFor(() =>
      expect(explainPanel(container).querySelector(".vogt-backlog-pending")).toBeTruthy(),
    );

    const panel = explainPanel(container);
    expect(panel.textContent).toContain("blast_radius");
    expect(panel.textContent).toContain("no dependency sweep has run against this project");
    // Named as an absence, not folded into the table as a row worth 0.00.
    expect(Object.keys(explainRows(container))).toEqual(["age"]);
  });

  it("reports an explanation Vogt refused instead of leaving a hole in the table", async () => {
    fakeVogt({
      ...TWO,
      "GET /why": refusal(500, "why: the ranker has no inputs configured"),
    });
    const { container } = backlog();
    await waitFor(() => expect(rows(container)).toHaveLength(2));

    await openWhy(container, "WI-1");
    await waitFor(() =>
      expect(explainPanel(container).textContent).toContain(
        "why: the ranker has no inputs configured",
      ),
    );
    // and it does not draw an explanation made of nothing beside the refusal
    expect(explainPanel(container).querySelector("tbody")).toBeNull();
  });
});
