import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { afterEach, describe, expect, it, vi } from "vitest";
import { api } from "../api";
import FileWorkflowDialog, { type FileWorkflow } from "../FileWorkflowDialog";

function workflow(initial: FileWorkflow) {
  let setOpen!: (value: boolean) => void;
  const onOpenFile = vi.fn();
  const onFileCreated = vi.fn();
  const rendered = render(() => {
    const [open, updateOpen] = createSignal(true);
    setOpen = updateOpen;
    return (
      <>
        <button type="button">Open palette</button>
        {open() ? (
          <FileWorkflowDialog
            workflow={initial}
            onClose={() => setOpen(false)}
            onOpenFile={onOpenFile}
            onFileCreated={onFileCreated}
          />
        ) : null}
      </>
    );
  });
  return { ...rendered, onOpenFile, onFileCreated };
}

describe("file workflows", () => {
  afterEach(() => vi.restoreAllMocks());

  it("creates an empty file from distinct destination and filename fields", async () => {
    const writeFile = vi.spyOn(api, "writeFile").mockResolvedValue({
      ok: true,
      bytes: 0,
    });
    const view = workflow("new");

    expect(screen.getByRole("dialog", { name: "New file" })).toBeVisible();
    await fireEvent.input(screen.getByLabelText("Destination folder"), {
      target: { value: "/notes/" },
    });
    await fireEvent.input(screen.getByLabelText("Filename"), {
      target: { value: "release.md" },
    });
    expect(screen.getByText("Create notes/release.md")).toBeVisible();
    await fireEvent.click(screen.getByRole("button", { name: "Create file" }));

    await waitFor(() =>
      expect(writeFile).toHaveBeenCalledWith("notes/release.md", "", true),
    );
    expect(view.onFileCreated).toHaveBeenCalledWith("notes/release.md");
    expect(view.onOpenFile).toHaveBeenCalledWith("notes/release.md");
    expect(screen.queryByRole("dialog", { name: "New file" })).not.toBeInTheDocument();
  });

  it("searches the workspace and opens the chosen result", async () => {
    const searchFiles = vi.spyOn(api, "searchFiles").mockResolvedValue([
      { name: "release.md", path: "notes/release.md" },
    ]);
    const view = workflow("open");

    expect(screen.getByRole("dialog", { name: "Open file" })).toBeVisible();
    await fireEvent.input(screen.getByLabelText("Search workspace files"), {
      target: { value: "release" },
    });
    const result = await screen.findByRole("button", { name: "release.md — notes/release.md" });
    expect(searchFiles).toHaveBeenCalledWith(
      "release",
      "",
      100,
      expect.any(AbortSignal),
    );
    expect(view.onFileCreated).not.toHaveBeenCalled();
    await fireEvent.click(result);

    expect(view.onOpenFile).toHaveBeenCalledWith("notes/release.md");
    expect(screen.queryByRole("dialog", { name: "Open file" })).not.toBeInTheDocument();
  });

  it.each<FileWorkflow>(["new", "open"])(
    "cancels the %s workflow without mutation and restores the invoking control's focus",
    async (mode) => {
      const writeFile = vi.spyOn(api, "writeFile");
      const searchFiles = vi.spyOn(api, "searchFiles");
      const invoker = document.createElement("button");
      invoker.textContent = "Go to…";
      document.body.append(invoker);
      invoker.focus();

      const view = workflow(mode);
      const initialField = mode === "new"
        ? screen.getByLabelText("Destination folder")
        : screen.getByLabelText("Search workspace files");
      await waitFor(() => expect(initialField).toHaveFocus());
      await fireEvent.click(screen.getByRole("button", { name: "Cancel" }));

      await waitFor(() => expect(invoker).toHaveFocus());
      expect(writeFile).not.toHaveBeenCalled();
      expect(searchFiles).not.toHaveBeenCalled();
      expect(view.onFileCreated).not.toHaveBeenCalled();
      invoker.remove();
    },
  );

  it("keeps provider data valid when file creation fails", async () => {
    vi.spyOn(api, "writeFile").mockRejectedValue(new Error("disk full"));
    const view = workflow("new");
    await fireEvent.input(screen.getByLabelText("Filename"), {
      target: { value: "release.md" },
    });
    await fireEvent.click(screen.getByRole("button", { name: "Create file" }));

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "File creation failed: disk full",
    );
    expect(view.onFileCreated).not.toHaveBeenCalled();
    expect(view.onOpenFile).not.toHaveBeenCalled();
    expect(screen.getByRole("dialog", { name: "New file" })).toBeVisible();
  });
});
