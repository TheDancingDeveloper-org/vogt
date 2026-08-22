// #247, bullets 5 and 7: the editor split buttons carry real accessible names,
// and the active tab is scrolled into view when it changes.
//
// The panes' Monaco/file-tree machinery is irrelevant here, so Editor,
// SplitEditor and FileTree are stubbed — the assertions are about the tab
// strip the workspace draws around them.
import { MemoryRouter, Route, createMemoryHistory } from "@solidjs/router";
import { render, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../Editor", () => ({ default: () => <div class="stub-editor" /> }));
vi.mock("../SplitEditor", () => ({ default: () => <div class="stub-split" /> }));
vi.mock("../FileTree", () => ({ default: () => <div class="stub-filetree" /> }));

import EditorWorkspace from "../EditorWorkspace";
import { focusTab, replaceTabs } from "../tabs";

function mount() {
  const history = createMemoryHistory();
  history.set({ value: "/e/a.ts" });
  return render(() => (
    <MemoryRouter history={history}>
      <Route path="*" component={() => <EditorWorkspace />} />
    </MemoryRouter>
  ));
}

beforeEach(() => {
  replaceTabs({
    tabs: [
      { id: "e1", kind: "editor", path: "a.ts", label: "a.ts" },
      { id: "e2", kind: "editor", path: "b.ts", label: "b.ts" },
    ],
    active: "e1",
  });
});

afterEach(() => {
  replaceTabs({ tabs: [], active: null });
  vi.restoreAllMocks();
});

describe("#247 — editor workspace polish", () => {
  it("gives the split buttons real accessible names", () => {
    const { container } = mount();
    const labels = [...container.querySelectorAll(".editor-tab-actions button")].map(
      (b) => b.getAttribute("aria-label"),
    );
    expect(labels).toEqual([
      "Split editor right",
      "Split editor down",
      "Unsplit editor",
    ]);
  });

  it("scrolls the active tab into view when the active tab changes", async () => {
    const scrollIntoView = vi.fn();
    // jsdom has no scrollIntoView; provide a spy so the effect's call is seen.
    (Element.prototype as unknown as { scrollIntoView: () => void }).scrollIntoView =
      scrollIntoView;

    mount();
    // The mount effect scrolls the initially-active tab; clear it, then switch.
    await waitFor(() => expect(scrollIntoView).toHaveBeenCalled());
    scrollIntoView.mockClear();

    focusTab("e2");
    await waitFor(() => expect(scrollIntoView).toHaveBeenCalled());

    delete (Element.prototype as unknown as { scrollIntoView?: () => void }).scrollIntoView;
  });
});
