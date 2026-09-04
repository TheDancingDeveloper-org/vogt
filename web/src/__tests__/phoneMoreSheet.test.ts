import { describe, expect, it } from "vitest";

import { moreSheetItems, moreSheetPlaces } from "../phoneMoreSheet";

const labels = (input: Parameters<typeof moreSheetItems>[0]) =>
  moreSheetItems(input).map((item) => item.label);

describe("the phone More sheet inventory", () => {
  it("always lists the always-on places plus Settings and Sign out", () => {
    const items = moreSheetItems({
      vogtConfigured: false,
      guiEnabled: false,
      assistantEnabled: false,
    });
    expect(items.map((item) => item.label)).toEqual([
      "Git",
      "Files",
      "History",
      "Tasks",
      "Settings",
      "Sign out",
    ]);
    // Settings and Sign out are actions, not navigation links.
    const settings = items.find((item) => item.label === "Settings");
    const signOut = items.find((item) => item.label === "Sign out");
    expect(settings).toEqual({ kind: "action", id: "settings", label: "Settings" });
    expect(signOut).toEqual({ kind: "action", id: "signout", label: "Sign out" });
  });

  it("gates Projects and Audit behind a configured Vogt core, like the rail", () => {
    expect(labels({ vogtConfigured: false, guiEnabled: false, assistantEnabled: false }))
      .not.toContain("Projects");
    const configured = labels({
      vogtConfigured: true,
      guiEnabled: false,
      assistantEnabled: false,
    });
    expect(configured).toContain("Projects");
    expect(configured).toContain("Audit");
    // Estate places lead, ahead of the Machine group.
    expect(configured.indexOf("Projects")).toBeLessThan(configured.indexOf("Git"));
  });

  it("shows the GUI stream row only when the stream is available", () => {
    expect(labels({ vogtConfigured: true, guiEnabled: false, assistantEnabled: false }))
      .not.toContain("GUI stream");
    expect(labels({ vogtConfigured: true, guiEnabled: true, assistantEnabled: false }))
      .toContain("GUI stream");
  });

  it("shows the Assistant row only when the assistant is enabled", () => {
    expect(labels({ vogtConfigured: true, guiEnabled: false, assistantEnabled: false }))
      .not.toContain("Assistant");
    expect(labels({ vogtConfigured: true, guiEnabled: false, assistantEnabled: true }))
      .toContain("Assistant");
  });

  it("routes each place at the same hash the desktop rail uses", () => {
    const byId = new Map(
      moreSheetPlaces({ vogtConfigured: true, guiEnabled: true, assistantEnabled: true })
        .map((place) => [place.id, place.href]),
    );
    expect(byId.get("projects")).toBe("#/projects");
    expect(byId.get("audit")).toBe("#/audit");
    expect(byId.get("git")).toBe("#/g");
    expect(byId.get("files")).toBe("#/files");
    expect(byId.get("history")).toBe("#/history");
    expect(byId.get("tasks")).toBe("#/tasks");
    expect(byId.get("gui")).toBe("#/gui");
    expect(byId.get("assistant")).toBe("#/assistant");
  });

  it("with everything on, reaches every non-bar place plus both actions", () => {
    expect(labels({ vogtConfigured: true, guiEnabled: true, assistantEnabled: true })).toEqual([
      "Projects",
      "Audit",
      "Git",
      "Files",
      "History",
      "Tasks",
      "GUI stream",
      "Assistant",
      "Settings",
      "Sign out",
    ]);
  });
});
