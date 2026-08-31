import { beforeEach, describe, expect, it } from "vitest";
import { DemoSocket } from "../demo/socket";
import { DemoStore } from "../demo/store";
import { ROUTES } from "../vogtApi";

beforeEach(() => {
  sessionStorage.clear();
});

function body(reason = "exercise a browser-only demo write") {
  return { reason, ref: "WI-101", id: "demo-version", entry_key: "ci:orbit-main", resolution: "rejected", to_state: "in_progress", name: "Demo project", subject: "demo:subject" };
}

const WRITES = new Set([
  "project.import", "project.register", "forge.account_link", "sweep",
  "work.create", "work.transition", "work.comment", "work.update",
  "drift.resolve", "inbox.archive", "inbox.snooze", "inbox.restore",
  "suppress", "work.adopt", "session.start", "session.stop",
]);

describe("public demo contracts", () => {
  it("has a deliberate response for every Vogt operation exposed by the PWA", async () => {
    const store = new DemoStore();
    for (const [operation, path] of Object.entries(ROUTES)) {
      const write = WRITES.has(operation);
      const query = new URLSearchParams({ ref: "WI-101", slug: "orbit", project: "orbit" });
      const response = await store.request(`/api/vogt${path}`, write ? "POST" : "GET", query, write ? { body: JSON.stringify(body()) } : undefined);
      const text = await response.text();
      expect(text, `${operation} has no demo responder`).not.toContain("demo.unhandled");
    }
  });

  it("keeps writes in sessionStorage and appends reconciling audit evidence", async () => {
    const store = new DemoStore();
    const transition = await store.request("/api/vogt/work/transition", "POST", new URLSearchParams(), { body: JSON.stringify(body()) });
    expect(transition.ok).toBe(true);
    const detail = await (await store.request("/api/vogt/work/get", "GET", new URLSearchParams({ ref: "WI-101" }))).json();
    expect(detail.item.state).toBe("in_progress");
    const audit = await (await store.request("/api/vogt/audit", "GET", new URLSearchParams())).json();
    expect(audit.records[0]).toMatchObject({ operation: "work.transition", reason: "exercise a browser-only demo write" });
    expect(JSON.parse(sessionStorage.getItem("vogt.demo.state.v1") ?? "{}").work[0].state).toBe("in_progress");
  });

  it("serves the same release provenance as the demo manifest", async () => {
    const store = new DemoStore({
      product_version: "0.2.2",
      source_ref: "v0.2.2",
      source_sha: "a".repeat(40),
    });
    const status = await (await store.request("/api/status", "GET", new URLSearchParams())).json();
    const config = await (await store.request("/api/config", "GET", new URLSearchParams())).json();
    expect(status).toMatchObject({ product_version: "0.2.2", source_ref: "v0.2.2", source_sha: "a".repeat(40) });
    expect(config.release_url).toBe("https://github.com/TheDancingDeveloper-org/vogt/releases/tag/v0.2.2");
  });

  it("refuses effects that would cross the browser boundary", async () => {
    const store = new DemoStore();
    const cases = [
      ["/api/vogt/projects/import", "POST"],
      ["/api/vogt/forge/accounts", "POST"],
      ["/api/gui/launch", "POST"],
      ["/api/push/subscribe", "POST"],
      ["/api/assistant/stt", "POST"],
    ] as const;
    for (const [path, method] of cases) {
      const response = await store.request(path, method, new URLSearchParams(), { body: JSON.stringify(body()) });
      expect(response.status, path).toBeGreaterThanOrEqual(400);
      expect(await response.text(), path).toMatch(/demo|public|speech|push/i);
    }
  });

  it("replays terminal snapshots before live binary output and executes nothing", async () => {
    const store = new DemoStore();
    const socket = new DemoSocket(store, "demo-build");
    const frames: unknown[] = [];
    socket.addEventListener("message", (event) => frames.push(event.data));
    await new Promise((resolve) => setTimeout(resolve, 10));
    expect(JSON.parse(String(frames[0]))).toMatchObject({ type: "snapshot-start", reset: true });
    expect(ArrayBuffer.isView(frames[1]) || Object.prototype.toString.call(frames[1]) === "[object ArrayBuffer]").toBe(true);
    expect(JSON.parse(String(frames[2]))).toEqual({ type: "snapshot-done" });
    socket.send(new TextEncoder().encode("uname -a\r"));
    await new Promise((resolve) => setTimeout(resolve, 1));
    expect(new TextDecoder().decode(frames.at(-1) as ArrayBuffer)).toContain("is not executed");
    socket.close();
  });

  it("mirrors the mobile Assistant transcript display metadata", async () => {
    const store = new DemoStore();
    const history = await (
      await store.request("/api/assistant/history", "GET", new URLSearchParams())
    ).json();
    expect(history.transcript[1]).toMatchObject({
      created_at: "2026-08-24T14:49:25Z",
      session_refs: [
        { id: "demo-agent", name: "Agent review", activity: "waiting-for-input" },
      ],
      actions: [
        { kind: "open-session", session_id: "demo-agent", label: "Open Agent review" },
      ],
    });
  });

  it("reset restores the canonical split presets and deterministic state", async () => {
    const store = new DemoStore();
    await store.request("/api/vogt/work/transition", "POST", new URLSearchParams(), { body: JSON.stringify(body()) });
    store.reset();
    const saved = JSON.parse(localStorage.getItem("vogt.terminalLayouts.v1") ?? "{}");
    expect(saved["term:demo-build"].root.children).toHaveLength(2);
    expect(saved["term:demo-agent"].root.children[1].children).toHaveLength(2);
    expect(saved["term:demo-logs"].root.children).toHaveLength(3);
    const detail = await (await store.request("/api/vogt/work/get", "GET", new URLSearchParams({ ref: "WI-101" }))).json();
    expect(detail.item.state).toBe("open");
  });
});
