import { expect, test, type Page } from "@playwright/test";
import { STRESS_PROFILE } from "../../src/demo/scale";

/** Release-only browser measurements for #422; normal CI skips this file. */
test.skip(!process.env.VOGT_RUN_PERF, "run with VOGT_RUN_PERF=1 for the release profile");
test.setTimeout(120_000);
test.describe.configure({ mode: "serial" });

const sha = "23ac0a9b8f7c6d5e4a32100123456789abcdef01";

async function installDemo(page: Page): Promise<void> {
  await page.route("**/demo-manifest.json", (route) => route.fulfill({
    status: 200,
    contentType: "application/json",
    body: JSON.stringify({ schema: 1, enabled: true, product_version: "local/dev", source_ref: "dev", source_sha: sha, scenario: "full-estate-v1" }),
  }));
  await page.route("**/demo-build.json", (route) => route.fulfill({
    status: 200,
    contentType: "application/json",
    body: JSON.stringify({ schema: 1, product_version: "local/dev", source_ref: "dev", source_sha: sha, assets: { "index.html": "a".repeat(64) } }),
  }));
  await page.route("**/api/**", (route) => route.abort("blockedbyclient"));
  await page.addInitScript(() => {
    const measurements: { name: string; start: number; duration: number }[] = [];
    (window as unknown as { __vogtPerf: typeof measurements }).__vogtPerf = measurements;
    if ("PerformanceObserver" in window) {
      const observer = new PerformanceObserver((list) => {
        for (const entry of list.getEntries()) {
          measurements.push({ name: "longtask", start: entry.startTime, duration: entry.duration });
        }
      });
      try { observer.observe({ type: "longtask", buffered: true }); } catch { /* support varies */ }
    }
  });
}

async function snapshot(page: Page, name: string): Promise<Record<string, unknown>> {
  return page.evaluate((label) => {
    const memory = (performance as Performance & { memory?: { usedJSHeapSize: number } }).memory;
    const tasks = ((window as unknown as { __vogtPerf: { name: string; duration: number }[] }).__vogtPerf ?? [])
      .filter((entry) => entry.name === "longtask");
    return {
      name: label,
      long_tasks: tasks.length,
      longest_task_ms: Math.max(0, ...tasks.map((entry) => entry.duration)),
      retained_nodes: document.querySelectorAll("*").length,
      heap_used_mb: memory ? Math.round(memory.usedJSHeapSize / 1024 / 1024 * 10) / 10 : null,
    };
  }, name);
}

test("large board stays windowed and records cold first interaction", async ({ page }) => {
  await installDemo(page);
  const started = Date.now();
  await page.goto(`/?demoScale=${STRESS_PROFILE.workItems}#/board`);
  await expect(page.getByRole("heading", { name: "Board" })).toBeVisible();
  await expect(page.locator(".board-card").first()).toBeVisible();
  const result = await snapshot(page, "board-cold");
  result.first_interaction_ms = Date.now() - started;
  result.board_cards = await page.locator(".board-card").count();
  console.log(JSON.stringify(result));
  expect(result.board_cards).toBeLessThan(200);
});

test("deep FileTree and 32-session rail record wake retained nodes", async ({ page }) => {
  await installDemo(page);
  await page.goto(`/?demoScale=${STRESS_PROFILE.workItems}#/sessions`);
  await expect(page.getByRole("heading", { name: "Sessions" })).toBeVisible();
  await expect(page.locator(".places-rail-session-area .session-row")).toHaveCount(STRESS_PROFILE.sessions);
  await page.getByRole("button", { name: "Files" }).click();
  await page.getByLabel("Expand stress").click();
  for (let depth = 1; depth <= STRESS_PROFILE.fileDepth; depth += 1) {
    await page.getByLabel(`Expand stress/depth-${String(depth).padStart(2, "0")}`).click();
  }
  const beforeWake = await snapshot(page, "tree-and-rail");
  await page.evaluate(() => {
    Object.defineProperty(document, "visibilityState", { configurable: true, value: "hidden" });
    document.dispatchEvent(new Event("visibilitychange"));
    Object.defineProperty(document, "visibilityState", { configurable: true, value: "visible" });
    document.dispatchEvent(new Event("visibilitychange"));
    window.dispatchEvent(new Event("focus"));
  });
  await page.waitForTimeout(250);
  const afterWake = await snapshot(page, "wake");
  console.log(JSON.stringify({ beforeWake, afterWake, profile: STRESS_PROFILE }));
  expect(await page.locator(".places-rail-session-area .session-row").count()).toBe(STRESS_PROFILE.sessions);
});
