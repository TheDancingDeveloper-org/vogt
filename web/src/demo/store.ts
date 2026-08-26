import type { ServerEvent } from "../api";
import type { DemoManifest } from "../runtimeTransport";
import { createDemoState, DEMO_NOW, type DemoState } from "./fixtures";

const STORAGE_KEY = "vogt.demo.state.v1";
const LOGICAL_TICK_MS = 1_000;

function json(value: unknown, init: ResponseInit = {}): Response {
  return new Response(JSON.stringify(value), {
    ...init,
    headers: { "Content-Type": "application/json", ...(init.headers ?? {}) },
  });
}

function refusal(message: string, status = 403): Response {
  return json({ error: { code: "demo.refused", message } }, { status });
}

function bodyOf(init?: RequestInit): Record<string, unknown> {
  if (typeof init?.body !== "string" || !init.body) return {};
  try {
    const value: unknown = JSON.parse(init.body);
    return value && typeof value === "object" ? value as Record<string, unknown> : {};
  } catch {
    return {};
  }
}

function hash(content: string): string {
  let value = 2166136261;
  for (const char of content) value = Math.imul(value ^ char.charCodeAt(0), 16777619);
  return `demo-${(value >>> 0).toString(16).padStart(8, "0")}`;
}

function workflowFor(kind: string): Record<string, unknown> {
  if (kind === "bug") return { kind, initial_state: "open", states: ["open", "in_progress", "done"], transitions: { open: ["in_progress"], in_progress: ["open", "done"], done: [] } };
  return { kind, initial_state: "open", states: ["open", "in_progress", "review", "done", "wont_do"], transitions: { open: ["in_progress", "wont_do"], in_progress: ["open", "review", "done"], review: ["in_progress", "done"], done: [], wont_do: [] } };
}

export class DemoStore {
  private state: DemoState;
  constructor(private readonly provenance: Pick<DemoManifest, "product_version" | "source_ref" | "source_sha"> = {
    product_version: "local/dev",
    source_ref: "local/dev",
    source_sha: "local/dev",
  }) {
    this.state = this.read();
  }
  private listeners = new Set<(event: ServerEvent) => void>();

  /** A stable, scenario-local clock: repeat tours produce repeat timestamps. */
  private now(): string {
    return new Date(Date.parse(DEMO_NOW) + this.state.next_id * LOGICAL_TICK_MS).toISOString();
  }

  private nowMs(): number {
    return Date.parse(this.now());
  }

  private read(): DemoState {
    try {
      const raw = sessionStorage.getItem(STORAGE_KEY);
      if (raw) {
        const parsed = JSON.parse(raw) as Partial<DemoState>;
        if (parsed.schema === 1 && parsed.work && parsed.sessions) return parsed as DemoState;
      }
    } catch {
      // A disabled sessionStorage simply makes the demo reset on reload.
    }
    const initial = createDemoState();
    this.write(initial);
    return initial;
  }

  private write(next = this.state): void {
    this.state = next;
    try { sessionStorage.setItem(STORAGE_KEY, JSON.stringify(next)); } catch { /* best effort */ }
  }

  reset(): void {
    try { sessionStorage.removeItem(STORAGE_KEY); } catch { /* best effort */ }
    for (const key of [
      "vogt.tabs.v2", "vogt.terminalLayouts.v1", "vogt.workspaceLayouts.v1",
      "vogt.places.v1", "vogt.recentPlaces.v1", "vogt.inbox.seen.v1",
      "vogt.demo.presentation.v1",
    ]) localStorage.removeItem(key);
    this.state = createDemoState();
    this.seedPresentation();
  }

  seedPresentation(): void {
    if (localStorage.getItem("vogt.demo.presentation.v1") === "seeded") return;
    localStorage.setItem("vogt.demo.presentation.v1", "seeded");
    localStorage.setItem("vogt.tabs.v2", JSON.stringify({
      tabs: [{ id: "term:demo-build", kind: "terminal", sessionId: "demo-build", label: "Build + tests" }],
      active: "term:demo-build",
    }));
    localStorage.setItem("vogt.terminalLayouts.v1", JSON.stringify({
      "term:demo-build": {
        root: { type: "split", id: "split:build-tests", direction: "row", children: [
          { type: "pane", id: "pane:demo-build", sessionId: "demo-build" },
          { type: "pane", id: "pane:demo-tests", sessionId: "demo-tests" },
        ] }, activePaneId: "pane:demo-build", broadcast: false,
      },
      "term:demo-agent": {
        root: { type: "split", id: "split:agent-review", direction: "row", children: [
          { type: "pane", id: "pane:demo-agent", sessionId: "demo-agent" },
          { type: "split", id: "split:agent-right", direction: "column", children: [
            { type: "pane", id: "pane:demo-server", sessionId: "demo-server" },
            { type: "pane", id: "pane:demo-logs", sessionId: "demo-logs" },
          ] },
        ] }, activePaneId: "pane:demo-agent", broadcast: false,
      },
      "term:demo-logs": {
        root: { type: "split", id: "split:incident", direction: "row", children: [
          { type: "pane", id: "pane:demo-logs", sessionId: "demo-logs" },
          { type: "pane", id: "pane:demo-metrics", sessionId: "demo-metrics" },
          { type: "pane", id: "pane:demo-shell", sessionId: "demo-shell" },
        ] }, activePaneId: "pane:demo-logs", broadcast: true,
      },
    }));
    localStorage.setItem("vogt.workspaceLayouts.v1", JSON.stringify([
      { id: "demo-layout-build", name: "Demo · Build + tests", layout_mode: "tabbed", tabs: [{ id: "term:demo-build", kind: "terminal", sessionId: "demo-build", label: "Build + tests" }], active: "term:demo-build", created_at: DEMO_NOW, updated_at: DEMO_NOW },
      { id: "demo-layout-agent", name: "Demo · Agent review", layout_mode: "tabbed", tabs: [{ id: "term:demo-agent", kind: "terminal", sessionId: "demo-agent", label: "Agent review" }], active: "term:demo-agent", created_at: DEMO_NOW, updated_at: "2026-08-24T14:59:00Z" },
      { id: "demo-layout-incident", name: "Demo · Incident view", layout_mode: "tabbed", tabs: [{ id: "term:demo-logs", kind: "terminal", sessionId: "demo-logs", label: "Incident view" }], active: "term:demo-logs", created_at: DEMO_NOW, updated_at: "2026-08-24T14:58:00Z" },
    ]));
    localStorage.setItem("vogt.guiLaunchers", JSON.stringify([
      { id: "demo-ide", label: "Orbit IDE", command: "demo ide" },
      { id: "demo-browser", label: "Preview browser", command: "demo browser" },
    ]));
  }

  subscribe(listener: (event: ServerEvent) => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  private emit(event: ServerEvent): void {
    for (const listener of this.listeners) listener(event);
  }

  private changed(kind: string, entityKind: string, entityId: string): void {
    this.state.revision += 1;
    this.write();
    this.emit({ type: "vogt-changed", kind, entity_kind: entityKind, entity_id: entityId, seq: this.state.revision });
  }

  terminalTranscript(id: string): string {
    const title = String(this.state.sessions[id]?.name ?? id);
    const lines: Record<string, string> = {
      "demo-build": "\x1b[1;36mVogt demo · Build PWA\x1b[0m\r\n$ pnpm build\r\n✓ TypeScript checked\r\n✓ 19 routes bundled\r\ntransforming modules… 812/812\r\nrendering chunks…\r\n",
      "demo-tests": "\x1b[1;35mVogt demo · Test suite\x1b[0m\r\n$ vitest run\r\n✓ transport  12 tests\r\n✓ terminal layout  18 tests\r\n✓ demo fixture contract  31 tests\r\nTest Files  27 passed\r\n",
      "demo-agent": "\x1b[1;33mAgent review · WI-104\x1b[0m\r\nI compared desktop, breakpoint, and phone compositions.\r\nFinding: the demo disclosure remains above the first fold.\r\n\r\n? Approve the updated phone composition [y/N]\r\n",
      "demo-server": "VITE v8.0.14  ready in 312 ms\r\n➜ Local: http://127.0.0.1:4173/\r\nGET /#/board 200 18ms\r\nGET /#/t/demo-agent 200 11ms\r\n",
      "demo-logs": "14:48:02 info request route=/api/events mode=demo\r\n14:48:10 info session id=demo-agent state=waiting-for-input\r\n14:48:14 info browser width=390 route=/inbox\r\n",
      "demo-metrics": "route_sweep_ok 19\nfailed_requests 0\nhorizontal_overflow 0\nactive_demo_tabs 1\n",
      "demo-shell": "Welcome to the simulated terminal.\r\nTry: help, status, git status, pnpm test\r\n",
      "demo-finished": "release provenance check\r\nasset identity: PASS\r\nexit 0\r\n",
    };
    return lines[id] ?? `\x1b[1m${title}\x1b[0m\r\nThis session is simulated in your browser.\r\n`;
  }

  liveTerminalFrames(id: string): [number, string][] {
    if (id === "demo-build") return [[700, "computing gzip size…\r\n"], [1400, "\x1b[32m✓ built in 3.08s\x1b[0m\r\n"]];
    if (id === "demo-tests") return [[900, "Assertions  284 passed\r\n"], [1700, "Duration  6.42s\r\n"]];
    if (id === "demo-logs") return [[1200, "14:49:01 info demo heartbeat state=healthy\r\n"]];
    return [];
  }

  cannedTerminalResponse(raw: string): string {
    const command = raw.trim().toLowerCase();
    if (!command) return "\x1b[38;5;111mvisitor@vogt-demo\x1b[0m:\x1b[38;5;110m~/Working/orbit\x1b[0m$ ";
    if (command === "help") return "Available canned commands: help, status, git status, pnpm test, clear\r\nNo input is executed.\r\n";
    if (command === "status") return "demo mode: active\r\nstate: private to this tab\r\nnetwork processes: none\r\n";
    if (command === "git status") return "On branch demo/full-estate\r\nChanges staged: src/runtime.ts\r\nChanges not staged: README.md\r\nUntracked: docs/demo-notes.md\r\n";
    if (command === "pnpm test") return "✓ 284 assertions passed in 6.42s\r\n";
    if (command === "clear") return "\x1bc";
    return `demo: '${raw.trim()}' is not executed; type help for canned commands\r\n`;
  }

  private audit(operation: string, entityKind: string, entityId: string, reason: string): void {
    const n = this.state.next_id++;
    const id = `audit-${n}`;
    this.state.audit.unshift({ id, txn_id: `txn-${n}`, revision: this.state.revision + 1, actor_id: "actor-visitor", actor_identity_ref: "demo:visitor", operation, entity_kind: entityKind, entity_id: entityId, reason, payload_digest: `sha256:demo${n}`, at: this.now() });
    this.state.events.unshift({ seq: this.state.revision + 1, kind: `${operation}.demo`, entity_kind: entityKind, entity_id: entityId, actor_id: "actor-visitor", audit_id: id, summary: { demo: true }, at: this.now() });
  }

  private sessionsForVogt(): Record<string, unknown>[] {
    return Object.values(this.state.sessions).map((session, index) => ({
      id: `vogt-session-${String(session.id)}`, engine_session_id: session.id,
      project: index % 3 === 0 ? "lighthouse" : "orbit", work_item: index % 2 === 0 ? "WI-101" : "WI-102",
      actor: "demo:visitor", cwd: session.cwd, template: index === 0 ? "Agent review" : "Shell",
      model: index === 0 ? "demo-model" : null, effort: index === 0 ? "medium" : null,
      reason: "show the public demo", started_at: session.created_at,
      stopped_at: session.exit_code === null ? null : "2026-08-24T14:30:00Z",
      activity: session.activity, alive: session.exit_code === null,
    }));
  }

  async request(pathname: string, method: string, query: URLSearchParams, init?: RequestInit): Promise<Response> {
    const body = bodyOf(init);
    if (pathname.startsWith("/api/vogt/")) return this.vogt(pathname.slice("/api/vogt".length), method, query, body);
    return this.engine(pathname, method, query, body);
  }

  private async engine(path: string, method: string, query: URLSearchParams, body: Record<string, unknown>): Promise<Response> {
    const product = {
      version: this.provenance.product_version,
      product_version: this.provenance.product_version,
      source_ref: this.provenance.source_ref,
      source_sha: this.provenance.source_sha,
      release_url: this.provenance.source_ref.startsWith("v") && this.provenance.product_version !== "local/dev"
        ? `https://github.com/TheDancingDeveloper-org/vogt/releases/tag/${this.provenance.source_ref}`
        : null,
    };
    if (path === "/api/auth/check") return json({ ...product, ok: true, storage: { state_dir: "browser sessionStorage (ephemeral)", workspace_root: "/Working (simulated)" } });
    if (path === "/api/status") return json({ ...product, session_count: Object.keys(this.state.sessions).length, push_subscription_count: 1, gui_process_count: 1, gui_stream_configured: true, fcm_enabled: false, history: { enabled: true, archived_session_count: 4, log_file_count: 4, log_bytes: 184320, db_bytes: 49152 }, agent_tasks: { task_count: this.state.tasks.length, prompt_task_dir_count: 3, prompt_file_count: 8, context_file_count: 6, prompt_bytes: 24576, orphan_task_dir_count: 0 }, auth_broker: { auto_agent_auth: false, helper: "not configured in public demo" }, storage: { state_dir: "browser sessionStorage (ephemeral)", workspace_root: "/Working (simulated)" } });
    if (path === "/api/config") return json({ ...product, gui_stream_url: "/demo-gui.html", gui_stream_available: true, assistant_enabled: true, assistant_stt_enabled: false, assistant_tts_enabled: false, assistant_model: "demo-model", assistant_profiles: [{ name: "Guided demo", model: "demo-model", default: true }, { name: "Concise", model: "demo-model-mini", default: false }], features: { demo: "full-estate-v1" }, vogt: { configured: true, api_prefix: "/api/vogt", mcp_prefix: "/mcp" }, session_templates: [{ name: "Shell", description: "A safe simulated shell with canned commands", command: null, cwd: "/Working/orbit", env: [], default_name: "Demo shell" }, { name: "Agent review", description: "Waiting-for-input review session", command: ["demo-agent"], cwd: "/Working/orbit", env: [], default_name: "Agent review" }] });
    if (path === "/api/install/status") return json({ install_mode: false });
    if (path === "/healthz") return json({ ok: true });
    if (path === "/api/sessions" && method === "GET") return json(Object.values(this.state.sessions));
    if (path === "/api/sessions" && method === "POST") {
      const id = `demo-new-${this.state.next_id++}`;
      const row = { id, name: String(body.name || "Demo session"), activity: "idle", exit_code: null, scrollback_bytes: Number(body.scrollback_bytes ?? 131072), cwd: String(body.cwd || "/Working/orbit"), command: Array.isArray(body.command) ? body.command.join(" ") : null, created_at: this.now(), activity_changed_at: this.now() };
      this.state.sessions[id] = row;
      this.write(); this.emit({ type: "session-created", id, name: String(row.name) });
      return json(row);
    }
    const sessionMatch = /^\/api\/sessions\/([^/]+)(?:\/(kill|input))?$/.exec(path);
    if (sessionMatch) {
      const id = decodeURIComponent(sessionMatch[1] ?? "");
      const action = sessionMatch[2];
      const row = this.state.sessions[id];
      if (!row) return json({ error: "session not found" }, { status: 404 });
      if (method === "GET") return json({ summary: row, scrollback_pos: 0, scrollback_base64: "" });
      if (method === "PATCH") { row.name = String(body.name || row.name); this.write(); this.emit({ type: "session-renamed", id, name: String(row.name) }); return json({ ok: true }); }
      if (action === "kill") { row.exit_code = 0; row.activity = "idle"; this.write(); this.emit({ type: "session-killed", id, exit_code: 0 }); return json({ ok: true }); }
      if (action === "input") return json({ ok: true });
      if (method === "DELETE") { delete this.state.sessions[id]; this.write(); return json({ ok: true }); }
    }
    if (path === "/api/dir" || path === "/api/tree") return json(this.directory(query.get("path") ?? "", path === "/api/tree"));
    if (path === "/api/files" && method === "GET") {
      const key = query.get("path") ?? ""; const file = this.state.files[key];
      if (!file) return json({ error: "not found" }, { status: 404 });
      return json({ path: key, size: file.content?.length ?? 96, content: file.binary ? null : file.content, content_base64: file.binary ? "iVBORw0KGgo=" : null, is_binary: Boolean(file.binary), mtime: file.mtime, hash: file.hash });
    }
    if (path === "/api/files/download" && method === "GET") {
      const key = query.get("path") ?? ""; const file = this.state.files[key];
      if (!file) return json({ error: "not found" }, { status: 404 });
      return new Response(file.content ?? "demo binary preview", {
        headers: {
          "Content-Type": file.binary ? "application/octet-stream" : "text/plain; charset=utf-8",
          "Content-Disposition": `attachment; filename="${key.split("/").at(-1) ?? "download"}"`,
        },
      });
    }
    if (path === "/api/files" && method === "PUT") {
      const key = String(body.path ?? ""); const current = this.state.files[key];
      if (body.if_match && current && current.hash !== body.if_match) return json({ error: "file changed since it was opened", code: "stale_write" }, { status: 409 });
      const content = typeof body.content === "string" ? body.content : atob(String(body.content_base64 ?? ""));
      const row = { content, mtime: this.nowMs(), hash: hash(content) };
      this.state.files[key] = row; this.write();
      return json({ ok: true, bytes: content.length, mtime: row.mtime, hash: row.hash });
    }
    if (path === "/api/files/op") { this.fileOp(body); this.write(); return json({ ok: true, path: body.to ?? body.path }); }
    if (path === "/api/search/files") { const needle = (query.get("q") ?? "").toLowerCase(); return json(Object.keys(this.state.files).filter((key) => key.toLowerCase().includes(needle)).map((key) => ({ path: key, name: key.split("/").at(-1) }))); }
    if (path === "/api/search") { const needle = (query.get("q") ?? "").toLowerCase(); const hits = Object.entries(this.state.files).flatMap(([file, row]) => (row.content ?? "").split("\n").map((text, i) => ({ path: file, line: i + 1, text })).filter((hit) => hit.text.toLowerCase().includes(needle))); return json(hits); }
    if (path === "/api/git/status") return json({ repo: query.get("repo") ?? "/Working/orbit", is_repo: true, branch: "demo/full-estate", ahead: 2, behind: 0, entries: this.state.git_entries });
    if (path === "/api/git/branch") return json({ current: "demo/full-estate", all: ["demo/full-estate", "main", "release/preview"] });
    if (path === "/api/git/log") return json([{ hash: "9c1f20a3", short: "9c1f20a", author: "Ana Demo", date: "2026-08-24T14:10:00Z", subject: "Populate every demo route" }, { hash: "44ee121f", short: "44ee121", author: "Lin Demo", date: "2026-08-24T12:30:00Z", subject: "Add terminal split scenarios" }, { hash: "1a520d99", short: "1a520d9", author: "Maya Demo", date: "2026-08-23T18:00:00Z", subject: "Introduce runtime transport seam" }]);
    if (path === "/api/git/diff") { const file = query.get("path") ?? "README.md"; const current = this.state.files[file]?.content ?? ""; return json({ path: file, head: current.replace("trustworthy, representative", "representative"), current }); }
    if (path === "/api/git/op") { this.gitOp(body); this.write(); return json({ ok: true, branch: String(body.branch ?? "demo/full-estate"), commit: body.op === "commit" ? "demo200" : undefined }); }
    if (path === "/api/history/sessions") return json(this.historySessions());
    if (path === "/api/history/search") { const q = query.get("q") ?? "demo"; return json(this.historySessions().map((row, index) => ({ session_id: row.id, session_name: row.name, created_at: row.created_at, match_snippet: `${row.name} contains ${q} output`, rank: index + 1 }))); }
    const historyMatch = /^\/api\/history\/([^/]+)(?:\/(log|download))?$/.exec(path);
    if (historyMatch) { const id = historyMatch[1] ?? ""; const row = this.historySessions().find((entry) => entry.id === id); if (!row) return json({ error: "not found" }, { status: 404 }); if (historyMatch[2] === "log") return json({ session_id: id, text: this.terminalTranscript(id === "history-build" ? "demo-build" : "demo-tests") + "\n[preview truncated to the final 64 KiB]", bytes: 4096, total_bytes: 131072, truncated: true }); if (historyMatch[2] === "download") return new Response(this.terminalTranscript(id === "history-build" ? "demo-build" : "demo-tests"), { headers: { "Content-Type": "text/plain; charset=utf-8", "Content-Disposition": `attachment; filename="${id}.log"` } }); if (method === "DELETE") return json({ ok: true }); return json(row); }
    if (path === "/api/history/cleanup") return json({ ok: true, removed_sessions: 1, retention_days: Number(body.retention_days ?? 30) });
    if (path === "/api/agent-tasks" && method === "GET") return json(this.state.tasks);
    if (path === "/api/agent-tasks" && method === "POST") { const row = { ...body, id: `task-${this.state.next_id++}`, status: body.enabled === false ? "paused" : "active", runs: [], run_count: 0, concurrency: Number(body.concurrency ?? 1), created_at: this.now(), updated_at: this.now(), next_run: null, last_run: null }; this.state.tasks.unshift(row); this.write(); return json(row); }
    const taskMatch = /^\/api\/agent-tasks\/([^/]+)(?:\/(run|pause|resume|steer|gates)(?:\/([^/]+)\/answer)?)?$/.exec(path);
    if (taskMatch) return this.taskRequest(taskMatch[1] ?? "", taskMatch[2], taskMatch[3], method, body);
    if (path === "/api/agent-tasks/artifacts/cleanup") return json({ removed_task_dir_count: 1, removed_prompt_file_count: 2, removed_context_file_count: 2, removed_bytes: 8192 });
    if (path === "/api/assistant/history") return json(this.state.assistant);
    if (path === "/api/assistant/message") { const text = String(body.text ?? ""); const reply = `I can demonstrate **${text || "that"}** without reaching a server. Try Board, Inbox, or the nested Agent review terminal layout.`; this.state.assistant.transcript.push({ role: "user", text }, { role: "assistant", text: reply, tool_trace: ["read deterministic demo state"] }); this.write(); return json({ reply, pending_action: this.state.assistant.pending_action, tool_trace: ["read deterministic demo state"] }); }
    if (path === "/api/assistant/reset") { this.state.assistant.transcript = []; this.state.assistant.pending_action = null; this.write(); return json({ ok: true }); }
    const actionMatch = /^\/api\/assistant\/actions\/([^/]+)$/.exec(path);
    if (actionMatch) { if (method === "PATCH") { const action = this.state.assistant.pending_action; if (action) action.reason = String(body.reason ?? action.reason); this.write(); return json(action); } this.state.assistant.pending_action = null; this.write(); return json({ reply: body.approve ? "Approved in demo state; no external effect occurred." : "Denied. Nothing changed.", pending_action: null, tool_trace: ["update browser-only approval"] }); }
    if (path === "/api/assistant/stt" || path === "/api/assistant/tts") return refusal("Speech capture and playback are unavailable in the public demo.", 404);
    if (path === "/api/gui/processes") return json([{ pid: 4242, command: ["demo", "ide"], launched_at: "2026-08-24T13:30:00Z" }]);
    if (path.startsWith("/api/gui/")) return refusal("The public demo cannot launch or kill a process. The visible desktop is a static simulation.");
    if (path === "/api/push/list") return json([{ id: "demo-browser", label: "Demo browser", created_at: "2026-08-24T12:00:00Z", kind: { kind: "web-push", endpoint_host: "demo.invalid" }, prefs: { waiting_for_input: true, errored: true, idle_stall: false, agent_task_started: true, agent_task_notify: true, drift: true, quiet_hours: { enabled: true, start_minute: 1320, end_minute: 420, utc_offset_minutes: 0, digest: true } }, pending_digest_count: 2, pending_digest_since: "2026-08-24T14:00:00Z" }]);
    if (path === "/api/push/public-key") return json({ vapid_public_key: "", fcm_enabled: false });
    if (path.startsWith("/api/push/")) return refusal("Push delivery is disabled in the public demo.");
    return json({ error: `No demo response for ${method} ${path}` }, { status: 404 });
  }

  private async vogt(path: string, method: string, query: URLSearchParams, body: Record<string, unknown>): Promise<Response> {
    const params = method === "GET" ? Object.fromEntries(query.entries()) : body;
    if (path === "/status") return json({ ok: true });
    if (path === "/place/metrics") return json({
      inbox_active: this.state.inbox.filter((row) => row.triage_state === "active").length,
      projects_total: this.projects().length,
      work_total: this.filterWork({}).length,
      backlog_total_considered: this.filterWork({}).length + 2,
      drift_present: this.state.drift.some((row) => row.status === "open"),
      revision: this.state.revision,
      generated_at: DEMO_NOW,
    });
    if (path === "/workflows") return json({ workflows: [workflowFor("feature"), workflowFor("bug"), workflowFor("chore"), workflowFor("question")] });
    if (path === "/work" && method === "GET") return json({ items: this.filterWork(params), total: this.filterWork(params).length, link_state: "linked" });
    if (path === "/work" && method === "POST") { const ref = `WI-${this.state.next_id++}`; const row = { ...this.state.work[0]!, ...body, id: `work-${this.state.next_id}`, ref, state: "open", created_at: this.now(), updated_at: this.now() }; this.state.work.unshift(row); this.audit("work.create", "work_item", row.id, String(body.reason)); this.changed("work.created", "work_item", row.id); return json({ item: row, comments: [], sessions: [] }); }
    if (path === "/board/list") { const cells = Array.isArray(body.cells) ? body.cells as Record<string, unknown>[] : []; const selected = this.filterWork(body); const laneMode = String(body.lane_mode ?? "none"); const lane = (item: Record<string, unknown>) => laneMode === "project" ? String(item.project_slug ?? "") : laneMode === "initiative" ? String(item.initiative_id ?? "") : ""; const columnTotals: Record<string, number> = {}; const laneTotals: Record<string, number> = {}; for (const item of selected) { columnTotals[item.state] = (columnTotals[item.state] ?? 0) + 1; const key = lane(item); laneTotals[key] = (laneTotals[key] ?? 0) + 1; } return json({ cells: cells.map((cell) => { const rows = selected.filter((item) => item.state === cell.state && lane(item) === String(cell.lane_key ?? "")); return { lane_key: cell.lane_key ?? "", state: cell.state, items: rows, total: rows.length, next_cursor: null }; }), column_totals: columnTotals, lane_totals: laneTotals, total: selected.length, backlog_candidates: selected.length + 2, declared_total: selected.length, link_state: "linked", excluded_unlinked: 0, snapshot: `demo-${this.state.revision}`, snapshot_at: DEMO_NOW, revision: this.state.revision }); }
    if (path === "/work/get") { const ref = String(params.ref ?? ""); const item = this.state.work.find((row) => row.ref === ref); if (!item) return json({ error: { message: `no work item ${ref}` } }, { status: 404 }); return json(this.workDetail(item)); }
    if (path === "/work/transition") return this.mutateWork(body, "work.transition", (item) => { item.state = String(body.to_state ?? item.state); });
    if (path === "/work/update") return this.mutateWork(body, "work.update", (item) => { for (const key of ["title", "priority", "assignee_identity_ref"] as const) if (body[key] !== undefined) (item as Record<string, unknown>)[key] = body[key]; });
    if (path === "/work/comment") { const item = this.state.work.find((row) => row.ref === body.ref); if (!item) return refusal("Work item not found", 404); const row = { id: `comment-${this.state.next_id++}`, body: String(body.body ?? ""), created_at: this.now() }; (this.state.comments[item.ref] ??= []).push(row); this.audit("work.comment", "work_item", item.id, String(body.reason)); this.changed("work.commented", "work_item", item.id); return json({ comment: row }); }
    if (path === "/backlog" || path === "/bugs") { const items = this.filterWork({ ...params, ...(path === "/bugs" ? { kind: "bug" } : {}) }).map((item, index) => ({ ...item, origin: item.origin ?? "declared", score: 9.8 - index * 1.13, source_url: index === 3 ? "https://example.invalid/orbit/issues/104" : null, observed_at: index === 3 ? "2026-08-24T13:20:00Z" : null })); return json({ items, total_considered: items.length + 2, declared: items.length, observed: 2, suppressed: 1, scope: "registered fictional projects", freshness: this.freshness() }); }
    if (path === "/why") { const ref = String(params.ref ?? "WI-101"); const item = this.state.work.find((row) => row.ref === ref); return json({ ref, title: item?.title ?? ref, total: 8.67, contributions: [{ input: "priority", detail: "P1 work is visible first", value: 4, weight: 1.25, contribution: 5 }, { input: "age", detail: "open for thirteen days", value: 13, weight: 0.18, contribution: 2.34 }, { input: "trust", detail: "verified declared work", value: 1, weight: 1.33, contribution: 1.33 }], inputs_not_yet_available: { business_value: "not declared on this item" } }); }
    if (path === "/projects" && method === "GET") return json({ projects: this.projects(), total: this.projects().length });
    if (path === "/projects" && method === "POST") { const slug = String(body.name ?? "demo-project").toLowerCase().replace(/[^a-z0-9]+/g, "-"); this.audit("project.register", "project", slug, String(body.reason)); this.changed("project.registered", "project", slug); return json({ project: { slug, name: body.name } }); }
    if (path === "/projects/get") { const slug = String(params.slug ?? params.project ?? "orbit"); return json({ project: this.projects().find((row) => row.slug === slug) }); }
    if (path === "/projects/brief") return json(this.projectBrief(String(params.slug ?? "orbit")));
    if (path === "/projects/import") return refusal("Repository import would clone and mutate an external checkout, so it is disabled in the public demo.");
    if (path === "/forge/accounts" && method === "GET") return json({ accounts: [{ host: "github.com", login: "demo-operator", scopes: "read:repo", linked: true }] });
    if (path === "/forge/accounts" && method === "POST") return refusal("Linking an external forge account is disabled in the public demo.");
    if (path === "/forge/repos") return json({ repos: [{ owner: "demo-labs", name: "orbit", default_branch: "main", visibility: "public", url: "https://example.invalid/demo-labs/orbit", already_registered: true }, { owner: "demo-labs", name: "northstar", default_branch: "trunk", visibility: "private", url: "https://example.invalid/demo-labs/northstar", already_registered: false }, { owner: "demo-labs", name: "lighthouse", default_branch: "main", visibility: "public", url: "https://example.invalid/demo-labs/lighthouse", already_registered: true }], login: "demo-operator", detail: "Fictional repositories supplied by the demo fixture." });
    if (path === "/sweep") return json({ scope: body.project ? `project:${body.project}` : "all registered projects", projects: body.project ? 1 : 2, subjects: 24, dep_refs: 5 });
    if (path === "/coverage") return json({ collectors: [{ collector: "git-local", status: "current", last_swept_at: "2026-08-24T14:42:00Z", age_seconds: 1080, projects: 2 }, { collector: "markers", status: "current", last_swept_at: "2026-08-24T14:41:00Z", age_seconds: 1140, projects: 2 }, { collector: "forge", status: "current", last_swept_at: "2026-08-24T14:38:00Z", age_seconds: 1320, projects: 2 }], swept_project_ids: ["project-orbit", "project-lighthouse"], unswept_project_ids: [] });
    if (path === "/labels") return json({ labels: [{ name: "demo", color: "#7c5cff" }, { name: "frontend", color: "#4aa8d8" }, { name: "terminal", color: "#d8a84a" }, { name: "security", color: "#d85c70" }, { name: "mobile", color: "#72c78a" }] });
    if (path === "/initiatives") return json({ initiatives: [{ id: "init-demo", slug: "demo-site", title: "Public demo site" }, { id: "init-reliability", slug: "reliability", title: "Session reliability" }] });
    if (path === "/actors") return json({ actors: [{ id: "actor-ana", identity_ref: "demo:ana", display_name: "Ana Demo" }, { id: "actor-lin", identity_ref: "demo:lin", display_name: "Lin Demo" }, { id: "actor-maya", identity_ref: "demo:maya", display_name: "Maya Demo" }, { id: "actor-visitor", identity_ref: "demo:visitor", display_name: "Demo visitor" }] });
    if (path === "/drift") return json({ proposals: this.state.drift.filter((row) => !params.project || row.project_slug === params.project).filter((row) => !params.status || row.status === params.status), human_gated: { ci_red_vs_healthy: "CI health never changes declared project state automatically." }, freshness: this.freshness() });
    if (path === "/drift/resolve") { const row = this.state.drift.find((entry) => entry.id === body.id); if (!row) return refusal("Drift proposal not found", 404); row.status = body.resolution; row.resolved_at = this.now(); row.resolution_reason = body.reason; this.audit("drift.resolve", "drift_proposal", String(row.id), String(body.reason)); this.changed("drift.resolved", "drift_proposal", String(row.id)); return json({ proposal: row, change_applied: body.resolution === "accepted" }); }
    if (path === "/deps") return json({ project: String(params.project ?? "orbit"), references_out: [{ subject_key: "dep:orbit:lighthouse", from_project_id: "project-orbit", from_project_slug: "orbit", ref_kind: "path", raw_target: "../lighthouse", manifest: "pyproject.toml", to_project_id: "project-lighthouse", to_project_slug: "lighthouse", observed_at: "2026-08-24T14:41:00Z" }, { subject_key: "dep:orbit:northstar", from_project_id: "project-orbit", from_project_slug: "orbit", ref_kind: "git", raw_target: "https://example.invalid/demo-labs/northstar", manifest: "package.json", to_project_id: null, to_project_slug: null, observed_at: "2026-08-24T14:41:00Z" }], referenced_by: [{ subject_key: "dep:lighthouse:orbit", from_project_id: "project-lighthouse", from_project_slug: "lighthouse", ref_kind: "path", raw_target: "../orbit", manifest: "Cargo.toml", to_project_id: "project-orbit", to_project_slug: "orbit", observed_at: "2026-08-24T14:41:00Z" }], unresolved: 1, freshness: this.freshness() });
    if (path === "/compliance") return json({ project: String(params.project ?? "orbit"), status: "non_compliant", contract_version: "3", checked_at: "2026-08-24T14:40:00Z", age_seconds: 1200, failing: [{ rule: "demo.provenance", target: "asset manifest", satisfied: false, detail: "One screenshot awaits acceptance." }], detail: "The demo reports, but never enforces, compliance." });
    if (path === "/observations") { const observations = [{ id: "obs-session", sweep_id: "sweep-24", collector: "session-outcomes", kind: "session.outcome", project_id: "project-orbit", subject_key: "session:demo-finished", payload: { session: "vogt-session-demo-finished", engine_session_id: "demo-finished", project: "orbit", work_item: "WI-101", cwd: "/Working/orbit", started_at: "2026-08-24T12:00:00Z", state: "finished", provisional: false, exit_code: 0 }, content_digest: "sha256:session", source_url: null, promoted: true, observed_at: "2026-08-24T14:40:00Z" }, { id: "obs-pr", sweep_id: "sweep-24", collector: "forge", kind: "forge.pull_request", project_id: "project-orbit", subject_key: "repo:orbit:pr:42", payload: { work_item: "WI-101", number: 42, state: "in-review", checks: "red" }, content_digest: "sha256:pr42", source_url: "https://example.invalid/orbit/pull/42", promoted: true, observed_at: "2026-08-24T14:38:00Z" }]; return json({ observations, total: observations.length }); }
    if (path === "/audit") { let rows = [...this.state.audit]; if (params.operation) rows = rows.filter((row) => row.operation === params.operation); if (params.actor_id) rows = rows.filter((row) => row.actor_id === params.actor_id); if (params.entity_id) { const item = this.state.work.find((candidate) => candidate.id === params.entity_id); rows = rows.filter((row) => row.entity_id === params.entity_id || (item && row.entity_id === item.id)); } if (params.project) { const ids = new Set(this.state.work.filter((item) => item.project_slug === params.project).map((item) => item.id)); rows = rows.filter((row) => ids.has(String(row.entity_id)) || row.entity_id === `project-${params.project}`); } const total = rows.length; const offset = Number(params.offset ?? 0); const limit = Number(params.limit ?? 50); return json({ records: rows.slice(offset, offset + limit), total }); }
    if (path === "/notifications") { const rows = [{ thread: "thread-review", project_slug: "orbit", repo: "demo-labs/orbit", title: "Review requested on split-layout showcase", reason: "review_requested", subject_type: "PullRequest", unread: true, url: "https://example.invalid/orbit/pull/42", updated_at: "2026-08-24T14:15:00Z", observed_at: "2026-08-24T14:17:00Z" }, { thread: "thread-mention", project_slug: "lighthouse", repo: "demo-labs/lighthouse", title: "You were mentioned in demo deployment notes", reason: "mention", subject_type: "Issue", unread: true, url: "https://example.invalid/lighthouse/issues/18", updated_at: "2026-08-24T13:00:00Z", observed_at: "2026-08-24T13:02:00Z" }].filter((row) => !params.project || row.project_slug === params.project).filter((row) => !params.reason || row.reason === params.reason).filter((row) => !params.unread_only || row.unread); return json({ notifications: rows, total: rows.length, by_reason: { review_requested: 1, mention: 1 }, unread: rows.filter((row) => row.unread).length, scope: "fictional repositories visible to the demo operator", freshness: this.freshness(), detail: "No upstream read state is changed." }); }
    if (path === "/inbox") { const triage = String(params.triage_state ?? params.state ?? "active"); const entries = this.state.inbox.filter((entry) => String(entry.triage_state ?? "active") === triage); return json({ entries, next_cursor: null, snapshot_at: DEMO_NOW, high_water: { github: "2026-08-24T14:17:00Z", drift: "2026-08-24T14:42:00Z", ci: "2026-08-24T13:53:00Z", agent: "2026-08-24T14:48:00Z" }, coverage: { github: { status: "current", count: 2, observed_at: "2026-08-24T14:17:00Z" }, drift: { status: "current", count: 2, observed_at: "2026-08-24T14:42:00Z" }, ci: { status: "current", count: 2, observed_at: "2026-08-24T13:53:00Z" }, agent: { status: "current", count: 1, observed_at: "2026-08-24T14:48:00Z" } }, counts: { active: this.state.inbox.filter((row) => row.triage_state === "active").length, archived: this.state.inbox.filter((row) => row.triage_state === "archived").length, snoozed: this.state.inbox.filter((row) => row.triage_state === "snoozed").length }, instance_scope: "two registered fictional projects", engine_available: true, engine_status: "available" }); }
    if (["/inbox/archive", "/inbox/snooze", "/inbox/restore"].includes(path)) { const entry = this.state.inbox.find((row) => row.entry_key === body.entry_key); if (!entry) return refusal("Inbox entry not found", 404); entry.triage_state = path.endsWith("archive") ? "archived" : path.endsWith("snooze") ? "snoozed" : "active"; if (path.endsWith("snooze")) entry.snooze_until = body.until; this.audit(`inbox.${path.split("/").at(-1)}`, "inbox_entry", String(entry.entry_key), String(body.reason)); this.changed("inbox.changed", "inbox_entry", String(entry.entry_key)); return json({ entry }); }
    if (path === "/suppressions" || path === "/work/adopt") { this.audit(path === "/suppressions" ? "suppress" : "work.adopt", "subject", String(body.subject), String(body.reason)); this.changed("subject.changed", "subject", String(body.subject)); return json({ ok: true, subject: body.subject }); }
    if (path === "/events") { const after = Number(params.after ?? 0); const rows = this.state.events.filter((row) => Number(row.seq) > after).filter((row) => !params.entity_id || row.entity_id === params.entity_id); return json({ events: rows, next_cursor: rows.reduce((max, row) => Math.max(max, Number(row.seq)), after) }); }
    if (path === "/sessions" && method === "GET") return json({ sessions: this.sessionsForVogt(), engine: "browser-simulated terminal sessions" });
    if (path === "/sessions" && method === "POST") { const id = `demo-new-${this.state.next_id++}`; const row = { id, name: String(body.name ?? "Demo work session"), activity: "idle", exit_code: null, scrollback_bytes: 131072, cwd: String(body.cwd ?? "/Working/orbit"), created_at: this.now(), activity_changed_at: this.now() }; this.state.sessions[id] = row; this.audit("session.start", "session", id, String(body.reason)); this.changed("session.started", "session", id); return json({ session: this.sessionsForVogt().find((session) => session.engine_session_id === id) }); }
    if (path === "/sessions/stop") { const row = this.state.sessions[String(body.id)]; if (row) row.exit_code = 0; this.audit("session.stop", "session", String(body.id), String(body.reason)); this.changed("session.stopped", "session", String(body.id)); return json({ session: this.sessionsForVogt().find((session) => session.engine_session_id === body.id) }); }
    return json({ error: { code: "demo.unhandled", message: `No demo responder for ${method} ${path}` } }, { status: 404 });
  }

  private filterWork(params: Record<string, unknown>): DemoState["work"] {
    let rows = [...this.state.work];
    const project = String(params.project ?? ""); if (project) rows = rows.filter((row) => row.project_slug === project);
    const kind = params.kind; if (kind) { const kinds = Array.isArray(kind) ? kind.map(String) : [String(kind)]; rows = rows.filter((row) => kinds.includes(row.kind)); }
    const state = params.state; if (state) { const states = Array.isArray(state) ? state.map(String) : [String(state)]; rows = rows.filter((row) => states.includes(row.state)); }
    return rows;
  }

  private workDetail(item: DemoState["work"][number]): Record<string, unknown> {
    return { item, comments: this.state.comments[item.ref] ?? [], sessions: this.sessionsForVogt().filter((row) => row.work_item === item.ref), branches: [{ name: `demo/${item.ref.toLowerCase()}`, source: "both", drift: false, tip: "9c1f20a", ahead: 2, behind: 0, default_branch: "main", last_commit_at: "2026-08-24T14:10:00Z", last_commit_age_seconds: 3000, observed_at: "2026-08-24T14:40:00Z" }, { name: `observed/${item.ref.toLowerCase()}-old`, source: "observed", drift: true, tip: "44ee121", ahead: 0, behind: 3, default_branch: "main", last_commit_at: "2026-08-22T11:00:00Z", last_commit_age_seconds: 187200, observed_at: "2026-08-24T14:40:00Z" }], git: { phase: "in_review", workflow_state: item.state, branches: [], pull_request: { number: 42, state: "in-review", title: "Show every route in the public demo", url: "https://example.invalid/orbit/pull/42", draft: false, review_decision: "CHANGES_REQUESTED", checks: "red", mergeable: "MERGEABLE", head_ref: `demo/${item.ref.toLowerCase()}`, base: "main", provenance: "forge", updated_at: "2026-08-24T14:15:00Z", updated_age_seconds: 2700, observed_at: "2026-08-24T14:17:00Z", observed_age_seconds: 2580 }, drift: [{ code: "checks_red", message: "The browser screenshot check is red while the workflow is in review.", provenance: "forge" }], task_conclusion_available: true } };
  }

  private mutateWork(body: Record<string, unknown>, operation: string, mutate: (item: DemoState["work"][number]) => void): Response {
    const item = this.state.work.find((row) => row.ref === body.ref); if (!item) return refusal("Work item not found", 404);
    const reason = String(body.reason ?? "").trim(); if (!reason) return refusal("A reason is required", 422);
    mutate(item); item.updated_at = this.now(); this.audit(operation, "work_item", item.id, reason); this.changed(`${operation}d`, "work_item", item.id); return json(this.workDetail(item));
  }

  private freshness(): Record<string, unknown> { return { status: "fresh", oldest_relevant_sweep: "2026-08-24T14:38:00Z", age_seconds: 1320, collectors: { "git-local": "current", markers: "current", forge: "current" }, detail: "Deterministic snapshot anchored at 2026-08-24 15:00 UTC." }; }

  private projects(): Record<string, unknown>[] { return [{ id: "project-orbit", slug: "orbit", name: "Orbit", root_path: "/Working/orbit", repo_url: "https://example.invalid/demo-labs/orbit", lifecycle_state: "active", trust_state: "verified" }, { id: "project-lighthouse", slug: "lighthouse", name: "Lighthouse", root_path: "/Working/lighthouse", repo_url: "https://example.invalid/demo-labs/lighthouse", lifecycle_state: "active", trust_state: "verified" }]; }

  private projectBrief(slug: string): Record<string, unknown> { const project = this.projects().find((row) => row.slug === slug) ?? this.projects()[0]; const rows = this.state.work.filter((item) => item.project_slug === slug); return { project, open_work: rows.filter((item) => !["done", "wont_do"].includes(item.state)).length, open_bugs: rows.filter((item) => item.kind === "bug" && item.state !== "done").length, by_state: Object.fromEntries(["open", "in_progress", "review", "done"].map((state) => [state, rows.filter((item) => item.state === state).length])), by_kind: Object.fromEntries(["feature", "bug", "chore", "question"].map((kind) => [kind, rows.filter((item) => item.kind === kind).length])), declared_version: "2.4.0", observed_version: "2.5.0", version_matches: false, ci_status: { status: "red", checks: 14, failing: ["browser / phone"], revision: "9c1f20a3", detail: "One intentional visual change awaits review." }, compliance_status: "non_compliant", dependencies: { status: "partial", references_out: 2, referenced_by: 1, unresolved: 1, detail: "One fictional repository is not registered." }, top_backlog: rows.slice(0, 3), freshness: this.freshness() }; }

  private directory(path: string, nested: boolean): Record<string, unknown>[] { const prefix = path ? `${path.replace(/\/$/, "")}/` : ""; const entries = new Map<string, Record<string, unknown>>(); for (const [key, file] of Object.entries(this.state.files)) { if (!key.startsWith(prefix)) continue; const rest = key.slice(prefix.length); if (!rest) continue; const first = rest.split("/")[0]!; const full = `${prefix}${first}`; const isDir = rest.includes("/"); entries.set(first, { name: first, path: full, is_dir: isDir, size: isDir ? 0 : file.content?.length ?? 96, ...(nested && isDir ? { children: this.directory(full, true) } : {}) }); } return [...entries.values()].sort((a, b) => Number(b.is_dir) - Number(a.is_dir) || String(a.name).localeCompare(String(b.name))); }

  private fileOp(body: Record<string, unknown>): void { const op = String(body.op); if (op === "mkdir") return; const from = String(body.from ?? body.path ?? ""); const to = String(body.to ?? ""); if (op === "delete") { delete this.state.files[from]; return; } const file = this.state.files[from]; if (!file) return; if (op === "move") { this.state.files[to] = file; delete this.state.files[from]; } if (op === "duplicate") this.state.files[to] = { ...file, hash: `${file.hash}-copy` }; }

  private gitOp(body: Record<string, unknown>): void { const op = String(body.op); const path = String(body.path ?? ""); const entry = this.state.git_entries.find((row) => row.path === path); if (op === "stage" && entry) { entry.index = "M"; entry.worktree = " "; entry.kind = "staged"; } if (op === "unstage" && entry) { entry.index = " "; entry.worktree = "M"; entry.kind = "modified"; } if (op === "discard") this.state.git_entries = this.state.git_entries.filter((row) => row.path !== path); if (op === "commit") this.state.git_entries = this.state.git_entries.filter((row) => row.index === " " || row.index === "?"); }

  private historySessions(): Record<string, unknown>[] { return [{ id: "history-build", name: "Successful PWA build", created_at: "2026-08-23T16:00:00Z", ended_at: "2026-08-23T16:04:00Z", exit_code: 0, cwd: "/Working/orbit/web", command: "pnpm build", scrollback_bytes: 131072 }, { id: "history-tests", name: "Browser matrix", created_at: "2026-08-23T17:00:00Z", ended_at: "2026-08-23T17:09:00Z", exit_code: 1, cwd: "/Working/orbit", command: "pnpm test:browser", scrollback_bytes: 262144 }, { id: "history-review", name: "Agent review complete", created_at: "2026-08-22T11:00:00Z", ended_at: "2026-08-22T11:12:00Z", exit_code: 0, cwd: "/Working/lighthouse", command: "demo-agent review", scrollback_bytes: 98304 }]; }

  private taskRequest(id: string, action: string | undefined, gateId: string | undefined, method: string, body: Record<string, unknown>): Response { const task = this.state.tasks.find((row) => row.id === id); if (!task) return json({ error: "task not found" }, { status: 404 }); if (method === "DELETE") { this.state.tasks = this.state.tasks.filter((row) => row.id !== id); this.write(); return json({ ok: true }); } if (method === "PATCH") Object.assign(task, body, { updated_at: this.now() }); if (action === "pause") task.status = "paused"; if (action === "resume") task.status = "active"; if (action === "run") { const run = { id: `run-${this.state.next_id++}`, task_id: id, started_at: this.now(), trigger: "manual", session_id: "demo-agent", session_name: "Agent review", prompt_file: `tasks/${id}/prompt.md`, context_file: `tasks/${id}/context.md`, status: "running", completed_at: null, exit_code: null, summary: "Started in browser-only demo state", findings: [] }; (task.runs as Record<string, unknown>[]).unshift(run); task.run_count = Number(task.run_count ?? 0) + 1; this.write(); return json(run); } if (action === "steer") { this.write(); return json({ ok: true }); } if (action === "gates" && gateId) { const runs = task.runs as Record<string, unknown>[]; const gate = (runs.flatMap((run) => run.gates as Record<string, unknown>[] ?? []).find((row) => row.id === gateId)); if (gate) Object.assign(gate, { state: "answered", option_index: body.option, option_label: Number(body.option) === 0 ? "Accept" : "Reject", approved: Number(body.option) === 0, actor: "demo:visitor", resolved_at: this.now() }); this.write(); return json(gate ?? { id: gateId, state: "answered" }); } this.write(); return json(task); }
}
