// FR-U21's other half: the engine is away, not Vogt.
//
// §6.2a: "The mirror of the outage tests that exist. Every one of them takes
// Vogt away; none takes the engine away, which in a test is no harder and in
// the product is the only half a person can reach."
//
// **What "the engine is away" means here**, because the two servers are not
// symmetrical and a test that got this backwards would be asserting a fiction.
// Every Vogt read in this PWA goes through the engine's front door, so an
// engine that is not running takes the Vogt views with it and there is nothing
// to design an absent state for. The case FR-U21 names is the other one: the
// front door answers, and the *session* engine behind it cannot be reached.
// Vogt reports that in its own answer — `sessions.list` carries an `engine`
// note saying why it could not ask — and every existing test sets that field
// to `null`, which is the engine present on every single run.
//
// So this file sets it. Two claims, and they pull in opposite directions:
//
//   1. The Vogt views keep answering. An engine outage must not empty a board
//      or grey out an item page: the estate is Vogt's and Vogt is fine.
//   2. The session controls stop, and say the engine's own reason. A start
//      button that posts into a void is worse than one that is disabled, and
//      a session Vogt cannot ask about must not be drawn as finished.
//
// The engine's own API is answered by nothing here, which is `harness.tsx`'s
// default and means these surfaces are tested against a genuinely absent
// engine rather than a mocked-out one.

import { describe, expect, it } from "vitest";
import { fireEvent, waitFor } from "@solidjs/testing-library";

import AuditBrowser from "../AuditBrowser";
import Backlog from "../Backlog";
import Board from "../Board";
import Projects from "../Projects";
import WorkItemDetail from "../WorkItemDetail";
import {
  auditRecord,
  fakeVogt,
  freshness,
  mountAt,
  rankedEntry,
  workItem,
} from "./harness";

/** What vogt-core says in `sessions.list` when it could not reach the engine. */
const NO_ENGINE = "engine unreachable: connection refused (http://engine:8080)";

/** A session Vogt has a record of, whatever the engine can say about it. */
const RECORDED_SESSION = {
  id: "ses_01",
  engine_session_id: "eng-1",
  work_item: "WI-1",
  project: "alpha",
  actor: "agent:session:ses_01",
  reason: "start the migration",
  cwd: "/srv/alpha",
  template: "claude",
  started_at: "2026-08-01T00:00:00Z",
  stopped_at: null,
  // Both null because the engine could not be asked — not because the
  // session is over. That distinction is the whole of clause 2.
  activity: null,
  alive: null,
};

/** Vogt answering fully, with the engine unreachable behind it. */
function vogtWithoutEngine(over: Record<string, unknown> = {}) {
  return fakeVogt({
    "GET /sessions": { body: { sessions: [RECORDED_SESSION], engine: NO_ENGINE } },
    "GET /work/get": {
      body: { item: workItem(), comments: [], sessions: [RECORDED_SESSION] },
    },
    ...over,
  });
}

function detail(itemRef = "WI-1") {
  return mountAt(`/w/${itemRef}`, `/w/${itemRef}`, () => (
    <WorkItemDetail itemRef={itemRef} />
  ));
}

/** The panel under a heading, or a failure that names the missing panel. */
function panelNamed(container: HTMLElement, heading: string): HTMLElement {
  const found = [...container.querySelectorAll<HTMLElement>(".wid-panel")].find(
    (node) => node.querySelector("h3")?.textContent === heading,
  );
  if (!found) throw new Error(`the item page has no "${heading}" panel`);
  return found;
}

describe("FR-U21 — an engine outage does not take the Vogt views with it", () => {
  it("draws the board from Vogt's answer, and calls nothing an outage", async () => {
    const vogt = vogtWithoutEngine({
      "GET /work": { body: { items: [workItem({ ref: "WI-1" })], total: 1 } },
    });
    const { container } = mountAt("/board", "/board", () => <Board />);

    await waitFor(() => expect(container.querySelector("#board-card-WI-1")).toBeTruthy());
    expect(container.querySelector(".board-banner--outage")).toBeNull();
    expect(container.textContent).not.toContain("Vogt is not answering");
    // The board never asks the engine anything, which is why it survives.
    expect(vogt.engineCalls.filter((call) => call.path.startsWith("/api/sessions"))).toEqual(
      [],
    );
  });

  it("draws the ranked backlog, with the ranking Vogt computed", async () => {
    vogtWithoutEngine({
      "GET /backlog": {
        body: { items: [rankedEntry({ ref: "WI-1" })], freshness: freshness() },
      },
    });
    const { container } = mountAt("/backlog", "/backlog", () => <Backlog />);

    await waitFor(() =>
      expect(container.querySelectorAll(".vogt-backlog-row")).toHaveLength(1),
    );
    expect(container.querySelector(".vogt-backlog-outage")).toBeNull();
    expect(container.textContent).toContain("4.25");
  });

  it("draws the project page and the audit log, which are Vogt's records alone", async () => {
    vogtWithoutEngine({
      "GET /audit": { body: { records: [auditRecord()], total: 1 } },
    });
    const projects = mountAt("/projects", "/projects?p=alpha", () => <Projects />);
    await waitFor(() =>
      expect(projects.container.textContent).toContain("alpha"),
    );
    expect(projects.container.querySelector(".vogt-projects-outage")).toBeNull();
    projects.unmount();

    const audit = mountAt("/audit", "/audit", () => <AuditBrowser />);
    await waitFor(() =>
      expect(audit.container.textContent).toContain("work.transition"),
    );
    expect(audit.container.textContent).toContain("the board said it was started");
    expect(audit.container.textContent).not.toContain("Vogt is not answering");
  });

  it("keeps every panel of the item page, and its Vogt writes with them", async () => {
    vogtWithoutEngine();
    const { container } = detail();

    await waitFor(() =>
      expect(container.textContent).toContain(
        "Teach the board to say what it does not know",
      ),
    );
    // The estate's own record is untouched: an engine outage is not a reason
    // to stop letting somebody correct a title.
    const edit = container.querySelector<HTMLButtonElement>(".wid-edit-open")!;
    await waitFor(() => expect(edit.disabled).toBe(false));
    expect(container.textContent).not.toContain("Vogt cannot be reached");
  });
});

describe("FR-U21 — the session controls stop, and say the engine's own reason", () => {
  it("disables the start-session form and names why", async () => {
    const vogt = vogtWithoutEngine();
    const { container } = detail();

    const sessions = await waitFor(() => panelNamed(container, "Sessions"));

    // The engine's sentence, carried through Vogt's answer to the screen.
    await waitFor(() => expect(sessions.textContent).toContain(NO_ENGINE));
    expect(sessions.textContent).toContain("The engine could not be asked");

    const blocked = [...sessions.querySelectorAll(".wid-blocked")].map(
      (node) => node.textContent ?? "",
    );
    expect(blocked.length).toBeGreaterThan(0);
    expect(blocked.every((note) => note.startsWith("Session controls are disabled:"))).toBe(
      true,
    );
    expect(blocked.some((note) => note.includes(NO_ENGINE))).toBe(true);

    // Every control that would write a session is refused, not merely
    // decorated: the reason box will not take a keystroke and the button
    // will not submit.
    const start = [...sessions.querySelectorAll<HTMLElement>(".wid-start form")][0]!;
    const reason = start.querySelector<HTMLInputElement>('input[type="text"]')!;
    expect(reason.disabled).toBe(true);
    expect(start.querySelector<HTMLButtonElement>("button[type=submit]")!.disabled).toBe(
      true,
    );

    fireEvent.submit(start);
    expect(vogt.matching("POST /sessions/start")).toEqual([]);
    expect(vogt.calls.filter((call) => call.method !== "GET")).toEqual([]);
  });

  it("will not stop a session it cannot see the state of", async () => {
    const vogt = vogtWithoutEngine();
    const { container } = detail();
    const sessions = await waitFor(() => panelNamed(container, "Sessions"));

    const open = await waitFor(() => {
      const button = [...sessions.querySelectorAll("button")].find(
        (node) => node.textContent?.trim() === "Stop session…",
      );
      expect(button, "the recorded session offers no stop control at all").toBeTruthy();
      return button!;
    });
    fireEvent.click(open);

    const form = await waitFor(() => {
      const found = [...sessions.querySelectorAll<HTMLFormElement>("form.wid-form")].find(
        (node) =>
          node.querySelector<HTMLInputElement>('input[type="text"]')?.placeholder ===
          "why this session should stop",
      );
      expect(found).toBeTruthy();
      return found!;
    });
    expect(form.querySelector<HTMLButtonElement>("button[type=submit]")!.disabled).toBe(true);
    expect(form.textContent).toContain("Session controls are disabled:");

    fireEvent.submit(form);
    expect(vogt.calls.filter((call) => call.method !== "GET")).toEqual([]);
  });

  it("calls the session's activity unknown, and never finished", async () => {
    vogtWithoutEngine();
    const { container } = detail();
    const sessions = await waitFor(() => panelNamed(container, "Sessions"));

    // Vogt's record of what it started is still listed — the outage is about
    // activity, not about existence.
    await waitFor(() => expect(sessions.querySelectorAll(".wid-session")).toHaveLength(1));
    expect(sessions.querySelector(".wid-mono")?.textContent).toBe("ses_01");
    // and the terminal is still offered, because Vogt never stopped it
    expect(sessions.querySelector(".wid-open-terminal")?.getAttribute("href")).toBe(
      "#/t/eng-1",
    );

    // The badge is the claim, and the only honest one available: not
    // "stopped", which is Vogt's own fact, and not "not running", which
    // would be the engine's answer to a question nobody could ask it.
    const badge = sessions.querySelector<HTMLElement>(".wid-activity")!;
    expect(badge.textContent).toBe("activity unknown");
    expect(badge.className).toContain("wid-activity--unknown");
    expect(badge.title).toContain("The engine could not be asked");
    expect(badge.title).toContain("not the same as it having finished");
  });

  it("is not the sentence it would use if Vogt were the thing that was away", async () => {
    // The two outages read differently on purpose: one of them means the
    // estate below is true and stale in one field, the other means there is
    // no estate on screen at all.
    vogtWithoutEngine();
    const { container } = detail();
    const sessions = await waitFor(() => panelNamed(container, "Sessions"));

    await waitFor(() => expect(sessions.textContent).toContain(NO_ENGINE));
    expect(sessions.textContent).not.toContain("Vogt cannot be reached");
    expect(sessions.textContent).not.toContain("nothing could be read");
    expect(container.querySelector(".wid-outage")).toBeNull();
  });
});
