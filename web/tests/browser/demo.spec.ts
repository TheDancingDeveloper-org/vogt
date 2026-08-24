import { expect, test, type Page } from "@playwright/test";

const sha = "23ac0a9b8f7c6d5e4a32100123456789abcdef01";

async function installDemo(page: Page) {
  await page.route("**/demo-manifest.json", (route) => route.fulfill({
    status: 200,
    contentType: "application/json",
    body: JSON.stringify({ schema: 1, enabled: true, source_ref: "dev", source_sha: sha, scenario: "full-estate-v1" }),
  }));
  await page.route("**/demo-build.json", (route) => route.fulfill({
    status: 200,
    contentType: "application/json",
    body: JSON.stringify({ schema: 1, source_ref: "dev", source_sha: sha, assets: { "index.html": "a".repeat(64) } }),
  }));
  // If the in-browser seam misses one request, fail rather than silently using
  // the Vite proxy or a developer's running engine.
  await page.route("**/api/**", (route) => route.abort("blockedbyclient"));
}

const routes = [
  ["/", "Vogt"], ["/sessions", "Sessions"], ["/t/demo-build", "Build PWA"],
  ["/e/README.md", "README.md"], ["/g", "Choose a registered project"], ["/g/README.md", "demo/full-estate"],
  ["/gui", "GUI"], ["/history", "History"], ["/tasks", "Agent Tasks"],
  ["/board", "Board"], ["/backlog", "Backlog"], ["/inbox", "Inbox"],
  ["/projects", "Projects"], ["/audit", "Audit"], ["/setup", "Setup"],
  ["/settings", "Settings"], ["/w/WI-101", "Make every surface"],
  ["/assistant", "Assistant"], ["/assistant/demo", "Assistant"],
] as const;

for (const [route, expected] of routes) {
  test(`demo route ${route} is populated`, async ({ page }) => {
    await installDemo(page);
    await page.goto(`/#${route}`);
    await expect(page.getByLabel("Public demo information")).toBeVisible();
    await expect(page.locator("body")).toContainText(expected, { timeout: 15_000 });
    await expect(page.getByText("This place could not be loaded")).toHaveCount(0);
    await expect(page.getByText("Checking your session…")).toHaveCount(0);
  });
}

test("canonical terminal links restore two-pane and nested three-pane layouts", async ({ page }) => {
  await installDemo(page);
  await page.goto("/#/t/demo-build");
  const build = page.locator('[data-tab-id="term:demo-build"]');
  await expect(build.locator(".terminal-pane")).toHaveCount(2, { timeout: 15_000 });
  await expect(build.locator(".terminal-pane-chip")).toContainText(["Build PWA", "Test suite"]);

  await page.goto("/#/t/demo-agent");
  const agent = page.locator('[data-tab-id="term:demo-agent"]');
  await expect(agent.locator(".terminal-pane")).toHaveCount(3, { timeout: 15_000 });
  await expect(agent.locator(".terminal-split.column")).toHaveCount(1);

  await page.goto("/#/t/demo-logs");
  const incident = page.locator('[data-tab-id="term:demo-logs"]');
  await expect(incident.locator(".terminal-pane")).toHaveCount(3, { timeout: 15_000 });
  await expect(incident.getByText("Input fan-out", { exact: true })).toBeVisible();
});

test("demo reset restores canonical tab-local state", async ({ page }) => {
  await installDemo(page);
  await page.goto("/#/board");
  await expect(page.getByLabel("Public demo information")).toBeVisible();
  await page.evaluate(() => {
    const state = JSON.parse(sessionStorage.getItem("vogt.demo.state.v1") ?? "{}");
    state.next_id = 999;
    sessionStorage.setItem("vogt.demo.state.v1", JSON.stringify(state));
  });
  await page.getByRole("button", { name: "Reset demo" }).click();
  await expect(page.getByLabel("Public demo information")).toBeVisible();
  expect(await page.evaluate(() => JSON.parse(sessionStorage.getItem("vogt.demo.state.v1") ?? "{}").next_id)).toBe(200);
});

test("phone composition keeps disclosure and navigation usable", async ({ page }, testInfo) => {
  test.skip(testInfo.project.name !== "phone", "phone project only");
  await installDemo(page);
  await page.goto("/#/inbox");
  await expect(page.getByLabel("Public demo information")).toBeVisible();
  await expect(page.locator(".phone-bottom-nav")).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth)).toBe(true);
});

test("stale provenance cannot activate demo mode", async ({ page }) => {
  await page.route("**/demo-manifest.json", (route) => route.fulfill({
    status: 200,
    contentType: "application/json",
    body: JSON.stringify({ schema: 1, enabled: true, source_ref: "dev", source_sha: sha, scenario: "full-estate-v1" }),
  }));
  await page.route("**/demo-build.json", (route) => route.fulfill({
    status: 200,
    contentType: "application/json",
    body: JSON.stringify({ schema: 1, source_ref: "dev", source_sha: "f".repeat(40), assets: { "index.html": "a".repeat(64) } }),
  }));
  await page.goto("/#/board");
  await expect(page.getByLabel("Public demo information")).toHaveCount(0);
  await expect(page.getByRole("heading", { name: /sign in/i })).toBeVisible();
});

test("canonical demo compositions stay visually stable at target widths", async ({ page }, testInfo) => {
  test.skip(testInfo.project.name !== "desktop", "one canonical screenshot set");
  await installDemo(page);

  await page.setViewportSize({ width: 1440, height: 1000 });
  await page.goto("/#/board");
  await expect(page.getByRole("heading", { name: "Board" })).toBeVisible();
  await expect(page).toHaveScreenshot("demo-board-1440.png", { animations: "disabled" });

  await page.setViewportSize({ width: 768, height: 900 });
  await page.goto("/#/inbox");
  await expect(page.getByRole("heading", { name: "Inbox" })).toBeVisible();
  await expect(page).toHaveScreenshot("demo-inbox-768.png", { animations: "disabled" });

  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/#/t/demo-agent");
  await expect(page.locator('[data-tab-id="term:demo-agent"] .terminal-pane')).toHaveCount(3);
  await page.waitForTimeout(250);
  await expect(page).toHaveScreenshot("demo-agent-390.png", { animations: "disabled" });
});
