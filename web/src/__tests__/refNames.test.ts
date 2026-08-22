import { describe, expect, it } from "vitest";

import { actorName, projectName } from "../refNames";

const actors = [
  { identity_ref: "user:ada", display_name: "Ada Lovelace" },
  { identity_ref: "agent:opus", display_name: "Opus" },
  { identity_ref: "user:blank", display_name: "" },
];

const projects = [
  { slug: "vogt", name: "Vogt" },
  { slug: "cadastre", name: "Cadastre" },
  { slug: "nameless", name: "" },
];

describe("actorName", () => {
  it("resolves a known ref to its display name", () => {
    expect(actorName(actors, "user:ada")).toBe("Ada Lovelace");
  });

  it("falls back to the raw ref when the ref is unknown", () => {
    expect(actorName(actors, "user:ghost")).toBe("user:ghost");
  });

  it("falls back to the raw ref when the display name is blank", () => {
    expect(actorName(actors, "user:blank")).toBe("user:blank");
  });

  it("returns an empty ref unchanged", () => {
    expect(actorName(actors, "")).toBe("");
  });

  it("falls back to the raw ref when nothing has loaded yet", () => {
    expect(actorName([], "agent:opus")).toBe("agent:opus");
  });
});

describe("projectName", () => {
  it("resolves a known slug to its name", () => {
    expect(projectName(projects, "vogt")).toBe("Vogt");
  });

  it("falls back to the raw slug when the slug is unknown", () => {
    expect(projectName(projects, "unknown")).toBe("unknown");
  });

  it("falls back to the raw slug when the name is blank", () => {
    expect(projectName(projects, "nameless")).toBe("nameless");
  });

  it("falls back to the raw slug when nothing has loaded yet", () => {
    expect(projectName([], "vogt")).toBe("vogt");
  });
});
