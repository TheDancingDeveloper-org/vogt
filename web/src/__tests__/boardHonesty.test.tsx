// The Board is honest about what it is not showing, and the rail agrees with
// it (#187). `list_board` draws declared cards only, so a Board of two beside
// a Backlog of many silently read as "the estate has two things". The server
// now carries the candidate population; these tests pin that the Board says so
// and that the left-rail Board count matches the population the Board renders.

import { describe, expect, it } from "vitest";
import { waitFor } from "@solidjs/testing-library";
import { MemoryRouter, Route, createMemoryHistory } from "@solidjs/router";
import { render } from "@solidjs/testing-library";
import Board from "../Board";
import App from "../App";
import { APP_ROUTES } from "../routes";
import { setToken } from "../api";
import { replaceTabs } from "../tabs";
import { fakeVogt, mountAt, workItem } from "./harness";

function board(url = "/board") {
  return mountAt("/board", url, () => <Board />);
}

/** A minimal shell mount, mirroring surfaceHeaderRailDelta.test.tsx's own. */
function mountShell(vogtRoutes: Record<string, unknown> = {}) {
  setToken("honesty-test-token");
  replaceTabs({ tabs: [], active: null });
  const vogt = fakeVogt(vogtRoutes as never, {
    "GET /api/status": { body: { ok: true } },
    "GET /api/config": {
      body: {
        version: "test",
        gui_stream_url: null,
        gui_stream_available: false,
        assistant_enabled: false,
        vogt: { configured: true },
      },
    },
    "GET /api/sessions": { body: [] },
    "GET /api/tree": { body: [] },
  });
  const history = createMemoryHistory();
  history.set({ value: "/board" });
  const rendered = render(() => (
    <MemoryRouter history={history}>
      <Route path={[...APP_ROUTES]} component={App} />
    </MemoryRouter>
  ));
  return { container: rendered.container, vogt };
}

describe("#187 — the Board admits the candidates it does not draw", () => {
  it("shows the honesty banner when there are more candidates than declared cards", async () => {
    fakeVogt({
      "GET /work": {
        body: {
          items: [
            workItem({ ref: "WI-1", state: "open" }),
            workItem({ ref: "WI-2", state: "open" }),
          ],
          total: 2,
          declared_total: 2,
          backlog_candidates: 159,
        },
      },
    });
    const { container } = board();

    const banner = await waitFor(() => {
      const found = container.querySelector<HTMLElement>(".board-banner--candidates");
      expect(found).toBeTruthy();
      return found!;
    });
    // The server's numbers, verbatim: two on the Board, 159 candidates.
    expect(banner.textContent).toContain("2 work items on the Board");
    expect(banner.textContent).toContain("159 backlog candidates");
    // 159 - 2 observed subjects, and a pointer to where they do rank.
    expect(banner.textContent).toContain("157");
    expect(banner.querySelector('a[href="#/backlog"]')).toBeTruthy();
  });

  it("stays quiet when the candidate population equals the declared one", async () => {
    fakeVogt({
      "GET /work": {
        body: {
          items: [workItem({ ref: "WI-1", state: "open" })],
          total: 1,
          declared_total: 1,
          backlog_candidates: 1,
        },
      },
    });
    const { container } = board();

    await waitFor(() =>
      expect(container.querySelector(".board-summary")).toBeTruthy(),
    );
    expect(container.querySelector(".board-banner--candidates")).toBeNull();
  });

  it("does not fabricate a count when the server omits the field", async () => {
    // An older server that never learned `backlog_candidates`: the banner is
    // silent rather than reading the missing field as zero candidates.
    fakeVogt({
      "POST /board/list": {
        body: {
          cells: [{ lane_key: "", state: "open", items: [workItem()], total: 1 }],
          column_totals: { open: 1 },
          lane_totals: { "": 1 },
          total: 1,
          snapshot: "s",
          snapshot_at: "2026-08-17T10:01:00Z",
          revision: 1,
        },
      },
      "GET /work": { body: { items: [workItem()], total: 1 } },
    });
    const { container } = board();

    await waitFor(() =>
      expect(container.querySelector(".board-summary")).toBeTruthy(),
    );
    expect(container.querySelector(".board-banner--candidates")).toBeNull();
  });
});

describe("#187 — the rail Board count matches the population the Board renders", () => {
  it("shows the same number in the rail as the Board's own 'of N matching'", async () => {
    const { container } = mountShell({
      "GET /work": {
        body: {
          items: [
            workItem({ ref: "WI-1", state: "open" }),
            workItem({ ref: "WI-2", state: "open" }),
          ],
          total: 2,
          declared_total: 2,
          backlog_candidates: 159,
        },
      },
    });

    // The rail's Board count settles to the declared board total.
    const railCount = await waitFor(() => {
      const found = container.querySelector<HTMLElement>(
        '.place-count[aria-label$="Board work items"]',
      );
      expect(found).toBeTruthy();
      expect(found!.textContent).toBe("2");
      return found!;
    });

    // And the Board surface renders the same population in its summary.
    await waitFor(() => {
      const summary = container.querySelector(".board-summary");
      expect(summary?.textContent).toContain("of 2 matching");
    });
    expect(railCount.textContent).toBe("2");
  });
});
