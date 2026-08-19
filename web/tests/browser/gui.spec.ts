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

interface PlaceMetricFixtures {
  sessionsUnavailable?: boolean;
  inboxActive?: number;
  projectsTotal?: number | null;
  boardTotal?: number | null;
  backlogTotal?: number | null;
}

async function installFixtures(
  page: Page,
  config: Record<string, unknown> = {},
  initialSessions: Record<string, unknown>[] = [],
  tree: Record<string, unknown>[] = [
    { name: "src", path: "src", is_dir: true },
    { name: "an-identifiable-long-filename.tsx", path: "src/an-identifiable-long-filename.tsx", is_dir: false },
  ],
  metricOverrides: PlaceMetricFixtures = {},
) {
  const metrics = {
    sessionsUnavailable: false,
    inboxActive: 1,
    projectsTotal: 1,
    boardTotal: 1,
    backlogTotal: 1,
    ...metricOverrides,
  };
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
    if (metrics.sessionsUnavailable) {
      return route.fulfill({ status: 503, body: "sessions unavailable" });
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
  await page.route("**/api/tree**", async (route) => {
    const path = new URL(route.request().url()).searchParams.get("path") ?? "";
    if (path === "src") {
      return route.fulfill({ json: [
        { name: "nested-component.tsx", path: "src/nested-component.tsx", is_dir: false },
      ] });
    }
    return route.fulfill({ json: tree });
  });
  await page.route("**/api/tasks**", async (route) => route.fulfill({ json: [] }));
  await page.route("**/api/vogt/**", async (route) => {
    const request = route.request();
    const url = new URL(request.url());
    if (url.pathname.endsWith("/inbox")) {
      inboxCalls += 1;
      return route.fulfill({ json: {
        ...inboxResult(),
        counts: { active: metrics.inboxActive, archived: 0, snoozed: 0 },
      } });
    }
    if (url.pathname.endsWith("/inbox/archive")) {
      return route.fulfill({ json: { entry: { ...inboxEntry, triage_state: "archived" } } });
    }
    if (url.pathname.endsWith("/workflows")) {
      return route.fulfill({ json: { workflows: [{ kind: "feature", initial_state: "open", states: ["open", "done"], transitions: { open: ["done"], done: [] } }] } });
    }
    if (url.pathname.endsWith("/work")) {
      if (metrics.boardTotal === null) return route.fulfill({ status: 503, body: "work unavailable" });
      return route.fulfill({ json: { items: boardItems, total: metrics.boardTotal, freshness: { status: "fresh" } } });
    }
    if (url.pathname.endsWith("/backlog")) {
      if (metrics.backlogTotal === null) return route.fulfill({ status: 503, body: "backlog unavailable" });
      return route.fulfill({ json: {
        items: boardItems,
        total_considered: metrics.backlogTotal,
        freshness: { status: "fresh", collectors: {} },
      } });
    }
    if (url.pathname.endsWith("/projects")) {
      if (metrics.projectsTotal === null) return route.fulfill({ status: 503, body: "projects unavailable" });
      return route.fulfill({ json: {
      projects: [{ slug: "vogt", name: "Vogt", root_path: "/workspace/vogt" }],
      total: metrics.projectsTotal,
      } });
    }
    if (url.pathname.endsWith("/labels")) return route.fulfill({ json: { labels: [] } });
    if (url.pathname.endsWith("/initiatives")) return route.fulfill({ json: { initiatives: [] } });
    if (url.pathname.endsWith("/actors")) return route.fulfill({ json: { actors: [] } });
    return route.fulfill({ json: {} });
  });
  return { inboxCalls: () => inboxCalls, sessions };
}

test("History palette results restore the selected session, query and match", async ({ page }) => {
  await installFixtures(page);
  const archivedSessions = [
    {
      id: "archive-alpha", name: "alpha archive", cwd: "/workspace/alpha",
      command: "pnpm test", created_at: "2026-08-18T09:00:00Z",
      ended_at: "2026-08-18T09:05:00Z", exit_code: 0, scrollback_bytes: 18,
    },
    {
      id: "archive-beta", name: "beta archive", cwd: "/workspace/beta",
      command: "cargo test", created_at: "2026-08-18T10:00:00Z",
      ended_at: "2026-08-18T10:05:00Z", exit_code: 0, scrollback_bytes: 17,
    },
  ];
  const matches = [
    {
      session_id: "archive-alpha", session_name: "alpha archive",
      created_at: "2026-08-18T09:00:00Z",
      match_snippet: "alpha says <mark>needle</mark>", rank: -2,
    },
    {
      session_id: "archive-beta", session_name: "beta archive",
      created_at: "2026-08-18T10:00:00Z",
      match_snippet: "beta says <mark>needle</mark>", rank: -1,
    },
  ];
  await page.route("**/api/history/**", async (route) => {
    const path = new URL(route.request().url()).pathname;
    if (path === "/api/history/sessions") return route.fulfill({ json: archivedSessions });
    if (path === "/api/history/search") return route.fulfill({ json: matches });
    if (path.endsWith("/log")) {
      const id = path.split("/").at(-2)!;
      return route.fulfill({ json: {
        session_id: id, text: `${id} says needle`, bytes: 20,
        total_bytes: 20, truncated: false,
      } });
    }
    const id = path.split("/").at(-1);
    const session = archivedSessions.find((candidate) => candidate.id === id);
    return session
      ? route.fulfill({ json: session })
      : route.fulfill({ status: 404, json: { error: "not found" } });
  });

  await page.goto("/#/board?project=vogt");
  await expect(page.getByRole("heading", { name: "Board" })).toBeVisible();
  await page.getByRole("button", { name: "Go to…" }).click();
  const palette = page.getByRole("dialog", { name: "Command palette" });
  await palette.locator("input").fill("> needle");
  await expect(palette.getByText("alpha archive", { exact: true })).toBeVisible();
  await expect(palette.getByText("beta archive", { exact: true })).toBeVisible();
  await palette.getByText("beta archive", { exact: true }).click();

  await expect(page).toHaveURL(/#\/history\?q=needle&session=archive-beta&match=m[0-9a-f]{8}$/);
  await expect(page.getByLabel("Search all archived output")).toHaveValue("needle");
  await expect(page.getByRole("heading", { name: "beta archive" })).toBeVisible();
  const selectedMatch = page.locator('.history-result-snippet.qualified-match[aria-current="true"]');
  await expect(selectedMatch).toContainText("beta says needle");
  await expect(selectedMatch.locator("mark")).toHaveText("needle");

  const qualifiedUrl = page.url();
  await page.reload();
  await expect(page).toHaveURL(qualifiedUrl);
  await expect(page.getByRole("heading", { name: "beta archive" })).toBeVisible();
  await expect(selectedMatch).toContainText("beta says needle");

  await page.goBack();
  await expect(page).toHaveURL(/#\/board\?project=vogt$/);
  await expect(page.getByRole("heading", { name: "Board" })).toBeVisible();

  await page.getByRole("button", { name: "Go to…" }).click();
  const searchPalette = page.getByRole("dialog", { name: "Command palette" });
  await searchPalette.locator("input").fill("Search History");
  await searchPalette.getByText("Search History", { exact: true }).click();
  await expect(page).toHaveURL(/#\/history\?focus=search$/);
  await expect(page.getByLabel("Search all archived output")).toBeFocused();
});

test("Login and authentication errors present Vogt as the only product", async ({ page }) => {
  await page.route("**/api/public-config", async (route) => route.fulfill({ json: {} }));
  await page.route("**/api/status", async (route) => route.fulfill({
    status: 401,
    json: { error: "unauthorized" },
  }));
  await page.goto("/#/sessions");

  await expect(page).toHaveTitle("Sign in · Vogt");
  await expect(page.getByRole("heading", { name: "Sign in to Vogt" })).toBeVisible();
  await expect(page.locator("body")).not.toContainText("MyDevEnv2");
  await page.getByLabel("Bearer token").fill("rejected-browser-token");
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(page.getByRole("alert")).toContainText("current Vogt token");
});

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

test("Vogt identity and route-aware titles survive navigation and reload", async ({ page }) => {
  await installFixtures(page, { assistant_enabled: true });
  await page.goto("/#/board?project=vogt");
  await expect(page).toHaveTitle("Board · Vogt");
  await expect(page.locator(".places-brand")).toHaveText("Vogt");

  await page.goto("/#/history");
  await expect(page).toHaveTitle("History · Vogt");
  await page.reload();
  await expect(page).toHaveTitle("History · Vogt");

  const manifest = await page.evaluate(async () => {
    const response = await fetch("/manifest.webmanifest");
    return response.json() as Promise<Record<string, unknown>>;
  });
  expect(manifest).toMatchObject({ id: "/", name: "Vogt", short_name: "Vogt" });

  const serviceWorker = await page.evaluate(async () => {
    const registration = await navigator.serviceWorker.ready;
    await registration.update();
    const source = await fetch("/sw.js").then((response) => response.text());
    return {
      script: registration.active?.scriptURL,
      source,
    };
  });
  expect(serviceWorker.script).toMatch(/\/sw\.js$/);
  expect(serviceWorker.source).toContain('const title = payload.title || "Vogt"');
  expect(serviceWorker.source).not.toContain("MyDevEnv2");
});

test("Inbox evidence, source filter and batch reason survive a desktop browser", async ({ page }) => {
  const fixture = await installFixtures(page);
  await page.goto("/#/inbox?source=drift");
  await expect(page.getByRole("heading", { name: inboxEntry.title })).toBeVisible();
  await expect(page.getByRole("region", { name: "Drift evidence" })).toContainText("observed_state");
  await expect(page.getByRole("button", { name: "Reject proposed change…" })).toBeVisible();
  await expect(page.getByPlaceholder("Why this triage decision?")).toHaveCount(0);
  await expect(page.locator(".inbox-filter select")).toHaveValue("drift");
  await page.getByLabel(`Select ${inboxEntry.title}`).check();
  await page.getByRole("button", { name: "Archive selected…" }).click();
  await page.getByLabel("Batch reason").fill("reviewed in browser");
  await page.locator(".inbox-batch-composer").getByRole("button", { name: "Confirm archive" }).click();
  await expect.poll(() => fixture.inboxCalls()).toBeGreaterThan(1);
  await expect(page).toHaveURL(/#\/inbox\?source=drift/);
});

test("Inbox puts its first answer before progressive support at every shell width", async ({ page }) => {
  await installFixtures(page);
  const widths = test.info().project.name === "phone" ? [390] : [768, 1280];
  for (const width of widths) {
    await page.setViewportSize({ width, height: 900 });
    await page.goto("/#/inbox");
    const entry = page.locator(".inbox-entry").first();
    const coverage = page.locator(".inbox-support").first();
    await expect(entry).toBeVisible();
    await expect(coverage).not.toHaveAttribute("open", "");
    const geometry = await page.locator(".inbox-surface").evaluate((surface) => {
      const entry = surface.querySelector<HTMLElement>(".inbox-entry")!;
      const coverage = surface.querySelector<HTMLElement>(".inbox-support")!;
      const batch = surface.querySelectorAll<HTMLElement>(".inbox-support")[1]!;
      return {
        entryTop: entry.getBoundingClientRect().top,
        entryBottom: entry.getBoundingClientRect().bottom,
        viewportHeight: document.documentElement.clientHeight,
        ordered: Boolean(
          entry.compareDocumentPosition(coverage) & Node.DOCUMENT_POSITION_FOLLOWING
          && coverage.compareDocumentPosition(batch) & Node.DOCUMENT_POSITION_FOLLOWING
        ),
        overflow: surface.scrollWidth - surface.clientWidth,
      };
    });
    expect(geometry.ordered).toBe(true);
    expect(geometry.entryTop).toBeGreaterThanOrEqual(0);
    expect(geometry.entryBottom).toBeLessThanOrEqual(geometry.viewportHeight);
    expect(geometry.overflow).toBeLessThanOrEqual(1);
  }
});

test("primary surface headers keep their shared order and geometry across zoom", async ({ page }) => {
  await installFixtures(page, {}, [liveSession]);
  const routes = ["board", "backlog", "inbox", "sessions"] as const;
  const zooms = ["80%", "100%", "125%", "150%", "200%"] as const;
  const widths = test.info().project.name === "phone" ? [390] : [1280, 768];
  const usefulContent = {
    board: ".board-scroll, .board-empty",
    backlog: ".vogt-backlog-listwrap",
    inbox: ".inbox-list",
    sessions: ".sessions-place-body",
  } as const;

  for (const width of widths) {
    await page.setViewportSize({ width, height: 900 });
    for (const routeName of routes) {
      await page.goto(`/#/${routeName}`);
      const header = page.locator("[data-surface-header]:visible");
      await expect(header).toBeVisible();

      const slots = await header.locator(":scope > [data-surface-header-slot]")
        .evaluateAll((elements) => elements.map((element) =>
          element.getAttribute("data-surface-header-slot"),
        ));
      const expected = ["title", "honesty", "spacer", "controls", "action", "detail"]
        .filter((slot) => slots.includes(slot));
      expect(slots).toEqual(expected);

      for (const zoom of zooms) {
        await page.locator("html").evaluate((element, nextZoom) => {
          element.style.zoom = nextZoom;
        }, zoom);
        await expect(header).toBeVisible();
        const geometry = await header.evaluate((element) => {
          const viewportWidth = document.documentElement.clientWidth;
          const viewportHeight = document.documentElement.clientHeight;
          const essential = [...element.querySelectorAll<HTMLElement>(
            '[data-surface-header-slot="honesty"], [data-surface-header-slot="controls"], [data-surface-header-slot="action"]',
          )];
          return {
            route: window.location.hash,
            zoom: document.documentElement.style.zoom,
            documentOverflow: document.documentElement.scrollWidth > viewportWidth + 1,
            headerOverflow: element.scrollWidth > element.clientWidth + 1,
            essentialOffscreen: essential.filter((child) => {
              const box = child.getBoundingClientRect();
              return box.left < -1 || box.right > viewportWidth + 1
                || box.top < -1 || box.top >= viewportHeight - 1
                || box.width === 0;
            }).map((child) => child.dataset.surfaceHeaderSlot),
          };
        });
        expect(geometry.documentOverflow, JSON.stringify(geometry)).toBe(false);
        expect(geometry.headerOverflow, JSON.stringify(geometry)).toBe(false);
        expect(geometry.essentialOffscreen, JSON.stringify(geometry)).toEqual([]);
        await expect(page.locator(usefulContent[routeName])).toBeVisible();
      }
      await page.locator("html").evaluate((element) => { element.style.zoom = "100%"; });
    }
  }
});

test("Phone shell keeps labelled primary navigation and Go to reachability", async ({ page }) => {
  await installFixtures(page);
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/#/sessions");
  await expect(page.getByRole("navigation", { name: "Primary navigation" })).toBeVisible();
  await expect(page.getByRole("link", { name: /Inbox/ })).toBeVisible();
  await expect(page.getByRole("button", { name: "Go to…" })).toBeVisible();
  await page.getByRole("button", { name: "Go to…" }).click();
  const palette = page.getByRole("dialog", { name: "Command palette" });
  await expect(palette).toBeVisible();
  const query = palette.getByRole("combobox", { name: "Search commands" });
  await expect(query).toBeFocused();
  await expect(page.getByText("Open Audit")).toBeVisible();
  await page.keyboard.press("ArrowDown");
  await expect(palette.getByRole("option", { name: /Open Backlog/ })).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await page.keyboard.press("Tab");
  await expect(query).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(page.getByRole("button", { name: "Go to…" })).toBeFocused();

  await page.getByRole("button", { name: "Go to…" }).click();
  await page.getByRole("combobox", { name: "Search commands" }).fill("Open Audit");
  await page.keyboard.press("Enter");
  await expect(page).toHaveURL(/#\/audit$/);
});

test("Places counts expose live workload meaning without overflowing the phone bar", async ({ page }) => {
  const waitingSession = {
    ...liveSession,
    id: "waiting-session",
    name: "needs-attention",
    activity: "waiting-for-input",
  };
  await installFixtures(page, {}, [liveSession, waitingSession], undefined, {
    inboxActive: 12_345,
    projectsTotal: 23_456,
    boardTotal: 34_567,
    backlogTotal: 45_678,
  });
  if (test.info().project.name === "phone") {
    await page.setViewportSize({ width: 320, height: 700 });
  }
  await page.goto("/#/sessions");

  if (test.info().project.name === "phone") {
    const nav = page.getByRole("navigation", { name: "Primary navigation" });
    await expect(nav.getByLabel(/2 sessions/)).toHaveText("2");
    await expect(nav.getByLabel(/12345 active Inbox entries/)).toHaveText("999+");
    await expect(nav.getByLabel(/34567 Board work items/)).toHaveText("999+");
    await expect(nav.getByLabel(/45678 Backlog candidates/)).toHaveText("999+");
    const geometry = await nav.evaluate((element) => ({
      left: element.getBoundingClientRect().left,
      right: element.getBoundingClientRect().right,
      viewport: document.documentElement.clientWidth,
      overflow: element.scrollWidth - element.clientWidth,
    }));
    expect(geometry.left).toBeGreaterThanOrEqual(0);
    expect(geometry.right).toBeLessThanOrEqual(geometry.viewport);
    expect(geometry.overflow).toBeLessThanOrEqual(0);
  } else {
    await expect(page.locator('.places-nav [aria-label="12345 active Inbox entries"]')).toBeVisible();
    await expect(page.locator('.places-nav [aria-label="23456 Projects"]')).toBeVisible();
    await expect(page.locator('.places-nav [aria-label^="1 sessions waiting for input"]')).toBeVisible();
    await expect(page.locator('.places-section-label [aria-label^="2 running sessions"]')).toBeVisible();
    await expect(page.locator(".session-row.waiting")).toContainText("needs-attention");
  }
});

test("Places counts distinguish real zero from an unavailable provider", async ({ page }) => {
  await installFixtures(page, {}, [], undefined, {
    sessionsUnavailable: true,
    inboxActive: 0,
    projectsTotal: null,
    boardTotal: 0,
    backlogTotal: null,
  });
  await page.goto("/#/sessions");

  if (test.info().project.name === "phone") {
    const nav = page.getByRole("navigation", { name: "Primary navigation" });
    await expect(nav.getByLabel("sessions unavailable")).toHaveText("—");
    await expect(nav.getByLabel("0 active Inbox entries")).toHaveText("0");
    await expect(nav.getByLabel("0 Board work items")).toHaveText("0");
    await expect(nav.getByLabel("Backlog candidates unavailable")).toHaveText("—");
  } else {
    await expect(page.locator('.places-nav [aria-label="0 active Inbox entries"]')).toHaveText("0");
    await expect(page.locator('.places-nav [aria-label="Projects unavailable"]')).toHaveText("—");
    await expect(page.locator('.places-nav [aria-label="sessions waiting for input unavailable"]')).toHaveText("—");
    await expect(page.locator('.places-section-label [aria-label="running sessions unavailable"]')).toHaveText("—");
  }
});

test("Desktop session rows open, bookmark, and close entirely from the keyboard", async ({ page }) => {
  test.skip(test.info().project.name === "phone", "The desktop Places rail owns session rows");
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
  await page.goto("/#/sessions");

  const row = page.locator('.session-row[role="link"]', { hasText: "browser-session" });
  await row.focus();
  await expect(row).toBeFocused();
  await page.keyboard.press("Space");
  await expect(page).toHaveURL(/#\/t\/browser-session$/);

  const bookmark = page.getByRole("button", { name: "Bookmark browser-session" });
  await bookmark.focus();
  await page.keyboard.press("Enter");
  await expect(page.getByRole("button", {
    name: "Remove bookmark from browser-session",
  })).toBeFocused();
  await expect(page).toHaveURL(/#\/t\/browser-session$/);

  const close = page.getByRole("button", { name: "Close browser-session" });
  await close.focus();
  await page.keyboard.press("Enter");
  const confirm = page.getByRole("dialog").getByRole("button", { name: "Confirm" });
  await confirm.focus();
  await page.keyboard.press("Enter");
  await expect(row).toHaveCount(0);
});

test("Global shortcut help works outside editable surfaces and returns focus", async ({ page }) => {
  await installFixtures(page);
  await page.goto("/#/sessions");

  const goTo = page.getByRole("button", { name: "Go to…" });
  await goTo.focus();
  await page.keyboard.press("ControlOrMeta+K");
  await expect(
    page.getByRole("dialog", { name: "Command palette" })
      .getByRole("combobox", { name: "Search commands" }),
  ).toBeFocused();
  await page.keyboard.press("Escape");
  await expect(goTo).toBeFocused();

  await page.keyboard.press("?");
  const help = page.getByRole("dialog", { name: "Keyboard Shortcuts" });
  await expect(help).toBeVisible();
  await expect(help.getByText("Outside text fields, editors, and terminals")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(goTo).toBeFocused();

  await page.getByRole("button", { name: "Go to…" }).click();
  const query = page.getByRole("combobox", { name: "Search commands" });
  await query.fill("audit");
  await page.keyboard.press("?");
  await expect(page.getByRole("dialog", { name: "Keyboard Shortcuts" })).toHaveCount(0);
  await expect(query).toHaveValue("audit?");
});

test("Repeated palette opening defers and reuses workspace discovery", async ({ page }) => {
  let manifestRequests = 0;
  await page.route("**/api/search/files**", async (route) => {
    manifestRequests += 1;
    await route.fulfill({ json: [] });
  });
  await installFixtures(page);
  await page.goto("/#/sessions");

  const goTo = page.getByRole("button", { name: "Go to…" });
  await goTo.click();
  const palette = page.getByRole("dialog", { name: "Command palette" });
  await expect(palette.getByText("Open Board", { exact: true })).toBeVisible();
  await page.keyboard.press("Escape");
  await goTo.click();
  await expect(palette.getByText("Open Board", { exact: true })).toBeVisible();
  expect(manifestRequests).toBe(0);

  await palette.locator("input").fill("#workspace");
  await expect.poll(() => manifestRequests).toBe(9);
  await page.keyboard.press("Escape");
  await goTo.click();
  await expect(palette).toBeVisible();
  await page.waitForTimeout(50);
  expect(manifestRequests).toBe(9);
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

test("Files rail keeps its hierarchy compact and exposes real modified-file status", async ({ page }) => {
  test.skip(test.info().project.name === "phone", "The places rail is a desktop surface");
  await installFixtures(page);
  await page.route("**/api/git/status**", async (route) => route.fulfill({ json: {
    repo: "", is_repo: true, branch: "dev", ahead: 0, behind: 0,
    entries: [{
      path: "src/an-identifiable-long-filename.tsx",
      index: " ", worktree: "M", kind: "modified",
    }],
  } }));
  await page.goto("/#/sessions");
  const files = page.getByRole("heading", { name: "Files" });
  const search = page.getByRole("searchbox", { name: "Search files" });
  await expect(files).toBeVisible();
  expect(await files.evaluate((element) => Boolean(
    element.compareDocumentPosition(document.querySelector('.file-tree-search'))
      & Node.DOCUMENT_POSITION_FOLLOWING,
  ))).toBe(true);
  await expect(page.getByText("an-identifiable-long-filename.tsx")).toBeVisible();
  await expect(search).toBeVisible();
  await expect(page.getByLabel("Modified file")).toHaveText("M");
  await expect(page.getByText("TSX", { exact: true })).toHaveCount(0);
  await expect(page.getByRole("button", { name: "New file" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Refresh files" })).toBeVisible();
  await expect(page.getByRole("button", { name: "New folder" })).toHaveCount(0);

  await page.getByRole("button", { name: "More file actions" }).click();
  await expect(page.getByRole("button", { name: "New folder" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Upload files" })).toBeVisible();
  await page.getByRole("button", { name: "More file actions" }).click();

  await page.getByRole("button", { name: "Expand src" }).click();
  await expect(page.getByText("nested-component.tsx")).toBeVisible();
  await expect(page.getByRole("button", { name: "Actions for src", exact: true })).toBeVisible();
  await expect(page.getByRole("button", { name: "Delete" })).toHaveCount(0);
  await page.getByRole("button", { name: "Actions for src", exact: true }).click();
  await expect(page.getByRole("button", { name: "Open terminal" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Delete" })).toBeVisible();
  await expect(page.locator(".places-rail > .file-tree"))
    .toHaveScreenshot("files-rail-nested-modified.png");
});

test("palette file commands open distinct real workflows and cancellation is inert", async ({ page }) => {
  await installFixtures(page);
  let createdPath: string | null = null;
  let manifestRequests = 0;
  const manifests = new Set([
    "package.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "package-lock.json",
    "Cargo.toml",
    "pyproject.toml",
    "Justfile",
    "justfile",
    "Makefile",
  ]);
  await page.route("**/api/search/files**", async (route) => {
    const query = new URL(route.request().url()).searchParams.get("q") ?? "";
    if (manifests.has(query)) manifestRequests += 1;
    return route.fulfill({ json: query === "identifiable" ? [
      { name: "an-identifiable-long-filename.tsx", path: "src/an-identifiable-long-filename.tsx" },
    ] : [] });
  });
  await page.route("**/api/files**", async (route) => {
    if (route.request().method() === "PUT") {
      createdPath = (route.request().postDataJSON() as { path: string }).path;
      return route.fulfill({ json: { ok: true, path: createdPath, bytes: 0 } });
    }
    const path = new URL(route.request().url()).searchParams.get("path") ?? "";
    return route.fulfill({ json: {
      path,
      size: 0,
      content: "",
      content_base64: null,
      is_binary: false,
    } });
  });
  await page.goto("/#/sessions");

  const goTo = page.getByRole("button", { name: "Go to…" });
  await goTo.click();
  const palette = page.getByRole("dialog", { name: "Command palette" });
  await palette.getByRole("combobox", { name: "Search commands" }).fill("#workspace");
  await expect.poll(() => manifestRequests).toBe(9);
  await page.keyboard.press("Escape");

  await goTo.click();
  await palette
    .getByText("New File", { exact: true })
    .click();
  const create = page.getByRole("dialog", { name: "New file" });
  await expect(create.getByLabel("Destination folder")).toBeVisible();
  await expect(create.getByLabel("Filename")).toBeVisible();
  await create.getByRole("button", { name: "Cancel" }).click();
  await expect(goTo).toBeFocused();
  expect(createdPath).toBeNull();
  expect(manifestRequests).toBe(9);

  await goTo.click();
  await palette
    .getByText("Open File...", { exact: true })
    .click();
  const chooser = page.getByRole("dialog", { name: "Open file" });
  await expect(chooser.getByLabel("Search workspace files")).toBeFocused();
  await chooser.getByLabel("Search workspace files").fill("identifiable");
  const existing = chooser.getByRole("button", {
    name: "an-identifiable-long-filename.tsx — src/an-identifiable-long-filename.tsx",
  });
  await expect(existing).toBeVisible();
  await existing.click();
  await expect(page).toHaveURL(/#\/e\/src%2Fan-identifiable-long-filename\.tsx$/);
  expect(manifestRequests).toBe(9);

  await goTo.click();
  await palette
    .getByText("New File", { exact: true })
    .click();
  const createAgain = page.getByRole("dialog", { name: "New file" });
  await createAgain.getByLabel("Destination folder").fill("notes");
  await createAgain.getByLabel("Filename").fill("palette.md");
  await expect(createAgain.getByText("Create notes/palette.md")).toBeVisible();
  await createAgain.getByRole("button", { name: "Create file" }).click();
  await expect.poll(() => createdPath).toBe("notes/palette.md");
  await expect(page).toHaveURL(/#\/e\/notes%2Fpalette\.md$/);

  await goTo.click();
  await palette.getByRole("combobox", { name: "Search commands" }).fill("#workspace");
  await expect.poll(() => manifestRequests).toBe(18);
});

test("Phone editor keeps the compact Files hierarchy and progressive controls usable", async ({ page }) => {
  test.skip(test.info().project.name !== "phone", "Phone geometry is validated in the phone browser project");
  await installFixtures(page);
  await page.addInitScript(() => {
    localStorage.setItem("mydevenv2.layoutMode.v1", "ide");
  });
  await page.route("**/api/files**", async (route) => route.fulfill({ json: {
    path: "src/an-identifiable-long-filename.tsx",
    size: 26,
    content: "export const answer = 42;\n",
    content_base64: null,
    is_binary: false,
  } }));
  await page.goto("/#/e/src%2Fan-identifiable-long-filename.tsx");

  const fileTree = page.locator(".editor-sidebar .file-tree");
  await expect(fileTree).toBeVisible();
  await expect(fileTree.getByRole("heading", { name: "Files" })).toBeVisible();
  await expect(fileTree.getByRole("searchbox", { name: "Search files" })).toBeVisible();
  await expect(fileTree.getByRole("button", { name: "New file" })).toBeVisible();
  await fileTree.getByRole("button", { name: "More file actions" }).tap();
  await expect(fileTree.getByRole("button", { name: "New folder" })).toBeVisible();
  await expect(fileTree.getByRole("button", { name: "Upload files" })).toBeVisible();
});

for (const height of [700, 900]) {
  test(`Crowded desktop rail keeps Files and its footer reachable at ${height}px`, async ({ page }) => {
    test.skip(test.info().project.name === "phone", "The places rail is a desktop surface");
    const crowdedSessions = Array.from({ length: 44 }, (_, index) => ({
      ...liveSession,
      id: `crowded-${index + 1}`,
      name: `Crowded session ${String(index + 1).padStart(2, "0")}`,
    }));
    await installFixtures(page, {}, crowdedSessions);
    await page.setViewportSize({ width: 1280, height });
    await page.goto("/#/sessions");

    const rail = page.getByRole("complementary", { name: "Places" });
    const files = page.getByRole("heading", { name: "Files" });
    const search = page.getByRole("searchbox", { name: "Search files" });
    const row = page.getByText("an-identifiable-long-filename.tsx");
    const settings = page.getByRole("button", { name: "Settings" });
    const metrics = await rail.evaluate((element) => ({
      overflowY: getComputedStyle(element).overflowY,
      scrollHeight: element.scrollHeight,
      clientHeight: element.clientHeight,
      nestedOverflow: [
        ".places-nav",
        ".places-rail-session-area",
        ".places-recent",
        ".file-tree .tree-scroll",
      ].map((selector) => getComputedStyle(element.querySelector(selector)!).overflowY),
    }));
    expect(metrics.overflowY).toBe("auto");
    expect(metrics.scrollHeight).toBeGreaterThan(metrics.clientHeight);
    expect(metrics.nestedOverflow).not.toContain("auto");
    expect(metrics.nestedOverflow).not.toContain("scroll");

    await settings.focus();
    await expect(settings).toBeFocused();
    await expect(settings).toBeInViewport();
    await expect(files).toBeInViewport();
    await expect(search).toBeInViewport();
    await expect(row).toBeInViewport();
    await page.getByRole("link", { name: "Board" }).focus();
    await expect(page.getByRole("link", { name: "Board" })).toBeInViewport();
    await page.getByRole("button", { name: "New file" }).focus();
    await expect(page.getByRole("button", { name: "New file" })).toBeInViewport();
    const fileHeight = await page.locator(".file-tree").evaluate((element) =>
      element.getBoundingClientRect().height,
    );
    expect(fileHeight).toBeGreaterThanOrEqual(259);
  });
}

test("Route truth owns unavailable links, current navigation and Settings return", async ({ page }) => {
  await installFixtures(page);
  await page.goto("/#/board?project=vogt");

  const phone = test.info().project.name === "phone";
  const currentNavigation = phone ? page.locator('.phone-bottom-nav a[aria-current="page"]') : page.locator('.places-nav a[aria-current="page"]');
  await expect(currentNavigation).toContainText("Board");
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
  await expect(currentNavigation).toContainText("Board");
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
    await expect(page.locator('.places-nav a[aria-current="page"]')).toContainText("History");
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

test("dirty Agent Task drafts guard navigation and browser exit", async ({ page }) => {
  await installFixtures(page);
  const task = {
    id: "browser-task",
    name: "Browser task",
    prompt: "Original prompt",
    schedule: { kind: "manual" },
    status: "active",
    command: null,
    cwd: null,
    env: [],
    context: null,
    notify_on_start: false,
    notify_on_phrase: null,
    auto_retry_on_rate_limit: true,
    next_run: null,
    last_run: null,
    run_count: 0,
    runs: [],
    created_at: "2026-08-18T00:00:00Z",
    updated_at: "2026-08-18T00:00:00Z",
  };
  await page.route("**/api/agent-tasks", async (route) =>
    route.fulfill({ json: [task] }),
  );

  await page.goto("/#/tasks");
  const name = page.getByRole("textbox", { name: "Name", exact: true });
  await expect(name).toHaveValue("Browser task");
  await name.fill("Protected browser draft");
  await expect(page.getByText("Unsaved draft")).toBeVisible();

  expect(await page.evaluate(() => {
    const event = new Event("beforeunload", { cancelable: true });
    window.dispatchEvent(event);
    return event.defaultPrevented;
  })).toBe(true);

  await page.getByRole("link", { name: /Board/ }).click();
  await expect(page.getByRole("dialog", {
    name: "Leave Agent Tasks with an unsaved draft?",
  })).toBeVisible();
  await page.getByRole("button", { name: "Stay here" }).click();
  await expect(page).toHaveURL(/#\/tasks$/);
  await expect(name).toHaveValue("Protected browser draft");

  await page.locator("body").dispatchEvent("keydown", {
    key: "w",
    ctrlKey: true,
    shiftKey: true,
  });
  await expect(page.getByRole("dialog", {
    name: "Save task draft before continuing?",
  })).toBeVisible();
  await page.getByRole("button", { name: "Stay here" }).click();
  await expect(name).toHaveValue("Protected browser draft");

  await page.getByRole("link", { name: /Board/ }).click();
  await page.getByRole("button", { name: "Discard draft", exact: true }).click();
  await expect(page).toHaveURL(/#\/board$/);
  await expect.poll(() => page.evaluate(() => {
    const event = new Event("beforeunload", { cancelable: true });
    window.dispatchEvent(event);
    return event.defaultPrevented;
  })).toBe(false);
});
