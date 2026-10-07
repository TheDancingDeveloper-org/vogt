import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { MemoryRouter, Route, createMemoryHistory } from "@solidjs/router";
import { afterEach, describe, expect, it, vi } from "vitest";

import History, { historyIdentity } from "../History";
import {
  api,
  type HistorySessionMetadata,
  type OperationalStatus,
  type SessionSummary,
} from "../api";
import { refreshSessions } from "../store";

const CONVERSATION = "6c1f0d2e-5b7a-4e1c-9f3d-2a8b7c6d5e4f";

/** The 2026-10-07 incident's session (WI-962): a shell named "Oversight" that
 *  someone typed `claude` into, lost to a redeploy. */
const lostOverseer: HistorySessionMetadata = {
  id: "lost-1",
  name: "Oversight",
  created_at: "2026-10-07T00:00:00Z",
  ended_at: "2026-10-07T00:20:00Z",
  exit_code: null,
  cwd: "/home/sprooty/Working",
  command: "/usr/local/bin/vogt-agent-auth shell",
  scrollback_bytes: 4096,
  template: null,
  role: "oversight",
  conversation_agent: "claude",
  conversation_id: CONVERSATION,
  resume_template: "claude",
};

const plainShell: HistorySessionMetadata = {
  id: "shell-1",
  name: "scratch",
  created_at: "2026-10-06T00:00:00Z",
  ended_at: "2026-10-06T00:05:00Z",
  exit_code: 0,
  cwd: "/home/sprooty",
  command: "bash",
  scrollback_bytes: 10,
};

function status(): OperationalStatus {
  return {
    version: "test",
    session_count: 0,
    push_subscription_count: 0,
    gui_process_count: 0,
    gui_stream_configured: false,
    fcm_enabled: false,
    history: {
      enabled: true,
      archived_session_count: 2,
      log_file_count: 2,
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

function renderHistory(onOpenSession = vi.fn(), overseer = lostOverseer) {
  vi.spyOn(api, "operationalStatus").mockResolvedValue(status());
  vi.spyOn(api, "listHistorySessions").mockResolvedValue([overseer, plainShell]);
  vi.spyOn(api, "getHistorySession").mockImplementation(async (id) =>
    id === overseer.id ? overseer : plainShell,
  );
  vi.spyOn(api, "getHistorySessionLog").mockImplementation(async (id) => ({
    session_id: id,
    text: "",
    bytes: 0,
    total_bytes: 0,
    truncated: false,
  }));
  const history = createMemoryHistory();
  history.set({ value: "/history" });
  render(() => (
    <MemoryRouter history={history}>
      <Route path="/history" component={() => <History onOpenSession={onOpenSession} />} />
    </MemoryRouter>
  ));
  return onOpenSession;
}

afterEach(async () => {
  vi.restoreAllMocks();
  vi.spyOn(api, "listSessions").mockResolvedValue([]);
  await refreshSessions();
  vi.restoreAllMocks();
});

describe("History identity and resume (WI-962)", () => {
  it("names a session by its role and conversation, not its shell", () => {
    expect(historyIdentity(lostOverseer)).toBe("oversight · claude · 6c1f0d2e…");
    expect(historyIdentity(plainShell)).toBeNull();
  });

  it("shows a lost overseer's conversation and resumes it with the same role", async () => {
    const created: SessionSummary = {
      id: "new-1",
      name: "Oversight",
      activity: "running",
      exit_code: null,
      scrollback_bytes: 0,
      cwd: "/home/sprooty/Working",
      command: "claude",
      created_at: "2026-10-07T01:00:00Z",
    };
    const create = vi.spyOn(api, "createSession").mockResolvedValue(created);
    const onOpenSession = renderHistory();

    const row = (await screen.findByText("oversight · claude · 6c1f0d2e…")).closest("button");
    expect(row).not.toBeNull();
    await fireEvent.click(row!);
    expect(await screen.findByRole("heading", { name: "Oversight" })).toBeVisible();
    expect(screen.getByText(`claude · ${CONVERSATION}`)).toBeVisible();

    await fireEvent.click(screen.getByRole("button", { name: "Resume" }));
    await waitFor(() => expect(onOpenSession).toHaveBeenCalledWith("new-1", "Oversight"));
    expect(create).toHaveBeenCalledWith(
      expect.objectContaining({
        name: "Oversight",
        template: "claude",
        resume: CONVERSATION,
        role: "oversight",
      }),
    );
    expect(create.mock.calls[0]?.[0].command).toBeUndefined();
  });

  it("offers no resume for a session with no conversation", async () => {
    renderHistory();
    const row = (await screen.findByText("scratch")).closest("button");
    await fireEvent.click(row!);
    expect(await screen.findByRole("heading", { name: "scratch" })).toBeVisible();
    expect(screen.queryByRole("button", { name: "Resume" })).not.toBeInTheDocument();
  });

  it("offers no resume for a session that is still running", async () => {
    vi.spyOn(api, "listSessions").mockResolvedValue([
      {
        id: lostOverseer.id,
        name: "Oversight",
        activity: "running",
        exit_code: null,
        scrollback_bytes: 1,
        cwd: "/home/sprooty/Working",
        command: "bash",
        created_at: lostOverseer.created_at,
        role: "oversight",
        conversation: { agent: "claude", id: CONVERSATION },
      },
    ]);
    await refreshSessions();
    renderHistory();
    const row = (await screen.findByText("oversight · claude · 6c1f0d2e…")).closest("button");
    await fireEvent.click(row!);
    expect(await screen.findByRole("heading", { name: "Oversight" })).toBeVisible();
    expect(screen.queryByRole("button", { name: "Resume" })).not.toBeInTheDocument();
  });

  it("names the work item it served and resumes bound to it (WI-998)", async () => {
    const bound = { ...lostOverseer, work_item: "WI-998" };
    expect(historyIdentity(bound)).toBe("WI-998 · oversight · claude · 6c1f0d2e…");
    const create = vi.spyOn(api, "createSession").mockResolvedValue({
      id: "new-2",
      name: "Oversight",
      activity: "running",
      exit_code: null,
      scrollback_bytes: 0,
      cwd: "/home/sprooty/Working",
      created_at: "2026-10-07T01:00:00Z",
      work_item: "WI-998",
    });
    renderHistory(vi.fn(), bound);
    const row = (await screen.findByText("WI-998 · oversight · claude · 6c1f0d2e…")).closest("button");
    await fireEvent.click(row!);
    expect(await screen.findByRole("link", { name: "WI-998" })).toHaveAttribute("href", "#/w/WI-998");
    await fireEvent.click(screen.getByRole("button", { name: "Resume" }));
    await waitFor(() =>
      expect(create).toHaveBeenCalledWith(
        expect.objectContaining({ resume: CONVERSATION, work_item: "WI-998" }),
      ),
    );
  });
});
