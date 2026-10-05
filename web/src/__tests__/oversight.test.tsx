// The oversight board (WI-915): one table of every session, most urgent
// first, from session.sweep — and an honest message when it cannot be had.

import { fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";

import Oversight, { ago } from "../Oversight";
import * as vogtApi from "../vogtApi";

const ROW = (over: Partial<vogtApi.SessionSweepRow>): vogtApi.SessionSweepRow => ({
  attention: "running",
  attention_reason: "working",
  session: {
    id: "ses_1",
    engine_session_id: "uuid-1",
    actor: "agent:session:ses_1",
    cwd: "/w",
    reason: "x",
    started_at: "2026-10-05T00:00:00Z",
  },
  screen_tail: [],
  ...over,
});

afterEach(() => vi.restoreAllMocks());

describe("Oversight", () => {
  it("lists every session in the order the core gave, with reason, reply and screen", async () => {
    vi.spyOn(vogtApi, "sweepSessions").mockResolvedValue({
      rows: [
        ROW({
          attention: "approval",
          attention_reason: "asking for approval: Do you want to proceed?",
          session: { ...ROW({}).session, id: "ses_2", engine_session_id: "uuid-2", work_item: "WI-7", last_reply_excerpt: "I will delete build/" },
          screen_tail: ["Do you want to proceed?", "❯ 1. Yes"],
        }),
        ROW({}),
      ],
      counts: { total: 2, needs_you: 1, approval: 1, running: 1 },
      swept_at: new Date().toISOString(),
      engine: null,
    });
    const opened: string[] = [];
    render(() => <Oversight onOpenSession={(id) => opened.push(id)} />);

    await screen.findByText("asking for approval: Do you want to proceed?");
    expect(screen.getByText("1 need you")).toBeTruthy();
    const badges = screen.getAllByText(/^(Approval|Running)$/).map((el) => el.textContent);
    expect(badges).toEqual(["Approval", "Running"]);
    expect(screen.getByText("I will delete build/")).toBeTruthy();
    expect(screen.getByLabelText(/Last lines of/).textContent).toContain("❯ 1. Yes");
    fireEvent.click(screen.getByText("ses_2"));
    expect(opened).toEqual(["uuid-2"]);
  });

  it("says why when the engine or the core cannot be asked", async () => {
    vi.spyOn(vogtApi, "sweepSessions").mockResolvedValue({
      rows: [],
      counts: { total: 0, needs_you: 0 },
      swept_at: new Date().toISOString(),
      engine: "the engine is not answering",
    });
    render(() => <Oversight />);
    await waitFor(() =>
      expect(screen.getByRole("status").textContent).toContain("the engine is not answering"),
    );
    expect(screen.queryByText("No live or hibernated sessions.")).toBeNull();
  });

  it("words ages plainly", () => {
    const now = Date.parse("2026-10-05T12:00:00Z");
    expect(ago("2026-10-05T11:59:40Z", now)).toBe("just now");
    expect(ago("2026-10-05T11:30:00Z", now)).toBe("30 min ago");
    expect(ago("2026-10-05T09:00:00Z", now)).toBe("3 h ago");
    expect(ago(null, now)).toBeNull();
  });
});

describe("answering from the board", () => {
  it("offers the dialog's options and answers by number with the question", async () => {
    const approvalRow = ROW({
      attention: "approval",
      attention_reason: "stopped at a startup gate (folder trust): Trust?",
      session: {
        ...ROW({}).session,
        approval: {
          question: "Trust?",
          kind: "folder-trust",
          options: [
            { number: 1, label: "Yes, I trust this folder", selected: true },
            { number: 2, label: "No, exit" },
          ],
        },
      },
    });
    vi.spyOn(vogtApi, "sweepSessions").mockResolvedValue({
      rows: [approvalRow],
      counts: { total: 1, needs_you: 1 },
      swept_at: new Date().toISOString(),
      engine: null,
    });
    const answer = vi
      .spyOn(vogtApi, "answerSessionInVogt")
      .mockResolvedValue({ dismissed: true, chosen: { number: 2, label: "No, exit" } });
    render(() => <Oversight />);
    fireEvent.click(await screen.findByText("2. No, exit"));
    await waitFor(() => expect(answer).toHaveBeenCalledWith("uuid-1", 2, "Trust?"));
  });
});

describe("what a session is running", () => {
  it("words the resolved runtime, falling back to the template", async () => {
    const { runtimeWord } = await import("../Oversight");
    const base = ROW({}).session;
    expect(
      runtimeWord({ ...base, running: { agent: "codex", model: "gpt-5.6", effort: "high" } }),
    ).toBe("codex · gpt-5.6 · effort high");
    expect(runtimeWord({ ...base, template: "Shell" })).toBe("Shell");
    expect(runtimeWord(base)).toBeNull();
    const { sessionRuntimeHint } = await import("../sessionRowModel");
    expect(
      sessionRuntimeHint({
        id: "x",
        name: "x",
        activity: "idle",
        exit_code: null,
        scrollback_bytes: 0,
        cwd: "/w",
        created_at: "",
        template: "claude",
        command: "vogt-agent-auth run -- claude --model claude-opus-5-5",
      }),
    ).toBe("template claude · claude · model claude-opus-5-5");
  });
});

describe("permission posture", () => {
  it("marks a session started without permission checks", async () => {
    vi.spyOn(vogtApi, "sweepSessions").mockResolvedValue({
      rows: [ROW({ session: { ...ROW({}).session, permission_mode: "bypass" } })],
      counts: { total: 1, needs_you: 0 },
      swept_at: new Date().toISOString(),
      engine: null,
    });
    render(() => <Oversight />);
    expect(await screen.findByText("⚠ no permission checks")).toBeTruthy();
  });
});
