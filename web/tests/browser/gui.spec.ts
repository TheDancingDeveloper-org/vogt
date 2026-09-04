import { expect, test, type Locator, type Page } from "@playwright/test";

// #295: the `live` Playwright project drives these specs against a *running*
// stack (real API) rather than the mocked Vite server. In that mode
// `installFixtures` stops intercepting routes — every request reaches the live
// front door — and seeds a real front-door token where the mocked run seeds a
// fake one. The project is opt-in (see playwright.config.ts), so `LIVE` is
// false for the default and PR invocations and these specs behave exactly as
// before. Per-test route overrides outside `installFixtures` are left in
// place; the live project's coverage is what `installFixtures` alone drives.
const LIVE = Boolean(process.env.PLAYWRIGHT_LIVE_BASE_URL);
const LIVE_TOKEN = process.env.PLAYWRIGHT_LIVE_TOKEN ?? "";

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

// A source-linked entry: a GitHub review request that deep-links out to the
// forge, with nothing in-app behind it. It carries no evidence, so its own
// disclosure stays folded — part of what keeps the list dense.
const sourceUrlEntry = {
  ...inboxEntry,
  entry_key: "github:pr-42",
  source: "github",
  kind: "review_requested",
  title: "Review requested on pull request 42",
  summary: "A teammate asked for your review on the estate front door.",
  source_subject_key: "github:pr-42",
  source_url: "https://github.example/org/repo/pull/42",
  work_item_ref: null,
  session_id: null,
  action: { kind: "observation" },
  evidence_snapshot: null,
  proposed_change: null,
};

// An entry with neither a work item nor a session behind it and no source
// link either: there is nowhere for "Open entry" to go.
const orphanEntry = {
  ...inboxEntry,
  entry_key: "ci:build-99",
  source: "ci",
  kind: "pipeline_failed",
  title: "Pipeline failed on main",
  summary: "The build step exited non-zero.",
  source_subject_key: "ci:build-99",
  source_url: null,
  work_item_ref: null,
  session_id: null,
  action: { kind: "observation" },
  evidence_snapshot: null,
  proposed_change: null,
};

// A session-backed entry, so "Open entry" has an in-app destination on at
// least one non-drift row too.
const sessionEntry = {
  ...inboxEntry,
  entry_key: "agent:session-7",
  source: "agent",
  kind: "session_waiting",
  title: "Agent session is waiting for input",
  summary: "A session paused for a decision.",
  source_subject_key: "agent:session-7",
  source_url: null,
  work_item_ref: null,
  session_id: "eng-session-7",
  action: { kind: "session" },
  evidence_snapshot: null,
  proposed_change: null,
};

const inboxResult = () => ({
  entries: [inboxEntry, sourceUrlEntry, orphanEntry, sessionEntry],
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
  /** The actor roster the item editor's assignee picker offers. */
  actors?: { identity_ref: string; display_name: string }[];
  /** The Markdown body `/work/get` returns for WI-7, for the renderer (#222). */
  workBody?: string;
  /** The server's push subscription list, for Settings push reconciliation. */
  pushSubscriptions?: Record<string, unknown>[];
  /** Seed files the `/api/files` fixture serves, keyed by workspace path. Reads
   *  return a content hash + mtime; a write carrying a stale `if_match` gets a
   *  409, so a test can simulate an external change and prove the editor's
   *  on-disk conflict handling (#237). */
  files?: Record<string, string>;
  /** First-run install mode (#292): what `/api/install/status` reports. */
  installMode?: boolean;
  /** Start signed out — the wizard and the login gate only appear then. */
  noToken?: boolean;
  /** What `forge.account_status` reports on the setup steps (#292). */
  forgeAccounts?: Record<string, unknown>[];
  /** What the setup repo picker lists (#292). */
  forgeRepos?: Record<string, unknown>[];
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
  agentTasks: Record<string, unknown>[] | null = null,
) {
  // In live mode this is a no-op: nothing is intercepted, so every request
  // below reaches the real front door. In mocked mode it is `page.route`, so
  // the fixtures install exactly as before. Typed as `Page["route"]` so each
  // route callback keeps its full type-checking either way.
  const registerRoute: Page["route"] = LIVE
    ? (((..._args: unknown[]) => Promise.resolve()) as unknown as Page["route"])
    : page.route.bind(page);
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
  const updateRequests: Record<string, unknown>[] = [];
  const sessionInputs: { id: string; text: string; submit: boolean }[] = [];
  const sessions = [...initialSessions];
  let createdSessions = 0;
  // A session DELETE is the kill path; composing a split and detaching a pane
  // must never reach it (#212), so a test can assert this stays at zero.
  let sessionDeletes = 0;
  await page.addInitScript(
    ({ signedIn, token }) => {
      // Live mode needs a real front-door token to pass the sign-in gate; the
      // mocked run uses a fixed fake one the mocked API accepts.
      if (signedIn) localStorage.setItem("vogt.token", token || "browser-test-token");
      // Pin the shell to Vogt Dark so the default rendering under test stays
      // deterministic — the existing baselines were taken in the dark palette,
      // and the theme system (#299) must not shift them. Per-theme tests below
      // register their own init script after this one to override it.
      localStorage.setItem("vogt.appTheme.v1", "dark");
    },
    { signedIn: !metrics.noToken, token: LIVE ? LIVE_TOKEN : "" },
  );
  // First-run install mode (#292). Answered for every test — the app asks
  // only when it starts signed out — so the wizard's presence is a fixture
  // decision, never an accidental 404.
  const bootstrapRequests: Record<string, unknown>[] = [];
  const linkRequests: Record<string, unknown>[] = [];
  const registerRequests: Record<string, unknown>[] = [];
  const importRequests: Record<string, unknown>[] = [];
  const sweepRequests: Record<string, unknown>[] = [];
  await registerRoute("**/api/install/status", async (route) =>
    route.fulfill({ json: { install_mode: metrics.installMode ?? false } }),
  );
  await registerRoute("**/api/install/bootstrap", async (route) => {
    const body = route.request().postDataJSON() as Record<string, unknown>;
    bootstrapRequests.push(body);
    const slug = String(body.display_name ?? "")
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, "-")
      .replace(/^-|-$/g, "");
    return route.fulfill({ json: {
      actor: {
        id: "act-wizard",
        identity_ref: `human:${slug}`,
        display_name: body.display_name,
        kind: "human",
      },
      token: { id: "tok-wizard", name: "first-run browser token", scopes: ["admin"] },
      secret: "vogt_browser-test-first-run-secret",
      warning: "This is the only time the secret is shown.",
    } });
  });
  await registerRoute("**/api/status**", async (route) => route.fulfill({ json: {
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
  await registerRoute("**/api/auth/check", async (route) => route.fulfill({ json: {
    ok: true,
    version: "test",
    product_version: "test",
    storage: { state_dir: "/tmp/vogt", workspace_root: "/workspace" },
  } }));
  await registerRoute("**/api/config**", async (route) => route.fulfill({ json: {
    assistant_enabled: false, gui_stream_url: null, session_templates: [],
    gui_stream_available: false,
    vogt: { configured: true },
    ...config,
  } }));
  await registerRoute("**/api/sessions", async (route) => {
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
  await registerRoute("**/api/sessions/*/kill", async (route) =>
    route.fulfill({ json: { ok: true } }),
  );
  await registerRoute("**/api/sessions/*/input", async (route) => {
    const id = new URL(route.request().url()).pathname.split("/").at(-2) ?? "";
    const body = route.request().postDataJSON() as { text: string; submit: boolean };
    sessionInputs.push({ id, ...body });
    return route.fulfill({ json: { ok: true } });
  });
  await registerRoute("**/api/sessions/*", async (route) => {
    if (
      route.request().method() === "POST"
      && new URL(route.request().url()).pathname.endsWith("/kill")
    ) {
      return route.fulfill({ json: { ok: true } });
    }
    if (route.request().method() === "DELETE") {
      sessionDeletes += 1;
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
  await registerRoute("**/api/events", async (route) => route.fulfill({
    status: 200,
    contentType: "text/event-stream",
    // Keep the mocked stream open and mark it answered. An immediately closed
    // body is correctly treated as a disconnect by the production client.
    body: `data: ${JSON.stringify({ type: "activity", id: "fixture-stream", state: "idle" })}\n\n`,
  }));
  await registerRoute("**/api/tree**", async (route) => {
    const path = new URL(route.request().url()).searchParams.get("path") ?? "";
    if (path === "src") {
      return route.fulfill({ json: [
        { name: "nested-component.tsx", path: "src/nested-component.tsx", is_dir: false },
      ] });
    }
    return route.fulfill({ json: tree });
  });
  // A stateful file store, so a read hands back a hash/mtime and a write with a
  // stale `if_match` conflicts (#237). Registered before any test-specific
  // `/api/files` route, which — being registered later — wins where present.
  // A regex keeps this off `/api/files/op` and `/api/files/download`.
  const fileHash = (s: string) => {
    let h = 5381;
    for (let i = 0; i < s.length; i += 1) h = ((h << 5) + h + s.charCodeAt(i)) >>> 0;
    return h.toString(16);
  };
  const fileStore = new Map<string, string>(Object.entries(metrics.files ?? {}));
  let fileMtime = 1000;
  await registerRoute(/\/api\/files(\?|$)/, async (route) => {
    const request = route.request();
    if (request.method() === "PUT") {
      const body = request.postDataJSON() as {
        path: string;
        content?: string;
        if_match?: string;
      };
      const currentHash = fileHash(fileStore.get(body.path) ?? "");
      if (body.if_match !== undefined && body.if_match !== currentHash) {
        return route.fulfill({
          status: 409,
          contentType: "application/json",
          body: JSON.stringify({ error: "file changed on disk since it was read" }),
        });
      }
      const content = body.content ?? "";
      fileStore.set(body.path, content);
      fileMtime += 1;
      return route.fulfill({
        json: { ok: true, bytes: content.length, hash: fileHash(content), mtime: fileMtime },
      });
    }
    const path = new URL(request.url()).searchParams.get("path") ?? "";
    const content = fileStore.get(path) ?? "";
    return route.fulfill({
      json: {
        path,
        size: content.length,
        content,
        content_base64: null,
        is_binary: false,
        mtime: fileMtime,
        hash: fileHash(content),
      },
    });
  });
  await registerRoute("**/api/tasks**", async (route) => route.fulfill({ json: [] }));
  // Push endpoints: Settings polls these on open. Answer them deterministically
  // so opening Settings never reaches a live backend. `pushSubscriptions` lets
  // a test model the server having dropped this device's subscription.
  await registerRoute("**/api/push/list", async (route) =>
    route.fulfill({ json: metrics.pushSubscriptions ?? [] }),
  );
  await registerRoute("**/api/push/public-key", async (route) =>
    route.fulfill({ json: { vapid_public_key: "", fcm_enabled: false } }),
  );
  await registerRoute("**/api/push/subscribe", async (route) =>
    route.fulfill({ json: { id: "browser-sub" } }),
  );
  await registerRoute("**/api/push/unsubscribe", async (route) =>
    route.fulfill({ json: { ok: true } }),
  );
  await registerRoute("**/api/push/test", async (route) =>
    route.fulfill({ json: { ok: 0, fail: 0, queued: 0 } }),
  );
  // Agent-task fixtures: only wired when a caller hands over tasks, so the
  // routes never shadow tests that stub `/api/agent-tasks` themselves.
  const agentTaskUpdates: Record<string, unknown>[] = [];
  const agentTaskRuns: string[] = [];
  if (agentTasks) {
    const tasks = agentTasks.map((task) => ({ ...task }));
    await registerRoute("**/api/agent-tasks", async (route) =>
      route.fulfill({ json: tasks }),
    );
    await registerRoute("**/api/agent-tasks/*/run", async (route) => {
      const id = new URL(route.request().url()).pathname.split("/").at(-2) ?? "";
      agentTaskRuns.push(id);
      return route.fulfill({ json: {
        id: `run-${agentTaskRuns.length}`,
        task_id: id,
        started_at: "2026-08-18T00:10:00Z",
        trigger: "manual",
        session_id: `run-session-${agentTaskRuns.length}`,
        session_name: `on-demand-${agentTaskRuns.length}`,
        prompt_file: "prompt.txt",
        context_file: "context.txt",
        status: "running",
        completed_at: null,
        exit_code: null,
        summary: null,
        findings: [],
      } });
    });
    await registerRoute("**/api/agent-tasks/*", async (route) => {
      const method = route.request().method();
      const id = new URL(route.request().url()).pathname.split("/").at(-1) ?? "";
      const index = tasks.findIndex((task) => task.id === id);
      if (method === "PATCH") {
        const body = route.request().postDataJSON() as Record<string, unknown>;
        agentTaskUpdates.push(body);
        if (index >= 0) tasks[index] = { ...tasks[index], ...body };
        return route.fulfill({ json: tasks[index] ?? { ...tasks[0], ...body } });
      }
      return route.fulfill({ json: tasks[index] ?? tasks[0] });
    });
  }
  // The shell's places rail and assistant poll the engine on mount. Left
  // unstubbed they reach whatever backend sits behind the dev proxy, and a
  // 401 from it flips the whole app to the login gate mid-test — so answer
  // them with benign, empty engine state.
  await registerRoute("**/api/git/status**", async (route) => route.fulfill({ json: {
    repo: "", is_repo: false, branch: "", ahead: 0, behind: 0, entries: [],
  } }));
  await registerRoute("**/api/assistant/history**", async (route) => route.fulfill({ json: {
    transcript: [],
  } }));
  // A default assistant turn: a plain acknowledgement. Individual tests
  // register their own `**/api/assistant/message` handler afterwards to fail
  // it, hold it open, or answer with Markdown — a later route wins in
  // Playwright, so those overrides take precedence over this default (#242).
  await registerRoute("**/api/assistant/message", async (route) => route.fulfill({ json: {
    reply: "Acknowledged.", pending_action: null, tool_trace: [],
  } }));
  await registerRoute("**/api/vogt/**", async (route) => {
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
    if (url.pathname.endsWith("/work/update")) {
      const body = request.postDataJSON() as Record<string, unknown>;
      updateRequests.push(body);
      const source = (metrics.boardItems ?? boardItems).find((item) => item.ref === body.ref) ?? boardItems[0];
      // Echo back the item with the edited fields applied, the way work.update
      // answers: the detail page keeps the server's version, not what was typed.
      return route.fulfill({ json: {
        item: {
          ...source,
          ref: body.ref,
          title: body.title ?? source.title,
          priority: body.priority ?? source.priority,
          updated_at: "2026-08-17T11:00:00Z",
        },
        comments: [],
        sessions: [],
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
      if (request.method() === "POST") {
        // project.register (#292): echo the registration back as a project.
        const body = request.postDataJSON() as { name?: string; root_path?: string };
        registerRequests.push(body as Record<string, unknown>);
        const slug = String(body.name ?? "project")
          .toLowerCase()
          .replace(/[^a-z0-9]+/g, "-")
          .replace(/^-|-$/g, "");
        return route.fulfill({ json: { project: { slug, name: body.name ?? slug } } });
      }
      if (metrics.projectsTotal === null) return route.fulfill({ status: 503, body: "projects unavailable" });
      const projects = metrics.projects ?? [{ slug: "vogt", name: "Vogt", root_path: "/workspace/vogt" }];
      return route.fulfill({ json: {
      projects,
      total: metrics.projectsTotal,
      } });
    }
    // The setup steps (#292): forge account linking, the repo picker, the
    // first import, and the sweep + coverage that follow it.
    if (url.pathname.endsWith("/forge/accounts")) {
      if (request.method() === "POST") {
        const body = request.postDataJSON() as Record<string, unknown>;
        linkRequests.push(body);
        return route.fulfill({ json: {
          host: "github.com", login: "ada", scopes: "repo", linked: true,
        } });
      }
      return route.fulfill({ json: { accounts: metrics.forgeAccounts ?? [] } });
    }
    if (url.pathname.endsWith("/forge/repos")) {
      return route.fulfill({ json: {
        repos: metrics.forgeRepos ?? [
          {
            owner: "ada", name: "engine", default_branch: "main",
            visibility: "private", url: "https://github.com/ada/engine",
            already_registered: false,
          },
        ],
        login: "ada",
        detail: null,
      } });
    }
    if (url.pathname.endsWith("/projects/import")) {
      const body = request.postDataJSON() as Record<string, unknown>;
      importRequests.push(body);
      return route.fulfill({ json: {
        project: { slug: "engine", name: "engine" },
        remote: String(body.repo ?? ""),
        root_path: "/imports/engine",
        cloned: true,
      } });
    }
    if (url.pathname.endsWith("/sweep")) {
      const body = request.postDataJSON() as Record<string, unknown>;
      sweepRequests.push(body);
      return route.fulfill({ json: {
        scope: "project:engine", projects: 1, subjects: 12, dep_refs: 3, reports: [],
      } });
    }
    if (url.pathname.endsWith("/coverage")) {
      return route.fulfill({ json: {
        collectors: [
          { collector: "git_local", status: "current", last_swept_at: null, age_seconds: 0, projects: 1 },
          { collector: "markers", status: "current", last_swept_at: null, age_seconds: 0, projects: 1 },
        ],
        swept_project_ids: ["prj-1"],
        unswept_project_ids: [],
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
          body: metrics.workBody ?? "", state: "open", priority: "normal", project_slug: "vogt",
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
    if (url.pathname.endsWith("/actors")) return route.fulfill({ json: { actors: metrics.actors ?? [] } });
    if (url.pathname.endsWith("/vogt/observations")) {
      return route.fulfill({ json: { observations: [], total: 0 } });
    }
    if (url.pathname.endsWith("/vogt/sessions")) {
      return route.fulfill({ json: { sessions: [], engine: null } });
    }
    return route.fulfill({ json: {} });
  });
  return {
    inboxCalls: () => inboxCalls,
    createdSessions: () => createdSessions,
    sessionDeletes: () => sessionDeletes,
    bootstrapRequests,
    linkRequests,
    registerRequests,
    importRequests,
    sweepRequests,
    boardRequests,
    transitionRequests,
    updateRequests,
    sessions,
    sessionInputs,
    agentTaskUpdates,
    agentTaskRuns,
    // Simulate a change to a file made outside this editor — the next save
    // against the pre-change hash will 409 (#237).
    externalChange: (path: string, content: string) => {
      fileStore.set(path, content);
      fileMtime += 1;
    },
  };
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

// The first-run wizard (#292). A fresh instance has no tokens, so the core
// reports install mode and the shell leads with the wizard rather than a
// demand for a token nobody has minted yet. Verified at the two shapes the
// product ships: a 1440×900 desktop and a 390×844 phone.
test("First run: the wizard claims the instance, shows the secret once, and signs in", async ({ page }) => {
  if (test.info().project.name === "desktop") {
    await page.setViewportSize({ width: 1440, height: 900 });
  } else {
    await page.setViewportSize({ width: 390, height: 844 });
  }
  const fixtures = await installFixtures(page, {}, [], undefined, {
    installMode: true,
    noToken: true,
  });
  await page.goto("/");

  await expect(page.getByRole("heading", { name: "Claim this instance" })).toBeVisible();
  // The whole journey is named up front, identity first.
  const steps = page.getByRole("list", { name: "Setup steps" });
  await expect(steps).toContainText("Identity");
  await expect(steps).toContainText("Forge");
  await expect(steps).toContainText("First project");
  // The wizard stays inside the viewport at both shapes.
  const overflow = await page.evaluate(
    () => document.documentElement.scrollWidth > document.documentElement.clientWidth + 1,
  );
  expect(overflow, "the wizard must not overflow horizontally").toBe(false);

  await page.getByLabel("Your name").fill("Ada Lovelace");
  await page.getByRole("button", { name: "Claim instance & mint my token" }).click();

  await expect(page.getByRole("heading", { name: "Welcome, Ada Lovelace" })).toBeVisible();
  await expect(page.getByTestId("setup-secret")).toContainText(
    "vogt_browser-test-first-run-secret",
  );
  expect(fixtures.bootstrapRequests).toEqual([{ display_name: "Ada Lovelace" }]);
  // The CLI/MCP equivalents are one disclosure away.
  await page.getByText("Use it from a terminal or an agent").click();
  await expect(page.getByText("vogt-mcp-remote")).toBeVisible();

  // Continue hands the minted secret to the ordinary sign-in path: the shell
  // comes up authenticated with it stored where a pasted token lives, and the
  // pending flag lands the fresh operator on the remaining setup steps.
  await page.getByRole("button", { name: "Continue to Vogt" }).click();
  await expect(page).toHaveURL(/#\/setup$/);
  await expect(
    page.getByRole("heading", { name: "Setup", exact: true }),
  ).toBeVisible();
  expect(await page.evaluate(() => localStorage.getItem("vogt.token"))).toBe(
    "vogt_browser-test-first-run-secret",
  );
});

// The setup steps at #/setup (#292 increment 3): forge link and first
// project, each with a visible pass/fail carrying the server's own words,
// then the first sweep and the coverage it earns. Verified at 1440×900 and,
// via the phone project, 390×844.
test("Setup steps: link the forge, import the first project, sweep, and see coverage", async ({ page }) => {
  if (test.info().project.name === "desktop") {
    await page.setViewportSize({ width: 1440, height: 900 });
  } else {
    await page.setViewportSize({ width: 390, height: 844 });
  }
  const fixtures = await installFixtures(page);
  await page.goto("/#/setup");

  await expect(page.getByRole("heading", { name: "Setup" })).toBeVisible();
  await expect(page.getByRole("list", { name: "Setup steps" })).toContainText("Identity");

  // Forge: paste a PAT with a typed reason, and the step reports as whom.
  await page.getByLabel("Personal Access Token").fill("ghp_wizard");
  await page
    .getByLabel("Forge step")
    .getByLabel("Reason (audited)")
    .fill("first-run: my own attribution");
  await page.getByRole("button", { name: "Link account" }).click();
  await expect(page.getByText("Linked as ada — token scopes: repo.")).toBeVisible();
  expect(fixtures.linkRequests).toEqual([
    { token: "ghp_wizard", reason: "first-run: my own attribution" },
  ]);

  // First project: pick a repository and import it, reason attached.
  await page.getByRole("button", { name: "Browse my repositories" }).click();
  await page.getByRole("radio").first().check();
  await page
    .getByLabel("First project step")
    .getByLabel("Reason (audited)")
    .first()
    .fill("first project for this instance");
  await page.getByRole("button", { name: "Import it" }).click();
  await expect(page.getByText(/Imported engine/)).toBeVisible();
  expect(fixtures.importRequests[0]).toMatchObject({
    repo: "https://github.com/ada/engine",
    reason: "first project for this instance",
  });

  // The first sweep, its numbers, and the coverage table behind them.
  await page
    .getByLabel("First project step")
    .getByLabel("Reason (audited)")
    .last()
    .fill("baseline evidence");
  await page.getByRole("button", { name: "Run the first sweep" }).click();
  await expect(page.getByText(/Swept 1 project/)).toBeVisible();
  await expect(page.getByRole("table", { name: "Collector coverage" })).toContainText("git_local");
  expect(fixtures.sweepRequests[0]).toMatchObject({
    project: "engine",
    reason: "baseline evidence",
  });

  // The surface never overflows horizontally, at either shape.
  const overflow = await page.evaluate(
    () => document.documentElement.scrollWidth > document.documentElement.clientWidth + 1,
  );
  expect(overflow, "the setup surface must not overflow horizontally").toBe(false);

  // Finishing lands on Projects with the pending flag cleared.
  await page.getByRole("button", { name: "Finish setup" }).click();
  await expect(page).toHaveURL(/#\/projects$/);
  expect(await page.evaluate(() => localStorage.getItem("vogt.setup.pending"))).toBeNull();
});

test("First run: a closed install mode means the ordinary sign-in gate", async ({ page }) => {
  await installFixtures(page, {}, [], undefined, {
    installMode: false,
    noToken: true,
  });
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "Sign in to Vogt" })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Claim this instance" })).not.toBeVisible();
});

test("First run: a reader who already holds a token can reach the gate from the wizard", async ({ page }) => {
  await installFixtures(page, {}, [], undefined, {
    installMode: true,
    noToken: true,
  });
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "Claim this instance" })).toBeVisible();
  await page.getByRole("button", { name: "Sign in instead" }).click();
  await expect(page.getByRole("heading", { name: "Sign in to Vogt" })).toBeVisible();
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

// #229: the chrome over the board was pushing the first card past the fold —
// a legend, a saved-lenses row and a success banner that all belonged
// elsewhere. With those moved into the `?` help and the `+ Filter` panel, the
// first card sits inside the first screen; and the card, being a button, opens
// on a single click of its body and on Space, not only by double-click.
test("Board leads with the first card, which opens on a click and on Space", async ({ page }) => {
  await installFixtures(page);
  await page.goto("/#/board");
  await expect(page.getByRole("heading", { name: "Board" })).toBeVisible();
  const card = page.locator(".board-card").filter({ hasText: "Measured board card" });
  await expect(card).toBeVisible();

  // The first card is above the fold: measured tops were 318px on the desk and
  // 470px on the phone before the trim.
  // #229 is about the chrome the *board* puts before the first card — the
  // legend, the saved-lenses row, the boxed filter bar, the success banner. The
  // app shell above the surface (the phone's Go-to bar, and a "Realtime
  // connection lost" banner that shows only because this harness serves no live
  // event stream) is not the board's to trim, so the budget is measured from
  // the top of the board surface rather than from the viewport.
  const chrome = await page.evaluate(() => {
    const surface = document.querySelector(".vogt-surface.board")!;
    const cardEl = document.querySelector(".board-card")!;
    return (
      cardEl.getBoundingClientRect().top - surface.getBoundingClientRect().top
    );
  });
  const ceiling = test.info().project.name === "phone" ? 300 : 200;
  expect(chrome, `board chrome before first card ${chrome} under ${ceiling}`)
    .toBeLessThan(ceiling);

  // A single click of the card body opens the item.
  await card.locator(".board-card-title").click();
  await expect(page).toHaveURL(/#\/w\/WI-7/);

  // And Space on the focused card opens it, the way a button is operated.
  await page.goBack();
  await expect(card).toBeVisible();
  await card.focus();
  await page.keyboard.press("Space");
  await expect(page).toHaveURL(/#\/w\/WI-7/);
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
 * Saved lenses live inside the `+ Filter` disclosure at every width now (#229),
 * so the first screen belongs to the work. Anything reaching for a lens control
 * has to open the panel first, on a desk as much as on a phone.
 */
async function openFilterPanel(group: Locator): Promise<void> {
  const add = group.getByRole("button", { name: "+ Filter", exact: true });
  if ((await add.getAttribute("aria-expanded")) === "false") await add.click();
}

test("Board progressive filters survive reload, history, and saved-lens recall", async ({ page }) => {
  await installFixtures(page);
  await page.goto("/#/board?project=vogt&lanes=project");
  await expect(page.getByRole("heading", { name: "Board" })).toBeVisible();

  const filters = page.getByRole("group", { name: "Board filters" });
  await expect(filters.getByText("Project: Vogt")).toBeVisible();
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

  await filters.getByRole("button", { name: "Remove filter Project: Vogt" }).click();
  await expect(page).not.toHaveURL(/project=vogt/);
  await page.reload();
  await expect(filters.getByText("Type: feature")).toBeVisible();
  await expect(filters.getByText("Swimlanes: project")).toBeVisible();

  await openFilterPanel(filters);
  await page.locator(".board-saved-recall").click();
  await expect(page).toHaveURL(/project=vogt/);
  await expect(page).toHaveURL(/kind=feature/);
  await expect(page).toHaveURL(/lanes=project/);

  await page.goto("/#/board?label=infra");
  await expect(filters.getByText("Label: infra")).toBeVisible();
  await page.goBack();
  await expect(filters.getByText("Project: Vogt")).toBeVisible();
  await expect(filters.getByText("Type: feature")).toBeVisible();
  await page.goForward();
  await expect(filters.getByText("Label: infra")).toBeVisible();
});

// #215: opening a work item unmounts the Board/Backlog surface. Browser Back
// keeps the query, but the rail/palette/bottom-bar links were bare `#/board`,
// so returning through them remounted the surface against an empty query and
// dropped the filter set. The links now carry the last query the surface
// wrote, so the filter survives a rail/bottom-bar return too.
test("Board filters survive a return through the rail, not just browser Back", async ({ page }) => {
  const nav = test.info().project.name === "phone" ? ".phone-bottom-nav" : ".places-nav";
  await installFixtures(page);
  await page.goto("/#/board?project=vogt");
  await expect(page.getByRole("heading", { name: "Board" })).toBeVisible();
  const filters = page.getByRole("group", { name: "Board filters" });
  await expect(filters.getByText("Project: Vogt")).toBeVisible();

  // Open the item, which unmounts the Board (the surface mounts only while the
  // path is exactly `/board`).
  await page.locator(".board-card").filter({ hasText: "Measured board card" })
    .locator(".board-card-title").click();
  await expect(page).toHaveURL(/#\/w\/WI-7/);
  await expect(page.locator(".vogt-surface.board")).toHaveCount(0);

  // Return through the rail/bottom-bar link — not browser Back.
  await page.locator(nav).getByRole("link", { name: /^Board/ }).click();

  await expect(page).toHaveURL(/project=vogt/);
  await expect(page.getByRole("heading", { name: "Board" })).toBeVisible();
  await expect(filters.getByText("Project: Vogt")).toBeVisible();
});

test("Backlog filters survive a return through the rail, not just browser Back", async ({ page }) => {
  const nav = test.info().project.name === "phone" ? ".phone-bottom-nav" : ".places-nav";
  await installFixtures(page);
  await page.goto("/#/backlog?project=vogt");
  await expect(page.getByRole("heading", { name: "Backlog" })).toBeVisible();
  const filters = page.getByRole("group", { name: "Backlog filters" });
  await expect(filters.getByText("Project: Vogt")).toBeVisible();

  // Open the item from its ref link, which unmounts the Backlog.
  await page.locator(".vogt-backlog-link").first().click();
  await expect(page).toHaveURL(/#\/w\/WI-7/);
  await expect(page.locator(".vogt-surface.vogt-backlog")).toHaveCount(0);

  await page.locator(nav).getByRole("link", { name: /^Backlog/ }).click();

  await expect(page).toHaveURL(/project=vogt/);
  await expect(page.getByRole("heading", { name: "Backlog" })).toBeVisible();
  await expect(filters.getByText("Project: Vogt")).toBeVisible();
});

// #217: a ref is a machine handle, not a name. Every surface that renders a
// project slug or an actor identity ref resolves it through the loaded lists to
// the human name, keeps the raw ref on the element's `title`, and — for a
// project — links to that project.
test("Board card shows resolved project and assignee names, with the raw refs kept in the title", async ({ page }) => {
  await installFixtures(page, {}, [], undefined, {
    projects: [{ slug: "vogt", name: "Vogt", root_path: "/workspace/vogt" }],
    actors: [{ identity_ref: "local:ana", display_name: "Ana" }],
    boardItems: [{
      ...boardItems[0], ref: "WI-7", title: "Named refs card", state: "open",
      project_slug: "vogt", assignee_identity_ref: "local:ana",
    }],
  });
  await page.goto("/#/board");
  const card = page.locator(".board-card").filter({ hasText: "Named refs card" });
  await expect(card).toBeVisible();
  const meta = card.locator(".board-card-meta");

  // The project reads as its name, links to the project, and carries the slug.
  const projectLink = meta.locator("a.board-card-project");
  await expect(projectLink).toHaveText("Vogt");
  await expect(projectLink).toHaveAttribute("title", "vogt");
  await expect(projectLink).toHaveAttribute("href", "#/projects?p=vogt");

  // The assignee reads as its display name, with the identity ref in the title.
  const assignee = meta.locator("span[title='local:ana']");
  await expect(assignee).toHaveText("Ana");
});

// #217: the counts on a project's Work panel are the way into that project's
// board, backlog and audit, filtered to it.
test("Projects Work panel cross-links the counts to the board, backlog and audit for the project", async ({ page }) => {
  test.skip(test.info().project.name === "phone", "The desktop panel layout is enough to prove the links (#217)");
  await installFixtures(page, {}, [], undefined, {
    projects: [{ slug: "vogt", name: "Vogt", root_path: "/workspace/vogt" }],
  });
  await page.route("**/api/vogt/projects/brief**", async (route) => route.fulfill({ json: {
    project: { slug: "vogt", name: "Vogt", lifecycle_state: "active", trust_state: "verified" },
    open_work: 3,
    open_bugs: 1,
    by_state: { open: 2, in_progress: 1 },
    by_kind: { feature: 2, bug: 1 },
    compliance_status: "not_applicable",
    freshness: { status: "fresh" },
  } }));
  // The overview also draws a compliance panel; give it a well-formed answer so
  // the panel renders rather than the whole detail erroring on empty data.
  await page.route("**/api/vogt/compliance**", async (route) => route.fulfill({ json: {
    contract_version: "1.0", checked_at: "2026-08-17T10:00:00Z", age_seconds: 0,
    failing: [], detail: null,
  } }));
  await page.goto("/#/projects?p=vogt");

  const work = page.locator(".vogt-projects-panel").filter({ hasText: "Work" });
  await expect(work).toBeVisible();
  // The four cross-links carry the project as `?project=vogt`.
  await expect(work.locator('a[href="#/board?project=vogt"]')).toHaveText("3");
  await expect(work.locator('a[href="#/backlog?view=bugs&project=vogt"]')).toHaveText("1");
  await expect(work.locator('a[href="#/board?project=vogt&state=open"]')).toContainText("open: 2");
  await expect(work.locator('a[href="#/audit?project=vogt"]')).toBeVisible();

  // Following the open-work link lands on the Board, scoped to the project, and
  // the filter chip names the project it landed on.
  await work.locator('a[href="#/board?project=vogt"]').click();
  await expect(page).toHaveURL(/project=vogt/);
  await expect(page.getByRole("heading", { name: "Board" })).toBeVisible();
  await expect(
    page.getByRole("group", { name: "Board filters" }).getByText("Project: Vogt"),
  ).toBeVisible();
});

// #227: the registry is where you go to find a project, so it filters.
test("Projects registry narrows the list to a name/slug search", async ({ page }) => {
  await installFixtures(page, {}, [], undefined, {
    projectsTotal: 3,
    projects: [
      { slug: "vogt", name: "Vogt", root_path: "/workspace/vogt" },
      { slug: "cadastre", name: "Cadastre", root_path: "/workspace/cadastre" },
      { slug: "beta", name: "Beta", root_path: "/workspace/beta" },
    ],
  });
  await page.goto("/#/projects");
  const cards = page.locator(".vogt-projects-card");
  await expect(cards).toHaveCount(3);

  await page.getByRole("searchbox", { name: "Filter projects" }).fill("cad");
  await expect(cards).toHaveCount(1);
  await expect(cards.first()).toContainText("Cadastre");
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

  const trigger = page.getByRole("button", { name: "Inbox actions" }).first();
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
      return {
        entryTop: entry.getBoundingClientRect().top,
        entryBottom: entry.getBoundingClientRect().bottom,
        viewportHeight: document.documentElement.clientHeight,
        // The attention answer leads; coverage and provenance stay below it.
        ordered: Boolean(
          entry.compareDocumentPosition(coverage) & Node.DOCUMENT_POSITION_FOLLOWING,
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

test("Inbox is dense, keeps the Source select on screen, links out and sticks its batch bar", async ({ page }) => {
  test.skip(test.info().project.name === "phone", "Desktop density and the Source select live on the desktop shell");
  await installFixtures(page);
  await page.setViewportSize({ width: 1280, height: 900 });
  await page.goto("/#/inbox");

  const entries = page.locator(".inbox-entry");
  await expect(entries).toHaveCount(4);

  // Density (#220): with every evidence disclosure collapsed, four entries
  // fit within one screen. Force the drift entry's auto-opened evidence shut
  // to measure the collapsed layout the requirement is about.
  await page.locator(".inbox-list").evaluate((list) => {
    list
      .querySelectorAll("details.inbox-evidence[open]")
      .forEach((node) => node.removeAttribute("open"));
  });
  const bottoms = await entries.evaluateAll((nodes) =>
    nodes.map((node) => node.getBoundingClientRect().bottom),
  );
  expect(Math.max(...bottoms)).toBeLessThanOrEqual(page.viewportSize()!.height);

  // The Source select keeps a right gutter rather than clipping at the edge.
  const selectBox = (await page.locator(".inbox-filter select").boundingBox())!;
  expect(selectBox.x + selectBox.width).toBeLessThan(page.viewportSize()!.width);

  // The source-linked entry offers an out-to-source deep link…
  const link = page.getByRole("link", { name: /Open on github/ });
  await expect(link).toBeVisible();
  await expect(link).toHaveAttribute("target", "_blank");
  await expect(link).toHaveAttribute("href", "https://github.example/org/repo/pull/42");

  // …and the entry with nothing in-app behind it disables Open entry.
  const orphan = page.locator('[data-entry-key="ci:build-99"]');
  await expect(orphan.getByRole("button", { name: "Open entry" })).toBeDisabled();

  // Selecting an entry raises a batch bar stuck to the top of the list.
  await page.getByLabel(`Select ${inboxEntry.title}`).check();
  const bar = page.locator(".inbox-batch-bar");
  await expect(bar).toBeVisible();
  expect(await bar.evaluate((element) => getComputedStyle(element).position)).toBe("sticky");
  const barBox = (await bar.boundingBox())!;
  const listBox = (await page.locator(".inbox-list").boundingBox())!;
  expect(barBox.y).toBeLessThan(listBox.y);
});

test("Phone Inbox shows a denser, collapsed-evidence list", async ({ page }) => {
  test.skip(test.info().project.name !== "phone", "The phone screenshot belongs to the phone project");
  await installFixtures(page);
  await page.goto("/#/inbox");
  await expect(page.locator(".inbox-entry")).toHaveCount(4);
  // Collapse the auto-opened drift evidence so the shot is of the dense list.
  await page.locator(".inbox-list").evaluate((list) => {
    list
      .querySelectorAll("details.inbox-evidence[open]")
      .forEach((node) => node.removeAttribute("open"));
  });
  await expect(page.locator(".inbox-list")).toHaveScreenshot("inbox-dense-list-phone.png");
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
      // "goto" is the phone's inline Go to… (WI-75); it exists only on a
      // narrow client, and the filter drops it wherever it is absent.
      const expected = ["title", "goto", "honesty", "spacer", "controls", "action", "detail"]
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

/**
 * The same slot-order and geometry contract, now that Projects and the Work
 * item wear the shared header too (#228). Kept a separate, shorter walk rather
 * than folded into the spec above: these two surfaces make far more calls per
 * mount, and stretching one test to six routes across two widths and five
 * zooms made it flaky under a loaded box, not more truthful.
 */
test("Projects and Work item headers answer the shared geometry contract", async ({ page }) => {
  await installFixtures(page, {}, [liveSession]);
  const routes = [
    { goto: "/#/projects", content: ".vogt-projects-list" },
    { goto: "/#/w/WI-7", content: ".wid-facts" },
  ] as const;
  const zooms = ["80%", "100%", "150%", "200%"] as const;
  const widths = test.info().project.name === "phone" ? [390] : [1280, 768];

  for (const width of widths) {
    await page.setViewportSize({ width, height: 900 });
    for (const route of routes) {
      await page.goto(route.goto);
      const header = page.locator("[data-surface-header]:visible");
      await expect(header).toBeVisible();
      await expect(page.locator(route.content).first()).toBeVisible();

      const more = header.locator(".surface-header-more");
      if (await more.count()) {
        if ((await more.getAttribute("aria-expanded")) === "false") await more.click();
      }

      const slots = await header.locator(":scope > [data-surface-header-slot]")
        .evaluateAll((elements) => elements.map((element) =>
          element.getAttribute("data-surface-header-slot"),
        ));
      expect(slots).toEqual(
        ["title", "goto", "honesty", "spacer", "controls", "action", "detail"]
          .filter((slot) => slots.includes(slot)),
      );

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

test("Phone More sheet reaches every remaining place plus Settings and Sign out", async ({ page }) => {
  test.skip(test.info().project.name !== "phone", "The More sheet is the phone bottom bar's fifth slot");
  await installFixtures(page, { assistant_enabled: true, gui_stream_available: true });
  await page.goto("/#/sessions");

  const nav = page.getByRole("navigation", { name: "Primary navigation" });
  const more = nav.getByRole("button", { name: "More" });
  await expect(more).toBeVisible();
  // One tap opens the sheet.
  await more.click();
  const sheet = page.getByRole("dialog", { name: "More places and actions" });
  await expect(sheet).toBeVisible();

  // Every place the four-slot bar cannot reach is present, gated rows included.
  for (const place of ["Projects", "Audit", "Git", "History", "Tasks", "GUI stream", "Assistant"]) {
    await expect(sheet.getByRole("link", { name: place, exact: true })).toBeVisible();
  }
  // Settings and Sign out are reachable without a command-palette round trip.
  await expect(sheet.getByRole("button", { name: "Settings", exact: true })).toBeVisible();
  await expect(sheet.getByRole("button", { name: "Sign out", exact: true })).toBeVisible();

  await expect(sheet).toHaveScreenshot("phone-more-sheet.png");

  // A second tap lands on the place: two taps from any surface reaches it.
  await sheet.getByRole("link", { name: "Audit", exact: true }).click();
  await expect(page).toHaveURL(/#\/audit$/);
  await expect(sheet).toHaveCount(0);
});

test("Desktop Ctrl+B toggles the Places rail while palette and help stay bound", async ({ page }) => {
  test.skip(test.info().project.name === "phone", "The rail is a desktop surface; the phone uses the More sheet");
  await installFixtures(page);
  await page.goto("/#/board");

  const rail = page.locator(".places-rail");
  await expect(rail).toBeVisible();

  // Ctrl/Cmd+B hides the rail, and again shows it — the keyboard equivalent of
  // the rail's own collapse/reopen controls.
  await page.keyboard.press("ControlOrMeta+B");
  await expect(rail).toBeHidden();
  await expect(page.getByRole("button", { name: "Show the Places rail" })).toBeVisible();
  await page.keyboard.press("ControlOrMeta+B");
  await expect(rail).toBeVisible();

  // The new binding does not shadow the existing global shortcuts.
  await page.keyboard.press("ControlOrMeta+K");
  await expect(page.getByRole("dialog", { name: "Command palette" })).toBeVisible();
  await page.keyboard.press("Escape");
  await page.keyboard.press("?");
  await expect(page.getByRole("dialog", { name: "Keyboard Shortcuts" })).toBeVisible();
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

test("Command palette lists the machine places, shows shortcuts, and ranks a session above a work item", async ({ page }) => {
  await installFixtures(
    page,
    {},
    [
      {
        id: "ses-deploy",
        name: "deploy-preview",
        cwd: "/workspace/vogt",
        activity: "idle",
        exit_code: null,
        scrollback_bytes: 0,
        created_at: "2026-08-18T08:00:00Z",
      },
    ],
  );
  // The palette lists work items from the /work endpoint; give it one whose
  // title contains the query word so it competes with the session.
  await page.route(/\/api\/vogt\/work(\?|$)/, async (route) =>
    route.fulfill({
      json: {
        items: [
          {
            ref: "WI-42", title: "deploy the shell", kind: "feature", state: "open",
            priority: "normal", project_slug: "vogt", trust_state: "verified", labels: [],
          },
        ],
        total: 1,
        freshness: { status: "fresh" },
      },
    }),
  );
  await page.goto("/#/sessions");

  await page.getByRole("button", { name: "Go to…" }).click();
  const palette = page.getByRole("dialog", { name: "Command palette" });
  await expect(palette).toBeVisible();

  // The machine places the rail and phone bar expose are reachable by name in
  // the palette too (#230): Inbox, Sessions and History were all missing.
  for (const label of ["Open Inbox", "Open Sessions", "Open History"]) {
    await expect(palette.getByText(label, { exact: true })).toBeVisible();
  }

  // A command with a keyboard binding shows it on the row. New Terminal Session
  // is bound to Ctrl/Cmd+Shift+T.
  const newSession = palette.getByRole("option", { name: /New Terminal Session/ });
  await expect(newSession.locator("kbd", { hasText: "Ctrl/Cmd" })).toBeVisible();

  // Typing a session-name prefix surfaces that session above the work item that
  // merely contains the word: a name match beats a description/word match, and
  // sessions are listed before work items on a tie.
  await palette.getByRole("combobox", { name: "Search commands" }).fill("deploy");
  await expect(palette.getByRole("option").first()).toContainText("deploy-preview");
  await expect(palette.getByText("deploy the shell")).toBeVisible();
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

// Settings is reachable from the desktop rail directly and from the phone
// bottom bar's More sheet. Open it the way the current project's user would.
async function openSettings(page: Page): Promise<void> {
  if (test.info().project.name === "phone") {
    await page
      .getByRole("navigation", { name: "Primary navigation" })
      .getByRole("button", { name: "More" })
      .click();
    const sheet = page.getByRole("dialog", { name: "More places and actions" });
    await sheet.getByRole("button", { name: "Settings", exact: true }).click();
  } else {
    await page.getByRole("button", { name: "Settings" }).click();
  }
}

test("Settings keeps its Save/Cancel footer on screen at desktop and phone sizes", async ({ page }) => {
  // #243: the footer must stay reachable, not scroll off the bottom of a long
  // dialog. Asserted at 1440×900 on desktop and at the phone's own viewport.
  if (test.info().project.name === "desktop") {
    await page.setViewportSize({ width: 1440, height: 900 });
  }
  await installFixtures(page);
  await page.goto("/#/sessions");
  await openSettings(page);
  const settings = page.getByRole("dialog", { name: "Settings" });
  await expect(settings).toBeVisible();

  const footer = settings.locator(".settings-modal-footer");
  const cancel = footer.getByRole("button", { name: "Cancel" });
  const save = footer.getByRole("button", { name: /^Save/ });
  await expect(cancel).toBeInViewport();
  await expect(save).toBeInViewport();

  // Scroll the body to its very end; a sticky footer stays put.
  await settings
    .getByRole("button", { name: "Refresh runtime" })
    .scrollIntoViewIfNeeded();
  await expect(cancel).toBeInViewport();
  await expect(save).toBeInViewport();
});

test("Settings confirms every destructive action before it runs", async ({ page }) => {
  test.skip(test.info().project.name === "phone", "Destructive-confirm plumbing is asserted on desktop");
  // Seed a saved auth profile so the Delete-profile path exists.
  await page.addInitScript(() => {
    localStorage.setItem(
      "vogt.authProfiles.v1",
      JSON.stringify([
        { id: "p1", name: "Read only", token: "ro-token", base: "", updated_at: "2026-08-22T00:00:00Z" },
      ]),
    );
  });
  await installFixtures(page);
  await page.goto("/#/sessions");
  await page.getByRole("button", { name: "Settings" }).click();
  const settings = page.getByRole("dialog", { name: "Settings" });
  await expect(settings).toBeVisible();

  const cases = [
    { button: "Sign out & clear saved auth", title: "Sign out of Vogt?" },
    { button: "Clear managed browser data", title: "Clear managed browser data?" },
    { button: "Clean archived history", title: "Purge archived history?" },
    { button: "Delete", title: "Delete this profile?" },
  ];
  for (const one of cases) {
    await settings.getByRole("button", { name: one.button, exact: true }).scrollIntoViewIfNeeded();
    await settings.getByRole("button", { name: one.button, exact: true }).click();
    const confirm = page.getByRole("dialog", { name: one.title });
    await expect(confirm).toBeVisible();
    await expect(confirm.getByRole("button", { name: "Confirm" })).toBeVisible();
    // Cancelling leaves the destructive action undone and Settings still open.
    await confirm.getByRole("button", { name: "Cancel" }).click();
    await expect(confirm).toHaveCount(0);
    await expect(settings).toBeVisible();
  }
});

test("Settings shows the blocked push state and disables Enable when permission is denied", async ({ page }) => {
  test.skip(test.info().project.name === "phone", "Push controls are asserted on the desktop route");
  // Force a denied Notification permission while keeping ServiceWorker and
  // PushManager present, so the push section renders its blocked state rather
  // than the unsupported fallback.
  await page.addInitScript(() => {
    const stub = function Notification() {} as unknown as { permission: string; requestPermission: () => Promise<string> };
    Object.defineProperty(stub, "permission", { get: () => "denied", configurable: true });
    stub.requestPermission = () => Promise.resolve("denied");
    Object.defineProperty(window, "Notification", { value: stub, configurable: true, writable: true });
  });
  await installFixtures(page);
  await page.goto("/#/sessions");
  await page.getByRole("button", { name: "Settings" }).click();
  const settings = page.getByRole("dialog", { name: "Settings" });
  await expect(settings).toBeVisible();

  const enable = settings.getByRole("button", { name: "Enable push" });
  await enable.scrollIntoViewIfNeeded();
  await expect(enable).toBeDisabled();
  await expect(
    settings.getByText("blocked — allow in site settings, then reload"),
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
  // The engine reads only files that exist (missing paths 404). New File relies
  // on that: creating over an occupied path is refused, so a create only lands
  // when the read first 404s.
  const existingFiles = new Set(["src/an-identifiable-long-filename.tsx"]);
  await page.route("**/api/files**", async (route) => {
    if (route.request().method() === "PUT") {
      createdPath = (route.request().postDataJSON() as { path: string }).path;
      return route.fulfill({ json: { ok: true, path: createdPath, bytes: 0 } });
    }
    const path = new URL(route.request().url()).searchParams.get("path") ?? "";
    if (!existingFiles.has(path)) {
      return route.fulfill({ status: 404, json: { error: { message: "not found" } } });
    }
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
  const existing = chooser.getByRole("option", {
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
    localStorage.setItem("vogt.layoutMode.v1", "ide");
  });
  await page.route("**/api/files**", async (route) => route.fulfill({ json: {
    path: "src/an-identifiable-long-filename.tsx",
    size: 26,
    content: "export const answer = 42;\n",
    content_base64: null,
    is_binary: false,
  } }));
  await page.goto("/#/e/src%2Fan-identifiable-long-filename.tsx");

  // On a phone the Files sidebar is an overlay drawer that defaults collapsed
  // (#240) — open it before inspecting the compact hierarchy it holds.
  await page.locator(".editor-sidebar-expand").tap();

  const fileTree = page.locator(".editor-sidebar .file-tree");
  await expect(fileTree).toBeVisible();
  await expect(fileTree.getByRole("heading", { name: "Files" })).toBeVisible();
  // The Files section defaults collapsed; expand it to reach its search box and
  // the rest of the compact control hierarchy.
  if ((await fileTree.getByRole("searchbox", { name: "Search files" }).count()) === 0) {
    await fileTree.getByRole("button", { name: "Files", exact: true }).tap();
  }
  await expect(fileTree.getByRole("searchbox", { name: "Search files" })).toBeVisible();
  await expect(fileTree.getByRole("button", { name: "New file" })).toBeVisible();
  await fileTree.getByRole("button", { name: "More file actions" }).tap();
  await expect(fileTree.getByRole("button", { name: "New folder" })).toBeVisible();
  await expect(fileTree.getByRole("button", { name: "Upload files" })).toBeVisible();
});

test("Phone editor gives the editor the width and floats Files as an overlay drawer (#240)", async ({ page }) => {
  test.skip(test.info().project.name !== "phone", "The overlay drawer is the phone editor layout");
  await installFixtures(page);
  await page.addInitScript(() => {
    localStorage.setItem("vogt.layoutMode.v1", "ide");
  });
  await page.goto("/#/e/src%2Fan-identifiable-long-filename.tsx");

  // With nothing persisted the drawer defaults collapsed on a phone, so the
  // editor keeps the full width instead of the old ~95px sliver.
  await expect(page.locator(".editor-sidebar")).toHaveCount(0);
  await expect(page.locator(".editor-sidebar-expand")).toBeVisible();

  const viewport = page.viewportSize()!;
  const contentWidth = await page.locator(".editor-content").evaluate(
    (element) => element.getBoundingClientRect().width,
  );
  expect(contentWidth).toBeGreaterThanOrEqual(viewport.width * 0.7);

  // Opening the drawer floats it over the editor (position: absolute) rather
  // than pushing the content aside as an inline column, and drops a scrim.
  await page.locator(".editor-sidebar-expand").tap();
  const sidebar = page.locator(".editor-sidebar");
  await expect(sidebar).toBeVisible();
  await expect(page.locator(".editor-sidebar-backdrop")).toBeVisible();
  const position = await sidebar.evaluate((element) => getComputedStyle(element).position);
  expect(position).toBe("absolute");

  // Tapping the scrim (its uncovered strip, right of the drawer and clear of the
  // fixed bottom nav) dismisses the drawer, restoring the full-width editor.
  await page.locator(".editor-sidebar-backdrop").tap({
    position: { x: viewport.width - 12, y: 48 },
  });
  await expect(page.locator(".editor-sidebar")).toHaveCount(0);
});

test("Phone editor splitter resizes on a touch drag (#240)", async ({ page }) => {
  test.skip(test.info().project.name !== "phone", "The touch splitter is exercised on the phone project");
  await installFixtures(page, {}, [], [
    { name: "src", path: "src", is_dir: true },
    { name: "alpha.tsx", path: "src/alpha.tsx", is_dir: false },
    { name: "beta.tsx", path: "src/beta.tsx", is_dir: false },
  ]);
  await page.addInitScript(() => {
    localStorage.setItem("vogt.layoutMode.v1", "ide");
  });
  // Two open editor tabs are what enables a split. The first goto persists the
  // tab; the second reloads and opens the second alongside it.
  await page.goto("/#/e/src%2Falpha.tsx");
  await page.goto("/#/e/src%2Fbeta.tsx");

  await page.locator('[title="Split right"]').click();
  const handle = page.locator(".split-handle");
  await expect(handle).toBeVisible();

  const firstPane = page.locator(".split-pane").first();
  const widthBefore = (await firstPane.boundingBox())!.width;

  // Drive the splitter with synthetic pointer events (a touch drag), the way a
  // finger would — mouse events would not reach the pointer handler.
  await handle.evaluate(async (element) => {
    const rect = element.getBoundingClientRect();
    const cy = rect.top + rect.height / 2;
    const cx = rect.left + rect.width / 2;
    const make = (type: string, x: number) =>
      new PointerEvent(type, {
        bubbles: true,
        cancelable: true,
        clientX: x,
        clientY: cy,
        pointerId: 1,
        pointerType: "touch",
      });
    element.dispatchEvent(make("pointerdown", cx));
    document.dispatchEvent(make("pointermove", cx - 120));
    await new Promise((resolve) => requestAnimationFrame(() => resolve(null)));
    document.dispatchEvent(make("pointerup", cx - 120));
  });

  await expect
    .poll(async () => (await firstPane.boundingBox())!.width)
    .toBeLessThan(widthBefore - 40);
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
  await installFixtures(page, {}, [liveSession], undefined, {}, []);
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

  // The redesigned session bar owns this route. The generic Sessions header
  // must not stack a second title/tools block above it.
  await expect(page.locator(".sessions-header")).toBeHidden();
  const sessionBar = page.locator(".terminal-mobile-header");
  await expect(sessionBar.getByRole("link", { name: "Back to Sessions" })).toBeVisible();
  await expect(sessionBar).toContainText("idle-shell");
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
  const composer = page.getByPlaceholder("Ask about your sessions…");
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
    mask: [
      page.locator(".session-list .state"),
      page.locator(".sessions-header-honesty"),
      page.locator(".session-waiting-age"),
    ],
  });

  await page.goto("/#/t/sess-idle");
  await expect(page.locator(".terminal-host").first()).toBeVisible();
  await expect(page.locator(".terminal-mobile-header")).toHaveScreenshot(
    "sessions-phone-terminal-header.png",
    { mask: [page.locator(".terminal-mobile-session > span")] },
  );
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
  // "Close pane" detaches the active pane now (#212): the session keeps running,
  // so there is no kill confirmation and the layout simply drops a pane.
  await page.getByRole("button", { name: "Close pane" }).click();
  await expect(page.locator(".terminal-pane")).toHaveCount(2);
});

test("a split composes an existing session, and closing a pane detaches it without killing (#212)", async ({ page }) => {
  test.skip(test.info().project.name === "phone", "Split geometry is validated in the desktop browser project");
  const besideSession = {
    ...liveSession,
    id: "beside-session",
    name: "beside-session",
    cwd: "/workspace/other",
  };
  const fixture = await installFixtures(page, {}, [liveSession, besideSession]);
  await page.routeWebSocket(/\/api\/sessions\/[^/]+\/attach$/, (socket) => {
    socket.onMessage(() => undefined);
    socket.send(JSON.stringify({ type: "snapshot-start", scrollback_bytes: 0, scrollback_pos: 0 }));
    socket.send(JSON.stringify({ type: "snapshot-done" }));
  });

  await page.goto("/#/t/browser-session");
  await expect(page.locator(".terminal-pane")).toHaveCount(1);

  // "Split right" offers a picker because a session that is not on screen
  // exists; choosing it composes that existing session into the new pane.
  await page.getByRole("button", { name: "Split right" }).click();
  await page.getByRole("dialog").getByRole("button", { name: /beside-session/ }).click();
  await expect(page.locator(".terminal-pane")).toHaveCount(2);
  // The whole point of #212: composing an existing session spawns nothing.
  expect(fixture.createdSessions()).toBe(0);

  // "Close pane" detaches the active pane: the layout drops back to one pane
  // and the session is left running — no kill, so no DELETE ever fires.
  await page.getByRole("button", { name: "Close pane" }).click();
  await expect(page.locator(".terminal-pane")).toHaveCount(1);
  expect(fixture.sessionDeletes()).toBe(0);
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
  expect(await page.evaluate(() => localStorage.getItem("vogt.terminalFontSize.v1")))
    .toBeNull();

  await page.getByRole("button", { name: "Increase terminal font size" }).click();
  expect(await page.evaluate(() => localStorage.getItem("vogt.terminalFontSize.v1")))
    .toBe("14");

  for (const zoom of ["80%", "100%", "125%", "150%", "200%"] as const) {
    await page.locator("html").evaluate((element, nextZoom) => {
      element.style.zoom = nextZoom;
    }, zoom);
    await expect(page.getByRole("navigation", { name: "Session tools" })).toBeVisible();
    await expect(host).toBeVisible();
  }
});

test("terminal find bar searches the live buffer and navigates matches (#234)", async ({ page }) => {
  test.skip(test.info().project.name === "phone", "Find is exercised in the desktop browser project");
  await installFixtures(page, {}, [liveSession]);
  await page.routeWebSocket(/\/api\/sessions\/[^/]+\/attach$/, (socket) => {
    socket.onMessage(() => undefined);
    socket.send(JSON.stringify({ type: "snapshot-start", scrollback_bytes: 0, scrollback_pos: 0 }));
    // Two lines carrying a unique token, so a search has something to find and
    // more than one match to step through.
    socket.send(Buffer.from("FINDTHISLINE alpha\r\nFINDTHISLINE beta\r\n"));
    socket.send(JSON.stringify({ type: "snapshot-done" }));
  });
  await page.goto("/#/t/browser-session");
  await expect(page.locator(".terminal-host")).toBeVisible();

  await page.getByRole("button", { name: "Find in terminal" }).click();
  const input = page.getByRole("textbox", { name: "Find in terminal" });
  await expect(input).toBeFocused();
  await input.fill("FINDTHISLINE");

  const count = page.locator(".terminal-find-count");
  // The xterm write queue drains on animation frames, so drive Next until the
  // buffer has been indexed and both matches are reported.
  await expect.poll(async () => {
    await page.getByRole("button", { name: "Next match" }).click();
    return (await count.textContent()) ?? "";
  }, { timeout: 5000 }).toContain("of 2");

  await input.focus();
  await page.keyboard.press("Escape");
  await expect(page.locator(".terminal-find-bar")).toHaveCount(0);
});

test("Phone terminal folds pane actions into a ··· menu, keeps errors visible, and never scrolls sideways (#236)", async ({ page }) => {
  test.skip(test.info().project.name !== "phone", "The overflow menu is the narrow toolbar's");
  await installFixtures(page, {}, [liveSession]);
  await stubTerminalAttach(page);
  // A split that fails, so the error span has something to show — the point of
  // keeping it visible on a phone.
  await page.route("**/api/sessions", async (route) => {
    if (route.request().method() !== "POST") return route.fallback();
    return route.fulfill({ status: 500, body: "engine refused the split" });
  });
  await page.goto("/#/t/browser-session");
  await expect(page.locator(".terminal-host").first()).toBeVisible();

  // The pane-management buttons are behind the ··· menu, not inline: the
  // toolbar carries no hidden horizontal scroll strip.
  const overflowToggle = page.getByRole("button", { name: "More terminal actions" });
  await expect(overflowToggle).toBeVisible();
  await expect(page.getByRole("button", { name: "Broadcast off" })).toHaveCount(0);
  const fit = await page.locator(".terminal-workspace-toolbar").evaluate((el) => ({
    scroll: el.scrollWidth,
    client: el.clientWidth,
  }));
  expect(fit.scroll, JSON.stringify(fit)).toBeLessThanOrEqual(fit.client + 1);

  // Open the menu; the collapsed actions live here.
  await overflowToggle.click();
  await expect(page.getByRole("button", { name: "Broadcast off" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Split right" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Close pane" })).toBeVisible();

  // A failed split reports into the error span, which is NOT display:none on a
  // phone any more.
  await page.getByRole("button", { name: "Split right" }).click();
  const error = page.locator(".terminal-workspace-error");
  await expect(error).toBeVisible();
  await expect(error).toContainText("split failed");
});

test("Phone terminal surfaces a Copy chip over a live selection (#236)", async ({ page }) => {
  test.skip(test.info().project.name !== "phone", "The Copy chip is the coarse-pointer copy affordance");
  await installFixtures(page, {}, [liveSession]);
  await page.routeWebSocket(/\/api\/sessions\/[^/]+\/attach$/, (socket) => {
    socket.onMessage(() => undefined);
    socket.send(JSON.stringify({ type: "snapshot-start", scrollback_bytes: 0, scrollback_pos: 0 }));
    // Selectable content on the first row for the drag to land on.
    socket.send(Buffer.from("SELECTME-ALPHA SELECTME-BETA SELECTME-GAMMA\r\n"));
    socket.send(JSON.stringify({ type: "snapshot-done" }));
  });
  await page.goto("/#/t/browser-session");
  const host = page.locator(".terminal-host").first();
  await expect(host).toBeVisible();

  // No selection, no chip.
  await expect(page.locator(".terminal-copy-chip")).toHaveCount(0);

  const box = await host.boundingBox();
  expect(box).not.toBeNull();
  // Drag across the first row to select text. xterm's write queue drains on
  // animation frames, so retry the drag until the selection registers.
  await expect.poll(async () => {
    await page.mouse.move(box!.x + 8, box!.y + 8);
    await page.mouse.down();
    await page.mouse.move(box!.x + 160, box!.y + 8, { steps: 6 });
    await page.mouse.up();
    return page.locator(".terminal-copy-chip").count();
  }, { timeout: 6000 }).toBeGreaterThan(0);

  const chip = page.locator(".terminal-copy-chip");
  await expect(chip).toBeVisible();
  await expect(chip).toHaveText("Copy");
});

test("exited session shows a banner and Remove skips the kill confirm (#235)", async ({ page }) => {
  const exitedSession = {
    ...liveSession,
    id: "exited-session",
    name: "exited-session",
    exit_code: 137,
  };
  await installFixtures(page, {}, [exitedSession]);
  await page.routeWebSocket(/\/api\/sessions\/[^/]+\/attach$/, (socket) => {
    socket.onMessage(() => undefined);
    socket.send(JSON.stringify({ type: "snapshot-start", scrollback_bytes: 0, scrollback_pos: 0 }));
    socket.send(JSON.stringify({ type: "snapshot-done" }));
  });
  await page.goto("/#/t/exited-session");

  const banner = page.locator(".terminal-exited-banner");
  await expect(banner).toContainText("Exited (code 137)");

  await banner.getByRole("button", { name: "Remove" }).click();
  // Removing an exited session is not destructive, so no confirm dialog appears
  // and the session is gone: the workspace drops to its session-unavailable
  // state and the exited banner with it.
  await expect(page.getByText(/no longer available/i)).toBeVisible();
  await expect(page.getByRole("button", { name: "Confirm" })).toHaveCount(0);
  await expect(banner).toHaveCount(0);
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

test("Editor surfaces an on-disk change with Overwrite / Reload (#237)", async ({ page }) => {
  test.skip(test.info().project.name === "phone", "Monaco lifecycle is validated in the desktop browser project");
  const editFile = "src/an-identifiable-long-filename.tsx";
  const fixture = await installFixtures(page, {}, [], undefined, {
    files: { [editFile]: "export const answer = 42;\n" },
  });
  await page.goto("/#/e/src%2Fan-identifiable-long-filename.tsx");
  const editor = page.locator(".monaco-editor");
  await expect(editor).toBeVisible();
  // Make an edit so the tab is dirty and Save is live.
  await editor.click();
  await page.keyboard.type(" // mine");
  const save = page.getByRole("button", { name: "Save", exact: true });
  await expect(save).toBeEnabled();

  // Someone changes the file on disk behind the editor's back.
  fixture.externalChange(editFile, "export const answer = 99;\n");
  await save.click();

  // The save is refused and the choice is surfaced inline, not applied silently.
  const banner = page.getByRole("alert").filter({ hasText: "File changed on disk" });
  await expect(banner).toBeVisible();
  await expect(banner.getByRole("button", { name: "Overwrite" })).toBeVisible();
  await expect(banner.getByRole("button", { name: "Reload" })).toBeVisible();

  // Reload takes the disk's newer content; the tab is clean again.
  await banner.getByRole("button", { name: "Reload" }).click();
  await expect(page.locator(".monaco-editor .view-lines")).toContainText("99");
  await expect(save).toBeDisabled();
});

test("Editor workspace keeps its file-tree expansion across a tab switch (#238)", async ({ page }) => {
  test.skip(test.info().project.name === "phone", "The retained editor workspace is a desktop shell");
  await installFixtures(page, {}, [], undefined, {
    files: { "src/an-identifiable-long-filename.tsx": "export const answer = 42;\n" },
  });
  await page.addInitScript(() => {
    localStorage.setItem("vogt.layoutMode.v1", "ide");
  });
  await page.goto("/#/e/src%2Fan-identifiable-long-filename.tsx");

  const sidebar = page.locator(".editor-sidebar .file-tree");
  await expect(sidebar).toBeVisible();
  await sidebar.getByRole("button", { name: "Files", exact: true }).click();
  await sidebar.getByRole("button", { name: "Expand src" }).click();
  await expect(sidebar.getByText("nested-component.tsx")).toBeVisible();

  // Switch to a non-editor place and back: the workspace stays mounted, so the
  // expansion is retained rather than collapsing (#238).
  await page.goto("/#/sessions");
  await page.goto("/#/e/src%2Fan-identifiable-long-filename.tsx");

  const sidebarAgain = page.locator(".editor-sidebar .file-tree");
  await expect(sidebarAgain.getByRole("button", { name: "Collapse src" })).toBeVisible();
  await expect(sidebarAgain.getByText("nested-component.tsx")).toBeVisible();
});

test("Git Stage all stages every change and unblocks committing (#239)", async ({ page }) => {
  test.skip(test.info().project.name === "phone", "The full Git tab is validated in the desktop browser project");
  await installFixtures(page);
  let staged = false;
  await page.route("**/api/git/status**", async (route) => route.fulfill({ json: {
    repo: "vogt", is_repo: true, branch: "dev", ahead: 0, behind: 0,
    entries: staged
      ? [
          { path: "src/a.ts", index: "M", worktree: " ", kind: "staged" },
          { path: "src/b.ts", index: "A", worktree: " ", kind: "staged" },
        ]
      : [
          { path: "src/a.ts", index: " ", worktree: "M", kind: "modified" },
          { path: "src/b.ts", index: "?", worktree: "?", kind: "untracked" },
        ],
  } }));
  await page.route("**/api/git/branch**", async (route) =>
    route.fulfill({ json: { current: "dev", all: ["dev"] } }),
  );
  await page.route("**/api/git/log**", async (route) => route.fulfill({ json: [] }));
  await page.route("**/api/git/op**", async (route) => {
    const body = route.request().postDataJSON() as { op: string };
    if (body.op === "stage") staged = true;
    return route.fulfill({ json: { ok: true } });
  });
  await page.goto("/#/g/vogt");

  const commit = page.getByRole("button", { name: /Commit staged/ });
  // A message alone is not enough while nothing is staged — the hint says why.
  await page.getByPlaceholder("Commit message").fill("stage it all");
  await expect(commit).toBeDisabled();
  await expect(page.getByText(/Stage a file to enable committing/)).toBeVisible();

  await page.getByRole("button", { name: "Stage all" }).click();

  // Staging flips the working tree to the index and unblocks the commit.
  await expect(commit).toBeEnabled();
  await expect(page.getByText(/Stage a file to enable committing/)).toHaveCount(0);
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
  await expect(page.getByText("No sessions yet.")).toHaveCount(0);

  const retry = page.getByRole("button", { name: "Retry history" });
  if (test.info().project.name === "phone") await retry.tap();
  else await retry.click();

  await expect(page.getByText("0 archived sessions")).toBeVisible();
  await expect(page.getByText("No sessions yet.")).toBeVisible();
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


test("Agent Tasks show bindings and findings, relabel Run Now, and survive reload", async ({ page }) => {
  const boundTask = {
    id: "bound-task",
    name: "Bound task",
    prompt: "Original prompt",
    schedule: { kind: "manual" },
    status: "active",
    command: null,
    cwd: null,
    env: [],
    context: null,
    vogt_project: "vogt",
    vogt_work_item: "WI-7",
    notify_on_start: false,
    notify_on_phrase: null,
    auto_retry_on_rate_limit: true,
    next_run: null,
    last_run: "2026-08-18T00:05:00Z",
    run_count: 1,
    runs: [{
      id: "run-0",
      task_id: "bound-task",
      started_at: "2026-08-18T00:00:00Z",
      trigger: "scheduled",
      session_id: "session-0",
      session_name: "nightly-run",
      prompt_file: "prompt.txt",
      context_file: "context.txt",
      status: "completed",
      completed_at: "2026-08-18T00:05:00Z",
      exit_code: 0,
      summary: null,
      findings: [{ at: "2026-08-18T00:03:00Z", text: "Queue is clear", source: "notify" }],
    }],
    created_at: "2026-08-18T00:00:00Z",
    updated_at: "2026-08-18T00:00:00Z",
  };
  await installFixtures(page, {}, [], undefined, {}, [boundTask]);

  await page.goto("/#/tasks");
  // The Vogt bindings the engine stores now round-trip into the form.
  await expect(page.getByRole("textbox", { name: "Vogt project" })).toHaveValue("vogt");
  await expect(page.getByRole("textbox", { name: "Vogt work item" })).toHaveValue("WI-7");
  // A run's finding is rendered under its row, not lost to a push.
  await expect(page.getByText("Queue is clear")).toBeVisible();

  const name = page.getByRole("textbox", { name: "Name", exact: true });
  await expect(name).toHaveValue("Bound task");
  await expect(page.getByRole("button", { name: "Run Now" })).toBeVisible();

  await name.fill("Bound task edited");
  // A dirty draft relabels Run Now so a click saves before it runs.
  await expect(page.getByRole("button", { name: "Save & Run" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Run Now" })).toHaveCount(0);

  // A full reload keeps the unsaved draft, carried in sessionStorage.
  await page.reload();
  await expect(page.getByRole("textbox", { name: "Name", exact: true }))
    .toHaveValue("Bound task edited");
  await expect(page.getByText("Unsaved draft")).toBeVisible();
});


test("Backlog filters are chips, a + Filter disclosure and a named lens", async ({ page }) => {
  await installFixtures(page);
  await page.goto("/#/backlog?project=vogt");

  const filters = page.getByRole("group", { name: "Backlog filters", exact: true });
  await expect(filters.getByText("Project: Vogt")).toBeVisible();
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

  await openFilterPanel(filters);
  await page.getByLabel("Lens name").fill("Vogt features");
  await page.getByRole("button", { name: "Save lens" }).click();
  await expect(page.locator(".vogt-backlog-saved-recall")).toHaveText("Vogt features");

  await filters.getByRole("button", { name: "Remove filter Project: Vogt" }).click();
  await expect(page).not.toHaveURL(/project=vogt/);
  await expect(page).toHaveURL(/kind=feature/);

  await filters.getByRole("button", { name: "Clear all" }).click();
  await expect(page).not.toHaveURL(/kind=feature/);

  await openFilterPanel(filters);
  await page.locator(".vogt-backlog-saved-recall").click();
  await expect(page).toHaveURL(/project=vogt/);
  await expect(page).toHaveURL(/kind=feature/);

  await page.reload();
  await expect(filters.getByText("Project: Vogt")).toBeVisible();
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

// #226: the row's most common act is reachable from the collapsed row rather
// than hidden inside the expanded detail, and the page-only State filter says
// so — a suffixed chip and an "N of M loaded rows" summary — because the ranked
// views take no state parameter.
test("Backlog reaches Start a session… from the collapsed row and counts a page-only State filter", async ({ page }) => {
  test.skip(test.info().project.name === "phone", "Collapsed-row reach and the summary are the desktop contract");
  await installFixtures(page, {}, [], undefined, {
    backlogTotal: 2,
    backlogItems: [
      {
        ref: "WI-1", title: "First ranked item", kind: "feature", state: "open",
        priority: "normal", project_slug: "vogt", trust_state: "verified", labels: [],
        score: 2, updated_at: "2026-08-17T10:00:00Z", origin: "declared",
      },
      {
        ref: "WI-2", title: "Second ranked item", kind: "feature", state: "in_progress",
        priority: "normal", project_slug: "vogt", trust_state: "verified", labels: [],
        score: 1, updated_at: "2026-08-17T10:00:00Z", origin: "declared",
      },
    ],
  });
  await page.goto("/#/backlog");

  const first = page.locator(".vogt-backlog-row").filter({ hasText: "First ranked item" });
  await expect(first).toBeVisible();
  // The row is collapsed — no detail — yet "Start a session…" is right there in
  // the facts row's overflow next to More, not one disclosure away.
  await expect(first.locator(".vogt-backlog-row-detail")).toHaveCount(0);
  await expect(first.getByRole("button", { name: "Less" })).toHaveCount(0);
  await expect(
    first.locator(".vogt-backlog-row-quick .vogt-backlog-row-session"),
  ).toBeVisible();
  await expect(first.getByRole("button", { name: "Start a session…" })).toBeVisible();

  // Applying a State filter marks it page-only and re-counts the loaded page.
  const filters = page.getByRole("group", { name: "Backlog filters", exact: true });
  await filters.getByRole("button", { name: "+ Filter", exact: true }).click();
  const panel = page.getByRole("group", { name: "Add Backlog filters" });
  await panel.getByRole("button", { name: "open", exact: true }).click();
  await expect(filters.getByText("State: open · this page only")).toBeVisible();
  await expect(page.locator(".vogt-backlog-count")).toContainText("1 of 2 loaded rows");
});

// #226 on the phone: the row leads with its title, keeps one compact meta line
// under it, and the bulk bar follows the selection to the top of the scroller
// rather than sitting off-screen above the ranked page.
test("Phone Backlog leads with title-first rows, marks the page-only State filter and sticks the bulk bar", async ({ page }) => {
  test.skip(test.info().project.name !== "phone", "The compact phone row and sticky bulk bar are the narrow-shell contract");
  const items = Array.from({ length: 6 }, (_, index) => ({
    ref: `WI-${index + 1}`,
    title: `Row ${index + 1}`,
    kind: "feature",
    // The leading rows carry a short state so the compact meta line stays on
    // one row; the last carries a longer one purely so the State chip below has
    // a second value to pick.
    state: index < 5 ? "open" : "in_progress",
    priority: "normal",
    project_slug: "vogt",
    trust_state: "verified",
    labels: [],
    score: 6 - index,
    updated_at: "2026-08-17T10:00:00Z",
    origin: "declared",
  }));
  await installFixtures(page, {}, [], undefined, {
    backlogTotal: items.length,
    backlogItems: items,
  });
  await page.goto("/#/backlog");

  const rows = page.locator(".vogt-backlog-row");
  await expect(rows.first()).toBeVisible();

  // At least three rows fit inside the list's own visible viewport: the compact
  // title-first layout, not a facts row that pushes the third row past the fold.
  const withinList = await page.evaluate(() => {
    const list = document.querySelector(".vogt-backlog-list")!.getBoundingClientRect();
    return [...document.querySelectorAll(".vogt-backlog-row")].filter((node) => {
      const box = node.getBoundingClientRect();
      return box.height > 0 && box.top >= list.top - 1 && box.bottom <= list.bottom + 1;
    }).length;
  });
  expect(withinList).toBeGreaterThanOrEqual(3);

  // Title first: the title box sits above the meta (facts) line in the row.
  const order = await rows.first().evaluate((row) => {
    const title = row.querySelector<HTMLElement>(".vogt-backlog-row-title")!.getBoundingClientRect();
    const facts = row.querySelector<HTMLElement>(".vogt-backlog-row-facts")!.getBoundingClientRect();
    return { titleTop: title.top, factsTop: facts.top };
  });
  expect(order.titleTop).toBeLessThan(order.factsTop);

  // Selecting a row raises the bulk bar, stuck to the top of the list scroller.
  await rows.first().getByRole("checkbox").check();
  const bar = page.locator(".vogt-backlog-bulk");
  await expect(bar).toBeVisible();
  expect(await bar.evaluate((element) => getComputedStyle(element).position)).toBe("sticky");
  const barBox = (await bar.boundingBox())!;
  const listBox = (await page.locator(".vogt-backlog-list").boundingBox())!;
  expect(Math.round(barBox.y)).toBeLessThanOrEqual(Math.round(listBox.y) + 2);

  // The State chip, once applied, is marked page-only here too.
  const filters = page.getByRole("group", { name: "Backlog filters", exact: true });
  await filters.getByRole("button", { name: "+ Filter", exact: true }).click();
  const panel = page.getByRole("group", { name: "Add Backlog filters" });
  await panel.getByRole("button", { name: "in_progress", exact: true }).click();
  await expect(filters.getByText("State: in_progress · this page only")).toBeVisible();
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

    // Inside the phone shell, not instead of it. Go to… rides inline in the
    // surface header on a phone (WI-75); the standalone bar is the fallback
    // for a screen with no header of its own.
    await expect(page.locator(".mobile-go-to, .go-to-inline").first(), `/${route.path} keeps Go to`).toBeVisible();
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
 * History is the one place that lists every session, live and dead (#477):
 * a running shell from the live registry is unioned into the list even when
 * the archive is empty, and badged live rather than hidden until it exits.
 */
test("History lists a live session even when the archive is empty", async ({ page }) => {
  await installFixtures(page, {}, [liveSession]);
  await page.route("**/api/history/sessions*", (route) => route.fulfill({ json: [] }));
  await page.goto("/#/history");

  const row = page.getByRole("button", { name: /browser-session/ });
  await expect(row).toBeVisible();
  await expect(row.getByText("Live")).toBeVisible();
  await expect(page.getByText("No sessions yet.")).toHaveCount(0);
});

// -- #213 / #224: editing and moving a work item from its own page ----------
//
// The detail page carries the three per-item forms the palette routes to it
// for (`USER_GUIDE.md` §4): transition (Move to), comment, and start session.
// #213 added the first; these prove it fires the transition and the rail
// follows the server's answer, on the desk and the phone alike, since a phone
// has no drag and the detail page is where it moves a card at all.

test("Work item state is changed from the detail page via Move to", async ({ page }) => {
  const fixture = await installFixtures(page);
  await page.goto("/#/w/WI-7");
  await expect(page.locator(".wid-view")).toBeVisible();
  await expect(page.getByRole("heading", { name: "Measured board card" })).toBeVisible();

  // The rail says the item is open.
  const rail = page.locator(".wid-rail");
  await expect(rail.locator(".wid-rail-state.is-current")).toHaveText("open");

  // Move to → reason → confirm, the same optimistic transition a drag commits.
  const move = page.locator(".wid-move-to");
  await expect(move).toBeVisible();
  await move.getByLabel("Move to").selectOption("done");
  await move.locator('input[type="text"]').fill("closing it from the detail page");
  await move.getByRole("button", { name: "Move", exact: true }).click();

  // Exactly that transition was requested — ref, target, and the typed reason.
  await expect.poll(() => fixture.transitionRequests).toEqual([
    { ref: "WI-7", to_state: "done", reason: "closing it from the detail page" },
  ]);
  // And the rail follows the server's answer rather than what was typed.
  await expect(rail.locator(".wid-rail-state.is-current")).toHaveText("done");
});

// #283: the branch panel is layout-bearing — declared and observed branches
// are shown side by side, an observed one says when it was last active, and a
// branch on one side only is marked drift rather than merged. This overrides
// `/work/get` for this test alone (a route registered later wins), so the
// shared fixture — and the visual snapshots that read it — are untouched.
test("The item detail page shows the branches it is worked on, with age (#283)", async ({
  page,
}) => {
  await installFixtures(page);
  await page.route(/\/work\/get(\?|$)/, (route) =>
    route.fulfill({
      json: {
        item: {
          id: "01JWORKITEM",
          ref: "WI-7",
          kind: "feature",
          title: "Measured board card",
          body: "",
          state: "open",
          priority: "normal",
          project_slug: "vogt",
          initiative_id: null,
          origin: "declared",
          trust_state: "verified",
          assignee_identity_ref: null,
          labels: [],
          relations: [],
          created_at: "2026-08-01T00:00:00Z",
          updated_at: "2026-08-17T10:00:00Z",
        },
        comments: [],
        sessions: [],
        branches: [
          {
            name: "wi-7",
            source: "both",
            drift: false,
            ahead: 1,
            behind: 0,
            default_branch: "main",
            last_commit_age_seconds: 7200,
          },
          { name: "wi-9", source: "declared", drift: true },
        ],
      },
    }),
  );

  await page.goto("/#/w/WI-7");
  const panel = page.locator('[data-testid="branches"]');
  await expect(panel).toBeVisible();
  // The observed-and-declared branch: named, badged `both`, active 2h ago.
  await expect(panel.locator('[data-testid="branch-wi-7"]')).toContainText("wi-7");
  await expect(panel).toContainText("active 2h ago");
  await expect(
    panel.locator('[data-testid="branch-wi-7"] .wid-branch-source--both'),
  ).toBeVisible();
  // The declared-only branch reads as drift, not as an observation.
  await expect(
    panel.locator('[data-testid="branch-wi-9"] .wid-branch-drift'),
  ).toBeVisible();
});

// #285: the derived git story is layout-bearing — a phase chip sits beside the
// workflow state, the PR row carries its state/checks/age, and a merged PR under
// an open item reads as drift. Overrides `/work/get` for this test alone (a
// route registered later wins), so the shared fixture is untouched.
test("The item detail page shows its derived git story — phase, PR, drift (#285)", async ({
  page,
}) => {
  await installFixtures(page);
  await page.route(/\/work\/get(\?|$)/, (route) =>
    route.fulfill({
      json: {
        item: {
          id: "01JWORKITEM",
          ref: "WI-7",
          kind: "feature",
          title: "Shipped but still open",
          body: "",
          state: "in_progress",
          priority: "normal",
          project_slug: "vogt",
          initiative_id: null,
          origin: "declared",
          trust_state: "verified",
          assignee_identity_ref: null,
          labels: [],
          relations: [],
          created_at: "2026-08-01T00:00:00Z",
          updated_at: "2026-08-17T10:00:00Z",
        },
        comments: [],
        sessions: [],
        branches: [
          {
            name: "wi-7",
            source: "observed",
            drift: false,
            last_commit_age_seconds: 3600,
          },
        ],
        git: {
          phase: "merged",
          workflow_state: "in_progress",
          branches: [
            {
              name: "wi-7",
              source: "observed",
              drift: false,
              last_commit_age_seconds: 3600,
            },
          ],
          pull_request: {
            number: 7,
            state: "merged",
            url: "https://example.test/pull/7",
            checks: "success",
            provenance: "from PR body",
            observed_age_seconds: 240,
          },
          drift: [
            {
              code: "merged_pr_open_item",
              message:
                "the pull request merged but the item is still open",
              provenance: "forge PR #7",
            },
          ],
          task_conclusion_available: false,
        },
      },
    }),
  );

  await page.goto("/#/w/WI-7");
  // The derived phase sits beside the workflow state, marked as git.
  await expect(page.locator('[data-testid="git-phase"]')).toContainText("merged");
  const panel = page.locator('[data-testid="git-story"]');
  await expect(panel).toBeVisible();
  // The PR row carries its derived state and its checks rollup.
  await expect(panel.locator('[data-testid="git-pr-state"]')).toHaveText("merged");
  await expect(panel.locator('[data-testid="git-pr-checks"]')).toContainText(
    "success",
  );
  // The contradiction between a merged PR and an open item reads as drift.
  await expect(
    panel.locator('[data-testid="git-drift-merged_pr_open_item"]'),
  ).toContainText("still open");
});

test("The item editor's assignee picker is keyboard-operable", async ({ page }) => {
  await installFixtures(page, {}, [], undefined, {
    actors: [
      { identity_ref: "local:ana", display_name: "Ana" },
      { identity_ref: "local:bo", display_name: "Bo" },
    ],
  });
  await page.goto("/#/w/WI-7");
  await expect(page.locator(".wid-view")).toBeVisible();

  await page.getByRole("button", { name: "Edit", exact: true }).click();
  const picker = page.getByLabel("Assignee");
  await expect(picker).toBeVisible();
  // It offers "nobody" and the two actors, and starts on the item's assignee.
  await expect(picker.locator("option")).toHaveText([
    "nobody",
    "Ana (local:ana)",
    "Bo (local:bo)",
  ]);

  // Focus it from nothing but the keyboard, and change the selection with the
  // arrow keys — no pointer touches it.
  await picker.focus();
  await expect(picker).toBeFocused();
  await picker.press("ArrowDown");
  await expect(picker).toHaveValue("local:ana");
  await picker.press("ArrowDown");
  await expect(picker).toHaveValue("local:bo");
});

test("The item page leads with comments and keeps the session form collapsed", async ({ page }) => {
  await installFixtures(page);
  await page.goto("/#/w/WI-7");
  await expect(page.locator(".wid-view")).toBeVisible();

  // Comments is the panel immediately after Description in the main column.
  const texts = await page.locator(".wid-main .wid-panel h3").allTextContents();
  const description = texts.indexOf("Description");
  expect(description).toBeGreaterThanOrEqual(0);
  expect(texts[description + 1]).toBe("Comments");

  // Start a session is collapsed by default: the opener is on the page, the
  // form is not, and clicking the opener reveals it.
  await expect(page.locator(".wid-start-open")).toBeVisible();
  await expect(page.locator(".wid-start form")).toHaveCount(0);
  await page.locator(".wid-start-open").click();
  await expect(page.locator(".wid-start form")).toBeVisible();
});

// -- #222: the item body is rendered Markdown, with a raw escape hatch -------
//
// With the forge pivot the body is GitHub-flavoured Markdown, so a heading,
// a list and a fenced block must become real nodes rather than arriving as
// literal `#`, `-` and backticks — and the reader can still see the source.

test("The item body renders Markdown, and the raw toggle shows its source", async ({ page }) => {
  const body = [
    "# Heading one",
    "",
    "A paragraph before the list.",
    "",
    "- first item",
    "- second item",
    "",
    "```ts",
    "const answer = 42;",
    "```",
  ].join("\n");
  await installFixtures(page, {}, [], undefined, { workBody: body });
  await page.goto("/#/w/WI-7");
  await expect(page.locator(".wid-view")).toBeVisible();

  const description = page.locator(".wid-main .wid-panel", { hasText: "Description" });
  const rendered = description.locator(".md-body");

  // A real heading, a real list and a real fenced code block — not literals.
  await expect(rendered.locator("h1")).toHaveText("Heading one");
  await expect(rendered.locator("ul > li")).toHaveText(["first item", "second item"]);
  await expect(rendered.locator("pre code")).toHaveText("const answer = 42;");

  // None of the Markdown punctuation survives as text in the rendered view.
  const renderedText = (await rendered.innerText()).trim();
  expect(renderedText).not.toContain("# Heading");
  expect(renderedText).not.toContain("- first");
  expect(renderedText).not.toContain("```");

  // The raw toggle swaps to the Markdown source, verbatim.
  await description.getByRole("button", { name: "Raw" }).click();
  const raw = description.locator(".wid-body-raw");
  await expect(raw).toBeVisible();
  await expect(raw).toContainText("# Heading one");
  await expect(raw).toContainText("```ts");
  // And back again.
  await description.getByRole("button", { name: "Rendered" }).click();
  await expect(description.locator(".md-body h1")).toHaveText("Heading one");
});

// -- the assistant composer's ergonomics (#242) ----------------------------
//
// A failed send that keeps what was typed, a Stop that cancels an in-flight
// turn, a multi-line composer, and Markdown replies — exercised end to end in
// a real browser, with the assistant message endpoint mocked per test.

async function openAssistant(page: Page): Promise<void> {
  await installFixtures(page, { assistant_enabled: true });
  await page.goto("/#/assistant");
  await expect(page.locator(".assistant-input")).toBeVisible();
}

test("a failed assistant send keeps the draft and offers a retry", async ({ page }) => {
  await openAssistant(page);
  // The engine refuses this turn.
  await page.route("**/api/assistant/message", async (route) =>
    route.fulfill({ status: 502, body: "upstream down" }),
  );

  await page.locator(".assistant-input").fill("what is on top?");
  await page.getByRole("button", { name: "Send" }).click();

  // The message is not eaten by the failure, and a Retry is offered.
  await expect(page.locator(".assistant-input")).toHaveValue("what is on top?");
  await expect(page.getByTestId("assistant-retry")).toBeVisible();
  await expect(page.locator(".assistant-row--failed")).toBeVisible();
});

test("Stop cancels an assistant send in flight", async ({ page }) => {
  await openAssistant(page);
  // A turn that never answers on its own: only the client's abort ends it.
  await page.route("**/api/assistant/message", async (route) => {
    await new Promise((resolve) => setTimeout(resolve, 10_000));
    try {
      await route.fulfill({ json: { reply: "late", pending_action: null, tool_trace: [] } });
    } catch {
      /* aborted by Stop before it could answer */
    }
  });

  await page.locator(".assistant-input").fill("summarise the backlog");
  await page.getByRole("button", { name: "Send" }).click();

  // While busy, Stop replaces Send; clicking it aborts the request and the
  // composer comes back with the message intact and Send restored.
  const stop = page.getByTestId("assistant-stop");
  await expect(stop).toBeVisible();
  await stop.click();
  await expect(page.getByRole("button", { name: "Send" })).toBeVisible();
  await expect(page.locator(".assistant-input")).toHaveValue("summarise the backlog");
});

test("Shift+Enter inserts a newline while Enter sends", async ({ page }) => {
  await openAssistant(page);

  const composer = page.locator(".assistant-input");
  await composer.click();
  await composer.pressSequentially("line one");
  await composer.press("Shift+Enter");
  await composer.pressSequentially("line two");

  // The newline is in the value, and nothing was sent yet.
  await expect(composer).toHaveValue("line one\nline two");
  await expect(page.getByText("Acknowledged.")).toHaveCount(0);

  // Plain Enter sends the whole multi-line message.
  await composer.press("Enter");
  await expect(page.getByText("Acknowledged.")).toBeVisible();
});

test("an assistant Markdown reply renders a code block", async ({ page }) => {
  await openAssistant(page);
  await page.route("**/api/assistant/message", async (route) =>
    route.fulfill({ json: {
      reply: "Run this:\n\n```sh\ncargo test\n```\n",
      pending_action: null,
      tool_trace: [],
    } }),
  );

  await page.locator(".assistant-input").fill("how do I test?");
  await page.getByRole("button", { name: "Send" }).click();

  const code = page.locator("pre.md-pre code");
  await expect(code).toBeVisible();
  await expect(code).toContainText("cargo test");
  await expect(page.getByTestId("assistant-copy")).toBeVisible();
});

test("Phone Assistant renders structured transcript and compact approval", async ({ page }) => {
  test.skip(test.info().project.name !== "phone", "The redesigned transcript is the narrow client's");
  const sessionId = "assistant-deploy";
  await installFixtures(page, { assistant_enabled: true }, [{
    id: sessionId,
    name: "deploy-agent",
    cwd: "/workspace/vogt",
    activity: "waiting-for-input",
    exit_code: null,
    scrollback_bytes: 1024,
    created_at: "2026-08-30T10:00:00Z",
    activity_changed_at: "2026-08-30T10:04:00Z",
  }]);
  await page.route("**/api/assistant/history**", async (route) =>
    route.fulfill({ json: {
      transcript: [
        { role: "user", text: "What needs me?", created_at: "2026-08-30T10:05:00Z" },
        {
          role: "assistant",
          text: "The deploy agent is waiting at a migration prompt.",
          created_at: "2026-08-30T10:05:25Z",
          tool_trace: ["listed sessions", "read deploy-agent tail"],
          session_refs: [{ id: sessionId, name: "deploy-agent", activity: "waiting-for-input" }],
          actions: [{ kind: "open-session", session_id: sessionId, label: "Open deploy-agent" }],
        },
      ],
      pending_action: {
        kind: "send_input",
        id: "pending-input",
        session_id: sessionId,
        session_name: "deploy-agent",
        text: "y",
        submit: true,
      },
    } }),
  );
  await page.goto("/#/assistant");

  await expect(page.locator(".sessions-header")).toBeHidden();
  await expect(page.locator(".assistant-watch-line")).toContainText(
    "watching 1 sessions · 1 waiting",
  );
  await expect(page.locator(".assistant-trace")).toContainText(
    "▸ listed sessions · read deploy-agent tail",
  );
  await expect(page.locator(".assistant-session-chip")).toHaveAttribute(
    "href",
    `#/t/${sessionId}`,
  );
  await expect(page.locator(".assistant-open-session")).toContainText(
    "Open deploy-agent ›",
  );
  await expect(page.getByRole("region", { name: "Pending approval" })).toContainText(
    "Send y ⏎ to deploy-agent",
  );
  await expect(page.locator(".assistant")).toHaveScreenshot(
    "assistant-phone-structured.png",
    { mask: [page.locator(".assistant-time-separator"), page.locator(".assistant-countdown")] },
  );
});

/**
 * #228: every primary surface now wears the one shared header — Projects and
 * the Work item joined Board, Backlog, Inbox and Sessions — with the honesty
 * standardised to the same age pill. The screenshot is the visual half of the
 * slot-order/geometry contract the zoom spec asserts numerically; the honesty
 * slot is masked because it counts seconds and would otherwise flake.
 */
const SURFACE_HEADERS = [
  { name: "board", goto: "/#/board", content: ".board-scroll, .board-empty" },
  { name: "backlog", goto: "/#/backlog", content: ".vogt-backlog-listwrap, .vogt-backlog-empty" },
  { name: "inbox", goto: "/#/inbox", content: ".inbox-list" },
  { name: "sessions", goto: "/#/sessions", content: ".sessions-place-body" },
  { name: "projects", goto: "/#/projects", content: ".vogt-projects-list" },
  { name: "workitem", goto: "/#/w/WI-7", content: ".wid-facts" },
] as const;

test("all six surface headers share the grammar and read the same at both sizes", async ({ page }) => {
  await installFixtures(page, {}, [liveSession]);
  // "goto" is the phone's inline Go to… (WI-75), present only on a narrow client.
  const order = ["title", "goto", "honesty", "spacer", "controls", "action", "detail"];

  for (const surface of SURFACE_HEADERS) {
    await page.goto(surface.goto);
    const header = page.locator("[data-surface-header]:visible").first();
    await expect(header).toBeVisible();
    await expect(page.locator(surface.content).first()).toBeVisible();

    // A narrow shell folds a surface's chrome behind one disclosure; open it so
    // the whole header is in the shot rather than half of it.
    const more = header.locator(".surface-header-more");
    if (await more.count()) {
      if ((await more.getAttribute("aria-expanded")) === "false") await more.click();
    }

    const slots = await header
      .locator(":scope > [data-surface-header-slot]")
      .evaluateAll((elements) =>
        elements.map((element) => element.getAttribute("data-surface-header-slot")),
      );
    expect(slots, `${surface.name} keeps the shared slot order`).toEqual(
      order.filter((slot) => slots.includes(slot)),
    );

    await expect(header).toHaveScreenshot(`surface-header-${surface.name}.png`, {
      // The age pill counts seconds; masking it is what keeps the shot stable.
      mask: [header.locator('[data-surface-header-slot="honesty"]')],
    });
  }
});

/**
 * The desktop half of #245's rail: a surface remembered under two different
 * queries is one place, so its chip appears once — not two identically
 * labelled chips, the bug the query-keyed-but-path-labelled recents had.
 */
test("Recent places dedupes a surface visited under different queries", async ({ page }) => {
  test.skip(test.info().project.name === "phone", "The places rail is a desktop surface");
  await installFixtures(page);

  await page.goto("/#/board?project=vogt");
  await expect(page.getByRole("heading", { name: "Board" })).toBeVisible();
  await page.goto("/#/board?lanes=project");
  await expect(page.getByRole("heading", { name: "Board" })).toBeVisible();

  const recent = page.locator(".places-recent");
  await expect(recent).toBeVisible();
  // One Board chip, pointing at the latest of the two visited queries.
  await expect(recent.getByRole("link", { name: "Board" })).toHaveCount(1);
  await expect(recent.getByRole("link", { name: "Board" })).toHaveAttribute(
    "href",
    "#/board?lanes=project",
  );
});

/**
 * The other desktop half of #245: with a full session list the rail's footer —
 * Settings, Sign out, the connection dot — used to sit below the fold and be
 * unreachable at 1440×900. Pinned now, it is in the viewport without scrolling.
 */
test("The rail footer stays reachable at 1440x900 with sessions present", async ({ page }) => {
  test.skip(test.info().project.name === "phone", "The places rail is a desktop surface");
  const crowd = Array.from({ length: 30 }, (_, index) => ({
    ...liveSession,
    id: `footer-crowd-${index}`,
    name: `Crowded session ${index + 1}`,
  }));
  await installFixtures(page, {}, crowd);
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.goto("/#/sessions");

  const settings = page.getByRole("button", { name: "Settings" });
  await expect(settings).toBeVisible();
  await expect(settings).toBeInViewport();
  await expect(page.getByRole("button", { name: "Sign out" })).toBeInViewport();
});

/**
 * #246: an empty Board is one panel that offers the next act — Quick create,
 * Clear filters, Register a project — not four per-column "Nothing here" with
 * no way out.
 */
test("An empty Board offers one panel with a next action, not four dead cells", async ({ page }) => {
  await installFixtures(page, {}, [], undefined, {
    boardItems: [],
    boardTotal: 0,
  });
  await page.goto("/#/board");

  const empty = page.locator(".board-empty--actions");
  await expect(empty).toBeVisible();
  await expect(empty).toContainText("No work items match");
  await expect(empty.getByRole("button", { name: "Quick create" })).toBeVisible();
  await expect(empty.getByRole("link", { name: "Register a project" })).toBeVisible();
  // The four per-column "Nothing here" are gone.
  await expect(page.locator(".board-cell-empty")).toHaveCount(0);

  await empty.getByRole("button", { name: "Quick create" }).click();
  await expect(page.getByRole("textbox", { name: "Title" }).first()).toBeVisible();
});

// ---------------------------------------------------------------------------
// Selectable app themes (#299): Vogt Dark, Dim, Light and High contrast are
// token sets on <html data-theme>. These tests pin each theme, prove it is
// applied, snapshot the primary surfaces per theme at both sizes, assert the
// no-flash pre-paint attribute, and check WCAG contrast for the light themes.
// ---------------------------------------------------------------------------

const APP_THEME_IDS = ["dark", "dim", "light", "hc-dark", "hc-light"] as const;

const THEME_SURFACES = [
  { name: "board", goto: "/#/board?project=vogt", ready: ".board-scroll, .board-empty" },
  { name: "backlog", goto: "/#/backlog?project=vogt", ready: ".vogt-backlog-listwrap, .vogt-backlog-empty" },
  { name: "workitem", goto: "/#/w/WI-7", ready: ".wid-facts" },
  { name: "sessions", goto: "/#/sessions", ready: ".sessions-place-body" },
] as const;

test("mobile redesign keeps its layout across all themes and required widths", async ({ page }) => {
  test.skip(test.info().project.name !== "phone", "The responsive matrix belongs to the narrow client");
  await installFixtures(page, { assistant_enabled: true }, THREE_SESSIONS);
  await stubTerminalAttach(page);
  await page.goto("/#/sessions");

  const setTheme = async (themeId: string) => {
    await page.evaluate(async (id) => {
      const { setAppTheme } = await import("/src/appThemes.ts");
      setAppTheme(id);
    }, themeId);
    expect(await page.evaluate(() => document.documentElement.dataset.theme)).toBe(themeId);
  };
  const expectNoHorizontalOverflow = async (selector: string) => {
    const dimensions = await page.locator(selector).first().evaluate((element) => ({
      client: element.clientWidth,
      scroll: element.scrollWidth,
    }));
    expect(dimensions.scroll, `${selector} overflows: ${JSON.stringify(dimensions)}`)
      .toBeLessThanOrEqual(dimensions.client + 1);
  };

  // Every named token set must preserve the Sessions composition; this is a
  // geometry smoke test rather than eight near-identical screenshot files.
  for (const themeId of ["dark", "dim", "hc-dark", "light", "soft", "sepia", "rose", "hc-light"]) {
    await page.setViewportSize({ width: 390, height: 844 });
    await page.goto("/#/sessions");
    await setTheme(themeId);
    await expect(page.locator(".sessions-place")).toBeVisible();
    await expectNoHorizontalOverflow(".sessions-place");
  }

  // The handoff explicitly calls out these four widths and both palette
  // bases. Exercise all three redesigned surface owners at every point.
  for (const themeId of ["dark", "light"]) {
    for (const width of [320, 375, 430, 768]) {
      await page.setViewportSize({ width, height: 844 });

      await page.goto("/#/sessions");
      await setTheme(themeId);
      await expect(page.locator(".sessions-place")).toBeVisible();
      await expectNoHorizontalOverflow(".sessions-place");

      await page.goto("/#/assistant");
      await setTheme(themeId);
      await expect(page.locator(".assistant-input")).toBeVisible();
      await expect(page.locator(".sessions-header")).toBeHidden();
      await expectNoHorizontalOverflow(".assistant");

      await page.goto("/#/t/sess-idle");
      await setTheme(themeId);
      await expect(page.locator(".terminal-mobile-header")).toBeVisible();
      await expect(page.locator(".terminal-host").first()).toBeVisible();
      await expect(page.locator(".sessions-header")).toBeHidden();
      const terminalHeight = await page.locator(".terminal-host").first()
        .evaluate((element) => element.getBoundingClientRect().height);
      expect(terminalHeight, `${themeId} terminal at ${width}px`).toBeGreaterThan(250);

      if (themeId === "light" && width === 375) {
        await page.getByRole("button", { name: "More terminal actions" }).click();
        await expect(page.locator(".terminal-mobile-overflow").getByLabel("Terminal color theme"))
          .toHaveValue("GitHub Light");
      }
    }
  }
});

// Register the theme selection as an init script so index.html's pre-paint
// script reads it — added AFTER installFixtures so it overrides the dark pin.
async function pinAppTheme(page: Page, themeId: string) {
  await page.addInitScript((id) => {
    localStorage.setItem("vogt.appTheme.v1", id as string);
  }, themeId);
}

// The two sizes the issue names: 1440x900 on desktop, 390x844 on phone.
async function useThemeViewport(page: Page) {
  if (test.info().project.name === "desktop") {
    await page.setViewportSize({ width: 1440, height: 900 });
  } else {
    await page.setViewportSize({ width: 390, height: 844 });
  }
}

for (const themeId of APP_THEME_IDS) {
  for (const surface of THEME_SURFACES) {
    test(`app theme ${themeId}: ${surface.name} renders and matches its baseline`, async ({ page }) => {
      await installFixtures(page, {}, [liveSession]);
      await pinAppTheme(page, themeId);
      await useThemeViewport(page);
      await page.goto(surface.goto);
      await expect(page.locator(surface.ready).first()).toBeVisible();
      expect(
        await page.evaluate(() => document.documentElement.getAttribute("data-theme")),
      ).toBe(themeId);
      await expect(page).toHaveScreenshot(`theme-${themeId}-${surface.name}.png`, {
        fullPage: true,
        animations: "disabled",
        // Relative-age pills tick, so masking them is what keeps the shot stable.
        mask: [page.locator(".vogt-age")],
      });
    });
  }
}

// No flash of the wrong palette: index.html's inline script must set
// `data-theme` synchronously, before the first paint. We capture the attribute
// inside the first animation frame (which fires before the first repaint); if
// it is already the stored theme, it was set before paint — not by a later
// module. The capture script is registered before the theme pin, but rAF
// resolves after parsing, by which point the inline head script has run.
test("app theme is applied before first paint (no flash)", async ({ page }) => {
  await installFixtures(page);
  await page.addInitScript(() => {
    (window as unknown as { __themeAtFirstFrame?: string | null }).__themeAtFirstFrame = "unset";
    requestAnimationFrame(() => {
      (window as unknown as { __themeAtFirstFrame?: string | null }).__themeAtFirstFrame =
        document.documentElement.getAttribute("data-theme");
    });
  });
  await pinAppTheme(page, "light");
  await page.goto("/#/board?project=vogt");
  await expect(page.locator(".board-scroll, .board-empty").first()).toBeVisible();
  const themeAtFirstFrame = await page.evaluate(
    () => (window as unknown as { __themeAtFirstFrame?: string | null }).__themeAtFirstFrame,
  );
  expect(themeAtFirstFrame).toBe("light");
});

// WCAG contrast for the light themes and high contrast (#299 test gate). The
// palette is what the surfaces read, so we measure the tokens directly: a
// hand-rolled relative-luminance ratio, no new dependency. Body text must
// clear 4.5:1; state/accent (large/chip) text must clear 3:1.
for (const themeId of ["light", "hc-light", "hc-dark"] as const) {
  test(`app theme ${themeId}: palette clears WCAG AA contrast`, async ({ page }) => {
    await installFixtures(page);
    await pinAppTheme(page, themeId);
    await page.goto("/#/board?project=vogt");
    await expect(page.locator(".board-scroll, .board-empty").first()).toBeVisible();

    const ratios = await page.evaluate(() => {
      const cs = getComputedStyle(document.documentElement);
      const tok = (n: string) => cs.getPropertyValue(n).trim();
      const hex = (c: string) => {
        let h = c.replace("#", "");
        if (h.length === 3) h = h.split("").map((x) => x + x).join("");
        return { r: parseInt(h.slice(0, 2), 16), g: parseInt(h.slice(2, 4), 16), b: parseInt(h.slice(4, 6), 16) };
      };
      const lum = ({ r, g, b }: { r: number; g: number; b: number }) => {
        const f = (v: number) => {
          const s = v / 255;
          return s <= 0.03928 ? s / 12.92 : Math.pow((s + 0.055) / 1.055, 2.4);
        };
        return 0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b);
      };
      const ratio = (a: string, b: string) => {
        const la = lum(hex(a)), lb = lum(hex(b));
        const hi = Math.max(la, lb), lo = Math.min(la, lb);
        return (hi + 0.05) / (lo + 0.05);
      };
      return {
        fgOnBg: ratio(tok("--fg"), tok("--bg")),
        mutedOnBg: ratio(tok("--fg-muted"), tok("--bg")),
        accentOnBg: ratio(tok("--accent"), tok("--bg")),
        dangerOnBg: ratio(tok("--danger"), tok("--bg")),
        doneOnBg: ratio(tok("--activity-done"), tok("--bg")),
        onAccentOnAccent: ratio(tok("--on-accent"), tok("--accent")),
      };
    });

    // Body text.
    expect(ratios.fgOnBg, "--fg on --bg").toBeGreaterThanOrEqual(4.5);
    expect(ratios.mutedOnBg, "--fg-muted on --bg").toBeGreaterThanOrEqual(4.5);
    // Large / chip / state text and the ink on accent controls.
    expect(ratios.accentOnBg, "--accent on --bg").toBeGreaterThanOrEqual(3);
    expect(ratios.dangerOnBg, "--danger on --bg").toBeGreaterThanOrEqual(3);
    expect(ratios.doneOnBg, "--activity-done on --bg").toBeGreaterThanOrEqual(3);
    expect(ratios.onAccentOnAccent, "--on-accent on --accent").toBeGreaterThanOrEqual(3);
  });
}
