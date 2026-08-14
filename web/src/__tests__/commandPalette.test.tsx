// The keyboard path to the Vogt surfaces (FR-U16).
//
// §6.2: "Views, sessions and work items, yes… **Projects are not reachable by
// name**: the palette imports `listWork` and nothing else from `vogtApi.ts`,
// so 'open project rustnzb' is not a thing the keyboard can do."
//
// `tests/test_pwa.py` already asserts the half that is checkable from source
// — that the palette imports no write, so no palette entry can invent a
// reason. What it cannot check is whether the reads are *reachable by name*,
// which is the clause this file covers.

import { afterEach, describe, expect, it } from "vitest";
import { fireEvent, render, waitFor } from "@solidjs/testing-library";
import { MemoryRouter, Route, createMemoryHistory } from "@solidjs/router";
import CommandPalette from "../CommandPalette";
import { refreshSessions } from "../store";
import { fakeVogt, settle, workItem, type FakeVogt } from "./harness";

function palette() {
  const history = createMemoryHistory();
  history.set({ value: "/board" });
  let closed = 0;
  const rendered = render(() => (
    <MemoryRouter history={history}>
      <Route
        path="*rest"
        component={() => (
          <CommandPalette open={true} onClose={() => (closed += 1)} />
        )}
      />
    </MemoryRouter>
  ));
  return {
    container: rendered.container,
    url: () => history.get(),
    closed: () => closed,
    type(text: string) {
      const input = rendered.container.querySelector("input")!;
      fireEvent.input(input, { target: { value: text } });
    },
    /** Every rendered row's text, whatever the shell calls its classes. */
    text(): string {
      return rendered.container.textContent ?? "";
    },
    click(label: string) {
      const found = [...rendered.container.querySelectorAll<HTMLElement>("*")].find(
        (node) =>
          node.children.length === 0 && (node.textContent ?? "").trim() === label,
      );
      if (!found) throw new Error(`no palette row reading "${label}"`);
      fireEvent.click(found);
    },
  };
}

const ESTATE = {
  "GET /projects": {
    body: {
      projects: [
        { slug: "rustnzb", name: "rustnzb" },
        { slug: "vogt", name: "Vogt" },
      ],
    },
  },
  "GET /work": {
    body: { items: [workItem({ ref: "WI-7", title: "Ship the board" })], total: 1 },
  },
};

describe("FR-U16 — every read surface by fuzzy name", () => {
  it("offers each registered project by name", async () => {
    fakeVogt(ESTATE);
    const view = palette();

    await waitFor(() => expect(view.text()).toContain("Open project rustnzb"));
    expect(view.text()).toContain("Open project Vogt");
  });

  it("finds a project by a fuzzy fragment of its name", async () => {
    fakeVogt(ESTATE);
    const view = palette();
    await waitFor(() => expect(view.text()).toContain("Open project rustnzb"));

    view.type("rstnz");
    await waitFor(() => expect(view.text()).toContain("Open project rustnzb"));
    // and the fragment does not also drag in the whole Vogt menu
    expect(view.text()).not.toContain("Open project Vogt");
  });

  it("opens the project's own deep link, which is the one a shared URL uses", async () => {
    fakeVogt(ESTATE);
    const view = palette();
    await waitFor(() => expect(view.text()).toContain("Open project rustnzb"));

    view.click("Open project rustnzb");
    await waitFor(() => expect(view.url()).toBe("/projects?p=rustnzb"));
    expect(view.closed()).toBe(1);
  });

  it("still reaches work items and every view by name", async () => {
    fakeVogt(ESTATE);
    const view = palette();

    await waitFor(() => expect(view.text()).toContain("WI-7 — Ship the board"));
    for (const label of ["Open Board", "Open Backlog", "Open Projects", "Open Audit"]) {
      expect(view.text()).toContain(label);
    }
  });

  it("contributes nothing when Vogt cannot be asked, rather than failing open", async () => {
    fakeVogt({
      "GET /projects": { status: 503, body: { error: { message: "no core" } } },
      "GET /work": { status: 503, body: { error: { message: "no core" } } },
    });
    const view = palette();

    // The surfaces are where an outage is reported; a command list somebody
    // is typing into is not.
    await waitFor(() => expect(view.text()).toContain("Open Board"));
    expect(view.text()).not.toContain("Open project");
    expect(view.text()).not.toContain("no core");
  });
});


describe("FR-U16 — a mutating verb opens the view that collects its reason", () => {
  it("opens the drift inbox rather than resolving anything", async () => {
    fakeVogt(ESTATE);
    const view = palette();
    await settle();
    view.type("Resolve Drift");
    view.click("Resolve Drift...");
    // The inbox, where each proposal shows both sides and takes a typed
    // reason. The palette cannot type one, which is why it must not act.
    await waitFor(() => expect(view.url()).toBe("/projects?view=drift"));
    expect(view.closed()).toBe(1);
  });

  it("opens the import form rather than importing anything", async () => {
    fakeVogt(ESTATE);
    const view = palette();
    await settle();
    view.type("Import a Project");
    view.click("Import a Project...");
    await waitFor(() => expect(view.url()).toBe("/projects?view=import"));
  });

  it("writes nothing to Vogt when a mutating verb is chosen", async () => {
    // The rule the whole entry set exists under: open, never execute.
    // Asserted at runtime here and by import in `test_pwa.py`, because the
    // two catch different mistakes — an entry that calls a write binding,
    // and an entry that posts by some other path.
    const vogt = fakeVogt(ESTATE);
    const view = palette();
    await settle();
    view.type("Resolve Drift");
    view.click("Resolve Drift...");
    await waitFor(() => expect(view.url()).toContain("view=drift"));
    expect(vogt.calls.filter((call) => call.method !== "GET")).toEqual([]);
  });
});

// -- FR-U16's fourth read surface -------------------------------------------
//
// §6.2a: "A palette test with a populated session store. `commandPalette.
// test.tsx` asserts projects, work items and views, and the session entries
// come from the engine's store, which no test seeds."
//
// The store is the engine's, not Vogt's: `store.ts` fills it from
// `GET /api/sessions` on the engine's own API, which `harness.tsx` answers
// through its second argument. Seeding it is the whole difficulty, and it is
// not much of one — but until something does, the "sessions" in FR-U16's
// "projects, work items, sessions, views" is a clause with an empty list
// behind it, and a palette that stopped offering sessions altogether would
// pass every test above this one.
//
// `sessionsStore` is module state that outlives a mount, so this block empties
// it again afterwards: a leaked session is the next test's phantom row.

/** Two engine sessions, as `GET /api/sessions` returns them. */
const SESSIONS = [
  {
    id: "ses_alpha",
    name: "alpha-refactor",
    activity: "running",
    exit_code: null,
    scrollback_bytes: 0,
    cwd: "/srv/alpha",
    created_at: "2026-08-01T00:00:00Z",
  },
  {
    id: "ses_beta",
    name: "beta-deploy",
    activity: "idle",
    exit_code: null,
    scrollback_bytes: 0,
    cwd: "/srv/beta",
    created_at: "2026-08-01T00:00:00Z",
  },
];

/** Fill the engine's session store the way the shell does, from the engine. */
async function seedSessions(sessions: unknown[] = SESSIONS): Promise<FakeVogt> {
  const vogt = fakeVogt(ESTATE, { "GET /api/sessions": { body: sessions } });
  await refreshSessions();
  return vogt;
}

describe("FR-U16 — sessions are a read surface, and reachable by name", () => {
  afterEach(async () => {
    // The store is module state; the next test would inherit these.
    await seedSessions([]);
  });

  it("offers every session the engine has, under the name it carries", async () => {
    await seedSessions();
    const view = palette();

    await waitFor(() => expect(view.text()).toContain("alpha-refactor"));
    expect(view.text()).toContain("beta-deploy");
    // Named with where it is running, which is what tells two shells apart.
    expect(view.text()).toContain("/srv/alpha");
  });

  it("finds a session by a fuzzy fragment of its name", async () => {
    await seedSessions();
    const view = palette();
    await waitFor(() => expect(view.text()).toContain("alpha-refactor"));

    view.type("arfc");

    await waitFor(() => expect(view.text()).toContain("alpha-refactor"));
    // A fragment matching one session does not drag the other in with it.
    expect(view.text()).not.toContain("beta-deploy");
  });

  it("opens the session's terminal rather than doing anything to it", async () => {
    const vogt = await seedSessions();
    const view = palette();
    await waitFor(() => expect(view.text()).toContain("beta-deploy"));
    const before = vogt.engineCalls.length;

    view.click("beta-deploy");

    await waitFor(() => expect(view.url()).toBe("/t/ses_beta"));
    expect(view.closed()).toBe(1);
    // Reaching a session is a read. The palette starts nothing, kills
    // nothing, and writes nowhere — to Vogt or to the engine.
    expect(vogt.calls.filter((call) => call.method !== "GET")).toEqual([]);
    expect(vogt.engineCalls.slice(before).filter((call) => call.method !== "GET")).toEqual(
      [],
    );
  });

  it("offers no sessions when the engine has none to offer", async () => {
    await seedSessions([]);
    const view = palette();

    // The views are still there, so this is an empty engine and not a
    // palette that failed to build its list at all.
    await waitFor(() => expect(view.text()).toContain("Open Board"));
    expect(view.text()).not.toContain("Jump to session");
  });
});
