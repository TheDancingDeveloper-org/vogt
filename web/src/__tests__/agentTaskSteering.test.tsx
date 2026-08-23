import { fireEvent, render, screen } from "@solidjs/testing-library";
import { afterEach, describe, expect, it, vi } from "vitest";

import AgentTaskSteering from "../AgentTaskSteering";
import type { AgentTaskGate, AgentTaskRun } from "../api";

function run(overrides: Partial<AgentTaskRun> = {}): AgentTaskRun {
  return {
    id: "run-1",
    task_id: "task-1",
    started_at: "2026-08-23T00:00:00Z",
    trigger: "manual",
    session_id: "sess-1",
    session_name: "[Task] nightly",
    prompt_file: "/p",
    context_file: "/c",
    status: "running",
    completed_at: null,
    exit_code: null,
    summary: null,
    findings: [],
    gates: [],
    ...overrides,
  };
}

function openGate(): AgentTaskGate {
  return {
    id: "gate-1",
    question: "Proceed with the deploy?",
    options: [
      { label: "Approve", input: "go", approve: true },
      { label: "Hold", input: "stop", approve: false },
    ],
    state: "open",
    opened_at: "2026-08-23T00:00:01Z",
    resolved_at: null,
  };
}

afterEach(() => {
  vi.restoreAllMocks();
});

describe("AgentTaskSteering", () => {
  it("renders an open gate's question and one button per option", () => {
    render(() => (
      <AgentTaskSteering
        run={run({ gates: [openGate()] })}
        onSteer={() => {}}
        onAnswerGate={() => {}}
      />
    ));
    expect(screen.getByText("Proceed with the deploy?")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Approve" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Hold" })).toBeTruthy();
  });

  it("answers a gate with the chosen option's index", () => {
    const onAnswerGate = vi.fn();
    render(() => (
      <AgentTaskSteering
        run={run({ gates: [openGate()] })}
        onSteer={() => {}}
        onAnswerGate={onAnswerGate}
      />
    ));
    fireEvent.click(screen.getByRole("button", { name: "Hold" }));
    expect(onAnswerGate).toHaveBeenCalledWith("gate-1", 1);
  });

  it("shows a steer bar on a running run and delivers its text", () => {
    const onSteer = vi.fn();
    render(() => (
      <AgentTaskSteering run={run()} onSteer={onSteer} onAnswerGate={() => {}} />
    ));
    const input = screen.getByLabelText("Steer this run") as HTMLInputElement;
    fireEvent.input(input, { target: { value: "focus on the flaky test" } });
    fireEvent.click(screen.getByRole("button", { name: "Steer" }));
    expect(onSteer).toHaveBeenCalledWith("focus on the flaky test", false);
    // The field resets after a successful steer.
    expect(input.value).toBe("");
  });

  it("passes the interrupt flag through", () => {
    const onSteer = vi.fn();
    render(() => (
      <AgentTaskSteering run={run()} onSteer={onSteer} onAnswerGate={() => {}} />
    ));
    fireEvent.input(screen.getByLabelText("Steer this run"), {
      target: { value: "stop" },
    });
    fireEvent.click(screen.getByLabelText("Interrupt first"));
    fireEvent.click(screen.getByRole("button", { name: "Steer" }));
    expect(onSteer).toHaveBeenCalledWith("stop", true);
  });

  it("hides the steer bar once the run is no longer running", () => {
    render(() => (
      <AgentTaskSteering
        run={run({ status: "completed" })}
        onSteer={() => {}}
        onAnswerGate={() => {}}
      />
    ));
    expect(screen.queryByTestId("agent-task-steer-bar")).toBeNull();
  });

  it("summarises a resolved gate as an audit line, blocked never approved", () => {
    const blocked: AgentTaskGate = {
      ...openGate(),
      state: "blocked",
      reason: "timed out awaiting an answer",
      resolved_at: "2026-08-23T00:05:00Z",
    };
    render(() => (
      <AgentTaskSteering
        run={run({ status: "errored", gates: [blocked] })}
        onSteer={() => {}}
        onAnswerGate={() => {}}
      />
    ));
    expect(
      screen.getByText(/Blocked: timed out awaiting an answer/),
    ).toBeTruthy();
    // A blocked gate offers no option buttons.
    expect(screen.queryByRole("button", { name: "Approve" })).toBeNull();
  });

  it("attributes an answered gate to its actor", () => {
    const answered: AgentTaskGate = {
      ...openGate(),
      state: "answered",
      option_index: 0,
      option_label: "Approve",
      approved: true,
      actor: "auto-approve",
      auto: true,
      resolved_at: "2026-08-23T00:02:00Z",
    };
    render(() => (
      <AgentTaskSteering
        run={run({ gates: [answered] })}
        onSteer={() => {}}
        onAnswerGate={() => {}}
      />
    ));
    expect(screen.getByText(/Approved \(Approve\) by auto-approve/)).toBeTruthy();
  });
});
