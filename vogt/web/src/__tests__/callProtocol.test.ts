// The call screen's model: what each server event does to what is shown.
import { describe, expect, it } from "vitest";

import {
  callPhaseLabel,
  initialCallView,
  parseCallEvent,
  reduceCall,
  type CallServerEvent,
  type CallView,
} from "../callProtocol";

function fold(events: CallServerEvent[], from: CallView = initialCallView()): CallView {
  return events.reduce(reduceCall, from);
}

describe("the call view", () => {
  it("captions a turn while it is spoken, then shows what was heard", () => {
    const view = fold([
      { type: "session.created", protocol: 1, call_id: "c", sample_rate: 16000, end_of_turn_ms: 700, barge_in_ms: 500 },
      { type: "input_audio_buffer.speech_started" },
      { type: "conversation.item.input_audio_transcription.partial", text: "what is" },
    ]);
    expect(view.phase).toBe("listening");
    expect(view).toMatchObject({ heard: "what is", heardIsPartial: true });
    const done = reduceCall(view, {
      type: "conversation.item.input_audio_transcription.completed",
      text: "what is running",
    });
    expect(done).toMatchObject({ heard: "what is running", heardIsPartial: false });
  });

  it("streams the reply and keeps only what was heard when it is cut", () => {
    const view = fold([
      { type: "response.created", response_id: "r1" },
      { type: "response.text.delta", response_id: "r1", delta: "First. " },
      { type: "response.text.delta", response_id: "r1", delta: "Second." },
      { type: "response.text.delta", response_id: "other", delta: "stray" },
    ]);
    expect(view.reply).toBe("First. Second.");
    const cut = reduceCall(view, {
      type: "response.done",
      response_id: "r1",
      status: "interrupted",
      text: "First.",
      metrics: { speech_end_to_first_audio_ms: 900 },
    });
    expect(cut.reply).toBe("First.");
    expect(cut.lastMetrics?.speech_end_to_first_audio_ms).toBe(900);
    const truncated = reduceCall(view, {
      type: "conversation.item.truncated",
      response_id: "r1",
      text: "First.",
    });
    expect(truncated.reply).toBe("First.");
  });

  it("holds an approval card until its own resolution arrives", () => {
    const action = {
      kind: "send_input" as const,
      id: "a1",
      session_id: "s",
      session_name: "shell",
      text: "ls",
      submit: true,
    };
    const view = fold([{ type: "approval.pending", card: action }]);
    expect(view.pending?.id).toBe("a1");
    expect(reduceCall(view, { type: "approval.resolved", id: "zz", approved: true }).pending).not.toBeNull();
    expect(reduceCall(view, { type: "approval.resolved", id: "a1", approved: true }).pending).toBeNull();
  });

  it("parses only frames that are events", () => {
    expect(parseCallEvent('{"type":"pong"}')).toEqual({ type: "pong" });
    expect(parseCallEvent("not json")).toBeNull();
    expect(parseCallEvent('{"no":"type"}')).toBeNull();
  });

  it("says what the call is doing, and that it is muted", () => {
    expect(callPhaseLabel("speaking", false)).toMatch(/talk to interrupt/);
    expect(callPhaseLabel("listening", true)).toBe("Muted");
    expect(callPhaseLabel("ended", true)).toBe("Call ended");
    expect(callPhaseLabel("awaiting_approval", false)).toMatch(/on screen/);
  });
});
