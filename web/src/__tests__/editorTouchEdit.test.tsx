import { fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// WI-1591: on a phone, Monaco swallows the long-press (no native selection, no
// WebView Copy/Paste menu) and reports it as an editor context-menu event. The
// editor answers that event with its own edit bar, wired to the shared
// clipboard path. This stand-in Monaco keeps just enough state — a buffer, a
// selection and the context-menu listener — to drive that from a test.
interface Sel {
  startLineNumber: number;
  startColumn: number;
  endLineNumber: number;
  endColumn: number;
}

const state = vi.hoisted(() => ({
  value: "",
  selection: null as Sel | null,
  contextMenu: null as ((e: unknown) => void) | null,
  edits: [] as { source: string; range: Sel; text: string }[],
}));

function offset(line: number, column: number): number {
  const lines = state.value.split("\n");
  let at = 0;
  for (let i = 0; i < line - 1; i++) at += lines[i]!.length + 1;
  return at + column - 1;
}

function makeSelection(s: Sel) {
  return {
    ...s,
    isEmpty: () => s.startLineNumber === s.endLineNumber && s.startColumn === s.endColumn,
    containsPosition: (p: { lineNumber: number; column: number }) =>
      offset(p.lineNumber, p.column) >= offset(s.startLineNumber, s.startColumn) &&
      offset(p.lineNumber, p.column) <= offset(s.endLineNumber, s.endColumn),
  };
}

vi.mock("../monaco", () => ({
  languageFor: () => "plaintext",
  monacoThemeForApp: () => "vs-dark",
  syncMonacoTheme: () => {},
  loadLanguage: async () => {},
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
          getValueInRange: (r: Sel) =>
            state.value.slice(
              offset(r.startLineNumber, r.startColumn),
              offset(r.endLineNumber, r.endColumn),
            ),
          getWordAtPosition: (p: { lineNumber: number; column: number }) => {
            const line = state.value.split("\n")[p.lineNumber - 1] ?? "";
            for (const m of line.matchAll(/\w+/g)) {
              const start = (m.index ?? 0) + 1;
              const end = start + m[0].length;
              if (p.column >= start && p.column <= end) {
                return { word: m[0], startColumn: start, endColumn: end };
              }
            }
            return null;
          },
          getValueLength: () => state.value.length,
          getFullModelRange: () => {
            const lines = state.value.split("\n");
            return {
              startLineNumber: 1,
              startColumn: 1,
              endLineNumber: lines.length,
              endColumn: lines[lines.length - 1]!.length + 1,
            };
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
        onContextMenu: (listener: (e: unknown) => void) => {
          state.contextMenu = listener;
          return { dispose: () => (state.contextMenu = null) };
        },
        getSelection: () => (state.selection ? makeSelection(state.selection) : null),
        setSelection: (s: Sel) => {
          state.selection = { ...s };
        },
        setPosition: (p: { lineNumber: number; column: number }) => {
          state.selection = {
            startLineNumber: p.lineNumber,
            startColumn: p.column,
            endLineNumber: p.lineNumber,
            endColumn: p.column,
          };
        },
        executeEdits: (source: string, edits: { range: Sel; text: string }[]) => {
          for (const edit of edits) {
            state.edits.push({ source, range: edit.range, text: edit.text });
            const from = offset(edit.range.startLineNumber, edit.range.startColumn);
            const to = offset(edit.range.endLineNumber, edit.range.endColumn);
            state.value = state.value.slice(0, from) + edit.text + state.value.slice(to);
          }
          return true;
        },
        pushUndoStop: () => true,
        addCommand: () => {},
        updateOptions: () => {},
        layout: () => {},
        dispose: () => {},
        focus: () => {},
      }),
    },
  }),
}));

const clipboard = vi.hoisted(() => ({
  writeClipboardText: vi.fn(async (_text: string) => undefined),
  readClipboardText: vi.fn(async () => "PASTED"),
}));
vi.mock("../clipboard", () => clipboard);

import Editor from "../Editor";
import { api, type FileRead } from "../api";
import { openEditorTab } from "../tabs";

const PATH = "src/touch.txt";
const TAB_ID = `edit:${PATH}`;

function diskRead(content: string): FileRead {
  return {
    path: PATH,
    size: content.length,
    content,
    content_base64: null,
    is_binary: false,
    mtime: 1000,
    hash: "hash-a",
  };
}

function setCoarsePointer(coarse: boolean) {
  Object.defineProperty(window, "matchMedia", {
    configurable: true,
    writable: true,
    value: (query: string) => ({
      matches: coarse && query === "(pointer: coarse)",
      media: query,
      addEventListener: () => {},
      removeEventListener: () => {},
      addListener: () => {},
      removeListener: () => {},
      onchange: null,
      dispatchEvent: () => false,
    }),
  });
}

/** A long-press on line `lineNumber`, column `column`, as Monaco reports it. */
function longPress(lineNumber: number, column: number) {
  state.contextMenu?.({ target: { position: { lineNumber, column } }, event: {} });
}

const originalMatchMedia = window.matchMedia;

beforeEach(() => {
  localStorage.clear();
  state.value = "";
  state.selection = null;
  state.contextMenu = null;
  state.edits = [];
  clipboard.writeClipboardText.mockClear();
  clipboard.readClipboardText.mockClear();
  clipboard.readClipboardText.mockResolvedValue("PASTED");
  openEditorTab(PATH);
  vi.spyOn(api, "readFile").mockResolvedValue(diskRead("alpha copyme beta\nsecond line\n"));
});

afterEach(() => {
  vi.restoreAllMocks();
  window.matchMedia = originalMatchMedia;
});

describe("Editor touch copy/paste (WI-1591)", () => {
  it("a long-press selects the word under the finger and Copy puts it on the clipboard", async () => {
    setCoarsePointer(true);
    render(() => <Editor tabId={TAB_ID} path={PATH} />);
    await waitFor(() => expect(state.contextMenu).not.toBeNull());

    longPress(1, 9); // inside "copyme"
    const bar = await screen.findByRole("toolbar", { name: "Edit" });
    await fireEvent.click(within(bar).getByRole("button", { name: "Copy" }));

    await waitFor(() => expect(clipboard.writeClipboardText).toHaveBeenCalledWith("copyme"));
    expect(await screen.findByText("Copied to clipboard")).toBeVisible();
    expect(screen.queryByRole("toolbar", { name: "Edit" })).not.toBeInTheDocument();
  });

  it("Paste replaces the selection with the clipboard text", async () => {
    setCoarsePointer(true);
    render(() => <Editor tabId={TAB_ID} path={PATH} />);
    await waitFor(() => expect(state.contextMenu).not.toBeNull());

    longPress(2, 2); // inside "second"
    const bar = await screen.findByRole("toolbar", { name: "Edit" });
    await fireEvent.click(within(bar).getByRole("button", { name: "Paste" }));

    await waitFor(() => expect(state.value).toBe("alpha copyme beta\nPASTED line\n"));
    expect(clipboard.readClipboardText).toHaveBeenCalledTimes(1);
  });

  it("Select all then Cut copies the whole buffer and empties it", async () => {
    setCoarsePointer(true);
    render(() => <Editor tabId={TAB_ID} path={PATH} />);
    await waitFor(() => expect(state.contextMenu).not.toBeNull());

    longPress(1, 1);
    const bar = await screen.findByRole("toolbar", { name: "Edit" });
    await fireEvent.click(within(bar).getByRole("button", { name: "Select all" }));
    await fireEvent.click(within(bar).getByRole("button", { name: "Cut" }));

    await waitFor(() =>
      expect(clipboard.writeClipboardText).toHaveBeenCalledWith("alpha copyme beta\nsecond line\n"),
    );
    await waitFor(() => expect(state.value).toBe(""));
  });

  it("a failed clipboard read is reported, not swallowed, and the buffer is untouched", async () => {
    setCoarsePointer(true);
    clipboard.readClipboardText.mockRejectedValueOnce(new Error("Android clipboard read timed out"));
    render(() => <Editor tabId={TAB_ID} path={PATH} />);
    await waitFor(() => expect(state.contextMenu).not.toBeNull());

    longPress(2, 2);
    const bar = await screen.findByRole("toolbar", { name: "Edit" });
    await fireEvent.click(within(bar).getByRole("button", { name: "Paste" }));

    expect(
      await screen.findByText("Paste failed: Android clipboard read timed out"),
    ).toBeVisible();
    expect(state.value).toBe("alpha copyme beta\nsecond line\n");
  });

  it("a fine pointer keeps Monaco's own context menu: no listener, no edit bar", async () => {
    setCoarsePointer(false);
    render(() => <Editor tabId={TAB_ID} path={PATH} />);
    // Wait until the editor exists (the loading marker clears once it does).
    await waitFor(() => expect(screen.queryByText("loading…")).not.toBeInTheDocument());
    expect(state.value).toBe("alpha copyme beta\nsecond line\n");
    expect(state.contextMenu).toBeNull();
    expect(screen.queryByRole("toolbar", { name: "Edit" })).not.toBeInTheDocument();
  });
});
