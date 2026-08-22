import { Route, Router } from "@solidjs/router";
import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import { api } from "../api";
import FileTree from "../FileTree";
import { setRailSection } from "../railSections";

vi.mock("../store", async () => {
  const actual = await vi.importActual<typeof import("../store")>("../store");
  return { ...actual, createSession: vi.fn() };
});

function mountTree(props: Parameters<typeof FileTree>[0] = {}) {
  return render(() => (
    <Router>
      <Route path="*" component={() => <FileTree {...props} />} />
    </Router>
  ));
}

const NO_GIT = {
  repo: "",
  is_repo: false,
  branch: "",
  ahead: 0,
  behind: 0,
  entries: [],
};

describe("FileTree state retention (#238)", () => {
  afterEach(() => vi.restoreAllMocks());

  it("keeps a folder expanded across an unmount/remount (a tab switch)", async () => {
    vi.spyOn(api, "tree").mockImplementation(async (path) =>
      path === "src"
        ? [{ name: "nested.tsx", path: "src/nested.tsx", is_dir: false }]
        : [{ name: "src", path: "src", is_dir: true }],
    );
    vi.spyOn(api, "gitStatus").mockResolvedValue(NO_GIT);
    // The Files section is collapsed by default; open it the way a reader would.
    setRailSection("files", true);

    const first = mountTree();
    await fireEvent.click(await screen.findByLabelText("Expand src"));
    expect(await screen.findByText("nested.tsx")).toBeVisible();

    // Switching to another tab unmounts the workspace. Coming back must not
    // collapse the tree the reader had expanded (#238).
    first.unmount();
    mountTree();

    // No second Expand click: the folder is already open and its child is back.
    expect(await screen.findByLabelText("Collapse src")).toBeVisible();
    expect(await screen.findByText("nested.tsx")).toBeVisible();
  });

  it("refetches only the affected parent after a file op, not the whole tree", async () => {
    const treeCalls: string[] = [];
    vi.spyOn(api, "tree").mockImplementation(async (rawPath) => {
      const path = rawPath ?? "";
      treeCalls.push(path);
      return path === "src"
        ? [{ name: "nested.tsx", path: "src/nested.tsx", is_dir: false }]
        : [{ name: "src", path: "src", is_dir: true }];
    });
    vi.spyOn(api, "gitStatus").mockResolvedValue(NO_GIT);
    const fileOp = vi.spyOn(api, "fileOp").mockResolvedValue({ ok: true });
    const confirmAction = vi.fn(async () => true);
    setRailSection("files", true);

    mountTree({ confirmAction });
    await fireEvent.click(await screen.findByLabelText("Expand src"));
    expect(await screen.findByText("nested.tsx")).toBeVisible();

    const rootCallsBefore = treeCalls.filter((p) => p === "").length;
    const srcCallsBefore = treeCalls.filter((p) => p === "src").length;

    // Delete a file nested under src → the parent is "src", so only that folder
    // is invalidated; the root level is left alone.
    await fireEvent.click(screen.getByLabelText("Actions for src/nested.tsx"));
    await fireEvent.click(screen.getByRole("button", { name: "Delete" }));
    await waitFor(() =>
      expect(fileOp).toHaveBeenCalledWith({
        op: "delete",
        path: "src/nested.tsx",
        recursive: false,
      }),
    );

    // The affected parent refetched; the root did not.
    await waitFor(() =>
      expect(treeCalls.filter((p) => p === "src").length).toBeGreaterThan(srcCallsBefore),
    );
    expect(treeCalls.filter((p) => p === "").length).toBe(rootCallsBefore);
  });
});
