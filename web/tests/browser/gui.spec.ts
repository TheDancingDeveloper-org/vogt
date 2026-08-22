import { expect, test, type Locator, type Page } from "@playwright/test";

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
  boardItems?: Record<string, unknown>[];
  /** What a session's scrollback says, for the waiting-session card. */
  scrollback?: string;
  /** Items per board cell page; 0 or absent serves the whole cell at once. */
  boardPageSize?: number;
  backlogItems?: Record<string, unknown>[];
  /** The workflows the board reads its columns and legal edges from. */
  workflows?: Record<string, unknown>[];
  /** The project registry, so a test can seed more than one swimlane. */
  projects?: { slug: string; name: string; root_path?: string }[];
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
  const boardRequests: Record<string, unknown>[] = [];
  const transitionRequests: Record<string, unknown>[] = [];
  const sessionInputs: { id: string; text: string; submit: boolean }[] = [];
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
  await page.route("**/api/sessions/*/input", async (route) => {
    const id = new URL(route.request().url()).pathname.split("/").at(-2) ?? "";
    const body = route.request().postDataJSON() as { text: string; submit: boolean };
    sessionInputs.push({ id, ...body });
    return route.fulfill({ json: { ok: true } });
  });
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
    if (route.request().method() === "GET") {
      const id = new URL(route.request().url()).pathname.split("/").at(-1);
      const session = sessions.find((one) => one.id === id);
      if (!session) return route.fulfill({ status: 404, json: { error: "not found" } });
      return route.fulfill({ json: {
        ...session,
        scrollback_base64: btoa(metrics.scrollback ?? "Apply this migration? (y/n) "),
      } });
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
  // The shell's places rail and assistant poll the engine on mount. Left
  // unstubbed they reach whatever backend sits behind the dev proxy, and a
  // 401 from it flips the whole app to the login gate mid-test — so answer
  // them with benign, empty engine state.
  await page.route("**/api/git/status**", async (route) => route.fulfill({ json: {
    repo: "", is_repo: false, branch: "", ahead: 0, behind: 0, entries: [],
  } }));
  await page.route("**/api/assistant/history**", async (route) => route.fulfill({ json: {
    transcript: [],
  } }));
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
      return route.fulfill({ json: { workflows: metrics.workflows ?? [{ kind: "feature", initial_state: "open", states: ["open", "done"], transitions: { open: ["done"], done: [] } }] } });
    }
    if (url.pathname.endsWith("/work/transition")) {
      const body = request.postDataJSON() as { ref: string; to_state: string; reason: string };
      transitionRequests.push(body as Record<string, unknown>);
      const source = (metrics.boardItems ?? boardItems).find((item) => item.ref === body.ref) ?? boardItems[0];
      return route.fulfill({ json: {
        item: { ...source, ref: body.ref, state: body.to_state, updated_at: "2026-08-17T11:00:00Z" },
      } });
    }
    if (url.pathname.endsWith("/board/list")) {
      if (metrics.boardTotal === null) return route.fulfill({ status: 503, body: "work unavailable" });
      const body = request.postDataJSON() as {
        cells?: { lane_key?: string; state?: string; cursor?: string }[];
        lane_mode?: string;
      };
      boardRequests.push(body as Record<string, unknown>);
      const fixtureItems = metrics.boardItems ?? boardItems;
      const page = metrics.boardPageSize ?? 0;
      // Which swimlane an item belongs to, so a lane's cell carries only its
      // own cards (#216 needs two lanes that do not share their contents).
      const laneMode = body.lane_mode ?? "none";
      const laneOf = (item: Record<string, unknown>) =>
        laneMode === "project"
          ? String(item.project_slug ?? "")
          : laneMode === "initiative"
            ? String(item.initiative_id ?? "")
            : "";
      const cells = (body.cells ?? []).map((cell) => {
        const all = fixtureItems.filter(
          (item) =>
            item.state === cell.state && laneOf(item) === (cell.lane_key ?? ""),
        );
        // A bounded cell: the first page carries a cursor, and the request
        // that returns with it gets the rest (NFR-S5, #63).
        const items = page > 0
          ? (cell.cursor ? all.slice(page) : all.slice(0, page))
          : all;
        return {
          lane_key: cell.lane_key ?? "",
          state: cell.state ?? "",
          items,
          total: all.length,
          next_cursor: page > 0 && !cell.cursor && all.length > page ? "cursor-1" : null,
        };
      });
      const columnTotals: Record<string, number> = {};
      const laneTotals: Record<string, number> = {};
      for (const item of fixtureItems) {
        const state = String(item.state ?? "");
        columnTotals[state] = (columnTotals[state] ?? 0) + 1;
        const lane = laneOf(item);
        laneTotals[lane] = (laneTotals[lane] ?? 0) + 1;
      }
      return route.fulfill({ json: {
        cells,
        column_totals: columnTotals,
        lane_totals: laneTotals,
        total: metrics.boardTotal,
        snapshot: "browser-board-snapshot",
        snapshot_at: "2026-08-17T10:01:00Z",
        revision: 1,
      } });
    }
    if (url.pathname.endsWith("/work")) {
      if (metrics.boardTotal === null) return route.fulfill({ status: 503, body: "work unavailable" });
      return route.fulfill({ json: { items: boardItems, total: metrics.boardTotal, freshness: { status: "fresh" } } });
    }
    if (url.pathname.endsWith("/backlog")) {
      if (metrics.backlogTotal === null) return route.fulfill({ status: 503, body: "backlog unavailable" });
      return route.fulfill({ json: {
        items: metrics.backlogItems ?? boardItems,
        total_considered: metrics.backlogTotal,
        freshness: { status: "fresh", collectors: {} },
      } });
    }
    if (url.pathname.endsWith("/projects")) {
      if (metrics.projectsTotal === null) return route.fulfill({ status: 503, body: "projects unavailable" });
      const projects = metrics.projects ?? [{ slug: "vogt", name: "Vogt", root_path: "/workspace/vogt" }];
      return route.fulfill({ json: {
      projects,
      total: metrics.projectsTotal,
      } });
    }
    if (url.pathname.endsWith("/vogt/events")) {
      return route.fulfill({ json: { events: [], last_id: 0 } });
    }
    if (url.pathname.endsWith("/vogt/audit")) {
      return route.fulfill({ json: { records: [], total: 0 } });
    }
    if (url.pathname.endsWith("/work/get")) {
      return route.fulfill({ json: {
        item: {
          id: "01JWORKITEM", ref: "WI-7", kind: "feature", title: "Measured board card",
          body: "", state: "open", priority: "normal", project_slug: "vogt",
          initiative_id: null, origin: "declared", trust_state: "verified",
          assignee_identity_ref: null, labels: [], relations: [],
          created_at: "2026-08-01T00:00:00Z", updated_at: "2026-08-17T10:00:00Z",
        },
        comments: [],
        sessions: [],
        audit: [],
      } });
    }
    if (url.pathname.endsWith("/why")) {
      return route.fulfill({ json: {
        ref: "WI-7",
        title: "Ranked",
        total: 1,
        contributions: [
          { input: "age", detail: "raised 9 days ago and still open", value: 9, weight: 0.1, contribution: 0.9 },
        ],
        inputs_not_yet_available: {},
      } });
    }
    if (url.pathname.endsWith("/labels")) return route.fulfill({ json: { labels: [] } });
    if (url.pathname.endsWith("/initiatives")) return route.fulfill({ json: { initiatives: [] } });
    if (url.pathname.endsWith("/actors")) return route.fulfill({ json: { actors: [] } });
    if (url.pathname.endsWith("/vogt/observations")) {
      return route.fulfill({ json: { observations: [], total: 0 } });
    }
    if (url.pathname.endsWith("/vogt/sessions")) {
      return route.fulfill({ json: { sessions: [], engine: null } });
    }
    return route.fulfill({ json: {} });
  });
  return { inboxCalls: () => inboxCalls, boardRequests, transitionRequests, sessions, sessionInputs };
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
  await expect(
    page.getByRole("group", { name: "Board filters", exact: true }),
  ).toBeVisible();
});

// #214: a coarse pointer cannot drag and cannot see the Shift+Arrow hint, so
// the "Move…" control is its only way to move a card. It opens the same
// composer a drag does, with a state select in place of the drop cell a tap
// never lands on — and the move it commits is an ordinary `work.transition`.
test("Phone Board moves a card by tapping Move… and choosing a state", async ({ page }) => {
  test.skip(test.info().project.name !== "phone", "The touch move control is the coarse-pointer path (#214)");
  const fixture = await installFixtures(page, {}, [], undefined, {
    boardItems: [{ ...boardItems[0], ref: "WI-7", title: "Phone move card", state: "open" }],
    workflows: [{
      kind: "feature",
      initial_state: "open",
      states: ["open", "in_progress", "done"],
      transitions: { open: ["in_progress", "done"], in_progress: ["done"], done: [] },
    }],
  });
  await page.goto("/#/board?project=vogt");
  await expect(page.getByRole("heading", { name: "Board" })).toBeVisible();
  const card = page.locator(".board-card").filter({ hasText: "Phone move card" });
  await expect(card).toBeVisible();

  // The control is revealed only by `pointer: coarse`, which is the mobile
  // emulation's reported pointer — a desk browser never shows it.
  const move = card.getByRole("button", { name: "Move WI-7" });
  await expect(move).toBeVisible();
  // A tap target with real height, not a bare text link.
  const moveBox = await move.boundingBox();
  expect(moveBox!.height).toBeGreaterThanOrEqual(24);

  await move.click();
  // The composer opens in the card's own column — the one a phone can see —
  // rather than in a target column off screen.
  const composer = page.locator(".board-composer");
  await expect(composer).toBeVisible();
  const select = composer.locator(".board-composer-state select");
  await expect(select).toBeVisible();
  // It offers exactly the workflow's listed edges from `open`, in order.
  await expect(select.locator("option")).toHaveText(["in progress", "done"]);
  // And it stays inside the phone viewport instead of overflowing it.
  const selectBox = await select.boundingBox();
  const viewport = page.viewportSize()!;
  expect(selectBox!.x).toBeGreaterThanOrEqual(0);
  expect(selectBox!.x + selectBox!.width).toBeLessThanOrEqual(viewport.width + 1);

  // The open composer is a new visual on the phone; keep a reviewed baseline.
  await expect(composer).toHaveScreenshot("board-move-composer.png");

  // Choose a state that is not the default first edge, give the reason the
  // write requires, and confirm.
  await select.selectOption("done");
  await composer.locator("textarea").fill("closing it from my phone");
  await composer.getByRole("button", { name: "Move", exact: true }).click();

  // Exactly the chosen transition was sent — no drag, no drop cell — and the
  // board says the move landed.
  await expect.poll(() => fixture.transitionRequests).toEqual([
    { ref: "WI-7", to_state: "done", reason: "closing it from my phone" },
  ]);
  await expect(page.getByText("WI-7 moved to done.")).toBeVisible();
});

// #216: dropping a card into another lane of its own column changes only the
// swimlane, which a drag cannot do. The cell is never offered as a target and,
// when a drop lands there anyway, it says why rather than doing nothing.
test("Board explains a same-column, other-lane drop instead of swallowing it", async ({ page }) => {
  test.skip(test.info().project.name === "phone", "The lane-drop hint is a desktop drag gesture (#216)");
  const fixture = await installFixtures(page, {}, [], undefined, {
    boardTotal: 2,
    projects: [
      { slug: "vogt", name: "Vogt", root_path: "/workspace/vogt" },
      { slug: "beta", name: "Beta", root_path: "/workspace/beta" },
    ],
    boardItems: [
      { ...boardItems[0], ref: "WI-7", title: "Vogt lane card", state: "open", project_slug: "vogt" },
      { ...boardItems[0], ref: "WI-8", title: "Beta lane card", state: "open", project_slug: "beta" },
    ],
  });
  await page.goto("/#/board?lanes=project");
  await expect(page.getByRole("heading", { name: "Board" })).toBeVisible();
  const vogtCard = page.locator(".board-card").filter({ hasText: "Vogt lane card" });
  const betaCard = page.locator(".board-card").filter({ hasText: "Beta lane card" });
  await expect(vogtCard).toBeVisible();
  await expect(betaCard).toBeVisible();

  // Two swimlanes are two rows: the cards share the `open` column but sit at
  // different heights, which is the arrangement the drop is about to conflate.
  const vogtBox = await vogtCard.boundingBox();
  const betaBox = await betaCard.boundingBox();
  expect(Math.abs(vogtBox!.y - betaBox!.y)).toBeGreaterThan(20);

  // The Beta lane's own `open` cell — where WI-8 lives — differs from the
  // dragged card's source only by lane.
  const betaOpen = page.locator('.board-cell[data-state="open"]', {
    has: page.getByText("Beta lane card"),
  });
  // A native HTML5 drag with one shared DataTransfer, dispatched on the real
  // nodes: the board's move is a drag/drop, and this is the gesture a reader
  // makes — carried through to the `drop` the cell handles.
  await page.evaluate(() => {
    const cardOf = (text: string) =>
      [...document.querySelectorAll<HTMLElement>(".board-card")].find((node) =>
        node.textContent?.includes(text),
      )!;
    const source = cardOf("Vogt lane card");
    const target = cardOf("Beta lane card").closest<HTMLElement>(".board-cell")!;
    const dataTransfer = new DataTransfer();
    source.dispatchEvent(new DragEvent("dragstart", { bubbles: true, dataTransfer }));
    target.dispatchEvent(new DragEvent("dragover", { bubbles: true, dataTransfer }));
    target.dispatchEvent(new DragEvent("drop", { bubbles: true, dataTransfer }));
    source.dispatchEvent(new DragEvent("dragend", { bubbles: true, dataTransfer }));
  });

  // The drop is a no-op with the reason left on screen: no composer opened and
  // no transition was requested.
  await expect(
    betaOpen.getByText(
      "Lanes group cards; to change the project/initiative open the item",
    ),
  ).toBeVisible();
  await expect(betaOpen.locator(".board-composer")).toHaveCount(0);
  expect(fixture.transitionRequests).toEqual([]);
  // The card never left its lane.
  await expect(betaOpen.filter({ has: page.getByText("Vogt lane card") })).toHaveCount(0);
});

/**
 * A narrow shell keeps the saved lenses inside the `+ Filter` disclosure, so
 * the first screen belongs to the work. Anything reaching for a lens control
 * has to open it there first; on a desk it is already beside the chips.
 */
async function openFilterPanelOnPhone(group: Locator): Promise<void> {
  if (test.info().project.name !== "phone") return;
  const add = group.getByRole("button", { name: "+ Filter", exact: true });
  if ((await add.getAttribute("aria-expanded")) === "false") await add.click();
}

test("Board progressive filters survive reload, history, and saved-lens recall", async ({ page }) => {
  await installFixtures(page);
  await page.goto("/#/board?project=vogt&lanes=project");
  await expect(page.getByRole("heading", { name: "Board" })).toBeVisible();

  const filters = page.getByRole("group", { name: "Board filters" });
  await expect(filters.getByText("Project: vogt")).toBeVisible();
  await expect(filters.getByText("Swimlanes: project")).toBeVisible();
  await expect(page.getByRole("group", { name: "Add Board filters" })).toBeHidden();

  await filters.getByRole("button", { name: "+ Filter", exact: true }).click();
  const addPanel = page.getByRole("group", { name: "Add Board filters" });
  await expect(addPanel).toBeVisible();
  await addPanel.getByText("Type", { exact: true }).click();
  await addPanel.getByRole("button", { name: "feature", exact: true }).click();
  await expect(page).toHaveURL(/kind=feature/);

  await page.getByLabel("Lens name").fill("Vogt features");
  await page.getByRole("button", { name: "Save lens" }).click();
  await expect(page.locator(".board-saved-recall")).toHaveText("Vogt features");

  await filters.getByRole("button", { name: "Remove filter Project: vogt" }).click();
  await expect(page).not.toHaveURL(/project=vogt/);
  await page.reload();
  await expect(filters.getByText("Type: feature")).toBeVisible();
  await expect(filters.getByText("Swimlanes: project")).toBeVisible();

  await openFilterPanelOnPhone(filters);
  await page.locator(".board-saved-recall").click();
  await expect(page).toHaveURL(/project=vogt/);
  await expect(page).toHaveURL(/kind=feature/);
  await expect(page).toHaveURL(/lanes=project/);

  await page.goto("/#/board?label=infra");
  await expect(filters.getByText("Label: infra")).toBeVisible();
  await page.goBack();
  await expect(filters.getByText("Project: vogt")).toBeVisible();
  await expect(filters.getByText("Type: feature")).toBeVisible();
  await page.goForward();
  await expect(filters.getByText("Label: infra")).toBeVisible();
});

test("Phone Board renders one URL-selected workflow state without widening the server filter", async ({ page }) => {
  test.skip(test.info().project.name !== "phone", "Phone state composition is a narrow-shell contract");
  const openItem = {
    ...boardItems[0],
    ref: "WI-OPEN",
    title: "Open phone card",
    state: "open",
  };
  const doneItem = {
    ...boardItems[0],
    ref: "WI-DONE",
    title: "Done phone card",
    state: "done",
  };
  const fixture = await installFixtures(page, {}, [], undefined, {
    boardTotal: 2,
    boardItems: [openItem, doneItem],
  });

  await page.goto("/#/board?project=vogt&column=done");
  const states = page.getByRole("group", { name: "Board workflow state" });
  await expect(states).toBeVisible();
  await expect(states.getByRole("button", { name: /open 1/i })).toBeVisible();
  await expect(states.getByRole("button", { name: /done 1/i })).toHaveAttribute(
    "aria-pressed",
    "true",
  );
  await expect(page.getByText("Done phone card")).toBeVisible();
  await expect(page.getByText("Open phone card")).toHaveCount(0);
  expect(fixture.boardRequests.at(-1)).not.toHaveProperty("states");
  expect(fixture.boardRequests.at(-1)?.cells).toEqual([
    { lane_key: "", state: "done" },
  ]);

  await states.getByRole("button", { name: /open 1/i }).click();
  await expect(page).toHaveURL(/column=open/);
  await expect(page.getByText("Open phone card")).toBeVisible();
  await expect(page.getByText("Done phone card")).toHaveCount(0);
  expect(fixture.boardRequests.at(-1)?.cells).toEqual([
    { lane_key: "", state: "open" },
  ]);
  expect(fixture.boardRequests.at(-1)?.project).toEqual("vogt");

  // Selecting a state rewrites the board's own address rather than stacking
  // history entries, the same way its filter chips do. What has to survive is
  // the address: reload it, and arrive at it by back and forward.
  await page.reload();
  await expect(page.getByText("Open phone card")).toBeVisible();
  await expect(states.getByRole("button", { name: /open 1/i })).toHaveAttribute(
    "aria-pressed",
    "true",
  );

  await page.goto("/#/board?project=vogt&column=done");
  await expect(page.getByText("Done phone card")).toBeVisible();
  await page.goBack();
  await expect(page).toHaveURL(/column=open/);
  await expect(page.getByText("Open phone card")).toBeVisible();
  await expect(page.getByText("Done phone card")).toHaveCount(0);
  await page.goForward();
  await expect(page).toHaveURL(/column=done/);
  await expect(page.getByText("Done phone card")).toBeVisible();
});

test("Board cards expand measured title and body content in place", async ({ page }) => {
  const longTitle = "A long Board title that must remain readable after expansion ".repeat(8);
  const longBody = "The expanded body stays in the measured card and never moves to a modal.";
  await installFixtures(page, {}, [], undefined, {
    boardItems: [{ ...boardItems[0], title: longTitle, body: longBody }],
  });
  await page.goto("/#/board");

  const card = page.locator(".board-card").filter({ hasText: "A long Board title" });
  const title = card.locator(".board-card-title");
  const before = await card.evaluate((element) => element.getBoundingClientRect().height);
  await expect(card.getByRole("button", { name: "Show more" })).toBeVisible();
  await card.getByRole("button", { name: "Show more" }).focus();
  await page.keyboard.press("Enter");
  await expect(card.getByText(longBody)).toBeVisible();
  await expect(title).toHaveClass(/expanded/);
  const expanded = await card.evaluate((element) => element.getBoundingClientRect().height);
  expect(expanded).toBeGreaterThan(before);
  await expect(page.getByRole("dialog")).toHaveCount(0);

  // Collapse the same way the reader reached the control: by pointer where the
  // board has room for one, and by keyboard everywhere. Phone containment of
  // the surrounding controls is tracked separately as first-viewport work.
  const collapse = card.getByRole("button", { name: "Show less" });
  if (test.info().project.name === "desktop") {
    await collapse.click();
  } else {
    await collapse.focus();
    await page.keyboard.press("Enter");
  }
  await expect(card.getByText(longBody)).toHaveCount(0);
  await expect(title).not.toHaveClass(/expanded/);
  await expect(card.getByRole("button", { name: "Show more" })).toBeFocused();
});

test("Vogt identity and route-aware titles survive navigation and reload", async ({ page }) => {
  await installFixtures(page, { assistant_enabled: true });
  await page.goto("/#/board?project=vogt");
  await expect(page).toHaveTitle("Board · Vogt");
  await expect(page.locator(".places-brand")).toContainText("Vogt");

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
  test.skip(test.info().project.name === "phone", "Phone Inbox uses the bottom action sheet");
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

test("Phone Inbox uses source pills and a focus-safe bottom action sheet", async ({ page }) => {
  test.skip(test.info().project.name !== "phone", "Bottom-sheet mechanics are validated in the phone project");
  await installFixtures(page);
  await page.goto("/#/inbox");

  const pills = page.getByRole("group", { name: "Source filter" });
  await expect(pills).toBeVisible();
  await expect(page.locator(".inbox-filter select")).toHaveCount(0);
  await pills.getByRole("button", { name: "drift" }).click();
  await expect(page).toHaveURL(/#\/inbox\?source=drift$/);
  await expect(page.getByRole("region", { name: "Drift evidence" })).toContainText("observed_state");

  const entryTitle = page.locator(".inbox-entry h2").first();
  const entrySummary = page.locator(".inbox-entry-summary").first();
  const closedTitleWidth = (await entryTitle.boundingBox())!.width;
  const closedSummaryWidth = (await entrySummary.boundingBox())!.width;

  const trigger = page.getByRole("button", { name: "Inbox actions" });
  await trigger.click();
  const sheet = page.getByRole("dialog", { name: `Actions for ${inboxEntry.title}` });
  await expect(sheet).toBeVisible();
  const targetHeights = await sheet.locator("button").evaluateAll((buttons) =>
    buttons.map((button) => Math.round(button.getBoundingClientRect().height)),
  );
  expect(targetHeights.every((height) => height >= 52)).toBe(true);
  await expect(sheet.getByRole("button", { name: "Reject proposed change…" })).toBeVisible();
  await expect(sheet.getByPlaceholder("Why this triage decision?")).toHaveCount(0);

  // #219: backdropClass replaces (rather than augments) the Dialog's default
  // "modal-backdrop" class, so the action sheet's own backdrop rule has to be
  // a fully self-contained fixed overlay or the "sheet" just flows inline.
  const backdrop = page.locator(".inbox-action-sheet-backdrop");
  const backdropStyle = await backdrop.evaluate((element) => {
    const style = getComputedStyle(element);
    return { position: style.position, alignItems: style.alignItems, display: style.display };
  });
  expect(backdropStyle.position).toBe("fixed");
  expect(backdropStyle.alignItems).toBe("flex-end");
  expect(backdropStyle.display).toBe("flex");

  const viewport = page.viewportSize()!;
  const backdropBox = (await backdrop.boundingBox())!;
  expect(Math.round(backdropBox.x)).toBe(0);
  expect(Math.round(backdropBox.y)).toBe(0);
  expect(Math.round(backdropBox.width)).toBe(viewport.width);
  expect(Math.round(backdropBox.height)).toBe(viewport.height);

  const sheetBox = (await sheet.boundingBox())!;
  expect(Math.round(sheetBox.y + sheetBox.height)).toBeCloseTo(viewport.height, -1);

  // The entry behind the sheet must keep its full-width layout: a backdrop
  // that isn't taken out of document flow squeezes the entry instead of
  // covering it, which is the exact regression #219 reported.
  const openTitleWidth = (await entryTitle.boundingBox())!.width;
  const openSummaryWidth = (await entrySummary.boundingBox())!.width;
  expect(Math.round(openTitleWidth)).toBe(Math.round(closedTitleWidth));
  expect(Math.round(openSummaryWidth)).toBe(Math.round(closedSummaryWidth));

  await expect(sheet).toHaveScreenshot("inbox-action-sheet-phone.png");

  await sheet.getByRole("button", { name: "Reject proposed change…" }).click();
  await sheet.getByPlaceholder("Why this triage decision?").fill("the observed evidence was reviewed");
  await page.keyboard.press("Escape");
  await expect(sheet).toHaveCount(0);
  await expect(trigger).toBeFocused();
  expect(page.url()).toContain("#/inbox?source=drift");
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

      // A narrow shell folds the controls and the detail behind one
      // disclosure so the surface's own work owns the first screen. The
      // contract below is what has to hold once they are shown, so they are
      // shown; that the fold exists at all is asserted where it belongs, with
      // the rest of the phone's first-viewport composition.
      const more = header.locator(".surface-header-more");
      if (await more.count()) {
        if ((await more.getAttribute("aria-expanded")) === "false") await more.click();
      }

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
    // The desktop Sessions overview also lists this session as a row (#233), so
    // scope to the rail's own highlight of the one waiting for input.
    await expect(page.locator(".places-rail-session-area .session-row.waiting")).toContainText("needs-attention");
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
  // The Live Sessions sub-panel was removed (#167); the running session is
  // listed in the places rail instead.

  await page.goto("/#/history");
  await expect(page.locator(".history-view")).toBeVisible();
  await expect(page.locator('[data-tab-kind="terminal"]')).toHaveCount(1);

  await page.goto("/#/tasks");
  await expect(page.locator(".agent-tasks-view")).toBeVisible();
  await expect(page.locator('[data-tab-kind="terminal"]')).toHaveCount(1);
  await expect(page.locator('[data-tab-kind="history"]')).toHaveCount(0);
  await expect(page.locator('[data-tab-kind="tasks"]')).toHaveCount(1);
});

// Three sessions in the three states the Sessions shell has to keep reachable
// and legible at once: an idle one, one waiting for input, one running (#232,
// #233, #231's reachability half).
const THREE_SESSIONS = [
  {
    id: "sess-idle", name: "idle-shell", cwd: "/workspace/vogt",
    activity: "idle", exit_code: null, scrollback_bytes: 1024,
    created_at: "2026-08-18T08:00:00Z", activity_changed_at: "2026-08-18T08:00:00Z",
  },
  {
    id: "sess-wait", name: "needs-answer", cwd: "/workspace/api",
    activity: "waiting-for-input", exit_code: null, scrollback_bytes: 1024,
    created_at: "2026-08-18T08:00:00Z", activity_changed_at: "2026-08-18T08:00:30Z",
  },
  {
    id: "sess-busy", name: "running-build", cwd: "/workspace/web",
    activity: "running", exit_code: null, scrollback_bytes: 1024,
    created_at: "2026-08-18T08:00:00Z", activity_changed_at: "2026-08-18T08:00:10Z",
  },
];

/** Answer a terminal's attach socket the way the split tests do, so a `/t/:id`
 *  route renders its layout rather than hanging on the WebSocket. */
async function stubTerminalAttach(page: Page): Promise<void> {
  await page.routeWebSocket(/\/api\/sessions\/[^/]+\/attach$/, (socket) => {
    socket.onMessage(() => undefined);
    socket.send(JSON.stringify({ type: "snapshot-start", scrollback_bytes: 0, scrollback_pos: 0 }));
    socket.send(JSON.stringify({ type: "snapshot-done" }));
  });
}

test("Phone Sessions shell keeps the terminal on screen and folds the header (#232)", async ({ page }) => {
  test.skip(test.info().project.name !== "phone", "The collapsed shell is the narrow client's");
  await installFixtures(page, {}, THREE_SESSIONS);
  await stubTerminalAttach(page);

  await page.goto("/#/t/sess-idle");
  await expect(page.locator(".terminal-host").first()).toBeVisible();

  // #232: the xterm had measured ~2px tall behind ~560px of header. The 40dvh
  // floor on `.terminal-layout` is what keeps it on screen.
  const geometry = await page.evaluate(() => {
    const layout = document.querySelector(".terminal-layout");
    const host = document.querySelector(".terminal-host");
    return {
      layout: layout ? layout.getBoundingClientRect().height : 0,
      host: host ? host.getBoundingClientRect().height : 0,
      viewport: window.innerHeight,
    };
  });
  // The 1px tolerance is subpixel: 40dvh of a 664px viewport is 265.6, and the
  // layout measures 265.594 — the floor is met, the remainder is rounding.
  expect(geometry.layout, JSON.stringify(geometry)).toBeGreaterThanOrEqual(geometry.viewport * 0.4 - 1);
  expect(geometry.host, JSON.stringify(geometry)).toBeGreaterThanOrEqual(geometry.viewport * 0.35);

  // The collapsed header is one row: the title and "+ Session". The two-line
  // honesty is folded behind the same disclosure SurfaceHeader already owns.
  const header = page.locator(".sessions-header");
  await expect(header.locator('[data-surface-header-slot="honesty"]')).toBeHidden();
  await expect(header.getByRole("button", { name: "+ Session" })).toBeVisible();
  const more = header.locator(".surface-header-more");
  await expect(more).toBeVisible();
  // Folded, never removed: the disclosure brings the connection line back.
  await more.click();
  await expect(header.locator('[data-surface-header-slot="honesty"]')).toBeVisible();
});

test("Phone machine tools open with their work in the first screen, not below a wall of header (#232)", async ({ page }) => {
  test.skip(test.info().project.name !== "phone", "The first-screen floor is the narrow client's");
  await installFixtures(page, { assistant_enabled: true }, THREE_SESSIONS);
  await stubTerminalAttach(page);
  await page.route("**/api/history/**", async (route) => route.fulfill({ json: [] }));
  await page.route("**/api/assistant/**", async (route) =>
    route.fulfill({ json: { transcript: [], pending_action: null } }),
  );
  await page.route("**/api/git/status**", async (route) => route.fulfill({ json: {
    repo: "vogt", is_repo: true, branch: "main", ahead: 0, behind: 0, entries: [],
  } }));
  await page.route("**/api/git/branch**", async (route) =>
    route.fulfill({ json: { current: "main", all: ["main"] } }),
  );
  await page.route("**/api/git/log**", async (route) => route.fulfill({ json: [] }));

  const topOf = async (selector: string): Promise<number> =>
    page.locator(selector).first().evaluate((node) => node.getBoundingClientRect().top);
  // Navigate within the loaded shell (a hash change, not a fresh load), so the
  // public config a tool route consults to open its tab is already in hand —
  // the Assistant tab in particular opens only once `assistant_enabled` is
  // known.
  const openTool = async (hash: string) => {
    await page.evaluate((next) => { window.location.hash = next; }, hash);
  };

  await page.goto("/#/sessions");
  await expect(page.locator(".sessions-place")).toBeVisible();

  await openTool("#/history");
  await expect(page.locator(".history-view")).toBeVisible();
  expect(await topOf(".history-view"), "History content").toBeLessThan(400);

  await openTool("#/g");
  await expect(page.locator(".git-repository-picker")).toBeVisible();
  expect(await topOf(".git-repository-picker"), "Git chooser").toBeLessThan(400);

  await openTool("#/assistant");
  const composer = page.getByPlaceholder("Ask about sessions or work");
  await expect(composer).toBeVisible();
  // The Assistant content region begins high on the screen — the pane top is
  // where #232 had pushed everything to ~855px.
  expect(await topOf('[data-tab-kind="assistant"]'), "Assistant pane").toBeLessThan(400);
});

test("Phone Sessions overview reaches idle and busy sessions, not only the waiting card (#231/#233)", async ({ page }) => {
  test.skip(test.info().project.name !== "phone", "The overview list is the narrow body");
  await installFixtures(page, {}, THREE_SESSIONS);
  await page.goto("/#/sessions");

  // The waiting session is an attention card above; the idle and busy ones are
  // reachable as rows in the list, each a link to its terminal.
  await expect(page.getByRole("article", { name: /needs-answer is waiting for input/ })).toBeVisible();
  const list = page.locator(".session-list");
  await expect(list.locator('a[href="#/t/sess-idle"]')).toContainText("idle-shell");
  await expect(list.locator('a[href="#/t/sess-busy"]')).toContainText("running-build");
  // The waiting session is not duplicated as a row: it is the card.
  await expect(list.locator('a[href="#/t/sess-wait"]')).toHaveCount(0);

  await list.locator('a[href="#/t/sess-idle"]').click();
  await expect(page).toHaveURL(/#\/t\/sess-idle$/);
});

test("Desktop Sessions overview lists the running sessions as links (#233)", async ({ page }) => {
  test.skip(test.info().project.name === "phone", "The overview panel is the desktop body");
  await installFixtures(page, {}, THREE_SESSIONS);
  await page.goto("/#/sessions");

  const overview = page.locator(".sessions-overview-list");
  await expect(overview).toBeVisible();
  // All three, waiting included — there are no attention cards on a desk.
  await expect(overview.locator('.session-list a[href="#/t/sess-idle"]')).toContainText("idle-shell");
  await expect(overview.locator('.session-list a[href="#/t/sess-wait"]')).toContainText("needs-answer");
  await expect(overview.locator('.session-list a[href="#/t/sess-busy"]')).toContainText("running-build");

  await overview.locator('.session-list a[href="#/t/sess-busy"]').click();
  await expect(page).toHaveURL(/#\/t\/sess-busy$/);
});

test("Desktop Sessions overview points an empty machine at a start (#233)", async ({ page }) => {
  test.skip(test.info().project.name === "phone", "The overview panel is the desktop body");
  await installFixtures(page, {}, []);
  await page.goto("/#/sessions");

  // With no sessions the body is a call to action, not an empty panel that
  // never says how to leave it.
  await expect(page.locator(".sessions-overview")).toBeVisible();
  await expect(page.getByRole("button", { name: "Start a session" })).toBeVisible();
});

test("Phone Sessions overview and collapsed terminal header look right", async ({ page }) => {
  test.skip(test.info().project.name !== "phone", "Phone composition is the narrow client's");
  await installFixtures(page, {}, THREE_SESSIONS);
  await stubTerminalAttach(page);

  await page.goto("/#/sessions");
  await expect(page.locator(".session-list").first()).toBeVisible();
  await expect(page.locator(".sessions-place")).toHaveScreenshot("sessions-phone-overview.png", {
    // The age beside each state word and the live connection line are wall-clock
    // and stream-state relative; the composition around them is what is pinned.
    mask: [page.locator(".session-list .state"), page.locator(".sessions-header-honesty")],
  });

  await page.goto("/#/t/sess-idle");
  await expect(page.locator(".terminal-host").first()).toBeVisible();
  await expect(page.locator(".sessions-header")).toHaveScreenshot("sessions-phone-terminal-header.png");
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


test("Backlog filters are chips, a + Filter disclosure and a named lens", async ({ page }) => {
  await installFixtures(page);
  await page.goto("/#/backlog?project=vogt");

  const filters = page.getByRole("group", { name: "Backlog filters", exact: true });
  await expect(filters.getByText("Project: vogt")).toBeVisible();
  await expect(page.getByRole("group", { name: "Add Backlog filters" })).toBeHidden();
  // The legacy always-open select grid is gone: the selects live behind the
  // disclosure, not above the ranked work.
  await expect(page.locator(".vogt-backlog-filter-grid")).toHaveCount(0);

  await filters.getByRole("button", { name: "+ Filter", exact: true }).click();
  const panel = page.getByRole("group", { name: "Add Backlog filters" });
  await expect(panel).toBeVisible();
  await panel.getByRole("button", { name: "feature", exact: true }).click();
  await expect(page).toHaveURL(/kind=feature/);
  await expect(filters.getByText("Type: feature")).toBeVisible();

  // Escape closes the disclosure and gives focus back to what opened it.
  await page.keyboard.press("Escape");
  await expect(panel).toBeHidden();
  await expect(filters.getByRole("button", { name: "+ Filter", exact: true })).toBeFocused();

  await openFilterPanelOnPhone(filters);
  await page.getByLabel("Lens name").fill("Vogt features");
  await page.getByRole("button", { name: "Save lens" }).click();
  await expect(page.locator(".vogt-backlog-saved-recall")).toHaveText("Vogt features");

  await filters.getByRole("button", { name: "Remove filter Project: vogt" }).click();
  await expect(page).not.toHaveURL(/project=vogt/);
  await expect(page).toHaveURL(/kind=feature/);

  await filters.getByRole("button", { name: "Clear all" }).click();
  await expect(page).not.toHaveURL(/kind=feature/);

  await openFilterPanelOnPhone(filters);
  await page.locator(".vogt-backlog-saved-recall").click();
  await expect(page).toHaveURL(/project=vogt/);
  await expect(page).toHaveURL(/kind=feature/);

  await page.reload();
  await expect(filters.getByText("Project: vogt")).toBeVisible();
  await expect(filters.getByText("Type: feature")).toBeVisible();
});

test("Backlog ranked rows size to their content and expand in place", async ({ page }) => {
  const longTitle =
    "A ranked title long enough to need a second line and then some more of one ".repeat(3);
  await installFixtures(page, {}, [], undefined, {
    backlogItems: [
      {
        ref: "WI-7", title: longTitle, kind: "feature", state: "open", priority: "normal",
        project_slug: "vogt", trust_state: "verified", labels: ["infra"], score: 1.25,
        updated_at: "2026-08-17T10:00:00Z", origin: "declared",
      },
      {
        ref: "gh:vogt#12", title: "An observed subject nobody has adopted", kind: "bug",
        state: "observed", priority: "normal", project_slug: "vogt", trust_state: "unverified",
        labels: [], score: 0.5, updated_at: "2026-08-16T10:00:00Z", origin: "observed",
        observation_kind: "forge issue", observed_at: "2026-08-16T10:00:00Z",
        source_url: "https://example.invalid/12",
      },
    ],
  });
  await page.goto("/#/backlog");

  const declared = page.locator(".vogt-backlog-row").filter({ hasText: "A ranked title" });
  const observed = page.locator(".vogt-backlog-row").filter({ hasText: "An observed subject" });
  await expect(declared).toBeVisible();

  // The collapsed row keeps the facts a reader ranks by.
  await expect(declared.locator(".vogt-backlog-rank")).toHaveText("1");
  await expect(declared.getByRole("button", { name: "WI-7" })).toBeVisible();
  await expect(declared.locator(".vogt-backlog-trust")).toHaveText("verified");
  await expect(declared.locator(".vogt-backlog-age")).not.toBeEmpty();
  await expect(declared.locator(".vogt-backlog-score")).toContainText("1.25");

  // The title wraps rather than ending in an ellipsis: nothing it says is
  // taller than the box drawn for it.
  const title = declared.locator(".vogt-backlog-row-title");
  const clipping = await title.evaluate((node) => {
    const line = Number.parseFloat(getComputedStyle(node).lineHeight) || 16;
    return {
      scroll: node.scrollHeight,
      client: node.clientHeight,
      lines: Math.round(node.getBoundingClientRect().height / line),
    };
  });
  expect(clipping.scroll).toBeLessThanOrEqual(clipping.client + 1);
  expect(clipping.lines).toBeGreaterThanOrEqual(2);

  // Every control below is reached from the keyboard: it is the operable path
  // the row has to keep, and it does not depend on where a phone's shell has
  // pushed the list (first-viewport containment is its own issue).
  const press = async (control: ReturnType<typeof page.getByRole>) => {
    await control.focus();
    await page.keyboard.press("Enter");
  };

  const before = await declared.evaluate((node) => node.getBoundingClientRect().height);
  await press(declared.getByRole("button", { name: "More" }));
  // Declared rows offer what a work item can do, and nothing a subject can.
  await expect(declared.getByRole("button", { name: "Open", exact: true })).toBeVisible();
  await expect(declared.getByRole("button", { name: "Select", exact: true })).toBeVisible();
  await expect(declared.getByRole("button", { name: "Start a session…" })).toBeVisible();
  await expect(declared.getByRole("button", { name: "Adopt as work item…" })).toHaveCount(0);
  await expect(page.getByRole("dialog")).toHaveCount(0);
  const expanded = await declared.evaluate((node) => node.getBoundingClientRect().height);
  expect(expanded).toBeGreaterThan(before);

  // The ranking evidence belongs to the row it explains.
  await press(declared.getByRole("button", { name: /1\.25/ }));
  await expect(declared.getByText("raised 9 days ago and still open")).toBeVisible();

  // A write from a row still asks for a reason of its own.
  await press(declared.getByRole("button", { name: "Start a session…" }));
  const confirm = declared.getByRole("button", { name: "Confirm" });
  await expect(confirm).toBeDisabled();
  await declared.locator("textarea").fill("picking this up now");
  await expect(confirm).toBeEnabled();
  await press(declared.getByRole("button", { name: "Cancel" }));

  // Observed rows offer the two writes a subject has, and no transition.
  await press(observed.getByRole("button", { name: "More" }));
  await expect(observed.getByText("Observed forge issue")).toBeVisible();
  await expect(observed.getByRole("button", { name: "Adopt as work item…" })).toBeVisible();
  await expect(observed.getByRole("button", { name: "Suppress source…" })).toBeVisible();
  await expect(observed.getByRole("button", { name: "Start a session…" })).toHaveCount(0);
  await expect(observed.getByRole("checkbox")).toBeDisabled();

  await press(declared.getByRole("button", { name: "Less" }));
  await expect(declared.getByRole("button", { name: "Open", exact: true })).toHaveCount(0);
});



/**
 * Stage 10's two phone rules, measured rather than asserted in prose: no
 * interactive target below 44 by 44 pixels, and nothing typed into below 16px
 * — the size at which mobile browsers zoom the visual viewport out from under
 * whoever is typing.
 *
 * Every route the phone can reach is walked, because the rule is the shell's
 * and not any one surface's, and a control added to Settings tomorrow is as
 * able to break it as one added to Inbox.
 */
const PHONE_ROUTES = [
  "sessions", "inbox", "board", "backlog",
  "projects", "audit", "settings", "git", "history", "tasks", "w/WI-7",
] as const;

async function undersizedControls(page: Page): Promise<string[]> {
  return page.evaluate(() => {
    const found: string[] = [];
    const nodes = document.querySelectorAll<HTMLElement>(
      "button, summary, input, select, textarea, [role=button]",
    );
    for (const node of nodes) {
      const box = node.getBoundingClientRect();
      if (box.width === 0 || box.height === 0) continue;
      // A tick box's target is the label a tap lands on, not the drawn box.
      const tick =
        node.tagName === "INPUT" &&
        /^(checkbox|radio)$/.test((node as HTMLInputElement).type);
      const target = tick ? (node.closest("label") ?? node).getBoundingClientRect() : box;
      const font = Number.parseFloat(getComputedStyle(node).fontSize);
      const name = `${node.tagName.toLowerCase()}.${node.className || "-"}`.slice(0, 50);
      if (target.height < 44 || target.width < 44) {
        found.push(`${name} is ${Math.round(target.width)}x${Math.round(target.height)}`);
      }
      if (/^(INPUT|TEXTAREA|SELECT)$/.test(node.tagName) && font < 16) {
        found.push(`${name} types at ${font}px`);
      }
    }
    return [...new Set(found)];
  });
}

test("Phone controls keep the 44px target and 16px form-text floors", { timeout: 90_000 }, async ({ page }) => {
  test.skip(test.info().project.name !== "phone", "The floors are the narrow shell's");
  await installFixtures(page);

  for (const route of PHONE_ROUTES) {
    await page.goto(`/#/${route}`);
    await page.waitForLoadState("networkidle");
    await page.waitForTimeout(200);
    expect(await undersizedControls(page), `at 390px on /${route}`).toEqual([]);
    const overflow = await page.evaluate(() => ({
      doc: document.documentElement.scrollWidth,
      win: window.innerWidth,
    }));
    expect(overflow.doc, `no sideways scroll on /${route}`).toBeLessThanOrEqual(overflow.win);
  }

  // The narrowest and widest phones the shell claims, because a 44px floor
  // that only holds at one width is a coincidence.
  for (const width of [320, 430]) {
    await page.setViewportSize({ width, height: 844 });
    for (const route of ["sessions", "inbox", "board", "backlog"]) {
      await page.goto(`/#/${route}`);
      await page.waitForLoadState("networkidle");
      await page.waitForTimeout(200);
      expect(await undersizedControls(page), `at ${width}px on /${route}`).toEqual([]);
    }
  }
});


/**
 * F1 of the live restructure report: a phone that spends its first screen on
 * controls is not a steering surface. Each primary route has to arrive with
 * its own work already on it — and the controls that moved out of the way
 * have to still be reachable, which is the half that makes it a composition
 * fix rather than a deletion.
 */
test("Phone primary surfaces lead with their work, not their controls", async ({ page }) => {
  test.skip(test.info().project.name !== "phone", "First-viewport composition is the phone's");
  await installFixtures(page);

  const firstUseful = {
    // The Live Sessions roster was removed (#167); Sessions leads with a
    // waiting card when one needs input, otherwise the machine workspace.
    sessions: ".session-waiting, .sessions-active-workspace",
    inbox: ".inbox-entry, .inbox-empty",
    board: ".board-card, .board-empty",
    backlog: ".vogt-backlog-row, .vogt-backlog-empty",
  } as const;

  for (const [route, selector] of Object.entries(firstUseful)) {
    await page.goto(`/#/${route}`);
    const first = page.locator(selector).first();
    await expect(first, `/${route} draws something to steer with`).toBeVisible();

    const geometry = await first.evaluate((node) => ({
      top: node.getBoundingClientRect().top,
      viewport: window.innerHeight,
      sideways: document.documentElement.scrollWidth > window.innerWidth,
    }));
    expect(geometry.sideways, `/${route} does not scroll sideways`).toBe(false);
    // Inside the first screen, and with room to be read rather than peeking
    // over the fold by a pixel.
    expect(geometry.top, `/${route} first content at ${geometry.top}`)
      .toBeLessThan(geometry.viewport - 80);
  }

  // What moved out of the first screen is one control away, not gone.
  await page.goto("/#/board");
  const header = page.locator("[data-surface-header]:visible");
  await expect(header.locator('[data-surface-header-slot="controls"]')).toBeHidden();
  await header.getByRole("button", { name: "View controls" }).click();
  await expect(header.locator('[data-surface-header-slot="controls"]')).toBeVisible();
  await expect(page.getByRole("button", { name: "Refresh now" })).toBeVisible();

  const filters = page.getByRole("group", { name: "Board filters", exact: true });
  await expect(page.getByLabel("Lens name")).toBeHidden();
  await filters.getByRole("button", { name: "+ Filter", exact: true }).click();
  await expect(page.getByLabel("Lens name")).toBeVisible();
});


/**
 * Stage 3's route matrix, walked on a phone (FR-U23).
 *
 * Reachability was never the question — the palette could open all of these.
 * What this asserts is that each one arrives *contained*: inside the phone
 * shell, saying what it is, without a sideways scroll, and with its own
 * address surviving a reload and the back button.
 */
const SECONDARY_ROUTES = [
  { path: "projects", title: "Projects" },
  { path: "audit", title: "Audit" },
  { path: "w/WI-7", title: "Measured board card" },
  { path: "g", title: "Sessions", tool: "Git" },
  { path: "g/src", title: "Sessions", tool: "Git" },
  { path: "history", title: "Sessions", tool: "History" },
  { path: "tasks", title: "Sessions", tool: "Tasks" },
  { path: "gui", title: "GUI stream is unavailable" },
  // No PTY answers in a fixture, so what has to hold here is that the route
  // says which session state it is in — live or not found — inside the shell.
  { path: "t/browser-session", title: "Session" },
  { path: "e/src/main.ts", title: "Sessions" },
] as const;

test("Phone secondary routes are contained, titled and addressable", async ({ page }) => {
  test.skip(test.info().project.name !== "phone", "Containment is the narrow shell's");
  await installFixtures(page);

  for (const route of SECONDARY_ROUTES) {
    await page.goto(`/#/${route.path}`);
    await page.waitForLoadState("networkidle");

    // Inside the phone shell, not instead of it.
    await expect(page.locator(".mobile-go-to"), `/${route.path} keeps Go to`).toBeVisible();
    await expect(
      page.getByRole("navigation", { name: "Primary navigation" }),
      `/${route.path} keeps the bottom bar`,
    ).toBeVisible();

    // Saying what it is.
    const titled = page.getByRole("heading", { name: route.title, exact: false }).first();
    await expect(titled, `/${route.path} says it is ${route.title}`).toBeVisible();
    if ("tool" in route && route.tool) {
      await expect(
        page.getByRole("navigation", { name: "Session tools" })
          .getByRole("link", { name: route.tool }),
        `/${route.path} marks its tool`,
      ).toHaveAttribute("aria-current", "page");
    }

    const sideways = await page.evaluate(
      () => document.documentElement.scrollWidth > window.innerWidth,
    );
    expect(sideways, `/${route.path} does not scroll sideways`).toBe(false);
  }

  // Settings is a modal route: what it opens over is covered rather than left
  // peeking out above the form.
  await page.goto("/#/settings");
  const settings = page.getByRole("dialog", { name: "Settings" });
  await expect(settings).toBeVisible();
  const covered = await page.evaluate(() => {
    const backdrop = document.querySelector(".modal-backdrop");
    if (!backdrop) return false;
    const box = backdrop.getBoundingClientRect();
    return box.top <= 0 && box.height >= window.innerHeight - 1;
  });
  expect(covered, "the Settings backdrop covers the shell it opened over").toBe(true);
  await expect(settings.getByLabel("Bearer token")).toBeVisible();

  // A deep link with a query survives a reload and the back button.
  await page.goto("/#/audit?actor=user%3Asam");
  await expect(page.getByRole("heading", { name: "Audit" })).toBeVisible();
  await page.reload();
  await expect(page).toHaveURL(/actor=user%3Asam/);
  await page.goto("/#/projects");
  await expect(page.getByRole("heading", { name: "Projects" })).toBeVisible();
  await page.goBack();
  await expect(page).toHaveURL(/#\/audit\?actor=user%3Asam/);
  await page.goForward();
  await expect(page).toHaveURL(/#\/projects/);
});


/**
 * Stage 9's waiting session, on the surface a phone actually steers from.
 *
 * The two things asserted here are the two the requirement is about: the card
 * shows the session's real prompt before offering anything, and each control
 * sends exactly the bytes a person at that terminal would have typed. Neither
 * is an approval, and the card says so.
 */
test("Phone waiting sessions show the prompt and send terminal input", async ({ page }) => {
  test.skip(test.info().project.name !== "phone", "Waiting cards are the narrow shell's");
  const waiting = {
    ...liveSession,
    id: "waiting-session",
    name: "needs-attention",
    activity: "waiting-for-input",
  };
  const exited = {
    ...liveSession,
    id: "exited-session",
    name: "already-finished",
    activity: "waiting-for-input",
    exit_code: 0,
  };
  const fixture = await installFixtures(page, {}, [liveSession, waiting, exited], undefined, {
    scrollback: "running migration 0011\nApply this migration? (y/n) ",
  });

  await page.goto("/#/sessions");

  const card = page.locator(".session-waiting").filter({ hasText: "needs-attention" });
  await expect(card).toBeVisible();
  // The Live Sessions roster was removed (#167); the waiting card is the
  // prompt the phone came for, and it leads the surface.

  // It shows before it asks: the session's own output, and the target it belongs to.
  await expect(card.getByTestId("waiting-tail")).toContainText("Apply this migration? (y/n)");
  await expect(card).toContainText("/workspace/vogt");
  await expect(card).toContainText("not Vogt approvals");

  const acts = card.getByRole("group", { name: "Terminal input for needs-attention" });
  await acts.getByRole("button", { name: "Send y + Enter" }).click();
  await expect.poll(() => fixture.sessionInputs.length).toBe(1);
  expect(fixture.sessionInputs[0]).toEqual({
    id: "waiting-session",
    text: "y",
    submit: true,
  });

  await acts.getByRole("button", { name: "Send Ctrl-C" }).click();
  await expect.poll(() => fixture.sessionInputs.length).toBe(2);
  expect(fixture.sessionInputs[1]).toEqual({
    id: "waiting-session",
    text: "\u0003",
    submit: false,
  });

  // An exited session refuses safely, and says why rather than offering a
  // control that could only fail at whoever pressed it.
  const dead = page.locator(".session-waiting").filter({ hasText: "already-finished" });
  await expect(dead).toContainText("has exited");
  await expect(dead.getByRole("button", { name: "Send y + Enter" })).toHaveCount(0);
  await expect(dead.getByRole("button", { name: "Send Ctrl-C" })).toHaveCount(0);
});


/**
 * #104's middle criterion, asserted where it is falsifiable: a reader on
 * Board, Backlog, Inbox or the Sessions roster must not download the editor
 * or the terminal. The budget script measures the built graph; this measures
 * what the browser actually asks for.
 */
test("Non-editor routes fetch neither the editor nor the terminal", async ({ page }) => {
  const deferred = /monaco|xterm|TerminalWorkspace|EditorWorkspace|\.worker/i;
  const asked: string[] = [];
  page.on("request", (request) => {
    const url = request.url();
    if (deferred.test(url)) asked.push(new URL(url).pathname);
  });

  await installFixtures(page);
  for (const route of ["board", "backlog", "inbox", "sessions", "projects", "audit"]) {
    await page.goto(`/#/${route}`);
    await page.waitForLoadState("networkidle");
  }
  expect(asked, "no editor or terminal code on a route that has neither").toEqual([]);

  // And it does arrive when a route needs it: the editor route is what pays
  // for the editor.
  await page.goto("/#/e/src/main.ts");
  await expect.poll(() => asked.length, { timeout: 15_000 }).toBeGreaterThan(0);
});

/**
 * #63's server-owned bounded pages, from the client's side: a cell that has
 * more than one page reads the next one with the *cursor and snapshot the
 * server gave it*, and appends. NFR-S5's point is that scrolling a column
 * must not become a read of the estate.
 */
test("A bounded Board cell continues from its cursor rather than re-reading", async ({ page }) => {
  test.skip(test.info().project.name === "phone", "Cell scrolling is the desk's column");
  const many = Array.from({ length: 8 }, (_, index) => ({
    ...boardItems[0],
    ref: `WI-${index + 1}`,
    title: `Ranked card ${index + 1}`,
  }));
  const fixture = await installFixtures(page, {}, [], undefined, {
    boardItems: many,
    boardPageSize: 3,
    boardTotal: many.length,
  });
  await page.goto("/#/board");

  await expect(page.getByText("Ranked card 1")).toBeVisible();
  await expect(page.getByText("Ranked card 4")).toHaveCount(0);
  // The column says how long it really is, not how much of it arrived.
  await expect(page.locator('.board-cell[data-state="open"]')).toHaveAttribute("data-wip", "8");

  const cell = page.locator('.board-cell[data-state="open"] .board-cell-cards');
  await cell.evaluate((node) => {
    node.scrollTop = node.scrollHeight;
    node.dispatchEvent(new Event("scroll"));
  });

  await expect(page.getByText("Ranked card 4")).toBeVisible();
  const last = fixture.boardRequests.at(-1) as {
    cells?: { state?: string; cursor?: string }[];
    snapshot?: string;
  };
  expect(last.cells).toEqual([{ lane_key: "", state: "open", cursor: "cursor-1" }]);
  expect(last.snapshot).toBe("browser-board-snapshot");
});

/**
 * #59's crowded rail: many live sessions must not push the file tree out of
 * the rail. The running list gets its own scroller and Files keeps a usable
 * minimum — asserted as geometry, because "usable" is a height.
 */
test("A crowded places rail keeps the file tree reachable", async ({ page }) => {
  test.skip(test.info().project.name === "phone", "The places rail is a desktop surface");
  const crowd = Array.from({ length: 24 }, (_, index) => ({
    ...liveSession,
    id: `crowd-${index}`,
    name: `crowded-session-${index}`,
  }));
  await installFixtures(page, {}, crowd);
  await page.goto("/#/sessions");

  const rail = page.locator(".places-rail");
  await expect(rail).toBeVisible();
  const filesToggle = page.getByRole("button", { name: /Files/ });
  await expect(filesToggle).toBeVisible();
  await expect(filesToggle).toHaveAttribute("aria-expanded", "false");
  await filesToggle.click();
  await expect(page.getByRole("heading", { name: "Files" })).toBeVisible();
  await expect(page.getByRole("searchbox", { name: "Search files" })).toBeVisible();

  const geometry = await rail.evaluate((node) => {
    const tree = node.querySelector(".file-tree");
    const sessions = node.querySelector(".places-rail-session-area");
    const treeBox = tree?.getBoundingClientRect();
    return {
      railBottom: node.getBoundingClientRect().bottom,
      viewport: window.innerHeight,
      treeTop: treeBox?.top ?? -1,
      treeHeight: treeBox?.height ?? -1,
      sessionsScrolls: sessions
        ? sessions.scrollHeight > sessions.clientHeight + 1
        : false,
      railScrolls: node.scrollHeight > node.clientHeight + 1,
    };
  });

  // The rail is the one desktop scroller (#59's decision: no nested traps),
  // it does not grow past the window, and Files keeps its usable minimum
  // rather than being squeezed to a sliver under the crowd.
  expect(geometry.railBottom).toBeLessThanOrEqual(geometry.viewport + 1);
  expect(geometry.railScrolls).toBe(true);
  expect(geometry.treeHeight).toBeGreaterThanOrEqual(260);

  // Reachable, not merely present: scrolling the rail brings Files onto the
  // screen, with its search still usable there.
  await rail.evaluate((node) => {
    node.scrollTop = node.scrollHeight;
  });
  const reached = await page.getByRole("heading", { name: "Files" }).evaluate((node) => {
    const box = node.getBoundingClientRect();
    return box.top >= 0 && box.bottom <= window.innerHeight;
  });
  expect(reached).toBe(true);
  await expect(page.getByRole("searchbox", { name: "Search files" })).toBeVisible();
});

/**
 * The other half of #104, learned the hard way on the dev instance: a lazy
 * component that is *always mounted* fetches its chunk at boot anyway. Three
 * dialogs did — Settings, the template selector and the shortcut help — so
 * the split saved nothing on them, and when `Settings-*.js` failed to arrive
 * the root error boundary replaced the entire product with its own message.
 * A dialog nobody opened must not be on the boot path at all.
 */
test("A dialog nobody opened is not downloaded at boot", async ({ page }) => {
  const dialogs = /(^|\/)(Settings|TemplateSelector|KeyboardShortcuts)[.-]/;
  const asked: string[] = [];
  page.on("request", (request) => {
    const path = new URL(request.url()).pathname;
    if (dialogs.test(path)) asked.push(path);
  });

  await installFixtures(page);
  for (const route of ["board", "sessions", "inbox", "backlog"]) {
    await page.goto(`/#/${route}`);
    await page.waitForLoadState("networkidle");
  }
  expect(asked, "no dialog chunk on the boot path of any place").toEqual([]);

  // And it does arrive when the reader asks for it — the deferral is not a
  // deletion.
  await page.goto("/#/settings");
  await expect(page.getByRole("dialog", { name: "Settings" })).toBeVisible();
  expect(asked.some((path) => /Settings/.test(path))).toBe(true);
});

/**
 * And when a chunk genuinely does not arrive — a deploy swapped the files
 * under an open tab, or the network dropped — the cost is that place, not the
 * product. This is the failure the dev instance actually showed: one dialog
 * chunk failed and the root boundary replaced the whole shell with "Vogt
 * could not render this view".
 */
test("A place whose chunk never arrives costs that place, not the shell", async ({ page }) => {
  await installFixtures(page);
  await page.route(/Backlog\.tsx|Backlog-[A-Za-z0-9_-]+\.js/, (route) => route.abort());

  await page.goto("/#/backlog");

  await expect(page.getByText("This place could not be loaded")).toBeVisible();
  await expect(page.getByRole("button", { name: "Reload" })).toBeVisible();
  // The shell is still a shell: navigation is there, and the places that did
  // load still work.
  const shell = test.info().project.name === "phone"
    ? page.getByRole("navigation", { name: "Primary navigation" })
    : page.getByRole("complementary", { name: "Places" });
  await expect(shell).toBeVisible();
  await expect(page.getByText("Vogt could not render this view")).toHaveCount(0);

  await page.goto("/#/board");
  await expect(page.getByRole("heading", { name: "Board" })).toBeVisible();
});


/**
 * The working header is one row with one vertical anchor.
 *
 * Reported from the dev instance as "this area is badly rendering on every
 * page", and it was: every slot hugged the top of a header whose slots are
 * all different heights, the controls bottom-aligned inside their own slot,
 * and a labelled select stacked its label above itself. Three baselines in
 * one bar — the primary action floating above the button beside it.
 *
 * Asserted as geometry rather than as a screenshot: the controls and the
 * action are on the same line as each other, and no control in the header is
 * taller than the row it is supposed to sit in.
 */
test("The working header puts its controls and its action on one line", async ({ page }) => {
  test.skip(test.info().project.name === "phone", "The narrow shell stacks by design");
  await installFixtures(page);

  for (const route of ["board", "backlog", "inbox", "sessions"]) {
    await page.goto(`/#/${route}`);
    const header = page.locator("[data-surface-header]:visible").first();
    await expect(header).toBeVisible();

    const rows = await header.evaluate((node) => {
      const box = (selector: string) => {
        const found = node.querySelector<HTMLElement>(selector);
        if (!found) return null;
        const rect = found.getBoundingClientRect();
        return { top: rect.top, bottom: rect.bottom, mid: rect.top + rect.height / 2 };
      };
      return {
        controls: box('[data-surface-header-slot="controls"]'),
        action: box('[data-surface-header-slot="action"]'),
        title: box('[data-surface-header-slot="title"]'),
        // The tallest thing inside the controls: a label stacked over its
        // select is what used to double this.
        tallest: Math.max(
          0,
          ...[...node.querySelectorAll<HTMLElement>(
            '[data-surface-header-slot="controls"] > *',
          )].map((child) => child.getBoundingClientRect().height),
        ),
      };
    });

    if (rows.controls && rows.action) {
      // Centres within a few pixels of each other: the two clusters are one
      // row, not two anchored at different edges.
      expect(
        Math.abs(rows.controls.mid - rows.action.mid),
        `/${route} controls and action share a line`,
      ).toBeLessThan(8);
    }
    if (rows.controls && rows.title) {
      expect(
        Math.abs(rows.controls.mid - rows.title.mid),
        `/${route} controls sit with the title`,
      ).toBeLessThan(20);
    }
    expect(rows.tallest, `/${route} no stacked label doubles a control`)
      .toBeLessThan(46);
  }
});


/**
 * The Places rail and the Sessions live-list are drag-resizable and
 * collapsible on a desk, and both remember what the reader set (r18: "make
 * it more compact by default, resizable, and collapsible").
 *
 * Desktop only — below the shell's own narrow breakpoint the rail is not a
 * grid column at all, it is `display: none` and replaced by the bottom nav,
 * and the resizable-pane machinery deliberately does not touch that layout
 * (a prior version of this feature bound its width inline regardless of
 * viewport, which out-specificities the narrow stylesheet rule and starved
 * every phone route of two thirds of its width — caught by the existing
 * Inbox first-viewport test going red at exactly this feature's introduction).
 */
test("The Places rail resizes, collapses, and remembers both across reload", async ({ page }) => {
  test.skip(test.info().project.name === "phone", "The rail is a desktop surface");
  await installFixtures(page);
  await page.goto("/#/board");

  const rail = page.locator(".places-rail");
  const before = await rail.evaluate((node) => node.getBoundingClientRect().width);
  expect(Math.round(before)).toBe(248);

  const handle = page.locator(".rail-resize-handle");
  const box = (await handle.boundingBox())!;
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.down();
  await page.mouse.move(box.x + 120, box.y + box.height / 2, { steps: 5 });
  await page.mouse.up();

  const widened = await rail.evaluate((node) => node.getBoundingClientRect().width);
  expect(widened).toBeGreaterThan(before + 80);

  // Reload before collapsing: the width survives on its own, independent of
  // the collapsed flag.
  await page.reload();
  await expect(rail.evaluate((node) => node.getBoundingClientRect().width))
    .resolves.toBeCloseTo(widened, 0);

  await page.getByRole("button", { name: "Hide the Places rail" }).click();
  await expect(rail).toBeHidden();
  // The reopen control sits in `main`'s own flow, not fixed over the
  // connection banner both claim the same corner of.
  const reopen = page.getByRole("button", { name: "Show the Places rail" });
  await expect(reopen).toBeVisible();
  const overlap = await reopen.evaluate((node) => {
    const banner = document.querySelector(".connection-banner");
    if (!banner) return false;
    const a = node.getBoundingClientRect();
    const b = banner.getBoundingClientRect();
    return a.left < b.right && a.right > b.left && a.top < b.bottom && a.bottom > b.top;
  });
  expect(overlap).toBe(false);

  await page.reload();
  await expect(page.locator(".places-rail")).toBeHidden();
  await expect(page.getByRole("button", { name: "Show the Places rail" })).toBeVisible();

  // And nothing about a collapsed, resized rail narrowed the surface it made
  // room for — the whole point of collapsing it.
  await page.getByRole("button", { name: "Show the Places rail" }).click();
  await expect(page.locator(".places-rail")).toBeVisible();
});

test("A resized, collapsed rail does not leak its width into the narrow shell", async ({ page }) => {
  test.skip(test.info().project.name === "phone", "seeds a desktop width, then narrows");
  await installFixtures(page);
  await page.goto("/#/board");
  const handle = page.locator(".rail-resize-handle");
  const box = (await handle.boundingBox())!;
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.down();
  await page.mouse.move(box.x + 120, box.y + box.height / 2, { steps: 5 });
  await page.mouse.up();

  // Narrow the same page rather than reloading at a narrow viewport: the
  // regression this guards was the *inline* style outliving the width that
  // made it correct, not a fresh render choosing the wrong one.
  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/#/inbox");
  await page.waitForLoadState("networkidle");

  const geometry = await page.evaluate(() => ({
    entryWidth: document.querySelector(".inbox-entry")?.getBoundingClientRect().width ?? 0,
    viewport: window.innerWidth,
  }));
  expect(geometry.entryWidth).toBeGreaterThan(geometry.viewport * 0.8);
});

// The Sessions live-list resize/collapse test was removed with the Live
// Sessions sub-panel (#167); the places rail is where sessions are listed now.

/**
 * History reads archived sessions only (`USER_GUIDE.md` §2: "Archived
 * scrollback from sessions that have ended"), and a reader with live shells
 * open has no way to tell that apart from a broken read without saying so.
 */
test("History explains an empty archive when live sessions are still running", async ({ page }) => {
  await installFixtures(page, {}, [liveSession]);
  await page.route("**/api/history/sessions*", (route) => route.fulfill({ json: [] }));
  await page.goto("/#/history");

  await expect(page.getByText("No archived sessions.", { exact: false })).toBeVisible();
  await expect(page.getByText(/session is currently running/)).toBeVisible();
});
