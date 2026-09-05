// #600: retargeting one pane's session must change exactly that pane and leave
// every other pane mounted — same xterm instance, scrollback and socket. This
// is proven at the component level: the untouched pane's DOM node keeps its
// identity across the retarget (a remount would replace it), and the workspace
// spawns no session (retarget is a pure tree transform, #212).

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
const GAMMA = session({ id: "gamma", name: "gamma-shell" });

async function seed(...sessions: SessionSummary[]) {
  const vogt = fakeVogt({}, { "GET /api/sessions": { body: sessions } });
  await refreshSessions();
  await settle();
  return vogt;
}

function paneEls(container: HTMLElement): HTMLElement[] {
  return Array.from(container.querySelectorAll<HTMLElement>(".terminal-pane"));
}

function paneSelects(container: HTMLElement): HTMLSelectElement[] {
  return Array.from(
    container.querySelectorAll<HTMLSelectElement>(".terminal-pane-session"),
  );
}

function createPosts(vogt: ReturnType<typeof fakeVogt>): number {
  return vogt.engineCalls.filter(
    (call) => call.method === "POST" && call.path === "/api/sessions",
  ).length;
}

describe("retargeting a pane's session (#600)", () => {
  beforeEach(() => localStorage.clear());

  it("changes only the retargeted pane and leaves the other one mounted", async () => {
    const vogt = await seed(ALPHA, BETA, GAMMA);
    const { container } = render(() => (
      <TerminalWorkspace tabId="tab-alpha" sessionId="alpha" />
    ));
    await waitFor(() => expect(paneEls(container).length).toBe(1));

    // Compose a two-pane split: alpha | beta.
    fireEvent.click(screen.getByRole("button", { name: "Split right" }));
    fireEvent.click(await screen.findByRole("button", { name: /beta-shell/ }));
    await waitFor(() => expect(paneEls(container).length).toBe(2));

    // Each pane header shows the session it renders.
    await waitFor(() => {
      const values = paneSelects(container).map((s) => s.value);
      expect(values).toEqual(["alpha", "beta"]);
    });

    // Capture the beta pane's DOM node — a remount would replace it.
    const betaPaneBefore = paneEls(container)[1]!;

    // Retarget the first pane (alpha) at the unshown session gamma.
    const alphaSelect = paneSelects(container)[0]!;
    fireEvent.change(alphaSelect, { target: { value: "gamma" } });

    // The first pane now shows gamma; the second still shows beta.
    await waitFor(() => {
      const values = paneSelects(container).map((s) => s.value);
      expect(values).toEqual(["gamma", "beta"]);
    });

    // The untouched beta pane kept its exact DOM node — not torn down and
    // rebuilt — so its terminal, scrollback and socket survived.
    const betaPaneAfter = paneEls(container)[1]!;
    expect(betaPaneAfter).toBe(betaPaneBefore);
    expect(betaPaneBefore.isConnected).toBe(true);

    // Retarget is a pure tree transform: no session was created (#212).
    expect(createPosts(vogt)).toBe(0);
  });

  it("shows the real session in the dropdown so a first selection is honoured", async () => {
    // #600 symptom 1: the control must display the session its pane actually
    // renders, or choosing that session reads as a no-op. After a split the
    // second pane's select reflects beta, not the first option (alpha).
    await seed(ALPHA, BETA, GAMMA);
    const { container } = render(() => (
      <TerminalWorkspace tabId="tab-alpha" sessionId="alpha" />
    ));
    await waitFor(() => expect(paneEls(container).length).toBe(1));

    fireEvent.click(screen.getByRole("button", { name: "Split right" }));
    fireEvent.click(await screen.findByRole("button", { name: /beta-shell/ }));
    await waitFor(() => expect(paneEls(container).length).toBe(2));

    await waitFor(() => {
      expect(paneSelects(container)[1]!.value).toBe("beta");
    });
  });
});
