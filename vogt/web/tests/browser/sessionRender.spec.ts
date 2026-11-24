import { expect, test, type Page } from "@playwright/test";

// WI-2328: what one stream event costs the page on a large roster.
//
// Prod carried 1,278 sessions (1,265 hibernated, 1,088 of them hidden from
// the default list) when the operator reported the rail lagging and the DOM
// thrashing on every update. This spec runs the real app against that shape:
// the session list is mocked, and `/api/events` is replaced in the page by a
// stream the test writes frames into, so each frame goes through the
// production parser and store exactly as a real one would.
//
// For every event kind it counts DOM mutations, session rows added or
// removed (a remount), fetches started, and whether the client tore down its
// own stream. `VOGT_RUN_PERF=1` prints the numbers; the assertions are the
// regression bounds and always run.

const TOTAL = 1278;
const VISIBLE = 190;
const NOW = Date.parse("2026-10-23T20:37:40Z");

function iso(offsetMs: number): string {
  return new Date(NOW - offsetMs).toISOString();
}

function makeSessions(): Record<string, unknown>[] {
  const out: Record<string, unknown>[] = [];
  for (let i = 0; i < TOTAL; i += 1) {
    const id = `00000000-0000-4000-8000-${String(i).padStart(12, "0")}`;
    const activity = i < 2 ? "running" : i < 11 ? "idle" : i < 13 ? "exited" : "hibernated";
    const live = i < 13;
    out.push({
      id,
      name: `session ${i}`,
      activity,
      exit_code: activity === "exited" ? 0 : null,
      alive: activity !== "exited",
      scrollback_bytes: 4 * 1024 * 1024,
      cwd: `/home/sprooty/Working/Active/WI-${1000 + i}`,
      command: "claude",
      created_at: iso(3_600_000 + i * 60_000),
      activity_changed_at: iso(60_000 + i * 60_000),
      turn_started_at: iso(120_000 + i * 60_000),
      last_output_at: iso(60_000 + i * 60_000),
      last_event_at: iso(60_000 + i * 60_000),
      last_person_input_at: iso(600_000 + i * 60_000),
      conversation: { agent: "claude", id: `conv-${i}` },
      keep_awake: false,
      autopilot: true,
      role: i < 10 ? "oversight" : null,
      work_item: `WI-${1000 + i}`,
      template: "claude",
      resources: live ? resources(i, 0) : null,
      economics: { turns: i, context_tokens: 1000 * i, peak_context_tokens: 2000 * i, compactions: 0 },
      budget: { turns: 120, wall_clock_minutes: 240 },
      hibernation: activity === "hibernated"
        ? { at: iso(3_000_000), trigger: "idle", resumable: true }
        : null,
    });
  }
  return out;
}

function resources(i: number, tick: number): Record<string, unknown> {
  return {
    rss_bytes: 500_000_000 + i * 1_000 + tick * 4096,
    cpu_pct: (i + tick) % 7,
    processes: 8,
    sampled_at: new Date(NOW + tick * 10_000).toISOString(),
  };
}

async function installRoster(page: Page): Promise<void> {
  const sessions = makeSessions();
  await page.addInitScript(() => {
    localStorage.setItem("vogt.token", "browser-test-token");
    localStorage.setItem("vogt.appTheme.v1", "dark");
    localStorage.setItem("vogt.rail.sections.v1", JSON.stringify({ showHidden: true }));

    // Every fetch the page starts, by path, so a test can count refetches.
    const fetches: string[] = [];
    // The event stream, written from the test. Each open is a new stream; an
    // abort of one is the client tearing its own stream down.
    const sse = {
      opens: 0,
      aborts: 0,
      controller: null as ReadableStreamDefaultController<Uint8Array> | null,
      push(events: unknown[]) {
        const text = events.map((ev) => `data: ${JSON.stringify(ev)}\n\n`).join("");
        sse.controller?.enqueue(new TextEncoder().encode(text));
      },
    };
    const realFetch = window.fetch.bind(window);
    window.fetch = (input: RequestInfo | URL, init?: RequestInit) => {
      const url = new URL(typeof input === "string" ? input : input instanceof URL ? input.href : input.url, location.href);
      fetches.push(url.pathname + url.search);
      if (url.pathname === "/api/events") {
        sse.opens += 1;
        const body = new ReadableStream<Uint8Array>({
          start(controller) {
            sse.controller = controller;
            controller.enqueue(new TextEncoder().encode(":ok\n\n"));
          },
        });
        init?.signal?.addEventListener("abort", () => {
          sse.aborts += 1;
        });
        return Promise.resolve(new Response(body, {
          status: 200,
          headers: { "content-type": "text/event-stream" },
        }));
      }
      return realFetch(input, init);
    };

    // A row element seen before and added again is a move (Solid's keyed
    // <For> reorders nodes); one never seen before is a remount.
    const seenRows = new WeakSet<Element>();
    const counts = { mutations: 0, records: 0, rowsAdded: 0, rowsRemoved: 0, rowsMoved: 0 };
    const isRow = (node: Node) =>
      node instanceof Element && (node.matches(".session-row") || node.querySelector(".session-row") !== null);
    const rowsIn = (nodes: NodeList) => {
      const out: Element[] = [];
      nodes.forEach((node) => {
        if (!(node instanceof Element)) return;
        if (node.matches(".session-row")) out.push(node);
        out.push(...node.querySelectorAll(".session-row"));
      });
      return out;
    };
    const countRows = (nodes: NodeList) => rowsIn(nodes).length;
    const noteAdded = (nodes: NodeList) => {
      let fresh = 0;
      for (const row of rowsIn(nodes)) {
        if (seenRows.has(row)) counts.rowsMoved += 1;
        else { seenRows.add(row); fresh += 1; }
      }
      return fresh;
    };
    const observer = new MutationObserver((records) => {
      counts.records += records.length;
      for (const record of records) {
        if (record.type === "childList") {
          counts.mutations += record.addedNodes.length + record.removedNodes.length;
          if ([...record.addedNodes].some(isRow)) counts.rowsAdded += noteAdded(record.addedNodes);
          if ([...record.removedNodes].some(isRow)) counts.rowsRemoved += countRows(record.removedNodes);
        } else {
          counts.mutations += 1;
        }
      }
    });
    document.addEventListener("DOMContentLoaded", () => {
      document.querySelectorAll(".session-row").forEach((row) => seenRows.add(row));
      observer.observe(document.body, { subtree: true, childList: true, attributes: true, characterData: true });
    });
    (window as unknown as Record<string, unknown>).__wi2328 = { fetches, sse, counts, observer };
  });

  await page.route("**/api/auth/check", (route) => route.fulfill({ json: {
    ok: true, version: "test", product_version: "test",
    storage: { state_dir: "/tmp/vogt", workspace_root: "/workspace" },
  } }));
  await page.route("**/api/config**", (route) => route.fulfill({ json: {
    assistant_enabled: false, gui_stream_url: null, session_templates: [],
    gui_stream_available: false, vogt: { configured: true },
  } }));
  await page.route(/\/api\/sessions(\?.*)?$/, (route) => {
    const hidden = new URL(route.request().url()).searchParams.get("include") === "hidden";
    return route.fulfill({ json: {
      sessions: hidden ? sessions : sessions.slice(0, VISIBLE),
      hidden_count: TOTAL - VISIBLE,
    } });
  });
  await page.route("**/api/tree**", (route) => route.fulfill({ json: [] }));
  await page.route("**/api/git/status**", (route) => route.fulfill({ json: {
    repo: "", is_repo: false, branch: "", ahead: 0, behind: 0, entries: [],
  } }));
  await page.route("**/api/vogt/place/metrics**", (route) => route.fulfill({ json: {
    inbox_active: 590, inbox_active_unfiltered: 590, inbox_filter: null, projects_total: 78,
    work_total: 1242, backlog_total_considered: 1151, drift_present: false, revision: 1,
    generated_at: "2026-10-23T20:37:36Z",
  } }));
  await page.route("**/api/vogt/sessions**", (route) => route.fulfill({ json: {
    sessions: [], digests: {}, engine: null, total: 0, truncated: false, hidden_count: 0,
  } }));
  await page.route("**/api/vogt/**", (route) => route.fulfill({ status: 404, json: { error: "not found" } }));
  await page.route("**/api/status**", (route) => route.fulfill({ json: { version: "test", session_count: TOTAL } }));
}

interface Cost {
  mutations: number;
  rowsAdded: number;
  rowsRemoved: number;
  rowsMoved: number;
  fetches: string[];
  handlerMs: number;
  frameMs: number;
  opens: number;
  aborts: number;
}

/** Push one batch of frames and measure until the page has settled. */
async function measure(page: Page, events: unknown[], settleMs = 400): Promise<Cost> {
  return page.evaluate(async ({ events, settleMs }) => {
    const h = (window as unknown as { __wi2328: {
      fetches: string[];
      sse: { opens: number; aborts: number; push(events: unknown[]): void };
      counts: { mutations: number; rowsAdded: number; rowsRemoved: number; rowsMoved: number };
      observer: MutationObserver;
    } }).__wi2328;
    // Drain anything still queued so it is not charged to this event.
    h.observer.takeRecords();
    h.counts.mutations = 0;
    h.counts.rowsAdded = 0;
    h.counts.rowsRemoved = 0;
    h.counts.rowsMoved = 0;
    const fetchStart = h.fetches.length;
    const opens = h.sse.opens;
    const aborts = h.sse.aborts;
    const t0 = performance.now();
    h.sse.push(events);
    // The reader resolves on a later task; the handler and every synchronous
    // effect it triggers run before this timeout.
    await new Promise((resolve) => setTimeout(resolve, 0));
    const t1 = performance.now();
    document.body.getBoundingClientRect();
    await new Promise((resolve) => requestAnimationFrame(() => resolve(null)));
    const t2 = performance.now();
    await new Promise((resolve) => setTimeout(resolve, settleMs));
    // Fold in records the observer has not delivered yet.
    const pending = h.observer.takeRecords();
    let extra = 0;
    for (const record of pending) extra += record.type === "childList" ? record.addedNodes.length + record.removedNodes.length : 1;
    return {
      mutations: h.counts.mutations + extra,
      rowsAdded: h.counts.rowsAdded,
      rowsRemoved: h.counts.rowsRemoved,
      rowsMoved: h.counts.rowsMoved,
      fetches: h.fetches.slice(fetchStart),
      handlerMs: Math.round((t1 - t0) * 10) / 10,
      frameMs: Math.round((t2 - t0) * 10) / 10,
      opens: h.sse.opens - opens,
      aborts: h.sse.aborts - aborts,
    };
  }, { events, settleMs });
}

function liveIds(): string[] {
  return Array.from({ length: 13 }, (_, i) => `00000000-0000-4000-8000-${String(i).padStart(12, "0")}`);
}

function resourcesTick(tick: number): unknown {
  return {
    type: "session-resources",
    samples: liveIds().slice(0, 11).map((id, i) => ({ id, resources: resources(i, tick) })),
  };
}

function vogtBurst(n: number, startSeq: number): unknown[] {
  return Array.from({ length: n }, (_, i) => ({
    type: "vogt-changed",
    kind: "sweep.completed",
    entity_kind: "sweep",
    entity_id: `swp_${startSeq + i}`,
    seq: startSeq + i,
    summary: { collector: "forge-checks", new: 0, outcome: "ok" },
  }));
}

async function openRail(page: Page): Promise<void> {
  await installRoster(page);
  await page.goto("/#/sessions");
  await expect(page.locator(".places-rail-session-area .session-row").first()).toBeVisible({ timeout: 20_000 });
  // Let boot reads and the first wake finish before measuring.
  await page.waitForTimeout(1500);
}

const report = (label: string, cost: Cost) => {
  if (!process.env.VOGT_RUN_PERF) return;
  const paths = cost.fetches.map((f) => f.split("?")[0]);
  console.log(JSON.stringify({ label, ...cost, fetches: paths.length, fetchPaths: [...new Set(paths)] }));
};

test.describe("session rail render cost (WI-2328)", () => {
  test.setTimeout(90_000);

  test("a session-resources tick changes no rows and fetches nothing", async ({ page }) => {
    await openRail(page);
    const rows = await page.locator(".session-row").count();
    if (process.env.VOGT_RUN_PERF) console.log(JSON.stringify({ label: "rows", rows }));
    let worst: Cost | null = null;
    for (let tick = 1; tick <= 5; tick += 1) {
      const cost = await measure(page, [resourcesTick(tick)]);
      report(`session-resources#${tick}`, cost);
      if (!worst || cost.mutations > worst.mutations) worst = cost;
    }
    expect(worst!.rowsAdded).toBe(0);
    expect(worst!.rowsRemoved).toBe(0);
    expect(worst!.fetches).toEqual([]);
    expect(worst!.aborts).toBe(0);
  });

  // Fails until the rail rows are keyed by id (WI-2328 part 3): today an
  // activity event remounts 3–4 rows.
  test("an activity event touches only its own row", async ({ page }) => {
    await openRail(page);
    const id = liveIds()[3];
    const states = Array.from({ length: 12 }, (_, i) => (i % 2 ? "idle" : "running"));
    let worst: Cost | null = null;
    for (const [i, state] of states.entries()) {
      const cost = await measure(page, [{
        type: "activity", id, state, activity_changed_at: new Date(NOW + 1000 * (i + 1)).toISOString(),
      }]);
      report(`activity#${i + 1}:${state}`, cost);
      if (!worst || cost.rowsAdded + cost.rowsMoved > worst.rowsAdded + worst.rowsMoved) worst = cost;
    }
    // No row is rebuilt; a row that changes section is moved, not remounted.
    expect(worst!.rowsAdded).toBe(0);
    // The rail and the Sessions page list each move the row once.
    expect(worst!.rowsMoved).toBeLessThanOrEqual(4);
    expect(worst!.fetches).toEqual([]);
  });

  test("a burst of vogt-changed refetches each surface at most once", async ({ page }) => {
    await openRail(page);
    const cost = await measure(page, vogtBurst(50, 2752), 2500);
    report("vogt-changed x50", cost);
    const perPath = new Map<string, number>();
    for (const f of cost.fetches) {
      const path = f.split("?")[0];
      perPath.set(path, (perPath.get(path) ?? 0) + 1);
    }
    if (process.env.VOGT_RUN_PERF) console.log(JSON.stringify({ label: "vogt-changed per path", perPath: Object.fromEntries(perPath) }));
    for (const [path, n] of perPath) {
      expect(n, `${path} refetched ${n} times for one burst`).toBeLessThanOrEqual(2);
    }
    expect(cost.rowsAdded).toBe(0);
    expect(cost.aborts).toBe(0);
  });

  test("the stream stays open under prod event load", async ({ page }) => {
    await openRail(page);
    let total = { opens: 0, aborts: 0, fetches: 0 };
    for (let round = 0; round < 3; round += 1) {
      const cost = await measure(page, [resourcesTick(10 + round), ...vogtBurst(50, 3000 + round * 50)], 1000);
      total = { opens: total.opens + cost.opens, aborts: total.aborts + cost.aborts, fetches: total.fetches + cost.fetches.length };
    }
    if (process.env.VOGT_RUN_PERF) console.log(JSON.stringify({ label: "load rounds", ...total }));
    expect(total.aborts).toBe(0);
    expect(total.opens).toBe(0);
  });
});

test("profile: where an activity event's time goes", async ({ page, browserName }) => {
  test.skip(!process.env.VOGT_PROFILE || browserName !== "chromium", "set VOGT_PROFILE=1 (chromium)");
  await openRail(page);
  const cdp = await page.context().newCDPSession(page);
  await cdp.send("Profiler.enable");
  await cdp.send("Profiler.setSamplingInterval", { interval: 100 });
  await cdp.send("Profiler.start");
  const id = liveIds()[3];
  for (let i = 0; i < 20; i += 1) {
    await measure(page, [{ type: "activity", id, state: i % 2 ? "idle" : "running", activity_changed_at: new Date(NOW + 1000 * (i + 1)).toISOString() }], 20);
  }
  const { profile } = await cdp.send("Profiler.stop");
  const self = new Map<string, number>();
  const byId = new Map(profile.nodes.map((n) => [n.id, n]));
  const dt = profile.timeDeltas ?? [];
  (profile.samples ?? []).forEach((sid, i) => {
    const n = byId.get(sid)!;
    const key = `${n.callFrame.functionName || "(anon)"} ${n.callFrame.url.split("/").pop()}:${n.callFrame.lineNumber + 1}`;
    self.set(key, (self.get(key) ?? 0) + (dt[i] ?? 0) / 1000);
  });
  const top = [...self.entries()].sort((a, b) => b[1] - a[1]).slice(0, 30);
  console.log(top.map(([k, v]) => `${v.toFixed(1)}ms ${k}`).join("\n"));
});
