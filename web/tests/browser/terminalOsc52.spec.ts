// OSC 52 clipboard write — the terminal must put a program's OSC 52 copy on the
// system clipboard. A PTY program (Claude Code, tmux, vim's +clipboard) copies
// by emitting `ESC ]52;c;<base64>`; without a handler xterm drops it silently.
// This mounts a real terminal over a routed WebSocket, streams an OSC 52 write
// as live PTY output, and asserts the browser clipboard holds the decoded text.

import { expect, test, type Page } from "@playwright/test";

const isLive = (project: string) => project === "live";

const SESSION = {
  id: "sess-osc",
  name: "osc",
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
  await page.route("**/api/sessions", (route) => route.fulfill({ json: [SESSION] }));
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

test.describe("terminal OSC 52 clipboard (mocked)", () => {
  test.beforeEach(({}, testInfo) => {
    test.skip(isLive(testInfo.project.name), "mocked only");
    test.skip(testInfo.project.name === "phone", "desktop clipboard path");
  });

  test("an OSC 52 write from the PTY lands on the system clipboard", async ({
    page,
    context,
  }) => {
    await context.grantPermissions(["clipboard-read", "clipboard-write"]);
    await mockedFixtures(page);

    const secret = `osc52-clipboard-${Date.now()}`;
    const payload = Buffer.from(secret, "utf8").toString("base64");
    const osc52 = `\x1b]52;c;${payload}\x07`;

    await page.routeWebSocket(/\/api\/sessions\/.*\/attach/, (ws) => {
      ws.onMessage(() => {
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
        // The OSC 52 copy, as a live PTY frame after the snapshot.
        ws.send(Buffer.from(osc52, "binary"));
      });
    });

    await page.goto(`/#/t/${SESSION.id}`);
    await expect(page.locator(".xterm")).toBeVisible();

    await expect
      .poll(async () => page.evaluate(() => navigator.clipboard.readText()), {
        timeout: 10_000,
      })
      .toBe(secret);
  });
});
