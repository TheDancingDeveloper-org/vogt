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

import { afterEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { MemoryRouter, Route, createMemoryHistory } from "@solidjs/router";
import { createSignal } from "solid-js";
import CommandPalette, { invalidateCommandPaletteProviders } from "../CommandPalette";
import { refreshSessions } from "../store";
import { fakeVogt, settle, workItem, type FakeVogt } from "./harness";

const PROJECT_MANIFEST_REQUEST_COUNT = 9;

function palette(fileCallbacks: {
  onNewFile?: () => void;
  onChooseFile?: () => void;
} = {}) {
  const history = createMemoryHistory();
  history.set({ value: "/board" });
  let closed = 0;
  const rendered = render(() => (
    <MemoryRouter history={history}>
      <Route
        path="*rest"
        component={() => (
          <CommandPalette
            open={true}
            onClose={() => (closed += 1)}
            {...fileCallbacks}
          />
        )}
      />
    </MemoryRouter>
  ));
  return {
    container: rendered.container,
    unmount: rendered.unmount,
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

afterEach(() => invalidateCommandPaletteProviders());

describe("file workflow commands", () => {
  it("offers only file commands backed by a real workflow", async () => {
    fakeVogt(ESTATE);
    const unsupported = palette();
    await settle();
    expect(unsupported.text()).not.toContain("New File");
    expect(unsupported.text()).not.toContain("Open File...");
    unsupported.unmount();

    const supported = palette({ onNewFile: vi.fn(), onChooseFile: vi.fn() });
    await settle();
    expect(supported.text()).toContain("New File");
    expect(supported.text()).toContain("Open File...");
  });

  it("hands New File and Open File to different workflows", async () => {
    vi.useFakeTimers();
    try {
      fakeVogt(ESTATE);
      const onNewFile = vi.fn();
      const newView = palette({ onNewFile, onChooseFile: vi.fn() });
      newView.type("New File");
      newView.click("New File");
      vi.runAllTimers();
      expect(onNewFile).toHaveBeenCalledOnce();
      newView.unmount();

      const onChooseFile = vi.fn();
      const openView = palette({ onNewFile: vi.fn(), onChooseFile });
      openView.type("Open File");
      openView.click("Open File...");
      vi.runAllTimers();
      expect(onChooseFile).toHaveBeenCalledOnce();
    } finally {
      vi.useRealTimers();
    }
  });
});

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

describe("accessible command interaction", () => {
  it("exposes a named modal combobox, listbox and active option", async () => {
    fakeVogt(ESTATE);
    palette();

    const dialog = screen.getByRole("dialog", { name: "Command palette" });
    const query = screen.getByRole("combobox", { name: "Search commands" });
    const results = screen.getByRole("listbox", { name: "Command results" });
    await waitFor(() => expect(query).toHaveFocus());
    expect(dialog).toHaveAttribute("aria-modal", "true");
    expect(query).toHaveAttribute("aria-controls", results.id);

    const first = screen.getByRole("option", { name: /Open Board/ });
    expect(first).toHaveAttribute("aria-selected", "true");
    expect(query).toHaveAttribute("aria-activedescendant", first.id);
    fireEvent.keyDown(query, { key: "ArrowDown" });
    const second = screen.getByRole("option", { name: /Open Backlog/ });
    expect(second).toHaveAttribute("aria-selected", "true");
    expect(query).toHaveAttribute("aria-activedescendant", second.id);
    expect(screen.getByRole("status")).toHaveTextContent("Open Backlog selected");
  });

  it("keeps focus in the query while arrows and Enter execute the active option", async () => {
    fakeVogt(ESTATE);
    const view = palette();
    const query = screen.getByRole("combobox", { name: "Search commands" });
    await waitFor(() => expect(query).toHaveFocus());

    fireEvent.keyDown(query, { key: "ArrowDown" });
    expect(query).toHaveFocus();
    fireEvent.keyDown(query, { key: "Enter" });

    await waitFor(() => expect(view.url()).toBe("/backlog"));
    expect(view.closed()).toBe(1);
  });

  it("restores its invoker and reopens with a blank query and first selection", async () => {
    fakeVogt(ESTATE);
    const history = createMemoryHistory();
    history.set({ value: "/board" });
    let setOpen!: (open: boolean) => void;
    const rendered = render(() => {
      const [open, updateOpen] = createSignal(false);
      setOpen = updateOpen;
      return (
        <MemoryRouter history={history}>
          <Route
            path="*rest"
            component={() => (
              <>
                <button onClick={() => setOpen(true)}>Go to…</button>
                <CommandPalette open={open()} onClose={() => setOpen(false)} />
              </>
            )}
          />
        </MemoryRouter>
      );
    });
    const invoker = rendered.getByRole("button", { name: "Go to…" });
    invoker.focus();
    fireEvent.click(invoker);
    let query = await screen.findByRole("combobox", { name: "Search commands" });
    await waitFor(() => expect(query).toHaveFocus());
    fireEvent.input(query, { target: { value: "audit" } });
    expect(query).toHaveValue("audit");

    fireEvent.keyDown(document, { key: "Escape" });
    await waitFor(() => expect(invoker).toHaveFocus());
    fireEvent.click(invoker);
    query = await screen.findByRole("combobox", { name: "Search commands" });
    await waitFor(() => expect(query).toHaveFocus());
    expect(query).toHaveValue("");
    expect(screen.getByRole("option", { name: /Open Board/ })).toHaveAttribute(
      "aria-selected",
      "true",
    );
  });
});

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

describe("command palette provider lifecycle", () => {
  it("renders static commands before dynamic providers resolve", async () => {
    let release = () => {};
    const waiting = new Promise<void>((resolve) => { release = resolve; });
    fakeVogt({
      "GET /projects": async () => {
        await waiting;
        return ESTATE["GET /projects"];
      },
      "GET /work": async () => {
        await waiting;
        return ESTATE["GET /work"];
      },
    });

    const view = palette();
    expect(view.text()).toContain("Open Board");
    expect(view.text()).toContain("Loading projects…");
    release();
    await waitFor(() => expect(view.text()).toContain("Open project rustnzb"));
  });

  it("reuses cached estate providers across repeated opens", async () => {
    const vogt = fakeVogt(ESTATE, { "GET /api/agent-tasks": { body: [] } });
    const first = palette();
    await waitFor(() => expect(first.text()).toContain("Open project rustnzb"));
    const afterFirst = {
      projects: vogt.matching("GET /projects").length,
      work: vogt.matching("GET /work").length,
      tasks: vogt.engineCalls.filter((call) => call.path === "/api/agent-tasks").length,
    };
    first.unmount();

    const second = palette();
    await waitFor(() => expect(second.text()).toContain("Open project rustnzb"));
    expect(vogt.matching("GET /projects")).toHaveLength(afterFirst.projects);
    expect(vogt.matching("GET /work")).toHaveLength(afterFirst.work);
    expect(vogt.engineCalls.filter((call) => call.path === "/api/agent-tasks"))
      .toHaveLength(afterFirst.tasks);
  });

  it("defers and caches the bounded workspace manifest scan until hash mode", async () => {
    const manifest = { name: "package.json", path: "web/package.json" };
    const vogt = fakeVogt(ESTATE, {
      "GET /api/agent-tasks": { body: [] },
      "GET /api/search/files": (call) => ({
        body: call.query.get("q") === "package.json" ? [manifest] : [],
      }),
      "GET /api/files": {
        body: {
          path: "web/package.json",
          size: 42,
          content: JSON.stringify({ name: "vogt-web", scripts: { test: "vitest run" } }),
          content_base64: null,
          is_binary: false,
        },
      },
    });
    const first = palette();
    await waitFor(() => expect(first.text()).toContain("Open Board"));
    expect(vogt.engineCalls.filter((call) => call.path === "/api/search/files"))
      .toHaveLength(0);

    first.type("#test");
    await waitFor(() => expect(first.text()).toContain("Run npm test"));
    expect(vogt.engineCalls.filter((call) => call.path === "/api/search/files"))
      .toHaveLength(PROJECT_MANIFEST_REQUEST_COUNT);
    first.unmount();

    const second = palette();
    second.type("#test");
    await waitFor(() => expect(second.text()).toContain("Run npm test"));
    expect(vogt.engineCalls.filter((call) => call.path === "/api/search/files"))
      .toHaveLength(PROJECT_MANIFEST_REQUEST_COUNT);
  });
});

describe("History palette deep links", () => {
  it("opens a selected output match with its query, session and excerpt", async () => {
    fakeVogt(ESTATE, {
      "GET /api/history/search": { body: [{
        session_id: "archive-beta",
        session_name: "beta archive",
        created_at: "2026-08-18T10:00:00Z",
        match_snippet: "beta says <mark>needle</mark>",
        rank: -1,
      }] },
    });
    const view = palette();

    view.type("> needle");
    await waitFor(() => expect(view.text()).toContain("beta archive"));
    view.click("beta archive");

    await waitFor(() => expect(view.url()).toMatch(
      /^\/history\?q=needle&session=archive-beta&match=m[0-9a-f]{8}$/,
    ));
    expect(view.closed()).toBe(1);
  });

  it("opens Search History with the archived-output field requested", async () => {
    fakeVogt(ESTATE);
    const view = palette();
    await waitFor(() => expect(view.text()).toContain("Search History"));

    view.click("Search History");

    await waitFor(() => expect(view.url()).toBe("/history?focus=search"));
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
