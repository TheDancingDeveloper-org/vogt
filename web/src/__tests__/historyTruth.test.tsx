import { fireEvent, render, screen, within } from "@solidjs/testing-library";
import { MemoryRouter, Route, createMemoryHistory } from "@solidjs/router";
import { afterEach, describe, expect, it, vi } from "vitest";

import History from "../History";
import {
  api,
  type HistorySessionMetadata,
  type OperationalStatus,
} from "../api";

function archive(index: number): HistorySessionMetadata {
  return {
    id: `history-${index}`,
    name: `archive-${index}`,
    created_at: `2026-08-18T00:${String(index % 60).padStart(2, "0")}:00Z`,
    ended_at: "2026-08-18T01:00:00Z",
    exit_code: 0,
    cwd: "/workspace/vogt",
    command: `command-${index}`,
    scrollback_bytes: index + 10,
  };
}

function status(total: number | null): OperationalStatus {
  return {
    version: "test",
    session_count: 0,
    push_subscription_count: 0,
    gui_process_count: 0,
    gui_stream_configured: false,
    fcm_enabled: false,
    history: {
      enabled: true,
      archived_session_count: total,
      log_file_count: total,
      log_bytes: 0,
      db_bytes: 0,
    },
    agent_tasks: {
      task_count: 0,
      prompt_task_dir_count: 0,
      prompt_file_count: 0,
      context_file_count: 0,
      prompt_bytes: 0,
      orphan_task_dir_count: 0,
    },
    auth_broker: { auto_agent_auth: false, helper: "" },
    storage: { state_dir: "/state", workspace_root: "/workspace" },
  };
}

function mockDetailReads(): void {
  vi.spyOn(api, "getHistorySession").mockImplementation(async (id) => {
    const index = Number.parseInt(id.replace("history-", ""), 10);
    return archive(index);
  });
  vi.spyOn(api, "getHistorySessionLog").mockImplementation(async (id) => ({
    session_id: id,
    text: `output-${id}`,
    bytes: 16,
    total_bytes: 16,
    truncated: false,
  }));
}

function renderHistory() {
  const history = createMemoryHistory();
  history.set({ value: "/history" });
  return render(() => (
    <MemoryRouter history={history}>
      <Route path="/history" component={() => <History />} />
    </MemoryRouter>
  ));
}

afterEach(() => vi.restoreAllMocks());

describe("History read truth", () => {
  it("keeps a failed initial read distinct from a successful empty archive and retries", async () => {
    vi.spyOn(api, "operationalStatus").mockResolvedValue(status(0));
    vi.spyOn(api, "listHistorySessions")
      .mockRejectedValueOnce(new Error("503 archive unavailable"))
      .mockResolvedValueOnce([]);

    renderHistory();

    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Failed to load history: 503 archive unavailable",
    );
    expect(screen.queryByText("0 archived sessions")).not.toBeInTheDocument();
    expect(screen.queryByText("No archived sessions.")).not.toBeInTheDocument();

    await fireEvent.click(screen.getByRole("button", { name: "Retry history" }));

    expect(await screen.findByText("0 archived sessions")).toBeVisible();
    expect(screen.getByText("No archived sessions.")).toBeVisible();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("retains the last successful archive as stale when refresh fails", async () => {
    const item = archive(1);
    vi.spyOn(api, "operationalStatus").mockResolvedValue(status(1));
    vi.spyOn(api, "listHistorySessions")
      .mockResolvedValueOnce([item])
      .mockRejectedValueOnce(new Error("history database busy"));
    mockDetailReads();

    const { container } = renderHistory();
    expect(await screen.findByText("1 of 1 archived sessions loaded")).toBeVisible();

    await fireEvent.click(screen.getByRole("button", { name: "Refresh" }));

    expect(await screen.findByRole("alert")).toHaveTextContent("history database busy");
    expect(screen.getByRole("alert")).toHaveTextContent("may be stale");
    expect(screen.getAllByText(item.name)[0]).toBeVisible();
    expect(container.querySelector(".history-list.stale")).not.toBeNull();
  });

  // Two hundred rendered rows and a second page of them: slow enough on a
  // loaded machine to exceed the default budget without anything being wrong.
  it("loads a continuation page without losing filters, selection, or pins", { timeout: 20_000 }, async () => {
    const firstPage = Array.from({ length: 200 }, (_, index) => archive(index));
    const lastPage = [archive(200)];
    vi.spyOn(api, "operationalStatus").mockResolvedValue(status(201));
    const list = vi.spyOn(api, "listHistorySessions")
      .mockResolvedValueOnce(firstPage)
      .mockResolvedValueOnce(lastPage);
    mockDetailReads();

    renderHistory();
    expect(await screen.findByText("200 of 201 archived sessions loaded")).toBeVisible();

    const chosen = screen.getByText(/^archive-12$/).closest("button");
    expect(chosen).not.toBeNull();
    await fireEvent.click(chosen!);
    expect(await screen.findByRole("heading", { name: "archive-12" })).toBeVisible();
    await fireEvent.click(screen.getByRole("button", { name: "Pin" }));
    const filter = screen.getByRole("searchbox", { name: "Filter loaded sessions" });
    await fireEvent.input(filter, { target: { value: "archive" } });

    await fireEvent.click(screen.getByRole("button", { name: "Load more" }));

    expect(await screen.findByText("201 of 201 archived sessions loaded")).toBeVisible();
    expect(list).toHaveBeenLastCalledWith(200, 200);
    expect(filter).toHaveValue("archive");
    expect(screen.getByRole("heading", { name: "archive-12" })).toBeVisible();
    expect(screen.getByRole("button", { name: "Unpin" })).toBeVisible();
    expect(screen.getByText("1 pinned")).toBeVisible();
    expect(screen.queryByRole("button", { name: "Load more" })).not.toBeInTheDocument();
  });

  it("does not imply completeness when the total is unknown", async () => {
    const firstPage = Array.from({ length: 200 }, (_, index) => archive(index));
    vi.spyOn(api, "operationalStatus").mockResolvedValue(status(null));
    vi.spyOn(api, "listHistorySessions")
      .mockResolvedValueOnce(firstPage)
      .mockResolvedValueOnce([]);
    mockDetailReads();

    renderHistory();
    expect(
      await screen.findByText("200 archived sessions loaded; more may be available"),
    ).toBeVisible();

    await fireEvent.click(screen.getByRole("button", { name: "Load more" }));

    expect(await screen.findByText("200 archived sessions loaded (all reached)")).toBeVisible();
    expect(screen.queryByRole("button", { name: "Load more" })).not.toBeInTheDocument();
  });

  it("keeps search, detail, and replay failures local and retryable", async () => {
    const item = archive(7);
    vi.spyOn(api, "operationalStatus").mockResolvedValue(status(1));
    vi.spyOn(api, "listHistorySessions").mockResolvedValue([item]);
    vi.spyOn(api, "getHistorySession")
      .mockRejectedValueOnce(new Error("detail offline"))
      .mockResolvedValue(item);
    vi.spyOn(api, "getHistorySessionLog")
      .mockRejectedValueOnce(new Error("log offline"))
      .mockResolvedValue({
        session_id: item.id,
        text: "recovered output",
        bytes: 16,
        total_bytes: 16,
        truncated: false,
      });
    vi.spyOn(api, "searchHistory")
      .mockRejectedValueOnce(new Error("search offline"))
      .mockResolvedValue([
        {
          session_id: item.id,
          session_name: item.name,
          created_at: item.created_at,
          match_snippet: "<mark>recovered</mark>",
          rank: 1,
        },
      ]);

    renderHistory();
    expect(await screen.findByText("Failed to load session detail: detail offline")).toBeVisible();

    await fireEvent.click(screen.getByRole("button", { name: "Retry session detail" }));
    expect(await screen.findByRole("heading", { name: item.name })).toBeVisible();
    expect(screen.getByText("Failed to load replay: log offline")).toBeVisible();
    await fireEvent.click(screen.getByRole("button", { name: "Retry replay" }));
    expect(await screen.findByText("recovered output")).toBeVisible();

    const search = screen.getByRole("searchbox", { name: "Search all archived output" });
    await fireEvent.input(search, { target: { value: "recovered" } });
    const alert = await screen.findByRole("alert");
    expect(within(alert).getByText(/Search failed: search offline/)).toBeVisible();
    expect(screen.queryByText("No output matches found.")).not.toBeInTheDocument();
    expect(screen.getByText(/Output search runs server-wide across the full archive/)).toBeVisible();

    await fireEvent.click(screen.getByRole("button", { name: "Retry output search" }));
    expect(await screen.findByText("1 output matches (up to 100)")).toBeVisible();
    expect(screen.getAllByText("recovered", { selector: "mark" })[0]).toBeVisible();
  });
});
