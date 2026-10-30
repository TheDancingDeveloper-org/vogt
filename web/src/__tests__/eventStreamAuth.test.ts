// WI-2708: a 401 from the event stream is not a sign-out by itself.
//
// subscribeEvents joins the shared /api/auth/check. The shell hears a
// rejection only when that check also returns 401, and then as a confirmed
// fact so it does not probe a second time.

import { afterEach, describe, expect, it } from "vitest";

import { setToken, subscribeAuthRejected, subscribeEvents } from "../api";
import { resetAuthRecheck } from "../authRecheck";
import { installRuntimeTransport, type RuntimeTransport } from "../runtimeTransport";

afterEach(() => {
  resetAuthRecheck();
  setToken("");
});

function transport(checkStatus: number): { transport: RuntimeTransport; checks: () => number } {
  let checks = 0;
  return {
    checks: () => checks,
    transport: {
      request: async (input) => {
        const url = String(input instanceof Request ? input.url : input);
        if (url.includes("/api/events")) return new Response("refused", { status: 401 });
        if (url.includes("/api/auth/check")) {
          checks += 1;
          return new Response(checkStatus === 401 ? "no" : "ok", { status: checkStatus });
        }
        throw new Error(`unexpected ${url}`);
      },
      openSocket: () => {
        throw new Error("not used");
      },
    },
  };
}

async function stream401(checkStatus: number) {
  const script = transport(checkStatus);
  installRuntimeTransport(script.transport);
  const seen: Array<{ status: number; confirmed?: boolean }> = [];
  const stop = subscribeAuthRejected((rejection) => {
    seen.push({ status: rejection.status, confirmed: rejection.confirmed });
  });
  const unsubscribe = subscribeEvents(() => {}, () => {});
  // The stream handler imports the re-check, then the probe awaits the
  // transport. Yield until that probe has been counted.
  for (let i = 0; i < 20 && script.checks() === 0; i++) {
    await new Promise((resolve) => setTimeout(resolve, 0));
  }
  // One more turn for the confirmed publish to reach the listener.
  await new Promise((resolve) => setTimeout(resolve, 0));
  unsubscribe();
  stop();
  return { seen, checks: script.checks() };
}

describe("event stream 401", () => {
  it("stays signed in when the re-check returns 200", async () => {
    setToken("live-token");
    const { seen, checks } = await stream401(200);
    expect(checks).toBe(1);
    expect(seen).toEqual([]);
  });

  it("ends the session when the re-check also returns 401", async () => {
    setToken("stale-token");
    const { seen, checks } = await stream401(401);
    expect(checks).toBe(1);
    expect(seen).toEqual([{ status: 401, confirmed: true }]);
  });
});
