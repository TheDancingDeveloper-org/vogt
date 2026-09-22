// The password login: the gate a person meets, and the client under it.
//
// The fake sits at `fetch`, so these tests exercise the real path
// (`/api/auth/login`), the real request body, and the real handling of the
// core's refusal shapes — a wrong pair, a throttled username, an absent core
// — rather than asserting a mock agreed with itself. The gate is rendered
// through the shell, so what is asserted is what a reader sees.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MemoryRouter, Route, createMemoryHistory } from "@solidjs/router";
import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";

import App from "../App";
import { APP_ROUTES } from "../routes";
import { ApiError, getToken } from "../api";
import { loginWithPassword } from "../authApi";
import { stopLiveStream } from "./harness";

type FetchArgs = [input: RequestInfo | URL, init?: RequestInit];
let fetchMock: ReturnType<typeof vi.fn<(...args: FetchArgs) => Promise<Response>>>;

const jsonResponse = (status: number, body: unknown): Response =>
  new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });

const session = {
  actor: { id: "act-1", identity_ref: "human:ada", display_name: "Ada", kind: "human" },
  token: { id: "tok-1", name: "browser session", scopes: ["read", "work.write"], kind: "session" },
  secret: "vogt_a-session-secret",
};

beforeEach(() => {
  localStorage.clear();
  fetchMock = vi.fn<(...args: FetchArgs) => Promise<Response>>();
  vi.stubGlobal("fetch", fetchMock);
});

afterEach(() => {
  stopLiveStream();
  vi.unstubAllGlobals();
  localStorage.clear();
});

describe("authApi — the login client", () => {
  it("posts the pair to the open login route and returns the session", async () => {
    fetchMock.mockResolvedValue(jsonResponse(200, session));
    const result = await loginWithPassword(" Ada ", "correct horse battery", "http://engine.test/");
    expect(result.secret).toBe("vogt_a-session-secret");
    const [url, init] = fetchMock.mock.calls[0]!;
    expect(String(url)).toBe("http://engine.test/api/auth/login");
    expect(init?.method).toBe("POST");
    expect(JSON.parse(String(init?.body))).toEqual({
      username: "Ada",
      password: "correct horse battery",
      session_name: "browser session",
    });
    // No credential travels with a login: there is none yet.
    expect(new Headers(init?.headers).get("Authorization")).toBeNull();
  });

  it("surfaces the core's refusals by status, in its own words", async () => {
    fetchMock.mockResolvedValue(
      jsonResponse(401, {
        error: { code: "unauthenticated", message: "the username or password is not right" },
      }),
    );
    await expect(loginWithPassword("ada", "no", "")).rejects.toMatchObject({
      status: 401,
      message: expect.stringContaining("the username or password is not right"),
    });
    fetchMock.mockResolvedValue(
      jsonResponse(429, { error: { code: "login_throttled", message: "too many failed logins" } }),
    );
    const throttled = await loginWithPassword("ada", "no", "").catch((e: unknown) => e);
    expect(throttled).toBeInstanceOf(ApiError);
    expect((throttled as ApiError).status).toBe(429);
  });
});

/** Route a fetch by path: the shell asks several things at boot. */
function engineAnswering(login: () => Response): void {
  fetchMock.mockImplementation(async (input, init) => {
    const url = new URL(typeof input === "string" ? input : String(input), "http://vogt.test");
    const method = (init?.method ?? "GET").toUpperCase();
    if (url.pathname === "/api/install/status") return jsonResponse(200, { install_mode: false });
    if (url.pathname === "/api/config") return jsonResponse(200, {});
    if (url.pathname === "/api/auth/login" && method === "POST") return login();
    if (url.pathname === "/api/auth/check") {
      return jsonResponse(200, {
        ok: true,
        version: "test",
        storage: { state_dir: "/tmp", workspace_root: "/w" },
      });
    }
    if (url.pathname === "/api/sessions") return jsonResponse(200, []);
    return new Response("not here", { status: 404 });
  });
}

function mountGate() {
  const history = createMemoryHistory();
  history.set({ value: "/sessions" });
  return render(() => (
    <MemoryRouter history={history}>
      <Route path={[...APP_ROUTES]} component={App} />
    </MemoryRouter>
  ));
}

describe("the gate — a person signs in with a password", () => {
  it("leads with a username and password, and enters the shell on the minted session", async () => {
    engineAnswering(() => jsonResponse(200, session));
    const { container } = mountGate();
    await screen.findByRole("heading", { name: "Sign in to Vogt" });
    await fireEvent.input(screen.getByLabelText("Username"), { target: { value: "ada" } });
    await fireEvent.input(screen.getByLabelText("Password"), {
      target: { value: "correct horse battery" },
    });
    await fireEvent.click(screen.getByRole("button", { name: "Sign in" }));
    await waitFor(() => expect(getToken()).toBe("vogt_a-session-secret"));
    await waitFor(() =>
      expect(container.querySelector(".login-screen")).toBeNull(),
    );
    const login = fetchMock.mock.calls.find(([, init]) => init?.method === "POST");
    expect(JSON.parse(String(login?.[1]?.body))).toMatchObject({
      username: "ada",
      password: "correct horse battery",
    });
  });

  it("tells a wrong pair apart from a throttled username and an absent core", async () => {
    let answer = () =>
      jsonResponse(401, {
        error: { code: "unauthenticated", message: "the username or password is not right" },
      });
    engineAnswering(() => answer());
    mountGate();
    await screen.findByRole("heading", { name: "Sign in to Vogt" });
    await fireEvent.input(screen.getByLabelText("Username"), { target: { value: "ada" } });
    await fireEvent.input(screen.getByLabelText("Password"), { target: { value: "wrong" } });

    await fireEvent.click(screen.getByRole("button", { name: "Sign in" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("username or password");
    expect(getToken()).toBe("");

    answer = () =>
      jsonResponse(429, { error: { code: "login_throttled", message: "too many failed logins" } });
    await fireEvent.click(screen.getByRole("button", { name: "Sign in" }));
    await waitFor(() =>
      expect(screen.getByRole("alert")).toHaveTextContent("Wait a minute"),
    );

    answer = () => jsonResponse(503, { error: "vogt-core is unavailable" });
    await fireEvent.click(screen.getByRole("button", { name: "Sign in" }));
    await waitFor(() =>
      expect(screen.getByRole("alert")).toHaveTextContent("core is unavailable"),
    );
  });

  it("keeps the token path one disclosure away", async () => {
    engineAnswering(() => jsonResponse(401, { error: "no" }));
    mountGate();
    await screen.findByRole("heading", { name: "Sign in to Vogt" });
    expect(screen.queryByLabelText("Bearer token")).toBeNull();
    await fireEvent.click(screen.getByRole("button", { name: "Sign in with a token" }));
    expect(screen.getByLabelText("Bearer token")).toBeInTheDocument();
    expect(screen.queryByLabelText("Username")).toBeNull();
    await fireEvent.click(screen.getByRole("button", { name: "Sign in with a password" }));
    expect(screen.getByLabelText("Username")).toBeInTheDocument();
  });
});
