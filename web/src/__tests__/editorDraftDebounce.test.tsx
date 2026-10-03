import { render, waitFor } from "@solidjs/testing-library";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// A Monaco stand-in that hands the test the content-change listener and
// counts how often the editor materialises the whole document (#538).
const monacoState = vi.hoisted(() => ({
  value: "",
  getValueCalls: 0,
  onChange: null as null | (() => void),
}));

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
        monacoState.value = content;
        return {
          getValueLength: () => monacoState.value.length,
          setValue: (v: string) => {
            monacoState.value = v;
          },
          dispose: () => {},
          uri: {},
        };
      },
      create: () => ({
        getValue: () => {
          monacoState.getValueCalls += 1;
          return monacoState.value;
        },
        saveViewState: () => null,
        restoreViewState: () => {},
        onDidChangeModelContent: (listener: () => void) => {
          monacoState.onChange = listener;
          return { dispose: () => {} };
        },
        addCommand: () => {},
        updateOptions: () => {},
        layout: () => {},
        dispose: () => {},
        focus: () => {},
      }),
    },
  }),
}));

import Editor from "../Editor";
import { api, type FileRead } from "../api";
import { readEditorDraft } from "../editorDrafts";
import { openEditorTab, tabsStore } from "../tabs";

const PATH = "src/big.txt";
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

const dirty = () => {
  const tab = tabsStore.tabs.find((t) => t.id === TAB_ID);
  return tab?.kind === "editor" && Boolean(tab.dirty);
};

function type(next: string) {
  monacoState.value = next;
  monacoState.onChange?.();
}

beforeEach(() => {
  localStorage.clear();
  vi.restoreAllMocks();
  monacoState.getValueCalls = 0;
  monacoState.onChange = null;
});

afterEach(() => {
  vi.useRealTimers();
});

describe("Editor draft capture", () => {
  it("captures the draft once typing pauses, not on every keystroke", async () => {
    openEditorTab(PATH);
    vi.spyOn(api, "readFile").mockResolvedValue(diskRead("abc"));
    render(() => <Editor tabId={TAB_ID} path={PATH} />);
    await waitFor(() => expect(monacoState.onChange).not.toBeNull());

    vi.useFakeTimers();
    const before = monacoState.getValueCalls;
    type("abcd");
    type("abcde");
    type("abcdef");

    // A length change is dirty at once, without materialising the document.
    expect(dirty()).toBe(true);
    expect(monacoState.getValueCalls).toBe(before);
    expect(readEditorDraft(TAB_ID, PATH)).toBeNull();

    vi.advanceTimersByTime(300);
    expect(monacoState.getValueCalls).toBe(before + 1);
    expect(readEditorDraft(TAB_ID, PATH)?.content).toBe("abcdef");
  });

  it("settles an equal-length edit exactly, and back-to-disk is clean", async () => {
    openEditorTab(PATH);
    vi.spyOn(api, "readFile").mockResolvedValue(diskRead("abc"));
    render(() => <Editor tabId={TAB_ID} path={PATH} />);
    await waitFor(() => expect(monacoState.onChange).not.toBeNull());

    vi.useFakeTimers();
    type("xyz");
    vi.advanceTimersByTime(300);
    expect(dirty()).toBe(true);
    expect(readEditorDraft(TAB_ID, PATH)?.content).toBe("xyz");

    type("abc");
    vi.advanceTimersByTime(300);
    expect(dirty()).toBe(false);
    expect(readEditorDraft(TAB_ID, PATH)).toBeNull();
  });
});
