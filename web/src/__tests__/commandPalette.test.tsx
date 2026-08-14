// The keyboard path to the Vogt surfaces (FR-U16).
//
// §6.2: "Views, sessions and work items, yes… **Projects are not reachable by
// name**: the palette imports `listWork` and nothing else from `vogtApi.ts`,
// so 'open project rustnzb' is not a thing the keyboard can do."
//
// `tests/test_pwa.py` already asserts the half that is checkable from source
// — that the palette imports no write, so no palette entry can invent a
// reason. What it cannot check is whether the reads are *reachable by name*,
// which is the clause this file covers.

import { describe, expect, it } from "vitest";
import { fireEvent, render, waitFor } from "@solidjs/testing-library";
import { MemoryRouter, Route, createMemoryHistory } from "@solidjs/router";
import CommandPalette from "../CommandPalette";
import { fakeVogt, workItem } from "./harness";

function palette() {
  const history = createMemoryHistory();
  history.set({ value: "/board" });
  let closed = 0;
  const rendered = render(() => (
    <MemoryRouter history={history}>
      <Route
        path="*rest"
        component={() => (
          <CommandPalette open={true} onClose={() => (closed += 1)} />
        )}
      />
    </MemoryRouter>
  ));
  return {
    container: rendered.container,
    url: () => history.get(),
    closed: () => closed,
    type(text: string) {
      const input = rendered.container.querySelector("input")!;
      fireEvent.input(input, { target: { value: text } });
    },
    /** Every rendered row's text, whatever the shell calls its classes. */
    text(): string {
      return rendered.container.textContent ?? "";
    },
    click(label: string) {
      const found = [...rendered.container.querySelectorAll<HTMLElement>("*")].find(
        (node) =>
          node.children.length === 0 && (node.textContent ?? "").trim() === label,
      );
      if (!found) throw new Error(`no palette row reading "${label}"`);
      fireEvent.click(found);
    },
  };
}

const ESTATE = {
  "GET /projects": {
    body: {
      projects: [
        { slug: "rustnzb", name: "rustnzb" },
        { slug: "vogt", name: "Vogt" },
      ],
    },
  },
  "GET /work": {
    body: { items: [workItem({ ref: "WI-7", title: "Ship the board" })], total: 1 },
  },
};

describe("FR-U16 — every read surface by fuzzy name", () => {
  it("offers each registered project by name", async () => {
    fakeVogt(ESTATE);
    const view = palette();

    await waitFor(() => expect(view.text()).toContain("Open project rustnzb"));
    expect(view.text()).toContain("Open project Vogt");
  });

  it("finds a project by a fuzzy fragment of its name", async () => {
    fakeVogt(ESTATE);
    const view = palette();
    await waitFor(() => expect(view.text()).toContain("Open project rustnzb"));

    view.type("rstnz");
    await waitFor(() => expect(view.text()).toContain("Open project rustnzb"));
    // and the fragment does not also drag in the whole Vogt menu
    expect(view.text()).not.toContain("Open project Vogt");
  });

  it("opens the project's own deep link, which is the one a shared URL uses", async () => {
    fakeVogt(ESTATE);
    const view = palette();
    await waitFor(() => expect(view.text()).toContain("Open project rustnzb"));

    view.click("Open project rustnzb");
    await waitFor(() => expect(view.url()).toBe("/projects?p=rustnzb"));
    expect(view.closed()).toBe(1);
  });

  it("still reaches work items and every view by name", async () => {
    fakeVogt(ESTATE);
    const view = palette();

    await waitFor(() => expect(view.text()).toContain("WI-7 — Ship the board"));
    for (const label of ["Open Board", "Open Backlog", "Open Projects", "Open Audit"]) {
      expect(view.text()).toContain(label);
    }
  });

  it("contributes nothing when Vogt cannot be asked, rather than failing open", async () => {
    fakeVogt({
      "GET /projects": { status: 503, body: { error: { message: "no core" } } },
      "GET /work": { status: 503, body: { error: { message: "no core" } } },
    });
    const view = palette();

    // The surfaces are where an outage is reported; a command list somebody
    // is typing into is not.
    await waitFor(() => expect(view.text()).toContain("Open Board"));
    expect(view.text()).not.toContain("Open project");
    expect(view.text()).not.toContain("no core");
  });
});
