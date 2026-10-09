// The live call's wire protocol (`GET /api/assistant/call`, WI-960) and the
// pure model the call screen renders from.
//
// The shapes mirror `CallClientEvent` / `CallServerEvent` in
// `engine/contract`; docs/ENGINE.md "Live call contract" is the prose. Event
// names follow the OpenAI Realtime API's where the meaning is the same.

import type { AssistantPendingAction } from "./api";

/** What the server says the call is doing. */
export type CallServerState =
  | "listening"
  | "user_speaking"
  | "thinking"
  | "speaking"
  | "awaiting_approval";

/** What the call screen shows: the server's state plus the socket's own. */
export type CallPhase = CallServerState | "connecting" | "reconnecting" | "ended";

export type CallResponseStatus = "completed" | "interrupted" | "pending_approval" | "failed";

export interface CallMetrics {
  endpoint_ms?: number;
  stt_ms?: number;
  llm_first_text_ms?: number;
  tts_first_ms?: number;
  speech_end_to_first_audio_ms?: number;
  tool_rounds?: number;
  filler?: boolean;
}

export type CallServerEvent =
  | {
      type: "session.created";
      /** voicepipe's wire-protocol version. */
      protocol: number;
      call_id: string;
      sample_rate: number;
      end_of_turn_ms: number;
      barge_in_ms: number;
    }
  | { type: "call.state"; state: CallServerState }
  | { type: "input_audio_buffer.speech_started" }
  | { type: "input_audio_buffer.speech_stopped" }
  | { type: "conversation.item.input_audio_transcription.partial"; text: string }
  | { type: "conversation.item.input_audio_transcription.completed"; text: string }
  | { type: "response.created"; response_id: string }
  | { type: "response.text.delta"; response_id: string; delta: string }
  | {
      type: "response.audio.start";
      response_id: string;
      index: number;
      text: string;
      content_type: string;
      bytes: number;
    }
  | {
      type: "response.done";
      response_id: string;
      status: CallResponseStatus;
      text?: string;
      metrics: CallMetrics;
    }
  | { type: "output_audio.clear"; response_id: string }
  | { type: "conversation.item.truncated"; response_id: string; text: string }
  | { type: "approval.pending"; card: AssistantPendingAction }
  | { type: "approval.resolved"; id: string; approved: boolean }
  | { type: "error"; message: string }
  | { type: "pong" };

export type CallClientEvent =
  | { type: "auth"; token: string }
  | { type: "session.update"; profile?: string }
  | { type: "response.cancel" }
  | { type: "output_audio.started"; response_id: string; index: number }
  | { type: "output_audio.idle"; response_id: string }
  | { type: "action.resolve"; id: string; approve: boolean }
  | { type: "ping" };

/** Parse one text frame; `null` for anything that is not a known event. */
export function parseCallEvent(text: string): CallServerEvent | null {
  try {
    const value = JSON.parse(text) as { type?: unknown };
    return typeof value?.type === "string" ? (value as CallServerEvent) : null;
  } catch {
    return null;
  }
}

/** Everything the call screen renders. */
export interface CallView {
  phase: CallPhase;
  /** What the user is saying or just said: a live caption, then the transcript. */
  heard: string;
  /** Whether `heard` is still a caption (the turn is not over). */
  heardIsPartial: boolean;
  /** The reply as it streams in, then as it was recorded. */
  reply: string;
  responseId: string | null;
  pending: AssistantPendingAction | null;
  lastMetrics: CallMetrics | null;
  error: string | null;
}

export function initialCallView(): CallView {
  return {
    phase: "connecting",
    heard: "",
    heardIsPartial: false,
    reply: "",
    responseId: null,
    pending: null,
    lastMetrics: null,
    error: null,
  };
}

/** Fold one server event into the view. Pure. */
export function reduceCall(view: CallView, event: CallServerEvent): CallView {
  switch (event.type) {
    case "session.created":
      return { ...view, phase: "listening", error: null };
    case "call.state":
      return { ...view, phase: event.state };
    case "input_audio_buffer.speech_started":
      return { ...view, heard: "", heardIsPartial: true };
    case "conversation.item.input_audio_transcription.partial":
      return { ...view, heard: event.text, heardIsPartial: true };
    case "conversation.item.input_audio_transcription.completed":
      return { ...view, heard: event.text, heardIsPartial: false };
    case "response.created":
      return { ...view, responseId: event.response_id, reply: "" };
    case "response.text.delta":
      if (event.response_id !== view.responseId) return view;
      return { ...view, reply: view.reply + event.delta };
    case "response.done":
      if (event.response_id !== view.responseId) {
        return { ...view, lastMetrics: event.metrics };
      }
      return {
        ...view,
        reply: event.text ?? view.reply,
        lastMetrics: event.metrics,
      };
    case "conversation.item.truncated":
      if (event.response_id !== view.responseId) return view;
      return { ...view, reply: event.text };
    case "approval.pending":
      return { ...view, pending: event.card };
    case "approval.resolved":
      return view.pending?.id === event.id ? { ...view, pending: null } : view;
    case "error":
      return { ...view, error: event.message };
    default:
      return view;
  }
}

/** The status line for a phase. */
export function callPhaseLabel(phase: CallPhase, muted: boolean): string {
  if (muted && phase !== "ended") return "Muted";
  switch (phase) {
    case "connecting":
      return "Connecting…";
    case "reconnecting":
      return "Reconnecting…";
    case "listening":
      return "Listening";
    case "user_speaking":
      return "Hearing you";
    case "thinking":
      return "Thinking…";
    case "speaking":
      return "Speaking — talk to interrupt";
    case "awaiting_approval":
      return "Approve or deny on screen";
    case "ended":
      return "Call ended";
  }
}
