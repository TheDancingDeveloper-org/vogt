import { afterEach, describe, expect, it, vi } from "vitest";
import {
  clearTaxonomyCache,
  clearTaxonomy,
  noteTaxonomyChange,
  taxonomy,
} from "../taxonomyCache";
import { fakeVogt, held } from "./harness";

const EMPTY = {
  "GET /projects": { body: { projects: [], total: 0 } },
  "GET /actors": { body: { actors: [] } },
  "GET /workflows": { body: { workflows: [] } },
  "GET /labels": { body: { labels: [] } },
  "GET /initiatives": { body: { initiatives: [] } },
};

afterEach(() => {
  clearTaxonomyCache();
  vi.useRealTimers();
});

describe("shared taxonomy reads", () => {
  it("single-flights concurrent typed reads and memoizes the answer", async () => {
    const pending = held();
    const vogt = fakeVogt({ ...EMPTY, "GET /projects": pending.handler });
    const first = taxonomy.projects({ limit: 200 });
    const second = taxonomy.projects({ limit: 200 });

    await pending.asked;
    expect(vogt.matching("GET /projects")).toHaveLength(1);
    pending.answer({ body: { projects: [], total: 0 } });
    await Promise.all([first, second]);
    await taxonomy.projects({ limit: 200 });

    expect(vogt.matching("GET /projects")).toHaveLength(1);
  });

  it("canonicalizes parameter order while keeping distinct pages separate", async () => {
    const vogt = fakeVogt(EMPTY);
    await taxonomy.projects({ limit: 200, offset: 0 });
    await taxonomy.projects({ offset: 0, limit: 200 });
    await taxonomy.projects({ limit: 100, offset: 0 });

    expect(vogt.matching("GET /projects")).toHaveLength(2);
  });

  it("invalidates only the affected entity kind", async () => {
    const vogt = fakeVogt(EMPTY);
    await taxonomy.projects();
    await taxonomy.actors();
    await taxonomy.labels();

    clearTaxonomy("projects");
    await taxonomy.projects();
    await taxonomy.actors();
    await taxonomy.labels();

    expect(vogt.matching("GET /projects")).toHaveLength(2);
    expect(vogt.matching("GET /actors")).toHaveLength(1);
    expect(vogt.matching("GET /labels")).toHaveLength(1);
  });

  it("revalidates after a matching sequence and ignores unrelated entities", async () => {
    const vogt = fakeVogt(EMPTY);
    await taxonomy.projects();
    await taxonomy.actors();

    noteTaxonomyChange("work_item", 20);
    await taxonomy.projects();
    expect(vogt.matching("GET /projects")).toHaveLength(1);

    noteTaxonomyChange("project", 21);
    await taxonomy.projects();
    await taxonomy.actors();
    expect(vogt.matching("GET /projects")).toHaveLength(2);
    expect(vogt.matching("GET /actors")).toHaveLength(1);
  });

  it("does not reuse a cached answer after the credential changes", async () => {
    const vogt = fakeVogt(EMPTY);
    await taxonomy.projects();
    localStorage.setItem("vogt.token", "another-identity");
    await taxonomy.projects();

    expect(vogt.matching("GET /projects")).toHaveLength(2);
  });

  it("does not cache failures", async () => {
    let failed = true;
    const vogt = fakeVogt({
      ...EMPTY,
      "GET /projects": () => {
        if (failed) {
          failed = false;
          return { status: 503, body: { error: { message: "offline" } } };
        }
        return EMPTY["GET /projects"];
      },
    });
    await expect(taxonomy.projects()).rejects.toThrow();
    await expect(taxonomy.projects()).resolves.toEqual({ projects: [], total: 0 });
    expect(vogt.matching("GET /projects")).toHaveLength(2);
  });
});
