import { afterEach, describe, expect, it } from "vitest";
import type { RuntimeTransport } from "../runtimeTransport";
import type { SessionSummary } from "../api";

// WI-2692: the rail polls the engine's slim list (no command, economics or
// conversation) and keeps the detail it already knew for a row.

afterEach(() => localStorage.clear());

describe("rail slim view", () => {
  it("asks the engine for view=rail, with and without hidden rows", async () => {
    localStorage.setItem("vogt.token", "browser-test-token");
    const urls: string[] = [];
    const transport: RuntimeTransport = {
      request: async (input) => {
        urls.push(String(input instanceof Request ? input.url : input));
        return new Response(JSON.stringify({ sessions: [], hidden_count: 0 }), {
          status: 200,
          headers: { "content-type": "application/json" },
        });
      },
      openSocket: () => {
        throw new Error("not used");
      },
    };
    const runtime = await import("../runtimeTransport");
    runtime.installRuntimeTransport(transport);
    const { api } = await import("../api");
    await api.listSessions(undefined, false);
    await api.listSessions(undefined, true);
    const queries = urls.map((u) => new URL(u, "http://x").searchParams);
    expect(queries[0]?.get("view")).toBe("rail");
    expect(queries[1]?.get("view")).toBe("rail");
    expect(queries[1]?.get("include")).toBe("hidden");
  });

  it("keeps a row's known command, economics and conversation across a slim read", async () => {
    const { withKnownDetail } = await import("../store");
    const known = {
      id: "s1",
      name: "one",
      command: "claude --model opus",
      conversation: { agent: "claude", id: "c1" },
    } as unknown as SessionSummary;
    const slim = { id: "s1", name: "renamed" } as unknown as SessionSummary;
    const merged = withKnownDetail(slim, known);
    expect(merged.name).toBe("renamed");
    expect(merged.command).toBe("claude --model opus");
    expect(merged.conversation).toEqual({ agent: "claude", id: "c1" });
    expect(withKnownDetail(slim, undefined)).toBe(slim);
  });
});
