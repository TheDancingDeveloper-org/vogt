import { describe, expect, it } from "vitest";
import {
  hasDirtyEditor,
  hasUnsavedWork,
  MAX_LIVE_TERMINALS,
  protectDirtyEditorExit,
  shouldMountTab,
  terminalPaneParked,
  tabRetention,
  touchRecentTerminal,
} from "../tabLifecycle";
import type { Tab } from "../tabs";

const terminal: Tab = {
  id: "term:one",
  kind: "terminal",
  sessionId: "one",
  label: "one",
};
const editor: Tab = {
  id: "edit:src/app.ts",
  kind: "editor",
  path: "src/app.ts",
  label: "app.ts",
  dirty: true,
};
const history: Tab = { id: "history", kind: "history", label: "History" };
const tasks: Tab = { id: "tasks", kind: "tasks", label: "Tasks", dirty: true };

describe("Sessions tab resource policy", () => {
  it("parks every inactive workspace pane but keeps the selected pane live", () => {
    expect(terminalPaneParked(true, true)).toBe(false);
    expect(terminalPaneParked(true, false)).toBe(true);
    expect(terminalPaneParked(false, true)).toBe(true);
  });

  it("retains every terminal but only the active non-terminal tool", () => {
    expect(tabRetention(terminal)).toBe("always");
    expect(tabRetention(editor)).toBe("active");
    expect(tabRetention(history)).toBe("active");
    expect(tabRetention(tasks)).toBe("always");
    expect(shouldMountTab(terminal, "history")).toBe(true);
    expect(shouldMountTab(editor, "history")).toBe(false);
    expect(shouldMountTab(history, "history")).toBe(true);
  });

  it("bounds a many-tab workspace to its terminals plus one active tool", () => {
    const tabs: Tab[] = Array.from({ length: 100 }, (_, index) => ({
      id: `git:repo-${index}`,
      kind: "git" as const,
      repo: `repo-${index}`,
      label: `repo-${index}`,
    }));
    tabs.push(terminal, {
      id: "term:two",
      kind: "terminal",
      sessionId: "two",
      label: "two",
    });

    expect(tabs.filter((tab) => shouldMountTab(tab, "git:repo-72")))
      .toHaveLength(3);
  });
});

describe("dirty editor browser lifecycle", () => {
  it("keeps the historical editor predicate narrow", () => {
    expect(hasDirtyEditor([history, { ...editor, dirty: false }])).toBe(false);
    expect(hasDirtyEditor([history, editor, terminal])).toBe(true);
    expect(hasDirtyEditor([history, tasks])).toBe(false);
  });

  it("recognises unsaved work across editors and task drafts", () => {
    expect(hasUnsavedWork([history, { ...tasks, dirty: false }])).toBe(false);
    expect(hasUnsavedWork([history, tasks])).toBe(true);
    expect(hasUnsavedWork([history, editor])).toBe(true);
  });

  it("cancels beforeunload using browser-owned confirmation copy", () => {
    const event = new Event("beforeunload", { cancelable: true }) as BeforeUnloadEvent;
    protectDirtyEditorExit(event);
    expect(event.defaultPrevented).toBe(true);
    expect(event.returnValue).toBe(false);
  });
});

/** WI-3129: an xterm and its WebGL context per visited session leaked without a cap. */
describe("live terminal cap", () => {
  const term = (name: string): Tab => ({
    id: `term:${name}`,
    kind: "terminal",
    sessionId: name,
    label: name,
  });
  const tabs: Tab[] = [term("a"), term("b"), term("c"), term("d"), term("e"), editor];

  it("keeps the most recently shown terminals, up to the cap", () => {
    let live: string[] = [];
    for (const id of ["term:a", "term:b", "term:c", "term:d", "term:e"]) {
      live = touchRecentTerminal(live, id, tabs);
    }
    expect(live).toEqual(["term:e", "term:d", "term:c", "term:b"]);
    expect(live).toHaveLength(MAX_LIVE_TERMINALS);
  });

  it("moves a re-shown terminal to the front without duplicating it", () => {
    const live = touchRecentTerminal(["term:b", "term:a"], "term:a", tabs);
    expect(live).toEqual(["term:a", "term:b"]);
  });

  it("drops the ids of closed terminals", () => {
    const open = [term("a"), term("b")];
    expect(touchRecentTerminal(["term:gone", "term:a"], "term:b", open)).toEqual([
      "term:b",
      "term:a",
    ]);
  });

  it("leaves the set alone when a non-terminal tab is shown", () => {
    expect(touchRecentTerminal(["term:a"], "edit:src/app.ts", tabs)).toEqual(["term:a"]);
  });

  it("caps live terminals at the dormant socket budget", () => {
    expect(MAX_LIVE_TERMINALS).toBe(4);
  });
});
