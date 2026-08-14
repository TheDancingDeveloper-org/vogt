// One work item, and the inline edit FR-U12 names and nothing implemented.
//
// §6.2: "There is no inline edit. `updateWork` is exported by `vogtApi.ts` and
// called by nothing — the dead binding that `test_pwa.py`'s own parity
// docstring warns about." These tests are what stops it going back.

import { describe, expect, it } from "vitest";
import { fireEvent, waitFor } from "@solidjs/testing-library";
import WorkItemDetail from "../WorkItemDetail";
import { fakeVogt, held, mountAt, refusal, unavailable, workItem } from "./harness";

function detail(itemRef = "WI-1") {
  return mountAt(`/w/${itemRef}`, `/w/${itemRef}`, () => (
    <WorkItemDetail itemRef={itemRef} />
  ));
}

function facts(container: HTMLElement): string {
  return container.querySelector(".wid-facts")?.textContent ?? "";
}

function editor(container: HTMLElement): HTMLElement {
  const found = container.querySelector<HTMLElement>(".wid-edit");
  if (!found) throw new Error("the item has no inline editor open");
  return found;
}

function field(form: HTMLElement, label: string): HTMLInputElement | HTMLSelectElement {
  const found = [...form.querySelectorAll<HTMLElement>(".wid-field")].find((node) =>
    (node.querySelector("span")?.textContent ?? "").startsWith(label),
  );
  const control = found?.querySelector<HTMLInputElement | HTMLSelectElement>(
    "input, select",
  );
  if (!control) throw new Error(`the editor has no ${label} field`);
  return control;
}

async function openEditor(container: HTMLElement): Promise<HTMLElement> {
  const button = container.querySelector<HTMLButtonElement>(".wid-edit-open")!;
  await waitFor(() => expect(button.disabled).toBe(false));
  fireEvent.click(button);
  await waitFor(() => editor(container));
  return editor(container);
}

describe("FR-U12 — an inline edit renders optimistically and the server decides", () => {
  it("opens on the item's own values, from the server", async () => {
    fakeVogt({
      "GET /work/get": {
        body: {
          item: workItem({ title: "As Vogt has it", priority: "p3" }),
          comments: [],
          sessions: [],
        },
      },
    });
    const { container } = detail();
    await waitFor(() => expect(facts(container)).toContain("p3"));

    const form = await openEditor(container);
    expect((field(form, "Title") as HTMLInputElement).value).toBe("As Vogt has it");
    expect((field(form, "Priority") as HTMLSelectElement).value).toBe("p3");
  });

  it("sends `work.update` with the reason the user typed", async () => {
    const vogt = fakeVogt({
      "POST /work/update": {
        body: {
          item: workItem({
            title: "A better title",
            priority: "p0",
            updated_at: "2026-08-03T00:00:00Z",
          }),
          comments: [],
          sessions: [],
        },
      },
    });
    const { container } = detail();
    await waitFor(() => expect(facts(container)).toContain("p2"));

    const form = await openEditor(container);
    fireEvent.input(field(form, "Title"), { target: { value: "A better title" } });
    fireEvent.input(field(form, "Priority"), { target: { value: "p0" } });
    fireEvent.input(field(form, "Reason"), { target: { value: "raised after the outage" } });
    fireEvent.submit(form.querySelector("form")!);

    await waitFor(() => expect(vogt.matching("POST /work/update")).toHaveLength(1));
    expect(vogt.matching("POST /work/update")[0]?.body).toEqual({
      ref: "WI-1",
      title: "A better title",
      priority: "p0",
      reason: "raised after the outage",
    });
  });

  it("renders the change while Vogt is still deciding, and says it is unsaved", async () => {
    const answer = held();
    fakeVogt({ "POST /work/update": answer.handler });
    const { container } = detail();
    await waitFor(() => expect(facts(container)).toContain("p2"));

    const form = await openEditor(container);
    fireEvent.input(field(form, "Title"), { target: { value: "Renamed on the spot" } });
    fireEvent.input(field(form, "Priority"), { target: { value: "p1" } });
    fireEvent.input(field(form, "Reason"), { target: { value: "the title was wrong" } });
    fireEvent.submit(form.querySelector("form")!);

    // Vogt has been asked and has not answered. The page already reads as the
    // change — and already admits the change is not yet true.
    await answer.asked;
    await waitFor(() =>
      expect(container.querySelector(".wid-heading h2")?.textContent).toBe(
        "Renamed on the spot",
      ),
    );
    expect(facts(container)).toContain("p1");
    expect(facts(container)).toContain("unsaved — Vogt is deciding");

    answer.answer({
      body: {
        item: workItem({
          title: "Renamed on the spot",
          priority: "p1",
          updated_at: "2026-08-03T00:00:00Z",
        }),
        comments: [],
        sessions: [],
      },
    });
    await waitFor(() => expect(facts(container)).not.toContain("unsaved"));
  });

  it("keeps the server's version of the change, not the one that was typed", async () => {
    fakeVogt({
      "POST /work/update": {
        body: {
          // Vogt normalised the title. The page must show Vogt's.
          item: workItem({
            title: "A better title",
            priority: "p0",
            updated_at: "2026-08-03T00:00:00Z",
          }),
          comments: [],
          sessions: [],
        },
      },
    });
    const { container } = detail();
    await waitFor(() => expect(facts(container)).toContain("p2"));

    const form = await openEditor(container);
    fireEvent.input(field(form, "Title"), { target: { value: "  a better title  " } });
    fireEvent.input(field(form, "Priority"), { target: { value: "p0" } });
    fireEvent.input(field(form, "Reason"), { target: { value: "tidying the wording" } });
    fireEvent.submit(form.querySelector("form")!);

    // Optimistically the heading reads what was typed; waiting for the
    // *server's* casing is what proves the reconcile happened at all.
    await waitFor(() =>
      expect(container.querySelector(".wid-heading h2")?.textContent).toBe(
        "A better title",
      ),
    );
    expect(facts(container)).toContain("p0");
    expect(facts(container)).not.toContain("unsaved");
  });

  it("rolls a refused edit back and shows Vogt's own reason beside the field", async () => {
    const REFUSED = "work.update: priority p0 is reserved for incidents";
    fakeVogt({ "POST /work/update": refusal(422, REFUSED) });
    const { container } = detail();
    await waitFor(() => expect(facts(container)).toContain("p2"));
    const before = container.querySelector(".wid-heading h2")?.textContent;

    const form = await openEditor(container);
    fireEvent.input(field(form, "Title"), { target: { value: "Something else" } });
    fireEvent.input(field(form, "Priority"), { target: { value: "p0" } });
    fireEvent.input(field(form, "Reason"), { target: { value: "it feels urgent" } });
    fireEvent.submit(form.querySelector("form")!);

    await waitFor(() =>
      expect(editor(container).querySelector(".wid-failure")?.textContent).toContain(
        REFUSED,
      ),
    );

    // Rolled back visibly: the facts row and the heading are the server's
    // again, and the surface says so in as many words.
    expect(facts(container)).toContain("p2");
    expect(facts(container)).not.toContain("unsaved");
    expect(container.querySelector(".wid-heading h2")?.textContent).toBe(before);
    expect(editor(container).querySelector(".wid-rolledback")?.textContent).toContain(
      "WI-1 is unchanged",
    );
  });

  it("does not re-derive a refused value when the editor is reopened", async () => {
    fakeVogt({ "POST /work/update": refusal(422, "work.update: refused") });
    const { container } = detail();
    await waitFor(() => expect(facts(container)).toContain("p2"));

    const form = await openEditor(container);
    fireEvent.input(field(form, "Title"), { target: { value: "Something else" } });
    fireEvent.input(field(form, "Priority"), { target: { value: "p0" } });
    fireEvent.input(field(form, "Reason"), { target: { value: "it feels urgent" } });
    fireEvent.submit(form.querySelector("form")!);
    await waitFor(() => expect(editor(container).querySelector(".wid-failure")).toBeTruthy());

    // Close and reopen: FR-U12's "never persist, cache, or re-derive".
    fireEvent.click(container.querySelector<HTMLButtonElement>(".wid-edit-open")!);
    await waitFor(() => expect(container.querySelector(".wid-edit")).toBeNull());
    const reopened = await openEditor(container);

    expect((field(reopened, "Title") as HTMLInputElement).value).toBe(
      "Teach the board to say what it does not know",
    );
    expect((field(reopened, "Priority") as HTMLSelectElement).value).toBe("p2");
    // And nothing about the refused edit reached storage.
    expect(JSON.stringify(localStorage)).not.toContain("Something else");
  });

  it("will not submit without a reason the user typed", async () => {
    const vogt = fakeVogt();
    const { container } = detail();
    await waitFor(() => expect(facts(container)).toContain("p2"));

    const form = await openEditor(container);
    const submit = form.querySelector<HTMLButtonElement>("button[type=submit]")!;
    fireEvent.input(field(form, "Title"), { target: { value: "Something else" } });
    expect(submit.disabled).toBe(true);

    fireEvent.submit(form.querySelector("form")!);
    expect(vogt.matching("POST /work/update")).toHaveLength(0);

    fireEvent.input(field(form, "Reason"), { target: { value: "   " } });
    expect(submit.disabled).toBe(true);
  });

  it("will not submit an edit that changes nothing, or empties the title", async () => {
    const vogt = fakeVogt();
    const { container } = detail();
    await waitFor(() => expect(facts(container)).toContain("p2"));

    const form = await openEditor(container);
    const submit = form.querySelector<HTMLButtonElement>("button[type=submit]")!;
    fireEvent.input(field(form, "Reason"), { target: { value: "a reason with no change" } });
    expect(submit.disabled).toBe(true);

    fireEvent.input(field(form, "Title"), { target: { value: "  " } });
    expect(submit.disabled).toBe(true);
    fireEvent.submit(form.querySelector("form")!);
    expect(vogt.matching("POST /work/update")).toHaveLength(0);
  });
});

describe("FR-U21 — the item page tells an outage from an empty item", () => {
  it("says Vogt cannot be reached, in Vogt's words, and disables the edit", async () => {
    fakeVogt({
      "GET /work/get": unavailable("vogt-core is not configured for this front door"),
    });
    const { container } = detail();

    await waitFor(() => expect(container.querySelector(".wid-outage")).toBeTruthy());
    expect(container.querySelector(".wid-outage")?.textContent).toContain(
      "vogt-core is not configured for this front door",
    );
    expect(container.textContent).toContain("This is an outage, not an empty work item.");
    expect(container.querySelector<HTMLButtonElement>(".wid-edit-open")?.disabled).toBe(true);
  });

  it("distinguishes a failed read from an outage", async () => {
    fakeVogt({ "GET /work/get": refusal(404, "work.get: no item WI-404") });
    const { container } = detail("WI-404");

    await waitFor(() => expect(container.querySelector(".wid-failure")).toBeTruthy());
    expect(container.querySelector(".wid-failure")?.textContent).toContain(
      "work.get: no item WI-404",
    );
    // A 404 is Vogt answering, so it is not reported as an outage.
    expect(container.querySelector(".wid-outage")).toBeNull();
  });
});
