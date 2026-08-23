import {
  For,
  Index,
  Show,
  createEffect,
  createMemo,
  createSignal,
  onCleanup,
  onMount,
} from "solid-js";
import { useBeforeLeave } from "@solidjs/router";
import type {
  AgentTask,
  AgentTaskRun,
  AgentTaskRunCost,
  AgentTaskRunOutcome,
  AgentTaskSchedule,
  AgentTaskTrigger,
  AgentTaskUpsertRequest,
  ForgeCheckStatus,
  TriggerKind,
} from "./api";
import { api } from "./api";
import AgentTaskSteering from "./AgentTaskSteering";
import Dialog from "./Dialog";
import { onSessionKilled } from "./store";
import { focusTab, setTasksDirty } from "./tabs";
import { readToolDraft, writeToolDraft } from "./toolDrafts";

export interface AgentTaskDraftGuard {
  dirty: () => boolean;
  requestLeave: (action: () => void) => void;
}

interface Props {
  onError?: (message: string) => void;
  onOpenSession?: (sessionId: string, label: string) => void;
  confirmAction?: (title: string, body?: string) => Promise<boolean>;
  registerDraftGuard?: (guard: AgentTaskDraftGuard | null) => void;
}

interface TaskDraft {
  name: string;
  prompt: string;
  scheduleKind: AgentTaskSchedule["kind"];
  intervalMinutes: string;
  dailyTimes: string;
  triggers: AgentTaskTrigger[];
  concurrency: string;
  commandText: string;
  cwd: string;
  envText: string;
  context: string;
  vogtProject: string;
  vogtWorkItem: string;
  enabled: boolean;
  notifyOnStart: boolean;
  notifyOnPhrase: string;
  autoRetryOnRateLimit: boolean;
}

/** The trigger kinds a task can arm, and how they read in the editor (#290). */
const TRIGGER_KINDS: { kind: TriggerKind; label: string }[] = [
  { kind: "work-transition", label: "Work item transition" },
  { kind: "observation-new", label: "New observation" },
  { kind: "drift-proposed", label: "Drift proposed" },
  { kind: "forge-pr-checks", label: "PR checks green/red" },
  { kind: "api", label: "API (programmatic)" },
];

function triggerLabel(kind: TriggerKind): string {
  return TRIGGER_KINDS.find((t) => t.kind === kind)?.label ?? kind;
}

/** A freshly-added trigger of a kind, with sensible starting filters (#290). */
function defaultTrigger(kind: TriggerKind): AgentTaskTrigger {
  switch (kind) {
    case "work-transition":
      return { enabled: true, kind, to_state: "ready" };
    case "observation-new":
      return { enabled: true, kind, observation_kind: "forge.issue" };
    case "drift-proposed":
      return { enabled: true, kind };
    case "forge-pr-checks":
      return { enabled: true, kind, status: "any" };
    case "api":
      return { enabled: true, kind };
  }
}

interface TasksViewDraft {
  selectedTaskId: string | null;
  draft: TaskDraft;
  creating: boolean;
  baseline?: TaskDraft;
}

interface PendingDraftDecision {
  title: string;
  body: string;
  action: () => void;
}

const EMPTY_DRAFT: TaskDraft = {
  name: "",
  prompt: "",
  scheduleKind: "manual",
  intervalMinutes: "720",
  dailyTimes: "09:00",
  triggers: [],
  concurrency: "1",
  commandText: "",
  cwd: "",
  envText: "",
  context: "",
  vogtProject: "",
  vogtWorkItem: "",
  enabled: true,
  notifyOnStart: false,
  notifyOnPhrase: "VOGT_NOTIFY:",
  autoRetryOnRateLimit: true,
};

function cloneDraft(value: TaskDraft): TaskDraft {
  return { ...value, triggers: value.triggers.map((t) => ({ ...t })) };
}

function draftsEqual(left: TaskDraft, right: TaskDraft): boolean {
  return JSON.stringify(left) === JSON.stringify(right);
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function taskToDraft(task: AgentTask): TaskDraft {
  return {
    name: task.name,
    prompt: task.prompt,
    scheduleKind: task.schedule.kind,
    intervalMinutes:
      task.schedule.kind === "interval" ? String(task.schedule.minutes) : "720",
    dailyTimes:
      task.schedule.kind === "daily" ? task.schedule.times.join(", ") : "09:00",
    triggers: (task.triggers ?? []).map((t) => ({ ...t })),
    concurrency: String(task.concurrency ?? 1),
    commandText: (task.command ?? []).join("\n"),
    cwd: task.cwd ?? "",
    envText: task.env.map(([key, value]) => `${key}=${value}`).join("\n"),
    context: task.context ?? "",
    vogtProject: task.vogt_project ?? "",
    vogtWorkItem: task.vogt_work_item ?? "",
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

/** The short label on a run's outcome badge (#291). */
function outcomeLabel(outcome: AgentTaskRunOutcome): string {
  switch (outcome) {
    case "succeeded":
      return "Succeeded";
    case "failed":
      return "Failed";
    case "partially-succeeded":
      return "Partial";
    case "skipped":
      return "Skipped";
    case "blocked":
      return "Blocked";
    default:
      return outcome;
  }
}

/** A run's duration, from milliseconds to a compact human string. */
function formatDuration(ms: number): string {
  if (ms < 1000) return `${ms}ms`;
  const seconds = ms / 1000;
  if (seconds < 60) return `${seconds.toFixed(seconds < 10 ? 1 : 0)}s`;
  const minutes = Math.floor(seconds / 60);
  const rest = Math.round(seconds % 60);
  return `${minutes}m ${rest}s`;
}

/** A conclusion's cost as a short string, or null when the CLI reported none. */
function formatCost(cost?: AgentTaskRunCost): string | null {
  if (!cost) return null;
  if (typeof cost.total_usd === "number") return `$${cost.total_usd.toFixed(2)}`;
  const tokens = [cost.input_tokens, cost.output_tokens].filter(
    (t): t is number => typeof t === "number",
  );
  if (tokens.length > 0) {
    return `${tokens.reduce((a, b) => a + b, 0)} tok`;
  }
  return null;
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

  const concurrency = Number.parseInt(draft.concurrency.trim() || "1", 10);
  if (!Number.isFinite(concurrency) || concurrency < 1) {
    throw new Error("Concurrency must be a positive integer");
  }

  // A work-transition trigger with no destination state matches nothing; catch
  // it here rather than let the engine 400 the whole save.
  for (const trigger of draft.triggers) {
    if (trigger.kind === "work-transition" && !trigger.to_state.trim()) {
      throw new Error("A work-item-transition trigger needs a destination state");
    }
    if (trigger.kind === "observation-new" && !trigger.observation_kind.trim()) {
      throw new Error("A new-observation trigger needs an observation kind");
    }
  }

  return {
    name,
    prompt,
    schedule,
    triggers: draft.triggers,
    concurrency,
    command: command.length > 0 ? command : null,
    cwd: draft.cwd.trim() || null,
    env: parseEnv(draft.envText),
    context: draft.context.trim() || null,
    vogt_project: draft.vogtProject.trim() || null,
    vogt_work_item: draft.vogtWorkItem.trim() || null,
    enabled: draft.enabled,
    notify_on_start: draft.notifyOnStart,
    notify_on_phrase: draft.notifyOnPhrase.trim() || null,
    auto_retry_on_rate_limit: draft.autoRetryOnRateLimit,
  };
}

const AgentTasks = (props: Props) => {
  const stored = readToolDraft<TasksViewDraft>("tasks", {
    selectedTaskId: null,
    draft: { ...EMPTY_DRAFT },
    creating: false,
  });
  // A draft that outlived a reload may predate a field this build added, so
  // fold it onto the current shape rather than trusting it wholesale.
  const restored: TasksViewDraft = {
    ...stored,
    draft: { ...EMPTY_DRAFT, ...stored.draft },
    baseline: stored.baseline
      ? { ...EMPTY_DRAFT, ...stored.baseline }
      : undefined,
  };
  const [tasks, setTasks] = createSignal<AgentTask[]>([]);
  const [tasksLoaded, setTasksLoaded] = createSignal(false);
  const [loading, setLoading] = createSignal(false);
  const [tasksError, setTasksError] = createSignal<string | null>(null);
  const [tasksStale, setTasksStale] = createSignal(false);
  const [saving, setSaving] = createSignal(false);
  const [runningTaskId, setRunningTaskId] = createSignal<string | null>(null);
  const [selectedTaskId, setSelectedTaskId] = createSignal<string | null>(
    restored.selectedTaskId,
  );
  const [draft, setDraft] = createSignal<TaskDraft>(cloneDraft(restored.draft));
  const [creating, setCreating] = createSignal(restored.creating);
  const [baseline, setBaseline] = createSignal<TaskDraft>(
    cloneDraft(restored.baseline ?? (restored.creating ? EMPTY_DRAFT : restored.draft)),
  );
  const [actionError, setActionError] = createSignal<string | null>(null);
  const [retryAction, setRetryAction] = createSignal<(() => void) | null>(null);
  const [steerBusy, setSteerBusy] = createSignal(false);
  const [newTriggerKind, setNewTriggerKind] =
    createSignal<TriggerKind>("work-transition");
  const [pendingDecision, setPendingDecision] =
    createSignal<PendingDraftDecision | null>(null);
  let restoreDraftPending = restored.selectedTaskId !== null && !restored.creating;
  let listRequest = 0;

  const dirty = createMemo(() => !draftsEqual(draft(), baseline()));

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

  const setDraftForTask = (task: AgentTask): void => {
    const next = taskToDraft(task);
    setDraft(cloneDraft(next));
    setBaseline(cloneDraft(next));
  };

  const setEmptyDraft = (): void => {
    setDraft(cloneDraft(EMPTY_DRAFT));
    setBaseline(cloneDraft(EMPTY_DRAFT));
  };

  const loadTasks = async (
    preferredTaskId?: string | null,
    preserveDraft = false,
  ): Promise<boolean> => {
    const request = ++listRequest;
    setLoading(true);
    try {
      const list = await api.listAgentTasks();
      if (request !== listRequest) return false;
      setTasks(list);
      setTasksLoaded(true);
      setTasksError(null);
      setTasksStale(false);
      const preserveCurrentDraft = preserveDraft || dirty();
      const currentSelected = selectedTaskId();
      const nextSelected = creating()
        ? null
        : currentSelected && list.some((task) => task.id === currentSelected)
          ? currentSelected
          : preferredTaskId && list.some((task) => task.id === preferredTaskId)
            ? preferredTaskId
            : list[0]?.id ?? null;
      setSelectedTaskId(nextSelected);
      const nextTask = list.find((task) => task.id === nextSelected) ?? null;
      if (nextTask) {
        if (restoreDraftPending && nextTask.id === restored.selectedTaskId) {
          restoreDraftPending = false;
          setBaseline(taskToDraft(nextTask));
        } else if (!preserveCurrentDraft) {
          setDraftForTask(nextTask);
        }
      } else if (!creating() && !preserveCurrentDraft) {
        setEmptyDraft();
      }
      return true;
    } catch (error) {
      if (request !== listRequest) return false;
      setTasksError(`Failed to load tasks: ${errorMessage(error)}`);
      setTasksStale(tasksLoaded());
      return false;
    } finally {
      if (request === listRequest) setLoading(false);
    }
  };

  const persistDraft = (): void => {
    writeToolDraft<TasksViewDraft>("tasks", {
      selectedTaskId: selectedTaskId(),
      draft: cloneDraft(draft()),
      creating: creating(),
      baseline: cloneDraft(baseline()),
    });
  };

  onMount(() => {
    props.registerDraftGuard?.({ dirty, requestLeave: requestDraftDecision });
    void loadTasks();
    // A run's PTY ending is what turns "Still running" into a real outcome.
    // The engine records the finding and status on the run, but only a reload
    // of the list surfaces it — so re-read when a session we launched exits.
    const unsubscribe = onSessionKilled((sessionId) => {
      const owns = tasks().some((task) =>
        task.runs.some((run) => run.session_id === sessionId),
      );
      if (owns) void loadTasks(selectedTaskId(), true);
    });
    onCleanup(unsubscribe);
  });

  createEffect(() => setTasksDirty(dirty()));

  // Mirror the live draft to storage on every change, not only on unmount: a
  // full page reload never runs onCleanup, and that is exactly the case
  // sessionStorage exists to survive.
  createEffect(persistDraft);

  onCleanup(() => {
    listRequest += 1;
    props.registerDraftGuard?.(null);
    setTasksDirty(false);
    persistDraft();
  });

  const startCreate = (): void => {
    setCreating(true);
    setSelectedTaskId(null);
    setEmptyDraft();
    setActionError(null);
    setRetryAction(null);
  };

  // --- trigger editing (#290) ---------------------------------------------
  const addTrigger = (kind: TriggerKind): void => {
    setDraft({ ...draft(), triggers: [...draft().triggers, defaultTrigger(kind)] });
  };

  const removeTrigger = (index: number): void => {
    setDraft({
      ...draft(),
      triggers: draft().triggers.filter((_, i) => i !== index),
    });
  };

  const patchTrigger = (
    index: number,
    patch: Partial<AgentTaskTrigger>,
  ): void => {
    setDraft({
      ...draft(),
      triggers: draft().triggers.map((trigger, i) =>
        i === index ? ({ ...trigger, ...patch } as AgentTaskTrigger) : trigger,
      ),
    });
  };

  const reportActionFailure = (
    label: string,
    error: unknown,
    retry: () => void,
  ): void => {
    const message = `${label}: ${errorMessage(error)}`;
    setActionError(message);
    setRetryAction(() => retry);
    props.onError?.(message);
  };

  const saveTask = async (): Promise<boolean> => {
    if (saving()) return false;
    const submittedDraft = cloneDraft(draft());
    setSaving(true);
    setActionError(null);
    setRetryAction(null);
    try {
      const request = buildRequest(submittedDraft);
      let saved: AgentTask;
      if (selectedTaskId()) {
        saved = await api.updateAgentTask(selectedTaskId()!, request);
      } else {
        saved = await api.createAgentTask(request);
      }
      setCreating(false);
      setSelectedTaskId(saved.id);
      const savedDraft = taskToDraft(saved);
      const submittedDraftStillCurrent = draftsEqual(draft(), submittedDraft);
      if (submittedDraftStillCurrent) {
        setDraft(cloneDraft(savedDraft));
      }
      setBaseline(cloneDraft(savedDraft));
      setTasks((current) => {
        const index = current.findIndex((task) => task.id === saved.id);
        if (index < 0) return [...current, saved];
        const next = [...current];
        next[index] = saved;
        return next;
      });
      void loadTasks(saved.id, true);
      return submittedDraftStillCurrent;
    } catch (error) {
      reportActionFailure("Failed to save task", error, () => void saveTask());
      return false;
    } finally {
      setSaving(false);
    }
  };

  const discardDraft = (): void => {
    setDraft(cloneDraft(baseline()));
    setActionError(null);
    setRetryAction(null);
  };

  const requestDraftDecision = (
    action: () => void,
    title = "Save task draft before continuing?",
    body = "This task has unsaved changes. Save them, discard them, or stay here.",
  ): void => {
    if (!dirty()) {
      action();
      return;
    }
    focusTab("tasks");
    setPendingDecision({ title, body, action });
  };

  useBeforeLeave((event) => {
    if (!dirty() || event.defaultPrevented) return;
    event.preventDefault();
    requestDraftDecision(
      () => event.retry(true),
      "Leave Agent Tasks with an unsaved draft?",
      "Save the task, discard the draft, or remain on Agent Tasks.",
    );
  });

  const selectTask = (task: AgentTask): void => {
    requestDraftDecision(() => {
      restoreDraftPending = false;
      setCreating(false);
      setSelectedTaskId(task.id);
      setDraftForTask(task);
      setActionError(null);
      setRetryAction(null);
    });
  };

  const refreshTasks = (): void => {
    // A re-read is not a navigation and must not put the draft at risk:
    // loadTasks preserves the draft when told to, so refresh does exactly
    // that rather than popping the save/discard decision.
    void loadTasks(selectedTaskId(), true);
  };

  const toggleTask = async (task: AgentTask): Promise<void> => {
    setActionError(null);
    try {
      if (task.status === "active") {
        await api.pauseAgentTask(task.id);
      } else {
        await api.resumeAgentTask(task.id);
      }
      await loadTasks(task.id, dirty());
    } catch (error) {
      reportActionFailure("Failed to update task", error, () => void toggleTask(task));
    }
  };

  const deleteTask = async (task: AgentTask) => {
    if (!props.confirmAction) return;
    if (!await props.confirmAction(
      `Delete task "${task.name}"?`,
      "The schedule, prompt, and saved task definition will be permanently deleted. Existing session history is retained.",
    )) return;
    try {
      await api.deleteAgentTask(task.id);
      const remaining = tasks().filter((candidate) => candidate.id !== task.id);
      setTasks(remaining);
      const next = remaining[0]?.id ?? null;
      setCreating(false);
      setSelectedTaskId(next);
      const nextTask = remaining.find((candidate) => candidate.id === next);
      if (nextTask) setDraftForTask(nextTask);
      else setEmptyDraft();
      setActionError(null);
      setRetryAction(null);
    } catch (error) {
      reportActionFailure("Failed to delete task", error, () => void deleteTask(task));
    }
  };

  const runTaskNow = async (task: AgentTask) => {
    // Run the prompt that is on screen, not a stale saved one: when the draft
    // is dirty, persist it first and only run if the save actually landed.
    if (dirty()) {
      const saved = await saveTask();
      if (!saved) return;
    }
    const taskId = selectedTaskId() ?? task.id;
    setRunningTaskId(taskId);
    try {
      await api.runAgentTask(taskId);
      // Surface the new run as a row and stop here. Auto-navigating to the
      // session mid-run yanks the user off Agent Tasks and trips the leave
      // guard; the run row's session name is a link they can follow instead.
      await loadTasks(taskId, dirty());
    } catch (error) {
      reportActionFailure(
        "Failed to run task",
        error,
        () => void runTaskNow(task),
      );
    } finally {
      setRunningTaskId(null);
    }
  };

  const openRunSession = (run: AgentTaskRun) => {
    props.onOpenSession?.(run.session_id, run.session_name);
  };

  const steerRun = async (task: AgentTask, text: string, interrupt: boolean) => {
    setSteerBusy(true);
    try {
      await api.steerAgentTask(task.id, { text, interrupt });
      await loadTasks(task.id, dirty());
    } catch (error) {
      reportActionFailure(
        "Failed to steer run",
        error,
        () => void steerRun(task, text, interrupt),
      );
    } finally {
      setSteerBusy(false);
    }
  };

  const answerGate = async (
    task: AgentTask,
    gateId: string,
    optionIndex: number,
  ) => {
    setSteerBusy(true);
    try {
      await api.answerAgentTaskGate(task.id, gateId, { option: optionIndex });
      await loadTasks(task.id, dirty());
    } catch (error) {
      reportActionFailure(
        "Failed to answer gate",
        error,
        () => void answerGate(task, gateId, optionIndex),
      );
    } finally {
      setSteerBusy(false);
    }
  };

  return (
    <div class="agent-tasks-view">
      <div class="agent-tasks-header">
        <div>
          <h2>Agent Tasks</h2>
          <div class="agent-tasks-summary">
            <Show
              when={tasksLoaded()}
              fallback={<span>Task count unavailable</span>}
            >
              <span>{tasks().length} total</span>
              <span>{activeCount()} active</span>
              <span>{pausedCount()} paused</span>
            </Show>
            <Show when={dirty()}>
              <span class="agent-task-dirty">Unsaved draft</span>
            </Show>
          </div>
        </div>
        <div class="agent-tasks-header-actions">
          <button disabled={loading()} onClick={refreshTasks}>
            {loading() && tasksLoaded() ? "Refreshing..." : "Refresh"}
          </button>
          <button onClick={() => requestDraftDecision(startCreate)}>New Task</button>
        </div>
      </div>

      <Show when={actionError()}>
        {(message) => (
          <div class="agent-task-action-error" role="alert">
            <strong>{message()}</strong>
            <Show when={retryAction()}>
              {(retry) => <button onClick={() => retry()()}>Retry action</button>}
            </Show>
          </div>
        )}
      </Show>

      <div class="agent-tasks-layout">
        <section class={`agent-tasks-list ${tasksStale() ? "stale" : ""}`}>
          <Show when={tasksError()}>
            {(message) => (
              <div class="agent-tasks-read-error" role="alert">
                <div>
                  <strong>{message()}</strong>
                  <Show when={tasksStale()}>
                    <span> Last successful task list is retained and may be stale.</span>
                  </Show>
                </div>
                <button onClick={() => void loadTasks(selectedTaskId(), dirty())}>
                  Retry tasks
                </button>
              </div>
            )}
          </Show>
          <Show when={loading() && !tasksLoaded()}>
            <div class="agent-tasks-empty">Loading tasks...</div>
          </Show>
          <Show when={tasksLoaded()}>
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
                    onClick={() => selectTask(task)}
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
                      disabled={runningTaskId() === task().id || saving()}
                    >
                      {runningTaskId() === task().id
                        ? "Starting..."
                        : dirty()
                          ? "Save & Run"
                          : "Run Now"}
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

            <label class="agent-task-field">
              <span>Vogt project</span>
              <input
                type="text"
                placeholder="Project slug this run is about"
                value={draft().vogtProject}
                onInput={(e) =>
                  setDraft({ ...draft(), vogtProject: e.currentTarget.value })
                }
              />
            </label>

            <label class="agent-task-field">
              <span>Vogt work item</span>
              <input
                type="text"
                placeholder="Work item ref, e.g. WI-7"
                value={draft().vogtWorkItem}
                onInput={(e) =>
                  setDraft({ ...draft(), vogtWorkItem: e.currentTarget.value })
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
                <span class="agent-task-subfield-label">Daily times (UTC)</span>
                <input
                  type="text"
                  aria-label="Daily times (UTC)"
                  placeholder="09:00, 17:30 (UTC)"
                  value={draft().dailyTimes}
                  onInput={(e) =>
                    setDraft({ ...draft(), dailyTimes: e.currentTarget.value })
                  }
                />
              </Show>
            </div>

            <div class="agent-task-field agent-task-field-wide agent-task-triggers">
              <span>Triggers</span>
              <p class="agent-task-subfield-label">
                Fire a run from Vogt's own state, on top of the schedule. Each
                trigger is separately enabled; a fire that can't start (capacity
                or the item is gone) is dropped, not retried.
              </p>
              <Index each={draft().triggers}>
                {(trigger, index) => {
                  const t = () => trigger();
                  return (
                    <div class="agent-task-trigger" data-testid="trigger-row">
                      <div class="agent-task-trigger-head">
                        <label class="agent-task-trigger-enable">
                          <input
                            type="checkbox"
                            aria-label="Trigger enabled"
                            checked={t().enabled}
                            onChange={(e) =>
                              patchTrigger(index, {
                                enabled: e.currentTarget.checked,
                              })
                            }
                          />
                          <span>{triggerLabel(t().kind)}</span>
                        </label>
                        <button
                          type="button"
                          class="agent-task-trigger-remove"
                          onClick={() => removeTrigger(index)}
                        >
                          Remove
                        </button>
                      </div>

                      <Show when={t().kind === "work-transition"}>
                        <div class="agent-task-trigger-filters">
                          <label class="agent-task-field">
                            <span>Destination state</span>
                            <input
                              type="text"
                              aria-label="Destination state"
                              placeholder="e.g. ready"
                              value={
                                (t() as Extract<
                                  AgentTaskTrigger,
                                  { kind: "work-transition" }
                                >).to_state
                              }
                              onInput={(e) =>
                                patchTrigger(index, {
                                  to_state: e.currentTarget.value,
                                })
                              }
                            />
                          </label>
                          <label class="agent-task-field">
                            <span>Project filter</span>
                            <input
                              type="text"
                              aria-label="Project filter"
                              placeholder="Any project"
                              value={
                                (t() as Extract<
                                  AgentTaskTrigger,
                                  { kind: "work-transition" }
                                >).project ?? ""
                              }
                              onInput={(e) =>
                                patchTrigger(index, {
                                  project: e.currentTarget.value || null,
                                })
                              }
                            />
                          </label>
                          <label class="agent-task-field">
                            <span>Item kind filter</span>
                            <input
                              type="text"
                              aria-label="Item kind filter"
                              placeholder="Any kind"
                              value={
                                (t() as Extract<
                                  AgentTaskTrigger,
                                  { kind: "work-transition" }
                                >).item_kind ?? ""
                              }
                              onInput={(e) =>
                                patchTrigger(index, {
                                  item_kind: e.currentTarget.value || null,
                                })
                              }
                            />
                          </label>
                          <label class="agent-task-field">
                            <span>Label filter</span>
                            <input
                              type="text"
                              aria-label="Label filter"
                              placeholder="Any label"
                              value={
                                (t() as Extract<
                                  AgentTaskTrigger,
                                  { kind: "work-transition" }
                                >).label ?? ""
                              }
                              onInput={(e) =>
                                patchTrigger(index, {
                                  label: e.currentTarget.value || null,
                                })
                              }
                            />
                          </label>
                        </div>
                      </Show>

                      <Show when={t().kind === "observation-new"}>
                        <div class="agent-task-trigger-filters">
                          <label class="agent-task-field">
                            <span>Observation kind</span>
                            <input
                              type="text"
                              aria-label="Observation kind"
                              placeholder="e.g. forge.issue"
                              value={
                                (t() as Extract<
                                  AgentTaskTrigger,
                                  { kind: "observation-new" }
                                >).observation_kind
                              }
                              onInput={(e) =>
                                patchTrigger(index, {
                                  observation_kind: e.currentTarget.value,
                                })
                              }
                            />
                          </label>
                          <label class="agent-task-field">
                            <span>Project filter</span>
                            <input
                              type="text"
                              aria-label="Project filter"
                              placeholder="Any project"
                              value={
                                (t() as Extract<
                                  AgentTaskTrigger,
                                  { kind: "observation-new" }
                                >).project ?? ""
                              }
                              onInput={(e) =>
                                patchTrigger(index, {
                                  project: e.currentTarget.value || null,
                                })
                              }
                            />
                          </label>
                        </div>
                      </Show>

                      <Show when={t().kind === "drift-proposed"}>
                        <div class="agent-task-trigger-filters">
                          <label class="agent-task-field">
                            <span>Project filter</span>
                            <input
                              type="text"
                              aria-label="Project filter"
                              placeholder="Any project"
                              value={
                                (t() as Extract<
                                  AgentTaskTrigger,
                                  { kind: "drift-proposed" }
                                >).project ?? ""
                              }
                              onInput={(e) =>
                                patchTrigger(index, {
                                  project: e.currentTarget.value || null,
                                })
                              }
                            />
                          </label>
                        </div>
                      </Show>

                      <Show when={t().kind === "forge-pr-checks"}>
                        <div class="agent-task-trigger-filters">
                          <label class="agent-task-field">
                            <span>Check status</span>
                            <select
                              aria-label="Check status"
                              value={
                                (t() as Extract<
                                  AgentTaskTrigger,
                                  { kind: "forge-pr-checks" }
                                >).status ?? "any"
                              }
                              onChange={(e) =>
                                patchTrigger(index, {
                                  status: e.currentTarget
                                    .value as ForgeCheckStatus,
                                })
                              }
                            >
                              <option value="any">Green or red</option>
                              <option value="green">Green only</option>
                              <option value="red">Red only</option>
                            </select>
                          </label>
                          <label class="agent-task-field">
                            <span>Work item filter</span>
                            <input
                              type="text"
                              aria-label="Work item filter"
                              placeholder="Any linked item"
                              value={
                                (t() as Extract<
                                  AgentTaskTrigger,
                                  { kind: "forge-pr-checks" }
                                >).work_item ?? ""
                              }
                              onInput={(e) =>
                                patchTrigger(index, {
                                  work_item: e.currentTarget.value || null,
                                })
                              }
                            />
                          </label>
                        </div>
                      </Show>

                      <Show when={t().kind === "api"}>
                        <p class="agent-task-subfield-label">
                          Lets this task be fired programmatically (records the
                          run as <code>api</code>). No core event matches it.
                        </p>
                      </Show>
                    </div>
                  );
                }}
              </Index>

              <div class="agent-task-trigger-add">
                <select
                  aria-label="Trigger kind to add"
                  value={newTriggerKind()}
                  onChange={(e) =>
                    setNewTriggerKind(e.currentTarget.value as TriggerKind)
                  }
                >
                  <For each={TRIGGER_KINDS}>
                    {(entry) => <option value={entry.kind}>{entry.label}</option>}
                  </For>
                </select>
                <button type="button" onClick={() => addTrigger(newTriggerKind())}>
                  Add trigger
                </button>
              </div>

              <label class="agent-task-field agent-task-concurrency">
                <span>Max concurrent runs</span>
                <input
                  type="text"
                  aria-label="Max concurrent runs"
                  value={draft().concurrency}
                  onInput={(e) =>
                    setDraft({ ...draft(), concurrency: e.currentTarget.value })
                  }
                />
              </label>
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
                placeholder="VOGT_NOTIFY:"
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
              <button onClick={() => requestDraftDecision(startCreate)}>New Draft</button>
            </Show>
            <Show when={dirty()}>
              <button onClick={discardDraft}>Discard Draft Changes</button>
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
                            <Show when={run.trigger_detail}>
                              {(detail) => (
                                <span
                                  class="agent-task-run-trigger-detail"
                                  data-testid="run-trigger-detail"
                                  title={
                                    detail().event_kind
                                      ? `${detail().event_kind}${
                                          detail().event_seq != null
                                            ? ` @ seq ${detail().event_seq}`
                                            : ""
                                        }`
                                      : detail().trigger_kind
                                  }
                                >
                                  {detail().description}
                                </span>
                              )}
                            </Show>
                          </div>
                          <div class="agent-task-run-meta">
                            <button
                              type="button"
                              class="agent-task-run-session-link"
                              onClick={() => openRunSession(run)}
                            >
                              {run.session_name}
                            </button>
                            <span>{runStatusLabel(run)}</span>
                            <span>
                              {run.completed_at
                                ? `Finished ${formatLocalDate(run.completed_at)}`
                                : `Prompt ${run.prompt_file}`}
                            </span>
                          </div>
                          <Show when={run.outcome}>
                            {(outcome) => (
                              <div class="agent-task-run-conclusion">
                                <span
                                  class={`agent-task-outcome-badge outcome-${outcome()}`}
                                  data-testid="run-outcome"
                                >
                                  {outcomeLabel(outcome())}
                                </span>
                                <Show when={run.conclusion}>
                                  {(concl) => (
                                    <span class="agent-task-conclusion-facts">
                                      <span>
                                        {formatDuration(concl().duration_ms)}
                                      </span>
                                      <Show when={concl().exit_code !== null}>
                                        <span>exit {concl().exit_code}</span>
                                      </Show>
                                      <Show when={(concl().retries ?? 0) > 0}>
                                        <span>{concl().retries} retries</span>
                                      </Show>
                                      <Show when={concl().final_sha}>
                                        {(sha) => (
                                          <span class="agent-task-sha">
                                            {sha().slice(0, 7)}
                                          </span>
                                        )}
                                      </Show>
                                      <Show when={concl().diffstat}>
                                        {(diff) => (
                                          <span class="agent-task-diffstat">
                                            {diff().files} files{" "}
                                            <span class="diff-add">
                                              +{diff().insertions}
                                            </span>{" "}
                                            <span class="diff-del">
                                              -{diff().deletions}
                                            </span>
                                          </span>
                                        )}
                                      </Show>
                                      <Show when={formatCost(concl().cost)}>
                                        {(cost) => (
                                          <span class="agent-task-cost">
                                            {cost()}
                                          </span>
                                        )}
                                      </Show>
                                    </span>
                                  )}
                                </Show>
                              </div>
                            )}
                          </Show>
                          <Show when={(run.findings ?? []).length > 0}>
                            <ul class="agent-task-run-findings">
                              <For each={run.findings ?? []}>
                                {(finding) => (
                                  <li class="agent-task-run-finding">
                                    <span class="agent-task-finding-text">
                                      {finding.text}
                                    </span>
                                    <span class="agent-task-finding-meta">
                                      {finding.source} •{" "}
                                      {formatLocalDate(finding.at)}
                                    </span>
                                  </li>
                                )}
                              </For>
                            </ul>
                          </Show>
                          <AgentTaskSteering
                            run={run}
                            busy={steerBusy()}
                            onSteer={(text, interrupt) =>
                              steerRun(task(), text, interrupt)
                            }
                            onAnswerGate={(gateId, optionIndex) =>
                              answerGate(task(), gateId, optionIndex)
                            }
                          />
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

      <Show when={pendingDecision()}>
        {(decision) => (
          <Dialog
            title={decision().title}
            description={decision().body}
            closeOnEscape={false}
            onClose={() => setPendingDecision(null)}
          >
            <Show when={actionError()}>
              {(message) => <div class="agent-task-dialog-error">{message()}</div>}
            </Show>
            <div class="modal-actions">
              <button type="button" onClick={() => setPendingDecision(null)}>
                Stay here
              </button>
              <button
                type="button"
                onClick={() => {
                  const next = pendingDecision();
                  if (!next) return;
                  discardDraft();
                  setPendingDecision(null);
                  next.action();
                }}
              >
                Discard draft
              </button>
              <button
                type="button"
                data-dialog-initial-focus
                disabled={saving()}
                onClick={() => {
                  const next = pendingDecision();
                  if (!next) return;
                  void saveTask().then((saved) => {
                    if (!saved) return;
                    setPendingDecision(null);
                    next.action();
                  });
                }}
              >
                {saving() ? "Saving..." : "Save and continue"}
              </button>
            </div>
          </Dialog>
        )}
      </Show>
    </div>
  );
};

export default AgentTasks;
