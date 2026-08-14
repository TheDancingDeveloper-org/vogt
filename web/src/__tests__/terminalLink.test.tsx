// FR-U20's return leg: a terminal opened for a work item links back to it.
//
// The forward leg — badge and open-terminal control on the item page — is in
// `workItemDetail.test.tsx`. This is the half §6.2a listed as asserted by
// nothing, and it was untestable for a smaller reason than it looked: xterm
// asks for `matchMedia` at mount and jsdom has none, so the terminal could
// not be mounted at all. `setup.ts` stubs it, the way it already stubs
// `ResizeObserver`.
//
// What is asserted here is the link and its silences. The terminal must keep
// working whatever Vogt is doing (FR-E9), so every failure mode of this badge
// is "no badge", never "no terminal".
import { describe, expect, it } from "vitest";
import { render, waitFor } from "@solidjs/testing-library";

import Terminal from "../Terminal";
import { fakeVogt, settle, unavailable } from "./harness";

const SESSION = {
  id: "ses_01",
  engine_session_id: "eng-1",
  work_item: "WI-7",
  work_item_id: "wi_01",
  project_id: "prj_01",
  actor: "agent:session:ses_01",
  reason: "start the migration",
  cwd: "/srv/alpha",
  template: "claude",
  started_at: "2026-08-01T00:00:00Z",
  stopped_at: null,
  activity: "running",
  alive: true,
};

function terminal(sessionId = "eng-1") {
  return render(() => <Terminal sessionId={sessionId} />);
}

describe("FR-U20 — a terminal says which work item it was opened for", () => {
  it("links back to the item, by the ref a person types", async () => {
    fakeVogt({ "GET /sessions": { body: { sessions: [SESSION], engine: null } } });
    const { container } = terminal();
    const link = await waitFor(() => {
      const el = container.querySelector<HTMLAnchorElement>(".terminal-work-link");
      expect(el).toBeTruthy();
      return el!;
    });
    expect(link.getAttribute("href")).toBe("#/w/WI-7");
    expect(link.textContent).toContain("WI-7");
  });

  it("asks Vogt including stopped sessions, because a finished run still had a subject", async () => {
    const vogt = fakeVogt({
      "GET /sessions": { body: { sessions: [SESSION], engine: null } },
    });
    terminal();
    await waitFor(() => expect(vogt.matching("GET /sessions").length).toBe(1));
    expect(vogt.matching("GET /sessions")[0]!.query.get("include_stopped")).toBe(
      "true",
    );
  });

  it("says nothing for a PTY Vogt did not start", async () => {
    // The engine knows this PTY and nothing about why it exists. A terminal
    // opened by hand has no work item, and inventing one would be worse than
    // an absent badge.
    fakeVogt({ "GET /sessions": { body: { sessions: [SESSION], engine: null } } });
    const { container } = terminal("eng-other");
    await settle();
    expect(container.querySelector(".terminal-work-link")).toBeNull();
  });

  it("keeps the terminal when Vogt cannot be asked at all", async () => {
    // FR-E9: the badge is worth having and worth nothing at all if it costs
    // the terminal.
    fakeVogt({ "GET /sessions": unavailable("vogt-core is not configured") });
    const { container } = terminal();
    await settle();
    expect(container.querySelector(".terminal-work-link")).toBeNull();
    expect(container.querySelector(".terminal-host")).toBeTruthy();
  });
});
