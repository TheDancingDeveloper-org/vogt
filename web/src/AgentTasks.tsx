import { For, Show, createEffect, createMemo, createSignal, onMount } from "solid-js";
import type {
  AgentTask,
  AgentTaskRun,
  AgentTaskSchedule,
  AgentTaskUpsertRequest,
} from "./api";
import { api } from "./api";

interface Props {
  onError?: (message: string) => void;
  onOpenSession?: (sessionId: string, label: string) => void;
}

interface TaskDraft {
  name: string;
  prompt: string;
  scheduleKind: AgentTaskSchedule["kind"];
  intervalMinutes: string;
  dailyTimes: string;
  commandText: string;
  cwd: string;
  envText: string;
  context: string;
  enabled: boolean;
  notifyOnStart: boolean;
  notifyOnPhrase: string;
  autoRetryOnRateLimit: boolean;
}

const EMPTY_DRAFT: TaskDraft = {
  name: "",
  prompt: "",
  scheduleKind: "manual",
  intervalMinutes: "720",
  dailyTimes: "09:00",
  commandText: "",
  cwd: "",
  envText: "",
  context: "",
  enabled: true,
  notifyOnStart: false,
  notifyOnPhrase: "MYDEVENV2_NOTIFY:",
  autoRetryOnRateLimit: true,
};

function taskToDraft(task: AgentTask): TaskDraft {
  return {
    name: task.name,
    prompt: task.prompt,
    scheduleKind: task.schedule.kind,
    intervalMinutes:
      task.schedule.kind === "interval" ? String(task.schedule.minutes) : "720",
    dailyTimes:
      task.schedule.kind === "daily" ? task.schedule.times.join(", ") : "09:00",
    commandText: (task.command ?? []).join("\n"),
    cwd: task.cwd ?? "",
    envText: task.env.map(([key, value]) => `${key}=${value}`).join("\n"),
    context: task.context ?? "",
    enabled: task.status === "active",
    notifyOnStart: task.notify_on_start,
    notifyOnPhrase: task.notify_on_phrase ?? "",
    autoRetryOnRateLimit: task.auto_retry_on_rate_limit,
  };
}

function formatLocalDate(value: string | null): string {
  if (!value) return "Never";
  try {
    return new Date(value).toLocaleString();
  } catch {
    return value;
  }
}

function runStatusLabel(run: AgentTaskRun): string {
  if (run.summary?.trim()) return run.summary;
  switch (run.status) {
    case "running":
      return "Still running";
    case "completed":
      return "Exited successfully";
    case "errored":
      return run.exit_code === null
        ? "Exited with error"
        : `Exited with status ${run.exit_code}`;
  }
}

function scheduleSummary(task: AgentTask): string {
  switch (task.schedule.kind) {
    case "manual":
      return "Manual";
    case "interval":
      return `Every ${task.schedule.minutes} min`;
    case "daily":
      return `Daily ${task.schedule.times.join(", ")}`;
  }
}

function parseEnv(text: string): [string, string][] {
  return text
    .split("\n")
    .map((line) => line.trim())
    .filter(Boolean)
    .map((line) => {
      const idx = line.indexOf("=");
      if (idx <= 0) {
        throw new Error(`Invalid env line: ${line}`);
      }
      return [line.slice(0, idx).trim(), line.slice(idx + 1)] as [string, string];
    });
}

function buildRequest(draft: TaskDraft): AgentTaskUpsertRequest {
  const name = draft.name.trim();
  const prompt = draft.prompt.trim();
  if (!name) throw new Error("Name is required");
  if (!prompt) throw new Error("Prompt is required");

  let schedule: AgentTaskSchedule;
  if (draft.scheduleKind === "manual") {
    schedule = { kind: "manual" };
  } else if (draft.scheduleKind === "interval") {
    const minutes = Number.parseInt(draft.intervalMinutes.trim(), 10);
    if (!Number.isFinite(minutes) || minutes < 1) {
      throw new Error("Interval minutes must be a positive integer");
    }
    schedule = { kind: "interval", minutes };
  } else {
    const times = draft.dailyTimes
      .split(/[,\n]/)
      .map((value) => value.trim())
      .filter(Boolean);
    if (times.length === 0) {
      throw new Error("Add at least one daily time");
    }
    schedule = { kind: "daily", times };
  }

  const command = draft.commandText
    .split("\n")
    .map((line) => line.trim())
    .filter(Boolean);

  return {
    name,
    prompt,
    schedule,
    command: command.length > 0 ? command : null,
    cwd: draft.cwd.trim() || null,
    env: parseEnv(draft.envText),
    context: draft.context.trim() || null,
    enabled: draft.enabled,
    notify_on_start: draft.notifyOnStart,
    notify_on_phrase: draft.notifyOnPhrase.trim() || null,
    auto_retry_on_rate_limit: draft.autoRetryOnRateLimit,
  };
}

const AgentTasks = (props: Props) => {
  const [tasks, setTasks] = createSignal<AgentTask[]>([]);
  const [loading, setLoading] = createSignal(true);
  const [saving, setSaving] = createSignal(false);
  const [runningTaskId, setRunningTaskId] = createSignal<string | null>(null);
  const [selectedTaskId, setSelectedTaskId] = createSignal<string | null>(null);
  const [draft, setDraft] = createSignal<TaskDraft>({ ...EMPTY_DRAFT });

  const sortedTasks = createMemo(() =>
    [...tasks()].sort((a, b) => {
      const aActive = a.status === "active" ? 0 : 1;
      const bActive = b.status === "active" ? 0 : 1;
      if (aActive !== bActive) return aActive - bActive;
      const aNext = a.next_run ? Date.parse(a.next_run) : Number.POSITIVE_INFINITY;
      const bNext = b.next_run ? Date.parse(b.next_run) : Number.POSITIVE_INFINITY;
      if (aNext !== bNext) return aNext - bNext;
      return Date.parse(b.updated_at) - Date.parse(a.updated_at);
    }),
  );

  const selectedTask = createMemo(() =>
    tasks().find((task) => task.id === selectedTaskId()) ?? null,
  );

  const activeCount = createMemo(
    () => tasks().filter((task) => task.status === "active").length,
  );

  const pausedCount = createMemo(
    () => tasks().filter((task) => task.status === "paused").length,
  );

  const loadTasks = async (preferredTaskId?: string | null) => {
    setLoading(true);
    try {
      const list = await api.listAgentTasks();
      setTasks(list);
      const nextSelected =
        preferredTaskId && list.some((task) => task.id === preferredTaskId)
          ? preferredTaskId
          : selectedTaskId() && list.some((task) => task.id === selectedTaskId())
            ? selectedTaskId()
            : list[0]?.id ?? null;
      setSelectedTaskId(nextSelected);
      if (!nextSelected) {
        setDraft({ ...EMPTY_DRAFT });
      }
    } catch (e) {
      props.onError?.(`Failed to load tasks: ${(e as Error).message}`);
    } finally {
      setLoading(false);
    }
  };

  onMount(() => {
    void loadTasks();
  });

  createEffect(() => {
    const task = selectedTask();
    if (task) {
      setDraft(taskToDraft(task));
    }
  });

  const startCreate = () => {
    setSelectedTaskId(null);
    setDraft({ ...EMPTY_DRAFT });
  };

  const saveTask = async () => {
    setSaving(true);
    try {
      const request = buildRequest(draft());
      if (selectedTaskId()) {
        const saved = await api.updateAgentTask(selectedTaskId()!, request);
        await loadTasks(saved.id);
      } else {
        const created = await api.createAgentTask(request);
        await loadTasks(created.id);
      }
    } catch (e) {
      props.onError?.(`Failed to save task: ${(e as Error).message}`);
    } finally {
      setSaving(false);
    }
  };

  const toggleTask = async (task: AgentTask) => {
    try {
      if (task.status === "active") {
        await api.pauseAgentTask(task.id);
      } else {
        await api.resumeAgentTask(task.id);
      }
      await loadTasks(task.id);
    } catch (e) {
      props.onError?.(`Failed to update task: ${(e as Error).message}`);
    }
  };

  const deleteTask = async (task: AgentTask) => {
    if (!window.confirm(`Delete task "${task.name}"?`)) return;
    try {
      await api.deleteAgentTask(task.id);
      const remaining = tasks().filter((candidate) => candidate.id !== task.id);
      setTasks(remaining);
      const next = remaining[0]?.id ?? null;
      setSelectedTaskId(next);
      if (!next) setDraft({ ...EMPTY_DRAFT });
    } catch (e) {
      props.onError?.(`Failed to delete task: ${(e as Error).message}`);
    }
  };

  const runTaskNow = async (task: AgentTask) => {
    setRunningTaskId(task.id);
    try {
      const run = await api.runAgentTask(task.id);
      await loadTasks(task.id);
      props.onOpenSession?.(run.session_id, run.session_name);
    } catch (e) {
      props.onError?.(`Failed to run task: ${(e as Error).message}`);
    } finally {
      setRunningTaskId(null);
    }
  };

  const openRunSession = (run: AgentTaskRun) => {
    props.onOpenSession?.(run.session_id, run.session_name);
  };

  return (
    <div class="agent-tasks-view">
      <div class="agent-tasks-header">
        <div>
          <h2>Agent Tasks</h2>
          <div class="agent-tasks-summary">
            <span>{tasks().length} total</span>
            <span>{activeCount()} active</span>
            <span>{pausedCount()} paused</span>
          </div>
        </div>
        <div class="agent-tasks-header-actions">
          <button onClick={() => void loadTasks(selectedTaskId())}>Refresh</button>
          <button onClick={startCreate}>New Task</button>
        </div>
      </div>

      <div class="agent-tasks-layout">
        <section class="agent-tasks-list">
          <Show
            when={!loading()}
            fallback={<div class="agent-tasks-empty">Loading tasks...</div>}
          >
            <Show
              when={sortedTasks().length > 0}
              fallback={
                <div class="agent-tasks-empty">
                  No recurring tasks yet. Create one to launch scheduled agent work.
                </div>
              }
            >
              <For each={sortedTasks()}>
                {(task) => (
                  <button
                    class={`agent-task-row ${
                      selectedTaskId() === task.id ? "active" : ""
                    }`}
                    onClick={() => setSelectedTaskId(task.id)}
                  >
                    <div class="agent-task-row-main">
                      <div class="agent-task-row-top">
                        <span class="agent-task-row-name">{task.name}</span>
                        <span
                          class={`agent-task-row-status ${
                            task.status === "active" ? "active" : "paused"
                          }`}
                        >
                          {task.status}
                        </span>
                      </div>
                      <div class="agent-task-row-meta">
                        <span>{scheduleSummary(task)}</span>
                        <span>{task.run_count} runs</span>
                        <span>Next: {formatLocalDate(task.next_run)}</span>
                      </div>
                    </div>
                  </button>
                )}
              </For>
            </Show>
          </Show>
        </section>

        <section class="agent-tasks-detail">
          <div class="agent-task-toolbar">
            <Show
              when={selectedTask()}
              fallback={<strong>Create Task</strong>}
            >
              {(task) => (
                <>
                  <strong>{task().name}</strong>
                  <div class="agent-task-toolbar-actions">
                    <button
                      onClick={() => void runTaskNow(task())}
                      disabled={runningTaskId() === task().id}
                    >
                      {runningTaskId() === task().id ? "Starting..." : "Run Now"}
                    </button>
                    <button onClick={() => void toggleTask(task())}>
                      {task().status === "active" ? "Pause" : "Resume"}
                    </button>
                    <button onClick={() => void deleteTask(task())}>Delete</button>
                  </div>
                </>
              )}
            </Show>
          </div>

          <div class="agent-task-form-grid">
            <label class="agent-task-field">
              <span>Name</span>
              <input
                type="text"
                value={draft().name}
                onInput={(e) =>
                  setDraft({ ...draft(), name: e.currentTarget.value })
                }
              />
            </label>

            <label class="agent-task-field">
              <span>Working Directory</span>
              <input
                type="text"
                placeholder="Optional workspace-relative or absolute path"
                value={draft().cwd}
                onInput={(e) =>
                  setDraft({ ...draft(), cwd: e.currentTarget.value })
                }
              />
            </label>

            <div class="agent-task-field">
              <span>Schedule</span>
              <div class="agent-task-segmented">
                <button
                  class={draft().scheduleKind === "manual" ? "active" : ""}
                  onClick={() => setDraft({ ...draft(), scheduleKind: "manual" })}
                >
                  Manual
                </button>
                <button
                  class={draft().scheduleKind === "interval" ? "active" : ""}
                  onClick={() => setDraft({ ...draft(), scheduleKind: "interval" })}
                >
                  Interval
                </button>
                <button
                  class={draft().scheduleKind === "daily" ? "active" : ""}
                  onClick={() => setDraft({ ...draft(), scheduleKind: "daily" })}
                >
                  Daily
                </button>
              </div>
              <Show when={draft().scheduleKind === "interval"}>
                <input
                  type="text"
                  placeholder="Minutes"
                  value={draft().intervalMinutes}
                  onInput={(e) =>
                    setDraft({ ...draft(), intervalMinutes: e.currentTarget.value })
                  }
                />
              </Show>
              <Show when={draft().scheduleKind === "daily"}>
                <input
                  type="text"
                  placeholder="09:00, 17:30"
                  value={draft().dailyTimes}
                  onInput={(e) =>
                    setDraft({ ...draft(), dailyTimes: e.currentTarget.value })
                  }
                />
              </Show>
            </div>

            <div class="agent-task-field agent-task-checks">
              <label>
                <input
                  type="checkbox"
                  checked={draft().enabled}
                  onChange={(e) =>
                    setDraft({ ...draft(), enabled: e.currentTarget.checked })
                  }
                />
                <span>Enabled</span>
              </label>
              <label>
                <input
                  type="checkbox"
                  checked={draft().notifyOnStart}
                  onChange={(e) =>
                    setDraft({
                      ...draft(),
                      notifyOnStart: e.currentTarget.checked,
                    })
                  }
                />
                <span>Notify on start</span>
              </label>
              <label>
                <input
                  type="checkbox"
                  checked={draft().autoRetryOnRateLimit}
                  onChange={(e) =>
                    setDraft({
                      ...draft(),
                      autoRetryOnRateLimit: e.currentTarget.checked,
                    })
                  }
                />
                <span title="Write a retry keystroke back into the session after a backoff when it prints a 429/rate-limit/overloaded message">
                  Auto-retry on rate limit
                </span>
              </label>
            </div>

            <label class="agent-task-field agent-task-field-wide">
              <span>Prompt</span>
              <textarea
                rows={8}
                value={draft().prompt}
                onInput={(e) =>
                  setDraft({ ...draft(), prompt: e.currentTarget.value })
                }
              />
            </label>

            <label class="agent-task-field">
              <span>Command Args</span>
              <textarea
                rows={6}
                placeholder={"/bin/sh\n-lc\ncodex exec --prompt-file {prompt_file}"}
                value={draft().commandText}
                onInput={(e) =>
                  setDraft({ ...draft(), commandText: e.currentTarget.value })
                }
              />
            </label>

            <label class="agent-task-field">
              <span>Environment</span>
              <textarea
                rows={6}
                placeholder={"KEY=value\nANOTHER=value"}
                value={draft().envText}
                onInput={(e) =>
                  setDraft({ ...draft(), envText: e.currentTarget.value })
                }
              />
            </label>

            <label class="agent-task-field">
              <span>Persistent Context</span>
              <textarea
                rows={6}
                value={draft().context}
                onInput={(e) =>
                  setDraft({ ...draft(), context: e.currentTarget.value })
                }
              />
            </label>

            <label class="agent-task-field">
              <span>Notification Phrase</span>
              <input
                type="text"
                placeholder="MYDEVENV2_NOTIFY:"
                value={draft().notifyOnPhrase}
                onInput={(e) =>
                  setDraft({ ...draft(), notifyOnPhrase: e.currentTarget.value })
                }
              />
            </label>
          </div>

          <div class="agent-task-form-actions">
            <button onClick={saveTask} disabled={saving()}>
              {saving()
                ? "Saving..."
                : selectedTaskId()
                  ? "Save Changes"
                  : "Create Task"}
            </button>
            <Show when={selectedTaskId()}>
              <button onClick={startCreate}>New Draft</button>
            </Show>
          </div>

          <Show when={selectedTask()}>
            {(task) => (
              <div class="agent-task-runs">
                <div class="agent-task-runs-header">
                  <strong>Recent Runs</strong>
                  <span>
                    Last: {formatLocalDate(task().last_run)} • Updated:{" "}
                    {formatLocalDate(task().updated_at)}
                  </span>
                </div>
                <Show
                  when={task().runs.length > 0}
                  fallback={<div class="agent-tasks-empty">No runs recorded yet.</div>}
                >
                  <For each={[...task().runs].slice().reverse()}>
                    {(run) => (
                      <div class="agent-task-run-row">
                        <div class="agent-task-run-main">
                          <div class="agent-task-run-top">
                            <span>{formatLocalDate(run.started_at)}</span>
                            <span class="agent-task-run-trigger">{run.trigger}</span>
                          </div>
                          <div class="agent-task-run-meta">
                            <span>{run.session_name}</span>
                            <span>{runStatusLabel(run)}</span>
                            <span>
                              {run.completed_at
                                ? `Finished ${formatLocalDate(run.completed_at)}`
                                : `Prompt ${run.prompt_file}`}
                            </span>
                          </div>
                        </div>
                        <button onClick={() => openRunSession(run)}>Open Session</button>
                      </div>
                    )}
                  </For>
                </Show>
              </div>
            )}
          </Show>
        </section>
      </div>
    </div>
  );
};

export default AgentTasks;
