import { describe, expect, it } from "vitest";

import {
  describeRoute,
  documentTitleForRoute,
  isCurrentPlace,
  isCurrentTool,
  isRestorableRoute,
  settingsReturnRoute,
} from "../routeModel";

const capabilities = {
  configReady: true,
  sessionsState: "ready" as const,
  sessionExists: (id: string) => id === "known",
  assistantEnabled: false,
  guiAvailable: false,
};

describe("route truth", () => {
  it("maps primary and secondary routes to one current place", () => {
    expect(isCurrentPlace(describeRoute("/board", capabilities), "board")).toBe(true);
    const history = describeRoute("/history", capabilities);
    expect(isCurrentPlace(history, "sessions")).toBe(true);
    expect(isCurrentTool(history, "history")).toBe(true);
  });

  it.each([
    ["/sessions", "sessions", null],
    ["/inbox", "inbox", null],
    ["/board", "board", null],
    ["/backlog", "backlog", null],
    ["/projects", "projects", null],
    ["/audit", "audit", null],
    ["/w/WI-7", "board", null],
    ["/t/known", "sessions", "terminal"],
    ["/e/src%2Fmain.ts", "sessions", "editor"],
    ["/g", "sessions", "git"],
    ["/history", "sessions", "history"],
    ["/tasks", "sessions", "tasks"],
    ["/assistant", "sessions", "assistant"],
    ["/gui", "sessions", "gui"],
  ] as const)("maps %s to %s / %s", (path, place, tool) => {
    const outcome = describeRoute(path, {
      ...capabilities,
      assistantEnabled: true,
      guiAvailable: true,
    });
    expect(outcome?.place).toBe(place);
    expect(outcome?.kind === "tool" ? outcome.tool : null).toBe(tool);
  });

  it("distinguishes missing terminals and disabled capabilities", () => {
    expect(describeRoute("/t/missing", capabilities)).toMatchObject({
      kind: "not-found",
      title: "Session not found",
    });
    expect(describeRoute("/assistant", capabilities)).toMatchObject({
      kind: "unavailable",
      title: "Assistant is unavailable",
    });
    expect(describeRoute("/gui", capabilities)).toMatchObject({
      kind: "unavailable",
      title: "GUI stream is unavailable",
    });
  });

  it("retains configured capabilities and Settings return candidates", () => {
    expect(describeRoute("/assistant/chat", {
      ...capabilities,
      assistantEnabled: true,
    })).toMatchObject({ kind: "tool", tool: "assistant" });
    expect(describeRoute("/gui", { ...capabilities, guiAvailable: true })).toMatchObject({
      kind: "tool",
      tool: "gui",
    });
    expect(isRestorableRoute("/board?project=vogt")).toBe(true);
    expect(isRestorableRoute("/settings")).toBe(false);
    expect(isRestorableRoute("/not-a-route")).toBe(false);
    expect(settingsReturnRoute("/settings", "/board?project=vogt"))
      .toBe("/board?project=vogt");
    expect(describeRoute("/settings", capabilities, "/history")).toMatchObject({
      kind: "settings",
      place: "sessions",
      tool: "history",
    });
  });

  it.each([
    ["/board", "Board · Vogt"],
    ["/history", "History · Vogt"],
    ["/e/src%2Fmain.ts", "Editor · Vogt"],
    ["/w/WI-7", "WI-7 · Vogt"],
    ["/settings", "Settings · Vogt"],
  ])("derives the %s title from route truth", (path, title) => {
    const outcome = describeRoute(path, capabilities, "/board");
    expect(documentTitleForRoute(outcome, path)).toBe(title);
  });

  it("titles route outcomes without a second pathname table", () => {
    const missing = describeRoute("/t/missing", capabilities);
    expect(documentTitleForRoute(missing, "/t/missing")).toBe("Session not found · Vogt");
    expect(documentTitleForRoute(null, "/not-a-route")).toBe("Not found · Vogt");
  });
});
