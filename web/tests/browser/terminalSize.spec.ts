// Terminal size sync across viewers (WI-1089).
//
// A desktop pane at 178×32 and a phone at ~50 columns share one PTY. The
// phone used to draw the stream at its own width; Klaudia (Bubble Tea) repaints
// its input box with `ESC[3A` + LF + the 178-column box line, which wraps at
// the phone's width, so every spinner tick left a ghost copy of the box and
// the transcript scrolled away. The engine now names the PTY's size on
// `snapshot-start` and in `resize` frames, and every viewer draws at it.

import { expect, test, type Page, type WebSocketRoute } from "@playwright/test";

const SESSION = {
  id: "sess-size",
  name: "klaudia",
  cwd: "/workspace",
  activity: "running",
  exit_code: null,
  scrollback_bytes: 0,
  created_at: "2026-10-08T00:00:00Z",
};

async function mockedFixtures(page: Page): Promise<void> {
  await page.addInitScript(() => {
    localStorage.setItem("vogt.token", "browser-test-token");
    localStorage.setItem("vogt.appTheme.v1", "dark");
  });
  const json = (body: unknown) => (route: { fulfill: (o: unknown) => Promise<void> }) =>
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
  await page.route("**/api/sessions/*", (route) => {
    if (route.request().method() !== "GET") return route.fulfill({ json: { ok: true } });
    return route.fulfill({ json: { ...SESSION, scrollback_base64: "" } });
  });
  await page.route("**/api/events", (route) =>
    route.fulfill({
      status: 200,
      contentType: "text/event-stream",
      body: `data: ${JSON.stringify({ type: "activity", id: "s", state: "running" })}\n\n`,
    }),
  );
}

// Klaudia's frame at `cols` wide: transcript, the bordered input box and the
// status line, cursor parked at the start of the status row.
function initialFrame(cols: number): string {
  const transcript = Array.from({ length: 8 }, (_, i) => `transcript line ${i + 1}`);
  const top = `╭${"─".repeat(cols - 3)}╮`;
  const bottom = `╰${"─".repeat(cols - 3)}╯`;
  return [...transcript, top, boxLine(cols, true), bottom, "  grok-4.7 · 58 turns"].join("\r\n") + "\r";
}

function boxLine(cols: number, cursorOn: boolean): string {
  const placeholder = cursorOn ? "\x1b[7mA\x1b[0msk Klaudia…" : "Ask Klaudia…";
  return `│ › ${placeholder}${" ".repeat(cols - 3 - 4 - 12)}│`;
}

// One spinner tick, byte for byte the shape in the live session log: up three
// rows, LF onto the box line, repaint it, then back down to the status row.
function repaint(cols: number, tick: number): string {
  return `\x1b[3A\n${boxLine(cols, tick % 2 === 0)}\x1b[K\r\n\n\r`;
}

interface Pty {
  ws: WebSocketRoute;
  resizes: { cols: number; rows: number }[];
}

// The engine side: answers attach with a 178×32 snapshot; on the phone's first
// resize it announces that size, then the desktop takes the size back and
// Klaudia repaints for 178 columns.
async function routeAttach(page: Page): Promise<{ ready: Promise<Pty> }> {
  let resolvePty: (pty: Pty) => void = () => {};
  const ready = new Promise<Pty>((r) => (resolvePty = r));
  await page.routeWebSocket(/\/api\/sessions\/.*\/attach/, (ws) => {
    const pty: Pty = { ws, resizes: [] };
    let authed = false;
    ws.onMessage((raw) => {
      if (typeof raw !== "string") return; // keystrokes
      const msg = JSON.parse(raw) as { type: string; cols?: number; rows?: number };
      if (msg.type === "auth" && !authed) {
        authed = true;
        const frame = Buffer.from(initialFrame(178));
        ws.send(
          JSON.stringify({
            type: "snapshot-start",
            session_id: SESSION.id,
            scrollback_bytes: frame.byteLength,
            scrollback_pos: frame.byteLength,
            reset: true,
            cols: 178,
            rows: 32,
          }),
        );
        ws.send(frame);
        ws.send(JSON.stringify({ type: "snapshot-done" }));
        return;
      }
      if (msg.type === "resize" && msg.cols && msg.rows) {
        pty.resizes.push({ cols: msg.cols, rows: msg.rows });
        if (pty.resizes.length === 1) resolvePty(pty);
      }
    });
  });
  return { ready };
}

const rowsText = (page: Page) =>
  page.locator(".terminal-host .xterm-rows").innerText({ timeout: 10_000 });

test.describe("terminal size sync (WI-1089)", () => {
  test.beforeEach(({}, testInfo) => {
    test.skip(testInfo.project.name !== "phone", "the phone viewport is the narrow viewer");
  });

  test("a phone draws a wider PTY's repaints without ghost frames, and can take the size", async ({
    page,
  }) => {
    await mockedFixtures(page);
    const { ready: ptyReady } = await routeAttach(page);
    await page.goto(`/#/t/${SESSION.id}`);

    const pty = await ptyReady;
    const phone = pty.resizes[0]!;
    expect(phone.cols).toBeLessThan(178);

    // The engine applies the phone's size, then the desktop takes it back.
    pty.ws.send(JSON.stringify({ type: "resize", cols: phone.cols, rows: phone.rows }));
    pty.ws.send(JSON.stringify({ type: "resize", cols: 178, rows: 32 }));
    pty.ws.send(Buffer.from(`\x1b[2J\x1b[H${initialFrame(178)}`));
    for (let tick = 0; tick < 30; tick++) pty.ws.send(Buffer.from(repaint(178, tick)));
    pty.ws.send(Buffer.from("\x1b[3A\nDONE-MARKER\x1b[K\r\n\n\r"));

    await expect.poll(() => rowsText(page), { timeout: 10_000 }).toContain("DONE-MARKER");
    const text = await rowsText(page);
    // One input box, not a stack of ghosts, and the transcript still on screen.
    expect(text.match(/Ask Klaudia/g) ?? []).toHaveLength(0); // the last tick replaced it
    expect(text.match(/DONE-MARKER/g)).toHaveLength(1);
    expect(text).toContain("transcript line 1");
    expect(text).toContain("transcript line 8");
    expect(text.match(/grok-4\.7/g)).toHaveLength(1);

    // The phone follows the desktop's size and no longer resizes the PTY on
    // its own; it offers to take the size instead.
    const chip = page.getByTestId("terminal-size-chip");
    await expect(chip).toBeVisible();
    await expect(chip).toContainText("178×32");
    const before = pty.resizes.length;
    // More output and a refit at the same size take nothing back.
    for (let tick = 0; tick < 10; tick++) pty.ws.send(Buffer.from(repaint(178, tick)));
    await page.evaluate(() => window.dispatchEvent(new Event("resize")));
    await page.waitForTimeout(300);
    expect(pty.resizes.length).toBe(before);

    // Tapping it asks for the phone's size; once the engine applies it the
    // phone draws at its own width again.
    await chip.click();
    await expect.poll(() => pty.resizes.length).toBeGreaterThan(before);
    const asked = pty.resizes.at(-1)!;
    expect(asked.cols).toBe(phone.cols);
    pty.ws.send(JSON.stringify({ type: "resize", cols: asked.cols, rows: asked.rows }));
    await expect(chip).toBeHidden();
  });
});
