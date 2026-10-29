// WI-2708: one 401 is a re-check, not a sign-out.
//
// A transient 401 (the re-check then returns 200, or the probe never
// arrives) keeps the session. A re-check that also returns 401 confirms it.
// A 401 that came only from the event stream still has to be confirmed by
// that same probe — the stream's own status is never enough.

import { afterEach, describe, expect, it, vi } from "vitest";

import { setToken } from "../api";
import {
  recheckAuth,
  resetAuthRecheck,
  resolveAuthRejection,
} from "../authRecheck";

afterEach(() => {
  resetAuthRecheck();
  setToken("");
  vi.useRealTimers();
});

function probe(status: number | "throw"): (signal: AbortSignal) => Promise<Response> {
  return async () => {
    if (status === "throw") throw new TypeError("Failed to fetch");
    return new Response(status === 401 ? "no" : "ok", { status });
  };
}

describe("auth re-check", () => {
  it("keeps the session when a 401 is followed by a successful check", async () => {
    setToken("live-token");
    const decision = await resolveAuthRejection(
      { status: 401, detail: "edge" },
      {},
      { probe: probe(200) },
    );
    expect(decision).toBeNull();
  });

  it("signs out only when the re-check also returns 401", async () => {
    setToken("stale-token");
    const decision = await resolveAuthRejection(
      { status: 401, detail: "the presented token is not valid" },
      {},
      { probe: probe(401) },
    );
    expect(decision).toEqual({
      status: 401,
      detail: "the presented token is not valid",
    });
  });

  it("keeps the session when the re-check cannot reach the engine", async () => {
    setToken("live-token");
    const decision = await resolveAuthRejection(
      { status: 401, detail: "edge" },
      {},
      { probe: probe("throw") },
    );
    expect(decision).toBeNull();
  });

  it("keeps the session on a 5xx from the re-check", async () => {
    setToken("live-token");
    const decision = await resolveAuthRejection(
      { status: 401, detail: "edge" },
      {},
      { probe: probe(503) },
    );
    expect(decision).toBeNull();
  });

  it("does not sign out on an SSE 401 unless the re-check confirms it", async () => {
    setToken("live-token");
    const transient = await resolveAuthRejection(
      { status: 401, detail: "the event stream was refused" },
      { fromStream: true },
      { probe: probe(200) },
    );
    expect(transient).toBeNull();

    resetAuthRecheck();
    const confirmed = await resolveAuthRejection(
      { status: 401, detail: "the event stream was refused" },
      { fromStream: true },
      { probe: probe(401) },
    );
    expect(confirmed?.status).toBe(401);
  });

  it("does not treat a deliberate sign-out on the stream path as a refusal to re-check", async () => {
    setToken("live-token");
    const decision = await resolveAuthRejection(
      { status: 0, detail: "signed out" },
      { fromStream: true },
      { probe: probe(401) },
    );
    expect(decision).toBeNull();
  });

  it("shares one in-flight probe across callers", async () => {
    setToken("live-token");
    let calls = 0;
    const slow: (signal: AbortSignal) => Promise<Response> = () => {
      calls += 1;
      return new Promise((resolve) => {
        setTimeout(() => resolve(new Response("ok", { status: 200 })), 10);
      });
    };
    const [a, b] = await Promise.all([
      recheckAuth({ probe: slow }),
      recheckAuth({ probe: slow }),
    ]);
    expect(a).toBe("kept");
    expect(b).toBe("kept");
    expect(calls).toBe(1);
  });

  it("passes a rejection that is already confirmed without probing again", async () => {
    setToken("stale-token");
    let calls = 0;
    const decision = await resolveAuthRejection(
      { status: 401, detail: "the event stream was refused", confirmed: true },
      { confirmed: true },
      {
        probe: async () => {
          calls += 1;
          return new Response("no", { status: 401 });
        },
      },
    );
    expect(decision?.status).toBe(401);
    expect(calls).toBe(0);
  });
});
