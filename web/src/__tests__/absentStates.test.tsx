// FR-U21, on all five Vogt surfaces, as something rendered rather than read.
//
// `tests/test_pwa.py::test_every_vogt_surface_distinguishes_an_outage_from_
// emptiness` asserts the structural precondition — that each surface imports
// `VogtUnavailable` and mentions `.message` somewhere — and says in its own
// docstring what it cannot do: "What it cannot check is whether the resulting
// copy is any good. That is in the M11 demo, and the demo needs a browser."
//
// These do not need a browser. They mount each surface against a front door
// that answers 503 the way the real one does, and assert three things the
// source-reading check cannot see:
//
//   1. The server's own sentence reaches the screen, verbatim.
//   2. The surface says what the absence *means* — that this is an outage and
//      not an empty estate — because a bare error string next to an empty
//      table still reads as "there is nothing here".
//   3. Nothing is rendered as data. An empty view under an outage is the
//      exact failure FR-U21 exists to prevent.
//
// The board, the backlog and the item page each carry their own outage tests
// in their own files, next to the interactions they belong to. These two are
// the remaining surfaces.

import { describe, expect, it } from "vitest";
import { waitFor } from "@solidjs/testing-library";
import AuditBrowser from "../AuditBrowser";
import Projects from "../Projects";
import { fakeVogt, mountAt, refusal, unavailable } from "./harness";

/** What the front door says when it has no core configured. */
const NO_CORE = "vogt-core is not configured for this front door";
/** And what it says when it has one that did not answer. */
const NO_ANSWER = "upstream vogt-core did not answer within 5s";

describe("FR-U21 — the projects surface reports the outage", () => {
  function projects(url = "/projects") {
    return mountAt("/projects", url, () => <Projects />);
  }

  it("renders Vogt's own reason instead of an empty estate", async () => {
    fakeVogt({ "GET /projects": unavailable(NO_CORE) });
    const { container } = projects();

    await waitFor(() =>
      expect(container.querySelector(".vogt-projects-outage")).toBeTruthy(),
    );
    expect(container.textContent).toContain("Vogt is not answering");
    expect(container.textContent).toContain(NO_CORE);
    expect(container.textContent).toContain(
      "Nothing is shown because nothing was read",
    );
  });

  it("keeps a failed read apart from an outage", async () => {
    fakeVogt({ "GET /projects": refusal(500, "project.list: the index is corrupt") });
    const { container } = projects();

    await waitFor(() =>
      expect(container.querySelector(".vogt-projects-outage")).toBeTruthy(),
    );
    // Vogt answered, so this is not "Vogt is not answering".
    expect(container.textContent).toContain("Projects failed to load");
    expect(container.textContent).toContain("project.list: the index is corrupt");
    expect(container.textContent).not.toContain("Vogt is not answering");
  });

  it("reports a per-panel outage on the panel it belongs to", async () => {
    // The estate lists; the brief behind one project does not. The surface
    // must not report the whole page as gone, nor the project as empty.
    fakeVogt({ "GET /projects/brief": unavailable(NO_ANSWER) });
    const { container } = projects("/projects?p=alpha");

    await waitFor(() =>
      expect(container.querySelector(".vogt-projects-outage")).toBeTruthy(),
    );
    expect(container.textContent).toContain(NO_ANSWER);
    // The page is still the page: the project the URL names is still the one
    // open, and the surface's own navigation is still there. What is missing
    // is the brief, and the brief is what says it is missing.
    expect(container.querySelector(".vogt-projects-crumb.active")?.textContent).toBe(
      "alpha",
    );
    expect(container.textContent).toContain("Drift inbox");
  });
});

describe("FR-U21 — the audit browser reports the outage", () => {
  function audit(url = "/audit") {
    return mountAt("/audit", url, () => <AuditBrowser />);
  }

  it("says why there are no records, in Vogt's words", async () => {
    fakeVogt({ "GET /audit": unavailable(NO_CORE) });
    const { container } = audit();

    await waitFor(() => expect(container.querySelector(".vab-outage")).toBeTruthy());
    expect(container.textContent).toContain("Vogt is not answering");
    expect(container.textContent).toContain(NO_CORE);
    // The sentence that matters most on this surface of the five: an empty
    // audit log is itself a claim.
    expect(container.textContent).toContain(
      "An empty audit log would say nothing has ever been written here",
    );
  });

  it("lists no records at all while it cannot read them", async () => {
    fakeVogt({ "GET /audit": unavailable(NO_ANSWER) });
    const { container } = audit();

    await waitFor(() => expect(container.querySelector(".vab-outage")).toBeTruthy());
    expect(container.querySelectorAll(".vab-row")).toHaveLength(0);
  });

  it("calls a failed read a failed read", async () => {
    fakeVogt({ "GET /audit": refusal(500, "audit.list: no such actor") });
    const { container } = audit();

    await waitFor(() => expect(container.querySelector(".vab-outage")).toBeTruthy());
    expect(container.textContent).toContain("The audit log could not be read");
    expect(container.textContent).toContain("audit.list: no such actor");
    expect(container.textContent).not.toContain("Vogt is not answering");
  });
});
