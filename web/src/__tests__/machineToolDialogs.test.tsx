import { Route, Router } from "@solidjs/router";
import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";

import AgentTasks from "../AgentTasks";
import GitTab from "../Git";
import GuiTab from "../Gui";
import History from "../History";
import { api, type AgentTask, type HistorySessionMetadata } from "../api";

const TASK: AgentTask = {
  id: "task-1",
  name: "Nightly review",
  prompt: "Review the queue",
  schedule: { kind: "manual" },
  concurrency: 1,
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

const ARCHIVE: HistorySessionMetadata = {
  id: "history-1",
  name: "failed-build",
  created_at: "2026-08-18T00:00:00Z",
  ended_at: "2026-08-18T00:01:00Z",
  exit_code: 1,
  cwd: "/workspace/vogt",
  command: "uv run pytest",
  scrollback_bytes: 100,
};

afterEach(() => vi.restoreAllMocks());

describe("machine tool destructive decisions", () => {
  it("confirms, cancels, and reports Agent Task delete failures", async () => {
    vi.spyOn(api, "listAgentTasks").mockResolvedValue([TASK]);
    const remove = vi.spyOn(api, "deleteAgentTask").mockRejectedValue(new Error("task locked"));
    const confirmAction = vi
      .fn<(title: string, body?: string) => Promise<boolean>>()
      .mockResolvedValueOnce(false)
      .mockResolvedValueOnce(true);
    const onError = vi.fn();
    render(() => (
      <Router>
        <Route
          path="*"
          component={() => (
            <AgentTasks
              confirmAction={confirmAction}
              onError={onError}
            />
          )}
        />
      </Router>
    ));

    expect((await screen.findAllByText("Nightly review"))[0]).toBeVisible();
    const deleteButton = await screen.findByRole("button", { name: "Delete" });
    await fireEvent.click(deleteButton);
    expect(remove).not.toHaveBeenCalled();
    await fireEvent.click(deleteButton);
    await waitFor(() => expect(remove).toHaveBeenCalledWith("task-1"));
    await waitFor(() =>
      expect(onError).toHaveBeenCalledWith("Failed to delete task: task locked"),
    );
    expect(confirmAction.mock.calls[0]?.[0]).toBe('Delete task "Nightly review"?');
  });

  it("confirms, cancels, and keeps Git discard failures local", async () => {
    vi.spyOn(api, "gitStatus").mockResolvedValue({
      repo: "vogt",
      is_repo: true,
      branch: "dev",
      ahead: 0,
      behind: 0,
      entries: [
        { path: "src/App.tsx", index: " ", worktree: "M", kind: "modified" },
      ],
    });
    vi.spyOn(api, "gitBranch").mockResolvedValue({ current: "dev", all: ["dev"] });
    vi.spyOn(api, "gitLog").mockResolvedValue([]);
    vi.spyOn(api, "gitDiff").mockResolvedValue({
      path: "src/App.tsx",
      current: "changed",
      head: "original",
    });
    const gitOp = vi.spyOn(api, "gitOp").mockRejectedValue(new Error("checkout refused"));
    const confirmAction = vi
      .fn<(title: string, body?: string) => Promise<boolean>>()
      .mockResolvedValueOnce(false)
      .mockResolvedValueOnce(true);
    render(() => (
      <Router>
        <Route
          path="*"
          component={() => <GitTab repo="vogt" confirmAction={confirmAction} />}
        />
      </Router>
    ));

    await fireEvent.click(await screen.findByText("src/App.tsx"));
    const discard = screen.getByRole("button", { name: "Discard" });
    await fireEvent.click(discard);
    expect(gitOp).not.toHaveBeenCalled();
    await fireEvent.click(discard);
    await waitFor(() => expect(gitOp).toHaveBeenCalledWith({
      op: "discard",
      repo: "vogt",
      path: "src/App.tsx",
    }));
    expect(await screen.findByText("checkout refused")).toBeVisible();
    expect(confirmAction.mock.calls[0]?.[0]).toBe('Discard changes in "src/App.tsx"?');
  });

  it("confirms, cancels, and reports History delete failures", async () => {
    vi.spyOn(api, "listHistorySessions").mockResolvedValue([ARCHIVE]);
    vi.spyOn(api, "getHistorySession").mockResolvedValue(ARCHIVE);
    vi.spyOn(api, "getHistorySessionLog").mockResolvedValue({
      session_id: ARCHIVE.id,
      text: "failed",
      bytes: 6,
      total_bytes: 6,
      truncated: false,
    });
    const remove = vi.spyOn(api, "deleteHistorySession").mockRejectedValue(new Error("archive busy"));
    const confirmAction = vi
      .fn<(title: string, body?: string) => Promise<boolean>>()
      .mockResolvedValueOnce(false)
      .mockResolvedValueOnce(true);
    const onError = vi.fn();
    render(() => (
      <Router>
        <Route
          path="*"
          component={() => (
            <History confirmAction={confirmAction} onError={onError} />
          )}
        />
      </Router>
    ));

    expect(await screen.findByRole("heading", { name: "failed-build" })).toBeVisible();
    const removeButton = screen.getByRole("button", { name: "Delete" });
    await fireEvent.click(removeButton);
    expect(remove).not.toHaveBeenCalled();
    await fireEvent.click(removeButton);
    await waitFor(() => expect(remove).toHaveBeenCalledWith("history-1"));
    await waitFor(() =>
      expect(onError).toHaveBeenCalledWith("Delete failed: archive busy"),
    );
    expect(confirmAction.mock.calls[0]?.[0]).toBe(
      'Delete archived session "failed-build"?',
    );
  });

  it("renders GUI launch failures locally and forwards them to feedback", async () => {
    vi.spyOn(api, "guiProcesses").mockResolvedValue([]);
    vi.spyOn(api, "guiLaunch").mockRejectedValue(new Error("display unavailable"));
    const nativeAlert = vi.spyOn(window, "alert").mockImplementation(() => {});
    const onError = vi.fn();
    render(() => <GuiTab streamUrl={null} onError={onError} />);

    await fireEvent.click(screen.getByRole("button", { name: "Launch" }));

    expect(await screen.findByText("GUI launch failed: display unavailable")).toBeVisible();
    expect(onError).toHaveBeenCalledWith("GUI launch failed: display unavailable");
    expect(nativeAlert).not.toHaveBeenCalled();
  });
});
