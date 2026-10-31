// A phone kills a terminal socket while the app is in the background
// (WI-2705). The pane must not retry invisibly, write [disconnected] or flash
// the reconnect overlay for a drop the reader never saw: it marks the socket
// dead and attaches again as soon as the page is visible.

import { expect, test, type Page } from "@playwright/test";

const isLive = (project: string) => project === "live";

const SESSION = {
  id: "sess-drop",
  name: "drop",
  cwd: "/workspace",
  activity: "idle",
  exit_code: null,
  scrollback_bytes: 0,
  created_at: "2026-09-20T00:00:00Z",
};

async function mockedFixtures(page: Page): Promise<void> {
  await page.addInitScript(() => {
    localStorage.setItem("vogt.token", "browser-test-token");
    localStorage.setItem("vogt.appTheme.v1", "dark");
  });
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
      session_count: 1,
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
  await page.route(/\/api\/sessions(\?.*)?$/, (route) =>
    route.fulfill({ json: [SESSION] }),
  );
  await page.route("**/api/sessions/*", (route) =>
    route.request().method() !== "GET"
      ? route.fulfill({ json: { ok: true } })
      : route.fulfill({ json: { ...SESSION, scrollback_base64: "" } }),
  );
  await page.route("**/api/events", (route) =>
    route.fulfill({
      status: 200,
      contentType: "text/event-stream",
      body: `data: ${JSON.stringify({ type: "activity", id: "s", state: "idle" })}\n\n`,
    }),
  );
}

/** Let the test flip document.visibilityState the way a phone does. */
async function controllableVisibility(page: Page): Promise<void> {
  await page.addInitScript(() => {
    let state: DocumentVisibilityState = "visible";
    Object.defineProperty(document, "visibilityState", {
      configurable: true,
      get: () => state,
    });
    Object.defineProperty(document, "hidden", {
      configurable: true,
      get: () => state === "hidden",
    });
    (
      window as unknown as {
        __setVisibility: (s: DocumentVisibilityState) => void;
      }
    ).__setVisibility = (next) => {
      state = next;
      document.dispatchEvent(new Event("visibilitychange"));
    };
    // Record every moment the reconnect overlay is in the DOM.
    const w = window as unknown as { __overlaySeen: number };
    w.__overlaySeen = 0;
    new MutationObserver(() => {
      if (document.querySelector(".terminal-reconnect-overlay"))
        w.__overlaySeen += 1;
    }).observe(document, { childList: true, subtree: true });
  });
}

test.describe("terminal socket dropped while hidden (mocked)", () => {
  test.beforeEach(({}, testInfo) => {
    test.skip(isLive(testInfo.project.name), "mocked only");
  });

  test("reconnects within 1 s of return with no overlay flash", async ({
    page,
  }) => {
    await controllableVisibility(page);
    await mockedFixtures(page);

    const attaches: {
      close: (o?: { code?: number; reason?: string }) => Promise<void>;
    }[] = [];
    await page.routeWebSocket(/\/api\/sessions\/.*\/attach/, (ws) => {
      attaches.push(ws);
      let answered = false;
      ws.onMessage(() => {
        if (answered) return;
        answered = true;
        ws.send(
          JSON.stringify({
            type: "snapshot-start",
            session_id: SESSION.id,
            scrollback_bytes: 0,
            scrollback_pos: 0,
            reset: true,
          }),
        );
        ws.send(JSON.stringify({ type: "snapshot-done" }));
        ws.send(Buffer.from("hello\r\n", "binary"));
      });
    });

    await page.goto(`/#/t/${SESSION.id}`);
    await expect(page.locator(".xterm")).toBeVisible();
    await expect.poll(() => attaches.length).toBe(1);
    await expect(page.locator(".terminal-status-overlay")).toHaveCount(0);

    await page.evaluate(() =>
      (
        window as unknown as { __setVisibility: (s: string) => void }
      ).__setVisibility("hidden"),
    );
    await attaches[0].close({ code: 1011, reason: "network gone" });
    // Hidden: no background retry.
    await page.waitForTimeout(1_500);
    expect(attaches.length).toBe(1);

    const returnedAt = Date.now();
    await page.evaluate(() =>
      (
        window as unknown as { __setVisibility: (s: string) => void }
      ).__setVisibility("visible"),
    );
    await expect.poll(() => attaches.length, { timeout: 1_000 }).toBe(2);
    expect(Date.now() - returnedAt).toBeLessThan(1_000);

    await page.waitForTimeout(500);
    expect(
      await page.evaluate(
        () => (window as unknown as { __overlaySeen: number }).__overlaySeen,
      ),
    ).toBe(0);
    await expect(page.locator(".terminal-reconnect-overlay")).toHaveCount(0);
  });
});
