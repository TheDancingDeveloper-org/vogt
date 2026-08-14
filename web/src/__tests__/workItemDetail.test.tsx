// One work item, and the inline edit FR-U12 names and nothing implemented.
//
// §6.2: "There is no inline edit. `updateWork` is exported by `vogtApi.ts` and
// called by nothing — the dead binding that `test_pwa.py`'s own parity
// docstring warns about." These tests are what stops it going back.

import { describe, expect, it } from "vitest";
import { fireEvent, waitFor } from "@solidjs/testing-library";
import WorkItemDetail from "../WorkItemDetail";
import {
  fakeVogt,
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
