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

  it("keeps actual names readable and moves secondary actions behind a labelled control", async () => {
    vi.spyOn(api, "tree").mockResolvedValue([
      { name: "source", path: "source", is_dir: true },
      {
        name: "a-very-long-and-identifiable-component-name.tsx",
        path: "source/a-very-long-and-identifiable-component-name.tsx",
        is_dir: false,
      },
    ]);

    render(() => (
      <Router>
        <Route path="*" component={() => <FileTree />} />
      </Router>
    ));

    expect(await screen.findByText("source")).toBeVisible();
    expect(screen.getByText("a-very-long-and-identifiable-component-name.tsx")).toBeVisible();
    expect(screen.getByLabelText("Expand source")).toHaveAttribute("aria-expanded", "false");
    expect(screen.queryByRole("button", { name: "Delete" })).not.toBeInTheDocument();

    await fireEvent.click(screen.getByLabelText("Actions for source"));
    expect(screen.getByRole("button", { name: "Open terminal" })).toBeVisible();
    expect(screen.getByRole("button", { name: "Delete" })).toBeVisible();
    expect(document.querySelector(".tree-icon")?.textContent).toContain("DIR");
    await waitFor(() => expect(api.tree).toHaveBeenCalled());
  });

  it("uses the in-product decision for delete, defaults to cancel, and reports failures", async () => {
    vi.spyOn(api, "tree").mockResolvedValue([
      { name: "notes.txt", path: "docs/notes.txt", is_dir: false },
    ]);
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
