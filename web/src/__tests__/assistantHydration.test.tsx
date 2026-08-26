// Request-count coverage for the assistant's lazy, shared hydration (#416).
import { render } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";

import Assistant from "../Assistant";
import { invalidateAssistantSnapshot, readAssistantSnapshot } from "../assistantCache";
import Sessions from "../Sessions";
import * as runtimeTransport from "../runtimeTransport";
import { fakeVogt, mountAt, settle } from "./harness";

const config = {
  gui_stream_url: null,
  version: "test",
  assistant_enabled: true,
  assistant_profiles: [],
};

const history = {
  transcript: [{ role: "assistant" as const, text: "ready" }],
  pending_action: null,
};

function assistantCalls(engine: ReturnType<typeof fakeVogt>): string[] {
  return engine.engineCalls
    .filter((call) => call.path.startsWith("/api/assistant/"))
    .map((call) => `${call.method} ${call.path}`);
}

afterEach(() => {
  invalidateAssistantSnapshot();
  vi.restoreAllMocks();
});

describe("assistant hydration request gates", () => {
  it("does not read assistant history when disabled", async () => {
    const engine = fakeVogt({}, {
      "GET /api/assistant/history": { body: history },
    });
    const mounted = mountAt("/sessions", "/sessions", () => (
      <Sessions assistantEnabled={false} />
    ));

    await settle();
    expect(assistantCalls(engine)).toEqual([]);
    mounted.unmount();
  });

  it("does not read assistant history while the panel is unopened", async () => {
    const engine = fakeVogt({}, {
      "GET /api/assistant/history": { body: history },
    });
    const mounted = mountAt("/sessions", "/sessions", () => (
      <Sessions assistantEnabled />
    ));

    await settle();
    expect(assistantCalls(engine)).toEqual([]);
    mounted.unmount();
  });

  it("reads the demo pending action without an approval deep-link", async () => {
    vi.spyOn(runtimeTransport, "isDemoMode").mockReturnValue(true);
    const engine = fakeVogt({}, {
      "GET /api/assistant/history": { body: history },
    });
    const mounted = mountAt("/t/demo-agent", "/t/demo-agent", () => (
      <Sessions assistantEnabled />
    ));

    await settle();
    expect(assistantCalls(engine)).toEqual(["GET /api/assistant/history"]);
    mounted.unmount();
  });

  it("shares first-open hydration across repeated Assistant instances", async () => {
    const engine = fakeVogt({}, {
      "GET /api/assistant/history": { body: history },
    });
    const first = render(() => (
      <Assistant
        assistantEnabled
        publicConfig={config}
        onError={() => undefined}
      />
    ));
    await settle();

    const second = render(() => (
      <Assistant
        assistantEnabled
        publicConfig={config}
        onError={() => undefined}
      />
    ));
    await settle();

    expect(assistantCalls(engine)).toEqual(["GET /api/assistant/history"]);
    expect(engine.calls.filter((call) => call.path === "/projects")).toHaveLength(1);
    first.unmount();
    second.unmount();
  });

  it("refetches history after pending-action invalidation", async () => {
    let reads = 0;
    const engine = fakeVogt({}, {
      "GET /api/assistant/history": () => ({
        body: {
          transcript: [],
          pending_action: reads++ === 0 ? null : {
            kind: "send_input",
            id: "act-2",
            session_id: "session-2",
            session_name: "worker",
            text: "continue\r",
            submit: true,
          },
        },
      }),
    });

    await readAssistantSnapshot(true);
    invalidateAssistantSnapshot();
    const refreshed = await readAssistantSnapshot(true);

    expect(assistantCalls(engine)).toEqual([
      "GET /api/assistant/history",
      "GET /api/assistant/history",
    ]);
    expect(refreshed?.pendingAction?.id).toBe("act-2");
  });
});
