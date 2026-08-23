import {
  MemoryRouter,
  Route,
  createMemoryHistory,
  useNavigate,
} from "@solidjs/router";
import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";

import AgentTasks, { type AgentTaskDraftGuard } from "../AgentTasks";
import {
  api,
  type AgentTask,
  type AgentTaskRun,
  type AgentTaskUpsertRequest,
} from "../api";
import { readToolDraft, writeToolDraft } from "../toolDrafts";
import * as store from "../store";

function task(id: string, name: string, prompt = `${name} prompt`): AgentTask {
  return {
    id,
    name,
    prompt,
    schedule: { kind: "manual" },
    status: "active",
    command: null,
    cwd: null,
    env: [],
    context: null,
    vogt_project: null,
    vogt_work_item: null,
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
}

function run(overrides: Partial<AgentTaskRun> = {}): AgentTaskRun {
  return {
    id: "run-1",
    task_id: "task-alpha",
    started_at: "2026-08-18T00:00:00Z",
    trigger: "scheduled",
    session_id: "session-1",
    session_name: "nightly-run",
    prompt_file: "prompt.txt",
    context_file: "context.txt",
    status: "completed",
    completed_at: "2026-08-18T00:05:00Z",
    exit_code: 0,
    summary: null,
    findings: [],
    ...overrides,
  };
}

const ALPHA = task("task-alpha", "Alpha review");
const BETA = task("task-beta", "Beta review");

function mountTasks(
  props: {
    registerDraftGuard?: (guard: AgentTaskDraftGuard | null) => void;
    onOpenSession?: (sessionId: string, label: string) => void;
    onError?: (message: string) => void;
  } = {},
) {
  const history = createMemoryHistory();
  history.set({ value: "/tasks" });
  const rendered = render(() => (
    <MemoryRouter history={history}>
      <Route path="/tasks" component={() => <AgentTasks {...props} />} />
    </MemoryRouter>
  ));
  return { ...rendered, history };
}

afterEach(() => vi.restoreAllMocks());

describe("Agent Tasks read truth", () => {
  it("distinguishes an initial failure from a legitimate empty task list", async () => {
    vi.spyOn(api, "listAgentTasks")
      .mockRejectedValueOnce(new Error("scheduler offline"))
      .mockResolvedValueOnce([]);

    mountTasks();

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Failed to load tasks: scheduler offline",
    );
    expect(screen.getByText("Task count unavailable")).toBeVisible();
    expect(screen.queryByText(/No recurring tasks yet/)).not.toBeInTheDocument();

    await fireEvent.click(screen.getByRole("button", { name: "Retry tasks" }));

    await waitFor(() => expect(screen.queryByRole("alert")).not.toBeInTheDocument());
    expect(screen.getByText("0 total")).toBeVisible();
    expect(screen.getByText(/No recurring tasks yet/)).toBeVisible();
  });

  it("retains and labels the last task list when refresh fails, then recovers", async () => {
    const list = vi.spyOn(api, "listAgentTasks")
      .mockResolvedValueOnce([ALPHA, BETA])
      .mockRejectedValueOnce(new Error("timeout"))
      .mockResolvedValueOnce([ALPHA, BETA]);

    mountTasks();
    await fireEvent.click(await screen.findByRole("button", { name: /Beta review/ }));
    expect(screen.getByLabelText("Name")).toHaveValue("Beta review");

    await fireEvent.click(screen.getByRole("button", { name: "Refresh" }));

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Last successful task list is retained and may be stale",
    );
    expect(screen.getByRole("button", { name: /Beta review/ })).toHaveClass("active");
    expect(screen.getByLabelText("Name")).toHaveValue("Beta review");

    await fireEvent.click(screen.getByRole("button", { name: "Retry tasks" }));
    await waitFor(() => expect(screen.queryByRole("alert")).not.toBeInTheDocument());
    expect(screen.getByRole("button", { name: /Beta review/ })).toHaveClass("active");
    expect(list).toHaveBeenCalledTimes(3);
  });
});

describe("#247 — the notify phrase is VOGT_NOTIFY", () => {
  it("defaults a new task's notification phrase to VOGT_NOTIFY:", async () => {
    vi.spyOn(api, "listAgentTasks").mockResolvedValue([]);
    mountTasks();

    await fireEvent.click(await screen.findByRole("button", { name: "New Task" }));
    const phrase = screen.getByLabelText("Notification Phrase") as HTMLInputElement;
    expect(phrase.value).toBe("VOGT_NOTIFY:");
    expect(phrase.placeholder).toBe("VOGT_NOTIFY:");
  });
});

describe("Agent Task draft ownership", () => {
  it("offers stay, discard, and save before changing task selection", async () => {
    let savedBeta = BETA;
    vi.spyOn(api, "listAgentTasks").mockImplementation(async () => [ALPHA, savedBeta]);
    const update = vi.spyOn(api, "updateAgentTask").mockImplementation(
      async (id: string, request: Partial<AgentTaskUpsertRequest>) => {
        savedBeta = { ...BETA, ...request, id, name: request.name ?? BETA.name };
        return savedBeta;
      },
    );

    mountTasks();
    await screen.findByRole("button", { name: /Alpha review/ });
    await fireEvent.input(screen.getByLabelText("Name"), {
      target: { value: "Alpha draft" },
    });
    expect(screen.getByText("Unsaved draft")).toBeVisible();

    await fireEvent.click(screen.getByRole("button", { name: /Beta review/ }));
    expect(screen.getByRole("dialog", { name: "Save task draft before continuing?" }))
      .toBeVisible();
    await fireEvent.click(screen.getByRole("button", { name: "Stay here" }));
    expect(screen.getByLabelText("Name")).toHaveValue("Alpha draft");
    expect(screen.getByRole("button", { name: /Alpha review/ })).toHaveClass("active");

    await fireEvent.click(screen.getByRole("button", { name: /Beta review/ }));
    await fireEvent.click(screen.getByRole("button", { name: "Discard draft" }));
    expect(screen.getByLabelText("Name")).toHaveValue("Beta review");
    expect(screen.getByRole("button", { name: /Beta review/ })).toHaveClass("active");

    await fireEvent.input(screen.getByLabelText("Name"), {
      target: { value: "Beta saved" },
    });
    await fireEvent.click(screen.getByRole("button", { name: /Alpha review/ }));
    await fireEvent.click(screen.getByRole("button", { name: "Save and continue" }));

    await waitFor(() => expect(update).toHaveBeenCalledTimes(1));
    expect(update).toHaveBeenCalledWith(
      "task-beta",
      expect.objectContaining({ name: "Beta saved", prompt: "Beta review prompt" }),
    );
    await waitFor(() =>
      expect(screen.getByRole("button", { name: /Alpha review/ })).toHaveClass("active"),
    );
    expect(screen.getByLabelText("Name")).toHaveValue("Alpha review");
  });

  it("keeps every new-draft value and the pending decision when save fails", async () => {
    let created: AgentTask | null = null;
    vi.spyOn(api, "listAgentTasks").mockImplementation(async () =>
      created ? [created] : [ALPHA],
    );
    const create = vi.spyOn(api, "createAgentTask")
      .mockRejectedValueOnce(new Error("write unavailable"))
      .mockImplementationOnce(async (request) => {
        created = {
          ...task("task-new", request.name, request.prompt),
          cwd: request.cwd ?? null,
          context: request.context ?? null,
        };
        return created;
      });

    mountTasks();
    await screen.findByRole("button", { name: /Alpha review/ });
    await fireEvent.click(screen.getByRole("button", { name: "New Task" }));
    await fireEvent.input(screen.getByLabelText("Name"), {
      target: { value: "Fresh task" },
    });
    await fireEvent.input(screen.getByLabelText("Prompt"), {
      target: { value: "Keep this exact prompt" },
    });
    await fireEvent.input(screen.getByLabelText("Persistent Context"), {
      target: { value: "Keep this context too" },
    });

    // Refresh no longer pops the save/discard decision (it preserves the
    // draft in place); starting another New Task while dirty still does.
    await fireEvent.click(screen.getByRole("button", { name: "New Task" }));
    await fireEvent.click(screen.getByRole("button", { name: "Save and continue" }));

    expect((await screen.findAllByText("Failed to save task: write unavailable"))[0])
      .toBeVisible();
    expect(screen.getByRole("dialog")).toBeVisible();
    expect(screen.getByLabelText("Name")).toHaveValue("Fresh task");
    expect(screen.getByLabelText("Prompt")).toHaveValue("Keep this exact prompt");
    expect(screen.getByLabelText("Persistent Context")).toHaveValue(
      "Keep this context too",
    );

    await fireEvent.click(screen.getByRole("button", { name: "Save and continue" }));
    await waitFor(() => expect(create).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
    expect(screen.queryByText("Unsaved draft")).not.toBeInTheDocument();
  });

  it("guards route exit and explicit tab-close requests without losing the draft", async () => {
    vi.spyOn(api, "listAgentTasks").mockResolvedValue([ALPHA]);
    let guard: AgentTaskDraftGuard | null = null;
    const closed = vi.fn();
    const history = createMemoryHistory();
    history.set({ value: "/tasks" });
    const LeaveControl = () => {
      const navigate = useNavigate();
      return (
        <>
          <AgentTasks registerDraftGuard={(next) => { guard = next; }} />
          <button onClick={() => navigate("/elsewhere")}>Leave surface</button>
        </>
      );
    };
    render(() => (
      <MemoryRouter history={history}>
        <Route path="/tasks" component={LeaveControl} />
        <Route path="/elsewhere" component={() => <div>Elsewhere</div>} />
      </MemoryRouter>
    ));
    await screen.findByRole("button", { name: /Alpha review/ });
    await fireEvent.input(screen.getByLabelText("Name"), {
      target: { value: "Protected draft" },
    });

    await fireEvent.click(screen.getByRole("button", { name: "Leave surface" }));
    expect(screen.getByRole("dialog", { name: "Leave Agent Tasks with an unsaved draft?" }))
      .toBeVisible();
    expect(history.get()).toBe("/tasks");
    await fireEvent.click(screen.getByRole("button", { name: "Stay here" }));
    expect(screen.getByLabelText("Name")).toHaveValue("Protected draft");

    guard!.requestLeave(closed);
    expect(closed).not.toHaveBeenCalled();
    await fireEvent.click(screen.getByRole("button", { name: "Discard draft" }));
    expect(closed).toHaveBeenCalledOnce();
  });
});

describe("Agent Task Vogt bindings", () => {
  it("round-trips the project and work-item bindings through the upsert", async () => {
    let saved = ALPHA;
    vi.spyOn(api, "listAgentTasks").mockImplementation(async () => [saved]);
    const update = vi.spyOn(api, "updateAgentTask").mockImplementation(
      async (id: string, request: Partial<AgentTaskUpsertRequest>) => {
        saved = {
          ...ALPHA,
          id,
          vogt_project: request.vogt_project ?? null,
          vogt_work_item: request.vogt_work_item ?? null,
        };
        return saved;
      },
    );

    mountTasks();
    await screen.findByRole("button", { name: /Alpha review/ });
    await fireEvent.input(screen.getByLabelText("Vogt project"), {
      target: { value: "vogt" },
    });
    await fireEvent.input(screen.getByLabelText("Vogt work item"), {
      target: { value: "WI-7" },
    });

    await fireEvent.click(screen.getByRole("button", { name: "Save Changes" }));

    await waitFor(() => expect(update).toHaveBeenCalledTimes(1));
    expect(update).toHaveBeenCalledWith(
      "task-alpha",
      expect.objectContaining({ vogt_project: "vogt", vogt_work_item: "WI-7" }),
    );
    // The saved values survive a re-read into the form.
    await waitFor(() =>
      expect(screen.getByLabelText("Vogt project")).toHaveValue("vogt"),
    );
    expect(screen.getByLabelText("Vogt work item")).toHaveValue("WI-7");
  });
});

describe("Agent Task run findings", () => {
  it("renders each finding recorded under a run row", async () => {
    const withFindings: AgentTask = {
      ...ALPHA,
      run_count: 1,
      runs: [
        run({
          findings: [
            { at: "2026-08-18T00:03:00Z", text: "Queue is clear", source: "notify" },
          ],
        }),
      ],
    };
    vi.spyOn(api, "listAgentTasks").mockResolvedValue([withFindings]);

    mountTasks();
    await screen.findByRole("button", { name: /Alpha review/ });

    expect(await screen.findByText("Queue is clear")).toBeVisible();
    expect(screen.getByText(/notify/)).toBeVisible();
  });
});

describe("Agent Task self-refresh", () => {
  it("reloads the list when a run's session exits", async () => {
    let listeners: ((id: string, exit: number | null) => void)[] = [];
    vi.spyOn(store, "onSessionKilled").mockImplementation((listener) => {
      listeners.push(listener);
      return () => {
        listeners = listeners.filter((one) => one !== listener);
      };
    });
    const running: AgentTask = {
      ...ALPHA,
      run_count: 1,
      runs: [run({ status: "running", completed_at: null, exit_code: null, summary: null })],
    };
    const list = vi.spyOn(api, "listAgentTasks").mockResolvedValue([running]);

    mountTasks();
    await screen.findByRole("button", { name: /Alpha review/ });
    await waitFor(() => expect(list).toHaveBeenCalledTimes(1));

    // A stranger's session exiting is ignored; ours triggers a re-read.
    listeners.forEach((fire) => fire("some-other-session", 0));
    expect(list).toHaveBeenCalledTimes(1);
    listeners.forEach((fire) => fire("session-1", 0));
    await waitFor(() => expect(list).toHaveBeenCalledTimes(2));
  });
});

describe("Agent Task safer Run Now", () => {
  it("relabels Run Now to Save & Run when dirty, saving before it runs", async () => {
    let saved = ALPHA;
    vi.spyOn(api, "listAgentTasks").mockImplementation(async () => [saved]);
    const update = vi.spyOn(api, "updateAgentTask").mockImplementation(
      async (id: string, request: Partial<AgentTaskUpsertRequest>) => {
        saved = { ...ALPHA, id, name: request.name ?? ALPHA.name };
        return saved;
      },
    );
    const runNow = vi.spyOn(api, "runAgentTask").mockResolvedValue(
      run({ session_id: "run-session", session_name: "on-demand" }),
    );
    const openSession = vi.fn();

    mountTasks({ onOpenSession: openSession });
    await screen.findByRole("button", { name: /Alpha review/ });
    // Not dirty: the action reads "Run Now".
    expect(screen.getByRole("button", { name: "Run Now" })).toBeVisible();

    await fireEvent.input(screen.getByLabelText("Name"), {
      target: { value: "Alpha edited" },
    });
    const saveRun = await screen.findByRole("button", { name: "Save & Run" });

    await fireEvent.click(saveRun);
    await waitFor(() => expect(update).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(runNow).toHaveBeenCalledTimes(1));
    expect(update).toHaveBeenCalledWith(
      "task-alpha",
      expect.objectContaining({ name: "Alpha edited" }),
    );
    // The run does not yank the user off Agent Tasks to the session.
    expect(openSession).not.toHaveBeenCalled();
  });
});

describe("Tool draft sessionStorage mirror", () => {
  afterEach(() => {
    sessionStorage.clear();
    vi.resetModules();
  });

  it("survives a remount that has lost the in-memory map", async () => {
    writeToolDraft("tasks", { hello: "world" });
    expect(sessionStorage.getItem("vogt.toolDraft.tasks")).toContain("world");

    // A reload keeps sessionStorage but starts a fresh module: re-import so
    // the in-memory Map is empty and the read can only come from storage.
    vi.resetModules();
    const fresh = await import("../toolDrafts");
    expect(fresh.readToolDraft("tasks", null)).toEqual({ hello: "world" });
    // Belt and braces: the original module still reads it too.
    expect(readToolDraft("tasks", null)).toEqual({ hello: "world" });
  });
});

describe("#291 — a run's typed outcome and conclusion", () => {
  it("renders the outcome badge with its diffstat, retries, sha, and cost", async () => {
    const concluded = run({
      status: "errored",
      exit_code: 1,
      summary: "Findings did not match the output schema",
      outcome: "partially-succeeded",
      retries: 2,
      schema_ok: false,
      conclusion: {
        started: "2026-08-18T00:00:00Z",
        finished: "2026-08-18T00:00:03Z",
        duration_ms: 3000,
        outcome: "partially-succeeded",
        exit_code: 1,
        retries: 2,
        branch: "feat/x",
        final_sha: "abcdef1234567890",
        base_sha: "0000000",
        diffstat: { files: 2, insertions: 10, deletions: 3 },
        cost: { total_usd: 0.42 },
        findings: [],
      },
    });
    const withRun: AgentTask = {
      ...task("task-alpha", "Alpha review"),
      runs: [concluded],
    };
    vi.spyOn(api, "listAgentTasks").mockResolvedValue([withRun]);

    mountTasks();
    await fireEvent.click(await screen.findByRole("button", { name: /Alpha review/ }));

    const badge = await screen.findByTestId("run-outcome");
    expect(badge).toHaveTextContent("Partial");
    expect(badge).toHaveClass("outcome-partially-succeeded");
    // The conclusion facts a reader scans without opening the session.
    expect(screen.getByText("3.0s")).toBeVisible();
    expect(screen.getByText("2 retries")).toBeVisible();
    expect(screen.getByText("abcdef1")).toBeVisible();
    expect(screen.getByText(/2 files/)).toBeVisible();
    expect(screen.getByText("+10")).toBeVisible();
    expect(screen.getByText("-3")).toBeVisible();
    expect(screen.getByText("$0.42")).toBeVisible();
  });

  it("shows no outcome badge for a run that is still running", async () => {
    const running = run({
      status: "running",
      completed_at: null,
      exit_code: null,
      outcome: undefined,
      conclusion: undefined,
    });
    const withRun: AgentTask = {
      ...task("task-alpha", "Alpha review"),
      runs: [running],
    };
    vi.spyOn(api, "listAgentTasks").mockResolvedValue([withRun]);

    mountTasks();
    await fireEvent.click(await screen.findByRole("button", { name: /Alpha review/ }));
    await screen.findByText("Still running");
    expect(screen.queryByTestId("run-outcome")).not.toBeInTheDocument();
  });
});
