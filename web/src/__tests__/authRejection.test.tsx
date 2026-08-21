// A credential that stops being valid while the app is running (#195).
//
// The boot probe always handled a 401 correctly, which is what made the live
// failure so confusing: the shell resolved auth once at mount and nothing ever
// re-examined it, so a token rotated underneath a running tab left every panel
// holding its own error, the stored token alive in `localStorage`, and no path
// back to the login screen short of a reload the reader had no reason to try.
//
// So these tests are about the *transition*, and about the three things that
// must not trigger it. A rejected credential is a session-level fact; a
// missing capability and an absent engine are not, and collapsing either into
// "signed out" is the FR-O4 conflation of "offline" with "unauthorized". The
// distinction is made in one place — `api.ts` publishes 401 and nothing else —
// so it is asserted there first and then through the shell that consumes it.

import { afterEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter, Route, createMemoryHistory } from "@solidjs/router";
import { render, waitFor } from "@solidjs/testing-library";

import App from "../App";
import { APP_ROUTES } from "../routes";
import {
  api,
  getBase,
  getToken,
  setBase,
  setToken,
  subscribeAuthRejected,
  validateCredentials,
  type AuthRejection,
} from "../api";
import { fakeVogt, refusal, stopLiveStream, type FakeVogt, type Routes } from "./harness";

afterEach(() => {
  // `App` starts the stream on sign-in; an unmounted shell must not leave a
  // reconnect timer reading on behalf of the next test.
  stopLiveStream();
});

// -- what ends a session, in the one place that decides ---------------------

/** Answer every request with one status, the way a re-keyed engine does. */
function engineAnswers(status: number, body = "no"): void {
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => new Response(body, { status })),
  );
}

/** Collect what `api.ts` publishes for the duration of one test. */
function rejections(): { seen: AuthRejection[]; stop: () => void } {
  const seen: AuthRejection[] = [];
  const stop = subscribeAuthRejected((rejection) => seen.push(rejection));
  return { seen, stop };
}

describe("api.ts — a refusal that is about the credential, and the ones that are not", () => {
  it("publishes the 401 an authenticated read met", async () => {
    setToken("stale-token");
    engineAnswers(401, "the presented token is not valid");
    const { seen, stop } = rejections();

    await expect(api.operationalStatus()).rejects.toThrow();
    stop();

    expect(seen).toHaveLength(1);
    expect(seen[0]?.status).toBe(401);
    // The server's own words travel with it; the shell renders its own copy,
    // but a caller that wants the reason has it.
    expect(seen[0]?.detail).toContain("not valid");
  });

  it("says nothing about a 403, because the credential is fine", async () => {
    setToken("good-token");
    engineAnswers(403, "actor lacks capability status.read");
    const { seen, stop } = rejections();

    await expect(api.operationalStatus()).rejects.toThrow();
    stop();

    expect(seen).toEqual([]);
  });

  it("says nothing about a 502, because an absent engine is not a refusal", async () => {
    setToken("good-token");
    engineAnswers(502, "upstream did not answer");
    const { seen, stop } = rejections();

    await expect(api.operationalStatus()).rejects.toThrow();
    stop();

    expect(seen).toEqual([]);
  });

  it("says nothing when the request never arrived at all", async () => {
    setToken("good-token");
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => {
        throw new TypeError("Failed to fetch");
      }),
    );
    const { seen, stop } = rejections();

    await expect(api.operationalStatus()).rejects.toThrow();
    stop();

    expect(seen).toEqual([]);
  });

  it("says nothing when a *candidate* credential is refused", async () => {
    // Settings and the login form both ask about a token the reader has just
    // typed. A 401 there is that form's answer — signing the reader out of a
    // working session for mistyping a token into Settings would be a bug of
    // exactly the shape this file is about.
    setToken("good-token");
    engineAnswers(401, "the presented token is not valid");
    const { seen, stop } = rejections();

    await expect(validateCredentials("typo", "")).rejects.toThrow();
    stop();

    expect(seen).toEqual([]);
    expect(getToken()).toBe("good-token");
  });
});

// -- the shell that consumes it ---------------------------------------------

interface Shell {
  container: HTMLElement;
  vogt: FakeVogt;
  go(url: string): void;
}

/**
 * Mount the whole shell, signed in, the way `shell.test.tsx` does.
 *
 * The engine stubs are the shell's own boot and not the requirement: a token
 * the front door accepts (`/api/status`), the public config every gate reads,
 * the session list and the file tree the rail draws. The requirement is what
 * happens to that shell *after* it has booted.
 */
function mountShell(url: string, vogtRoutes: Routes = {}): Shell {
  setToken("live-token");
  setBase("http://engine.test");
  const vogt = fakeVogt(vogtRoutes, {
    "GET /api/status": { body: { ok: true } },
    "GET /api/config": {
      body: {
        version: "test",
        gui_stream_url: null,
        gui_stream_available: false,
        assistant_enabled: false,
        vogt: { configured: true },
      },
    },
    "GET /api/sessions": { body: [] },
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

/** The login gate, by the heading only it draws. */
function loginShown(container: HTMLElement): boolean {
  return container.querySelector(".login-screen") !== null;
}

async function signedIn(shell: Shell): Promise<void> {
  await waitFor(() => expect(loginShown(shell.container)).toBe(false));
  await waitFor(() => expect(shell.vogt.calls.length).toBeGreaterThan(0));
}

describe("#195 — a token refused after boot returns the reader to the login screen", () => {
  it("signs out on the next authenticated read, with no reload and with the 401 copy", async () => {
    const shell = mountShell("/board");
    await signedIn(shell);

    // The rotation happens here: the reader is on a working board, and the
    // *next* thing they open is refused.
    shell.vogt.route(
      "GET /backlog",
      refusal(401, "the presented token is not valid"),
    );
    shell.go("/backlog");

    await waitFor(() => expect(loginShown(shell.container)).toBe(true));
    expect(shell.container.querySelector(".login-error")?.textContent).toContain(
      "That token was rejected (401)",
    );
  });

  it("forgets the rejected credential, so a reload does not re-present it", async () => {
    const shell = mountShell("/board");
    await signedIn(shell);

    shell.vogt.route("GET /backlog", refusal(401, "the presented token is not valid"));
    shell.go("/backlog");

    await waitFor(() => expect(loginShown(shell.container)).toBe(true));
    expect(getToken()).toBe("");
    expect(getBase()).toBe("");
  });

  it("keeps the reader signed in when the capability is what is missing", async () => {
    // 403: the credential is good and the answer is about what it may do.
    // Signing out here would hide the one sentence that says what to fix.
    const shell = mountShell("/board");
    await signedIn(shell);

    shell.vogt.route(
      "GET /backlog",
      refusal(403, "actor lacks capability backlog.read"),
    );
    shell.go("/backlog");

    await waitFor(() =>
      expect(
        shell.container.querySelector(".vogt-backlog-outage")?.textContent,
      ).toContain("actor lacks capability backlog.read"),
    );
    expect(loginShown(shell.container)).toBe(false);
    expect(getToken()).toBe("live-token");
  });

  it("keeps the reader signed in when it is the engine that is away", async () => {
    // FR-O4: "offline" and "unauthorized" are different states. A 502 says
    // nothing whatever about the token, and a reader sent to a login screen
    // by an outage would type a perfectly good token into it and be refused
    // again for the same reason.
    const shell = mountShell("/board");
    await signedIn(shell);

    shell.vogt.route("GET /backlog", refusal(502, "vogt-core did not answer"));
    shell.go("/backlog");

    await waitFor(() =>
      expect(
        shell.container.querySelector(".vogt-backlog-outage")?.textContent,
      ).toContain("vogt-core did not answer"),
    );
    expect(loginShown(shell.container)).toBe(false);
    expect(getToken()).toBe("live-token");
  });

  it("signs out when another tab's read was the one refused", async () => {
    // The carrier is the auth channel the stored-credential machinery already
    // uses, so a rotation noticed anywhere is noticed everywhere — this is
    // what a second tab's 401 looks like arriving here.
    const shell = mountShell("/board");
    await signedIn(shell);

    const otherTab = new BroadcastChannel("mydevenv2.auth");
    otherTab.postMessage({
      type: "auth-rejected",
      source: "another-tab",
      status: 401,
      detail: "the presented token is not valid",
    });

    await waitFor(() => expect(loginShown(shell.container)).toBe(true));
    otherTab.close();
    expect(getToken()).toBe("");
  });

  it("offers a sign-out control that clears both token and base", async () => {
    // The wrong-but-not-401 case the issue names: a token pointed at the wrong
    // base fails every read without ever being refused, and before this there
    // was nothing on screen to press.
    const shell = mountShell("/board");
    await signedIn(shell);

    const control = [...shell.container.querySelectorAll("button")].find(
      (button) => button.textContent?.trim() === "Sign out",
    );
    expect(control).toBeDefined();
    control?.click();

    await waitFor(() => expect(loginShown(shell.container)).toBe(true));
    expect(getToken()).toBe("");
    expect(getBase()).toBe("");
  });
});
