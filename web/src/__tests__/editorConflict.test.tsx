import { fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { beforeEach, describe, expect, it, vi } from "vitest";

// A minimal Monaco stand-in. The factory keeps its own buffer so the editor's
// getValue() reflects whatever content the model was created with — enough to
// drive save/reload without a real editor.
vi.mock("../monaco", () => {
  const state = { value: "" };
  return {
    languageFor: () => "plaintext",
    loadMonaco: async () => ({
      KeyMod: { CtrlCmd: 2048 },
      KeyCode: { KeyS: 49 },
      Uri: { parse: (s: string) => ({ path: s }) },
      editor: {
        createModel: (content: string) => {
          state.value = content;
          return {
            setValue: (v: string) => {
              state.value = v;
            },
            dispose: () => {},
            uri: {},
          };
        },
        create: () => ({
          getValue: () => state.value,
          saveViewState: () => null,
          restoreViewState: () => {},
          onDidChangeModelContent: () => ({ dispose: () => {} }),
          addCommand: () => {},
          updateOptions: () => {},
          layout: () => {},
          dispose: () => {},
          focus: () => {},
        }),
      },
    }),
  };
});

import Editor from "../Editor";
import { api, ApiError, type FileRead, type WriteFileResponse } from "../api";
import { openEditorTab } from "../tabs";
import { rememberEditorDraft } from "../editorDrafts";

const PATH = "src/notes.txt";
const TAB_ID = `edit:${PATH}`;

function diskRead(content: string, hash: string): FileRead {
  return {
    path: PATH,
    size: content.length,
    content,
    content_base64: null,
    is_binary: false,
    mtime: 1000,
    hash,
  };
}

beforeEach(() => {
  localStorage.clear();
  vi.restoreAllMocks();
});

describe("Editor on-disk conflict + draft notice (#237)", () => {
  it("flags a restored draft that differs from disk", async () => {
    openEditorTab(PATH);
    // A remembered draft that the disk has since moved past.
    rememberEditorDraft(TAB_ID, { path: PATH, content: "draft body", viewState: null });
    vi.spyOn(api, "readFile").mockResolvedValue(diskRead("disk body", "hash-a"));

    render(() => <Editor tabId={TAB_ID} path={PATH} />);

    expect(
      await screen.findByText("Restored unsaved draft; disk differs."),
    ).toBeVisible();
  });

  it("refuses a stale save and offers Overwrite / Reload, then Overwrite forces past the guard", async () => {
    openEditorTab(PATH);
    // Draft differs so the tab is dirty and Save is enabled.
    rememberEditorDraft(TAB_ID, { path: PATH, content: "my edit", viewState: null });
    vi.spyOn(api, "readFile").mockResolvedValue(diskRead("original", "hash-a"));

    const write = vi
      .spyOn(api, "writeFile")
      .mockRejectedValueOnce(new ApiError(409, "file changed on disk since it was read"))
      .mockResolvedValue({ ok: true, bytes: 7, hash: "hash-b", mtime: 2000 } as WriteFileResponse);

    render(() => <Editor tabId={TAB_ID} path={PATH} />);

    const save = await screen.findByRole("button", { name: "Save" });
    await waitFor(() => expect(save).toBeEnabled());
    await fireEvent.click(save);

    // The conflict is surfaced inline, not applied silently.
    await screen.findByText("File changed on disk since you opened it.");
    const banner = screen.getByRole("alert");
    const overwrite = within(banner).getByRole("button", { name: "Overwrite" });
    expect(within(banner).getByRole("button", { name: "Reload" })).toBeVisible();

    // First save carried the last-read hash as the guard.
    expect(write).toHaveBeenNthCalledWith(1, PATH, "my edit", false, "hash-a");

    await fireEvent.click(overwrite);

    // The banner clears once the forced write lands, and the force drops the guard.
    await waitFor(() =>
      expect(
        screen.queryByText("File changed on disk since you opened it."),
      ).not.toBeInTheDocument(),
    );
    expect(write).toHaveBeenNthCalledWith(2, PATH, "my edit", false, undefined);
  });

  it("Reload takes the disk content and clears the conflict", async () => {
    openEditorTab(PATH);
    rememberEditorDraft(TAB_ID, { path: PATH, content: "my edit", viewState: null });
    vi.spyOn(api, "readFile")
      .mockResolvedValueOnce(diskRead("original", "hash-a"))
      .mockResolvedValue(diskRead("newer disk content", "hash-c"));
    vi.spyOn(api, "writeFile").mockRejectedValue(
      new ApiError(409, "file changed on disk since it was read"),
    );

    render(() => <Editor tabId={TAB_ID} path={PATH} />);

    const save = await screen.findByRole("button", { name: "Save" });
    await waitFor(() => expect(save).toBeEnabled());
    await fireEvent.click(save);
    await screen.findByText("File changed on disk since you opened it.");

    await fireEvent.click(within(screen.getByRole("alert")).getByRole("button", { name: "Reload" }));

    await waitFor(() =>
      expect(
        screen.queryByText("File changed on disk since you opened it."),
      ).not.toBeInTheDocument(),
    );
    // After reload the buffer matches disk, so the tab is clean and Save disables.
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "Save" })).toBeDisabled(),
    );
  });
});
