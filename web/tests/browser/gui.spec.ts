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

const liveSession = {
  id: "browser-session",
  name: "browser-session",
  cwd: "/workspace/vogt",
  activity: "idle",
  exit_code: null,
  scrollback_bytes: 1024,
  created_at: "2026-08-18T08:00:00Z",
};

async function installFixtures(
  page: Page,
  config: Record<string, unknown> = {},
  initialSessions: Record<string, unknown>[] = [],
) {
  let inboxCalls = 0;
  const sessions = [...initialSessions];
  let createdSessions = 0;
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
    storage: { state_dir: "/tmp/vogt", workspace_root: "/workspace" },
  } }));
  await page.route("**/api/config**", async (route) => route.fulfill({ json: {
    assistant_enabled: false, gui_stream_url: null, session_templates: [],
    gui_stream_available: false,
    vogt: { configured: true },
    ...config,
  } }));
  await page.route("**/api/sessions", async (route) => {
    if (route.request().method() === "POST") {
      createdSessions += 1;
      const body = route.request().postDataJSON() as Record<string, unknown>;
      const session = {
        id: `browser-split-${createdSessions}`,
        name: body.name ?? `browser-split-${createdSessions}`,
        cwd: body.cwd ?? "/workspace/vogt",
        activity: "idle",
        exit_code: null,
        scrollback_bytes: 1024,
        created_at: "2026-08-18T08:00:00Z",
      };
      sessions.push(session);
      return route.fulfill({ json: session });
    }
    return route.fulfill({ json: sessions });
  });
  await page.route("**/api/sessions/*/kill", async (route) =>
    route.fulfill({ json: { ok: true } }),
  );
  await page.route("**/api/sessions/*", async (route) => {
    if (
      route.request().method() === "POST"
      && new URL(route.request().url()).pathname.endsWith("/kill")
    ) {
      return route.fulfill({ json: { ok: true } });
    }
    if (route.request().method() === "DELETE") {
      const id = new URL(route.request().url()).pathname.split("/").at(-1);
      const index = sessions.findIndex((session) => session.id === id);
      if (index >= 0) sessions.splice(index, 1);
      return route.fulfill({ json: { ok: true } });
    }
    return route.fulfill({ status: 404, json: { error: "not found" } });
  });
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
    if (url.pathname.endsWith("/projects")) return route.fulfill({ json: {
      projects: [{ slug: "vogt", name: "Vogt", root_path: "/workspace/vogt" }],
      total: 1,
    } });
    if (url.pathname.endsWith("/labels")) return route.fulfill({ json: { labels: [] } });
    if (url.pathname.endsWith("/initiatives")) return route.fulfill({ json: { initiatives: [] } });
    if (url.pathname.endsWith("/actors")) return route.fulfill({ json: { actors: [] } });
    return route.fulfill({ json: {} });
  });
  return { inboxCalls: () => inboxCalls, sessions };
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

test("Sessions owns the tool workspace and retains only terminal continuity", async ({ page }) => {
  await installFixtures(page, {}, [liveSession]);
  await page.routeWebSocket(/\/api\/sessions\/[^/]+\/attach$/, (socket) => {
    socket.onMessage(() => undefined);
    socket.send(JSON.stringify({
      type: "snapshot-start",
      scrollback_bytes: 0,
      scrollback_pos: 0,
    }));
    socket.send(JSON.stringify({ type: "snapshot-done" }));
  });
  await page.route("**/api/history/sessions**", async (route) =>
    route.fulfill({ json: [] }),
  );

  await page.goto("/#/t/browser-session");
  await expect(page.locator(".terminal-host")).toBeVisible();
  await expect(page.getByRole("region", { name: "Sessions" })).toBeVisible();
  if (test.info().project.name === "desktop") {
    await expect(page.getByRole("complementary", { name: "Live sessions" }))
      .toContainText("browser-session");
  }

  await page.goto("/#/history");
  await expect(page.locator(".history-view")).toBeVisible();
  await expect(page.locator('[data-tab-kind="terminal"]')).toHaveCount(1);

  await page.goto("/#/tasks");
  await expect(page.locator(".agent-tasks-view")).toBeVisible();
  await expect(page.locator('[data-tab-kind="terminal"]')).toHaveCount(1);
  await expect(page.locator('[data-tab-kind="history"]')).toHaveCount(0);
  await expect(page.locator('[data-tab-kind="tasks"]')).toHaveCount(1);
});

test("Git chooser is addressable and a failed panel recovers in place", async ({ page }) => {
  await installFixtures(page);
  let historyUnavailable = true;
  await page.route("**/api/git/status**", async (route) => route.fulfill({ json: {
    repo: "vogt", is_repo: true, branch: "main", ahead: 0, behind: 0, entries: [],
  } }));
  await page.route("**/api/git/branch**", async (route) =>
    route.fulfill({ json: { current: "main", all: ["main"] } }),
  );
  await page.route("**/api/git/log**", async (route) => {
    if (historyUnavailable) {
      return route.fulfill({ status: 503, body: "history service offline" });
    }
    return route.fulfill({ json: [{
      hash: "abc1234", short: "abc1234", author: "Ada",
      date: "2026-08-18T08:00:00Z", subject: "Recovered in browser",
    }] });
  });

  await page.goto("/#/g");
  await expect(page.getByRole("heading", { name: "Choose a repository" })).toBeVisible();
  const choice = page.getByRole("button", { name: /Vogt/ });
  if (test.info().project.name === "phone") {
    await choice.tap();
  } else {
    await choice.focus();
    await page.keyboard.press("Enter");
  }
  await expect(page).toHaveURL(/#\/g\/vogt$/);

  const error = page.getByRole("alert").filter({ hasText: "Commit history" });
  await expect(error).toContainText("history service offline");
  historyUnavailable = false;
  await error.getByRole("button", { name: "Retry history" }).click();
  await expect(page.getByText("Recovered in browser")).toBeVisible();

  await page.goBack();
  await expect(page.getByRole("heading", { name: "Choose a repository" })).toBeVisible();
  await page.goForward();
  await expect(page).toHaveURL(/#\/g\/vogt$/);
  await page.reload();
  await expect(page.getByText("Recovered in browser")).toBeVisible();
});

test("terminal split commits atomically, nests, closes and survives reload", async ({ page }) => {
  test.skip(test.info().project.name === "phone", "Split geometry is validated in the desktop browser project");
  await installFixtures(page, {}, [liveSession]);
  const resizeMessages: unknown[] = [];
  await page.routeWebSocket(/\/api\/sessions\/[^/]+\/attach$/, (socket) => {
    socket.onMessage((message) => {
      if (typeof message !== "string") return;
      const parsed = JSON.parse(message) as { type?: string };
      if (parsed.type === "resize") resizeMessages.push(parsed);
    });
    socket.send(JSON.stringify({ type: "snapshot-start", scrollback_bytes: 0, scrollback_pos: 0 }));
    socket.send(JSON.stringify({ type: "snapshot-done" }));
  });
  await page.goto("/#/t/browser-session");
  await expect(page.locator(".terminal-pane")).toHaveCount(1);

  await page.getByRole("button", { name: "Split right" }).click();
  await expect(page.locator(".terminal-pane")).toHaveCount(2);
  await page.getByRole("button", { name: "Split down" }).click();
  await expect(page.locator(".terminal-pane")).toHaveCount(3);
  await expect(page.locator(".terminal-split.row .terminal-split.column"))
    .toHaveCount(1);
  await expect(page.locator(".terminal-pane.active")).toHaveCount(1);

  resizeMessages.length = 0;
  await page.setViewportSize({ width: 1100, height: 760 });
  await expect.poll(() => resizeMessages.length).toBeGreaterThanOrEqual(3);

  await page.reload();
  await expect(page.locator(".terminal-pane")).toHaveCount(3);
  await page.getByRole("button", { name: "Close pane" }).click();
  await page.getByRole("dialog").getByRole("button", { name: "Confirm" }).click();
  await expect(page.locator(".terminal-pane")).toHaveCount(2);
});

test("terminal split retries at the default cwd when the source cwd is outside the workspace", async ({ page }) => {
  test.skip(test.info().project.name === "phone", "Split fallback is validated in the desktop browser project");
  await installFixtures(page, {}, [liveSession]);
  await page.routeWebSocket(/\/api\/sessions\/[^/]+\/attach$/, (socket) => {
    socket.onMessage(() => undefined);
    socket.send(JSON.stringify({ type: "snapshot-start", scrollback_bytes: 0, scrollback_pos: 0 }));
    socket.send(JSON.stringify({ type: "snapshot-done" }));
  });

  const requests: Record<string, unknown>[] = [];
  await page.route("**/api/sessions", async (route) => {
    if (route.request().method() !== "POST") return route.fallback();
    const body = route.request().postDataJSON() as Record<string, unknown>;
    requests.push(body);
    if (body.cwd) {
      return route.fulfill({ status: 400, body: "cwd escapes workspace_root" });
    }
    return route.fulfill({ json: {
      ...liveSession,
      id: "fallback-split",
      name: String(body.name),
    } });
  });

  await page.goto("/#/t/browser-session");
  await page.getByRole("button", { name: "Split right" }).click();
  await expect(page.locator(".terminal-pane")).toHaveCount(2);
  await expect(page.getByText("Split opened at the default cwd")).toBeVisible();
  expect(requests).toHaveLength(2);
  expect(requests[0]?.cwd).toBe("/workspace/vogt");
  expect(requests[1]).not.toHaveProperty("cwd");
});

test("terminal split deletes the created session if its pane disappears in flight", async ({ page }) => {
  test.skip(test.info().project.name === "phone", "Split rollback is validated in the desktop browser project");
  await installFixtures(page, {}, [liveSession]);
  await page.routeWebSocket(/\/api\/sessions\/[^/]+\/attach$/, (socket) => {
    socket.onMessage(() => undefined);
    socket.send(JSON.stringify({ type: "snapshot-start", scrollback_bytes: 0, scrollback_pos: 0 }));
    socket.send(JSON.stringify({ type: "snapshot-done" }));
  });

  let releaseCreate: (() => void) | null = null;
  let deletedId: string | null = null;
  await page.route("**/api/sessions", async (route) => {
    if (route.request().method() !== "POST") return route.fallback();
    await new Promise<void>((resolve) => { releaseCreate = resolve; });
    return route.fulfill({ json: {
      ...liveSession,
      id: "orphan-candidate",
      name: "orphan-candidate",
    } });
  });
  await page.route("**/api/sessions/orphan-candidate", async (route) => {
    if (route.request().method() !== "DELETE") return route.fallback();
    deletedId = "orphan-candidate";
    return route.fulfill({ json: { ok: true } });
  });

  await page.goto("/#/t/browser-session");
  await page.getByRole("button", { name: "Split right" }).click();
  await page.getByTitle("Kill & remove").click();
  await page.getByRole("dialog").getByRole("button", { name: "Confirm" }).click();
  await expect(page.getByRole("heading", { name: "Session not found" })).toBeVisible();
  expect(releaseCreate).not.toBeNull();
  releaseCreate?.();
  await expect.poll(() => deletedId).toBe("orphan-candidate");
});

test("terminal leaves browser zoom gestures alone and offers explicit font controls", async ({ page }) => {
  await installFixtures(page, {}, [liveSession]);
  await page.routeWebSocket(/\/api\/sessions\/[^/]+\/attach$/, (socket) => {
    socket.onMessage(() => undefined);
    socket.send(JSON.stringify({ type: "snapshot-start", scrollback_bytes: 0, scrollback_pos: 0 }));
    socket.send(JSON.stringify({ type: "snapshot-done" }));
  });
  await page.goto("/#/t/browser-session");
  const host = page.locator(".terminal-host");
  await expect(host).toBeVisible();

  const cancelled = await host.evaluate((element) =>
    !element.dispatchEvent(new WheelEvent("wheel", {
      bubbles: true,
      cancelable: true,
      ctrlKey: true,
      deltaY: -120,
    })),
  );
  expect(cancelled).toBe(false);
  expect(await page.evaluate(() => localStorage.getItem("mydevenv2.terminalFontSize.v1")))
    .toBeNull();

  await page.getByRole("button", { name: "Increase terminal font size" }).click();
  expect(await page.evaluate(() => localStorage.getItem("mydevenv2.terminalFontSize.v1")))
    .toBe("14");

  for (const zoom of ["80%", "100%", "125%", "150%", "200%"] as const) {
    await page.locator("html").evaluate((element, nextZoom) => {
      element.style.zoom = nextZoom;
    }, zoom);
    await expect(page.getByRole("navigation", { name: "Session tools" })).toBeVisible();
    await expect(host).toBeVisible();
  }
});

test("dirty editor requests browser exit confirmation only until save", async ({ page }) => {
  test.skip(test.info().project.name === "phone", "Monaco lifecycle is validated in the desktop browser project");
  await installFixtures(page);
  let saved = "export const answer = 42;\n";
  await page.route("**/api/files**", async (route) => {
    if (route.request().method() === "PUT") {
      saved = (route.request().postDataJSON() as { content: string }).content;
      return route.fulfill({ json: { ok: true, bytes: saved.length } });
    }
    return route.fulfill({ json: {
      path: "src/an-identifiable-long-filename.tsx",
      size: saved.length,
      content: saved,
      content_base64: null,
      is_binary: false,
    } });
  });
  await page.route("**/api/history/sessions**", async (route) =>
    route.fulfill({ json: [] }),
  );
  await page.goto("/#/e/src%2Fan-identifiable-long-filename.tsx");
  const editor = page.locator(".monaco-editor");
  await expect(editor).toBeVisible();
  await editor.click();
  await page.keyboard.press("Control+A");
  await page.keyboard.type("export const answer = 43;");

  expect(await page.evaluate(() => {
    const event = new Event("beforeunload", { cancelable: true });
    window.dispatchEvent(event);
    return event.defaultPrevented;
  })).toBe(true);

  await page.goto("/#/history");
  await expect(page.locator(".monaco-editor")).toHaveCount(0);
  await page.goto("/#/e/src%2Fan-identifiable-long-filename.tsx");
  await expect(page.locator(".monaco-editor .view-lines")).toContainText("43");

  await page.getByRole("button", { name: "Save", exact: true }).click();
  await expect.poll(() => saved).toContain("43");
  await expect.poll(() => page.evaluate(() => {
    const event = new Event("beforeunload", { cancelable: true });
    window.dispatchEvent(event);
    return event.defaultPrevented;
  })).toBe(false);
});

test("History distinguishes an archive outage from an empty archive and recovers", async ({ page }) => {
  await installFixtures(page);
  let failed = false;
  await page.route("**/api/history/sessions**", async (route) => {
    if (!failed) {
      failed = true;
      return route.fulfill({ status: 503, json: { error: "archive offline" } });
    }
    return route.fulfill({ json: [] });
  });

  await page.goto("/#/history");
  const error = page.getByRole("alert");
  await expect(error).toContainText("Failed to load history");
  await expect(error).toContainText("archive offline");
  await expect(page.getByText("0 archived sessions")).toHaveCount(0);
  await expect(page.getByText("No archived sessions.")).toHaveCount(0);

  const retry = page.getByRole("button", { name: "Retry history" });
  if (test.info().project.name === "phone") await retry.tap();
  else await retry.click();

  await expect(page.getByText("0 archived sessions")).toBeVisible();
  await expect(page.getByText("No archived sessions.")).toBeVisible();
  await expect(error).toHaveCount(0);
});
