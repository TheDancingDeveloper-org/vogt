// The shell, and what a pasted link does to it (FR-U11, FR-T6).
//
// Every other file here mounts one surface at one URL, which settles what a
// surface does with the URL it is handed and says nothing about how it came
// to be handed one. `App.tsx`'s URL effect is the half in between — the
// half M11 found broken for *every* surface, and the half §6.2a records as
// "mounted by nothing". So this file mounts the shell, behind the same route
// table `index.tsx` routes with, and asserts the property FR-U11 actually
// claims: a link names a surface, and opening the link opens that surface,
// with the thing the rest of the link names already in it.
//
// It was untestable for a smaller reason than it looked. `App.tsx` boots into
// a login gate, reads `/api/config`, opens a file tree and starts an event
// stream — all of them the *engine's* API, which the harness answers only
// where a test says so. Once `/api/status`, `/api/config`, `/api/sessions`
// and `/api/tree` are stubbed, the shell mounts in jsdom like any other
// component. Nothing about it needed a browser.
//
// Two things are deliberately not faked. The router is a real
// `@solidjs/router` with a real history, so `params` and `location` are the
// product's own; and the surfaces behind the tabs are the real surfaces,
// reading the real front door — a tab that opened but drew nothing would fail
// these tests, which is the failure a person pasting the link would see.

import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { MemoryRouter, Route, createMemoryHistory } from "@solidjs/router";
import { render, waitFor } from "@solidjs/testing-library";

import App from "../App";
import { APP_ROUTES } from "../routes";
import { setToken } from "../api";
import { replaceTabs, tabsStore } from "../tabs";
import {
  fakeVogt,
  INBOX_RESULT,
  stopLiveStream,
  workItem,
  type FakeVogt,
  type Routes,
} from "./harness";

/** A session the engine knows about, as `GET /api/sessions` lists them. */
const SESSION = {
  id: "eng-1",
  name: "alpha-build",
  cwd: "/srv/alpha",
  activity: "idle",
  exit_code: null,
};

interface Shell {
  container: HTMLElement;
  vogt: FakeVogt;
  /** Paste another link, as the address bar would. */
  go(url: string): void;
}

interface ShellOptions {
  /** Overrides merged into `/api/config` — `assistant_enabled` lives here. */
  config?: Record<string, unknown>;
  /** What `GET /api/sessions` lists, which is what a `/t/:id` link resolves against. */
  sessions?: unknown[];
  /** Extra Vogt handlers, keyed as elsewhere. */
  vogt?: Routes;
  /** Extra session-engine handlers for the machine tools under Sessions. */
  engine?: Routes;
}

/**
 * Mount the whole shell at one URL.
 *
 * The engine stubs are the shell's own boot, not the requirement: a token the
 * front door accepts (`/api/status`), the public config every gate reads, the
 * session list the places rail and the terminal links resolve against, and the
 * file tree in the rail. Left unstubbed they answer 404 — which is a real
 * state and one `absentStates.test.tsx` is about, and not the state a test of
 * "does this link open this surface" wants to be in.
 */
function mountShell(url: string, options: ShellOptions = {}): Shell {
  setToken("shell-test-token");
  const vogt = fakeVogt(options.vogt ?? {}, {
    "GET /api/status": { body: { ok: true } },
    "GET /api/config": {
      body: {
        version: "test",
        gui_stream_url: null,
        gui_stream_available: false,
        assistant_enabled: false,
        vogt: { configured: true },
        ...options.config,
      },
    },
    "GET /api/sessions": { body: options.sessions ?? [] },
    "GET /api/tree": { body: [] },
    ...options.engine,
  });

  const history = createMemoryHistory();
  history.set({ value: url });
  const rendered = render(() => (
    <MemoryRouter history={history}>
      <Route path={[...APP_ROUTES]} component={App} />
    </MemoryRouter>
  ));

  return {
    container: rendered.container,
    vogt,
    go: (next: string) => history.set({ value: next }),
  };
}

/**
 * The place or pane the reader is actually looking at.
 *
 * Terminal panes remain mounted for continuity; inactive non-terminal tools
 * unmount. A test must still select the pane whose inline display is `flex`,
 * because retained terminals deliberately remain in the DOM.
 */
function shown(container: HTMLElement): HTMLElement | null {
  const place = container.querySelector<HTMLElement>(".stable-place");
  if (place) return place;
  const panes = [...container.querySelectorAll<HTMLElement>(".tab-view > div")];
  return panes.find((pane) => pane.style.display === "flex") ?? null;
}

/** The surface on screen, waited for by the class it renders itself under. */
async function surface(container: HTMLElement, selector: string): Promise<HTMLElement> {
  // The panes are lazy in the shipped bundle (#104), so the first look at one
  // waits for its chunk as well as for its render.
  return await waitFor(() => {
    const pane = shown(container);
    const found = pane?.querySelector<HTMLElement>(selector);
    expect(found, `no ${selector} is on screen`).toBeTruthy();
    return found!;
  }, { timeout: 5_000 });
}

/** Stable places deliberately have no top-level tab strip. */
function tabLabels(container: HTMLElement): string[] {
  return [...container.querySelectorAll(".tab-strip .tab .label")].map(
    (node) => node.textContent ?? "",
  );
}

beforeEach(() => {
  localStorage.removeItem("vogt.rail.sections.v1");
  // `tabs.ts` holds its store in module state, so a tab opened by the last
  // test is still open in this one — and every assertion here is about which
  // tabs a URL produced.
  replaceTabs({ tabs: [], active: null });
});

afterEach(() => {
  // The shell starts the engine's event stream on a successful login.
  stopLiveStream();
  setToken("");
});

describe("FR-U11 — a pasted link opens the surface it names", () => {
  it("opens the board", async () => {
    const { container } = mountShell("/board");

    await surface(container, ".vogt-surface.board");
    expect(tabLabels(container)).toEqual([]);
    expect(container.querySelector(".tab-strip")).toBeNull();
    expect(
      [...container.querySelectorAll<HTMLElement>(".places-nav a")].map(
        (link) => [link.querySelector("span")?.textContent ?? link.textContent, link.getAttribute("href")],
      ),
    ).toEqual([
      ["Board", "#/board"],
      ["Backlog", "#/backlog"],
      ["Inbox", "#/inbox"],
      ["Projects", "#/projects"],
      ["Audit", "#/audit"],
      ["Sessions", "#/sessions"],
      ["Git", "#/g"],
      ["History", "#/history"],
      ["Tasks", "#/tasks"],
    ]);
  });

  it("opens the ranked backlog", async () => {
    const { container } = mountShell("/backlog");

    await surface(container, ".vogt-surface.vogt-backlog");
    expect(tabLabels(container)).toEqual([]);
  });

  it("opens the project page on the project the link names", async () => {
    // The path opens the surface and the query says where in it: the two
    // halves of one link, and the second is worthless without the first.
    const { container } = mountShell("/projects?p=alpha");

    const page = await surface(container, ".vogt-surface.vogt-projects");
    await waitFor(() =>
      expect(page.querySelector(".vogt-projects-crumb.active")?.textContent).toBe(
        "alpha",
      ),
    );
  });

  it("opens the work item the link names, and asks Vogt for that item", async () => {
    const { container, vogt } = mountShell("/w/WI-1");

    const page = await surface(container, ".vogt-surface.wid-view");
    await waitFor(() =>
      expect(page.textContent).toContain(
        "Teach the board to say what it does not know",
      ),
    );
    expect(vogt.matching("GET /work/get")[0]?.query.get("ref")).toBe("WI-1");
    expect(tabLabels(container)).toEqual([]);
  });

  it("opens an audit query, narrowed to what the link carried", async () => {
    // The link the item page hands out: `#/audit?ref=WI-1`. FR-U11 names the
    // audit *query* as addressable, so arriving at an unfiltered log would be
    // a different answer wearing the same heading.
    const { container, vogt } = mountShell("/audit?ref=WI-1");

    await surface(container, ".vogt-surface.vab");
    await waitFor(() => expect(vogt.matching("GET /audit").length).toBeGreaterThan(0));
    const asked = vogt.matching("GET /audit").at(-1)!;
    expect(asked.query.get("entity_id")).toBe(workItem().id);
  });

  it("opens the session's terminal, under the name the server gave it", async () => {
    const { container } = mountShell("/t/eng-1", { sessions: [SESSION] });

    await surface(container, ".terminal-host");
    expect(tabLabels(container)).toEqual([]);
  });

  it("names a missing terminal and offers recovery without a phantom tab", async () => {
    const { container } = mountShell("/t/eng-gone", { sessions: [SESSION] });

    await waitFor(() => expect(container.textContent).toContain("Session not found"));
    expect(container.textContent).toContain("Return to Sessions");
    expect(tabLabels(container)).toEqual([]);
    expect(container.querySelector(".terminal-host")).toBeNull();
  });

  it("redirects the root to the desktop default place", async () => {
    const { container } = mountShell("/");

    await surface(container, ".vogt-surface.board");
    expect(tabLabels(container)).toEqual([]);
  });

  it("follows a second link into a second stable place", async () => {
    // The effect runs on every URL, not once at boot. Stable places replace
    // one another in the main area and never accumulate closable tabs.
    const { container, go } = mountShell("/board");
    await surface(container, ".vogt-surface.board");

    go("/backlog");

    await surface(container, ".vogt-surface.vogt-backlog");
    expect(tabLabels(container)).toEqual([]);
    expect(container.querySelector(".tab-strip")).toBeNull();
  });
});

describe("shared live steering", () => {
  it("labels desktop and phone counts, then refreshes them from the live stream", async () => {
    const { container, vogt } = mountShell("/sessions", {
      sessions: [
        SESSION,
        { ...SESSION, id: "eng-wait", name: "needs-answer", activity: "waiting-for-input" },
      ],
      vogt: {
        "GET /inbox": { body: { ...INBOX_RESULT, counts: { active: 0 } } },
        "GET /projects": { status: 503, body: { error: { message: "projects offline" } } },
        "GET /work": { body: { items: [], total: 12 } },
        "GET /backlog": {
          body: { items: [], total_considered: 7, freshness: { status: "fresh", collectors: {} } },
        },
      },
    });

    await waitFor(() => {
      // #168: the rail Sessions badge counts all sessions (2), matching the
      // mobile nav and the Running section — not just those waiting for input.
      expect(container.querySelector('aside nav [aria-label="2 sessions"]')).toBeTruthy();
      expect(container.querySelector('[aria-label="0 active Inbox entries"]')).toBeTruthy();
      expect(container.querySelector('[aria-label="Projects unavailable"]')).toBeTruthy();
      expect(container.querySelector('.phone-bottom-nav [aria-label="12 Board work items"]')).toBeTruthy();
      expect(container.querySelector('.phone-bottom-nav [aria-label="7 Backlog candidates"]')).toBeTruthy();
      expect(container.querySelector('.places-section-label [aria-label="2 running sessions"]')).toBeTruthy();
      expect(container.querySelectorAll(".session-row.waiting")).toHaveLength(1);
      expect(container.querySelector(".rail-connection.connected .rail-connection-dot")).toBeTruthy();
    });

    await vogt.stream.opened();
    vogt.route("GET /inbox", {
      body: { ...INBOX_RESULT, counts: { active: 3 } },
    });
    vogt.stream.changed();
    await waitFor(() =>
      expect(container.querySelector('[aria-label="3 active Inbox entries"]')).toBeTruthy(),
    );
  });

  it("renders one navigation-only waiting attention pointer and a single session menu", async () => {
    const { container } = mountShell("/sessions", {
      sessions: [{ ...SESSION, activity: "waiting-for-input", activity_changed_at: new Date().toISOString() }],
    });
    await waitFor(() => {
      expect(container.querySelectorAll(".rail-attention")).toHaveLength(1);
      expect(container.querySelector(".rail-attention")?.getAttribute("href")).toBe("#/t/eng-1");
      expect(container.querySelectorAll(".session-row .row-menu")).toHaveLength(1);
      expect(container.querySelectorAll(".session-row .row-btn, .session-row .close")).toHaveLength(0);
    });
  });

  it("persists a collapsed Files section and keeps the running count in its toggle", async () => {
    localStorage.removeItem("vogt.rail.sections.v1");
    const { container } = mountShell("/sessions", { sessions: [SESSION] });
    await waitFor(() => expect(container.querySelector(".places-section-toggle")).toBeTruthy());
    const files = [...container.querySelectorAll<HTMLButtonElement>(".places-section-toggle")].find((button) => button.textContent?.includes("Files"));
    expect(files?.getAttribute("aria-expanded")).toBe("false");
    files?.click();
    expect(JSON.parse(localStorage.getItem("vogt.rail.sections.v1") ?? "{}").files).toBe(true);
  });

  it("keeps the last session values stale when a refresh goes offline", async () => {
    const { container } = mountShell("/sessions", {
      engine: {
        "GET /api/sessions": {
          status: 503,
          body: { error: "session service offline" },
        },
      },
    });

    await waitFor(() => {
      expect(container.querySelector('.places-nav .place-count[data-state="stale"]')).toBeTruthy();
      expect(container.querySelector('.places-section-label .place-count[data-state="stale"]')).toBeTruthy();
      expect(container.querySelector(".rail-connection.offline .rail-connection-dot")).toBeTruthy();
      expect(container.textContent).toContain("Offline");
    });
  });

  it("opens, bookmarks, and closes a session entirely from the keyboard", async () => {
    const { container } = mountShell("/sessions", {
      sessions: [SESSION],
      engine: {
        "POST /api/sessions/eng-1/kill": { body: { ok: true } },
        "DELETE /api/sessions/eng-1": { body: { ok: true } },
      },
    });
    const row = await waitFor(() => {
      const found = container.querySelector<HTMLElement>('.session-row[role="link"]');
      expect(found).toBeTruthy();
      return found!;
    });
    row.focus();
    row.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    await waitFor(() => expect(tabsStore.active).toBe("term:eng-1"));

    const menu = container.querySelector<HTMLButtonElement>('[aria-label="Actions for alpha-build"]');
    expect(menu).toBeTruthy();
    menu!.focus();
    menu!.click();
    const bookmark = await waitFor(() => [...container.querySelectorAll<HTMLButtonElement>('[role="menuitem"]')].find((button) => button.textContent?.includes("Bookmark"))!);
    expect(bookmark).toBeTruthy();
    bookmark!.click();
    menu!.click();
    const close = await waitFor(() => [...container.querySelectorAll<HTMLButtonElement>('[role="menuitem"]')].find((button) => button.textContent?.includes("Kill & remove"))!);
    close!.click();
    const confirm = await waitFor(() => {
      const found = [...document.querySelectorAll<HTMLButtonElement>("button")]
        .find((button) => button.textContent?.trim() === "Confirm");
      expect(found).toBeTruthy();
      return found!;
    });
    confirm.click();
    await waitFor(() => expect(container.querySelector('.session-row[role="link"]')).toBeNull());
  });
});

describe("FR-T6 — the assistant is not there to be reached when it is not provisioned", () => {
  it("explains a hand-typed #/assistant when no key is configured", async () => {
    // The route exists in the table — it has to, or the URL would not resolve
    // at all — so the gate is the config, read in the effect. Without it the
    // link opened a tab against routes that answer 404, which is a worse
    // answer than no tab: it puts the surface in front of a reader who cannot
    // have it, and then fails.
    const { container } = mountShell("/assistant", {
      config: { assistant_enabled: false },
    });

    // Wait for the config to have arrived, so this is an assertion about a
    // resolved gate and not about a race the shell would lose later.
    await waitFor(() =>
      expect(container.querySelector(".places-rail")).toBeTruthy(),
    );
    await waitFor(() => expect(container.textContent).toContain("Assistant is unavailable"));
    expect(tabLabels(container)).toEqual([]);
    expect(container.textContent).not.toContain("Ask about your terminal sessions");
  });

  it("opens it for the same link when a key is configured", async () => {
    // The mirror, and the reason the test above is not passing on a dead
    // route: the arm is live, and what closes it is the config.
    const { container } = mountShell("/assistant", {
      config: { assistant_enabled: true },
    });

    await waitFor(() =>
      expect(shown(container)?.textContent).toContain(
        "Ask about your terminal sessions",
      ),
    );
    expect(container.querySelector(".tab-strip")).toBeNull();
    expect(tabLabels(container)).toEqual([]);
  });
});

describe("route-owned navigation state", () => {
  it("identifies Sessions and History as the current place and tool", async () => {
    const { container } = mountShell("/history");

    await surface(container, ".history-view");
    const current = [...container.querySelectorAll('a[aria-current="page"]')]
      .map((link) => link.textContent?.trim());
    expect(current).toContain("History");
    expect(current).not.toContain("Board");
  });

  it("hides GUI affordances but gives old direct links a truthful outcome", async () => {
    const { container } = mountShell("/gui", {
      config: { gui_stream_url: null, gui_stream_available: false },
    });

    await waitFor(() => expect(container.textContent).toContain("GUI stream is unavailable"));
    expect([...container.querySelectorAll(".places-nav a")].map((link) => link.textContent))
      .not.toContain("GUI stream");
    expect(container.querySelector(".gui-shell")).toBeNull();
  });

  it("removes a restored GUI tab after the public gate resolves disabled", async () => {
    replaceTabs({
      tabs: [{ id: "gui", kind: "gui", label: "GUI" }],
      active: "gui",
    });

    mountShell("/sessions", {
      config: { gui_stream_url: null, gui_stream_available: false },
    });

    await waitFor(() =>
      expect(tabsStore.tabs.some((tab) => tab.kind === "gui")).toBe(false),
    );
  });

  it("removes a restored missing terminal without navigating away from its outcome", async () => {
    replaceTabs({
      tabs: [{ id: "term:gone", kind: "terminal", sessionId: "gone", label: "gone" }],
      active: "term:gone",
    });

    const { container } = mountShell("/t/gone", { sessions: [] });

    await waitFor(() => expect(container.textContent).toContain("Session not found"));
    expect(tabsStore.tabs.some((tab) => tab.kind === "terminal")).toBe(false);
    expect(container.querySelector(".terminal-host")).toBeNull();
  });
});

describe("FR-U23 — Sessions owns its machine workspace", () => {
  it("keeps the machine workspace and internal pane bar around a deep-linked tool", async () => {
    const { container } = mountShell("/history", { sessions: [SESSION] });

    await surface(container, ".history-view");
    // The Live Sessions sub-panel was removed (#167); Sessions still owns the
    // machine workspace, so its place and tool bar persist around a deep link.
    expect(container.querySelector(".sessions-place")).toBeTruthy();
    expect(container.querySelector('[aria-label="Session tools"] a[aria-current="page"]')?.textContent)
      .toBe("History");
  });

  it("retains terminals across tool routes but unmounts inactive non-terminals", async () => {
    const { container, go, vogt } = mountShell("/t/eng-1", {
      sessions: [SESSION],
      config: { assistant_enabled: true },
    });
    await surface(container, ".terminal-host");

    go("/history");
    await surface(container, ".history-view");
    expect(container.querySelectorAll('[data-tab-kind="terminal"]')).toHaveLength(1);
    expect(container.querySelectorAll('[data-tab-kind="history"]')).toHaveLength(1);

    go("/tasks");
    await surface(container, ".agent-tasks-view");
    expect(container.querySelectorAll('[data-tab-kind="terminal"]')).toHaveLength(1);
    expect(container.querySelector('[data-tab-kind="history"]')).toBeNull();
    expect(container.querySelectorAll('[data-tab-kind="tasks"]')).toHaveLength(1);
    expect(vogt.engineCalls.filter((call) => call.path.startsWith("/api/history")))
      .not.toHaveLength(0);
  });

  it("composes every machine tool at its existing deep link", async () => {
    const { container, go } = mountShell("/g/vogt", {
      config: {
        assistant_enabled: true,
        gui_stream_url: "https://stream.example.test/view",
        gui_stream_available: true,
      },
      engine: {
        "GET /api/git/status": { body: {
          repo: "vogt", branch: "main", ahead: 0, behind: 0,
          entries: [], is_repo: true,
        } },
        "GET /api/git/branch": { body: { current: "main", all: ["main"] } },
        "GET /api/git/log": { body: [] },
        "GET /api/history/sessions": { body: [] },
        "GET /api/agent-tasks": { body: [] },
        "GET /api/gui/processes": { body: [] },
        "GET /api/assistant/history": { body: { transcript: [], pending_action: null } },
        "GET /api/files": { body: {
          path: "src/app.ts", size: 24,
          content: "export const app = true;\n", content_base64: null, is_binary: false,
        } },
      },
    });

    await surface(container, ".git-shell");
    go("/history");
    await surface(container, ".history-view");
    go("/tasks");
    await surface(container, ".agent-tasks-view");
    go("/gui");
    await surface(container, ".gui-shell");
    go("/assistant");
    await waitFor(() =>
      expect(shown(container)?.textContent).toContain("Ask about your terminal sessions"),
    );
    go("/e/src%2Fapp.ts");
    await surface(container, ".editor-shell");

    expect(container.querySelector(".sessions-place")).toBeTruthy();
  });

  it("restores unsaved Git and new-task drafts after their views unmount", async () => {
    const { container, go } = mountShell("/g/vogt", {
      engine: {
        "GET /api/git/status": { body: {
          repo: "vogt", branch: "main", ahead: 0, behind: 0,
          entries: [], is_repo: true,
        } },
        "GET /api/git/branch": { body: { current: "main", all: ["main"] } },
        "GET /api/git/log": { body: [] },
        "GET /api/history/sessions": { body: [] },
        "GET /api/agent-tasks": { body: [] },
      },
    });

    const git = await surface(container, ".git-shell");
    const commit = git.querySelector<HTMLTextAreaElement>('textarea[placeholder="Commit message"]')!;
    commit.value = "keep this commit message";
    commit.dispatchEvent(new InputEvent("input", { bubbles: true }));
    go("/history");
    await surface(container, ".history-view");
    go("/g/vogt");
    await waitFor(() => expect(
      surfaceValue(container, 'textarea[placeholder="Commit message"]'),
    ).toBe("keep this commit message"));

    go("/tasks");
    const tasks = await surface(container, ".agent-tasks-view");
    const name = tasks.querySelector<HTMLInputElement>('.agent-task-field input[type="text"]')!;
    name.value = "keep this task draft";
    name.dispatchEvent(new InputEvent("input", { bubbles: true }));
    go("/history");
    await surface(container, ".history-view");
    go("/tasks");
    await waitFor(() => expect(
      surfaceValue(container, '.agent-task-field input[type="text"]'),
    ).toBe("keep this task draft"));
  });
});

function surfaceValue(container: HTMLElement, selector: string): string | undefined {
  return shown(container)?.querySelector<HTMLInputElement | HTMLTextAreaElement>(selector)?.value;
}
