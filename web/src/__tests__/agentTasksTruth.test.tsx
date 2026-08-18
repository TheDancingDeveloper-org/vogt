import {
  MemoryRouter,
  Route,
  createMemoryHistory,
  useNavigate,
} from "@solidjs/router";
import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";

import AgentTasks, { type AgentTaskDraftGuard } from "../AgentTasks";
import { api, type AgentTask, type AgentTaskUpsertRequest } from "../api";

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

const ALPHA = task("task-alpha", "Alpha review");
const BETA = task("task-beta", "Beta review");

function mountTasks(
  props: { registerDraftGuard?: (guard: AgentTaskDraftGuard | null) => void } = {},
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

    await fireEvent.click(screen.getByRole("button", { name: "Refresh" }));
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
