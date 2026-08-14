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
    fireEvent.click(button(view.container, "Save"));
    await waitFor(() =>
      expect(view.container.querySelector(".vogt-backlog-saved-recall")?.textContent).toBe(
        "beta only",
      ),
    );

    fireEvent.click(button(view.container, "Clear filters"));
    await waitFor(() => expect(queryOf(view.url()).get("project")).toBeNull());

    fireEvent.click(
      view.container.querySelector<HTMLButtonElement>(".vogt-backlog-saved-recall")!,
    );
    await waitFor(() => expect(queryOf(view.url()).get("project")).toBe("beta"));
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
