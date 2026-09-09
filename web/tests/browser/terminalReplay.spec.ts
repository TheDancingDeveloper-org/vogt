// Terminal replay budget + live acceptance (H3, WI-124).
//
// Two projects, one file:
//
// - The default mocked projects (`desktop`/`phone`, Vite dev server) stream a
//   bounded corpus into a real terminal over a routed WebSocket and assert the
//   per-pane replay stays under budget and actually renders. No live stack.
//
// - The `live` project (only registered when PLAYWRIGHT_LIVE_BASE_URL is set,
//   selected by e2e.yml) drives a real load session on a real stack and asserts
//   the two operator symptoms directly: switching away and back replays only a
//   bounded tail (F1; a no-reset resume once F4 lands), and a reload does not
//   slow the active pane (F3/F5), with no spurious `[disconnected]` (F2). See
//   docs/local/TERMINAL_ATTACH_BUDGET_PLAN.md.

import { readFileSync } from "node:fs";
import { gunzipSync } from "node:zlib";

import { expect, test, type Page } from "@playwright/test";

const isLive = (project: string) => project === "live";

// ─── Mocked project ────────────────────────────────────────────────────────

// ~1 MiB — the client's per-pane replay budget (REPLAY_TAIL_MAX_BYTES), i.e. the
// most a cold attach delivers once F1 bounds it. Built from a real transcript.
function buildCorpus(target: number): Uint8Array {
  const seed = gunzipSync(
    readFileSync(new URL("../fixtures/transcripts/shell-plain.bin.gz", import.meta.url)),
  );
  const out = new Uint8Array(target);
  for (let off = 0; off < target; off += seed.byteLength) {
    out.set(seed.subarray(0, Math.min(seed.byteLength, target - off)), off);
  }
  return out;
}
const CORPUS = buildCorpus(1024 * 1024);

// The CI per-pane replay budget. Generous headroom over the measured parse rate
// (~5–6 MB/s ⇒ ~0.2 s for 1 MiB) so it is a real ceiling, not a flaky stopwatch;
// tune down once the self-hosted runner's number is known.
const REPLAY_BUDGET_MS = 2500;

const SESSION = {
  id: "sess-load",
  name: "load",
  cwd: "/workspace",
  activity: "idle",
  exit_code: null,
  scrollback_bytes: 0,
  created_at: "2026-09-09T00:00:00Z",
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
  await page.route("**/api/sessions/*", (route) => {
    if (route.request().method() !== "GET") return route.fulfill({ json: { ok: true } });
    return route.fulfill({ json: { ...SESSION, scrollback_base64: "" } });
  });
  await page.route("**/api/events", (route) =>
    route.fulfill({
      status: 200,
      contentType: "text/event-stream",
      body: `data: ${JSON.stringify({ type: "activity", id: "s", state: "idle" })}\n\n`,
    }),
  );
}

// Route the attach WebSocket and stream `corpus` as one cold snapshot, exactly
// as the engine would (snapshot-start → binary chunks → snapshot-done).
async function streamSnapshot(page: Page, corpus: Uint8Array): Promise<void> {
  await page.routeWebSocket(/\/api\/sessions\/.*\/attach/, (ws) => {
    ws.onMessage(() => {
      ws.send(
        JSON.stringify({
          type: "snapshot-start",
          session_id: SESSION.id,
          scrollback_bytes: corpus.byteLength,
          scrollback_pos: corpus.byteLength,
          reset: true,
        }),
      );
      const CHUNK = 64 * 1024;
      for (let i = 0; i < corpus.byteLength; i += CHUNK) {
        ws.send(Buffer.from(corpus.subarray(i, Math.min(i + CHUNK, corpus.byteLength))));
      }
      ws.send(JSON.stringify({ type: "snapshot-done" }));
    });
  });
}

test.describe("terminal replay budget (mocked)", () => {
  test.beforeEach(({}, testInfo) => {
    test.skip(isLive(testInfo.project.name), "mocked only");
  });

  test("a bounded snapshot renders and replays under the per-pane budget", async ({ page }) => {
    const replayLines: string[] = [];
    page.on("console", (msg) => {
      if (msg.text().includes("[vogt] terminal replay")) replayLines.push(msg.text());
    });

    await mockedFixtures(page);
    await streamSnapshot(page, CORPUS);
    await page.goto("/#/t/sess-load");

    // The snapshot replay emits a `vogt-terminal-replay.snapshot` measure.
    const handle = await page.waitForFunction(
      () => {
        const m = performance
          .getEntriesByType("measure")
          .find((e) => e.name === "vogt-terminal-replay.snapshot");
        return m ? { duration: m.duration } : null;
      },
      undefined,
      { timeout: 20_000 },
    );
    const measure = (await handle.jsonValue()) as { duration: number };

    // The measure only exists once the replay has drained into xterm, so its
    // presence proves the 1 MiB snapshot rendered; its duration is the per-pane
    // budget the plan bounds.
    expect(measure.duration).toBeGreaterThan(0);
    expect(measure.duration).toBeLessThan(REPLAY_BUDGET_MS);

    // The client logged the `[vogt] terminal replay` telemetry the live spec
    // reads (kind, snapshotBytes, replayDurationMs).
    await expect.poll(() => replayLines.length, { timeout: 5_000 }).toBeGreaterThan(0);
  });
});

// ─── Live project ──────────────────────────────────────────────────────────
//
// Runs only under PLAYWRIGHT_LIVE_BASE_URL (the `live` project), against a real
// stack booted by e2e.yml with a load-generating session. It asserts the two
// operator symptoms as the plan's H3 requires. Not exercised by mocked runs.

const LIVE_TOKEN = process.env.PLAYWRIGHT_LIVE_TOKEN ?? "";
// ~3 MiB/min of coloured, sequence-numbered output, then idle — the same shape
// as the engine harness's load session, so the ring wraps within the test.
const LIVE_LOAD_COMMAND = [
  "/bin/bash",
  "-c",
  "i=0; while [ \"$i\" -lt 400000 ]; do " +
    "printf '\\033[3%dmSEQ%08d the quick brown fox jumps over the lazy dog\\033[0m\\n' " +
    '"$((i % 8))" "$i"; i=$((i + 1)); ' +
    "if [ \"$((i % 64))\" -eq 0 ]; then printf '\\033[H'; fi; " +
    "done; exec sleep 3600",
];

async function liveCreateLoadSession(page: Page): Promise<string> {
  const res = await page.request.post("/api/sessions", {
    headers: { authorization: `Bearer ${LIVE_TOKEN}` },
    data: { name: "h3-load", command: LIVE_LOAD_COMMAND },
  });
  expect(res.ok()).toBeTruthy();
  return ((await res.json()) as { id: string }).id;
}

async function liveScrollbackPos(page: Page, id: string): Promise<number> {
  const res = await page.request.get(`/api/sessions/${id}`, {
    headers: { authorization: `Bearer ${LIVE_TOKEN}` },
  });
  return ((await res.json()) as { scrollback_pos?: number }).scrollback_pos ?? 0;
}

test.describe("terminal replay budget (live acceptance)", () => {
  test.beforeEach(({}, testInfo) => {
    test.skip(!isLive(testInfo.project.name), "live only");
  });

  test("symptom 1: switch away past the ring and back — bounded reattach, no disconnect", async ({
    page,
  }) => {
    const replays: { kind?: string; snapshotBytes?: number; reset?: boolean }[] = [];
    page.on("console", (msg) => {
      const text = msg.text();
      if (!text.includes("[vogt] terminal replay")) return;
      const m = text.match(/\{.*\}/);
      if (m) {
        try {
          replays.push(JSON.parse(m[0]));
        } catch {
          /* not the structured arg */
        }
      }
    });

    const id = await liveCreateLoadSession(page);
    await page.goto(`/#/t/${id}`);
    await expect(page.locator(".xterm-rows")).toContainText("SEQ", { timeout: 20_000 });
    const openPos = await liveScrollbackPos(page, id);

    // Switch away to another place, wait until the ring has wrapped well past
    // where we were, then switch back.
    await page.goto("/#/sessions");
    await expect
      .poll(() => liveScrollbackPos(page, id), { timeout: 60_000, intervals: [500] })
      .toBeGreaterThan(openPos + 4 * 1024 * 1024);
    replays.length = 0; // only measure the reattach
    await page.goto(`/#/t/${id}`);
    await expect(page.locator(".xterm-rows")).toContainText("SEQ", { timeout: 20_000 });

    // No [disconnected] marker in the buffer.
    await expect(page.locator(".xterm-rows")).not.toContainText("[disconnected]");
    // The reattach replayed at most the budget (F1). It may be a bounded reset
    // until F4 lands, when it becomes a no-reset resume; either way it is <= budget.
    const reattach = replays.filter((r) => r.kind === "snapshot" || r.kind === "cache");
    for (const r of reattach) {
      expect(r.snapshotBytes ?? 0).toBeLessThanOrEqual(1024 * 1024);
    }
  });

  test("symptom 2: reload with the session cached — active pane under budget", async ({ page }) => {
    const id = await liveCreateLoadSession(page);
    await page.goto(`/#/t/${id}`);
    await expect(page.locator(".xterm-rows")).toContainText("SEQ", { timeout: 20_000 });
    // Let the client persist its cache, then reload.
    await page.waitForTimeout(6000);
    await page.reload();

    const handle = await page.waitForFunction(
      () => {
        const m = performance
          .getEntriesByType("measure")
          .find((e) => e.name.startsWith("vogt-terminal-replay."));
        return m ? { duration: m.duration } : null;
      },
      undefined,
      { timeout: 20_000 },
    );
    const measure = (await handle.jsonValue()) as { duration: number };
    expect(measure.duration).toBeLessThan(REPLAY_BUDGET_MS);
    await expect(page.locator(".xterm-rows")).not.toContainText("[disconnected]");
  });
});
