import { beforeEach, describe, expect, it, vi } from "vitest";
import { createRoot, createEffect } from "solid-js";
import type * as Store from "../store";

// A poll that answers what the rail already shows must not hand every row a
// new object: on prod each read re-rendered the whole rail (WI-1903).
const mocks = vi.hoisted(() => ({ listSessions: vi.fn() }));

vi.mock("../api", () => ({
  api: { listSessions: (...args: unknown[]) => mocks.listSessions(...args) },
  subscribeEvents: () => () => {},
}));

const row = (id: string, name: string) => ({
  id,
  name,
  exit_code: null,
  activity: "idle",
  cwd: "/w",
});

let store: typeof Store;

beforeEach(async () => {
  vi.resetModules();
  store = await import("../store");
  mocks.listSessions.mockReset();
});

describe("session list refresh", () => {
  it("keeps unchanged rows and the order as they were", async () => {
    mocks.listSessions.mockResolvedValue({ sessions: [row("a", "A"), row("b", "B")], hidden_count: 0 });
    await store.refreshSessions();
    const a = store.sessionsStore.sessions.a;
    const order = store.sessionsStore.order;

    let runs = 0;
    const dispose = createRoot((dispose) => {
      createEffect(() => {
        void store.sessionsStore.sessions.a?.name;
        void store.sessionsStore.order.length;
        runs += 1;
      });
      return dispose;
    });
    expect(runs).toBe(1);

    mocks.listSessions.mockResolvedValue({ sessions: [row("a", "A"), row("b", "B")], hidden_count: 0 });
    await store.refreshSessions();
    expect(store.sessionsStore.sessions.a).toBe(a);
    expect(store.sessionsStore.order).toBe(order);
    expect(runs).toBe(1);
    dispose();
  });

  it("applies a changed row, a new row and a removed row", async () => {
    mocks.listSessions.mockResolvedValue({ sessions: [row("a", "A"), row("b", "B")], hidden_count: 0 });
    await store.refreshSessions();
    const b = store.sessionsStore.sessions.b;

    mocks.listSessions.mockResolvedValue({ sessions: [row("b", "B2"), row("c", "C")], hidden_count: 1 });
    await store.refreshSessions();
    expect(store.sessionsStore.order).toEqual(["b", "c"]);
    expect(store.sessionsStore.sessions.a).toBeUndefined();
    expect(store.sessionsStore.sessions.b).toBe(b);
    expect(store.sessionsStore.sessions.b?.name).toBe("B2");
    expect(store.sessionsStore.sessions.c?.name).toBe("C");
    expect(store.sessionsStore.hiddenCount).toBe(1);
  });
});
