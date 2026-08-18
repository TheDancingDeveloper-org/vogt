import { expect, test, type Page } from "@playwright/test";

const inboxEntry = {
  entry_key: "drift:proposal-1:material-v1",
  source: "drift",
  kind: "state_mismatch",
  occurred_at: "2026-08-17T10:00:00Z",
  observed_at: "2026-08-17T10:01:00Z",
  title: "Work item state differs from observed state",
  summary: "Observed state is done; declared state is in_progress.",
  project_slug: "vogt",
  work_item_ref: "WI-7",
  source_subject_key: "proposal-1",
  trust_state: "disputed",
  freshness: "current",
  triage_state: "active",
  action: { kind: "drift", drift_id: "proposal-1" },
  evidence_snapshot: { observed_state: "done", observed_at: "2026-08-17T10:00:00Z" },
  proposed_change: { state: "done" },
};

const inboxResult = () => ({
  entries: [inboxEntry],
  snapshot_at: "2026-08-17T10:01:00Z",
  high_water: { github: null, drift: "2026-08-17T10:00:00Z", ci: null, agent: null },
  coverage: {
    github: { status: "unswept", count: 0, detail: "not collected" },
    drift: { status: "current", count: 1 },
    ci: { status: "unswept", count: 0, detail: "not collected" },
    agent: { status: "unconfigured", count: 0, detail: "no engine" },
  },
  counts: { active: 1, archived: 0, snoozed: 0 },
  instance_scope: "registered projects only",
  engine_status: "not_configured",
  engine_available: false,
});

const boardItems = [
  {
    ref: "WI-7", title: "Measured board card", kind: "feature", state: "open",
    priority: "normal", project_slug: "vogt", trust_state: "verified", labels: [],
    score: 1, updated_at: "2026-08-17T10:00:00Z", origin: "declared",
  },
];

async function installFixtures(
  page: Page,
  config: Record<string, unknown> = {},
) {
  let inboxCalls = 0;
  await page.addInitScript(() => {
    localStorage.setItem("mydevenv2.token", "browser-test-token");
  });
  await page.route("**/api/status**", async (route) => route.fulfill({ json: {
    version: "test",
    session_count: 0,
    push_subscription_count: 0,
    gui_process_count: 0,
    gui_stream_configured: false,
    fcm_enabled: false,
    history: {
      enabled: true, archived_session_count: 0, log_file_count: 0,
      log_bytes: 0, db_bytes: 0,
    },
    agent_tasks: {
      task_count: 0, prompt_task_dir_count: 0, prompt_file_count: 0,
      context_file_count: 0, prompt_bytes: 0, orphan_task_dir_count: 0,
    },
    auth_broker: { auto_agent_auth: false, helper: "disabled" },
    storage: { state_dir: "/tmp/vogt", workspace_root: "/workspace/vogt" },
  } }));
  await page.route("**/api/config**", async (route) => route.fulfill({ json: {
    assistant_enabled: false, gui_stream_url: null, session_templates: [],
    gui_stream_available: false,
    vogt: { configured: true },
    ...config,
  } }));
  await page.route("**/api/sessions", async (route) => route.fulfill({ json: [] }));
  await page.route("**/api/events", async (route) => route.fulfill({ status: 200, contentType: "text/event-stream", body: "" }));
  await page.route("**/api/tree**", async (route) => route.fulfill({ json: [
    { name: "src", path: "src", is_dir: true },
    { name: "an-identifiable-long-filename.tsx", path: "src/an-identifiable-long-filename.tsx", is_dir: false },
  ] }));
  await page.route("**/api/tasks**", async (route) => route.fulfill({ json: [] }));
  await page.route("**/api/vogt/**", async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    if (url.pathname.endsWith("/inbox")) {
      inboxCalls += 1;
      return route.fulfill({ json: inboxResult() });
    }
    if (url.pathname.endsWith("/inbox/archive")) {
      return route.fulfill({ json: { entry: { ...inboxEntry, triage_state: "archived" } } });
    }
    if (url.pathname.endsWith("/workflows")) {
      return route.fulfill({ json: { workflows: [{ kind: "feature", initial_state: "open", states: ["open", "done"], transitions: { open: ["done"], done: [] } }] } });
    }
    if (url.pathname.endsWith("/work")) return route.fulfill({ json: { items: boardItems, total: 1, freshness: { status: "fresh" } } });
    if (url.pathname.endsWith("/projects")) return route.fulfill({ json: { projects: [{ slug: "vogt", name: "Vogt" }] } });
    if (url.pathname.endsWith("/labels")) return route.fulfill({ json: { labels: [] } });
    if (url.pathname.endsWith("/initiatives")) return route.fulfill({ json: { initiatives: [] } });
    if (url.pathname.endsWith("/actors")) return route.fulfill({ json: { actors: [] } });
    return route.fulfill({ json: {} });
  });
  return { inboxCalls: () => inboxCalls };
}

test("Board dragover/drop uses the real browser gesture and keeps its filter on reload", async ({ page }) => {
  test.skip(test.info().project.name === "phone", "Drag/drop is validated in the desktop browser project");
  await installFixtures(page);
  await page.goto("/#/board?project=vogt");
  await expect(page.getByRole("heading", { name: "Board" })).toBeVisible();
  await expect(page.getByText("Measured board card")).toBeVisible();
  const card = page.locator(".board-card").filter({ hasText: "Measured board card" });
  const target = page.locator('.board-cell[data-state="done"]');
  await card.dragTo(target);
  await expect(target.locator("textarea")).toBeVisible();
  await page.reload();
  await expect(page).toHaveURL(/#\/board\?project=vogt/);
  await expect(page.getByLabel("Board filters")).toBeVisible();
});

test("Inbox evidence, source filter and batch reason survive a desktop browser", async ({ page }) => {
  const fixture = await installFixtures(page);
  await page.goto("/#/inbox?source=drift");
  await expect(page.getByRole("heading", { name: inboxEntry.title })).toBeVisible();
  await expect(page.getByRole("region", { name: "Drift evidence" })).toContainText("observed_state");
  await expect(page.getByRole("button", { name: "Reject proposed change" })).toBeVisible();
  await expect(page.locator(".inbox-filter select")).toHaveValue("drift");
  await page.getByLabel(`Select ${inboxEntry.title}`).check();
  await page.getByLabel("Batch reason").fill("reviewed in browser");
  await page.getByRole("button", { name: "Archive selected" }).click();
  await expect.poll(() => fixture.inboxCalls()).toBeGreaterThan(1);
  await expect(page).toHaveURL(/#\/inbox\?source=drift/);
});

test("Phone shell keeps labelled primary navigation and Go to reachability", async ({ page }) => {
  await installFixtures(page);
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/#/sessions");
  await expect(page.getByRole("navigation", { name: "Primary navigation" })).toBeVisible();
  await expect(page.getByRole("link", { name: "Inbox" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Go to…" })).toBeVisible();
  await page.getByRole("button", { name: "Go to…" }).click();
  await expect(page.getByRole("dialog", { name: "Command palette" })).toBeVisible();
  await expect(page.getByText("Open Audit")).toBeVisible();
});

test("Dialog focus is contained and restored, and feedback matches its live region", async ({ page }) => {
  test.skip(test.info().project.name === "phone", "Settings is a desktop route");
  await installFixtures(page);
  await page.goto("/#/sessions");

  const goTo = page.getByRole("button", { name: "Go to…" });
  await goTo.click();
  const palette = page.getByRole("dialog", { name: "Command palette" });
  await expect(palette).toBeVisible();
  await expect(palette.locator("input")).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(palette).toBeHidden();
  await expect(goTo).toBeFocused();

  await page.getByRole("button", { name: "Settings" }).click();
  const settings = page.getByRole("dialog", { name: "Settings", includeHidden: true });
  await expect(settings).toBeVisible();
  const saveLayout = settings.getByRole("button", { name: "Save Current Layout" });
  await saveLayout.click();
  const layoutPrompt = page.getByRole("dialog", { name: "Save workspace layout" });
  await expect(layoutPrompt).toBeVisible();
  await expect(settings).toHaveAttribute("aria-hidden", "true");
  await layoutPrompt.locator("input").fill("Browser layout");
  await layoutPrompt.getByRole("button", { name: "Save" }).click();
  await expect(
    page.getByRole("region", { name: "Notifications" }).getByRole("status"),
  ).toContainText('Saved layout "Browser layout"');
  await expect(saveLayout).toBeFocused();
  await settings.getByRole("button", { name: "Cancel" }).click();

  await page.route("**/api/sessions", async (route) => {
    if (route.request().method() === "POST") {
      return route.fulfill({ status: 503, json: { error: "session service offline" } });
    }
    return route.fulfill({ json: [] });
  });
  await page.getByRole("button", { name: "Go to…" }).click();
  const createPalette = page.getByRole("dialog", { name: "Command palette" });
  await createPalette.getByText("New Terminal Session", { exact: true }).click();
  const prompt = page.getByRole("dialog", { name: "New session" });
  await expect(prompt).toBeVisible();
  await prompt.locator("input").fill("browser-failure");
  await prompt.getByRole("button", { name: "Save" }).click();
  const error = page.getByRole("alert");
  await expect(error).toContainText("Error");
  await expect(error).toContainText("Session creation failed");
  await expect(error.getByText("Open details")).toBeVisible();
  await expect(error.getByRole("button", { name: "Retry" })).toBeVisible();
  await expect(
    error.getByRole("button", { name: "Dismiss Session creation failed" }),
  ).toBeVisible();
});

test("Files rail keeps names legible and puts secondary actions behind one control", async ({ page }) => {
  test.skip(test.info().project.name === "phone", "The places rail is a desktop surface");
  await installFixtures(page);
  await page.goto("/#/sessions");
  await expect(page.getByText("an-identifiable-long-filename.tsx")).toBeVisible();
  await expect(page.getByText("TSX", { exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "Actions for src", exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "Delete" })).toHaveCount(0);
  await page.getByRole("button", { name: "Actions for src", exact: true }).click();
  await expect(page.getByRole("button", { name: "Open terminal" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Delete" })).toBeVisible();
});

test("Route truth owns unavailable links, current navigation and Settings return", async ({ page }) => {
  await installFixtures(page);
  await page.goto("/#/board?project=vogt");

  const phone = test.info().project.name === "phone";
  const currentNavigation = phone ? page.locator('.phone-bottom-nav a[aria-current="page"]') : page.locator('.places-nav a[aria-current="page"]');
  await expect(currentNavigation).toHaveText("Board");
  await expect(page.getByRole("link", { name: "GUI stream" })).toHaveCount(0);

  await page.getByRole("button", { name: "Go to…" }).click();
  await expect(page.getByRole("dialog", { name: "Command palette" }).getByText("Open GUI Stream", { exact: true }))
    .toHaveCount(0);
  await page.keyboard.press("Escape");

  if (phone) await page.getByRole("button", { name: "Go to…" }).click();
  if (phone) {
    await page.getByRole("dialog", { name: "Command palette" })
      .getByText("Open Settings", { exact: true })
      .click();
  } else await page.getByRole("button", { name: "Settings" }).click();
  await expect(page).toHaveURL(/#\/settings$/);
  await expect(currentNavigation).toHaveText("Board");
  await page.getByRole("dialog", { name: "Settings" }).getByRole("button", { name: "Cancel" }).click();
  await expect(page).toHaveURL(/#\/board\?project=vogt$/);

  if (!phone) {
    await page.getByRole("button", { name: "Settings" }).click();
    await expect(page).toHaveURL(/#\/settings$/);
    await page.goBack();
    await expect(page).toHaveURL(/#\/board\?project=vogt$/);
    await expect(page.getByRole("dialog", { name: "Settings" })).toHaveCount(0);
  }

  await page.goto("/#/history");
  if (phone) {
    await expect(page.locator('.phone-bottom-nav a[aria-current="page"]')).toContainText("Sessions");
  } else {
    await expect(page.locator('.places-nav a[aria-current="page"]')).toHaveText("History");
  }
  await expect(page.getByRole("navigation", { name: "Session tools" }).getByRole("link", { name: "History" }))
    .toHaveAttribute("aria-current", "page");

  if (phone) await page.getByRole("button", { name: "Go to…" }).click();
  if (phone) {
    await page.getByRole("dialog", { name: "Command palette" })
      .getByText("Open Settings", { exact: true })
      .click();
  } else await page.getByRole("button", { name: "Settings" }).click();
  await expect(page).toHaveURL(/#\/settings$/);
  await expect(page.getByRole("navigation", { name: "Session tools" })
    .getByRole("link", { name: "History" }))
    .toHaveAttribute("aria-current", "page");
  await page.getByRole("dialog", { name: "Settings" })
    .getByRole("button", { name: "Cancel" })
    .click();
  await expect(page).toHaveURL(/#\/history$/);

  await page.goto("/#/history");
  await page.evaluate(() => {
    window.location.hash = "#/t/missing-session";
  });
  await expect(page).toHaveURL(/#\/t\/missing-session$/);
  await expect(page.getByRole("heading", { name: "Session not found" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Return to Sessions" })).toBeVisible();
  await page.reload();
  await expect(page.getByRole("heading", { name: "Session not found" })).toBeVisible();
  await page.goBack();
  await expect(page).toHaveURL(/#\/history$/);
  await expect(page.locator(".history-view")).toBeVisible();
  await page.goForward();
  await expect(page.getByRole("heading", { name: "Session not found" })).toBeVisible();

  await page.goto("/#/history");
  await page.evaluate(() => {
    window.location.hash = "#/assistant";
  });
  await expect(page).toHaveURL(/#\/assistant$/);
  await expect(page.getByRole("heading", { name: "Assistant is unavailable" })).toBeVisible();
  await page.reload();
  await expect(page.getByRole("heading", { name: "Assistant is unavailable" })).toBeVisible();
  await page.goBack();
  await expect(page).toHaveURL(/#\/history$/);
  await expect(page.locator(".history-view")).toBeVisible();
  await page.goForward();
  await expect(page.getByRole("heading", { name: "Assistant is unavailable" })).toBeVisible();

  await page.goto("/#/gui");
  await expect(page.getByRole("heading", { name: "GUI stream is unavailable" })).toBeVisible();

  await page.goto("/#/settings");
  await page.reload();
  await page.getByRole("dialog", { name: "Settings" })
    .getByRole("button", { name: "Cancel" })
    .click();
  await expect(page).toHaveURL(/#\/sessions$/);
});

test("Enabled GUI capability is reachable and renders its configured stream", async ({ page }) => {
  await page.route("https://stream.example.test/**", async (route) =>
    route.fulfill({ contentType: "text/html", body: "<main>fixture stream</main>" }),
  );
  await installFixtures(page, {
    gui_stream_url: "https://stream.example.test/view",
    gui_stream_available: true,
    features: { selkies: "1.6.2" },
  });
  await page.route("**/api/gui/processes", async (route) => route.fulfill({ json: [] }));
  await page.goto("/#/sessions");

  await expect(page.getByRole("link", { name: "GUI stream" }).first()).toBeVisible();
  await page.getByRole("button", { name: "Go to…" }).click();
  await page.getByRole("dialog", { name: "Command palette" })
    .getByText("Open GUI Stream", { exact: true })
    .click();

  await expect(page).toHaveURL(/#\/gui$/);
  const frame = page.getByTitle("GUI stream");
  await expect(frame).toBeVisible();
  await expect(frame).toHaveAttribute("src", "https://stream.example.test/view");
  await expect(frame.contentFrame().getByText("fixture stream")).toBeVisible();
});
