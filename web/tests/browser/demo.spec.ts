import { expect, test, type Page } from "@playwright/test";
import { readFileSync } from "node:fs";
import { DEMO_NOW } from "../../src/demo/fixtures";

const sha = "23ac0a9b8f7c6d5e4a32100123456789abcdef01";
const mobileShowcase = readFileSync(
  new URL("../../src/demo/mobile-showcase.html", import.meta.url),
  "utf8",
);

async function installDemo(page: Page) {
  await page.route("**/mobile-demo.html", (route) => route.fulfill({
    status: 200,
    contentType: "text/html",
    body: mobileShowcase,
  }));
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

test("canonical terminal links restore split layouts with responsive chrome", async ({ page }, testInfo) => {
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
  if (testInfo.project.name === "phone") {
    await expect(incident.locator(".terminal-mobile-header")).toBeVisible();
    await expect(incident.locator(".terminal-mobile-counter")).toHaveText("3 / 8");
    await expect(incident.getByRole("button", { name: "Show Metrics watch" })).toBeVisible();
  } else {
    await expect(incident.getByText("Input fan-out", { exact: true })).toBeVisible();
  }
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

test("phone Sessions overview exposes waiting and non-waiting work", async ({ page }, testInfo) => {
  test.skip(testInfo.project.name !== "phone", "phone project only");
  await installDemo(page);
  await page.goto("/#/sessions");
  await expect(page.getByLabel("Public demo information")).toBeVisible();
  await expect(page.getByRole("article", { name: /Agent review is waiting for input/ })).toBeVisible();
  await expect(page.locator('.session-list a[href="#/t/demo-build"]')).toContainText("Build PWA");
  await expect(page.locator('.session-list a[href="#/t/demo-server"]')).toContainText("Preview server");
  await expect(page.locator(".phone-bottom-nav")).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= document.documentElement.clientWidth)).toBe(true);
});

test("phone terminal uses the implemented attention pager", async ({ page }, testInfo) => {
  test.skip(testInfo.project.name !== "phone", "phone project only");
  await installDemo(page);
  await page.goto("/#/t/demo-server");

  await expect(page.locator(".sessions-header")).toBeHidden();
  const workspace = page.locator('[data-tab-id="term:demo-server"]');
  await expect(workspace.locator(".terminal-mobile-session strong")).toHaveText("Preview server");
  await expect(workspace.locator(".terminal-mobile-stage")).toBeVisible();
  await workspace.getByRole("button", { name: "Show Test suite" }).click();
  await expect(page).toHaveURL(/#\/t\/demo-tests$/);
  await expect(page.locator('[data-tab-id="term:demo-tests"] .terminal-mobile-session strong')).toHaveText("Test suite");
  await expect(page.locator(".phone-bottom-nav")).toBeHidden();
});

test("phone Assistant shows structured context and inline approval", async ({ page }, testInfo) => {
  test.skip(testInfo.project.name !== "phone", "phone project only");
  await installDemo(page);
  await page.goto("/#/assistant");

  await expect(page.locator(".sessions-header")).toBeHidden();
  await expect(page.locator(".assistant-trace")).toContainText(
    "▸ listed sessions · read Agent review tail",
  );
  await expect(page.locator(".assistant-session-chip")).toHaveAttribute(
    "href",
    "#/t/demo-agent",
  );
  await expect(page.locator(".assistant-open-session")).toContainText(
    "Open Agent review ›",
  );
  await expect(page.getByRole("region", { name: "Pending approval" })).toContainText(
    "Send approve demo snapshot ⏎ to Agent review",
  );
});

test("mobile demo website frames the real responsive PWA", async ({ page }, testInfo) => {
  test.skip(testInfo.project.name !== "desktop", "outer showcase is a desktop website");
  await page.clock.setFixedTime(new Date(DEMO_NOW));
  await installDemo(page);
  await page.setViewportSize({ width: 1440, height: 1000 });
  await page.goto("/mobile-demo.html");

  await expect(page.getByRole("heading", { name: "Vogt in your hand." })).toBeVisible();
  const frame = page.frameLocator('iframe[title="Interactive Vogt mobile application"]');
  await expect(frame.getByRole("heading", { name: "Sessions", exact: true })).toBeVisible();
  await expect(frame.locator(".phone-bottom-nav")).toBeVisible();
  await expect(page).toHaveScreenshot("demo-mobile-site-1440.png", {
    animations: "disabled",
  });

  await page.getByRole("link", { name: "Assistant" }).click();
  await expect(frame.getByRole("heading", { name: "Assistant", exact: true })).toBeVisible();
  await expect(frame.locator(".assistant-session-chip")).toContainText("Agent review");
});

test("stale provenance cannot activate demo mode", async ({ page }) => {
  await page.route("**/demo-manifest.json", (route) => route.fulfill({
    status: 200,
    contentType: "application/json",
    body: JSON.stringify({ schema: 1, enabled: true, product_version: "local/dev", source_ref: "dev", source_sha: sha, scenario: "full-estate-v1" }),
  }));
  await page.route("**/demo-build.json", (route) => route.fulfill({
    status: 200,
    contentType: "application/json",
    body: JSON.stringify({ schema: 1, product_version: "local/dev", source_ref: "dev", source_sha: "f".repeat(40), assets: { "index.html": "a".repeat(64) } }),
  }));
  await page.goto("/#/board");
  await expect(page.getByLabel("Public demo information")).toHaveCount(0);
  await expect(page.getByRole("heading", { name: /sign in/i })).toBeVisible();
});

test("canonical demo compositions stay visually stable at target widths", async ({ page }, testInfo) => {
  test.skip(testInfo.project.name !== "desktop", "one canonical screenshot set");
  // Freeze the wall clock at the scenario anchor. The demo's data timestamps
  // are a stable logical clock, but the "updated Xs ago" badges (viewAge's
  // createNow) subtract the *real* Date.now(), so their text advances between
  // captures and drifted these screenshots by ~1% on the age strings alone.
  // setFixedTime pins Date.now() without pausing timers, so the scripted
  // terminal streams below still play out for the three-pane shot.
  await page.clock.setFixedTime(new Date(DEMO_NOW));
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
