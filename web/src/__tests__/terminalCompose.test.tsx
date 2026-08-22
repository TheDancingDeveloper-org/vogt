// #212: a split can compose a session that already exists, and closing a pane
// detaches it without killing the shell. Both are proven here at the component
// level by the calls the workspace does *not* make — no POST to create a
// session, no DELETE to remove one — since that is the whole point of the
// change: composing and detaching must never reach the create/kill endpoints.
//
// The workspace mounts a real `<Terminal>` per pane (xterm needs the
// `matchMedia`/`ResizeObserver` stubs `setup.ts` installs); the fake under
// `fetch` records every engine call, so "created nothing" is an assertion about
// the request table, not about a mock of the store.

import { describe, expect, it, beforeEach } from "vitest";
import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";

import TerminalWorkspace from "../TerminalWorkspace";
import { refreshSessions, sessionsStore } from "../store";
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

/** Seed the session store from the engine list, the way the app does on load. */
async function seed(...sessions: SessionSummary[]) {
  const vogt = fakeVogt({}, { "GET /api/sessions": { body: sessions } });
  await refreshSessions();
  await settle();
  return vogt;
}

function createPosts(vogt: ReturnType<typeof fakeVogt>): number {
  return vogt.engineCalls.filter(
    (call) => call.method === "POST" && call.path === "/api/sessions",
  ).length;
}

function sessionDeletes(vogt: ReturnType<typeof fakeVogt>): number {
  return vogt.engineCalls.filter(
    (call) => call.method === "DELETE" && /^\/api\/sessions\/[^/]+$/.test(call.path),
  ).length;
}

function panes(container: HTMLElement): number {
  return container.querySelectorAll(".terminal-pane").length;
}

describe("composing existing sessions into a split (#212)", () => {
  beforeEach(() => {
    localStorage.clear();
  });

  it("splits with an existing session and creates none", async () => {
    const vogt = await seed(ALPHA, BETA);
    const { container } = render(() => (
      <TerminalWorkspace tabId="tab-alpha" sessionId="alpha" />
    ));
    await waitFor(() => expect(panes(container)).toBe(1));

    // "Split right" opens the picker because an eligible session exists.
    fireEvent.click(screen.getByRole("button", { name: "Split right" }));
    const option = await screen.findByRole("button", { name: /beta-shell/ });
    fireEvent.click(option);

    await waitFor(() => expect(panes(container)).toBe(2));
    // The compose path spawned nothing: no session was created.
    expect(createPosts(vogt)).toBe(0);
  });

  it("closes a pane by detaching it, without killing its session", async () => {
    const vogt = await seed(ALPHA, BETA);
    const { container } = render(() => (
      <TerminalWorkspace tabId="tab-alpha" sessionId="alpha" />
    ));
    await waitFor(() => expect(panes(container)).toBe(1));

    fireEvent.click(screen.getByRole("button", { name: "Split right" }));
    fireEvent.click(await screen.findByRole("button", { name: /beta-shell/ }));
    await waitFor(() => expect(panes(container)).toBe(2));

    // "Close pane" now detaches: the layout drops back to one pane…
    fireEvent.click(screen.getByRole("button", { name: "Close pane" }));
    await waitFor(() => expect(panes(container)).toBe(1));

    // …and the detached session is neither deleted nor gone from the store.
    expect(sessionDeletes(vogt)).toBe(0);
    expect(sessionsStore.sessions["beta"]).toBeTruthy();
  });
});
