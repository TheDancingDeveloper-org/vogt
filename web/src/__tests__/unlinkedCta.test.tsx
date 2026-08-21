// The link-or-publish CTA (#183). An unlinked project has no work surface:
// the server answers a project scope with empty items and a machine-readable
// `link_state: "unlinked"` marker, and the Backlog and Board must render the
// way forward — link or publish — rather than an empty state that reads as
// "there is nothing to do". These are the web half of the repurposed
// forge-less honesty tests; the server half lives in
// `tests/test_unlinked_surfaces.py`.

import { describe, expect, it } from "vitest";
import { waitFor } from "@solidjs/testing-library";
import Backlog from "../Backlog";
import Board from "../Board";
import { fakeVogt, freshness, mountAt, rankedEntry } from "./harness";

function mountBacklog(url = "/backlog?project=folder") {
  return mountAt("/backlog", url, () => <Backlog />);
}

function mountBoard(url = "/board?project=folder") {
  return mountAt("/board", url, () => <Board />);
}

describe("#183 — an unlinked project scope renders the link/publish CTA", () => {
  it("the Backlog shows the CTA with the migration count, not a bare empty state", async () => {
    fakeVogt({
      "GET /backlog": {
        body: {
          items: [],
          total_considered: 0,
          declared: 0,
          observed: 0,
          link_state: "unlinked",
          excluded_unlinked: 2,
          scope: "folder",
          freshness: freshness(),
        },
      },
    });
    const { container } = mountBacklog();

    const banner = await waitFor(() => {
      const found = container.querySelector<HTMLElement>(".board-banner--candidates");
      expect(found).toBeTruthy();
      return found!;
    });
    expect(banner.textContent).toContain("not linked to a forge");
    expect(banner.textContent).toContain("forge publish");
    expect(banner.textContent).toContain("2 existing native items will migrate");
    expect(banner.querySelector('a[href="#/projects"]')).toBeTruthy();
  });

  it("the Backlog stays quiet for a linked or global answer", async () => {
    fakeVogt({
      "GET /backlog": {
        body: {
          items: [rankedEntry()],
          total_considered: 1,
          link_state: null,
          freshness: freshness(),
        },
      },
    });
    const { container } = mountBacklog("/backlog");
    await waitFor(() => {
      expect(container.querySelector(".vogt-backlog-row")).toBeTruthy();
    });
    expect(container.textContent).not.toContain("not linked to a forge");
  });

  it("the Board shows the CTA for an unlinked project scope", async () => {
    fakeVogt({
      "GET /work": {
        body: {
          items: [],
          total: 0,
          link_state: "unlinked",
          excluded_unlinked: 1,
        },
      },
    });
    const { container } = mountBoard();

    const banner = await waitFor(() => {
      const found = container.querySelector<HTMLElement>(".board-banner--candidates");
      expect(found).toBeTruthy();
      return found!;
    });
    expect(banner.textContent).toContain("no Board");
    expect(banner.textContent).toContain("forge publish");
    expect(banner.textContent).toContain("1 existing native item will migrate");
    expect(banner.querySelector('a[href="#/projects"]')).toBeTruthy();
  });
});
