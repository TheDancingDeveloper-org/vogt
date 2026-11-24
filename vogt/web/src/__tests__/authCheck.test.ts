import { afterEach, describe, expect, it, vi } from "vitest";
import { setBase, setToken, validateCredentials } from "../api";

afterEach(() => {
  setBase("");
  setToken("");
  vi.unstubAllGlobals();
});

describe("cheap authenticated startup check", () => {
  it("does not read operational status when auth check succeeds", async () => {
    const requests: string[] = [];
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      requests.push(url);
      return new Response(JSON.stringify({
        ok: true,
        version: "test",
        storage: { state_dir: "/state", workspace_root: "/workspace" },
      }));
    }));

    const result = await validateCredentials("candidate", "https://engine.test");

    expect(result.ok).toBe(true);
    expect(requests).toEqual(["https://engine.test/api/auth/check"]);
  });

  it("falls back to status once for an older engine", async () => {
    const requests: string[] = [];
    vi.stubGlobal("fetch", vi.fn(async (input: RequestInfo | URL) => {
      const url = String(input);
      requests.push(url);
      if (url.endsWith("/api/auth/check")) return new Response("missing", { status: 404 });
      return new Response(JSON.stringify({
        version: "legacy",
        storage: { state_dir: "/state", workspace_root: "/workspace" },
      }));
    }));

    const result = await validateCredentials("candidate", "https://engine.test");

    expect(result.version).toBe("legacy");
    expect(requests).toEqual([
      "https://engine.test/api/auth/check",
      "https://engine.test/api/status",
    ]);
  });
});
