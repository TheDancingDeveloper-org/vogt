import { describe, expect, it } from "vitest";
import {
  hasDirtyEditor,
  hasUnsavedWork,
  protectDirtyEditorExit,
  MAX_LIVE_TERMINAL_TABS,
  shouldMountTab,
  touchRecentTerminal,
  terminalPaneParked,
  tabRetention,
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

describe("Live terminal bound (WI-3129)", () => {
  const terminals: Tab[] = Array.from({ length: 20 }, (_, i) => ({
    id: `term:${i}`,
    kind: "terminal" as const,
    sessionId: String(i),
    label: String(i),
  }));

  it("keeps only the most recent terminals in the recency list", () => {
    let recent: string[] = [];
    for (const t of terminals) recent = touchRecentTerminal(recent, t.id);
    expect(recent).toEqual(["term:19", "term:18", "term:17"]);
    expect(recent).toHaveLength(MAX_LIVE_TERMINAL_TABS);
    expect(touchRecentTerminal(recent, "term:17")[0]).toBe("term:17");
  });

  it("mounts (so unmounts and disposes the rest) at most the bound after 20 switches", () => {
    let recent: string[] = [];
    for (const t of terminals) recent = touchRecentTerminal(recent, t.id);
    const live = new Set(recent);
    const mounted = terminals.filter((t) =>
      shouldMountTab(t, "term:19", false, live),
    );
    expect(mounted.map((t) => t.id).sort()).toEqual(
      ["term:17", "term:18", "term:19"],
    );
  });
});
