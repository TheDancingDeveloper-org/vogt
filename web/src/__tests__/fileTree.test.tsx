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
});
