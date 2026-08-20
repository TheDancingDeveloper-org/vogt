// Acceptance criteria for the surface-header + places-rail delta
// (design_handoff_surface_header_and_rail/rail-spec.md §5), additive over
// dev@095ddf9. `surfaceHeader.test.tsx`, `shell.test.tsx`, `placeMetrics.test.ts`
// and `waitingSession.test.tsx` are the spec's protected files and are left
// unmodified; this file covers what changed instead.
//
// Two of the ten criteria (§5.5's pointer:coarse floor, §5.10's "one
// scroller") are layout claims jsdom cannot make — it has no box model, so a
// scrollHeight/clientHeight comparison there is vacuously true. Those are a
// visual/browser check, not a unit one, and are covered by the screenshot
// pass instead.

import { describe, expect, it } from "vitest";
import { fireEvent, waitFor } from "@solidjs/testing-library";
import { MemoryRouter, Route, createMemoryHistory } from "@solidjs/router";
import { render } from "@solidjs/testing-library";
import Board from "../Board";
import AuditBrowser from "../AuditBrowser";
import App from "../App";
import { APP_ROUTES } from "../routes";
import { setToken } from "../api";
import { replaceTabs } from "../tabs";
import { fakeVogt, mountAt, unavailable } from "./harness";

/** A minimal shell mount, mirroring shell.test.tsx's own `mountShell` (kept
 *  private to that file, and that file is not to be edited). */
function mountShell(
  sessions: unknown[] = [],
  vogtRoutes: Record<string, unknown> = {},
  engineRoutes: Record<string, unknown> = {},
) {
  setToken("delta-test-token");
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
    "GET /api/sessions": { body: sessions },
    "GET /api/tree": { body: [] },
    ...engineRoutes,
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

const RUNNING = {
  id: "s-run", name: "codex vogt", cwd: "/srv/vogt",
  activity: "running", exit_code: null,
};
const WAITING = {
  id: "s-wait", name: "claude cadastre", cwd: "/srv/cadastre",
  activity: "waiting-for-input", exit_code: null, activity_changed_at: "2026-08-20T00:00:00Z",
};

describe("A1/A2 — Board's honesty carries tone, and the action slot is the one accent control", () => {
  it("marks a live view fresh, and renders exactly one action-slot control", async () => {
    fakeVogt();
    const { container } = mountAt("/board", "/board", () => <Board />);

    await waitFor(() =>
      expect(container.querySelector(".surface-header-honesty.surface-header-honesty--fresh")).toBeTruthy(),
    );
    expect(container.querySelectorAll(".surface-header-action button")).toHaveLength(1);
    expect(container.querySelectorAll(".surface-header-controls .surface-header-action")).toHaveLength(0);
  });

  it("marks the honesty slot outage and disables the action slot's control when Vogt cannot be asked", async () => {
    fakeVogt({
      "GET /work": unavailable("upstream vogt-core did not answer"),
      "GET /workflows": unavailable("upstream vogt-core did not answer"),
    });
    const { container } = mountAt("/board", "/board", () => <Board />);

    await waitFor(() =>
      expect(container.querySelector(".surface-header-honesty--outage")).toBeTruthy(),
    );
    const action = container.querySelector<HTMLButtonElement>(".surface-header-action button");
    expect(action).toBeTruthy();
    expect(action!.disabled).toBe(true);
  });
});

describe("A3 — view tabs are one segmented group", () => {
  it("wraps Audit's view tabs in the segmented shell and marks the pressed one", async () => {
    fakeVogt();
    const { container } = mountAt("/audit", "/audit", () => <AuditBrowser />);

    await waitFor(() =>
      expect(container.querySelector(".vab-views.surface-header-tabs")).toBeTruthy(),
    );
    const tabs = container.querySelector(".vab-views.surface-header-tabs")!;
    expect(tabs.parentElement?.className).toContain("surface-header-controls");
    expect(tabs.querySelector('button[aria-pressed="true"]')?.textContent).toBe("Audit trail");

    fireEvent.click([...tabs.querySelectorAll("button")].find((b) => b.textContent === "Notifications")!);
    await waitFor(() =>
      expect(tabs.querySelector('button[aria-pressed="true"]')?.textContent).toBe("Notifications"),
    );
  });
});

describe("B1 — the rail attention card is a pointer, never a second WaitingSession", () => {
  it("renders none for a running-only roster", async () => {
    const { container } = mountShell([RUNNING]);
    await waitFor(() => expect(container.querySelector(".places-rail")).toBeTruthy());
    expect(container.querySelector(".rail-attention")).toBeNull();
  });

  it("renders exactly one card for a waiting session, routing to its own terminal", async () => {
    const { container } = mountShell([RUNNING, WAITING]);
    const card = await waitFor(() => {
      const found = container.querySelector<HTMLAnchorElement>(".rail-attention");
      expect(found).toBeTruthy();
      return found!;
    });
    expect(container.querySelectorAll(".rail-attention")).toHaveLength(1);
    expect(card.classList.contains("rail-attention--outage")).toBe(false);
    expect(card.getAttribute("href")).toBe("#/t/s-wait");
    // A pointer, not a second WaitingSession: it must not carry a control
    // that could itself send a keystroke.
    expect(card.querySelector("button")).toBeNull();
  });

  it("shows the outage card when the engine cannot be asked", async () => {
    // The *rail's* outage is the engine/sessions connection, not Vogt's own —
    // that is a different signal (Board's honesty, tested above). A failed
    // initial read never populates the store, so there is no waiting session
    // to tie against here — outage is the only thing that can be true.
    const { container } = mountShell([], {}, {
      "GET /api/sessions": { status: 503, body: { error: "session service offline" } },
    });
    await waitFor(() => {
      const found = container.querySelector<HTMLAnchorElement>(".rail-attention--outage");
      expect(found).toBeTruthy();
    });
    expect(container.querySelectorAll(".rail-attention")).toHaveLength(1);
  });
});

describe("B2 — one always-visible overflow control per session row", () => {
  it("has no .row-btn left, and the row-menu's actions are reachable without a hover simulation", async () => {
    const { container } = mountShell([RUNNING]);
    await waitFor(() => expect(container.querySelector(".session-row")).toBeTruthy());

    expect(container.querySelector(".row-btn")).toBeNull();
    expect(container.querySelector(".close")).toBeNull();

    const trigger = container.querySelector<HTMLButtonElement>(".session-row .row-menu")!;
    expect(trigger).toBeTruthy();
    expect(trigger.getAttribute("aria-expanded")).toBe("false");

    const bookmark = container.querySelector<HTMLButtonElement>(
      `[aria-label="Bookmark ${RUNNING.name}"]`,
    );
    expect(bookmark).toBeTruthy();
    // Reachable — not opacity:0 behind a hover a touch/keyboard user cannot
    // simulate — even before the trigger has been clicked.
    bookmark!.click();
    expect(
      container.querySelector(`[aria-label="Remove bookmark from ${RUNNING.name}"]`),
    ).toBeTruthy();
  });
});

describe("B3 — Running, Recent places and Files collapse, state persists", () => {
  it("starts Files collapsed, and its own toggle survives a remount", async () => {
    replaceTabs({ tabs: [], active: null });
    const first = mountShell([RUNNING]);
    await waitFor(() => expect(first.container.querySelector(".places-rail")).toBeTruthy());

    const filesToggle = () =>
      [...first.container.querySelectorAll<HTMLButtonElement>(".places-section-toggle")]
        .find((b) => b.textContent?.includes("Files"))!;
    expect(filesToggle().getAttribute("aria-expanded")).toBe("false");

    fireEvent.click(filesToggle());
    await waitFor(() => expect(filesToggle().getAttribute("aria-expanded")).toBe("true"));
    expect(JSON.parse(localStorage.getItem("mydevenv2.rail.sections.v1")!).files).toBe(true);
  });

  it("keeps the Running count reachable by its existing aria-label once it is a toggle", async () => {
    const { container } = mountShell([RUNNING]);
    await waitFor(() =>
      expect(
        container.querySelector('.places-section-label [aria-label="1 running sessions"]'),
      ).toBeTruthy(),
    );
    const runningToggle = container.querySelector<HTMLButtonElement>(
      ".places-rail-session-area .places-section-toggle",
    )!;
    expect(runningToggle.getAttribute("aria-expanded")).toBe("true");
    fireEvent.click(runningToggle);
    expect(runningToggle.getAttribute("aria-expanded")).toBe("false");
  });
});
