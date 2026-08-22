// One work item, and the inline edit FR-U12 names and nothing implemented.
//
// §6.2: "There is no inline edit. `updateWork` is exported by `vogtApi.ts` and
// called by nothing — the dead binding that `test_pwa.py`'s own parity
// docstring warns about." These tests are what stops it going back.

import { describe, expect, it } from "vitest";
import { fireEvent, waitFor } from "@solidjs/testing-library";
import WorkItemDetail from "../WorkItemDetail";
import {
  auditRecord,
  fakeVogt,
  feedEvent,
  held,
  mountAt,
  observation,
  refusal,
  unavailable,
  workItem,
} from "./harness";

function detail(itemRef = "WI-1") {
  return mountAt(`/w/${itemRef}`, `/w/${itemRef}`, () => (
    <WorkItemDetail itemRef={itemRef} />
  ));
}

function facts(container: HTMLElement): string {
  return container.querySelector(".wid-facts")?.textContent ?? "";
}

function editor(container: HTMLElement): HTMLElement {
  const found = container.querySelector<HTMLElement>(".wid-edit");
  if (!found) throw new Error("the item has no inline editor open");
  return found;
}

function field(form: HTMLElement, label: string): HTMLInputElement | HTMLSelectElement {
  const found = [...form.querySelectorAll<HTMLElement>(".wid-field")].find((node) =>
    (node.querySelector("span")?.textContent ?? "").startsWith(label),
  );
  const control = found?.querySelector<HTMLInputElement | HTMLSelectElement>(
    "input, select",
  );
  if (!control) throw new Error(`the editor has no ${label} field`);
  return control;
}

/** The observed-evidence panel, or a failure that says it is missing. */
function observed(container: HTMLElement): HTMLElement {
  const found = container.querySelector<HTMLElement>(".wid-observed");
  if (!found) throw new Error("the item page has no observed-evidence panel");
  return found;
}

/** The badge words the panel put on each observation, in order. */
function settlements(container: HTMLElement): string[] {
  return [...observed(container).querySelectorAll(".wid-settlement")].map(
    (node) => node.textContent ?? "",
  );
}

async function openEditor(container: HTMLElement): Promise<HTMLElement> {
  const button = container.querySelector<HTMLButtonElement>(".wid-edit-open")!;
  await waitFor(() => expect(button.disabled).toBe(false));
  fireEvent.click(button);
  await waitFor(() => editor(container));
  return editor(container);
}

/** The panel under a heading, or a failure that names the missing panel. */
function panelNamed(container: HTMLElement, heading: string): HTMLElement {
  const found = [...container.querySelectorAll<HTMLElement>(".wid-panel")].find(
    (section) => section.querySelector("h3")?.textContent === heading,
  );
  if (!found) throw new Error(`the item page has no "${heading}" panel`);
  return found;
}

// -- FR-U5: what is on the page ---------------------------------------------
//
// Everything else in this file mounts the item page to watch it *do*
// something — an edit, an outage, an answer to a waiting session. FR-U5 is a
// claim about what the page *is*: description, comments, relations, labels,
// collected evidence and the control that starts a session, all on one page
// and not spread over five. A panel quietly dropped would have passed every
// test above, and the person it fails is the one who came to this page to
// find out what they already know about this item.
//
// Each panel is asserted with the server's own answer in it. A heading with
// an empty body under it is the failure this shape catches and a
// heading-only assertion would not — on this page most of all, where an empty
// panel reads as "there are no comments" rather than "the comments are gone".

const RICH_ITEM = workItem({
  body: "The card should say when it does not know, rather than looking empty.",
  labels: ["infra", "docs"],
  relations: [
    {
      kind: "blocks",
      related_id: "01JOTHER",
      related_ref: "WI-9",
      related_title: "Ship the front door",
      related_state: "open",
    },
  ],
});

const COMMENT = {
  id: "01JCOMMENT",
  body: "Held until the front door lands.",
  created_at: "2026-08-02T00:00:00Z",
  actor_display_name: "ana",
};

const WHY = {
  ref: "WI-1",
  title: "Teach the board to say what it does not know",
  total: 4.25,
  contributions: [
    {
      input: "age",
      detail: "opened 12 days ago",
      value: 12,
      weight: 0.25,
      contribution: 3,
    },
  ],
  inputs_not_yet_available: { ci: "no sweep has read this project's CI" },
};

function fullItem() {
  return fakeVogt({
    "GET /work/get": {
      body: { item: RICH_ITEM, comments: [COMMENT], sessions: [] },
    },
    "GET /why": { body: WHY },
  });
}

describe("FR-U5 — one item, one page, and everything about it on it", () => {
  it("shows the description the item was written with", async () => {
    fullItem();
    const { container } = detail();

    await waitFor(() =>
      expect(panelNamed(container, "Description").textContent).toContain(
        "The card should say when it does not know",
      ),
    );
  });

  it("shows the comments recorded against it, with who and when", async () => {
    fullItem();
    const { container } = detail();

    const comments = await waitFor(() => {
      const found = panelNamed(container, "Comments");
      expect(found.querySelector(".wid-comments")).toBeTruthy();
      return found;
    });
    expect(comments.textContent).toContain("Held until the front door lands.");
    expect(comments.textContent).toContain("ana");
    expect(comments.textContent).toContain("1 recorded");
  });

  it("shows its relations as links to the items they name", async () => {
    // A relation to `01JOTHER` is a database row; a link to WI-9 is a thing a
    // person can follow, which is the whole of what this panel is for.
    fullItem();
    const { container } = detail();

    const relations = await waitFor(() => {
      const found = panelNamed(container, "Relations");
      expect(found.querySelector(".wid-relations")).toBeTruthy();
      return found;
    });
    expect(relations.textContent).toContain("blocks");
    const link = relations.querySelector<HTMLAnchorElement>("a")!;
    expect(link.getAttribute("href")).toBe("#/w/WI-9");
    expect(link.textContent).toContain("Ship the front door");
  });

  it("shows its labels", async () => {
    fullItem();
    const { container } = detail();

    await waitFor(() => {
      const labels = [...panelNamed(container, "Labels").querySelectorAll(".wid-chip")];
      expect(labels.map((node) => node.textContent)).toEqual(["infra", "docs"]);
    });
  });

  it("shows the collected evidence behind its ranking, per input", async () => {
    // FR-U6's shape: the contributions, not a single score. A total on its
    // own is a number nobody can argue with, which is the problem with it.
    fullItem();
    const { container } = detail();

    const evidencePanel = await waitFor(() => {
      const found = panelNamed(container, "Collected evidence");
      expect(found.querySelector(".wid-table")).toBeTruthy();
      return found;
    });
    const cells = [...evidencePanel.querySelectorAll("tbody td")].map(
      (node) => node.textContent,
    );
    expect(cells).toEqual(["age", "opened 12 days ago", "12.00", "0.25", "3.000"]);
    expect(evidencePanel.textContent).toContain("rank score 4.250");
    // And what did not fire, said rather than left out — an input missing
    // from the table is otherwise indistinguishable from one that scored zero.
    expect(evidencePanel.textContent).toContain("ci");
    expect(evidencePanel.textContent).toContain(
      "no sweep has read this project's CI",
    );
  });

  it("offers the control that starts a session on this item", async () => {
    // #224: the form is collapsed by default, behind a button that names it.
    // Opening it is what reveals the fields and the submit.
    fullItem();
    const { container } = detail();

    const start = await waitFor(() => {
      const found = container.querySelector<HTMLElement>(".wid-start");
      expect(found, "the item page has no start-session control").toBeTruthy();
      return found!;
    });
    expect(start.textContent).toContain("Start a session for WI-1");
    // Collapsed: the submit is not on the page until the control is opened.
    expect(
      [...start.querySelectorAll("button")].find(
        (button) => button.textContent?.trim() === "Start session",
      ),
    ).toBeFalsy();

    fireEvent.click(start.querySelector<HTMLButtonElement>(".wid-start-open")!);
    const submit = await waitFor(() => {
      const found = [...start.querySelectorAll("button")].find(
        (button) => button.textContent?.trim() === "Start session",
      );
      expect(found).toBeTruthy();
      return found!;
    });
    expect(submit).toBeTruthy();
  });

  it("keeps all six on one page, at once", async () => {
    // The requirement is the *page*, not the six panels: a reader who has to
    // navigate between them to answer "what is going on with WI-1" has the
    // product FR-U5 was written against.
    fullItem();
    const { container } = detail();

    await waitFor(() => panelNamed(container, "Collected evidence"));
    for (const heading of [
      "Description",
      "Comments",
      "Relations",
      "Labels",
      "Collected evidence",
    ]) {
      expect(panelNamed(container, heading)).toBeTruthy();
    }
    expect(container.querySelector(".wid-start")).toBeTruthy();
  });
});

// -- FR-U5's "state history" ------------------------------------------------
//
// §6.2 recorded this as the one clause of FR-U5 the page did not answer: the
// server could, and the surface did not ask. The obstacle it names is why
// these fixtures are shaped the way they are — `audit` keeps a
// `payload_digest` rather than the payload, so the audit row can say a
// transition happened and never which state it came from. The `from` is in
// the *event*, the reason and the readable identity are on the *audit row*,
// and the event names that row in `audit_id`. Every test below is about the
// join between them, or about what the panel says when it does not close.

/** The state-history panel, or a failure that says it is missing. */
function history(container: HTMLElement): HTMLElement {
  const found = container.querySelector<HTMLElement>(".wid-history");
  if (!found) throw new Error("the item page has no state-history panel");
  return found;
}

/** Each move as "open → in_progress", in the order the panel put them. */
function movesShown(container: HTMLElement): string[] {
  return [...history(container).querySelectorAll(".wid-move-states")].map((node) =>
    (node.textContent ?? "").replace(/\s+/g, " ").trim(),
  );
}

/** Who each move says moved it. Never blank, in any of its three forms. */
function moversShown(container: HTMLElement): string[] {
  return [...history(container).querySelectorAll(".wid-move-actor")].map(
    (node) => node.textContent ?? "",
  );
}

/** The reasons quoted inline — not the ones linked to. */
function reasonsShown(container: HTMLElement): string[] {
  return [...history(container).querySelectorAll(".wid-move-reason")].map((node) =>
    (node.textContent ?? "").replace(/[“”]/g, ""),
  );
}

const CREATED = feedEvent({
  seq: 1,
  kind: "work.created",
  audit_id: "01JAUDIT0",
  summary: { ref: "WI-1", kind: "feature", title: "Teach the board" },
  at: "2026-08-01T00:00:00Z",
});

const STARTED = feedEvent({
  seq: 2,
  audit_id: "01JAUDIT1",
  summary: { ref: "WI-1", from: "open", to: "in_progress" },
  at: "2026-08-02T00:00:00Z",
});

const FINISHED = feedEvent({
  seq: 3,
  actor_id: "01JACTOR2",
  audit_id: "01JAUDIT2",
  summary: { ref: "WI-1", from: "in_progress", to: "done" },
  at: "2026-08-03T00:00:00Z",
});

/** A comment on the item. In the same slice of the feed and not a move — the
 *  feed is every write about this entity, not only the ones that changed its
 *  state. */
const COMMENTED = feedEvent({
  seq: 4,
  kind: "work.commented",
  audit_id: "01JAUDIT3",
  summary: { ref: "WI-1", comment: "01JCOMMENT" },
  at: "2026-08-04T00:00:00Z",
});

const TRAIL = [
  auditRecord({
    id: "01JAUDIT0",
    operation: "work.create",
    reason: "raised from the sweep",
  }),
  auditRecord({ id: "01JAUDIT1", reason: "picked up in Monday's planning" }),
  auditRecord({
    id: "01JAUDIT2",
    actor_id: "01JACTOR2",
    actor_identity_ref: "local:bo",
    reason: "merged in #218",
  }),
];

function withHistory(events: unknown[], records: unknown[] = TRAIL) {
  return fakeVogt({
    "GET /events": { body: { events, next_cursor: events.length } },
    "GET /audit": { body: { records, total: records.length } },
  });
}

describe("FR-U5 — the item's own state history, on the item page", () => {
  it("asks the feed for this item's history, by the id the feed is keyed on", async () => {
    // The ref is what a person can read; the entity id is what both feeds are
    // filed under. An unnarrowed read would be the whole estate's feed, which
    // is a different question, and a read narrowed in the client would decide
    // the history ended at the first quiet stretch of it.
    const vogt = withHistory([CREATED, STARTED]);
    detail();

    await waitFor(() => expect(vogt.matching("GET /events")).toHaveLength(1));
    expect(vogt.matching("GET /events")[0]?.query.get("entity_id")).toBe(
      "01JWORKITEM",
    );
  });

  it("shows what each move came from, in the order the moves happened", async () => {
    // The `from` is the whole point: the audit log keeps a digest rather than
    // the payload, so without the feed a reader can be told the item moved
    // and never what it moved out of.
    //
    // The comment in the middle is in this item's slice of the feed and is
    // not a move. An entry for it would be a state change that never
    // happened — and it would sort into the middle of the ones that did.
    withHistory([CREATED, STARTED, COMMENTED, FINISHED]);
    const { container } = detail();

    await waitFor(() =>
      expect(movesShown(container)).toEqual([
        "created in open",
        "open → in_progress",
        "in_progress → done",
      ]),
    );
  });

  it("names who moved it readably, from the audit row the event points at", async () => {
    // An event names its actor by id. `01JACTOR2` is technically who and is
    // not an answer, so the panel joins through `audit_id` for the identity —
    // which is the reason the audit row is fetched at all.
    withHistory([STARTED, FINISHED]);
    const { container } = detail();

    await waitFor(() =>
      expect(moversShown(container)).toEqual(["by local:ana", "by local:bo"]),
    );
    expect(history(container).textContent).not.toContain("01JACTOR2");
  });

  it("quotes the reason recorded against each move", async () => {
    withHistory([STARTED, FINISHED]);
    const { container } = detail();

    await waitFor(() =>
      expect(reasonsShown(container)).toEqual([
        "picked up in Monday's planning",
        "merged in #218",
      ]),
    );
  });

  it("links to the audit trail rather than leaving a reason blank", async () => {
    // The audit row this move names is not among the ones the page holds.
    // There *is* a reason — Vogt refuses a write without one — so a blank
    // would say nobody gave one, which is a claim and a false one.
    withHistory([STARTED, FINISHED], [TRAIL[0]!, TRAIL[1]!]);
    const { container } = detail();

    await waitFor(() => expect(movesShown(container)).toHaveLength(2));
    expect(reasonsShown(container)).toEqual(["picked up in Monday's planning"]);

    const link = history(container).querySelector<HTMLAnchorElement>(".wid-move-why");
    expect(link, "a move with no reason in hand rendered nothing at all").toBeTruthy();
    expect(link!.getAttribute("href")).toBe("#/audit?ref=WI-1&op=work.transition");
    // And it is counted on the surface, so a reader is not left to notice.
    expect(history(container).textContent).toContain(
      "an audit row this page does not hold",
    );
    // Who still gets answered, by the id the event does carry.
    expect(moversShown(container)).toEqual(["by local:ana", "by actor 01JACTOR2"]);
  });

  it("says an item that has never moved has never moved", async () => {
    // FR-U21's rule on this panel: the absence is designed. A creation and no
    // transitions is a complete history, and rendering it as an empty list
    // under a heading would read as the moves having been lost.
    withHistory([CREATED]);
    const { container } = detail();

    await waitFor(() => expect(movesShown(container)).toEqual(["created in open"]));
    expect(history(container).textContent).toContain(
      "WI-1 has not been moved since it was created",
    );
    expect(history(container).textContent).not.toContain("transitions");
  });

  it("claims the whole history, because the feed it reads is never pruned", async () => {
    withHistory([CREATED, STARTED]);
    const { container } = detail();

    await waitFor(() => expect(movesShown(container)).toHaveLength(2));
    expect(history(container).textContent).toContain("1 transition");
    expect(history(container).textContent).toContain(
      "this is the whole of its history and not a recent window",
    );
  });

  it("says the list is cut when the feed had more than it walked", async () => {
    // The panel must not imply a completeness it does not have. Five full
    // pages and the feed still going means there are later moves it is not
    // showing — and the later ones are the ones that matter most.
    const vogt = fakeVogt({
      // A feed that keeps answering, and answers from wherever it is asked.
      "GET /events": (call) => {
        const after = Number(call.query.get("after") ?? 0);
        return {
          body: {
            events: Array.from({ length: 200 }, (_, index) =>
              feedEvent({ seq: after + index + 1, audit_id: null }),
            ),
            next_cursor: after + 200,
          },
        };
      },
    });
    const { container } = detail();

    await waitFor(() =>
      expect(history(container).textContent).toContain(
        "this is the first 1000 events recorded",
      ),
    );
    expect(history(container).textContent).toContain(
      "later moves are missing from the list below",
    );
    expect(history(container).textContent).not.toContain("the whole of its history");

    // Five pages, each asked for from where the last one ended. A walk that
    // did not carry the cursor forward would read the first page five times
    // and then claim it had read a thousand events.
    const walked = vogt.matching("GET /events");
    expect(walked.map((call) => call.query.get("after"))).toEqual([
      "0",
      "200",
      "400",
      "600",
      "800",
    ]);
    expect(walked.map((call) => call.query.get("limit"))).toEqual(
      Array(5).fill("200"),
    );
  });

  it("says the audit log was longer than the reasons it holds", async () => {
    // The second read caps too, and the cap has to be said for the same
    // reason the first one's does: some of these moves link for their reason
    // because the log was long, not because nobody gave one.
    fakeVogt({
      "GET /events": { body: { events: [STARTED, FINISHED], next_cursor: 3 } },
      "GET /audit": { body: { records: [TRAIL[1]!], total: 600 } },
    });
    const { container } = detail();

    await waitFor(() => expect(movesShown(container)).toHaveLength(2));
    expect(history(container).textContent).toContain(
      "the log being longer than 500 rows",
    );
  });

  it("re-reads both feeds when the page is refreshed", async () => {
    // The page reports its own age beside the Refresh, and a panel that
    // Refresh did not reach would be the one part of it the badge was lying
    // about.
    const vogt = withHistory([STARTED], [TRAIL[1]!]);
    const { container } = detail();
    await waitFor(() => expect(movesShown(container)).toEqual(["open → in_progress"]));

    vogt.route("GET /events", {
      body: { events: [STARTED, FINISHED], next_cursor: 3 },
    });
    vogt.route("GET /audit", { body: { records: TRAIL, total: TRAIL.length } });
    fireEvent.click(
      [...container.querySelectorAll("button")].find(
        (button) => button.textContent?.trim() === "Refresh",
      )!,
    );

    await waitFor(() =>
      expect(movesShown(container)).toEqual([
        "open → in_progress",
        "in_progress → done",
      ]),
    );
    expect(reasonsShown(container)).toEqual([
      "picked up in Monday's planning",
      "merged in #218",
    ]);
  });

  it("renders Vogt's own sentence and no moves when the history cannot be read", async () => {
    const NO_CORE = "vogt-core is not configured for this front door";
    fakeVogt({ "GET /events": unavailable(NO_CORE) });
    const { container } = detail();

    await waitFor(() =>
      expect(history(container).querySelector(".wid-failure")).toBeTruthy(),
    );
    const panel = history(container);
    expect(panel.textContent).toContain("Vogt cannot be reached");
    expect(panel.textContent).toContain(NO_CORE);
    expect(panel.textContent).toContain("not because WI-1 has never moved");
    expect(movesShown(container)).toHaveLength(0);
    // Nothing claims the item has never moved, and the page is still the page.
    expect(panel.querySelector(".wid-absent")).toBeNull();
    expect(container.querySelector(".wid-outage")).toBeNull();
    expect(container.textContent).toContain("Collected evidence");
    expect(container.textContent).toContain("Comments");
  });

  it("keeps the moves when only the reasons could not be read", async () => {
    // Two reads, and the one that fails must not take the other's answer with
    // it: what the item did is knowable from the feed alone.
    fakeVogt({
      "GET /events": { body: { events: [STARTED, FINISHED], next_cursor: 3 } },
      "GET /audit": refusal(500, "audit.list: the store is locked"),
    });
    const { container } = detail();

    await waitFor(() =>
      expect(movesShown(container)).toEqual(["open → in_progress", "in_progress → done"]),
    );
    const panel = history(container);
    expect(panel.textContent).toContain("the reasons could not be read");
    expect(panel.textContent).toContain("audit.list: the store is locked");
    expect(reasonsShown(container)).toHaveLength(0);
    expect(panel.querySelectorAll(".wid-move-why")).toHaveLength(2);
    expect(moversShown(container)).toEqual(["by actor 01JACTOR1", "by actor 01JACTOR2"]);
  });

  it("no longer sends a reader to the audit trail for the transitions", async () => {
    // The State panel used to say the transitions "belong in the audit trail
    // below, not in a second story told by this panel". They are told here
    // now, and a sentence saying otherwise would be the surface disagreeing
    // with itself.
    withHistory([CREATED, STARTED]);
    const { container } = detail();

    await waitFor(() => expect(movesShown(container)).toHaveLength(2));
    expect(panelNamed(container, "State").textContent).not.toContain(
      "not in a second story told by this panel",
    );
  });
});

describe("FR-U12 — an inline edit renders optimistically and the server decides", () => {
  it("opens on the item's own values, from the server", async () => {
    fakeVogt({
      "GET /work/get": {
        body: {
          item: workItem({ title: "As Vogt has it", priority: "p3" }),
          comments: [],
          sessions: [],
        },
      },
    });
    const { container } = detail();
    await waitFor(() => expect(facts(container)).toContain("p3"));

    const form = await openEditor(container);
    expect((field(form, "Title") as HTMLInputElement).value).toBe("As Vogt has it");
    expect((field(form, "Priority") as HTMLSelectElement).value).toBe("p3");
  });

  it("sends `work.update` with the reason the user typed", async () => {
    const vogt = fakeVogt({
      "POST /work/update": {
        body: {
          item: workItem({
            title: "A better title",
            priority: "p0",
            updated_at: "2026-08-03T00:00:00Z",
          }),
          comments: [],
          sessions: [],
        },
      },
    });
    const { container } = detail();
    await waitFor(() => expect(facts(container)).toContain("p2"));

    const form = await openEditor(container);
    fireEvent.input(field(form, "Title"), { target: { value: "A better title" } });
    fireEvent.input(field(form, "Priority"), { target: { value: "p0" } });
    fireEvent.input(field(form, "Reason"), { target: { value: "raised after the outage" } });
    fireEvent.submit(form.querySelector("form")!);

    await waitFor(() => expect(vogt.matching("POST /work/update")).toHaveLength(1));
    expect(vogt.matching("POST /work/update")[0]?.body).toEqual({
      ref: "WI-1",
      title: "A better title",
      priority: "p0",
      reason: "raised after the outage",
    });
  });

  it("renders the change while Vogt is still deciding, and says it is unsaved", async () => {
    const answer = held();
    fakeVogt({ "POST /work/update": answer.handler });
    const { container } = detail();
    await waitFor(() => expect(facts(container)).toContain("p2"));

    const form = await openEditor(container);
    fireEvent.input(field(form, "Title"), { target: { value: "Renamed on the spot" } });
    fireEvent.input(field(form, "Priority"), { target: { value: "p1" } });
    fireEvent.input(field(form, "Reason"), { target: { value: "the title was wrong" } });
    fireEvent.submit(form.querySelector("form")!);

    // Vogt has been asked and has not answered. The page already reads as the
    // change — and already admits the change is not yet true.
    await answer.asked;
    await waitFor(() =>
      expect(container.querySelector(".wid-heading h2")?.textContent).toBe(
        "Renamed on the spot",
      ),
    );
    expect(facts(container)).toContain("p1");
    expect(facts(container)).toContain("unsaved — Vogt is deciding");

    answer.answer({
      body: {
        item: workItem({
          title: "Renamed on the spot",
          priority: "p1",
          updated_at: "2026-08-03T00:00:00Z",
        }),
        comments: [],
        sessions: [],
      },
    });
    await waitFor(() => expect(facts(container)).not.toContain("unsaved"));
  });

  it("keeps the server's version of the change, not the one that was typed", async () => {
    fakeVogt({
      "POST /work/update": {
        body: {
          // Vogt normalised the title. The page must show Vogt's.
          item: workItem({
            title: "A better title",
            priority: "p0",
            updated_at: "2026-08-03T00:00:00Z",
          }),
          comments: [],
          sessions: [],
        },
      },
    });
    const { container } = detail();
    await waitFor(() => expect(facts(container)).toContain("p2"));

    const form = await openEditor(container);
    fireEvent.input(field(form, "Title"), { target: { value: "  a better title  " } });
    fireEvent.input(field(form, "Priority"), { target: { value: "p0" } });
    fireEvent.input(field(form, "Reason"), { target: { value: "tidying the wording" } });
    fireEvent.submit(form.querySelector("form")!);

    // Optimistically the heading reads what was typed; waiting for the
    // *server's* casing is what proves the reconcile happened at all.
    await waitFor(() =>
      expect(container.querySelector(".wid-heading h2")?.textContent).toBe(
        "A better title",
      ),
    );
    expect(facts(container)).toContain("p0");
    expect(facts(container)).not.toContain("unsaved");
  });

  it("rolls a refused edit back and shows Vogt's own reason beside the field", async () => {
    const REFUSED = "work.update: priority p0 is reserved for incidents";
    fakeVogt({ "POST /work/update": refusal(422, REFUSED) });
    const { container } = detail();
    await waitFor(() => expect(facts(container)).toContain("p2"));
    const before = container.querySelector(".wid-heading h2")?.textContent;

    const form = await openEditor(container);
    fireEvent.input(field(form, "Title"), { target: { value: "Something else" } });
    fireEvent.input(field(form, "Priority"), { target: { value: "p0" } });
    fireEvent.input(field(form, "Reason"), { target: { value: "it feels urgent" } });
    fireEvent.submit(form.querySelector("form")!);

    await waitFor(() =>
      expect(editor(container).querySelector(".wid-failure")?.textContent).toContain(
        REFUSED,
      ),
    );

    // Rolled back visibly: the facts row and the heading are the server's
    // again, and the surface says so in as many words.
    expect(facts(container)).toContain("p2");
    expect(facts(container)).not.toContain("unsaved");
    expect(container.querySelector(".wid-heading h2")?.textContent).toBe(before);
    expect(editor(container).querySelector(".wid-rolledback")?.textContent).toContain(
      "WI-1 is unchanged",
    );
  });

  it("does not re-derive a refused value when the editor is reopened", async () => {
    fakeVogt({ "POST /work/update": refusal(422, "work.update: refused") });
    const { container } = detail();
    await waitFor(() => expect(facts(container)).toContain("p2"));

    const form = await openEditor(container);
    fireEvent.input(field(form, "Title"), { target: { value: "Something else" } });
    fireEvent.input(field(form, "Priority"), { target: { value: "p0" } });
    fireEvent.input(field(form, "Reason"), { target: { value: "it feels urgent" } });
    fireEvent.submit(form.querySelector("form")!);
    await waitFor(() => expect(editor(container).querySelector(".wid-failure")).toBeTruthy());

    // Close and reopen: FR-U12's "never persist, cache, or re-derive".
    fireEvent.click(container.querySelector<HTMLButtonElement>(".wid-edit-open")!);
    await waitFor(() => expect(container.querySelector(".wid-edit")).toBeNull());
    const reopened = await openEditor(container);

    expect((field(reopened, "Title") as HTMLInputElement).value).toBe(
      "Teach the board to say what it does not know",
    );
    expect((field(reopened, "Priority") as HTMLSelectElement).value).toBe("p2");
    // And nothing about the refused edit reached storage.
    expect(JSON.stringify(localStorage)).not.toContain("Something else");
  });

  it("will not submit without a reason the user typed", async () => {
    const vogt = fakeVogt();
    const { container } = detail();
    await waitFor(() => expect(facts(container)).toContain("p2"));

    const form = await openEditor(container);
    const submit = form.querySelector<HTMLButtonElement>("button[type=submit]")!;
    fireEvent.input(field(form, "Title"), { target: { value: "Something else" } });
    expect(submit.disabled).toBe(true);

    fireEvent.submit(form.querySelector("form")!);
    expect(vogt.matching("POST /work/update")).toHaveLength(0);

    fireEvent.input(field(form, "Reason"), { target: { value: "   " } });
    expect(submit.disabled).toBe(true);
  });

  it("will not submit an edit that changes nothing, or empties the title", async () => {
    const vogt = fakeVogt();
    const { container } = detail();
    await waitFor(() => expect(facts(container)).toContain("p2"));

    const form = await openEditor(container);
    const submit = form.querySelector<HTMLButtonElement>("button[type=submit]")!;
    fireEvent.input(field(form, "Reason"), { target: { value: "a reason with no change" } });
    expect(submit.disabled).toBe(true);

    fireEvent.input(field(form, "Title"), { target: { value: "  " } });
    expect(submit.disabled).toBe(true);
    fireEvent.submit(form.querySelector("form")!);
    expect(vogt.matching("POST /work/update")).toHaveLength(0);
  });
});

// -- #213: the Move-to transition, from the detail page ---------------------
//
// The same three rules as the board's drag and the inline edit: the move
// renders optimistically, the server's answer is authoritative, and a refusal
// — a 409 above all — is rolled back visibly to the state it was in.

/** The body textarea in the editor — `field` above returns inputs and selects,
 *  and the description is the one control that is neither. */
function bodyField(form: HTMLElement): HTMLTextAreaElement {
  const found = [...form.querySelectorAll<HTMLElement>(".wid-field")].find((node) =>
    (node.querySelector("span")?.textContent ?? "").startsWith("Description"),
  );
  const control = found?.querySelector<HTMLTextAreaElement>("textarea");
  if (!control) throw new Error("the editor has no Description field");
  return control;
}

/** The Move-to control under the state rail, or a failure that names it. */
function moveForm(container: HTMLElement): HTMLElement {
  const found = container.querySelector<HTMLElement>(".wid-move-to");
  if (!found) throw new Error("the item page has no Move-to control");
  return found;
}

/** The single state chip in the facts row — what the page says the item is. */
function stateChip(container: HTMLElement): string {
  return container.querySelector(".wid-chip--state")?.textContent ?? "";
}

describe("#213 — a work item's state is changed from its own page", () => {
  it("offers only the workflow's declared edges from the current state", async () => {
    fakeVogt();
    const { container } = detail();

    const select = (await waitFor(() => {
      const control = field(moveForm(container), "Move to") as HTMLSelectElement;
      expect(control).toBeTruthy();
      return control;
    })) as HTMLSelectElement;
    const options = [...select.querySelectorAll("option")]
      .map((option) => option.value)
      .filter((value) => value.length > 0);
    // FEATURE_WORKFLOW: open → in_progress / wont_do, and never open itself.
    expect(options).toEqual(["in_progress", "wont_do"]);
  });

  it("sends work.transition with the reason the user typed, and moves optimistically", async () => {
    const vogt = fakeVogt({
      "POST /work/transition": {
        body: {
          item: workItem({ state: "in_progress", updated_at: "2026-08-03T00:00:00Z" }),
          comments: [],
          sessions: [],
        },
      },
    });
    const { container } = detail();
    await waitFor(() => expect(stateChip(container)).toBe("open"));

    const form = moveForm(container);
    fireEvent.input(field(form, "Move to"), { target: { value: "in_progress" } });
    fireEvent.input(field(form, "Reason"), { target: { value: "picked it up" } });
    fireEvent.submit(form.querySelector("form")!);

    await waitFor(() => expect(vogt.matching("POST /work/transition")).toHaveLength(1));
    expect(vogt.matching("POST /work/transition")[0]?.body).toEqual({
      ref: "WI-1",
      to_state: "in_progress",
      reason: "picked it up",
    });
    // The rail follows the server's answer.
    await waitFor(() => expect(stateChip(container)).toBe("in_progress"));
  });

  it("shows the move the moment it is submitted, before Vogt answers", async () => {
    const answer = held();
    fakeVogt({ "POST /work/transition": answer.handler });
    const { container } = detail();
    await waitFor(() => expect(stateChip(container)).toBe("open"));

    const form = moveForm(container);
    fireEvent.input(field(form, "Move to"), { target: { value: "in_progress" } });
    fireEvent.input(field(form, "Reason"), { target: { value: "starting it" } });
    fireEvent.submit(form.querySelector("form")!);

    // Vogt has been asked and has not answered: the rail already reads the
    // move, and the page already admits it is not yet true.
    await answer.asked;
    await waitFor(() => expect(stateChip(container)).toBe("in_progress"));
    expect(facts(container)).toContain("unsaved — Vogt is deciding");

    answer.answer({
      body: {
        item: workItem({ state: "in_progress", updated_at: "2026-08-03T00:00:00Z" }),
        comments: [],
        sessions: [],
      },
    });
    await waitFor(() => expect(facts(container)).not.toContain("unsaved"));
    expect(stateChip(container)).toBe("in_progress");
  });

  it("rolls a refused move back to the state it was in, on a 409", async () => {
    const REFUSED = "work.transition: WI-1 already moved to done";
    fakeVogt({ "POST /work/transition": refusal(409, REFUSED) });
    const { container } = detail();
    await waitFor(() => expect(stateChip(container)).toBe("open"));

    const form = moveForm(container);
    fireEvent.input(field(form, "Move to"), { target: { value: "in_progress" } });
    fireEvent.input(field(form, "Reason"), { target: { value: "starting it" } });
    fireEvent.submit(form.querySelector("form")!);

    // The server's own sentence, and the rollback stated as many words.
    await waitFor(() =>
      expect(moveForm(container).querySelector(".wid-failure")?.textContent).toContain(
        REFUSED,
      ),
    );
    expect(stateChip(container)).toBe("open");
    expect(facts(container)).not.toContain("unsaved");
    expect(moveForm(container).querySelector(".wid-rolledback")?.textContent).toContain(
      "WI-1 is still open",
    );
  });

  it("will not move without a reason the user typed", async () => {
    const vogt = fakeVogt();
    const { container } = detail();
    const form = await waitFor(() => moveForm(container));
    const submit = [...form.querySelectorAll<HTMLButtonElement>("button")].find(
      (button) => button.type === "submit",
    )!;

    fireEvent.input(field(form, "Move to"), { target: { value: "in_progress" } });
    expect(submit.disabled).toBe(true);
    fireEvent.submit(form.querySelector("form")!);
    expect(vogt.matching("POST /work/transition")).toHaveLength(0);
  });
});

// -- #213: the editor now changes assignee, effort, labels and body ---------

describe("#213 — the editor changes assignee, effort, labels and body", () => {
  it("opens the actor picker as a keyboard-operable select on the item's assignee", async () => {
    fakeVogt({
      "GET /work/get": {
        body: {
          item: workItem({ assignee_identity_ref: "local:ana" }),
          comments: [],
          sessions: [],
        },
      },
      "GET /actors": {
        body: {
          actors: [
            { identity_ref: "local:ana", display_name: "Ana" },
            { identity_ref: "local:bo", display_name: "Bo" },
          ],
        },
      },
    });
    const { container } = detail();
    await waitFor(() => expect(facts(container)).toContain("assignee: local:ana"));

    const form = await openEditor(container);
    const picker = field(form, "Assignee") as HTMLSelectElement;
    // A native select is focusable and keyboard-driven without any code of our
    // own — the a11y property #213 needs from the picker.
    expect(picker.tagName).toBe("SELECT");
    await waitFor(() =>
      expect(picker.querySelectorAll("option").length).toBeGreaterThan(2),
    );
    expect(picker.value).toBe("local:ana");
  });

  it("sends assignee, effort, labels and body through work.update", async () => {
    const vogt = fakeVogt({
      "GET /actors": {
        body: {
          actors: [
            { identity_ref: "local:ana", display_name: "Ana" },
            { identity_ref: "local:bo", display_name: "Bo" },
          ],
        },
      },
      "POST /work/update": {
        body: { item: workItem(), comments: [], sessions: [] },
      },
    });
    const { container } = detail();
    await waitFor(() => expect(facts(container)).toContain("p2"));

    const form = await openEditor(container);
    await waitFor(() =>
      expect(
        (field(form, "Assignee") as HTMLSelectElement).querySelectorAll("option").length,
      ).toBeGreaterThan(1),
    );
    fireEvent.input(field(form, "Assignee"), { target: { value: "local:bo" } });
    fireEvent.input(field(form, "Effort"), { target: { value: "m" } });
    fireEvent.input(field(form, "Labels"), { target: { value: "infra, docs" } });
    fireEvent.input(bodyField(form), { target: { value: "A real body now." } });
    fireEvent.input(field(form, "Reason"), { target: { value: "triaged" } });
    fireEvent.submit(form.querySelector("form")!);

    await waitFor(() => expect(vogt.matching("POST /work/update")).toHaveLength(1));
    // Only what changed, and labels as an add-diff — not a replacement.
    expect(vogt.matching("POST /work/update")[0]?.body).toEqual({
      ref: "WI-1",
      reason: "triaged",
      assignee: "local:bo",
      effort: "m",
      body: "A real body now.",
      add_labels: ["infra", "docs"],
    });
  });

  it("clears the assignee and effort and removes a label with the diff flags", async () => {
    const vogt = fakeVogt({
      "GET /work/get": {
        body: {
          item: workItem({
            assignee_identity_ref: "local:ana",
            effort: "l",
            labels: ["infra", "docs"],
          }),
          comments: [],
          sessions: [],
        },
      },
      "GET /actors": {
        body: { actors: [{ identity_ref: "local:ana", display_name: "Ana" }] },
      },
      "POST /work/update": {
        body: { item: workItem(), comments: [], sessions: [] },
      },
    });
    const { container } = detail();
    await waitFor(() => expect(facts(container)).toContain("assignee: local:ana"));

    const form = await openEditor(container);
    // The editor opens on the server's own values.
    expect((field(form, "Effort") as HTMLSelectElement).value).toBe("l");
    expect((field(form, "Labels") as HTMLInputElement).value).toBe("infra, docs");

    fireEvent.input(field(form, "Assignee"), { target: { value: "" } });
    fireEvent.input(field(form, "Effort"), { target: { value: "" } });
    fireEvent.input(field(form, "Labels"), { target: { value: "infra" } });
    fireEvent.input(field(form, "Reason"), { target: { value: "unassigning" } });
    fireEvent.submit(form.querySelector("form")!);

    await waitFor(() => expect(vogt.matching("POST /work/update")).toHaveLength(1));
    expect(vogt.matching("POST /work/update")[0]?.body).toEqual({
      ref: "WI-1",
      reason: "unassigning",
      clear_assignee: true,
      clear_effort: true,
      remove_labels: ["docs"],
    });
  });
});

// -- #224: the page leads with the item, not its machinery ------------------

describe("#224 — the item page collapses its chrome and leads with comments", () => {
  it("keeps the start-a-session form collapsed until it is asked for", async () => {
    fakeVogt();
    const { container } = detail();

    const start = await waitFor(() => {
      const found = container.querySelector<HTMLElement>(".wid-start");
      expect(found).toBeTruthy();
      return found!;
    });
    // Collapsed: the template picker and the submit are not on the page.
    expect(start.querySelector("select")).toBeNull();
    expect(
      [...start.querySelectorAll("button")].some(
        (button) => button.textContent?.trim() === "Start session",
      ),
    ).toBe(false);

    fireEvent.click(start.querySelector<HTMLButtonElement>(".wid-start-open")!);
    await waitFor(() => expect(start.querySelector("select")).toBeTruthy());
  });

  it("places Comments directly under Description", async () => {
    fakeVogt();
    const { container } = detail();

    await waitFor(() => panelNamed(container, "Comments"));
    const headings = [...container.querySelectorAll(".wid-main .wid-panel h3")].map(
      (node) => node.textContent,
    );
    const description = headings.indexOf("Description");
    const comments = headings.indexOf("Comments");
    expect(description).toBeGreaterThanOrEqual(0);
    // Comments is the very next panel after Description.
    expect(comments).toBe(description + 1);
  });
});

// FR-U17's second clause, which §6.2 recorded as newly checkable and not met:
// "a claim backed by a still-running session is marked provisional, not
// fresh". `session_outcomes.py` writes the judgement into the evidence and
// `tests/test_session_outcomes.py` asserts it is written; until this panel
// existed nothing read it back, so the rule had nowhere to be kept and
// nowhere to be broken. These are what stops it going back to that.

describe("FR-U17 — observed evidence says whether it has settled", () => {
  it("asks the observed store, scoped to this item's project", async () => {
    const vogt = fakeVogt();
    detail();
    await waitFor(() => expect(vogt.matching("GET /observations")).toHaveLength(1));

    const asked = vogt.matching("GET /observations")[0]!;
    expect(asked.query.get("project")).toBe("alpha");
    expect(asked.query.get("latest_only")).toBe("true");
  });

  it("marks a claim backed by a still-running session provisional, not fresh", async () => {
    fakeVogt({
      "GET /observations": {
        body: {
          observations: [
            observation(
              { id: "01JLIVE", subject_key: "session:01JLIVE" },
              // What the collector writes for a session that has not
              // finished: running, provisional, and no exit code at all.
              { state: "running", provisional: true, exit_code: undefined },
            ),
          ],
          total: 1,
        },
      },
    });
    const { container } = detail();

    await waitFor(() => expect(settlements(container)).toEqual(["provisional"]));
    const panel = observed(container);
    // The distinction is carried by a class, not only by a word, so it is
    // visible at a glance and not only to a reader of the sentence.
    expect(panel.querySelector(".wid-observation--provisional")).toBeTruthy();
    expect(panel.querySelector(".wid-observation--settled")).toBeNull();
    expect(panel.querySelector(".wid-freshness--provisional")).toBeTruthy();
    // And it says what provisional means, rather than leaving it a label.
    expect(panel.textContent).toContain("had not finished when it was observed");
    expect(panel.textContent).toContain("no exit code — it had not exited");
  });

  it("does not call a finished session's outcome provisional", async () => {
    fakeVogt({
      "GET /observations": {
        body: { observations: [observation()], total: 1 },
      },
    });
    const { container } = detail();

    await waitFor(() => expect(settlements(container)).toEqual(["settled"]));
    const panel = observed(container);
    expect(panel.querySelector(".wid-observation--provisional")).toBeNull();
    expect(panel.querySelector(".wid-freshness--provisional")).toBeNull();
    expect(panel.textContent).toContain("exit 0");
  });

  it("reads evidence that does not say as unverified, never as blank", async () => {
    // An older sweep, or a collector whose kind carries no such flag. The
    // honest answer is "nobody checked", and a blank badge would say "no
    // opinion" — the rule the board and the backlog already keep.
    fakeVogt({
      "GET /observations": {
        body: {
          observations: [
            observation(
              { id: "01JQUIET", kind: "marker", collector: "markers" },
              { provisional: undefined, state: undefined, exit_code: undefined },
            ),
          ],
          total: 1,
        },
      },
    });
    const { container } = detail();

    await waitFor(() => expect(settlements(container)).toEqual(["unverified"]));
    const panel = observed(container);
    expect(panel.querySelector(".wid-observation--unverified")).toBeTruthy();
    expect(panel.textContent).toContain(
      "does not say whether what produced it had finished",
    );
    // Not settled by omission: "unknown" and "settled" are different answers.
    expect(settlements(container)).not.toContain("settled");
  });

  it("keeps one provisional row visible among settled ones", async () => {
    fakeVogt({
      "GET /observations": {
        body: {
          observations: [
            observation({ id: "01JA", subject_key: "session:01JA" }),
            observation(
              { id: "01JB", subject_key: "session:01JB" },
              { state: "running", provisional: true, exit_code: undefined },
            ),
          ],
          total: 2,
        },
      },
    });
    const { container } = detail();

    await waitFor(() =>
      expect(settlements(container)).toEqual(["settled", "provisional"]),
    );
    // One mid-flight claim makes the whole panel provisional. Averaging it
    // away would be the panel deciding the exception does not matter.
    expect(observed(container).querySelector(".wid-freshness--provisional")).toBeTruthy();
  });

  it("shows only the evidence that names this item", async () => {
    fakeVogt({
      "GET /observations": {
        body: {
          observations: [
            observation({ id: "01JMINE", subject_key: "session:01JMINE" }),
            observation(
              { id: "01JTHEIRS", subject_key: "session:01JTHEIRS" },
              { work_item: "WI-9", state: "running", provisional: true },
            ),
          ],
          total: 2,
        },
      },
    });
    const { container } = detail();

    await waitFor(() => expect(settlements(container)).toEqual(["settled"]));
    // Another item's still-running session must not make this item's evidence
    // read as provisional, which is the same lie in the other direction.
    expect(observed(container).textContent).not.toContain("session:01JTHEIRS");
    expect(observed(container).querySelector(".wid-freshness--provisional")).toBeNull();
  });

  it("says nothing has been observed rather than showing an empty panel", async () => {
    fakeVogt();
    const { container } = detail();

    await waitFor(() => expect(observed(container).querySelector(".wid-absent")).toBeTruthy());
    expect(observed(container).textContent).toContain(
      "No collector has recorded anything about WI-1",
    );
    expect(observed(container).textContent).toContain('"nobody has looked"');
    expect(settlements(container)).toHaveLength(0);
  });

  it("says the list is cut when the store had more than it asked for", async () => {
    // `total` equal to the limit means the store had at least that many. A
    // panel that showed the cut list as though it were all there is would be
    // claiming completeness it cannot have.
    fakeVogt({
      "GET /observations": {
        body: { observations: [observation()], total: 200 },
      },
    });
    const { container } = detail();

    await waitFor(() => expect(settlements(container)).toEqual(["settled"]));
    expect(observed(container).textContent).toContain("a cut list rather than all");
  });
});

describe("FR-U21 — the observed panel tells an outage from no evidence", () => {
  it("renders Vogt's own reason instead of an empty evidence panel", async () => {
    const NO_CORE = "vogt-core is not configured for this front door";
    fakeVogt({ "GET /observations": unavailable(NO_CORE) });
    const { container } = detail();

    await waitFor(() =>
      expect(observed(container).querySelector(".wid-failure")).toBeTruthy(),
    );
    const panel = observed(container);
    expect(panel.textContent).toContain("Vogt cannot be reached");
    expect(panel.textContent).toContain(NO_CORE);
    expect(panel.textContent).toContain("not because nothing has been observed");
    // Nothing is rendered as data, and nothing claims the item has no evidence.
    expect(settlements(container)).toHaveLength(0);
    expect(panel.querySelector(".wid-absent")).toBeNull();
    // The rest of the page is still the page: this is one panel's outage.
    expect(container.querySelector(".wid-outage")).toBeNull();
    expect(container.textContent).toContain("Collected evidence");
  });

  it("calls a refused read a failed read, not an outage", async () => {
    fakeVogt({
      "GET /observations": refusal(500, "observations.list: the store is locked"),
    });
    const { container } = detail();

    await waitFor(() =>
      expect(observed(container).querySelector(".wid-failure")).toBeTruthy(),
    );
    const panel = observed(container);
    expect(panel.textContent).toContain("The observed evidence could not be read");
    expect(panel.textContent).toContain("observations.list: the store is locked");
    expect(panel.textContent).not.toContain("Vogt cannot be reached");
    expect(settlements(container)).toHaveLength(0);
  });
});

describe("FR-U21 — the item page tells an outage from an empty item", () => {
  it("says Vogt cannot be reached, in Vogt's words, and disables the edit", async () => {
    fakeVogt({
      "GET /work/get": unavailable("vogt-core is not configured for this front door"),
    });
    const { container } = detail();

    await waitFor(() => expect(container.querySelector(".wid-outage")).toBeTruthy());
    expect(container.querySelector(".wid-outage")?.textContent).toContain(
      "vogt-core is not configured for this front door",
    );
    expect(container.textContent).toContain("This is an outage, not an empty work item.");
    expect(container.querySelector<HTMLButtonElement>(".wid-edit-open")?.disabled).toBe(true);
  });

  it("distinguishes a failed read from an outage", async () => {
    fakeVogt({ "GET /work/get": refusal(404, "work.get: no item WI-404") });
    const { container } = detail("WI-404");

    await waitFor(() => expect(container.querySelector(".wid-failure")).toBeTruthy());
    expect(container.querySelector(".wid-failure")?.textContent).toContain(
      "work.get: no item WI-404",
    );
    // A 404 is Vogt answering, so it is not reported as an outage.
    expect(container.querySelector(".wid-outage")).toBeNull();
  });
});


// -- FR-M1: answering a session that is waiting for input -------------------
//
// MERGE §14's M12 demo ends "open it, unblock it", and until now unblocking
// meant a terminal — a PTY under a phone keyboard, to type one character.
// These mount the item page against *both* servers: Vogt for the session
// record, the engine for what that session has on screen. The harness 404s
// unstubbed engine paths, which is what makes the engine-is-away case the
// default rather than something a test has to arrange.

const WAITING_SESSION = {
  id: "ses_01",
  engine_session_id: "eng-1",
  work_item_id: "WI-1",
  project_id: "prj_01",
  actor: "agent:session:ses_01",
  reason: "start the migration",
  cwd: "/srv/alpha",
  template: "claude",
  started_at: "2026-08-01T00:00:00Z",
  stopped_at: null,
  activity: "waiting-for-input",
  alive: true,
};

function withWaitingSession(engine: Record<string, unknown> = {}) {
  return fakeVogt(
    {
      "GET /work/get": {
        body: { item: workItem(), comments: [], sessions: [WAITING_SESSION] },
      },
      "GET /sessions": { body: { sessions: [WAITING_SESSION], engine: null } },
    },
    engine as Parameters<typeof fakeVogt>[1],
  );
}

/** A scrollback snapshot, encoded the way the engine encodes it. */
function scrollback(text: string): string {
  return btoa(String.fromCharCode(...new TextEncoder().encode(text)));
}

async function openAnswer(container: HTMLElement): Promise<void> {
  const open = await waitFor(() => {
    const button = [...container.querySelectorAll("button")].find(
      (b) => b.textContent?.trim() === "Answer…",
    );
    expect(button).toBeTruthy();
    return button!;
  });
  fireEvent.click(open);
}

describe("FR-M1 — a session waiting for input can be answered from the item", () => {
  it("offers no answer control for a session the engine does not report waiting", async () => {
    const running = { ...WAITING_SESSION, activity: "running" };
    fakeVogt({
      "GET /work/get": {
        body: { item: workItem(), comments: [], sessions: [running] },
      },
      "GET /sessions": { body: { sessions: [running], engine: null } },
    });
    const { container } = detail();
    await waitFor(() => expect(container.textContent).toContain("running"));
    expect(container.textContent).not.toContain("Answer…");
  });

  it("shows what the session is asking before there is anything to press", async () => {
    withWaitingSession({
      "GET /api/sessions/eng-1": {
        body: {
          summary: {},
          scrollback_pos: 0,
          scrollback_base64: scrollback(
            "$ ./migrate.sh\nThis will drop 4 tables.\nProceed? (y/n) ",
          ),
        },
      },
    });
    const { container } = detail();
    await openAnswer(container);

    const tail = await waitFor(() => {
      const pre = container.querySelector('[data-testid="prompt-tail"]');
      expect(pre).toBeTruthy();
      return pre!;
    });
    // The prompt, and the line that gives it its meaning.
    expect(tail.textContent).toContain("Proceed? (y/n)");
    expect(tail.textContent).toContain("This will drop 4 tables.");
  });

  it("sends the typed answer to the engine, with the return that commits it", async () => {
    const vogt = withWaitingSession({
      "GET /api/sessions/eng-1": {
        body: {
          summary: {},
          scrollback_pos: 0,
          scrollback_base64: scrollback("Which branch? "),
        },
      },
      "POST /api/sessions/eng-1/input": { body: { ok: true } },
    });
    const { container } = detail();
    await openAnswer(container);
    await waitFor(() => expect(container.textContent).toContain("Which branch?"));

    const field = container.querySelector<HTMLInputElement>(
      '[aria-label="Answer this session"]',
    )!;
    fireEvent.input(field, { target: { value: "main" } });
    fireEvent.click(
      [...container.querySelectorAll("button")].find(
        (b) => b.textContent?.trim() === "Send",
      )!,
    );

    const sent = await waitFor(() => {
      const call = vogt.engineCalls.find(
        (c) => c.method === "POST" && c.path === "/api/sessions/eng-1/input",
      );
      expect(call).toBeTruthy();
      return call!;
    });
    expect(sent.body).toEqual({ text: "main", submit: true });
  });

  it("offers y and n only when the prompt reads like a yes/no question", async () => {
    withWaitingSession({
      "GET /api/sessions/eng-1": {
        body: {
          summary: {},
          scrollback_pos: 0,
          scrollback_base64: scrollback("Enter a commit message: "),
        },
      },
    });
    const { container } = detail();
    await openAnswer(container);
    await waitFor(() =>
      expect(container.textContent).toContain("Enter a commit message:"),
    );
    const labels = [...container.querySelectorAll("button")].map((b) =>
      b.textContent?.trim(),
    );
    expect(labels).not.toContain("y");
    expect(labels).not.toContain("n");
  });

  it("answers nothing when the engine cannot say what is being asked", async () => {
    // The engine path is unstubbed, so it 404s. A blank box above a Send
    // button would invite somebody to answer a question they were never
    // shown, which is the failure this control is arranged against.
    withWaitingSession();
    const { container } = detail();
    await openAnswer(container);

    await waitFor(() =>
      expect(container.textContent).toContain("Open the terminal instead"),
    );
    expect(container.querySelector('[aria-label="Answer this session"]')).toBeNull();
    expect(container.querySelector('[data-testid="prompt-tail"]')).toBeNull();
  });

  it("reads the prompt through terminal escape sequences", async () => {
    withWaitingSession({
      "GET /api/sessions/eng-1": {
        body: {
          summary: {},
          scrollback_pos: 0,
          scrollback_base64: scrollback(
            "\x1b]0;migrate\x07\x1b[1;31mDanger\x1b[0m\r\nOverwrite? (y/n) ",
          ),
        },
      },
    });
    const { container } = detail();
    await openAnswer(container);
    const tail = await waitFor(() => {
      const pre = container.querySelector('[data-testid="prompt-tail"]');
      expect(pre?.textContent).toContain("Overwrite?");
      return pre!;
    });
    // The words, not the bytes a terminal would have eaten.
    expect(tail.textContent).toContain("Danger");
    expect(tail.textContent).not.toContain("1;31m");
    expect(tail.textContent).not.toContain("migrate");
    expect(
      [...container.querySelectorAll("button")].map((b) => b.textContent?.trim()),
    ).toContain("y");
  });
});


// -- FR-U20: the item, its session's liveness, and the way to the terminal --
//
// §6.2a said this needed "a fixture nobody has written" — an engine stub
// beside the Vogt one — which the FR-M1 work above now provides. The badge
// and the control are the forward leg; the terminal's link back is the
// return leg and is asserted in `terminalLink.test.tsx`.

describe("FR-U20 — a work item shows what its session is doing, and how to reach it", () => {
  it("shows the engine's activity for the session, not Vogt's record of it", async () => {
    fakeVogt({
      "GET /work/get": {
        body: { item: workItem(), comments: [], sessions: [WAITING_SESSION] },
      },
      "GET /sessions": {
        body: {
          sessions: [{ ...WAITING_SESSION, activity: "running" }],
          engine: null,
        },
      },
    });
    const { container } = detail();
    const badge = await waitFor(() => {
      const el = container.querySelector(".wid-activity");
      expect(el).toBeTruthy();
      return el!;
    });
    expect(badge.textContent).toContain("running");
  });

  it("says the activity is unknown rather than idle when the engine was not asked", async () => {
    // The distinction this file exists for: `activity: null` is "we do not
    // know", and rendering it as idle would be a plausible screen that is a
    // lie.
    const unasked = { ...WAITING_SESSION, activity: null, alive: null };
    fakeVogt({
      "GET /work/get": {
        body: { item: workItem(), comments: [], sessions: [unasked] },
      },
      "GET /sessions": { body: { sessions: [unasked], engine: null } },
    });
    const { container } = detail();
    const badge = await waitFor(() => {
      const el = container.querySelector(".wid-activity");
      expect(el).toBeTruthy();
      return el!;
    });
    expect(badge.textContent).toContain("unknown");
    expect(badge.textContent).not.toContain("idle");
  });

  it("offers a control that navigates to the terminal attached to that session", async () => {
    fakeVogt({
      "GET /work/get": {
        body: { item: workItem(), comments: [], sessions: [WAITING_SESSION] },
      },
      "GET /sessions": { body: { sessions: [WAITING_SESSION], engine: null } },
    });
    const { container } = detail();
    const link = await waitFor(() => {
      const el = container.querySelector<HTMLAnchorElement>(".wid-open-terminal");
      expect(el).toBeTruthy();
      return el!;
    });
    // The engine's session id, not Vogt's — they are different identifiers
    // and only one of them addresses a PTY.
    expect(link.getAttribute("href")).toBe("#/t/eng-1");
  });

  it("offers no terminal control for a session Vogt has stopped", async () => {
    // A stopped session has no PTY to attach to, so a control that led there
    // would quietly do nothing — which is worse than saying the terminal is
    // closed.
    const stopped = {
      ...WAITING_SESSION,
      activity: null,
      stopped_at: "2026-08-02T00:00:00Z",
    };
    fakeVogt({
      "GET /work/get": {
        body: { item: workItem(), comments: [], sessions: [stopped] },
      },
      "GET /sessions": { body: { sessions: [stopped], engine: null } },
    });
    const { container } = detail();
    await waitFor(() => expect(container.textContent).toContain("terminal closed"));
    expect(container.querySelector("a.wid-open-terminal")).toBeNull();
  });
});
