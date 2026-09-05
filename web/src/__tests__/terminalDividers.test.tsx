// #601: a split renders a draggable divider between its panes, and the
// keyboard path (arrow keys / double-click reset) moves the split ratio and
// persists it. The pointer drag itself needs real element geometry that jsdom
// does not provide, so it is exercised by hand; here the reachable, geometry-
// free paths are covered, plus the flex-grow weights the sizes produce.

import { describe, expect, it, beforeEach } from "vitest";
import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";

import TerminalWorkspace from "../TerminalWorkspace";
import { refreshSessions } from "../store";
import { fakeVogt, settle } from "./harness";
import type { SessionSummary } from "../api";

function session(over: Partial<SessionSummary> & { id: string }): SessionSummary {
  return {
    name: over.id,
    activity: "idle",
    exit_code: null,
    scrollback_bytes: 0,
    cwd: "",
    created_at: "2026-08-22T00:00:00Z",
    ...over,
  };
}

const ALPHA = session({ id: "alpha", name: "alpha-shell" });
const BETA = session({ id: "beta", name: "beta-shell" });

async function seed(...sessions: SessionSummary[]) {
  const vogt = fakeVogt({}, { "GET /api/sessions": { body: sessions } });
  await refreshSessions();
  await settle();
  return vogt;
}

function paneEls(container: HTMLElement): HTMLElement[] {
  return Array.from(container.querySelectorAll<HTMLElement>(".terminal-pane"));
}

function divider(container: HTMLElement): HTMLElement {
  return container.querySelector<HTMLElement>(".terminal-split-divider")!;
}

function grow(el: HTMLElement): number {
  return Number.parseFloat(el.style.flexGrow || "0");
}

async function splitTwo(container: HTMLElement) {
  await waitFor(() => expect(paneEls(container).length).toBe(1));
  fireEvent.click(screen.getByRole("button", { name: "Split right" }));
  fireEvent.click(await screen.findByRole("button", { name: /beta-shell/ }));
  await waitFor(() => expect(paneEls(container).length).toBe(2));
}

describe("split divider resize (#601)", () => {
  beforeEach(() => localStorage.clear());

  it("renders one separator between two panes, equal to start", async () => {
    await seed(ALPHA, BETA);
    const { container } = render(() => (
      <TerminalWorkspace tabId="tab-alpha" sessionId="alpha" />
    ));
    await splitTwo(container);

    const separators = container.querySelectorAll(".terminal-split-divider");
    expect(separators.length).toBe(1);
    expect(divider(container).getAttribute("role")).toBe("separator");
    // Equal shares until dragged.
    const [a, b] = paneEls(container);
    expect(grow(a!)).toBeCloseTo(0.5);
    expect(grow(b!)).toBeCloseTo(0.5);
  });

  it("arrow keys move the ratio toward the pressed side and persist it", async () => {
    await seed(ALPHA, BETA);
    const { container } = render(() => (
      <TerminalWorkspace tabId="tab-alpha" sessionId="alpha" />
    ));
    await splitTwo(container);

    const bar = divider(container);
    fireEvent.keyDown(bar, { key: "ArrowRight" });
    fireEvent.keyDown(bar, { key: "ArrowRight" });

    await waitFor(() => {
      const [a, b] = paneEls(container);
      expect(grow(a!)).toBeGreaterThan(0.5);
      expect(grow(b!)).toBeLessThan(0.5);
    });

    // Ratios ride along in the persisted layout.
    const saved = JSON.parse(localStorage.getItem("vogt.terminalLayouts.v1")!);
    const sizes = saved["tab-alpha"].root.sizes as number[];
    expect(sizes[0]).toBeGreaterThan(sizes[1]!);
  });

  it("Home and double-click reset the neighbours to equal", async () => {
    await seed(ALPHA, BETA);
    const { container } = render(() => (
      <TerminalWorkspace tabId="tab-alpha" sessionId="alpha" />
    ));
    await splitTwo(container);

    const bar = divider(container);
    fireEvent.keyDown(bar, { key: "ArrowRight" });
    await waitFor(() => expect(grow(paneEls(container)[0]!)).toBeGreaterThan(0.5));

    fireEvent.keyDown(bar, { key: "Home" });
    await waitFor(() => {
      expect(grow(paneEls(container)[0]!)).toBeCloseTo(0.5);
      expect(grow(paneEls(container)[1]!)).toBeCloseTo(0.5);
    });

    fireEvent.keyDown(bar, { key: "ArrowLeft" });
    await waitFor(() => expect(grow(paneEls(container)[0]!)).toBeLessThan(0.5));
    fireEvent.dblClick(bar);
    await waitFor(() => {
      expect(grow(paneEls(container)[0]!)).toBeCloseTo(0.5);
      expect(grow(paneEls(container)[1]!)).toBeCloseTo(0.5);
    });
  });
});
