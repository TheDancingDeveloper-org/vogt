import { Route, Router } from "@solidjs/router";
import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";
import { api } from "../api";
import FileTree from "../FileTree";

vi.mock("../store", async () => {
  const actual = await vi.importActual<typeof import("../store")>("../store");
  return { ...actual, createSession: vi.fn() };
});

describe("FileTree", () => {
  afterEach(() => vi.restoreAllMocks());

  it("puts the Files hierarchy first, keeps names dominant, and progressively discloses every secondary action", async () => {
    vi.spyOn(api, "tree").mockImplementation(async (path) => path === "source"
      ? [{ name: "nested.tsx", path: "source/nested.tsx", is_dir: false }]
      : [
          { name: "source", path: "source", is_dir: true },
          {
            name: "a-very-long-and-identifiable-component-name.tsx",
            path: "a-very-long-and-identifiable-component-name.tsx",
            is_dir: false,
          },
        ]);
    vi.spyOn(api, "gitStatus").mockResolvedValue({
      repo: "",
      is_repo: true,
      branch: "dev",
      ahead: 0,
      behind: 0,
      entries: [{
        path: "a-very-long-and-identifiable-component-name.tsx",
        index: " ",
        worktree: "M",
        kind: "modified",
      }],
    });

    render(() => (
      <Router>
        <Route path="*" component={() => <FileTree />} />
      </Router>
    ));

    // Files starts collapsed (rail-spec.md B3); expand it before asserting on
    // its contents, the same as a reader would.
    expect(screen.getByRole("button", { name: "Files" })).toHaveAttribute("aria-expanded", "false");
    await fireEvent.click(screen.getByRole("button", { name: "Files" }));

    expect(await screen.findByText("source")).toBeVisible();
    expect(screen.getByText("a-very-long-and-identifiable-component-name.tsx")).toBeVisible();
    expect(screen.getByRole("heading", { name: "Files" })).toBeVisible();
    expect(screen.getByRole("heading", { name: "Files" }))
      .toAppearBefore(screen.getByRole("searchbox", { name: "Search files" }));
    expect(screen.getByLabelText("Modified file")).toHaveTextContent("M");
    expect(screen.getByLabelText("Expand source")).toHaveAttribute("aria-expanded", "false");
    expect(screen.queryByText("DIR", { exact: true })).not.toBeInTheDocument();
    expect(screen.queryByText("TSX", { exact: true })).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "New file" })).toBeVisible();
    expect(screen.getByRole("button", { name: "Refresh files" })).toBeVisible();
    expect(screen.queryByRole("button", { name: "New folder" })).not.toBeInTheDocument();

    await fireEvent.click(screen.getByRole("button", { name: "More file actions" }));
    expect(screen.getByRole("button", { name: "New folder" })).toBeVisible();
    expect(screen.getByRole("button", { name: "Upload files" })).toBeVisible();

    await fireEvent.click(screen.getByLabelText("Expand source"));
    expect(await screen.findByText("nested.tsx")).toBeVisible();
    expect(screen.queryByRole("button", { name: "Delete" })).not.toBeInTheDocument();

    await fireEvent.click(screen.getByLabelText("Actions for source"));
    expect(screen.getByRole("button", { name: "Open terminal" })).toBeVisible();
    expect(screen.getByRole("button", { name: "Delete" })).toBeVisible();
    await waitFor(() => expect(api.tree).toHaveBeenCalledWith("source", 0));
  });

  it("dismisses a folder's actions picker on collapse, outside-click and Escape (#186)", async () => {
    vi.spyOn(api, "tree").mockImplementation(async (path) =>
      path === "source"
        ? [{ name: "nested.tsx", path: "source/nested.tsx", is_dir: false }]
        : [{ name: "source", path: "source", is_dir: true }],
    );
    vi.spyOn(api, "gitStatus").mockResolvedValue({
      repo: "",
      is_repo: false,
      branch: "",
      ahead: 0,
      behind: 0,
      entries: [],
    });

    render(() => (
      <Router>
        <Route path="*" component={() => <FileTree />} />
      </Router>
    ));

    await fireEvent.click(screen.getByRole("button", { name: "Files" }));
    expect(await screen.findByText("source")).toBeVisible();

    // Collapsing the folder takes its open picker with it (the orphaned-menu bug).
    await fireEvent.click(screen.getByLabelText("Expand source"));
    await fireEvent.click(screen.getByLabelText("Actions for source"));
    expect(screen.getByRole("button", { name: "Open terminal" })).toBeVisible();
    await fireEvent.click(screen.getByLabelText("Collapse source"));
    expect(screen.queryByRole("button", { name: "Open terminal" })).not.toBeInTheDocument();

    // A click outside the row dismisses it, the way a menu does.
    await fireEvent.click(screen.getByLabelText("Actions for source"));
    expect(screen.getByRole("button", { name: "Rename / move" })).toBeVisible();
    await fireEvent.click(document.body);
    expect(screen.queryByRole("button", { name: "Rename / move" })).not.toBeInTheDocument();

    // And Escape dismisses it.
    await fireEvent.click(screen.getByLabelText("Actions for source"));
    expect(screen.getByRole("button", { name: "Rename / move" })).toBeVisible();
    await fireEvent.keyDown(document, { key: "Escape" });
    expect(screen.queryByRole("button", { name: "Rename / move" })).not.toBeInTheDocument();
  });

  it("keeps only one node's actions picker open at a time (#186)", async () => {
    vi.spyOn(api, "tree").mockResolvedValue([
      { name: "one.txt", path: "one.txt", is_dir: false },
      { name: "two.txt", path: "two.txt", is_dir: false },
    ]);
    vi.spyOn(api, "gitStatus").mockResolvedValue({
      repo: "",
      is_repo: false,
      branch: "",
      ahead: 0,
      behind: 0,
      entries: [],
    });

    render(() => (
      <Router>
        <Route path="*" component={() => <FileTree />} />
      </Router>
    ));

    await fireEvent.click(screen.getByRole("button", { name: "Files" }));
    expect(await screen.findByText("one.txt")).toBeVisible();

    await fireEvent.click(screen.getByLabelText("Actions for one.txt"));
    expect(screen.getAllByRole("button", { name: "Rename / move" })).toHaveLength(1);

    // Opening the second node's picker closes the first — one picker, not two.
    await fireEvent.click(screen.getByLabelText("Actions for two.txt"));
    expect(screen.getAllByRole("button", { name: "Rename / move" })).toHaveLength(1);
  });

  it("uses the in-product decision for delete, defaults to cancel, and reports failures", async () => {
    vi.spyOn(api, "tree").mockResolvedValue([
      { name: "notes.txt", path: "docs/notes.txt", is_dir: false },
    ]);
    vi.spyOn(api, "gitStatus").mockResolvedValue({
      repo: "",
      is_repo: false,
      branch: "",
      ahead: 0,
      behind: 0,
      entries: [],
    });
    const fileOp = vi.spyOn(api, "fileOp").mockRejectedValue(new Error("disk is read-only"));
    const confirmAction = vi
      .fn<(title: string, body?: string) => Promise<boolean>>()
      .mockResolvedValueOnce(false)
      .mockResolvedValueOnce(true);
    const onError = vi.fn();

    render(() => (
      <Router>
        <Route
          path="*"
          component={() => (
            <FileTree confirmAction={confirmAction} onError={onError} />
          )}
        />
      </Router>
    ));

    await fireEvent.click(screen.getByRole("button", { name: "Files" }));
    expect(await screen.findByText("notes.txt")).toBeVisible();
    await fireEvent.click(screen.getByLabelText("Actions for docs/notes.txt"));
    await fireEvent.click(screen.getByRole("button", { name: "Delete" }));
    expect(confirmAction).toHaveBeenCalledWith(
      'Delete file "docs/notes.txt"?',
      "The file will be permanently deleted.",
    );
    expect(fileOp).not.toHaveBeenCalled();

    await fireEvent.click(screen.getByRole("button", { name: "Delete" }));
    await waitFor(() => expect(fileOp).toHaveBeenCalledWith({
      op: "delete",
      path: "docs/notes.txt",
      recursive: false,
    }));
    await waitFor(() =>
      expect(onError).toHaveBeenCalledWith("delete failed: disk is read-only"),
    );
  });
});
