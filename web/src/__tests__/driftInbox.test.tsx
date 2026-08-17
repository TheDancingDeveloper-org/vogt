// The drift inbox's ordering rule (FR-U18).
//
// §6.2a: "A mount of the drift inbox against a proposal carrying an evidence
// snapshot. That bulk accept cannot arrive is asserted from source; that the
// evidence is *shown first* is not asserted anywhere."
//
// The requirement is unusual in being about *order*: "both sides of the
// disagreement, with provenance and age, **before** any act is possible". A
// list of accept buttons over one-line summaries would satisfy every other
// clause in FR-U18 and defeat the whole of it — so the assertions below are
// about what is on screen and where it is relative to the controls, not about
// whether a panel exists somewhere on the page.
//
// `projectPage.test.tsx` mounts this inbox for FR-U7 and asserts that the
// proposal Vogt returned is in it. Neither that nor anything else looks at
// what a reader would have to do to reach the evidence, which is the clause.

import { describe, expect, it } from "vitest";
import { fireEvent, waitFor } from "@solidjs/testing-library";

import Projects from "../Projects";
import { driftProposal, fakeVogt, freshness, mountAt, type Routes } from "./harness";

/** The inbox, against whatever `GET /drift` is made to answer. */
function inbox(routes: Routes = {}) {
  const vogt = fakeVogt({
    "GET /drift": {
      body: { proposals: [driftProposal()], human_gated: {}, freshness: freshness() },
    },
    ...routes,
  });
  const mounted = mountAt("/projects", "/projects?view=drift", () => <Projects />);
  return { vogt, ...mounted };
}

/** The one proposal card on screen, once it has arrived. */
async function card(container: HTMLElement): Promise<HTMLElement> {
  return waitFor(() => {
    const found = container.querySelector<HTMLElement>(".vogt-projects-drift");
    expect(found, "no drift proposal is on screen").toBeTruthy();
    return found!;
  });
}

/** The two evidence panels, declared first and observed second. */
function sides(card: HTMLElement): HTMLElement[] {
  return [...card.querySelectorAll<HTMLElement>(".vogt-projects-side")];
}

/** A proposal whose evidence names a collector, a subject and a time. */
const CARRIED = driftProposal({
  evidence_observation_id: "obs_01",
  evidence_snapshot: {
    subject_key: "project:alpha",
    collector: "git",
    content_digest: "sha256:9f1c2b3d4e5f60718293a4b5c6d7e8f9",
    observed_at: "2026-08-01T00:00:00Z",
    payload: { version: "1.3.0", branch: "main" },
  },
  proposed_change: { from: "1.2.0", to: "1.3.0" },
});

describe("FR-U18 — both sides, with provenance and age, before anything can be done", () => {
  it("shows the declared value against the observed one, each said to come from somewhere", async () => {
    const { container } = inbox({
      "GET /drift": {
        body: { proposals: [CARRIED], human_gated: {}, freshness: freshness() },
      },
    });
    const proposal = await card(container);
    const [declared, observed] = sides(proposal);

    expect(declared, "the declared side is not rendered").toBeTruthy();
    expect(observed, "the observed side is not rendered").toBeTruthy();

    // Both values, so a reader can see what the disagreement is about
    // without being told the answer by the summary line.
    expect(declared!.querySelector(".vogt-projects-side-value")?.textContent).toBe("1.2.0");
    expect(observed!.querySelector(".vogt-projects-side-value")?.textContent).toBe("1.3.0");

    // Provenance: which store the declared half is from, and which collector
    // saw the observed half, against which subject and which record.
    const declaredFrom = declared!.textContent ?? "";
    expect(declaredFrom).toContain("this instance's declared state");
    expect(declaredFrom).toContain("project.current_version");
    const observedFrom = observed!.textContent ?? "";
    expect(observedFrom).toContain("collector: git");
    expect(observedFrom).toContain("subject: project:alpha");
    expect(observedFrom).toContain("observation: obs_01");

    // Age: when the observed half was true, and — for the declared half,
    // which has no sweep — why it has no age of its own rather than a blank.
    expect(observed!.querySelector(".vogt-projects-age")?.textContent).toContain("ago");
    expect(observed!.querySelector(".vogt-projects-age")?.textContent).toContain(
      "2026-08-01 00:00:00",
    );
    expect(declared!.querySelector(".vogt-projects-age")?.textContent).toContain(
      "declared state has no sweep age",
    );
  });

  it("renders the evidence open, not behind a disclosure a reader can skip", async () => {
    const { container } = inbox({
      "GET /drift": {
        body: { proposals: [CARRIED], human_gated: {}, freshness: freshness() },
      },
    });
    const proposal = await card(container);

    // No click, no expansion, nothing hidden: both sides are already in the
    // page and neither is inside a collapsed `details`.
    for (const side of sides(proposal)) {
      expect(side.closest("details")).toBeNull();
    }
    // The raw record may be a disclosure — it is a dump, not the argument —
    // but the two values, their provenance and their ages are not.
    expect(proposal.querySelector(".vogt-projects-sides")?.closest("details")).toBeNull();
  });

  it("puts the evidence ahead of every control that acts on it", async () => {
    const { container } = inbox({
      "GET /drift": {
        body: { proposals: [CARRIED], human_gated: {}, freshness: freshness() },
      },
    });
    const proposal = await card(container);

    const evidence = proposal.querySelector(".vogt-projects-sides")!;
    const form = proposal.querySelector(".vogt-projects-resolve")!;
    expect(form, "the resolve form is not on an open proposal").toBeTruthy();

    // "Before" in the only sense a page has: the evidence comes first in
    // document order, so a reader reaches it on the way to the buttons and
    // not after having pressed one.
    const order = evidence.compareDocumentPosition(form);
    expect(order & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();

    // And the effect of accepting is stated in the same breath, above the
    // controls rather than beside them.
    const effect = proposal.querySelector(".vogt-projects-effect")!;
    expect(effect.textContent).toContain("current_version = 1.3.0");
    expect(effect.compareDocumentPosition(form) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });

  it("refuses to offer an act at all when there is no evidence to weigh", async () => {
    const { container } = inbox({
      "GET /drift": {
        body: {
          proposals: [driftProposal({ evidence_snapshot: undefined, evidence_observation_id: null })],
          human_gated: {},
          freshness: freshness(),
        },
      },
    });
    const proposal = await card(container);

    // Not a disabled button — no form. FR-U18 makes seeing both sides the
    // precondition for acting, so a proposal that cannot show them is not
    // resolvable from here, and the page says where it can be.
    expect(proposal.querySelector(".vogt-projects-resolve")).toBeNull();
    expect(proposal.querySelector("button[type=submit]")).toBeNull();
    expect(proposal.textContent).toContain("carries no evidence snapshot");
    expect(proposal.textContent).toContain("Resolve it with the CLI");
  });

  it("offers nothing to act with on a proposal somebody already resolved", async () => {
    const { container } = inbox({
      "GET /drift": {
        body: {
          proposals: [
            driftProposal({
              status: "accepted",
              resolution_reason: "the tag was cut before the manifest was bumped",
              resolved_by_identity_ref: "local:ana",
            }),
          ],
          human_gated: {},
          freshness: freshness(),
        },
      },
    });
    const proposal = await card(container);

    expect(proposal.querySelector(".vogt-projects-resolve")).toBeNull();
    expect(proposal.textContent).toContain("a proposal is resolved once");
    // The evidence and the reason both survive, because the record of why is
    // the point of having collected the reason at all.
    expect(sides(proposal)).toHaveLength(2);
    expect(proposal.textContent).toContain("the tag was cut before the manifest was bumped");
  });
});

describe("FR-U18 — one proposal, one typed reason, one act", () => {
  it("will not resolve without a reason, and sends the reason that was typed", async () => {
    const { vogt, container } = inbox({
      "GET /drift": {
        body: { proposals: [CARRIED], human_gated: {}, freshness: freshness() },
      },
      "POST /drift/resolve": { body: { id: "dft_01", change_applied: true } },
    });
    const proposal = await card(container);

    const submit = proposal.querySelector<HTMLButtonElement>("button[type=submit]")!;
    expect(submit.disabled).toBe(true);
    fireEvent.submit(proposal.querySelector("form.vogt-projects-resolve")!);
    expect(vogt.matching("POST /drift/resolve")).toHaveLength(0);

    const reason = proposal.querySelector<HTMLInputElement>(
      ".vogt-projects-field input[type=text]",
    )!;
    fireEvent.input(reason, { target: { value: "the manifest is behind the tag" } });
    await waitFor(() => expect(submit.disabled).toBe(false));

    // Reject rather than accept, so the resolution sent is the one chosen
    // and not the default the form opened on.
    const reject = [...proposal.querySelectorAll<HTMLInputElement>('input[type="radio"]')].find(
      (node) => node.value === "rejected",
    )!;
    fireEvent.change(reject);
    fireEvent.click(submit);

    await waitFor(() => expect(vogt.matching("POST /drift/resolve")).toHaveLength(1));
    expect(vogt.matching("POST /drift/resolve")[0]!.body).toEqual({
      id: "dft_01",
      resolution: "rejected",
      reason: "the manifest is behind the tag",
    });
  });

  it("gives two proposals two forms, and nothing that would resolve both", async () => {
    const { container } = inbox({
      "GET /drift": {
        body: {
          proposals: [CARRIED, driftProposal({ id: "dft_02", summary: "beta drifted too" })],
          human_gated: {},
          freshness: freshness(),
        },
      },
    });
    await card(container);

    await waitFor(() =>
      expect(container.querySelectorAll(".vogt-projects-drift")).toHaveLength(2),
    );
    // Each proposal carries its own reason, so there is no act that spans
    // two — and no checkbox anywhere on the surface to build one out of.
    expect(container.querySelectorAll("form.vogt-projects-resolve")).toHaveLength(2);
    expect(container.querySelectorAll('input[type="checkbox"]')).toHaveLength(0);
    expect(container.textContent).toContain("there is no bulk accept");
  });
});

describe("FR-R6 — superseded is a reading aid, not a resolution", () => {
  it("says a later sweep stopped reproducing it, and still offers the act", async () => {
    const { container } = inbox({
      "GET /drift": {
        body: {
          proposals: [
            driftProposal({
              ...CARRIED,
              superseded_at: "2026-08-17T00:00:00Z",
              superseded_detail:
                "git completed a sweep at 2026-08-17T00:00:00+00:00, after this " +
                "was raised, and the condition that raised it no longer reproduces",
            }),
          ],
          human_gated: {},
          freshness: freshness(),
        },
      },
    });
    const proposal = await card(container);

    expect(proposal.textContent).toContain("Superseded by fresher evidence");
    expect(proposal.textContent).toContain("no longer reproduces");
    // Still open, still resolvable, still showing both sides: the thirty-six
    // were cleared by a person, and FR-R6 changes what they can see, not who
    // decides (FR-R2, FR-U18).
    expect(proposal.classList.contains("vogt-projects-drift--open")).toBe(true);
    expect(proposal.querySelector("form.vogt-projects-resolve")).toBeTruthy();
    expect(sides(proposal)).toHaveLength(2);
  });

  it("says nothing at all about a proposal no sweep has overtaken", async () => {
    const { container } = inbox({
      "GET /drift": {
        body: { proposals: [CARRIED], human_gated: {}, freshness: freshness() },
      },
    });
    const proposal = await card(container);
    expect(proposal.querySelector(".vogt-projects-superseded")).toBeNull();
  });
});

describe("FR-R7 — a reference read from an item's own text", () => {
  it("names both registers and says accepting writes nothing", async () => {
    const { container } = inbox({
      "GET /drift": {
        body: {
          proposals: [
            driftProposal({
              id: "dft_09",
              kind: "referenced_issue_state_mismatch",
              subject_kind: "work_item",
              summary: "WI-16 references gh:o/vogt#44, which was open when last observed",
              evidence_snapshot: {
                subject_key: "gh:o/vogt#44",
                collector: "gh-issues",
                observed_at: "2026-08-17T00:00:00Z",
                payload: { state: "open", number: 44 },
              },
              proposed_change: {
                entity: "work_item",
                action: "review",
                subject_key: "gh:o/vogt#44",
                work_ref: "WI-16",
                declared_state: "done",
                upstream_state: "open",
              },
            }),
          ],
          human_gated: {
            referenced_issue_state_mismatch:
              "the reference was read out of the item's own text rather than adopted",
          },
          freshness: freshness(),
        },
      },
    });
    const proposal = await card(container);
    const [declared, observed] = sides(proposal);

    expect(declared!.textContent).toContain("done");
    expect(declared!.textContent).toContain("not adopted as a link");
    expect(observed!.textContent).toContain("open");
    expect(observed!.textContent).toContain("gh:o/vogt#44");
    expect(proposal.querySelector(".vogt-projects-effect")!.textContent).toContain(
      "Accepting writes nothing",
    );
    // Not the generic fallback: this build knows the kind.
    expect(proposal.textContent).not.toContain("this GUI does not know this drift kind");
  });
});
