export const DEMO_NOW = "2026-08-24T15:00:00.000Z";

const work = [
  { id: "work-101", ref: "WI-101", kind: "feature", title: "Make every surface useful on first visit", body: "## Outcome\n\nPopulate the demo with trustworthy, representative data.\n\n- Keep the production components\n- Explain observed evidence\n- Exercise writes safely", state: "open", priority: "p1", effort: "medium", project_slug: "orbit", initiative_id: "init-demo", origin: "declared", trust_state: "verified", assignee_identity_ref: "demo:ana", labels: ["demo", "frontend"], relations: [{ kind: "blocks", related_id: "work-103" }], created_at: "2026-08-11T09:00:00Z", updated_at: "2026-08-24T14:20:00Z" },
  { id: "work-102", ref: "WI-102", kind: "bug", title: "Terminal reconnect loses the progress marker", body: "The reconnect path should preserve the last output position.", state: "in_progress", priority: "p0", effort: "small", project_slug: "orbit", initiative_id: "init-reliability", origin: "declared", trust_state: "disputed", assignee_identity_ref: "demo:lin", labels: ["terminal", "bug"], relations: [], created_at: "2026-08-19T10:00:00Z", updated_at: "2026-08-24T14:42:00Z" },
  { id: "work-103", ref: "WI-103", kind: "chore", title: "Document the demo deployment boundary", body: "No public request may reach a PTY or shared state.", state: "review", priority: "p2", effort: "small", project_slug: "lighthouse", initiative_id: "init-demo", origin: "declared", trust_state: "verified", assignee_identity_ref: "demo:maya", labels: ["docs", "security"], relations: [], created_at: "2026-08-15T12:00:00Z", updated_at: "2026-08-24T13:10:00Z" },
  { id: "work-104", ref: "WI-104", kind: "question", title: "Which mobile split preset is easiest to understand?", body: "Compare a row split with the nested agent-review layout at phone width.", state: "open", priority: "p3", effort: null, project_slug: "lighthouse", initiative_id: null, origin: "declared", trust_state: "unverified", assignee_identity_ref: null, labels: ["mobile"], relations: [], created_at: "2026-08-22T08:00:00Z", updated_at: "2026-08-24T12:00:00Z" },
  { id: "work-105", ref: "WI-105", kind: "feature", title: "Add deterministic Assistant approvals", body: "The pending action must show its exact payload and audited reason.", state: "done", priority: "p2", effort: "medium", project_slug: "orbit", initiative_id: "init-reliability", origin: "declared", trust_state: "verified", assignee_identity_ref: "demo:ana", labels: ["assistant"], relations: [], created_at: "2026-08-05T08:00:00Z", updated_at: "2026-08-23T17:00:00Z" },
];

export interface DemoState {
  schema: 1;
  revision: number;
  next_id: number;
  work: typeof work;
  comments: Record<string, { id: string; body: string; created_at: string }[]>;
  sessions: Record<string, Record<string, unknown>>;
  inbox: Record<string, unknown>[];
  audit: Record<string, unknown>[];
  events: Record<string, unknown>[];
  drift: Record<string, unknown>[];
  files: Record<string, { content: string | null; binary?: boolean; mtime: number; hash: string }>;
  git_entries: Record<string, unknown>[];
  tasks: Record<string, unknown>[];
  assistant: {
    transcript: {
      role: string;
      text: string;
      tool_trace?: string[];
      created_at?: string;
      session_refs?: { id: string; name: string; activity: string }[];
      actions?: { kind: "open-session"; session_id: string; label: string }[];
    }[];
    pending_action: Record<string, unknown> | null;
  };
}

export function createDemoState(): DemoState {
  const sessions = [
    ["demo-agent", "Agent review", "waiting-for-input", null, "/Working/orbit"],
    ["demo-build", "Build PWA", "running", null, "/Working/orbit/web"],
    ["demo-tests", "Test suite", "running", null, "/Working/orbit"],
    ["demo-server", "Preview server", "idle", null, "/Working/orbit/web"],
    ["demo-logs", "Live logs", "running", null, "/Working/orbit"],
    ["demo-metrics", "Metrics watch", "running", null, "/Working/lighthouse"],
    ["demo-shell", "Scratch shell", "idle", null, "/Working/orbit"],
    ["demo-finished", "Release check", "idle", 0, "/Working/lighthouse"],
  ].map(([id, name, activity, exit_code, cwd], index) => [id, {
    id, name, activity, exit_code, cwd, scrollback_bytes: 131072,
    created_at: `2026-08-24T${String(12 + Math.floor(index / 2)).padStart(2, "0")}:${index % 2 ? "30" : "00"}:00Z`,
    activity_changed_at: `2026-08-24T14:${String(10 + index).padStart(2, "0")}:00Z`,
  }]);
  return {
    schema: 1,
    revision: 7,
    next_id: 200,
    work: structuredClone(work),
    comments: {
      "WI-101": [
        { id: "comment-1", body: "The route sweep now includes phone navigation and every disclosure.", created_at: "2026-08-24T13:00:00Z" },
        { id: "comment-2", body: "Keep the fixture names fictional and the terminal entirely in-browser.", created_at: "2026-08-24T14:00:00Z" },
      ],
    },
    sessions: Object.fromEntries(sessions),
    inbox: [
      { entry_key: "drift:demo-version", source: "drift", kind: "version_mismatch", occurred_at: "2026-08-24T14:40:00Z", observed_at: "2026-08-24T14:41:00Z", title: "Declared and observed versions differ", summary: "Orbit declares 2.4.0 while its checkout reports 2.5.0.", project_slug: "orbit", work_item_ref: "WI-101", source_subject_key: "project:orbit", trust_state: "disputed", freshness: "current", triage_state: "active", evidence_snapshot: { collector: "git-local", observed_version: "2.5.0", observed_at: "2026-08-24T14:41:00Z" }, proposed_change: { from: "2.4.0", to: "2.5.0" }, action: { kind: "drift", drift_id: "demo-version" } },
      { entry_key: "github:pr-42", source: "github", kind: "pull_request_review", occurred_at: "2026-08-24T14:15:00Z", observed_at: "2026-08-24T14:17:00Z", title: "Review requested on split-layout showcase", summary: "Two comments remain on the responsive terminal layout.", project_slug: "orbit", work_item_ref: "WI-102", source_subject_key: "repo:orbit:pr:42", source_url: "https://example.invalid/orbit/pull/42", trust_state: "verified", freshness: "current", triage_state: "active" },
      { entry_key: "ci:orbit-main", source: "ci", kind: "checks_failed", occurred_at: "2026-08-24T13:52:00Z", observed_at: "2026-08-24T13:53:00Z", title: "Responsive browser check needs attention", summary: "The 768px boundary screenshot changed intentionally and awaits review.", project_slug: "orbit", work_item_ref: "WI-104", source_subject_key: "checks:orbit:demo-72", trust_state: "verified", freshness: "current", triage_state: "active" },
      { entry_key: "agent:demo-agent", source: "agent", kind: "waiting_for_input", occurred_at: "2026-08-24T14:48:00Z", observed_at: "2026-08-24T14:48:00Z", title: "Agent review is waiting for approval", summary: "The scripted agent wants permission to update a demo-only snapshot.", project_slug: "orbit", work_item_ref: "WI-101", session_id: "demo-agent", source_subject_key: "session:demo-agent", trust_state: "provisional", freshness: "live", triage_state: "active" },
      { entry_key: "github:archived", source: "github", kind: "issue_closed", occurred_at: "2026-08-23T11:00:00Z", title: "Keyboard coverage accepted", summary: "The shortcut help review was completed.", project_slug: "lighthouse", source_subject_key: "repo:lighthouse:issue:18", triage_state: "archived" },
      { entry_key: "ci:snoozed", source: "ci", kind: "checks_slow", occurred_at: "2026-08-24T10:00:00Z", title: "Bundle analysis is slower than baseline", summary: "Recheck after the shared runner cools down.", project_slug: "orbit", source_subject_key: "checks:orbit:bundle", triage_state: "snoozed", snooze_until: "2026-08-25T09:00:00Z" },
    ],
    audit: [
      { id: "audit-7", txn_id: "txn-7", revision: 7, actor_id: "actor-ana", actor_identity_ref: "demo:ana", operation: "work.transition", entity_kind: "work_item", entity_id: "work-103", reason: "ready for a human documentation review", payload_digest: "sha256:713b", at: "2026-08-24T14:10:00Z" },
      { id: "audit-6", txn_id: "txn-6", revision: 6, actor_id: "actor-lin", actor_identity_ref: "demo:lin", operation: "work.comment", entity_kind: "work_item", entity_id: "work-101", reason: "record the completed browser coverage", payload_digest: "sha256:62ea", at: "2026-08-24T13:00:00Z" },
      { id: "audit-5", txn_id: "txn-5", revision: 5, actor_id: "actor-maya", actor_identity_ref: "demo:maya", operation: "drift.resolve", entity_kind: "drift_proposal", entity_id: "drift-old", reason: "the generated manifest is authoritative", payload_digest: "sha256:5aa1", at: "2026-08-23T17:30:00Z" },
    ],
    events: [
      { seq: 7, kind: "work.transitioned", entity_kind: "work_item", entity_id: "work-103", actor_id: "actor-ana", audit_id: "audit-7", summary: { ref: "WI-103", from: "in_progress", to: "review" }, at: "2026-08-24T14:10:00Z" },
      { seq: 6, kind: "work.commented", entity_kind: "work_item", entity_id: "work-101", actor_id: "actor-lin", audit_id: "audit-6", summary: { ref: "WI-101" }, at: "2026-08-24T13:00:00Z" },
    ],
    drift: [
      { id: "demo-version", kind: "version_mismatch", subject_kind: "project", subject_id: "project-orbit", project_id: "project-orbit", project_slug: "orbit", summary: "orbit declares 2.4.0; the checkout reports 2.5.0", evidence_observation_id: "obs-version", evidence_snapshot: { subject_key: "project:orbit", content_digest: "sha256:version", observed_at: "2026-08-24T14:41:00Z", collector: "git-local", payload: { version: "2.5.0", branch: "main" } }, proposed_change: { from: "2.4.0", to: "2.5.0" }, status: "open", opened_at: "2026-08-24T14:42:00Z" },
      { id: "demo-ci", kind: "ci_red_vs_healthy", subject_kind: "project", subject_id: "project-lighthouse", project_id: "project-lighthouse", project_slug: "lighthouse", summary: "lighthouse is healthy but one browser check is red", evidence_observation_id: "obs-ci", evidence_snapshot: { subject_key: "ci:lighthouse", content_digest: "sha256:ci", observed_at: "2026-08-24T13:53:00Z", collector: "forge", payload: { status: "red" } }, proposed_change: { failing: ["browser / phone"], revision: "4f90c2d" }, status: "open", opened_at: "2026-08-24T13:54:00Z" },
    ],
    files: {
      "README.md": { content: "# Orbit\n\nA fictional product used to demonstrate Vogt.\n\n## Demo guarantees\n\n- No command reaches a shell.\n- Changes live in this browser tab.\n- Reset restores the canonical scenario.\n", mtime: 1787581200000, hash: "demo-readme-v1" },
      "package.json": { content: "{\n  \"name\": \"orbit-demo\",\n  \"version\": \"2.5.0\",\n  \"scripts\": { \"test\": \"vitest run\" }\n}\n", mtime: 1787581210000, hash: "demo-package-v1" },
      "src/main.ts": { content: "import { start } from './runtime';\n\nstart({ mode: 'demo', safe: true });\n", mtime: 1787581220000, hash: "demo-main-v1" },
      "src/runtime.ts": { content: "export function start(options: { mode: string; safe: boolean }) {\n  return options.safe;\n}\n", mtime: 1787581230000, hash: "demo-runtime-v1" },
      "src/styles.css": { content: ":root { color-scheme: dark light; }\n.demo { display: grid; gap: 1rem; }\n", mtime: 1787581240000, hash: "demo-css-v1" },
      "docs/architecture.md": { content: "# Architecture\n\nThe browser owns a deterministic store and a simulated terminal protocol. The deployed engine remains locked behind an undisclosed random token.\n\n" + "Representative long-form content demonstrates editor wrapping and scrolling.\n\n".repeat(24), mtime: 1787581250000, hash: "demo-doc-v1" },
      "assets/preview.png": { content: null, binary: true, mtime: 1787581260000, hash: "demo-binary-v1" },
    },
    git_entries: [
      { path: "README.md", index: " ", worktree: "M", kind: "modified" },
      { path: "src/runtime.ts", index: "M", worktree: " ", kind: "staged" },
      { path: "docs/demo-notes.md", index: "?", worktree: "?", kind: "untracked" },
    ],
    tasks: demoTasks(),
    assistant: {
      transcript: [
        {
          role: "user",
          text: "What needs me?",
          created_at: "2026-08-24T14:49:00Z",
        },
        {
          role: "assistant",
          text: "The **Agent review** session is waiting at the mobile composition approval. The terminal follows the real snapshot protocol, but its input is canned and never reaches a process.",
          tool_trace: ["listed sessions", "read Agent review tail"],
          created_at: "2026-08-24T14:49:25Z",
          session_refs: [
            { id: "demo-agent", name: "Agent review", activity: "waiting-for-input" },
          ],
          actions: [
            { kind: "open-session", session_id: "demo-agent", label: "Open Agent review" },
          ],
        },
      ],
      pending_action: { kind: "send_input", id: "action-demo-input", session_id: "demo-agent", session_name: "Agent review", text: "approve demo snapshot", submit: true },
    },
  };
}

function demoTasks(): Record<string, unknown>[] {
  const completed = { id: "run-complete", task_id: "task-a11y", started_at: "2026-08-24T12:00:00Z", trigger: "scheduled", session_id: "demo-finished", session_name: "Accessibility sweep", prompt_file: "tasks/task-a11y/prompt.md", context_file: "tasks/task-a11y/context.md", status: "completed", completed_at: "2026-08-24T12:04:00Z", exit_code: 0, summary: "All named controls passed.", findings: [{ at: "2026-08-24T12:03:00Z", text: "All dialog focus-return checks passed", source: "report.json" }], outcome: "succeeded", conclusion: { started: "2026-08-24T12:00:00Z", finished: "2026-08-24T12:04:00Z", duration_ms: 240000, outcome: "succeeded", exit_code: 0, retries: 0, branch: "demo/a11y", final_sha: "aa11bb22", base_sha: "00112233", diffstat: { files: 2, insertions: 18, deletions: 3 }, cost: { total_usd: 0.12, input_tokens: 4200, output_tokens: 800 }, findings: [] } };
  const running = { id: "run-live", task_id: "task-review", started_at: "2026-08-24T14:30:00Z", trigger: "event", trigger_detail: { trigger_kind: "forge-pr-checks", event_kind: "checks.red", event_id: "demo-72", description: "phone browser check changed" }, session_id: "demo-agent", session_name: "Agent review", prompt_file: "tasks/task-review/prompt.md", context_file: "tasks/task-review/context.md", status: "running", completed_at: null, exit_code: null, summary: "Waiting at screenshot approval gate", findings: [{ at: "2026-08-24T14:45:00Z", text: "The change is limited to the demo disclosure", source: "agent" }], gates: [{ id: "gate-screenshot", question: "Accept the updated phone composition?", options: [{ label: "Accept", approve: true }, { label: "Reject", approve: false }], state: "open", opened_at: "2026-08-24T14:46:00Z", resolved_at: null }] };
  return [
    { id: "task-review", name: "Review changed browser snapshots", prompt: "Inspect changed screenshots and explain the visual delta.", schedule: { kind: "manual" }, triggers: [{ kind: "forge-pr-checks", enabled: true, status: "red", work_item: "WI-104" }], concurrency: 1, status: "active", command: ["demo-agent"], cwd: "/Working/orbit", env: [], context: "Demo-only review", vogt_project: "orbit", vogt_work_item: "WI-104", gates: [{ id: "screenshot", question: "Accept the composition?", options: [{ label: "Accept", approve: true }, { label: "Reject", approve: false }] }], auto_approve: false, notify_on_start: true, notify_on_phrase: "waiting", auto_retry_on_rate_limit: false, next_run: null, last_run: "2026-08-24T14:30:00Z", run_count: 3, runs: [running], created_at: "2026-08-20T09:00:00Z", updated_at: "2026-08-24T14:30:00Z" },
    { id: "task-a11y", name: "Nightly accessibility sweep", prompt: "Visit every route at desktop and phone widths.", schedule: { kind: "daily", times: ["02:00"] }, triggers: [], concurrency: 1, status: "active", command: ["demo-a11y"], cwd: "/Working/lighthouse", env: [], context: null, vogt_project: "lighthouse", vogt_work_item: "WI-103", gates: [], auto_approve: false, notify_on_start: false, notify_on_phrase: null, auto_retry_on_rate_limit: true, next_run: "2026-08-25T02:00:00Z", last_run: "2026-08-24T12:00:00Z", run_count: 12, runs: [completed], created_at: "2026-08-12T09:00:00Z", updated_at: "2026-08-24T12:04:00Z" },
    { id: "task-deps", name: "Dependency evidence refresh", prompt: "Report unresolved project references.", schedule: { kind: "interval", minutes: 120 }, triggers: [{ kind: "observation-new", enabled: true, observation_kind: "dependency.ref", project: "orbit" }], concurrency: 2, status: "paused", command: null, cwd: "/Working/orbit", env: [], context: null, vogt_project: "orbit", vogt_work_item: null, gates: [], auto_approve: false, notify_on_start: false, notify_on_phrase: null, auto_retry_on_rate_limit: true, next_run: null, last_run: "2026-08-23T18:00:00Z", run_count: 5, runs: [{ ...completed, id: "run-error", task_id: "task-deps", status: "errored", exit_code: 1, outcome: "failed", summary: "One fixture did not match its schema" }], created_at: "2026-08-14T09:00:00Z", updated_at: "2026-08-23T18:02:00Z" },
  ];
}
