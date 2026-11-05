// WI-3129: every visited session used to leave a mounted xterm and a WebGL
// context behind. Chrome caps ~16 contexts per page and then drops the oldest,
// and each hidden terminal kept parsing live output. Terminal tabs are now
// bounded to the active one plus the most recent few (MAX_LIVE_TERMINAL_TABS);
// older ones dispose their xterm and WebGL addon and restore from the
// terminal cache on revisit.

import { expect, test, type Page } from "@playwright/test";

const isLive = (project: string) => project === "live";

// Keep in step with MAX_LIVE_TERMINAL_TABS in src/tabLifecycle.ts.
const CAP = 3;
const SESSION_COUNT = 8;

const SESSIONS = Array.from({ length: SESSION_COUNT }, (_, i) => ({
  id: `sess-cap-${i}`,
  name: `cap ${i}`,
  cwd: "/workspace",
  activity: "idle",
  exit_code: null,
  scrollback_bytes: 0,
  created_at: "2026-09-20T00:00:00Z",
}));

async function mockedFixtures(page: Page, renderer: "dom" | "webgl"): Promise<void> {
  await page.addInitScript((preference) => {
    localStorage.setItem("vogt.token", "browser-test-token");
    localStorage.setItem("vogt.appTheme.v1", "dark");
    localStorage.setItem("vogt.terminalRenderer.v1", preference);
    // Count every WebGL context loss. The event does not bubble, so listen
    // in the capture phase.
    const w = window as unknown as { __contextLost: number };
    w.__contextLost = 0;
    document.addEventListener(
      "webglcontextlost",
      () => {
        w.__contextLost += 1;
      },
      true,
    );
  }, renderer);
  const json =
    (body: unknown) => (route: { fulfill: (o: unknown) => Promise<void> }) =>
      route.fulfill({ json: body });
  await page.route("**/api/install/status", json({ install_mode: false }));
  await page.route(
    "**/api/auth/check",
    json({
      ok: true,
      version: "test",
      product_version: "test",
      storage: { state_dir: "/tmp", workspace_root: "/workspace" },
    }),
  );
  await page.route(
    "**/api/status**",
    json({
      version: "test",
      session_count: SESSION_COUNT,
      push_subscription_count: 0,
      gui_process_count: 0,
      gui_stream_configured: false,
      fcm_enabled: false,
      history: {
        enabled: true,
        archived_session_count: 0,
        log_file_count: 0,
        log_bytes: 0,
        db_bytes: 0,
      },
      agent_tasks: {
        task_count: 0,
        prompt_task_dir_count: 0,
        prompt_file_count: 0,
        context_file_count: 0,
        prompt_bytes: 0,
        orphan_task_dir_count: 0,
      },
      auth_broker: { auto_agent_auth: false, helper: "disabled" },
      storage: { state_dir: "/tmp", workspace_root: "/workspace" },
    }),
  );
  await page.route(
    "**/api/config**",
    json({
      assistant_enabled: false,
      gui_stream_url: null,
      session_templates: [],
      gui_stream_available: false,
      vogt: { configured: true },
    }),
  );
  await page.route(/\/api\/sessions(\?.*)?$/, (route) => route.fulfill({ json: SESSIONS }));
  await page.route("**/api/sessions/*", (route) => {
    const id = route.request().url().split("/api/sessions/")[1]?.split(/[?/]/)[0];
    const session = SESSIONS.find((s) => s.id === id) ?? SESSIONS[0];
    return route.request().method() !== "GET"
      ? route.fulfill({ json: { ok: true } })
      : route.fulfill({ json: { ...session, scrollback_base64: "" } });
  });
  await page.route("**/api/events", (route) =>
    route.fulfill({
      status: 200,
      contentType: "text/event-stream",
      body: `data: ${JSON.stringify({ type: "activity", id: "s", state: "idle" })}\n\n`,
    }),
  );
  // Each session paints its own marker on attach.
  await page.routeWebSocket(/\/api\/sessions\/.*\/attach/, (ws) => {
    const id = /\/api\/sessions\/([^/]+)\/attach/.exec(ws.url())?.[1] ?? "?";
    let answered = false;
    ws.onMessage(() => {
      if (answered) return;
      answered = true;
      ws.send(
        JSON.stringify({
          type: "snapshot-start",
          session_id: id,
          scrollback_bytes: 0,
          scrollback_pos: 0,
          reset: true,
        }),
      );
      ws.send(JSON.stringify({ type: "snapshot-done" }));
      ws.send(Buffer.from(`marker-${id}\r\n`, "binary"));
    });
  });
}

const liveTerminals = (page: Page) => page.locator(".xterm").count();
const contextLosses = (page: Page) =>
  page.evaluate(() => (window as unknown as { __contextLost: number }).__contextLost);

async function visit(page: Page, id: string): Promise<void> {
  await page.evaluate((hash) => {
    location.hash = hash;
  }, `#/t/${id}`);
  await expect(page.locator(`[data-tab-id][data-tab-kind="terminal"]:visible .xterm`)).toHaveCount(1);
}

test.describe("live terminals are bounded (mocked)", () => {
  test.beforeEach(({}, testInfo) => {
    test.skip(isLive(testInfo.project.name), "mocked only");
    test.skip(testInfo.project.name === "phone", "narrow layout mounts only the active terminal");
  });

  for (const renderer of ["dom", "webgl"] as const) {
    test(`20 switches keep at most the cap mounted and revisit restores (${renderer})`, async ({ page }) => {
      await mockedFixtures(page, renderer);
      await page.goto(`/#/t/${SESSIONS[0].id}`);
      await expect(page.locator(".xterm")).toHaveCount(1);
      // Text is readable from the DOM only under the DOM renderer.
      const screen = page.locator('[data-tab-kind="terminal"]:visible .xterm-rows');
      const expectMarker = async (id: string) => {
        if (renderer === "dom") await expect(screen).toContainText(`marker-${id}`);
      };
      await expectMarker(SESSIONS[0].id);

      let peak = 0;
      for (let hop = 0; hop < 20; hop += 1) {
        await visit(page, SESSIONS[hop % SESSION_COUNT].id);
        const mounted = await liveTerminals(page);
        peak = Math.max(peak, mounted);
        expect(mounted).toBeLessThanOrEqual(CAP);
      }
      // The run must actually have reached the cap, or the bound proves nothing.
      expect(peak).toBe(CAP);
      expect(await contextLosses(page)).toBe(0);

      // The first session was disposed long ago; revisiting restores its screen.
      await visit(page, SESSIONS[0].id);
      await expectMarker(SESSIONS[0].id);
      expect(await liveTerminals(page)).toBeLessThanOrEqual(CAP);
      expect(await contextLosses(page)).toBe(0);
    });
  }
});
