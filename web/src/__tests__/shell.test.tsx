// The shell, and what a pasted link does to it (FR-U11, FR-T6).
//
// Every other file here mounts one surface at one URL, which settles what a
// surface does with the URL it is handed and says nothing about how it came
// to be handed one. `App.tsx`'s URL→tabs effect is the half in between — the
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
import { fireEvent, render, waitFor } from "@solidjs/testing-library";

import App from "../App";
import { APP_ROUTES } from "../routes";
import { setToken } from "../api";
import { replaceTabs } from "../tabs";
import {
  fakeVogt,
  settle,
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
}

/**
 * Mount the whole shell at one URL.
 *
 * The engine stubs are the shell's own boot, not the requirement: a token the
 * front door accepts (`/api/status`), the public config every gate reads, the
 * session list the drawer and the terminal links resolve against, and the
 * file tree in the drawer. Left unstubbed they answer 404 — which is a real
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
        assistant_enabled: false,
        vogt: { configured: true },
        ...options.config,
      },
    },
    "GET /api/sessions": { body: options.sessions ?? [] },
    "GET /api/tree": { body: [] },
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
 * The pane the reader is actually looking at.
 *
 * Every open tab stays mounted — that is how a terminal survives a tab switch
 * — and all but the active one are `display: none`. A test that queried the
 * whole container would pass on a surface the reader cannot see, which is the
 * exact failure "the link opened the wrong tab" produces.
 */
function shown(container: HTMLElement): HTMLElement | null {
  const place = container.querySelector<HTMLElement>(".place-view");
  if (place) return place;
  const panes = [...container.querySelectorAll<HTMLElement>(".tab-view > div")];
  return panes.find((pane) => pane.style.display === "flex") ?? null;
}

/** The surface on screen, waited for by the class it renders itself under. */
async function surface(container: HTMLElement, selector: string): Promise<HTMLElement> {
  return await waitFor(() => {
    const pane = shown(container);
    const found = pane?.querySelector<HTMLElement>(selector);
    expect(found, `no ${selector} is on screen`).toBeTruthy();
    return found!;
  });
}

/** The labels in the tab strip, in order. */
function tabLabels(container: HTMLElement): string[] {
  return [...container.querySelectorAll(".tab-strip .tab .label")].map(
    (node) => node.textContent ?? "",
  );
}

/** The drawer buttons, by their visible text. */
function drawerButtons(container: HTMLElement): string[] {
  return [...container.querySelectorAll(".drawer-actions button")].map(
    (node) => node.textContent ?? "",
  );
}

beforeEach(() => {
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
    expect(tabLabels(container)).toEqual(["alpha-build"]);
  });

  it("opens no phantom terminal for a session the server does not know", async () => {
    // A stale link is a link to a session that has gone, and the honest
    // answer is nothing — a tab named after six characters of an id, attached
    // to a PTY that does not exist, is a worse one.
    const { container } = mountShell("/t/eng-gone", { sessions: [SESSION] });

    await waitFor(() => expect(container.querySelector(".session-list")).toBeTruthy());
    await settle();
    expect(tabLabels(container)).toEqual([]);
    expect(container.querySelector(".terminal-host")).toBeNull();
  });

  it("opens nothing at the root, which names no surface", async () => {
    const { container } = mountShell("/");

    await waitFor(() => expect(container.querySelector(".drawer")).toBeTruthy());
    await settle();
    expect(tabLabels(container)).toEqual([]);
    expect(container.querySelector(".vogt-surface.board")).toBeTruthy();
  });

  it("follows a second link into a second surface, without closing the first", async () => {
    // The effect runs on every URL, not once at boot: a link followed from a
    // chat message while the app is already open is the common case, and a
    // boot-only effect would leave the reader staring at the previous tab.
    const { container, go } = mountShell("/board");
    await surface(container, ".vogt-surface.board");

    go("/backlog");

    await surface(container, ".vogt-surface.vogt-backlog");
    expect(tabLabels(container)).toEqual([]);
  });
});

describe("FR-T6 — the assistant is not there to be reached when it is not provisioned", () => {
  it("opens nothing for a hand-typed #/assistant when no key is configured", async () => {
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
      expect(drawerButtons(container)).toContain("Board"),
    );
    await settle();
    expect(drawerButtons(container)).not.toContain("Assistant");
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
    expect(drawerButtons(container)).toContain("Assistant");
    expect(tabLabels(container)).toEqual(["Assistant"]);
  });
});


describe("the drawer's width is the reader's, and persists", () => {
  it("offers a resizer that a keyboard can reach and use", async () => {
    // A panel resizable only by pointer is one a keyboard user cannot widen
    // when its contents do not fit — which is precisely the state this
    // control was added to escape.
    const { container } = mountShell("/board");
    await settle();
    const grip = container.querySelector<HTMLElement>(".drawer-resizer")!;
    expect(grip).toBeTruthy();
    expect(grip.getAttribute("aria-label")).toBeTruthy();

    fireEvent.keyDown(grip, { key: "ArrowRight" });
    await settle();
    expect(document.documentElement.style.getPropertyValue("--drawer-width")).toBe(
      "276px",
    );
    expect(localStorage.getItem("mydevenv2.drawerWidth")).toBe("276");
  });

  it("will not let the panel be dragged away to nothing", async () => {
    // The grip lives on the panel's own edge, so a drawer squeezed to zero
    // takes the handle with it and cannot be brought back.
    const { container } = mountShell("/board");
    await settle();
    const grip = container.querySelector<HTMLElement>(".drawer-resizer")!;
    for (let i = 0; i < 30; i += 1) {
      fireEvent.keyDown(grip, { key: "ArrowLeft", shiftKey: true });
    }
    await settle();
    expect(document.documentElement.style.getPropertyValue("--drawer-width")).toBe(
      "180px",
    );
  });

  it("restores the width it was left at", async () => {
    localStorage.setItem("mydevenv2.drawerWidth", "420");
    mountShell("/board");
    await settle();
    expect(document.documentElement.style.getPropertyValue("--drawer-width")).toBe(
      "420px",
    );
  });
});
